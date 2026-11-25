"""Failure-path checks for the CI scripts and policies, with no network or product store."""
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
import urllib.error
import urllib.request
import urllib.response

import yaml  # test-only, hash-pinned in tests/ci/requirements-release-producers.txt

try:
    import tomllib
except ImportError:  # Python before 3.11; CI runs 3.13
    tomllib = None

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.github/workflows'
FIXTURES = Path(__file__).resolve().parent / 'fixtures'


def workflow(name):
    """A workflow parsed with a YAML loader, so tests see policy and not layout."""
    return yaml.safe_load((WORKFLOWS / name).read_text())


def triggers(doc):
    """The `on:` block (PyYAML reads the key `on` as True)."""
    return doc.get('on', doc.get(True))


def steps_of(doc, job=None):
    jobs = doc['jobs'].values() if job is None else [doc['jobs'][job]]
    return [step for entry in jobs for step in entry.get('steps', [])]


def step_named(doc, name):
    found = [step for step in steps_of(doc) if step.get('name') == name]
    assert len(found) == 1, (name, len(found))
    return found[0]


def uses_steps(doc, action):
    return [step for step in steps_of(doc) if str(step.get('uses', '')).startswith(action + '@')]


def command_lines(step):
    """The commands of a `run:` step, without blank lines and comments."""
    return [line.strip() for line in step['run'].splitlines()
            if line.strip() and not line.strip().startswith('#')]


def glob_matches(pattern, path):
    """GitHub path-filter globs: `*` stays inside one folder, `**` crosses folders."""
    regex = re.sub(r'\\\*\\\*', '.*', re.escape(pattern))
    regex = regex.replace('\\*', '[^/]*')
    return re.fullmatch(regex, path) is not None


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/ci' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = load('install_tools')
SARIF = load('check_sarif')
SET_ASIDE = load('set_aside_profiles')


