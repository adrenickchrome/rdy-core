"""Failure-path checks for the CI scripts and policies, with no network or product store."""
from contextlib import redirect_stdout
import hashlib
import http.client
import importlib.util
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock
import urllib.error
import urllib.request
import urllib.response

try:
    import tomllib
except ImportError:  # Python before 3.11; CI runs 3.13
    tomllib = None

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/ci' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = load('install_tools')
SARIF = load('check_sarif')


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
    'crates/rdy-engine/tests/hub_http.rs': ['rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf',
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
FINGERPRINT_FILES = ['crates/rdy-engine/tests/signing_crypto.rs']
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
            ('crates/rdy-engine/tests/hub_http.rs', REALISTIC_HUB, '"access' + '_token":"' + REALISTIC_HUB + '"'),
            ('crates/rdy-cli/src/brief_text.rs', REALISTIC_HEX, '"tok' + 'en": "' + REALISTIC_HEX + '"'),
            ('crates/rdy-engine/tests/signing_crypto.rs', REALISTIC_B64, 'sec' + 'ret = "' + REALISTIC_B64 + '"'),
            ('crates/rdy-qa/src/fixtures/example.rs', REALISTIC_HEX, 'x'),
            ('crates/rdy-qa/fixtures-old/example.json', REALISTIC_HEX, 'x'),
            ('crates/rdy-qa/tests/golden.rs', REALISTIC_HEX, 'x'),
            ('crates/rdy-engine/tests/hub_http.rs', 'rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYfX', 'x'),
            ('crates/rdy-cli/src/brief_text.rs', '0123456789abcdef01234', 'x'),
        ]
        for path, secret, line in cases:
            with self.subTest(path=path, secret=secret):
                self.assertFalse(allowlisted(self.config, path, secret, line))

    def test_shape_allowlists_apply_to_the_generic_rule_only(self):
        value = SYNTHETIC['crates/rdy-engine/tests/hub_http.rs'][0]
        self.assertFalse(allowlisted(self.config, 'crates/rdy-engine/tests/hub_http.rs', value, 'x',
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



if __name__ == '__main__':
    unittest.main()
