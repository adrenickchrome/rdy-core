"""Failure-path checks for artifact parsing and result policy, with no network or product store."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock
import os
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/ci' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = load('install_tools')
SARIF = load('check_sarif')
FAST = load('run_fast')
SECRETS = load('security_scan')


def report(score='8.1', results=True):
    return {'version': '2.1.0', 'runs': [{'tool': {'driver': {'name': 'CodeQL', 'rules': [
        {'id': 'test/rule', 'properties': {'security-severity': score}}]}},
        'results': [{'ruleId': 'test/rule'}] if results else []}]}


class SarifPolicy(unittest.TestCase):
    def test_empty_result_report_does_not_assert_analysis_completion(self):
        self.assertEqual(SARIF.inspect_sarif(report(results=False))['blocking_results'], 0)

    def test_high_and_critical_block_low_remains_visible(self):
        for severity, expected in [('9.8', 1), ('7.0', 1), ('6.9', 0), ('0', 0)]:
            self.assertEqual(SARIF.inspect_sarif(report(severity))['blocking_results'], expected)

    def test_suppression_is_not_an_approved_waiver(self):
        item = report()
        item['runs'][0]['results'][0]['suppressions'] = [{'kind': 'inSource'}]
        self.assertEqual(SARIF.inspect_sarif(item)['blocking_results'], 1)

    def test_missing_or_unknown_results_refused(self):
        items = [{}, {'version': '2.1.0', 'runs': []}, report(), report()]
        del items[2]['runs'][0]['results']
        items[3]['runs'][0]['results'][0]['ruleId'] = 'not-in-rules'
        for item in items:
            with self.assertRaises((ValueError, KeyError)):
                SARIF.inspect_sarif(item)

    def test_empty_rule_inventory_refused(self):
        item = report(results=False)
        item['runs'][0]['tool']['driver']['rules'] = []
        with self.assertRaises(ValueError):
            SARIF.inspect_sarif(item)

    def test_invalid_severity_refused(self):
        for score in ['NaN', 'Infinity', '-1', '11', True, 'unknown']:
            with self.assertRaises(ValueError):
                SARIF.inspect_sarif(report(score))

    def test_analyzer_errors_are_not_clean(self):
        for invocation in [{'executionSuccessful': False}, {'toolExecutionNotifications': [{'level': 'error'}]},
                           {'toolConfigurationNotifications': [{'level': 'error'}]}]:
            item = report(results=False)
            item['runs'][0]['invocations'] = [invocation]
            with self.assertRaises(ValueError):
                SARIF.inspect_sarif(item)

    def test_unscored_error_blocks(self):
        item = report()
        del item['runs'][0]['tool']['driver']['rules'][0]['properties']['security-severity']
        item['runs'][0]['results'][0]['level'] = 'error'
        self.assertEqual(SARIF.inspect_sarif(item)['blocking_results'], 1)


class SarifComponents(unittest.TestCase):
    def document(self):
        # Reduced shape of successful hosted CodeQL2.27.3 Actions analysis:
        # empty driver, query-pack extension and empty supporting extensions.
        return {'version': '2.1.0', 'runs': [{'tool': {
            'driver': {'name': 'CodeQL', 'semanticVersion': '2.27.3'},
            'extensions': [{'name': 'codeql-action/pr-diff-range', 'rules': []},
                           {'name': 'codeql/actions-queries', 'guid': 'dbfe174b-1af3-4d67-8c42-520a794df84b',
                            'rules': [{'id': 'actions/test', 'properties': {'security-severity': '9'}}]},
                           {'name': 'codeql/actions-all', 'rules': []}]}, 'results': []}]}

    def result(self):
        return {'ruleId': 'actions/test', 'ruleIndex': 0,
                'rule': {'id': 'actions/test', 'index': 0, 'toolComponent': {'index': 1}}}

    def test_hosted_extension_shape_accepts_empty_results(self):
        self.assertEqual(SARIF.inspect_sarif(self.document()), {'results': 0, 'blocking_results': 0})

    def test_extension_result_uses_its_own_namespace(self):
        doc = self.document(); run = doc['runs'][0]
        run['tool']['driver']['rules'] = [{'id': 'actions/test', 'properties': {'security-severity': '0'}}]
        run['results'] = [self.result()]
        self.assertEqual(SARIF.inspect_sarif(doc)['blocking_results'], 1)
        run['results'][0]['suppressions'] = [{'kind': 'external'}]
        self.assertEqual(SARIF.inspect_sarif(doc)['blocking_results'], 1)

    def test_component_identity_and_index_must_agree(self):
        for ref in ({'index': 1, 'name': 'codeql/actions-queries'},
                    {'name': 'codeql/actions-queries'},
                    {'guid': 'dbfe174b-1af3-4d67-8c42-520a794df84b'}):
            doc = self.document(); result = self.result(); result['rule']['toolComponent'] = ref
            doc['runs'][0]['results'] = [result]
            self.assertEqual(SARIF.inspect_sarif(doc)['blocking_results'], 1)
        for ref in ({'index': 1, 'name': 'wrong'}, {'index': 0}, {'index': True},
                    {'index': -2}, {'index': 9}, {'index': '1'}, {'name': 'wrong'}, {'bogus': 'x'}, None):
            doc = self.document(); result = self.result(); result['rule']['toolComponent'] = ref
            doc['runs'][0]['results'] = [result]
            with self.subTest(ref=ref), self.assertRaises(ValueError): SARIF.inspect_sarif(doc)

    def test_mismatched_rule_ids_or_indexes_refuse(self):
        for key, value in (('ruleId', 'different'), ('ruleIndex', 1), ('ruleIndex', True)):
            doc = self.document(); result = self.result(); result[key] = value
            doc['runs'][0]['results'] = [result]
            with self.subTest(key=key), self.assertRaises(ValueError): SARIF.inspect_sarif(doc)

    def test_unqualified_extension_result_is_not_guessed(self):
        doc = self.document(); doc['runs'][0]['results'] = [{'ruleId': 'actions/test'}]
        with self.assertRaises(ValueError): SARIF.inspect_sarif(doc)

    def test_ambiguous_component_names_refuse(self):
        doc = self.document(); tool = doc['runs'][0]['tool']
        tool['extensions'].append({'name': 'codeql/actions-queries', 'rules': []})
        result = self.result(); result['rule']['toolComponent'] = {'name': 'codeql/actions-queries'}
        doc['runs'][0]['results'] = [result]
        with self.assertRaises(ValueError): SARIF.inspect_sarif(doc)

    def test_missing_malformed_or_empty_all_component_rules_refuse(self):
        for mutation in ('empty', 'missing-results', 'bad-extension', 'bad-rules', 'duplicate-rule'):
            doc = self.document(); run = doc['runs'][0]; component = run['tool']['extensions'][1]
            if mutation == 'empty': component['rules'] = []
            elif mutation == 'missing-results': del run['results']
            elif mutation == 'bad-extension': run['tool']['extensions'] = {}
            elif mutation == 'bad-rules': component['rules'] = None
            else: component['rules'] *= 2
            with self.subTest(mutation=mutation), self.assertRaises(ValueError): SARIF.inspect_sarif(doc)

    def test_legacy_driver_rule_index_still_enforces_policy(self):
        doc = report(); doc['runs'][0]['results'][0]['ruleIndex'] = 0
        self.assertEqual(SARIF.inspect_sarif(doc)['blocking_results'], 1)
        doc['runs'][0]['results'][0]['ruleIndex'] = 1
        with self.assertRaises(ValueError): SARIF.inspect_sarif(doc)



class SecretDispositions(unittest.TestCase):
    def setUp(self):
        self.line = b'synthetic fixture, no operational credential'
        self.files = [{'path': 'tests/fixture.rs', 'start': 3, 'lines': [self.line, b'next']}]
        self.finding = {'RuleID': 'generic-api-key', 'StartLine': 3, 'EndLine': 3}
        self.policy = {'schema_version': 1, 'fixtures': [{'path': 'tests/fixture.rs', 'line': 1,
            'rule': 'generic-api-key', 'line_sha256': hashlib.sha256(self.line).hexdigest(),
            'reason': 'Synthetic fixture for a boundary test.'}]}

    def test_only_exact_single_line_disposition_matches(self):
        self.assertEqual(SECRETS.summarize([self.finding], self.files, self.policy)['blocking_findings'], 0)
        for field, value in [('EndLine', 4), ('RuleID', 'another-rule')]:
            finding = dict(self.finding, **{field: value})
            self.assertEqual(SECRETS.summarize([finding], self.files, self.policy)['blocking_findings'], 1)
        changed = [{'path': 'tests/fixture.rs', 'start': 3, 'lines': [self.line + b' changed', b'next']}]
        self.assertEqual(SECRETS.summarize([self.finding], changed, self.policy)['blocking_findings'], 1)
        other = [{'path': 'other.rs', 'start': 3, 'lines': [self.line, b'next']}]
        self.assertEqual(SECRETS.summarize([self.finding], other, self.policy)['blocking_findings'], 1)

    def test_invalid_or_outside_locations_refuse(self):
        for first, last in [(True, 3), ('3', 3), (0, 3), (2, 3), (3, 2), (3, 5), (99, 99)]:
            with self.assertRaises(ValueError):
                SECRETS.summarize([dict(self.finding, StartLine=first, EndLine=last)], self.files, self.policy)

    def test_dispositions_are_exact_not_path_globs(self):
        for field, value in [('path', '../outside'), ('line', True), ('rule', '*'),
                             ('line_sha256', '*'), ('reason', '')]:
            policy = {'schema_version': 1, 'fixtures': [dict(self.policy['fixtures'][0], **{field: value})]}
            with self.assertRaises(ValueError):
                SECRETS.fixture_keys(policy)
        policy = {'schema_version': 1, 'fixtures': self.policy['fixtures'] * 2}
        with self.assertRaises(ValueError):
            SECRETS.fixture_keys(policy)

class ToolArchive(unittest.TestCase):
    @staticmethod
    def archive(entries):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as tar:
            for name, kind in entries:
                member = tarfile.TarInfo(name)
                if kind == 'file':
                    member.size = 4
                    tar.addfile(member, io.BytesIO(b'tool'))
                elif kind == 'link':
                    member.type = tarfile.SYMTYPE
                    member.linkname = '/tmp/outside'
                    tar.addfile(member)
                else:
                    member.type = tarfile.CHRTYPE
                    tar.addfile(member)
        return output.getvalue()

    def extract(self, entries):
        data = self.archive(entries)
        return TOOLS.extract_binary(data, hashlib.sha256(data).hexdigest(), 'tool')

    def test_regular_exact_binary_only(self):
        self.assertEqual(self.extract([('package/LICENSE', 'file'), ('package/tool', 'file')]), b'tool')

    def test_bad_hash_refused_before_tar(self):
        with self.assertRaises(ValueError):
            TOOLS.extract_binary(b'not-an-archive', '0' * 64, 'tool')

    def test_path_escape_links_devices_duplicates_missing_refused(self):
        for entries in [[('../tool', 'file')], [('/tool', 'file')], [('tool', 'link')],
                        [('tool', 'device')], [('x/tool', 'file'), ('y/tool', 'file')], [('other', 'file')]]:
            with self.assertRaises(ValueError):
                self.extract(entries)


class FastGate(unittest.TestCase):
    def run_gate(self, status=0, change=None):
        saved = Path.cwd()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'crates/rdy-qa').mkdir(parents=True)
            (root / 'Cargo.lock').write_text('original')
            (root / 'crates/rdy-qa/qa-baseline.json').write_text('{}')
            (root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.99.0"\n')
            calls = []
            def run(args, **kwargs):
                calls.append((args, kwargs))
                if args[1] == 'qa' and change:
                    (root / change).write_text('changed')
                return subprocess.CompletedProcess(args, status if args[1] == 'qa' else 0)
            try:
                with mock.patch.object(FAST, 'ROOT', root), mock.patch.object(FAST.os, 'geteuid', return_value=501), mock.patch.object(FAST, 'tracked_snapshot', return_value={'file': (420, 'hash')}), mock.patch.object(FAST.subprocess, 'run', side_effect=run):
                    result = FAST.main()
            finally:
                os.chdir(saved)
            return result, calls

    def test_source_snapshot_observes_content_modes_links_and_ignores_untracked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tracked').write_text('one')
            (root / 'link').symlink_to('tracked')
            with mock.patch.object(FAST.subprocess, 'check_output', return_value=b'tracked\0link\0'):
                first = FAST.tracked_snapshot(root)
                (root / 'untracked').write_text('generated')
                self.assertEqual(first, FAST.tracked_snapshot(root))
                (root / 'tracked').write_text('two')
                self.assertNotEqual(first, FAST.tracked_snapshot(root))
                (root / 'tracked').write_text('one')
                (root / 'tracked').chmod(0o700)
                self.assertNotEqual(first, FAST.tracked_snapshot(root))

    def test_original_gate_failure_preserved(self):
        self.assertEqual(self.run_gate(status=7)[0], 7)

    def test_lock_and_baseline_changes_fail(self):
        for path in ['Cargo.lock', 'crates/rdy-qa/qa-baseline.json']:
            self.assertEqual(self.run_gate(change=path)[0], 1)

    def test_fetch_locked_then_children_offline_no_perf(self):
        result, calls = self.run_gate()
        self.assertEqual(result, 0)
        self.assertIn('--locked', calls[0][0])
        self.assertEqual(calls[1][1]['env']['CARGO_NET_OFFLINE'], 'true')
        self.assertIn('--require-all', calls[1][0])
        self.assertIn('--skip-perf', calls[1][0])
        self.assertNotIn('--rebaseline', calls[1][0])


if __name__ == '__main__':
    unittest.main()
