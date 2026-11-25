"""The release gate's evidence lookup, with fake GitHub answers only.

Nothing here reaches the network or GitHub. Every identity below is synthetic.
"""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/release' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # the release scripts import each other: one module, one error class
    spec.loader.exec_module(module)
    return module


CANDIDATE = load('candidate')
GITHUB = load('github')
BUDGET = load('gate_budget')
ControllerError = CANDIDATE.ControllerError

V = '0.59.0'
S = '1' * 40
OTHER = '2' * 40
REPO = 'adrenickchrome/rdy-core'
REPO_ID = 1403751388


def listed(run_id, sha=S, branch='rel/' + V, event='push', status='completed', conclusion='success',
           attempt=1, workflow='gate.yml'):
    return GITHUB.ListedRunFact(run_id=run_id, run_attempt=attempt, path='.github/workflows/' + workflow,
                                head_sha=sha, head_branch=branch, event=event, status=status,
                                conclusion=conclusion, html_url='https://github.com/' + REPO + '/actions/runs/'
                                + str(run_id))


class FakeClient(object):
    """Answers workflow_runs from a table keyed by workflow file. Records every query."""

    def __init__(self, runs=None):
        self.runs = runs or {}
        self.queries = []

    def workflow_runs(self, workflow_file, **query):
        self.queries.append((workflow_file, query))
        return list(self.runs.get(workflow_file, []))