def report(score='8.1', results=True):
    return {'version': '2.1.0', 'runs': [{'tool': {'driver': {'name': 'CodeQL', 'rules': [
        {'id': 'test/rule', 'properties': {'security-severity': score}}]}},
        'invocations': [{'executionSuccessful': True}],
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
        for invocation in [{'executionSuccessful': False},
                           {'executionSuccessful': True, 'toolExecutionNotifications': [{'level': 'error'}]},
                           {'executionSuccessful': True, 'toolConfigurationNotifications': [{'level': 'error'}]}]:
            item = report(results=False)
            item['runs'][0]['invocations'] = [invocation]
            with self.assertRaises(ValueError):
                SARIF.inspect_sarif(item)

    def test_codeql_consistency_self_checks_do_not_block(self):
        # Shape from hosted CodeQL 2.27.4 on Rust: an error-level count with no location.
        check = {'level': 'error', 'message': {'text': 'Ill-formed type mention'},
                 'descriptor': {'id': 'rust/diagnostics/type-inference-consistency-counts', 'index': 0}}
        for key in ('toolExecutionNotifications', 'toolConfigurationNotifications'):
            item = report(results=False)
            item['runs'][0]['invocations'] = [{'executionSuccessful': True, key: [check]}]
            with redirect_stderr(io.StringIO()) as err:
                SARIF.inspect_sarif(item)
            self.assertIn('type-inference-consistency-counts: Ill-formed type mention', err.getvalue())
        for descriptor in [None, {}, {'id': 7}, {'id': 'rust/diagnostics/extraction-errors'},
                           {'id': 'rust/diagnostics/type-inference-consistency-counts/x'},
                           {'id': 'rust/queries/type-inference-consistency-counts'}]:
            item = report(results=False)
            item['runs'][0]['invocations'] = [{'executionSuccessful': True, 'toolExecutionNotifications': [
                {'level': 'error', 'descriptor': descriptor}]}]
            with self.subTest(descriptor=descriptor), self.assertRaises(ValueError):
                SARIF.inspect_sarif(item)

    def test_unscored_error_blocks(self):
        item = report()
        del item['runs'][0]['tool']['driver']['rules'][0]['properties']['security-severity']
        item['runs'][0]['results'][0]['level'] = 'error'
        self.assertEqual(SARIF.inspect_sarif(item)['blocking_results'], 1)


class SarifComponents(unittest.TestCase):
    def document(self):
        # Reduced shape of successful hosted CodeQL 2.27.3 Actions analysis:
        # empty driver, query-pack extension and empty supporting extensions.
        return {'version': '2.1.0', 'runs': [{'tool': {
            'driver': {'name': 'CodeQL', 'semanticVersion': '2.27.3'},
            'extensions': [{'name': 'codeql-action/pr-diff-range', 'rules': []},
                           {'name': 'codeql/actions-queries', 'guid': 'dbfe174b-1af3-4d67-8c42-520a794df84b',
                            'rules': [{'id': 'actions/test', 'properties': {'security-severity': '9'}}]},
                           {'name': 'codeql/actions-all', 'rules': []}]},
            'invocations': [{'executionSuccessful': True, 'toolExecutionNotifications': []}], 'results': []}]}

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


class SarifDiagnostics(unittest.TestCase):
    def document(self):
        doc = report()
        result = doc['runs'][0]['results'][0]
        result['message'] = {'text': 'SENSITIVE_FREE_TEXT'}
        result['locations'] = [{'physicalLocation': {
            'artifactLocation': {'uri': 'crates/example.rs', 'uriBaseId': '%SRCROOT%'},
            'region': {'startLine': 23, 'snippet': {'text': 'SENSITIVE_SOURCE'}}}}]
        return doc

    def inspect(self, doc):
        return SARIF.inspect_sarif(doc, {'crates/example.rs'})

    def test_only_bounded_identifiers_severity_and_tracked_location_emitted(self):
        summary = self.inspect(self.document())
        self.assertEqual(summary, {'results': 1, 'blocking_results': 1,
            'blocking_diagnostics': [{'rule_id': 'test/rule', 'severity': 8.1,
                                      'location': {'path': 'crates/example.rs', 'line': 23}}]})
        self.assertNotIn('SENSITIVE', json.dumps(summary))

    def test_arbitrary_uri_traversal_untracked_and_controls_withheld(self):
        for uri in ('/crates/example.rs', '../crates/example.rs', 'crates/../example.rs',
                    'crates//example.rs', './crates/example.rs', 'https://host/private',
                    'file:///private/example.rs', 'C:/private/example.rs', 'crates\\example.rs',
                    'crates/example.rs?key=PRIVATE', 'crates/example.rs#PRIVATE',
                    'crates/%2e%2e/private', 'crates/example\nPRIVATE.rs', 'untracked.rs',
                    'a' * 1025):
            doc = self.document()
            doc['runs'][0]['results'][0]['locations'][0]['physicalLocation']['artifactLocation']['uri'] = uri
            with self.subTest(uri=uri):
                summary = self.inspect(doc)
                self.assertEqual(summary['blocking_results'], 1)
                self.assertIsNone(summary['blocking_diagnostics'][0]['location'])

    def test_invalid_or_non_source_bases_and_lines_withheld(self):
        for base in ('PRIVATE', 'file:///private', 1):
            doc = self.document()
            doc['runs'][0]['results'][0]['locations'][0]['physicalLocation']['artifactLocation']['uriBaseId'] = base
            self.assertIsNone(self.inspect(doc)['blocking_diagnostics'][0]['location'])
        for line in (True, '23', 0, -1, 2147483648, None):
            doc = self.document()
            doc['runs'][0]['results'][0]['locations'][0]['physicalLocation']['region']['startLine'] = line
            self.assertIsNone(self.inspect(doc)['blocking_diagnostics'][0]['location'])

    def test_indexed_artifact_must_resolve_without_conflict(self):
        doc = self.document(); run = doc['runs'][0]
        physical = run['results'][0]['locations'][0]['physicalLocation']
        run['artifacts'] = [{'location': physical['artifactLocation']}]
        physical['artifactLocation'] = {'index': 0}
        self.assertEqual(self.inspect(doc)['blocking_diagnostics'][0]['location']['line'], 23)
        for artifact in ({'index': 0, 'uri': 'different.rs'}, {'index': True},
                         {'index': -1}, {'index': 99}, {'index': '0'}):
            physical['artifactLocation'] = artifact
            self.assertIsNone(self.inspect(doc)['blocking_diagnostics'][0]['location'])

    def test_unusable_location_shapes_do_not_hide_blocking_result(self):
        for locations in (None, {}, [None], [{}], [{'physicalLocation': None}],
                          [{'physicalLocation': {'artifactLocation': None}}]):
            doc = self.document(); doc['runs'][0]['results'][0]['locations'] = locations
            summary = self.inspect(doc)
            self.assertEqual(summary['blocking_results'], 1)
            self.assertIsNone(summary['blocking_diagnostics'][0]['location'])

    def test_invalid_identifier_is_withheld_without_waiver(self):
        for identifier in ('test/PRIVATE\nINJECTION', 'x' * 161):
            doc = self.document(); run = doc['runs'][0]
            run['tool']['driver']['rules'][0]['id'] = identifier
            run['results'][0]['ruleId'] = identifier
            summary = self.inspect(doc)
            self.assertEqual(summary['blocking_results'], 1)
            self.assertEqual(summary['blocking_diagnostics'][0]['rule_id'], '[withheld-invalid-identifier]')

    def test_diagnostics_preserve_scores_suppression_and_unscored_errors(self):
        for score, expected in (('6.9', 0), ('7.0', 1), ('9.8', 1)):
            doc = self.document(); run = doc['runs'][0]
            run['tool']['driver']['rules'][0]['properties']['security-severity'] = score
            run['results'][0]['suppressions'] = [{'kind': 'external'}]
            summary = self.inspect(doc)
            self.assertEqual(summary['blocking_results'], expected)
            self.assertEqual(len(summary['blocking_diagnostics']), expected)
        doc = self.document(); run = doc['runs'][0]
        run['tool']['driver']['rules'][0]['properties'] = {}
        run['results'][0]['level'] = 'error'
        self.assertEqual(self.inspect(doc)['blocking_diagnostics'][0]['severity'], 'unscored-error')

    def test_sarif_step_logs_every_result_and_does_not_block_on_them(self):
        with tempfile.TemporaryDirectory() as directory:
            doc = self.document(); doc['runs'][0]['results'] *= 101
            for filename in ('one.sarif', 'two.sarif'):
                (Path(directory) / filename).write_text(json.dumps(doc))
            summary_file = Path(directory) / 'summary.md'
            output = io.StringIO()
            with mock.patch.object(SARIF.subprocess, 'check_output', return_value=b'crates/example.rs\0'), \
                 mock.patch.dict(SARIF.os.environ, {'GITHUB_STEP_SUMMARY': str(summary_file)}), \
                 redirect_stdout(output):
                self.assertEqual(SARIF.main(['sarif', directory]), 0)
            lines = output.getvalue().splitlines()
            self.assertIn('2 file(s), 202 result(s), 202 high or critical', lines[0])
            self.assertEqual(lines[1], 'test/rule  high 8.1  crates/example.rs:23')
            self.assertEqual(len([line for line in lines if line.startswith('test/rule')]), 202)
            self.assertEqual(len(lines), 203)
            self.assertNotIn('SENSITIVE', output.getvalue())
            summary = summary_file.read_text()
            self.assertEqual(summary.count('`test/rule  high 8.1  crates/example.rs:23`'), 100)
            self.assertIn('102 more not shown here. The full list is in the job log and in the codeql-sarif '
                          'artifact.', summary)

    def test_short_summary_lists_everything_without_a_note(self):
        with tempfile.TemporaryDirectory() as directory:
            summary_file = Path(directory) / 'summary.md'
            with mock.patch.dict(SARIF.os.environ, {'GITHUB_STEP_SUMMARY': str(summary_file)}):
                SARIF.write_summary('Title', ['line'] * 100)
            self.assertEqual(summary_file.read_text().count('- `line`'), 100)
            self.assertNotIn('more not shown', summary_file.read_text())

    def test_sarif_step_labels_critical_and_unscored_errors(self):
        self.assertEqual(SARIF.severity_label(9.8), 'critical 9.8')
        self.assertEqual(SARIF.severity_label(7.0), 'high 7')
        self.assertEqual(SARIF.severity_label('unscored-error'), 'error (no security severity)')
        self.assertEqual(SARIF.result_lines([{'rule_id': 'r', 'severity': 7.5, 'location': None}]),
                         ['r  high 7.5  (location withheld)'])

    def test_sarif_step_still_fails_on_analyzer_errors_and_bad_sarif(self):
        with tempfile.TemporaryDirectory() as directory:
            doc = self.document(); doc['runs'][0]['invocations'] = [{'executionSuccessful': False}]
            (Path(directory) / 'report.sarif').write_text(json.dumps(doc))
            with mock.patch.object(SARIF.subprocess, 'check_output', return_value=b'crates/example.rs\0'):
                with self.assertRaises(ValueError):
                    SARIF.main(['sarif', directory])
                (Path(directory) / 'report.sarif').write_text('{not json')
                with self.assertRaises(ValueError):
                    SARIF.main(['sarif', directory])
        with tempfile.TemporaryDirectory() as empty:
            with self.assertRaises(ValueError):
                SARIF.main(['sarif', empty])

    def test_invocations_must_state_execution_as_a_boolean(self):
        def run(invocations, delete=False):
            doc = self.document()
            if delete:
                del doc['runs'][0]['invocations']
            else:
                doc['runs'][0]['invocations'] = invocations
            with tempfile.TemporaryDirectory() as directory:
                (Path(directory) / 'report.sarif').write_text(json.dumps(doc))
                with mock.patch.object(SARIF.subprocess, 'check_output', return_value=b'crates/example.rs\0'), \
                     redirect_stdout(io.StringIO()):
                    return SARIF.main(['sarif', directory])
        with self.assertRaises(ValueError):
            run(None, delete=True)
        for invocations in ([], None, {}, [None], [{}], [{'executionSuccessful': 'false'}],
                            [{'executionSuccessful': 'true'}], [{'executionSuccessful': 0}],
                            [{'executionSuccessful': 1}], [{'executionSuccessful': None}],
                            [{'executionSuccessful': True}, {}],
                            [{'executionSuccessful': True, 'toolExecutionNotifications': {}}],
                            [{'executionSuccessful': True, 'toolConfigurationNotifications': [None]}]):
            with self.subTest(invocations=invocations), self.assertRaises(ValueError):
                run(invocations)
        self.assertEqual(run([{'executionSuccessful': True, 'toolExecutionNotifications': [{'level': 'note'}]}]), 0)

    def test_missing_repository_inventory_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / 'report.sarif').write_text(json.dumps(self.document()))
            with mock.patch.object(SARIF.subprocess, 'check_output',
                                   side_effect=subprocess.CalledProcessError(1, 'git')):
                with self.assertRaises(subprocess.CalledProcessError):
                    SARIF.main(['sarif', directory])


def alert(number=1, state='open', level='high', tool='CodeQL', category='/language:rust/'):
    """The fields of a code-scanning alert that the gate reads."""
    return {'number': number, 'state': state,
            'rule': {'id': 'rust/test-rule', 'severity': 'error', 'security_severity_level': level},
            'tool': {'name': tool, 'version': '2.27.3'},
            'most_recent_instance': {'ref': 'refs/pull/7/merge', 'category': category, 'state': state,
                                     'location': {'path': 'crates/example.rs', 'start_line': 12}}}


class FakeResponse(io.BytesIO):
    """A response whose headers are a real HTTPMessage, so repeated Link fields stay separate."""
    def __init__(self, body, links=()):
        super().__init__(body)
        self.headers = http.client.HTTPMessage()
        for value in links:
            self.headers['Link'] = value

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


ALERTS_URL = 'https://api.github.com/repos/owner/repo/code-scanning/alerts'
QUERY = 'tool_name=CodeQL&ref=refs%2Fpull%2F7%2Fmerge&state=open&per_page=100'
SERVER_URL = 'https://example.invalid/repositories/987/code-scanning/alerts?severity=low&page='
NEXT = '<' + SERVER_URL + '2>; rel="next"'
PREV = '<' + SERVER_URL + '1>; rel="prev"'


def built(page):
    return ALERTS_URL + '?' + QUERY + '&page=' + str(page)


def respond(*pages):
    """Each page is a body, or (body, [Link field values])."""
    responses = iter([FakeResponse(json.dumps(page[0]).encode(), page[1]) if isinstance(page, tuple)
                      else FakeResponse(json.dumps(page).encode()) for page in pages])
    requests = []

    def urlopen(request, timeout):
        requests.append(request)
        return next(responses)
    return urlopen, requests


ENV = {'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_TOKEN': 'x', 'GITHUB_API_URL': 'https://api.github.com'}


class AlertGate(unittest.TestCase):
    def decide(self, *alerts):
        return SARIF.blocking_alerts(list(alerts), 'CodeQL')

    def test_open_high_and_critical_block(self):
        self.assertEqual([a['number'] for a in self.decide(alert(1), alert(2, level='critical'))], [1, 2])

    def test_dismissed_or_fixed_high_does_not_block(self):
        self.assertEqual(self.decide(alert(state='dismissed'), alert(state='fixed'),
                                     alert(level='critical', state='dismissed')), [])

    def test_open_medium_low_or_unscored_does_not_block(self):
        self.assertEqual(self.decide(alert(level='medium'), alert(level='low'), alert(level=None)), [])

    def test_other_tool_does_not_block(self):
        self.assertEqual(self.decide(alert(tool='Other'), alert(tool='Other', level='critical')), [])

    def test_any_category_blocks(self):
        found = self.decide(alert(1, category='/language:actions/'), alert(2, category='/language:rust'),
                            alert(3, category='other'), alert(4, category=None))
        self.assertEqual([a['number'] for a in found], [1, 2, 3, 4])

    def test_malformed_alerts_refuse(self):
        for broken in ({}, None, dict(alert(), number='1'), dict(alert(), state=None),
                       dict(alert(), rule=None), dict(alert(), tool=None),
                       dict(alert(), most_recent_instance=None)):
            with self.subTest(broken=broken), self.assertRaises(ValueError):
                self.decide(broken)
        with self.assertRaises(ValueError):
            SARIF.blocking_alerts({}, 'CodeQL')

    def test_unknown_states_refuse(self):
        for state in ('unexpected', 'OPEN', 'closed', 'auto_dismissed', '', 1, None):
            with self.subTest(state=state), self.assertRaises(ValueError):
                self.decide(alert(state=state))

    def test_tool_without_a_name_refuses(self):
        for tool in ({}, {'name': ''}, {'name': None}, {'name': 5}):
            with self.subTest(tool=tool), self.assertRaises(ValueError):
                self.decide(dict(alert(), tool=tool))

    def test_unknown_or_missing_security_severity_refuses(self):
        for level in ('HIGH', 'Critical', 'severe', '', 8, True, ['high']):
            with self.subTest(level=level), self.assertRaises(ValueError):
                self.decide(alert(level=level))
        missing = alert()
        del missing['rule']['security_severity_level']
        with self.assertRaises(ValueError):
            self.decide(missing)
        self.assertEqual(self.decide(alert(level=None)), [])

    def test_unreadable_alert_fails_the_gate(self):
        for broken in (alert(state='unexpected'), dict(alert(), tool={}), alert(level='HIGH')):
            urlopen, _ = respond([broken])
            with self.subTest(broken=broken), self.assertRaises(SARIF.AlertQueryError):
                self.run_gate(urlopen)

    def run_gate(self, urlopen, ref='refs/pull/7/merge', env=ENV):
        output = io.StringIO()
        with mock.patch.object(SARIF, 'urlopen', urlopen), \
             mock.patch.dict(SARIF.os.environ, env, clear=True), redirect_stdout(output):
            code = SARIF.main(['alerts', '--tool', 'CodeQL', '--ref', ref])
        return code, output.getvalue()

    def test_gate_pages_through_alerts_and_prints_each_blocking_one(self):
        urlopen, requests = respond(([alert(n, level='medium') for n in range(1, 101)], [NEXT]), [alert(101), alert(102, state='dismissed')])
        code, output = self.run_gate(urlopen)
        self.assertEqual(code, 1)
        self.assertIn('#101  rust/test-rule  high  crates/example.rs:12', output)
        self.assertNotIn('#102', output)
        self.assertEqual([r.full_url for r in requests], [built(1), built(2)])

    def test_separate_link_fields_are_all_read(self):
        urlopen, requests = respond(([], [PREV, NEXT]), [alert(1)])
        self.assertEqual(self.run_gate(urlopen)[0], 1)
        self.assertEqual([r.full_url for r in requests], [built(1), built(2)])

    def test_short_or_empty_page_with_next_goes_on(self):
        for first in ([alert(1, level='medium')], []):
            urlopen, requests = respond((first, [NEXT]), ([], [PREV, NEXT]), [alert(3)])
            with self.subTest(first=first):
                code, output = self.run_gate(urlopen)
                self.assertEqual(code, 1)
                self.assertIn('#3  rust/test-rule', output)
                self.assertEqual([r.full_url for r in requests], [built(1), built(2), built(3)])

    def test_full_page_without_next_goes_on_until_an_empty_page(self):
        urlopen, requests = respond([alert(n, level='medium') for n in range(1, 101)], [])
        self.assertEqual(self.run_gate(urlopen)[0], 0)
        self.assertEqual([r.full_url for r in requests], [built(1), built(2)])

    def test_short_page_without_next_ends_the_list(self):
        urlopen, requests = respond(([alert(1, level='medium')], [PREV]), [alert(2)])
        self.assertEqual(self.run_gate(urlopen)[0], 0)
        self.assertEqual([r.full_url for r in requests], [built(1)])

    def test_server_urls_are_never_requested(self):
        urlopen, requests = respond(([], [NEXT]), ([], [NEXT]), [])
        self.run_gate(urlopen)
        self.assertEqual([r.full_url for r in requests], [built(1), built(2), built(3)])
        self.assertFalse(any('example.invalid' in r.full_url for r in requests))

    def test_relations_compare_case_insensitively_and_may_be_listed(self):
        for link in ('<' + SERVER_URL + '2>; rel="Next"', '<' + SERVER_URL + '2>; REL="next"',
                     '<' + SERVER_URL + '2>; Rel = "NEXT"', '<' + SERVER_URL + '2>; rel="prev next"',
                     '<' + SERVER_URL + '2>; title="x"; rel="last Next"'):
            urlopen, requests = respond(([], [link]), [alert(2)])
            with self.subTest(link=link):
                self.assertEqual(self.run_gate(urlopen)[0], 1)
                self.assertEqual([r.full_url for r in requests], [built(1), built(2)])
        for link in ('<' + SERVER_URL + '2>; rel="nextpage"', '<' + SERVER_URL + '2>; title="next"'):
            urlopen, requests = respond(([], [link]), [alert(2)])
            with self.subTest(link=link):
                self.assertEqual(self.run_gate(urlopen)[0], 0)
                self.assertEqual(len(requests), 1)

    def test_malformed_link_fails_the_gate(self):
        for links in (['not a link header'], [NEXT, 'garbage'], ['<x>; rel=next'], ['<a b>; rel="next"']):
            urlopen, _ = respond(([], links), [alert(1, level='medium')])
            with self.subTest(links=links), self.assertRaisesRegex(SARIF.AlertQueryError, 'Link'):
                self.run_gate(urlopen)

    def test_page_cap_is_a_failure(self):
        urlopen, _ = respond(*[([], [NEXT]) for _ in range(4)])
        with mock.patch.object(SARIF, 'MAX_PAGES', 3), self.assertRaisesRegex(SARIF.AlertQueryError, 'pages'):
            self.run_gate(urlopen)

    def test_redirects_are_refused_by_the_real_handler(self):
        """urllib's own redirect processing runs; only the HTTPS transport is replaced."""
        targets = ('https://api.github.com/repos/other/repo/code-scanning/alerts?severity=low',
                   'https://example.invalid/repos/owner/repo/code-scanning/alerts')
        for code in (301, 302, 303, 307, 308, 300):
            for target in targets:
                sent = []

                def https_open(handler, request):
                    sent.append((request.full_url, request.get_header('Authorization')))
                    headers = http.client.HTTPMessage()
                    headers['Location'] = target
                    headers['Content-Type'] = 'application/json'
                    response = urllib.response.addinfourl(io.BytesIO(b'[]'), headers, request.full_url, code)
                    response.msg = 'Redirect'
                    return response
                with self.subTest(code=code, target=target), \
                     mock.patch.object(urllib.request.HTTPSHandler, 'https_open', https_open), \
                     mock.patch.dict(SARIF.os.environ, ENV, clear=True), redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(SARIF.AlertQueryError, 'redirect'):
                        SARIF.main(['alerts', '--tool', 'CodeQL', '--ref', 'refs/pull/7/merge'])
                    self.assertEqual(sent, [(built(1), 'Bearer x')])

    def test_real_handler_reads_a_plain_answer(self):
        sent = []

        def https_open(handler, request):
            sent.append(request.full_url)
            headers = http.client.HTTPMessage()
            response = urllib.response.addinfourl(io.BytesIO(json.dumps([alert(1)]).encode()), headers,
                                                  request.full_url, 200)
            response.msg = 'OK'
            return response
        with mock.patch.object(urllib.request.HTTPSHandler, 'https_open', https_open), \
             mock.patch.dict(SARIF.os.environ, ENV, clear=True), redirect_stdout(io.StringIO()):
            self.assertEqual(SARIF.main(['alerts', '--tool', 'CodeQL', '--ref', 'refs/pull/7/merge']), 1)
        self.assertEqual(sent, [built(1)])

    def test_gate_passes_when_only_dismissed_or_lower_alerts_are_open(self):
        urlopen, _ = respond([alert(1, state='dismissed'), alert(2, level='medium')])
        self.assertEqual(self.run_gate(urlopen, ref='refs/heads/main')[0], 0)
        urlopen, _ = respond([])
        self.assertEqual(self.run_gate(urlopen)[0], 0)

    def test_api_errors_fail_the_gate(self):
        def forbidden(request, timeout):
            raise urllib.error.HTTPError(request.full_url, 403, 'Forbidden', {},
                                         io.BytesIO(b'{"message": "Resource not accessible by integration"}'))

        def offline(request, timeout):
            raise urllib.error.URLError('no route')
        not_a_list, _ = respond({'message': 'no analysis found'})
        for urlopen in (forbidden, offline, not_a_list):
            with self.subTest(urlopen=urlopen), self.assertRaises(SARIF.AlertQueryError):
                self.run_gate(urlopen)
        with self.assertRaisesRegex(SARIF.AlertQueryError, 'HTTP 403 Resource not accessible'):
            self.run_gate(forbidden)

    def test_missing_token_repository_or_ref_fails_the_gate(self):
        urlopen, _ = respond([])
        for env in (dict(ENV, GITHUB_TOKEN=''), {k: v for k, v in ENV.items() if k != 'GITHUB_REPOSITORY'},
                    dict(ENV, GITHUB_API_URL='http://api.github.com')):
            with self.subTest(env=env), self.assertRaises(SARIF.AlertQueryError):
                self.run_gate(urlopen, env=env)
        for ref in ('', 'main', 'refs/pull/7/merge\nx'):
            with self.subTest(ref=ref), self.assertRaises(SARIF.AlertQueryError):
                self.run_gate(urlopen, ref=ref)

    def test_malformed_alert_in_response_fails_the_gate(self):
        urlopen, _ = respond([{'number': 1}])
        with self.assertRaises(SARIF.AlertQueryError):
            self.run_gate(urlopen)


def gitleaks_config():
    return tomllib.loads((ROOT / 'scripts/ci/gitleaks.toml').read_text())


def allowlisted(config, path, secret, line, rule='generic-api-key'):
    """The gitleaks allowlist decision for one finding (global allowlists only)."""
    for entry in config.get('allowlists', []):
        if entry.get('targetRules') and rule not in entry['targetRules']:
            continue
        target = {'secret': secret, 'line': line}[entry.get('regexTarget', 'secret')]
        if entry.get('condition', 'OR') not in ('OR', 'AND'):
            raise ValueError('unknown condition')
        checks = []
        if 'paths' in entry:
            checks.append(any(re.search(pattern, path) for pattern in entry['paths']))
        if 'regexes' in entry:
            checks.append(any(re.search(pattern, target) for pattern in entry['regexes']))
        if (all(checks) if entry.get('condition', 'OR') == 'AND' else any(checks)):
            return True
    return False


def tracked(prefix='crates/'):
    names = subprocess.check_output(['git', 'ls-files', '-z', '--', prefix], cwd=ROOT).decode().split('\0')
    return [name for name in names if name]


# Today's generic-api-key hits outside the fixture and golden directories: the synthetic values
# per file. Each must be in its file and be allowlisted by shape, not by line.
SYNTHETIC = {
    'crates/rdy-cli/src/brief_text.rs': ['0123456789abcdef0123', 'abcdef0123456789', '0123456789abcdef'],
    'crates/rdy-engine/tests/hub/hub_http.rs': ['rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf',
                                            'rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ'],
    'crates/rdy-qa/tests/cli/cli_hub_auth.rs': ['rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf',
                                                'rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC',
                                                'rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ',
                                                'rdyh_p_000000000000000000000000000000000000000025UVUe'],
    'crates/rdy-qa/tests/mcp/mcp_hub_auth.rs': ['rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf',
                                                'rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC'],
    'crates/rdy-qa/tests/mcp/mcp_hub_keys.rs': ['rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf',
                                                'rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC'],
}
FINGERPRINT_FILES = ['crates/rdy-engine/tests/config/signing_crypto.rs']
FIXTURE_FILES = [
    'crates/rdy-qa/fixtures/dump/v2-s14.expect.json', 'crates/rdy-qa/fixtures/dump/v2-s15-holder.expect.json',
    'crates/rdy-qa/fixtures/dump/v2-s15.expect.json', 'crates/rdy-qa/fixtures/dump/v3-s16-chain.expect.json',
    'crates/rdy-qa/fixtures/dump/v3-s16-holder-chain.expect.json',
    'crates/rdy-qa/fixtures/dump/v3-s16-holder.expect.json', 'crates/rdy-qa/fixtures/dump/v3-s16.expect.json',
    'crates/rdy-qa/fixtures/sync/v1-s15.expect.json', 'crates/rdy-qa/tests/golden/mcp/rdy_brief--doc.json',
    'crates/rdy-qa/tests/golden/mcp/rdy_write--refused-brief-required.json',
]
# Built from parts so that no literal here looks like a credential to the scanner.
REALISTIC_HUB = 'rdyh_a_' + 'Vq3ZtK8mWp1xRb7LcN4yHd2JfG6sTa9QeU5oBk0P' + 'r8YtXw'
REALISTIC_HEX = '9f3ac27e' + '5b18d460ab3c'
REALISTIC_B64 = 'Zk4pQw9' + 'rT2mXv7LcB1nYh5JdG8sKa3UeF6oWi0Rq4tPz2M'


@unittest.skipIf(tomllib is None, 'tomllib needs Python 3.11 or later')
class SecretAllowlists(unittest.TestCase):
    def setUp(self):
        self.config = gitleaks_config()

    def test_config_extends_defaults_and_every_allowlist_is_described(self):
        self.assertEqual(self.config['extend'], {'useDefault': True})
        self.assertNotIn('allowlist', self.config)
        self.assertGreater(len(self.config['allowlists']), 0)
        for entry in self.config['allowlists']:
            self.assertLessEqual(set(entry), {'description', 'condition', 'paths', 'regexes', 'targetRules'})
            self.assertTrue(entry['description'].strip())
            self.assertIn(entry.get('condition', 'OR'), ('OR', 'AND'))
            for pattern in entry.get('paths', []) + entry.get('regexes', []):
                # The scanner uses RE2: no lookaround or backreference, so Python agrees with it.
                self.assertIsNone(re.search(r'\(\?[=!<]|\\[1-9]', pattern), pattern)

    def test_path_allowlists_exist_and_never_cover_a_src_tree(self):
        files = tracked()
        sources = [name for name in files if re.match(r'crates/[^/]+/src/', name)]
        self.assertGreater(len(sources), 0)
        for entry in self.config['allowlists']:
            for pattern in entry.get('paths', []):
                with self.subTest(pattern=pattern):
                    self.assertTrue(any(re.search(pattern, name) for name in files), 'no tracked file')
                    self.assertEqual([name for name in sources if re.search(pattern, name)], [])

    def test_fixture_and_golden_files_are_allowlisted_by_path(self):
        for path in FIXTURE_FILES:
            with self.subTest(path=path):
                self.assertTrue((ROOT / path).is_file())
                self.assertTrue(allowlisted(self.config, path, REALISTIC_HEX, 'x'))
                self.assertTrue(allowlisted(self.config, './' + path, REALISTIC_HEX, 'x'))

    def test_synthetic_values_are_allowlisted_by_shape(self):
        for path, values in SYNTHETIC.items():
            text = (ROOT / path).read_text()
            for value in values:
                with self.subTest(path=path, value=value):
                    self.assertIn(value, text)
                    line = next(line for line in text.splitlines() if value in line)
                    self.assertTrue(allowlisted(self.config, path, value, line))

    def test_public_key_fingerprints_are_allowlisted_in_their_file_only(self):
        for path in FINGERPRINT_FILES:
            lines = [line for line in (ROOT / path).read_text().splitlines() if 'key SHA256:' in line]
            self.assertEqual(len(lines), 2)
            for line in lines:
                value = re.search(r'key SHA256:([A-Za-z0-9+/]{43})', line).group(1)
                with self.subTest(line=line):
                    self.assertTrue(allowlisted(self.config, path, value, line))
                    self.assertFalse(allowlisted(self.config, 'crates/rdy-engine/src/hub.rs', value, line))
                    self.assertFalse(allowlisted(self.config, 'crates/rdy-engine/tests/other.rs', value, line))

    def test_contract_domain_label_is_allowlisted_in_its_file_only(self):
        path = 'crates/rdy-qa/tests/release/release_contract.rs'
        value = 'rdy-tension-vkey-2'
        line = next(line for line in (ROOT / path).read_text().splitlines() if value in line)
        self.assertTrue(allowlisted(self.config, path, value, line))
        self.assertFalse(allowlisted(self.config, 'crates/rdy-engine/src/tensions.rs', value, line))
        self.assertFalse(allowlisted(self.config, path, value + 'x', line))
        self.assertFalse(allowlisted(self.config, path, REALISTIC_B64, line))

    def test_a_secret_beside_a_fingerprint_comment_is_not_allowlisted(self):
        fingerprint = re.search(r'key SHA256:([A-Za-z0-9+/]{43})',
                                (ROOT / FINGERPRINT_FILES[0]).read_text()).group(0)
        line = 'let auth = "' + REALISTIC_HUB + '"; // ' + fingerprint
        for path in ('crates/rdy-engine/src/hub.rs', FINGERPRINT_FILES[0]):
            for secret in (REALISTIC_HUB, REALISTIC_B64):
                with self.subTest(path=path, secret=secret):
                    self.assertFalse(allowlisted(self.config, path, secret, line))

    def test_realistic_values_outside_the_shapes_and_paths_are_not_allowlisted(self):
        cases = [
            ('crates/rdy-engine/src/hub.rs', REALISTIC_HUB, '"access' + '_token":"' + REALISTIC_HUB + '"'),
            ('crates/rdy-engine/tests/hub/hub_http.rs', REALISTIC_HUB, '"access' + '_token":"' + REALISTIC_HUB + '"'),
            ('crates/rdy-cli/src/brief_text.rs', REALISTIC_HEX, '"tok' + 'en": "' + REALISTIC_HEX + '"'),
            ('crates/rdy-engine/tests/config/signing_crypto.rs', REALISTIC_B64, 'sec' + 'ret = "' + REALISTIC_B64 + '"'),
            ('crates/rdy-qa/src/fixtures/example.rs', REALISTIC_HEX, 'x'),
            ('crates/rdy-qa/fixtures-old/example.json', REALISTIC_HEX, 'x'),
            ('crates/rdy-qa/tests/golden.rs', REALISTIC_HEX, 'x'),
            ('crates/rdy-engine/tests/hub/hub_http.rs', 'rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYfX', 'x'),
            ('crates/rdy-cli/src/brief_text.rs', '0123456789abcdef01234', 'x'),
        ]
        for path, secret, line in cases:
            with self.subTest(path=path, secret=secret):
                self.assertFalse(allowlisted(self.config, path, secret, line))

    def test_shape_allowlists_apply_to_the_generic_rule_only(self):
        value = SYNTHETIC['crates/rdy-engine/tests/hub/hub_http.rs'][0]
        self.assertFalse(allowlisted(self.config, 'crates/rdy-engine/tests/hub/hub_http.rs', value, 'x',
                                     rule='another-rule'))


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


class ToolDownload(unittest.TestCase):
    # Runs f17ab534 and 6f62675a: GitHub timed out unauthenticated requests (incident 2026-11-08
    # 10:36Z to 12:32Z), urlopen raised, and the refusal named neither the tool nor the cause.
    URL = 'https://github.com/owner/tool/releases/download/v1/tool.tar.gz'

    class Body(io.BytesIO):
        def geturl(self):
            return ToolDownload.URL

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            self.close()

    def run_download(self, outcomes):
        calls, sleeps = [], []
        outcomes = iter(outcomes)

        def urlopen(request, timeout):
            calls.append((request.full_url, timeout))
            outcome = next(outcomes)
            if isinstance(outcome, BaseException):
                raise outcome
            return self.Body(outcome)
        return TOOLS.download(self.URL, urlopen=urlopen, sleep=sleeps.append), calls, sleeps

    def test_transient_network_errors_are_retried_then_the_body_is_returned(self):
        body, calls, sleeps = self.run_download([
            TimeoutError('timed out'),
            urllib.error.URLError(ConnectionResetError()),
            urllib.error.HTTPError(self.URL, 503, 'unavailable', {}, None),
            b'archive'])
        self.assertEqual(body, b'archive')
        self.assertEqual(len(calls), 4)
        self.assertEqual(sleeps, list(TOOLS.RETRY_DELAYS))

    def test_retries_are_bounded(self):
        with self.assertRaises(TimeoutError):
            self.run_download([TimeoutError('timed out')] * (len(TOOLS.RETRY_DELAYS) + 1))

    def test_a_client_error_is_not_retried(self):
        with self.assertRaises(urllib.error.HTTPError):
            self.run_download([urllib.error.HTTPError(self.URL, 404, 'not found', {}, None), b'x'])

    def test_a_retried_download_is_still_verified(self):
        payload = ToolArchive.archive([('tool', 'file')])
        manifest = {'tools': {'tool': {'version': '1', 'platforms': {'K': {
            'url': self.URL, 'sha256': '0' * 64, 'binary': 'tool'}}}}}
        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(TOOLS, 'download', return_value=payload) as download:
            with self.assertRaises(SystemExit) as refused:
                TOOLS.install(manifest, 'K', Path(out), ['tool'])
            self.assertEqual(os.listdir(out), [])
        download.assert_called_once()
        self.assertIn('tool: tool archive digest/size mismatch', str(refused.exception))
        self.assertIn('no unverified executable was run', str(refused.exception))

    def test_the_refusal_names_the_tool_and_the_network_cause(self):
        manifest = {'tools': {'gitleaks': {'version': '1', 'platforms': {'K': {
            'url': self.URL, 'sha256': '0' * 64, 'binary': 'gitleaks'}}}}}
        error = urllib.error.URLError(TimeoutError('timed out'))
        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(TOOLS, 'download', side_effect=error):
            with self.assertRaises(SystemExit) as refused:
                TOOLS.install(manifest, 'K', Path(out), ['gitleaks'])
        self.assertIn('gitleaks: download failed (TimeoutError)', str(refused.exception))



PROFILE_ROOT = 'target/llvm-cov-target'
CORRUPT_HEADER = 'error: {p}: invalid instrumentation profile data (file header is corrupt)\n'
# The stub llvm-profdata reads the part of a profile before the first "|" as its behaviour.
# Each entry is (stderr template, status); a negative status kills the stub with that signal.
STUB_TABLE = {
    'corrupt': (CORRUPT_HEADER, 1),
    'suffix': (CORRUPT_HEADER[:-1] + ' x\n', 1),
    'second': (CORRUPT_HEADER + 'note: more\n', 1),
    'nolf': (CORRUPT_HEADER[:-1], 1),
    'status2': (CORRUPT_HEADER, 2),
    'signal': (CORRUPT_HEADER, -9),
    'badmagic': ('error: {p}: invalid instrumentation profile data (bad magic)\n', 1),
    'incomplete': ('error: {p}: unrecognized instrumentation profile encoding format\n'
                   'note: Perhaps you forgot to use the --sample or --memory option?\n', 1),
    'version': ('error: {p}: unsupported instrumentation profile format version\n', 1),
    'body': ('error: {p}: malformed instrumentation profile data\n', 1),
    'perm': ('error: {p}: Permission denied\n', 1),
    'big': ('x' * 200000 + '\n', 3),
}
STUB_SOURCE = """#!{exe}
import json, os, signal, sys
with open(os.environ['STUB_LOG'], 'a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
if len(sys.argv) != 3 or sys.argv[1] != 'show':
    sys.exit(99)
path = sys.argv[2]
if not os.stat(path).st_mode & 0o400:
    os.kill(os.getpid(), signal.SIGKILL)  # SIGKILL: no macOS crash report or "quit unexpectedly" alert
with open(path, 'rb') as profile:
    kind = profile.read().split(b'|')[0].decode()
if kind == 'good':
    print('stdout noise that must be discarded')
    sys.exit(0)
template, status = json.loads({table!r})[kind]
sys.stderr.buffer.write(template.format(p=path).encode())
sys.stderr.buffer.flush()
if status < 0:
    os.kill(os.getpid(), -status)
sys.exit(status)
"""
CARGO_STUB = """#!/bin/sh
echo "$*" >> "$CARGO_LOG"
case "$*" in *--html*) kind=html;; *--json*) kind=json;; *) kind=summary; echo summary-line;; esac
[ "$kind" = "$CARGO_FAIL" ] && exit 7
exit 0
"""


def reports_step():
    """The coverage job's Reports step, parsed from nightly.yml."""
    return step_named(workflow('nightly.yml'), 'Reports')


def reports_commands():
    """(owner-read fix, profile precheck, report commands), each read from the step itself."""
    lines = command_lines(reports_step())
    fixes = [line for line in lines if line.startswith('find ') and '*.profraw' in line]
    prechecks = [line for line in lines if 'scripts/ci/set_aside_profiles.py' in line]
    reports = [line for line in lines if line.startswith('cargo llvm-cov report')]
    assert len(fixes) == 1 and len(prechecks) == 1 and len(reports) == 3, lines
    return fixes[0], prechecks[0], reports


def report_script():
    return ''.join(line + '\n' for line in reports_commands()[2])


class plz5_CoverageProfiles(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name).resolve()
        previous = os.getcwd()
        os.chdir(self.dir)
        self.addCleanup(os.chdir, previous)
        self.root = Path(PROFILE_ROOT)
        self.root.mkdir(parents=True)
        self.work = self.dir / 'runner-temp'
        self.work.mkdir()
        self.bin = self.dir / 'bin'
        self.bin.mkdir()
        self.profdata = self.bin / 'llvm-profdata'
        self.profdata.write_text(STUB_SOURCE.format(exe=sys.executable, table=json.dumps(STUB_TABLE)))
        self.profdata.chmod(0o755)
        cargo = self.bin / 'cargo'
        cargo.write_text(CARGO_STUB)
        cargo.chmod(0o755)
        self.stub_log = self.dir / 'stub.log'
        self.cargo_log = self.dir / 'cargo.log'
        env = mock.patch.dict(os.environ, {'STUB_LOG': str(self.stub_log), 'CARGO_LOG': str(self.cargo_log)})
        env.start()
        self.addCleanup(env.stop)

    def profile(self, relative, content, mode=None):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        if mode is not None:
            path.chmod(mode)
        return path

    def run_precheck(self):
        out = io.StringIO()
        with redirect_stdout(out):
            count = SET_ASIDE.set_aside_profiles(self.root, self.profdata, self.work)
        return count, out.getvalue()

    def run_reports(self, fail=''):
        env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'], CARGO_FAIL=fail)
        return subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', report_script()], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=self.dir)

    def checked_paths(self):
        if not self.stub_log.exists():
            return []
        return [json.loads(line)[1] for line in self.stub_log.read_text().splitlines()]

    def cargo_calls(self):
        return self.cargo_log.read_text().splitlines() if self.cargo_log.exists() else []

    def expected_calls(self, count):
        return ['llvm-cov report --summary-only', 'llvm-cov report --html --output-dir coverage-html',
                'llvm-cov report --json --summary-only --output-path coverage-summary.json'][:count]

    def set_aside_dirs(self):
        return sorted(self.work.iterdir())

    def check_then_report(self):
        """What the step does: reports run only when the precheck succeeds (set -e)."""
        try:
            count, out = self.run_precheck()
        except (SET_ASIDE.ProfileCheckFailed, OSError):
            return None
        return count, out, self.run_reports()

    def refused(self, kind, content=b'x'):
        path = self.profile('only.profraw', kind.encode() + b'|' + content)
        template, status = STUB_TABLE[kind]
        stderr = io.BytesIO()
        fake = mock.Mock(buffer=stderr)
        with mock.patch.object(sys, 'stderr', fake):
            with self.assertRaises(SET_ASIDE.ProfileCheckFailed) as caught:
                self.run_precheck()
        self.assertEqual(caught.exception.status, status if status > 0 else 1)
        self.assertEqual(stderr.getvalue(), template.format(p=str(path)).encode())
        self.assertTrue(path.exists())
        self.assertEqual(self.set_aside_dirs(), [])
        return path

    def test_corrupt_headers_are_set_aside(self):
        self.profile('a/x.profraw', b'corrupt|first bytes \x00\xff')
        self.profile('b c/x.profraw', b'corrupt|second bytes')
        odd = 'tab\there\né.profraw'
        self.profile(odd, b'corrupt|third')
        good = self.profile('ok.profraw', b'good|valid')
        self.profile('a/notes.txt', b'not a profile')
        (self.dir / 'elsewhere').mkdir()
        self.profile('../../elsewhere/y.profraw', b'corrupt|outside the tree')
        (self.root / 'link').symlink_to(self.dir / 'elsewhere', target_is_directory=True)
        result = self.check_then_report()
        self.assertIsNotNone(result)
        count, out, reports = result
        self.assertEqual(count, 4)
        self.assertEqual(self.checked_paths(), [
            str(self.root / 'a/x.profraw'), str(self.root / 'b c/x.profraw'),
            str(good), str(self.root / odd)])
        (kept,) = self.set_aside_dirs()
        self.assertRegex(kept.name, r'^rdy-profraw\.')
        moves = [('a/x.profraw', b'corrupt|first bytes \x00\xff'), ('b c/x.profraw', b'corrupt|second bytes'),
                 (odd, b'corrupt|third')]
        expected = ''
        for relative, content in moves:
            source, destination = self.root / relative, kept / 'rejected' / relative
            self.assertFalse(source.exists())
            self.assertEqual(destination.read_bytes(), content)
            expected += ('coverage: set aside corrupt-header profile: %s -> %s\n'
                         % (json.dumps(str(source)), json.dumps(str(destination))))
        self.assertEqual(out, expected)
        self.assertTrue(out.isascii() and out.count('\n') == 3)
        self.assertEqual(len({kept / 'rejected' / r for r, _ in moves}), 3)
        self.assertEqual(good.read_bytes(), b'good|valid')
        self.assertTrue((self.dir / 'elsewhere/y.profraw').exists())
        self.assertEqual(reports.returncode, 0, reports.stderr)
        self.assertEqual(self.cargo_calls(), self.expected_calls(3))

    def test_empty_profiles_are_left_for_the_merge(self):
        empty = self.profile('empty.profraw', b'')
        good = self.profile('sub/ok.profraw', b'good|valid')
        result = self.check_then_report()
        self.assertIsNotNone(result)
        count, out, reports = result
        self.assertEqual(count, 1)
        self.assertEqual(self.checked_paths(), [str(good)])
        self.assertTrue(empty.exists())
        self.assertEqual(empty.read_bytes(), b'')
        self.assertEqual(out, '')
        self.assertEqual(self.set_aside_dirs(), [])
        self.assertEqual(reports.returncode, 0, reports.stderr)
        self.assertEqual(self.cargo_calls(), self.expected_calls(3))

    def test_owner_read_fix_keeps_valid_profiles(self):
        write_only = self.profile('w.profraw', b'good|valid', 0o200)
        group_other = self.profile('d/g.profraw', b'good|valid', 0o066)
        chmod = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', reports_commands()[0]],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=self.dir)
        self.assertEqual(chmod.returncode, 0, chmod.stderr)
        result = self.check_then_report()
        self.assertIsNotNone(result)
        count, out, reports = result
        self.assertEqual(count, 2)
        self.assertEqual(out, '')
        for path in (write_only, group_other):
            self.assertEqual(path.read_bytes(), b'good|valid')
        self.assertEqual(self.checked_paths(), [str(group_other), str(write_only)])
        self.assertEqual(reports.returncode, 0, reports.stderr)
        self.assertEqual(self.cargo_calls(), self.expected_calls(3))

    def test_other_check_failures_stay_fatal(self):
        for kind in STUB_TABLE:
            if kind == 'corrupt':
                continue
            with self.subTest(kind=kind):
                path = self.refused(kind)
                self.assertIsNone(self.check_then_report_after_refusal())
                path.unlink()
                self.stub_log.unlink()
        self.assertEqual(self.cargo_calls(), [])

    def check_then_report_after_refusal(self):
        with redirect_stdout(io.StringIO()), mock.patch.object(sys, 'stderr', mock.Mock(buffer=io.BytesIO())):
            return self.check_then_report()

    def test_environment_and_tree_failures_stay_fatal(self):
        good = self.profile('ok.profraw', b'good|valid')
        with self.assertRaises(OSError):
            SET_ASIDE.set_aside_profiles(self.root, self.bin / 'missing-profdata', self.work)
        with self.assertRaises(OSError):
            SET_ASIDE.set_aside_profiles(self.dir / 'no-such-root', self.profdata, self.work)
        (self.root / 'dangling.profraw').symlink_to(self.dir / 'nowhere')
        with self.assertRaises(OSError):
            self.run_precheck()
        (self.root / 'dangling.profraw').unlink()
        locked = self.root / 'locked'
        locked.mkdir()
        if os.geteuid() != 0:  # root reads any directory
            locked.chmod(0)
            with self.assertRaises(OSError):
                self.run_precheck()
            locked.chmod(0o700)
        locked.rmdir()
        self.assertTrue(good.exists())
        self.assertEqual(self.cargo_calls(), [])

    def test_unstattable_entries_stay_fatal(self):
        # A readable but non-searchable directory lists its entries but refuses to stat them.
        if os.geteuid() == 0:  # root stats anything
            self.skipTest('root can stat any entry')
        nested = self.profile('nested/deep.profraw', b'good|valid')
        self.root.chmod(0o400)
        self.addCleanup(self.root.chmod, 0o700)
        with self.assertRaises(OSError):
            self.run_precheck()
        self.assertIsNone(self.check_then_report())
        self.assertEqual(self.checked_paths(), [])
        self.assertEqual(self.cargo_calls(), [])
        self.root.chmod(0o700)
        self.assertEqual(nested.read_bytes(), b'good|valid')

    def test_unmovable_profiles_stay_fatal(self):
        bad = self.profile('d/bad.profraw', b'corrupt|bytes')
        with self.assertRaises(OSError):
            SET_ASIDE.set_aside_profiles(self.root, self.profdata, self.dir / 'no-such-work-root')
        self.assertTrue(bad.exists())
        taken = self.work / 'prepared'
        (taken / 'rejected/d').mkdir(parents=True)
        (taken / 'rejected/d/bad.profraw').write_bytes(b'earlier')
        out = io.StringIO()
        with mock.patch.object(SET_ASIDE.tempfile, 'mkdtemp', return_value=str(taken)):
            with redirect_stdout(out), self.assertRaises(OSError):
                SET_ASIDE.set_aside_profiles(self.root, self.profdata, self.work)
        self.assertEqual(out.getvalue(), '')
        self.assertEqual(bad.read_bytes(), b'corrupt|bytes')
        self.assertEqual((taken / 'rejected/d/bad.profraw').read_bytes(), b'earlier')
        with mock.patch.object(SET_ASIDE.os, 'rename', side_effect=OSError('rename refused')):
            with redirect_stdout(out), self.assertRaises(OSError):
                self.run_precheck()
        self.assertEqual(out.getvalue(), '')
        self.assertEqual(bad.read_bytes(), b'corrupt|bytes')

    def run_main(self, argv=('set_aside_profiles.py',), env=None, runs=(), check=None):
        out, err = io.StringIO(), mock.Mock(buffer=io.BytesIO())
        err.write = lambda text: err.buffer.write(text.encode())
        err.flush = lambda: None
        calls = []
        pending = list(runs)

        def fake_run(args, **kwargs):
            calls.append(list(args))
            item = pending.pop(0)
            if isinstance(item, BaseException):
                raise item
            return subprocess.CompletedProcess(args, item[0], item[1], item[2])
        environment = {'RUNNER_TEMP': str(self.work)} if env is None else env
        patches = [mock.patch.object(sys, 'argv', list(argv)), mock.patch.object(sys, 'stderr', err),
                   mock.patch.dict(os.environ, environment), mock.patch.object(SET_ASIDE.subprocess, 'run', fake_run)]
        if env is not None:
            os.environ.pop('RUNNER_TEMP', None)
        if check is not None:
            patches.append(mock.patch.object(SET_ASIDE, 'set_aside_profiles', check))
        with redirect_stdout(out):
            for patch in patches:
                patch.start()
            try:
                status = SET_ASIDE.main()
            finally:
                for patch in reversed(patches):
                    patch.stop()
        return status, out.getvalue(), err.buffer.getvalue().decode(), calls

    def test_main_texts_and_arguments(self):
        status, out, err, calls = self.run_main(argv=('set_aside_profiles.py', 'extra'))
        self.assertEqual((status, out, err, calls), (1, '', 'coverage: expected no arguments\n', []))
        status, out, err, calls = self.run_main(env={})
        self.assertEqual((status, out, err, calls), (1, '', 'coverage: RUNNER_TEMP is not set\n', []))
        status, out, err, calls = self.run_main(runs=[(2, '', 'rustc exploded\n')])
        self.assertEqual((status, out, err), (1, '', 'rustc exploded\n'))
        self.assertEqual(calls, [['rustc', '--print', 'sysroot']])
        status, out, err, calls = self.run_main(runs=[(0, '/sys\n', ''), (3, '', 'no version\n')])
        self.assertEqual((status, err), (1, 'no version\n'))
        status, out, err, calls = self.run_main(runs=[(0, '/sys\n', ''), (0, 'rustc 1.99.0\nrelease: 1.99.0\n', '')])
        self.assertEqual((status, out, err), (1, '', 'coverage: rustc did not report a host\n'))
        status, out, err, calls = self.run_main(runs=[OSError('rustc: not found')])
        self.assertEqual((status, out, err), (1, '', 'rustc: not found\n'))
        seen = []

        def check(root, profdata, work):
            seen.append((root, profdata, work))
            return 5
        versions = (0, 'rustc 1.99.0\nhost: aarch64-apple-darwin\nrelease: 1.99.0\n', '')
        status, out, err, calls = self.run_main(runs=[(0, '/sys root\n', ''), versions], check=check)
        self.assertEqual((status, out, err), (0, '', ''))
        self.assertEqual(calls, [['rustc', '--print', 'sysroot'], ['rustc', '-vV']])
        self.assertEqual(seen, [(Path('target/llvm-cov-target'),
                                 Path('/sys root/lib/rustlib/aarch64-apple-darwin/bin/llvm-profdata'),
                                 self.work)])

        def refuse(root, profdata, work):
            raise SET_ASIDE.ProfileCheckFailed(3)

        def fail(root, profdata, work):
            raise PermissionError(13, 'Permission denied', 'x.profraw')
        status, out, err, calls = self.run_main(runs=[(0, '/s\n', ''), versions], check=refuse)
        self.assertEqual((status, out, err), (3, '', ''))
        status, out, err, calls = self.run_main(runs=[(0, '/s\n', ''), versions], check=fail)
        self.assertEqual((status, out, err), (1, '', "[Errno 13] Permission denied: 'x.profraw'\n"))

    def test_merge_failures_stay_fatal(self):
        self.profile('ok.profraw', b'good|valid')
        for fail, calls in [('summary', 1), ('html', 2), ('json', 3)]:
            with self.subTest(fail=fail):
                self.cargo_log.unlink(missing_ok=True)
                count, out = self.run_precheck()
                self.assertEqual((count, out), (1, ''))
                reports = self.run_reports(fail)
                self.assertEqual(reports.returncode, 7)
                self.assertEqual(self.cargo_calls(), self.expected_calls(calls))
            self.stub_log.unlink()

    def test_empty_and_all_corrupt_input_still_reach_the_first_report(self):
        count, out = self.run_precheck()
        self.assertEqual((count, out), (0, ''))
        reports = self.run_reports('summary')
        self.assertEqual(reports.returncode, 7)
        self.assertEqual(self.cargo_calls(), self.expected_calls(1))
        self.cargo_log.unlink()
        self.profile('a/1.profraw', b'corrupt|1')
        self.profile('b/1.profraw', b'corrupt|2')
        count, out = self.run_precheck()
        self.assertEqual(count, 2)
        self.assertEqual(out.count('\n'), 2)
        reports = self.run_reports('summary')
        self.assertEqual(reports.returncode, 7)
        self.assertEqual(self.cargo_calls(), self.expected_calls(1))


class ReportsStepPolicy(unittest.TestCase):
    """What the coverage Reports step must do, read from the parsed workflow."""

    def test_reports_run_after_failed_tests_and_stop_at_the_first_error(self):
        step = reports_step()
        self.assertEqual(step['if'], 'always()')
        self.assertEqual(command_lines(step)[0], 'set -euo pipefail')

    def test_owner_read_fix_then_precheck_then_every_report(self):
        fix, precheck, reports = reports_commands()
        lines = command_lines(reports_step())
        self.assertLess(lines.index(fix), lines.index(precheck))
        for report in reports:
            self.assertLess(lines.index(precheck), lines.index(report))
        self.assertIn('target/llvm-cov-target', fix)
        self.assertIn('chmod u+r', fix)
        self.assertEqual(precheck, 'python3 scripts/ci/set_aside_profiles.py')

    def test_reports_are_uploaded_even_when_the_job_failed(self):
        doc = workflow('nightly.yml')
        steps = steps_of(doc, 'coverage')
        upload = steps[steps.index(reports_step()) + 1]
        self.assertTrue(upload['uses'].startswith('actions/upload-artifact@'))
        self.assertEqual(upload['if'], 'always()')
        self.assertEqual(sorted(upload['with']['path'].split()),
                         ['coverage-html', 'coverage-summary.json', 'coverage-summary.txt'])


@unittest.skipIf(hasattr(os, 'geteuid') and os.geteuid() == 0,
                 'root reads any file; the owner-read fix is for a standard account')
class ReportsStepStandardAccount(unittest.TestCase):
    """The real Reports step, from nightly.yml, run in a bash under the current standard account.

    Only llvm-profdata, rustc and cargo are stand-ins. Everything else runs for real: bash, find,
    chmod, python3 and scripts/ci/set_aside_profiles.py, on files with real modes.
    """

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name).resolve()
        self.repo = self.dir / 'repo'
        (self.repo / 'scripts/ci').mkdir(parents=True)
        shutil.copy(ROOT / 'scripts/ci/set_aside_profiles.py', self.repo / 'scripts/ci')
        self.root = self.repo / PROFILE_ROOT
        self.root.mkdir(parents=True)
        self.work = self.dir / 'runner-temp'
        self.work.mkdir()
        self.summary = self.dir / 'summary.md'
        self.bin = self.dir / 'bin'
        self.bin.mkdir()
        sysroot = self.dir / 'sysroot'
        profdata = sysroot / 'lib/rustlib/stub-host/bin/llvm-profdata'
        profdata.parent.mkdir(parents=True)
        profdata.write_text(STUB_SOURCE.format(exe=sys.executable, table=json.dumps(STUB_TABLE)))
        profdata.chmod(0o755)
        shutil.copy(FIXTURES / 'coverage_stubs/rustc', self.bin / 'rustc')
        (self.bin / 'cargo').write_text(CARGO_STUB)
        (self.bin / 'cargo').chmod(0o755)
        self.cargo_log = self.dir / 'cargo.log'
        self.stub_log = self.dir / 'stub.log'
        self.env = dict(
            os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'], RUNNER_TEMP=str(self.work),
            GITHUB_STEP_SUMMARY=str(self.summary), STUB_SYSROOT=str(sysroot), STUB_LOG=str(self.stub_log),
            CARGO_LOG=str(self.cargo_log), CARGO_FAIL='')

    def profile(self, relative, content, mode):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        path.chmod(mode)
        return path

    def run_step(self, script=None, **env):
        script = reports_step()['run'] if script is None else script
        return subprocess.run(['bash', '-c', script], cwd=self.repo, env=dict(self.env, **env),
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def cargo_calls(self):
        return self.cargo_log.read_text().splitlines() if self.cargo_log.exists() else []

    def test_restricted_valid_profiles_survive_and_a_corrupt_one_is_set_aside(self):
        write_only = self.profile('w.profraw', b'good|1', 0o200)
        group_other = self.profile('d/g.profraw', b'good|2', 0o066)
        plain = self.profile('p.profraw', b'good|3', 0o600)
        empty = self.profile('e.profraw', b'', 0o600)
        corrupt = self.profile('c.profraw', b'corrupt|4', 0o600)
        done = self.run_step()
        self.assertEqual(done.returncode, 0, done.stderr)
        for path, content in ((write_only, b'good|1'), (group_other, b'good|2'), (plain, b'good|3')):
            self.assertEqual(path.read_bytes(), content)
        self.assertTrue(empty.exists())
        self.assertFalse(corrupt.exists())
        self.assertEqual(len(list(self.work.glob('rdy-profraw.*/rejected/c.profraw'))), 1)
        self.assertIn('set aside corrupt-header profile', done.stdout)
        self.assertEqual(len(self.cargo_calls()), 3)
        self.assertIn('## Coverage', self.summary.read_text())

    def test_without_the_owner_read_fix_a_restricted_profile_stops_the_step(self):
        self.profile('w.profraw', b'good|1', 0o200)
        fix = reports_commands()[0]
        script = '\n'.join(line for line in reports_step()['run'].splitlines() if line.strip() != fix)
        self.assertNotEqual(script, reports_step()['run'])
        done = self.run_step(script)
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(self.cargo_calls(), [])

    def test_other_check_errors_and_report_errors_fail_the_step(self):
        self.profile('bad.profraw', b'body|1', 0o600)
        done = self.run_step()
        self.assertEqual(done.returncode, 1)
        self.assertEqual(self.cargo_calls(), [])
        (self.root / 'bad.profraw').unlink()
        self.profile('ok.profraw', b'good|1', 0o200)
        for fail, calls in (('summary', 1), ('html', 2), ('json', 3)):
            with self.subTest(fail=fail):
                self.cargo_log.unlink(missing_ok=True)
                done = self.run_step(CARGO_FAIL=fail)
                self.assertEqual(done.returncode, 7)
                self.assertEqual(len(self.cargo_calls()), calls)


class CodeqlConfig(unittest.TestCase):
    CONFIG = ROOT / '.github/codeql/codeql-config.yml'

    def config(self):
        return yaml.safe_load(self.CONFIG.read_text())

    def ignored(self):
        return self.config()['paths-ignore']

    def test_codeql_ignores_exactly_the_tests_folders(self):
        self.assertEqual(self.ignored(), ['crates/*/tests/**'])
        crates = sorted(path.name for path in (ROOT / 'crates').iterdir() if path.is_dir())
        self.assertTrue(crates)
        for crate in crates:
            self.assertTrue(any(glob_matches(g, f'crates/{crate}/tests/a/b.rs') for g in self.ignored()), crate)

    def test_codeql_never_ignores_src(self):
        crates = sorted(path.name for path in (ROOT / 'crates').iterdir() if path.is_dir())
        for crate in crates:
            for path in (f'crates/{crate}/src/lib.rs', f'crates/{crate}/src/tests/mod.rs',
                         f'crates/{crate}/build.rs'):
                self.assertFalse(any(glob_matches(g, path) for g in self.ignored()), path)
        # no `paths:` allow-list narrows the scan to a subset of the product code
        self.assertNotIn('paths', self.config())

    def test_security_workflow_codeql_init_uses_the_config(self):
        inits = uses_steps(workflow('security.yml'), 'github/codeql-action/init')
        self.assertEqual(len(inits), 1)
        config_file = ROOT / inits[0]['with']['config-file']
        self.assertEqual(config_file.resolve(), self.CONFIG.resolve())


class WindowsScriptPolicy(unittest.TestCase):
    """Source checks for the Windows gate scripts.

    These stay as source checks: the scripts need Windows, and pwsh is not available on the
    machines that run these tests. The workflow side of each rule is in PlatformWorkflowPolicy.
    """

    def test_wx34_windows_runs_the_full_fast_gate(self):
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        inner = (ROOT / 'scripts/ci/windows_gate_inner.ps1').read_text()
        self.assertIn('cargo qa --skip-perf --require-all', inner)
        self.assertIn('cargo qa --tier release --platform-tier windows --budget-secs $BudgetSecs --require-all', inner)
        self.assertIn('cargo qa --tier nightly --skip-perf --require-all', inner)
        self.assertIn("[ValidateSet('release', 'nightly')][string]$Mode = ''", inner)
        # the mode and the clock reach the account as checked arguments, never as free text
        self.assertIn("$env:RDY_GATE_MODE -notin @('release', 'nightly')", gate)
        self.assertIn("$env:RDY_GATE_BUDGET_SECS -notmatch '^[1-9][0-9]{0,5}$'", gate)
        self.assertIn('cargo fetch --locked', inner)
        # an ordinary account, a private checkout, cache and profile; the credential is for this run only
        self.assertIn('New-LocalUser', gate)
        self.assertIn('Start-Process', gate)
        self.assertIn('-Credential', gate)
        self.assertIn('-LoadUserProfile', gate)
        self.assertIn('::add-mask::', gate)
        self.assertIn('RandomNumberGenerator', gate)
        self.assertIn('CARGO_HOME', gate)
        self.assertIn('RUSTUP_HOME', gate)
        self.assertIn('Remove-LocalUser', (ROOT / 'scripts/ci/windows_gate_cleanup.ps1').read_text())
        self.assertIn('IsInRole', inner)
        self.assertIn('CARGO_NET_OFFLINE', inner)
        # the password never reaches the environment, a file or the step output
        for line in gate.splitlines():
            if 'password' in line.lower():
                self.assertNotIn('GITHUB_ENV', line)
                self.assertNotIn('Out-File', line)
                self.assertNotIn('Write-Host', line)

    def test_wx34_windows_gate_quotes_paths_with_spaces(self):
        # Start-Process joins ArgumentList without quoting; C:\Program Files\Git must stay one argument
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        start = gate.index('$arguments = @(')
        block = gate[start:gate.index('Start-Process', start)]
        self.assertIn('function Quote-Arg', gate)
        for name in ('$inner', '$work', '$cargoHome', '$rustupHome', '$node', '$git'):
            self.assertIn(f'(Quote-Arg {name})', block)

    def test_wx34_windows_account_has_private_temp_and_profile_paths(self):
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        inner = (ROOT / 'scripts/ci/windows_gate_inner.ps1').read_text()
        for name in ('TEMP', 'TMP', 'APPDATA', 'LOCALAPPDATA'):
            self.assertRegex(inner, rf'\$env:{name} = ')
        self.assertIn('(Quote-Arg $home_)', gate)
        self.assertIn('[string]$PrivateHome', inner)
        self.assertIn('/inheritance:r', gate)
        self.assertIn("'/setowner', $account", gate)
        # the paths are set before the first build command
        self.assertLess(inner.index('$env:LOCALAPPDATA'), inner.index('cargo fetch'))

    def test_wx35_private_home_acl_order_keeps_the_folder_reachable(self):
        # Run 4a2db5cf: inheritance was removed after the account took ownership, so the folder had an
        # empty DACL and the administrator's grant failed ("C:\\rdy-ci\\home: Access is denied."); /C hid
        # the failure and the linker hit LNK1104. Grant first, then drop inheritance, then hand over.
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        lines = [l for l in gate.splitlines() if 'icacls' in l and '$home_' in l]
        grant = next(i for i, l in enumerate(lines) if '/grant:r' in l)
        strip = next(i for i, l in enumerate(lines) if '/inheritance:r' in l)
        owner = next(i for i, l in enumerate(lines) if '/setowner' in l)
        self.assertLess(grant, strip)
        self.assertLess(strip, owner)
        for sid in ('${account}:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F'):
            self.assertIn(sid, lines[grant])
        for line in lines:
            self.assertNotIn("'/C'", line)
        # the account proves it can create and delete a file in its TEMP before the first build
        inner = (ROOT / 'scripts/ci/windows_gate_inner.ps1').read_text()
        probe = inner.index('New-Item -ItemType File')
        self.assertLess(inner.index('$env:TMP = '), probe)
        self.assertLess(probe, inner.index('Remove-Item'))
        self.assertLess(inner.index('Remove-Item'), inner.index('cargo fetch'))

    def test_windows_report_names_failing_rows(self):
        self.assertIn('| FAIL', (ROOT / 'scripts/ci/windows_report.ps1').read_text())

    def test_windows_ci_root_is_on_the_drive_with_most_free_space(self):
        # Run 42959710193: the gate filled C: (os error 112 in about 60 tests). The root goes on the
        # fixed NTFS drive with the most free space, and every later step reads RDY_CI_ROOT.
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        self.assertIn("Win32_LogicalDisk -Filter 'DriveType = 3'", gate)
        self.assertIn("$_.FileSystem -eq 'NTFS'", gate)
        self.assertIn('Sort-Object -Property FreeSpace -Descending', gate)
        self.assertIn('"RDY_CI_ROOT=$root"', gate)
        self.assertLess(gate.index('RDY_CI_ROOT=$root'), gate.index('New-LocalUser'))
        for path in ('.github/workflows/gate.yml', '.github/workflows/release-candidate.yml',
                     'scripts/ci/windows_gate_inner.ps1', 'scripts/ci/windows_report.ps1',
                     'scripts/ci/windows_gate_cleanup.ps1', 'scripts/ci/windows_disk_report.ps1'):
            text = (ROOT / path).read_text()
            self.assertNotIn('C:\\rdy-ci', text, path)
            self.assertNotIn('C:/rdy-ci', text, path)
        self.assertEqual(gate.count("'rdy-ci'"), 2)

    def test_windows_ci_root_acl_does_not_depend_on_the_drive(self):
        # The root's ACL is set, not inherited from whichever drive holds it: the account, the
        # administrators and SYSTEM only. Grant first, then drop inheritance (wx35 order); no /C.
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        self.assertIn('foreach ($r in $roots) {', gate)
        lines = [l for l in gate.splitlines() if 'icacls' in l and '@($r,' in l]
        grant = next(i for i, l in enumerate(lines) if '/grant:r' in l)
        strip = next(i for i, l in enumerate(lines) if '/inheritance:r' in l)
        self.assertLess(grant, strip)
        for sid in ('${account}:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F'):
            self.assertIn(sid, lines[grant])
        for line in lines:
            self.assertNotIn("'/C'", line)
        self.assertLess(gate.index('New-LocalUser'), gate.index("icacls' @($r,"))
        self.assertLess(gate.index("icacls' @($r,"), gate.index('robocopy'))
        # the private home and work folder steps are unchanged
        self.assertIn("Invoke-Native 'icacls' @($home_, '/inheritance:r', '/Q')", gate)
        self.assertIn("Invoke-Native 'icacls' @($dir, '/setowner', $account, '/T', '/C', '/Q')", gate)

    def test_windows_ci_root_has_8dot3_short_names(self):
        # Run 42982134149: the hosted D: has 8.3 names off (C: has them on), so wx24, wx25b and ain4
        # saw no short spelling. The gate turns them on for the chosen volume before the root
        # exists, proves it with a probe folder, and else keeps the private home on the system drive.
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        enable = gate.index('fsutil 8dot3name set $volume 0')
        self.assertLess(gate.index('$volume = $drive.DeviceID'), enable)
        self.assertLess(enable, gate.index('New-Item -ItemType Directory -Force -Path $root'))
        self.assertIn('fsutil 8dot3name set 2', gate)
        self.assertIn('.ShortPath', gate)
        self.assertLess(gate.index('$shortNames = Test-ShortNames $root'), gate.index("$home_ = Join-Path $homeRoot 'home'"))
        self.assertIn("$homeRoot = Join-Path ($env:SystemDrive + '\\') 'rdy-ci'", gate)
        self.assertNotIn("$work = Join-Path $homeRoot", gate)
        self.assertIn("$cargoHome = Join-Path $root 'cargo'", gate)
        self.assertIn("$rustupHome = Join-Path $root 'rustup'", gate)
        report = (ROOT / 'scripts/ci/windows_disk_report.ps1').read_text()
        self.assertIn('fsutil 8dot3name query', report)
        self.assertIn('[string]$HomeRoot = $Root', report)
        self.assertEqual(gate.count('-HomeRoot $homeRoot -Label'), 3)

    def test_windows_gate_prints_disk_reports(self):
        # A disk report at the start, every 15 minutes while the gate runs, and at the end with the
        # largest leftovers in home\temp, so a full disk can be read from the job log.
        gate = (ROOT / 'scripts/ci/windows_gate.ps1').read_text()
        report = (ROOT / 'scripts/ci/windows_disk_report.ps1').read_text()
        self.assertIn("Join-Path $PSScriptRoot 'windows_disk_report.ps1'", gate)
        start = gate.index("-Label 'start'")
        during = gate.index("-Label 'during the gate'")
        end = gate.index("-Label 'end' -Leftovers")
        self.assertLess(start, gate.index('$process = Start-Process'))
        self.assertLess(gate.index('$process = Start-Process'), during)
        self.assertLess(during, end)
        self.assertIn('WaitForExit(900000)', gate)
        self.assertNotIn('-Wait -PassThru', gate)
        self.assertIn('$null = $process.Handle', gate)
        self.assertLess(gate.index('$status = $process.ExitCode'), end)
        for word in ('DriveType = 3', 'FreeSpace', "'work\\target'", "'home\\temp'", "'cargo'",
                     "'rustup'", '[switch]$Leftovers'):
            self.assertIn(word, report)
        self.assertTrue(report.rstrip().endswith('exit 0'))

    def test_wx34_windows_failures_propagate_and_publish_counts(self):
        scripts = [(ROOT / 'scripts/ci' / n).read_text() for n in (
            'windows_gate.ps1', 'windows_gate_inner.ps1', 'windows_gate_cleanup.ps1', 'windows_report.ps1')]
        for script in scripts[:2]:
            self.assertIn('$ErrorActionPreference', script)
            self.assertIn('LASTEXITCODE', script)
        self.assertIn('ExitCode', scripts[0])
        self.assertIn('exit $', scripts[1])
        self.assertIn('GITHUB_STEP_SUMMARY', scripts[3])
        self.assertIn('Failing in', scripts[3])
        for word in ('passed', 'failed', 'skipped', 'ignored'):
            self.assertIn(word, scripts[3])


GATE_TARGETS = {'linux': 'x86_64-unknown-linux-gnu', 'macos': 'aarch64-apple-darwin',
                'windows': 'x86_64-pc-windows-msvc'}
GATE_HOSTED = {'linux': 'ubuntu-26.04', 'macos': 'macos-26', 'windows': 'windows-2025'}


class GateWorkflowPolicy(unittest.TestCase):
    """What .github/workflows/gate.yml must do (the release gate, plan Task 6), read from the parsed workflow."""

    def setUp(self):
        self.doc = workflow('gate.yml')
        self.jobs = self.doc['jobs']

    def runs(self, job):
        return [step['run'] for step in self.jobs[job]['steps'] if 'run' in step]

    def test_push_to_a_release_branch_or_a_manual_run_only(self):
        on = triggers(self.doc)
        self.assertEqual(sorted(on), ['push', 'workflow_dispatch'])
        self.assertEqual(on['push'], {'branches': ['rel/**']})
        self.assertEqual(on['workflow_dispatch'], {'inputs': {'hosted': {
            'description': 'Prove the workflow on GitHub-hosted runners (never release evidence)',
            'type': 'boolean', 'default': False}}})
        self.assertEqual(self.doc['concurrency'], {'group': 'rdy-core-gate-${{ github.ref }}',
                                                   'cancel-in-progress': False})

    def test_three_jobs_on_our_machines_with_the_gate_line_as_their_limit(self):
        self.assertEqual(sorted(self.jobs), sorted(GATE_TARGETS))
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                # Linux runs on a machine of its own; the build servers share one host with other gates.
                group = 'rdy-core-gate' if name == 'linux' else 'rdy-gate'
                labels = '\"' + group + '\",\"' + name + '\"'
                self.assertEqual(job['runs-on'], "${{ inputs.hosted && '" + GATE_HOSTED[name]
                                 + "' || fromJSON('[\"self-hosted\"," + labels + "]') }}")
                # A push (release evidence) gets a 45-minute hang guard and a gate clock with a 1080-second
                # target and a 2400-second ceiling; the budget job, not these limits, checks the release
                # time. Only a manual hosted proof run gets more; gate_budget.py never reads it as evidence.
                self.assertEqual(job['timeout-minutes'], '${{ inputs.hosted && 90 || 45 }}')
                self.assertEqual(job['env']['GATE_BUDGET_SECS'], "${{ inputs.hosted && '5100' || '1080' }}")
                self.assertEqual(job['env']['GATE_CEILING_SECS'], "${{ inputs.hosted && '5100' || '2400' }}")
                self.assertEqual(job['permissions'], {'contents': 'read'})
                self.assertNotIn('environment', job)
                self.assertNotIn('needs', job)
                self.assertNotIn('continue-on-error', job)
                for step in job['steps']:
                    self.assertNotIn('continue-on-error', step)
        self.assertEqual(self.doc['permissions'], {})
        text = (WORKFLOWS / 'gate.yml').read_text()
        for word in ('secrets.', 'id-token', 'github.token', 'pull_request'):
            self.assertNotIn(word, text)
        labels = yaml.safe_load((ROOT / 'scripts/ci/actionlint.yaml').read_text())['self-hosted-runner']['labels']
        self.assertEqual(labels, ['ubuntu-26.04', 'rdy-gate', 'rdy-bench', 'rdy-core-gate'])

    def test_hosted_proof_runs_are_never_release_evidence(self):
        spec = importlib.util.spec_from_file_location('gate_budget', ROOT / 'scripts/release/gate_budget.py')
        budget = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(budget)
        self.assertEqual(budget.GATE_WORKFLOW, 'gate.yml')
        self.assertEqual(budget.GATE_EVENT, 'push')

    def test_every_gate_run_is_the_release_tier_on_the_gate_clock(self):
        clock = {'linux': 'cargo qa --tier release --budget-secs "$GATE_BUDGET_SECS" --ceiling-secs "$GATE_CEILING_SECS" '
                          '--require-all --tmp-root "$GATE_TMP"',
                 'macos': 'cargo qa --tier release --budget-secs "$GATE_BUDGET_SECS" --ceiling-secs "$GATE_CEILING_SECS" '
                          '--require-all '
                          '--tmp-root "$GATE_TMP"',
                 'windows': 'cargo qa --tier release --platform-tier windows --budget-secs $env:GATE_BUDGET_SECS '
                            '--ceiling-secs $env:GATE_CEILING_SECS --require-all --tmp-root $env:GATE_TMP'}
        for name in self.jobs:
            with self.subTest(job=name):
                gate = [step for step in self.jobs[name]['steps'] if step.get('id') == 'gate']
                self.assertEqual(len(gate), 1)
                self.assertIn(clock[name], gate[0]['run'])
                self.assertEqual(gate[0]['env'], {'CARGO_NET_OFFLINE': 'true'})
                lines = [line for run in self.runs(name) for line in run.splitlines() if 'cargo qa' in line]
                self.assertEqual(len(lines), 2, lines)
                for line in lines:
                    self.assertIn('--tier release', line)
                    self.assertIn('--budget-secs', line)
                    self.assertIn('--ceiling-secs', line)
                    self.assertIn('--require-all', line)
                    self.assertIn('--tmp-root', line)
                    for word in ('--local', '--skip-perf', '--deep', '--only', '--focus'):
                        self.assertNotIn(word, line)
                # One retry of the failed binaries, in the time left on the same clock.
                retry = step_named({'jobs': {name: self.jobs[name]}}, 'Retry failed binaries once')
                self.assertIn('--retry-failed target', retry['run'])
                self.assertRegex(retry['run'], r'GATE_CEILING_SECS.*GATE_START')
                self.assertIn("steps.gate.outputs.status != '0'", retry['if'])
        # The hosted Windows proof runs the same tier through the standard-account script.
        hosted = step_named(self.doc, 'Hosted proof under a standard account (Windows tier)')
        self.assertEqual(hosted['if'], '${{ inputs.hosted }}')
        self.assertEqual(hosted['env'], {'RDY_QA_HOSTED_CI': '1', 'RDY_GATE_MODE': 'release',
                                         'RDY_GATE_BUDGET_SECS': '${{ env.GATE_BUDGET_SECS }}'})
        self.assertIn('./scripts/ci/windows_gate.ps1', hosted['run'])

    def test_test_files_go_to_a_ram_disk_or_tmpfs_that_is_removed(self):
        linux = ' '.join(self.runs('linux'))
        self.assertIn('dir="/tmp/rdy-gate-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"', linux)
        mac = ' '.join(self.runs('macos'))
        self.assertIn('hdiutil attach -nomount ram://16777216', mac)
        self.assertIn('diskutil erasevolume APFS', mac)
        self.assertNotIn('HFS', mac.replace('never HFS+', ''))
        for name, step in (('linux', 'Remove the temp root'), ('macos', 'Remove the RAM disk'),
                           ('windows', 'Remove the temp root')):
            removal = step_named({'jobs': {name: self.jobs[name]}}, step)
            self.assertTrue(removal['if'].startswith('always()'), name)

    def test_clean_checkout_scratch_home_and_kept_target(self):
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                checkouts = [step for step in job['steps'] if str(step.get('uses', '')).startswith('actions/checkout@')]
                self.assertEqual(len(checkouts), 1)
                self.assertEqual(checkouts[0]['with'], {'persist-credentials': False})
                home = step_named({'jobs': {name: job}}, 'Scratch home and kept target')
                # the work folder's parent, from github.workspace: actionlint knows no runner.workspace
                self.assertNotIn('env', home)
                self.assertIn('GITHUB_WORKSPACE', home['run'])
                self.assertNotIn('runner.workspace', home['run'])
                self.assertIn('gate-target', home['run'])
                self.assertIn('HOME=', home['run'])
                self.assertLess(job['steps'].index(home), job['steps'].index(checkouts[0]))
                for step in job['steps']:
                    if 'uses' in step:
                        self.assertRegex(step['uses'], r'@[0-9a-f]{40}$')

    def test_counts_and_reports_are_uploaded_for_the_candidate(self):
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                target = GATE_TARGETS[name]
                steps = job['steps']
                counts = step_named({'jobs': {name: job}}, 'Fast gate counts')
                self.assertIn('scripts/release/candidate.py gate-counts --report', counts['run'])
                self.assertRegex(counts['run'], r'--commit \$(env:)?GITHUB_SHA|--commit "\$GITHUB_SHA"')
                self.assertNotIn('if', counts)
                uploads = [step for step in steps if str(step.get('uses', '')).startswith('actions/upload-artifact@')]
                self.assertEqual([u['with']['name'] for u in uploads],
                                 ['fast-gate-' + target, 'qa-report-' + target + '-a${{ github.run_attempt }}'])
                fast, report = uploads
                self.assertLess(steps.index(counts), steps.index(fast))
                self.assertNotIn('if', fast)
                self.assertEqual(fast['with']['path'], '${{ runner.temp }}/fast-gate.json')
                self.assertEqual(fast['with']['if-no-files-found'], 'error')
                self.assertIs(fast['with']['overwrite'], True)
                self.assertEqual(report['if'], 'always()')

    def test_security_runs_on_the_release_branch_too(self):
        on = triggers(workflow('security.yml'))
        self.assertEqual(on['push']['branches'], ['main', 'rel/**'])


def workflow_on(name):
    """The parsed `on:` block of a workflow (PyYAML reads the key `on` as True)."""
    doc = workflow(name)
    return doc, triggers(doc)


class CiSplit(unittest.TestCase):
    """ci.yml holds the cheap checks of a PR and of main; what runs at night is in nightly.yml."""

    def test_ci_runs_on_prs_and_main_only(self):
        doc, on = workflow_on('ci.yml')
        self.assertEqual(sorted(on), ['pull_request', 'push', 'workflow_dispatch'])
        self.assertEqual(on['push']['branches'], ['main'])
        self.assertEqual(on['pull_request'], {'branches': ['main']})
        self.assertEqual(doc['concurrency']['group'], 'ci-${{ github.event.pull_request.number || github.ref }}')

    def test_ci_runs_the_control_tests_and_core_ci_only(self):
        # The test suite runs in gate.yml and nightly.yml. The release evidence reads `Core CI` here and the
        # release gate's Linux job in gate.yml (scripts/release/evidence.py SECURITY_JOBS).
        doc, _ = workflow_on('ci.yml')
        self.assertEqual(sorted(doc['jobs']), ['policy-tests', 'required'])
        required = doc['jobs']['required']
        self.assertEqual((required['name'], required['if'], required['needs']), ('Core CI', 'always()', ['policy-tests']))
        self.assertIn("assert set(results) == {'policy-tests'}", required['steps'][0]['run'])
        text = (WORKFLOWS / 'ci.yml').read_text()
        for word in ('schedule', 'labeled', 'issues: write', 'cargo qa'):
            self.assertNotIn(word, text)
        self.assertEqual(doc['permissions'], {})

    def test_the_evidence_reads_the_gate_linux_job_by_its_name(self):
        spec = importlib.util.spec_from_file_location('eligibility', ROOT / 'scripts/release/eligibility.py')
        sys.path.insert(0, str(ROOT / 'scripts/release'))
        self.addCleanup(sys.path.remove, str(ROOT / 'scripts/release'))
        elig = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(elig)
        gate = workflow('gate.yml')['jobs']['linux']['name']
        self.assertEqual(elig.SECURITY_WORKFLOWS[gate], '.github/workflows/gate.yml')
        self.assertEqual(elig.security_events('.github/workflows/gate.yml'), ('push',))
        self.assertEqual(workflow('ci.yml')['jobs']['required']['name'], 'Core CI')
        self.assertEqual(elig.SECURITY_WORKFLOWS['Core CI'], '.github/workflows/ci.yml')
        # Every required job outside the CodeQL matrix is a job name of its own workflow.
        for name, path in elig.SECURITY_WORKFLOWS.items():
            if not name.startswith('CodeQL ('):
                names = [job.get('name') for job in workflow(Path(path).name)['jobs'].values()]
                self.assertIn(name, names)


NIGHTLY_JOBS = {
    'own': (120, None),
    'own-windows': (240, ['self-hosted', 'rdy-gate', 'windows']),
    'hosted-macos': (180, 'macos-26'),
    'hosted-windows': (300, 'windows-2025'),
    'bench': (240, ['self-hosted', 'rdy-bench']),
    'coverage': (180, 'ubuntu-24.04'),
    'report': (5, 'ubuntu-24.04'),
}


class NightlyPolicy(unittest.TestCase):
    """The nightly run: what no release reads, once a day on main, with one issue for a red night."""

    def setUp(self):
        self.doc, self.on = workflow_on('nightly.yml')
        self.text = (WORKFLOWS / 'nightly.yml').read_text()

    def test_triggers_and_concurrency(self):
        self.assertEqual(self.on, {'schedule': [{'cron': '0 3 * * *'}], 'workflow_dispatch': None})
        self.assertEqual(self.doc['concurrency'], {'group': 'rdy-core-nightly', 'cancel-in-progress': True})
        self.assertEqual(self.doc['permissions'], {})

    def test_jobs_runners_and_limits(self):
        jobs = self.doc['jobs']
        self.assertEqual(sorted(jobs), sorted(NIGHTLY_JOBS))
        for name, (minutes, runs_on) in NIGHTLY_JOBS.items():
            self.assertEqual(jobs[name]['timeout-minutes'], minutes, name)
            if runs_on is not None:
                self.assertEqual(jobs[name]['runs-on'], runs_on, name)
        own = jobs['own']
        self.assertEqual(own['runs-on'], ['self-hosted', 'rdy-gate', '${{ matrix.os }}'])
        self.assertEqual(own['strategy'], {'fail-fast': False, 'matrix': {'os': ['linux', 'macos']}})

    def test_only_the_report_job_writes_and_no_job_holds_a_secret(self):
        for name, job in self.doc['jobs'].items():
            expected = {'issues': 'write'} if name == 'report' else {'contents': 'read'}
            self.assertEqual(job['permissions'], expected, name)
            self.assertNotIn('environment', job, name)
        for word in ('secrets.', 'id-token', 'vars.'):
            self.assertNotIn(word, self.text)

    def test_whole_suite_lines(self):
        lines = [line.strip() for line in self.text.splitlines() if 'cargo qa' in line]
        suite = 'run: cargo qa --tier nightly --skip-perf --require-all'
        self.assertEqual(lines, [suite, suite, suite, 'run: cargo qa --deep --require-all'])
        # The nightly run meets the real disk: no RAM disk and no budget.
        code = '\n'.join(line for line in self.text.splitlines() if not line.lstrip().startswith('#'))
        for word in ('--tmp-root', '--budget-secs', '/dev/shm', 'hdiutil', 'RDY_GATE_BUDGET_SECS'):
            self.assertNotIn(word, code)
        hosted = step_named({'jobs': {'w': self.doc['jobs']['hosted-windows']}},
                            'Whole suite under a standard account')
        self.assertEqual(hosted['run'], './scripts/ci/windows_gate.ps1')
        self.assertEqual(hosted['env'], {'RDY_QA_HOSTED_CI': '1', 'RDY_GATE_MODE': 'nightly'})
        cleanup = self.doc['jobs']['hosted-windows']['steps'][-1]
        self.assertEqual((cleanup['if'], cleanup['run']), ('always()', './scripts/ci/windows_gate_cleanup.ps1'))

    def test_own_machines_keep_their_own_target_and_a_scratch_home(self):
        for name in ('own', 'own-windows'):
            text = json.dumps(self.doc['jobs'][name])
            self.assertIn('nightly-target', text, name)
            self.assertNotIn('gate-target', text, name)
            self.assertIn('home', text, name)

    def test_every_suite_job_uploads_its_report_under_one_prefix(self):
        for name, job in self.doc['jobs'].items():
            if name in ('coverage', 'report'):
                continue
            uploads = [s for s in job['steps'] if str(s.get('uses', '')).startswith('actions/upload-artifact@')]
            self.assertEqual(len(uploads), 1, name)
            self.assertEqual(uploads[0]['if'], 'always()', name)
            self.assertTrue(uploads[0]['with']['name'].startswith('qa-report-nightly-'), name)

    def test_one_report_job_opens_one_issue_for_a_red_night(self):
        report = self.doc['jobs']['report']
        self.assertEqual(report['needs'], sorted(set(NIGHTLY_JOBS) - {'report'}, key=list(NIGHTLY_JOBS).index))
        self.assertEqual(report['if'], 'always()')
        download, issue = report['steps']
        self.assertEqual(download['with']['pattern'], 'qa-report-nightly-*')
        self.assertEqual(issue['env']['GH_TOKEN'], '${{ github.token }}')
        self.assertEqual(issue['env']['RESULTS'], '${{ toJSON(needs) }}')
        self.assertIn("'gh', 'issue', 'create'", issue['run'])
        self.assertEqual(self.text.count('gh issue create') + self.text.count("'gh', 'issue', 'create'"), 1)

    def test_coverage_moved_whole_from_ci(self):
        job = self.doc['jobs']['coverage']
        self.assertNotIn('if', job)
        names = [s.get('name') for s in job['steps']]
        self.assertIn('Reports', names)
        self.assertIn('Pinned coverage tool', names)
        self.assertNotIn('Open an issue for a failed nightly run', names)

    def test_report_script_lists_failed_jobs_and_fail_rows(self):
        issue = self.doc['jobs']['report']['steps'][1]
        script = issue['run'].split("<<'PY'\n", 1)[1].rsplit('PY', 1)[0]
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        (root / 'reports/qa-report-nightly-bench').mkdir(parents=True)
        (root / 'reports/qa-report-nightly-bench/report.md').write_text(
            '| a | PASS |\n| perf_x | REGRESSED |\n| b | FAIL |\n')
        bin_ = root / 'bin'
        bin_.mkdir()
        (bin_ / 'gh').write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" > "$GH_ARGS"\n')
        (bin_ / 'gh').chmod(0o755)
        results = {'own': {'result': 'success'}, 'bench': {'result': 'failure'}, 'coverage': {'result': 'skipped'}}
        env = dict(os.environ, RUNNER_TEMP=str(root), RESULTS=json.dumps(results), SHA='a' * 40,
                   RUN_URL='https://example.invalid/run', GITHUB_REPOSITORY='o/r', GH_ARGS=str(root / 'args'),
                   PATH=f'{bin_}{os.pathsep}{os.environ["PATH"]}')
        subprocess.run([sys.executable, '-c', script], env=env, check=True, capture_output=True)
        args = (root / 'args').read_text()
        self.assertIn('nightly failed aaaaaaaaaaaa: bench', args)
        self.assertIn('| perf_x | REGRESSED |', args)
        self.assertIn('| b | FAIL |', args)
        self.assertNotIn('| a | PASS |', args)
        (root / 'args').unlink()
        env['RESULTS'] = json.dumps({'own': {'result': 'success'}})
        subprocess.run([sys.executable, '-c', script], env=env, check=True, capture_output=True)
        self.assertFalse((root / 'args').exists())


if __name__ == '__main__':
    unittest.main()
