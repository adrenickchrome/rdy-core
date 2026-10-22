"""Sanitized report contract; no product test semantics are replaced here."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location('fast_report', Path(__file__).resolve().parents[2] / 'scripts/ci/run_fast.py')
FAST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FAST)
COMMIT = 'a' * 40
SECRET = 'private-source-fragment /private/customer/token secretvalue'


def report(passed=True):
    gates = [{'name': name, 'pass': passed, 'detail': SECRET} for name in FAST.GATES]
    gates[3]['detail'] = '21 passed, 0 failed, 2 ignored, 1 skipped' if passed else '20 passed, 1 failed, 2 ignored, 1 skipped; failing: ' + SECRET
    return {'revision': COMMIT[:8], 'dirty': False, 'machine': SECRET,
            'mode': 'standard', 'duration_secs': 12.5, 'pass': passed,
            'gates': gates, 'areas': [
                {'name': name, 'ok': passed, 'passed': 5, 'failed': 0 if passed else 1,
                 'ignored': 2, 'skipped': 1, 'skip_lines': [SECRET], 'failures': [SECRET],
                 'panics': {'test': SECRET}} for name in sorted(FAST.AREAS)],
            'perf': [], 'perf_note': 'Skipped (`--skip-perf`).',
            'notes': [SECRET], 'failing_seeds': [SECRET], 'guarded': {'area': [SECRET]}}


class SanitizedReport(unittest.TestCase):
    def test_exact_counts_outcomes_without_raw_details(self):
        for passed in (True, False):
            summary = FAST.sanitized_qa(json.dumps(report(passed)), COMMIT)
            self.assertEqual(summary['pass'], passed)
            self.assertEqual(summary['stages'][3]['counts']['ignored'], 2)
            self.assertEqual(summary['stages'][3]['counts']['skipped'], 1)
            self.assertEqual(summary['area_totals']['ignored'], 44)
            self.assertEqual(summary['area_totals']['skipped'], 22)
            self.assertNotIn(SECRET, json.dumps(summary))
            self.assertNotIn('skip_lines', json.dumps(summary))

    def test_invalid_counts_names_revision_and_duplicate_keys_refused(self):
        mutations = [lambda r: r.update(revision='b'*8),
                     lambda r: r.update(mode='deep'),
                     lambda r: r.update(duration_secs=float('nan')),
                     lambda r: r['areas'][0].update(passed=True),
                     lambda r: r['areas'][0].update(ignored=-1),
                     lambda r: r['areas'][0].update(name=SECRET),
                     lambda r: r['gates'][3].update(detail=SECRET),
                     lambda r: r['areas'].append(r['areas'][0])]
        for mutate in mutations:
            value = report()
            mutate(value)
            with self.subTest(value=str(mutate)), self.assertRaises((ValueError, TypeError)):
                FAST.sanitized_qa(json.dumps(value), COMMIT)
        with self.assertRaises(ValueError):
            FAST.sanitized_qa('{"pass":true,"pass":false}', COMMIT)
        with self.assertRaises(ValueError):
            FAST.sanitized_qa(b' ' * (FAST.MAX_RAW_REPORT + 1), COMMIT)

    def test_contradictory_success_refused_but_partial_failure_preserved(self):
        for change in (lambda r: r.update(areas=[]),
                       lambda r: r['areas'][0].update(ok=False),
                       lambda r: r['areas'][0].update(failed=1),
                       lambda r: r['gates'][0].update(**{'pass': False}),
                       lambda r: r['gates'][3].update(detail='0 passed, 1 failed, 0 ignored, 0 skipped')):
            value = report()
            change(value)
            with self.assertRaises(ValueError):
                FAST.sanitized_qa(json.dumps(value), COMMIT)
        value = report(False)
        value['areas'] = []
        self.assertFalse(FAST.sanitized_qa(json.dumps(value), COMMIT)['pass'])

    def test_runtime_identity_uses_checked_out_commit_and_tree(self):
        with mock.patch.object(FAST.subprocess, 'check_output', side_effect=[(COMMIT+'\n').encode(), b'b'*40]), mock.patch.dict(os.environ, {'GITHUB_SHA':'c'*40,'GITHUB_RUN_ID':'999','GITHUB_RUN_ATTEMPT':'2'}, clear=True):
            value = FAST.identity(Path('.'))
        self.assertEqual(value['commit'], COMMIT)
        self.assertEqual(value['tree'], 'b'*40)
        self.assertEqual(value['github_sha'], 'c'*40)
        self.assertEqual(value['run_id'], '999')


class GateCapture(unittest.TestCase):
    def run_gate(self, status=0, raw='valid', fetch_failure=False, mutate=False, stale=False,
                 launch_failure=False, emit_failure=False, clear_failure=False):
        saved = Path.cwd()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'crates/rdy-qa').mkdir(parents=True)
            (root / 'Cargo.lock').write_text('original')
            (root / 'crates/rdy-qa/qa-baseline.json').write_text('{}')
            (root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.99.0"\n')
            (root / 'target/qa').mkdir(parents=True)
            if stale:
                (root / 'target/qa/report.json').write_text(json.dumps(report()))
                (root / 'target/qa/ci-fast-report.json').write_text('{"stale":true}')
            (root / 'github-output').touch()
            calls = []
            def run(args, **kwargs):
                calls.append((args, kwargs))
                if args[1] != 'qa':
                    if fetch_failure:
                        raise subprocess.CalledProcessError(9, args)
                else:
                    if launch_failure:
                        raise OSError('private raw failure')
                    if raw in ('valid', 'false_verdict'):
                        (root / 'target/qa/report.json').write_text(json.dumps(report(status == 0 and raw != 'false_verdict')))
                    elif raw == 'invalid':
                        (root / 'target/qa/report.json').write_text('{"secret":"' + SECRET + '"}')
                    if mutate:
                        (root / 'Cargo.lock').write_text('changed')
                return subprocess.CompletedProcess(args, status if args[1] == 'qa' else 0)
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.ExitStack() as stack:
                stack.enter_context(mock.patch.object(FAST, 'ROOT', root))
                stack.enter_context(mock.patch.dict(os.environ, {'GITHUB_OUTPUT': str(root / 'github-output')}))
                stack.enter_context(mock.patch.object(FAST.os, 'geteuid', return_value=501))
                stack.enter_context(mock.patch.object(FAST, 'identity', return_value={'commit':COMMIT,'tree':'b'*40,'os':'Darwin','architecture':'arm64'}))
                stack.enter_context(mock.patch.object(FAST, 'tracked_snapshot', return_value={'file': (420, 'hash')}))
                stack.enter_context(mock.patch.object(FAST.subprocess, 'run', side_effect=run))
                stack.enter_context(contextlib.redirect_stdout(output))
                stack.enter_context(contextlib.redirect_stderr(errors))
                if emit_failure:
                    stack.enter_context(mock.patch.object(FAST.os, 'replace', side_effect=OSError(SECRET)))
                if clear_failure:
                    stack.enter_context(mock.patch.object(FAST, 'clear_reports', side_effect=OSError(SECRET)))
                try:
                    code = FAST.main()
                except (OSError, subprocess.CalledProcessError) as exception:
                    code = exception
                finally:
                    os.chdir(saved)
            path = root / 'target/qa/ci-fast-report.json'
            summary = json.loads(path.read_text()) if path.exists() else None
            output_file = root / 'github-output'
            if not emit_failure and not clear_failure:
                self.assertEqual(output_file.read_text(), 'report_ready=true\n')
            else:
                self.assertEqual(output_file.read_text(), '')
            self.assertNotIn(SECRET, output.getvalue() + errors.getvalue())
            return code, summary, calls

    def test_success_and_nonzero_preserve_cargo_authority(self):
        for status in (0, 7):
            code, summary, calls = self.run_gate(status=status)
            self.assertEqual(code, status)
            self.assertEqual(summary['cargo_qa_exit_code'], status)
            self.assertEqual(summary['wrapper_exit_code'], status)
            self.assertEqual(summary['report_status'], 'captured')
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0][0], ['cargo', '+1.99.0', 'fetch', '--locked'])
            self.assertEqual(calls[1][0], ['cargo', 'qa', '--skip-perf', '--require-all'])
            self.assertEqual(calls[1][1]['env']['CARGO_NET_OFFLINE'], 'true')

    def test_missing_invalid_or_unemittable_report_fail_success_only(self):
        for status in (0, 7):
            for raw in ('missing', 'invalid'):
                code, summary, _ = self.run_gate(status=status, raw=raw)
                self.assertEqual(code, status or 1)
                self.assertEqual(summary['report_status'], raw)
                self.assertIsNone(summary['qa'])
            code, summary, _ = self.run_gate(status=status, emit_failure=True)
            self.assertEqual(code, status or 1)
            self.assertIsNone(summary)

    def test_false_report_verdict_cannot_be_success(self):
        code, summary, _ = self.run_gate(raw='false_verdict')
        self.assertEqual(code, 1)
        self.assertEqual(summary['cargo_qa_exit_code'], 0)
        self.assertFalse(summary['qa']['pass'])

    def test_stale_reports_removed_before_fetch_failure(self):
        code, summary, calls = self.run_gate(stale=True, fetch_failure=True)
        self.assertIsInstance(code, subprocess.CalledProcessError)
        self.assertEqual(code.returncode, 9)
        self.assertEqual(len(calls), 1)
        self.assertEqual(summary['stage'], 'locked_fetch')
        self.assertEqual(summary['report_status'], 'not_run')
        self.assertIsNone(summary['qa'])

    def test_compile_or_launch_failure_never_reuses_old_report(self):
        code, summary, _ = self.run_gate(stale=True, status=101, raw='missing')
        self.assertEqual(code, 101)
        self.assertEqual(summary['report_status'], 'missing')
        self.assertIsNone(summary['qa'])
        code, summary, _ = self.run_gate(stale=True, launch_failure=True)
        self.assertIsInstance(code, OSError)
        self.assertEqual(summary['stage'], 'cargo_qa')
        self.assertEqual(summary['report_status'], 'incomplete')
        self.assertIsNone(summary['qa'])

    def test_protected_source_failure_still_wins(self):
        code, summary, _ = self.run_gate(mutate=True)
        self.assertEqual(code, 1)
        self.assertEqual(summary['cargo_qa_exit_code'], 0)
        self.assertEqual(summary['wrapper_exit_code'], 1)
        self.assertFalse(summary['protected_inputs_unchanged'])

    def test_unclearable_old_reports_prevent_execution(self):
        code, _, calls = self.run_gate(clear_failure=True)
        self.assertEqual(code, 1)
        self.assertEqual(calls, [])


class SafeFiles(unittest.TestCase):
    def test_output_symlink_replaced_without_touching_referent_or_fixed_temp(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'target/qa').mkdir(parents=True)
            protected = root / 'Cargo.lock'
            protected.write_text('protected')
            (root / 'target/qa/ci-fast-report.json').symlink_to(protected)
            (root / 'target/qa/ci-fast-report.tmp').symlink_to(protected)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertTrue(FAST.emit_report(root, {'schema_version':1}))
            self.assertEqual(protected.read_text(), 'protected')
            self.assertFalse((root / 'target/qa/ci-fast-report.json').is_symlink())
            self.assertEqual(list((root / 'target/qa').glob('ci-fast-report-*.tmp')), [])

    def test_symlink_directories_and_raw_report_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / 'outside'
            outside.mkdir()
            (outside / 'report.json').write_text(json.dumps(report()))
            (root / 'target').symlink_to(outside, target_is_directory=True)
            with self.assertRaises(OSError):
                FAST.clear_reports(root)
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertFalse(FAST.emit_report(root, {'secret':False}))
            self.assertEqual(sorted(p.name for p in outside.iterdir()), ['report.json'])
            (root / 'target').unlink()
            (root / 'target/qa').mkdir(parents=True)
            (root / 'target/qa/report.json').symlink_to(outside / 'report.json')
            summary = {'source':{'commit':COMMIT},'qa':None}
            FAST.capture_qa(root, summary)
            self.assertEqual(summary['report_status'], 'invalid')
            self.assertIsNone(summary['qa'])
            (root / 'target/qa/report.json').unlink()
            os.mkfifo(root / 'target/qa/report.json')
            FAST.capture_qa(root, summary)
            self.assertEqual(summary['report_status'], 'invalid')

    def test_workflows_retain_only_sanitized_json_on_all_outcomes(self):
        root = Path(__file__).resolve().parents[2]
        for filename in ('ci.yml', 'platform.yml'):
            content = (root / '.github/workflows' / filename).read_text()
            block = content.split('- name: Retain sanitized fast-gate evidence\n', 1)[1].split('  coverage:',1)[0].split('  windows-portable:',1)[0]
            self.assertIn("if: always() && steps.fast-gate.outputs.report_ready == 'true'", block)
            self.assertIn('path: target/qa/ci-fast-report.json', block)
            self.assertIn('retention-days: 7', block)
            self.assertIn('if-no-files-found: error', block)
            self.assertNotIn('path: target/qa/report', block)


if __name__ == '__main__':
    unittest.main()