class GateRun(unittest.TestCase):
    def test_newest_passed_push_run_at_s_on_its_release_branch(self):
        client = FakeClient({'gate.yml': [listed(101), listed(205), listed(150)]})
        self.assertEqual(BUDGET.gate_run(client, V, S), 205)
        self.assertEqual(client.queries, [('gate.yml', {'branch': 'rel/' + V, 'event': 'push', 'head_sha': S,
                                                        'status': 'success'})])

    def test_rows_that_are_not_evidence_are_ignored(self):
        # GitHub filters, and the rows are checked again: another commit, branch or event, a manual run
        # (the hosted proof mode), a failed or a running run never counts as the gate at S.
        rows = [listed(301, sha=OTHER), listed(302, branch='rel/test-1'), listed(303, event='workflow_dispatch'),
                listed(304, conclusion='failure'), listed(305, status='in_progress', conclusion=None),
                listed(306, branch=None)]
        with self.assertRaises(ControllerError) as caught:
            BUDGET.gate_run(FakeClient({'gate.yml': rows}), V, S)
        self.assertEqual(str(caught.exception), 'no gate run passed at ' + S)
        self.assertEqual(BUDGET.gate_run(FakeClient({'gate.yml': rows + [listed(299)]}), V, S), 299)

    def test_no_run_refuses(self):
        with self.assertRaises(ControllerError) as caught:
            BUDGET.gate_run(FakeClient(), V, S)
        self.assertEqual(str(caught.exception), 'no gate run passed at ' + S)

    def test_malformed_version_or_commit_refuses_before_any_read(self):
        for version, commit, reason in (('0.59.1', S, 'the version is not 0.<minor>.0'),
                                        ('v0.59.0', S, 'the version is not 0.<minor>.0'),
                                        (V, 'abc', 'the commit is not a full 40-hex SHA')):
            client = FakeClient()
            with self.assertRaises(ControllerError) as caught:
                BUDGET.gate_run(client, version, commit)
            self.assertEqual(str(caught.exception), reason)
            self.assertEqual(client.queries, [])

    def test_main_writes_the_run_id_to_github_output(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'output'
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                status = BUDGET.main(['gate-run', '--version', V, '--commit', S, '--github-output', str(output)],
                                     environ={}, client=FakeClient({'gate.yml': [listed(7)]}))
            self.assertEqual((status, err.getvalue()), (0, ''))
            self.assertEqual(output.read_text(), 'gate_run_id=7\n')
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                status = BUDGET.main(['gate-run', '--version', V, '--commit', S], environ={}, client=FakeClient())
            self.assertEqual((status, err.getvalue()), (1, 'gate budget: no gate run passed at ' + S + '\n'))

    def test_main_without_a_token_refuses(self):
        err = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(err):
            status = BUDGET.main(['gate-run', '--version', V, '--commit', S], environ={})
        self.assertEqual((status, err.getvalue()), (1, 'gate budget: the GitHub transport has no credentials\n'))


class Reply(object):
    def __init__(self, status, value, headers=None):
        self.status, self.headers, self.body = status, dict(headers or {}), json.dumps(value).encode()


class Transport(object):
    def __init__(self, routes):
        self.routes, self.calls = routes, []

    def request(self, method, path, body=None, limit=None):
        self.calls.append(path)
        reply = self.routes.get(path, Reply(404, {'message': 'Not Found'}))
        return GITHUB.Response(reply.status, reply.headers, reply.body)


def run_row(run_id, **changes):
    row = {'id': run_id, 'run_attempt': 1, 'path': '.github/workflows/gate.yml', 'head_sha': S,
           'head_branch': 'rel/' + V, 'event': 'push', 'status': 'completed', 'conclusion': 'success',
           'html_url': 'https://github.com/' + REPO + '/actions/runs/' + str(run_id),
           'run_started_at': '2026-11-25T10:00:00Z', 'updated_at': '2026-11-25T10:09:30Z',
           'repository': {'id': REPO_ID}, 'head_repository': {'id': REPO_ID}}
    row.update(changes)
    return row


ROOT_PATH = '/repos/' + REPO
REPO_REPLY = Reply(200, {'id': REPO_ID, 'full_name': REPO})
LIST = ROOT_PATH + '/actions/workflows/gate.yml/runs?branch=rel%2F0.59.0&event=push'


class ClientReads(unittest.TestCase):
    def client(self, routes):
        transport = Transport(dict(routes, **{ROOT_PATH: REPO_REPLY}))
        return GITHUB.GitHub(transport, pause=lambda attempt: None), transport

    def test_workflow_runs_quotes_the_query_and_reads_every_row(self):
        page = LIST + '&per_page=100&page=1'
        client, transport = self.client({page: Reply(200, {'total_count': 2, 'workflow_runs': [
            run_row(11), run_row(12, run_attempt=3, conclusion=None, status='in_progress')]})})
        runs = client.workflow_runs('gate.yml', branch='rel/' + V, event='push')
        self.assertEqual([(r.run_id, r.run_attempt, r.conclusion) for r in runs], [(11, 1, 'success'),
                                                                                    (12, 3, None)])
        self.assertIn(page, transport.calls)

    def test_workflow_runs_refuses_rows_from_elsewhere(self):
        page = LIST + '&per_page=100&page=1'
        for row, reason in ((run_row(11, path='.github/workflows/ci.yml'), 'a run is not from gate.yml'),
                            (run_row(11, head_repository={'id': 5}), 'a run is not from rdy-core'),
                            (run_row(11, head_sha='x'), 'the head SHA is malformed')):
            client, _ = self.client({page: Reply(200, {'total_count': 1, 'workflow_runs': [row]})})
            with self.assertRaises(GITHUB.Malformed) as caught:
                client.workflow_runs('gate.yml', branch='rel/' + V, event='push')
            self.assertEqual(str(caught.exception), 'GitHub workflow runs: ' + reason)

    def test_workflow_runs_refuses_a_malformed_query(self):
        client, transport = self.client({})
        for name, query in (('../x.yml', {}), ('gate.yml', {'actor': 'x'}), ('gate.yml', {'branch': ''})):
            with self.assertRaises(ControllerError):
                client.workflow_runs(name, **query)
        self.assertEqual(transport.calls, [])

    def test_attempt_time_reads_github_start_and_update(self):
        path = ROOT_PATH + '/actions/runs/11/attempts/2'
        client, _ = self.client({path: Reply(200, run_row(11, run_attempt=2))})
        fact = client.attempt_time(11, 2)
        self.assertEqual((fact.started_at, fact.updated_at, fact.status), ('2026-11-25T10:00:00Z',
                                                                           '2026-11-25T10:09:30Z', 'completed'))
        for change, reason in (({'run_attempt': 1}, 'the answer is not run 11 attempt 2'),
                               ({'run_started_at': '2026-11-25 10:00'}, 'a run time is not UTC YYYY-MM-DDTHH:MM:SSZ'),
                               ({'updated_at': None}, 'the run update is malformed')):
            client, _ = self.client({path: Reply(200, run_row(11, **dict({'run_attempt': 2}, **change)))})
            with self.assertRaises(GITHUB.Malformed) as caught:
                client.attempt_time(11, 2)
            self.assertEqual(str(caught.exception), 'GitHub run attempt time: ' + reason)


if __name__ == '__main__':
    unittest.main()
