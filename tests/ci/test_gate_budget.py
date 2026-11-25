"""The release gate budget: one number in three places, the hard stop of each run and each release.

The budget is 1,800 seconds. It is held here as a literal, in release/gate-budget.json and in the
repository variable RDY_GATE_BUDGET_SECONDS, which only the founder sets. The candidate's budget job
fails when the three differ. These tests fail when the file's lines do not add up to the literal, when
the timeouts of the longest path from the gate to the signature are over the budget less the install
check, when a gate run has no clock, or when a job that reaches the key runs on our own machines.

Nothing here reaches the network or GitHub. Every identity below is synthetic.
"""
import importlib.util
import io
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.github/workflows'
# The release gate budget. Changing it here alone fails these tests; changing it with the budget file
# fails every release until the founder sets RDY_GATE_BUDGET_SECONDS to match.
RELEASE_GATE_SECONDS = 1800


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


# ---------------------------------------------------------------------------------------------
# One number in three places, and the hard stops
# ---------------------------------------------------------------------------------------------

import yaml  # noqa: E402  test-only, hash-pinned in tests/ci/requirements-release-producers.txt


def workflow(name):
    return yaml.safe_load((WORKFLOWS / name).read_text())


def budget_file():
    return json.loads((ROOT / 'release/gate-budget.json').read_text())


# A push (release evidence) takes the value after `||`; only a manual hosted proof run takes the other.
PUSH_VALUE = {
    '${{ inputs.hosted && 90 || 11 }}': 11,
}
# Each job's timeout is its line of the budget file.
JOB_LINES = {
    ('gate.yml', 'linux'): 'gate', ('gate.yml', 'macos'): 'gate', ('gate.yml', 'windows'): 'gate',
    ('release-candidate.yml', 'check'): 'candidate_check',
    ('release-candidate.yml', 'build-mac'): 'candidate_build',
    ('release-candidate.yml', 'build-linux'): 'candidate_build',
    ('release-candidate.yml', 'build-windows'): 'candidate_build',
    ('release-candidate.yml', 'assemble'): 'manifest',
    ('release-candidate.yml', 'budget'): 'budget',
    ('release-candidate.yml', 'sign'): 'sign',
}


def timeout_seconds(job):
    value = job['timeout-minutes']
    if isinstance(value, str):
        value = PUSH_VALUE[value]
    assert type(value) is int, value
    return 60 * value


def longest_path(doc):
    """The longest sum of timeouts along `needs` in one workflow, in seconds."""
    jobs = doc['jobs']
    memo = {}

    def finish(name):
        if name not in memo:
            needs = jobs[name].get('needs', [])
            needs = [needs] if isinstance(needs, str) else needs
            memo[name] = timeout_seconds(jobs[name]) + max([finish(n) for n in needs] or [0])
        return memo[name]
    return max(finish(name) for name in jobs)


class OneNumber(unittest.TestCase):
    def test_the_budget_file_says_the_literal_and_its_lines_add_up_to_it(self):
        value = budget_file()
        self.assertEqual(value, {'schema': 1, 'release_gate_seconds': RELEASE_GATE_SECONDS, 'lines': {
            'gate': 660, 'candidate_check': 60, 'candidate_build': 720, 'manifest': 60, 'budget': 60, 'sign': 60,
            'install_check': 180}})
        self.assertEqual(sum(value['lines'].values()), RELEASE_GATE_SECONDS)
        self.assertEqual(BUDGET.read_budget((ROOT / 'release/gate-budget.json').read_bytes()), value)
        # the budget job reads this file's literal as the third place
        self.assertEqual(BUDGET.control_literal(Path(__file__).read_text()), RELEASE_GATE_SECONDS)

    def test_a_malformed_or_inconsistent_budget_file_refuses(self):
        good = budget_file()
        for change, reason in (
                ({'release_gate_seconds': 1900}, 'the budget lines add up to 1800 s, not 1900 s'),
                ({'schema': 2}, 'the budget file is not schema 1'),
                ({'lines': dict(good['lines'], gate=0)}, 'a budget line is not a number of seconds'),
                ({'lines': {k: v for k, v in good['lines'].items() if k != 'sign'}},
                 'the budget lines are not exactly ' + ', '.join(BUDGET.LINES)),
                ({'extra': 1}, "unknown field 'extra'")):
            with self.subTest(reason=reason):
                with self.assertRaises(ControllerError) as caught:
                    BUDGET.read_budget(json.dumps(dict(good, **change)).encode())
                self.assertEqual(str(caught.exception), reason)

    def test_the_three_places_must_agree(self):
        good = budget_file()
        self.assertEqual(BUDGET.agreed_budget(good, 1800, '1800'), 1800)
        for literal, variable, reason in (
                (1800, '', 'the repository variable RDY_GATE_BUDGET_SECONDS is not set'),
                (1800, '1800s', 'the repository variable RDY_GATE_BUDGET_SECONDS is not a number of seconds'),
                (1800, '60', 'the budget differs: release/gate-budget.json 1800 s, tests/ci/test_gate_budget.py '
                             '1800 s, RDY_GATE_BUDGET_SECONDS 60 s'),
                (2400, '2400', 'the budget differs: release/gate-budget.json 1800 s, tests/ci/test_gate_budget.py '
                               '2400 s, RDY_GATE_BUDGET_SECONDS 2400 s')):
            with self.subTest(variable=variable):
                with self.assertRaises(ControllerError) as caught:
                    BUDGET.agreed_budget(good, literal, variable)
                self.assertEqual(str(caught.exception), reason)


class HardStops(unittest.TestCase):
    def test_each_job_limit_is_its_budget_line(self):
        lines = budget_file()['lines']
        seen = set()
        for name in ('gate.yml', 'release-candidate.yml'):
            for job_id, job in workflow(name)['jobs'].items():
                with self.subTest(workflow=name, job=job_id):
                    self.assertIn((name, job_id), JOB_LINES)
                    self.assertEqual(timeout_seconds(job), lines[JOB_LINES[(name, job_id)]])
                    seen.add((name, job_id))
        self.assertEqual(seen, set(JOB_LINES))

    def test_the_longest_path_from_the_gate_to_the_signature_fits_the_budget(self):
        lines = budget_file()['lines']
        path = longest_path(workflow('gate.yml')) + longest_path(workflow('release-candidate.yml'))
        self.assertLessEqual(path, RELEASE_GATE_SECONDS - lines['install_check'])
        # check, the slowest build, assemble, budget, sign: the path the signature waits on
        self.assertEqual(longest_path(workflow('release-candidate.yml')), 60 + 720 + 60 + 60 + 60)

    def test_every_gate_run_has_the_release_tier_and_a_clock_inside_the_gate_line(self):
        gate_line = budget_file()['lines']['gate']
        doc = workflow('gate.yml')
        for job_id, job in doc['jobs'].items():
            with self.subTest(job=job_id):
                match = re.fullmatch(r"\$\{\{ inputs\.hosted && '([0-9]+)' \|\| '([0-9]+)' \}\}",
                                     job['env']['GATE_BUDGET_SECS'])
                self.assertIsNotNone(match)
                self.assertLess(int(match.group(2)), gate_line)
                for step in job['steps']:
                    for line in step.get('run', '').splitlines():
                        if 'cargo qa' in line:
                            self.assertIn('--tier release', line)
                            self.assertRegex(line, r'--budget-secs ("\$GATE_BUDGET_SECS"|\$env:GATE_BUDGET_SECS|'
                                                   r'"\$left"|\$left)( |$)')
                    if step.get('env', {}).get('RDY_GATE_MODE') is not None:
                        self.assertEqual(step['env'], {'RDY_QA_HOSTED_CI': '1', 'RDY_GATE_MODE': 'release',
                                                       'RDY_GATE_BUDGET_SECS': '${{ env.GATE_BUDGET_SECS }}'})
                    if '$left' in step.get('run', ''):
                        self.assertRegex(step['run'], r'GATE_BUDGET_SECS.*GATE_START')

    def test_no_job_that_reaches_the_key_runs_on_our_machines(self):
        for path in sorted(WORKFLOWS.glob('*.yml')):
            for job_id, job in (yaml.safe_load(path.read_text()).get('jobs') or {}).items():
                with self.subTest(workflow=path.name, job=job_id):
                    reaches = ((job.get('permissions') or {}).get('id-token') == 'write' or 'environment' in job
                               or 'secrets' in job)
                    if reaches:
                        self.assertNotIn('self-hosted', json.dumps(job.get('runs-on')))
                        self.assertNotIn('rdy-gate', json.dumps(job.get('runs-on')))

    def test_sign_waits_on_the_budget_job(self):
        jobs = workflow('release-candidate.yml')['jobs']
        self.assertEqual(jobs['sign']['needs'], ['check', 'assemble', 'budget'])
        budget = jobs['budget']
        self.assertEqual(budget['needs'], ['check', 'assemble'])
        self.assertEqual(budget['runs-on'], 'ubuntu-24.04')
        self.assertEqual(budget['permissions'], {'actions': 'read', 'contents': 'read', 'issues': 'write'})
        self.assertNotIn('environment', budget)
        run = budget['steps'][1]
        self.assertEqual(run['env'], {'GITHUB_TOKEN': '${{ github.token }}',
                                      'BUDGET_SECONDS': '${{ vars.RDY_GATE_BUDGET_SECONDS }}',
                                      'BUDGET_OVERRIDE': '${{ vars.RDY_GATE_BUDGET_OVERRIDE }}'})
        self.assertIn('scripts/release/gate_budget.py check --version "$VERSION" --repo "$GITHUB_REPOSITORY"',
                      run['run'])
        self.assertIn('--budget-file release/gate-budget.json --variable "$BUDGET_SECONDS"', run['run'])
        issue = budget['steps'][-1]
        self.assertEqual(issue['if'], 'failure()')
        self.assertIn('--label gate-budget-breach', issue['run'])
        # the budget script never runs where the key is
        self.assertNotIn('gate_budget', json.dumps(jobs['sign']))
        for job_id, job in jobs.items():
            self.assertEqual('vars.RDY_GATE_BUDGET' in json.dumps(job), job_id == 'budget', job_id)

    def test_the_guard_watches_the_budget_and_the_files_that_enforce_it(self):
        doc = workflow('budget-guard.yml')
        on = doc.get('on', doc.get(True))
        guarded = ['release/gate-budget.json', 'crates/rdy-qa/gate-allowances.json', 'tests/ci/test_gate_budget.py',
                   'scripts/release/gate_budget.py', 'crates/rdy-qa/src/bin/qa-gate.rs',
                   'crates/rdy-qa/src/gate_steps.rs', '.github/workflows/gate.yml',
                   '.github/workflows/release-candidate.yml', '.github/workflows/budget-guard.yml']
        self.assertEqual(list(on), ['push'])
        self.assertEqual(on['push'], {'paths': guarded})
        job = doc['jobs']['guard']
        self.assertEqual(job['permissions'], {'contents': 'read', 'issues': 'write'})
        self.assertEqual(doc['permissions'], {})
        step = job['steps'][-1]
        self.assertEqual(' '.join(re.search(r'guarded=\(([^)]*)\)', step['run']).group(1).split()), ' '.join(guarded))
        self.assertIn('gh issue create', step['run'])
        self.assertNotIn('continue-on-error', json.dumps(job))


# ---------------------------------------------------------------------------------------------
# The release's gate time
# ---------------------------------------------------------------------------------------------

def at(minute, second=0):
    return '2026-11-25T10:%02d:%02dZ' % (minute, second)


T0 = BUDGET.utc_seconds(at(0))


class TimedClient(FakeClient):
    """workflow_runs from a table, and attempt_time from (run id, attempt) -> (status, conclusion, start, end)."""

    def __init__(self, runs, attempts):
        FakeClient.__init__(self, runs)
        self.attempts = attempts

    def attempt_time(self, run_id, attempt):
        status, conclusion, start, end = self.attempts[(run_id, attempt)]
        return GITHUB.AttemptTimeFact(run_id, attempt, status, conclusion, start, end,
                                      'https://github.com/' + REPO + '/actions/runs/' + str(run_id))


def release(gate=(), security=(), candidate=()):
    """Runs of one release; each entry is (run id, [attempt tuples])."""
    runs, attempts = {}, {}
    for workflow_file, branch, entries in (('gate.yml', 'rel/' + V, gate), ('security.yml', 'rel/' + V, security),
                                           ('release-candidate.yml', 'candidate/v' + V, candidate)):
        rows = []
        for run_id, tries in entries:
            rows.append(listed(run_id, branch=branch, attempt=len(tries), workflow=workflow_file))
            for n, value in enumerate(tries, 1):
                attempts[(run_id, n)] = value
        runs[workflow_file] = rows
    return TimedClient(runs, attempts)


def done(start, end, conclusion='success'):
    return ('completed', conclusion, at(*start), at(*end))


class GateTime(unittest.TestCase):
    def check(self, client, variable='1800', override='', now=T0 + 3600):
        return BUDGET.check(client, V, budget_file(), RELEASE_GATE_SECONDS, variable, override, now)

    def test_two_runs_at_the_same_time_count_once(self):
        # the gate (0:00 to 0:09) and security (0:01 to 0:15) overlap; the candidate runs 0:20 to 0:34
        client = release(gate=[(1, [done((0,), (9,))])], security=[(2, [done((1,), (15,))])],
                         candidate=[(3, [done((20,), (34,))])])
        record = self.check(client)
        self.assertEqual(record['gate_seconds'], 15 * 60 + 14 * 60)
        self.assertEqual(record['following_seconds'], 240)
        self.assertEqual(record['total_seconds'], 29 * 60 + 240)
        self.assertFalse(record['pass'])
        self.assertEqual([(r['workflow'], r['run_id'], r['attempt']) for r in record['runs']],
                         [('gate.yml', 1, 1), ('security.yml', 2, 1), ('release-candidate.yml', 3, 1)])
        self.assertEqual(client.queries, [('gate.yml', {'branch': 'rel/' + V}), ('security.yml', {'branch': 'rel/' + V}),
                                          ('release-candidate.yml', {'branch': 'candidate/v' + V})])

    def test_a_failed_attempt_and_its_rerun_both_count(self):
        client = release(gate=[(1, [done((0,), (8,), 'failure'), done((10,), (18,))])],
                         candidate=[(3, [done((20,), (30,))])])
        record = self.check(client)
        self.assertEqual(record['gate_seconds'], (8 + 8 + 10) * 60)
        self.assertEqual([r['result'] for r in record['runs']], ['failure', 'success', 'success'])
        self.assertTrue(record['pass'])
        self.assertEqual(record['total_seconds'], 26 * 60 + 240)

    def test_a_cancelled_run_counts_and_a_running_attempt_ends_now(self):
        client = release(gate=[(1, [done((0,), (5,), 'cancelled')]), (2, [done((6,), (15,))])],
                         candidate=[(3, [('in_progress', None, at(16), at(16))])])
        record = self.check(client, now=T0 + 25 * 60)
        self.assertEqual(record['gate_seconds'], (5 + 9 + 9) * 60)
        self.assertEqual(record['runs'][-1]['ended_at'], at(25))
        self.assertEqual(record['runs'][-1]['result'], 'in_progress')

    def test_the_union_of_nested_and_touching_intervals(self):
        self.assertEqual(BUDGET.union_seconds([(0, 100), (10, 20), (100, 150), (200, 210)]), 160)
        self.assertEqual(BUDGET.union_seconds([]), 0)

    def test_an_override_names_one_version(self):
        client = release(gate=[(1, [done((0,), (40,))])])
        record = self.check(client, override='0.58.0:4000')
        self.assertFalse(record['override'])
        self.assertEqual(record['limit_seconds'], 1800)
        self.assertFalse(record['pass'])
        record = self.check(client, override=V + ':4000')
        self.assertTrue(record['override'])
        self.assertEqual((record['limit_seconds'], record['budget_seconds']), (4000, 1800))
        self.assertTrue(record['pass'])
        self.assertIn('OVERRIDE: RDY_GATE_BUDGET_OVERRIDE sets the limit of v0.59.0 to 4000 s', BUDGET.table(record))
        with self.assertRaises(ControllerError) as caught:
            self.check(client, override='all')
        self.assertEqual(str(caught.exception), 'the repository variable RDY_GATE_BUDGET_OVERRIDE is not <V>:<seconds>')

    def test_a_run_of_another_branch_or_a_backward_attempt_refuses(self):
        client = release(gate=[(1, [done((5,), (4,))])])
        with self.assertRaises(ControllerError) as caught:
            self.check(client)
        self.assertEqual(str(caught.exception), 'run 1 attempt 1 ends before it starts')
        client = release(gate=[(1, [done((0,), (4,))])])
        client.runs['gate.yml'] = [listed(1, branch='main')]
        with self.assertRaises(ControllerError) as caught:
            self.check(client)
        self.assertEqual(str(caught.exception), 'GitHub listed a gate.yml run of another branch')


class CheckCommand(unittest.TestCase):
    def run_check(self, client, variable='1800', override=''):
        folder = tempfile.mkdtemp()
        self.addCleanup(__import__('shutil').rmtree, folder)
        out, summary, issue = Path(folder) / 'b' / 'gate-budget.json', Path(folder) / 'summary', Path(folder) / 'i.md'
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = BUDGET.main(['check', '--version', V, '--repo', REPO, '--budget-file',
                                  str(ROOT / 'release/gate-budget.json'), '--variable', variable, '--override', override,
                                  '--out', str(out), '--summary', str(summary), '--issue-body', str(issue)],
                                 environ={}, client=client, now=T0 + 3600)
        read = lambda path: path.read_text() if path.exists() else None  # noqa: E731
        return status, stdout.getvalue(), stderr.getvalue(), read(out), read(summary), read(issue)

    def test_within_the_budget_it_passes_and_writes_the_record(self):
        status, stdout, stderr, out, summary, issue = self.run_check(release(gate=[(1, [done((0,), (9,))])],
                                                                            candidate=[(3, [done((10,), (22,))])]))
        self.assertEqual((status, stderr, issue), (0, '', None))
        record = json.loads(out)
        self.assertEqual((record['total_seconds'], record['pass']), (21 * 60 + 240, True))
        self.assertEqual(stdout, summary)
        self.assertIn('Total: 1500 s of 1800 s.', stdout)

    def test_over_the_budget_it_exits_31_and_writes_the_issue(self):
        status, stdout, stderr, out, summary, issue = self.run_check(release(gate=[(1, [done((0,), (30,))])]))
        line = 'E_GATE_BUDGET: v0.59.0 spent 2040 s in gates; the budget is 1800 s'
        self.assertEqual((status, stderr), (31, line + '\n'))
        self.assertFalse(json.loads(out)['pass'])
        self.assertTrue(issue.startswith(line + '\n\nNo signature was made'))
        self.assertIn('| gate.yml | [7]', issue.replace('[1]', '[7]'))
        self.assertIn('**' + line + '**', summary)

    def test_a_variable_that_differs_fails_and_writes_the_issue(self):
        status, _, stderr, out, _, issue = self.run_check(release(), variable='60')
        self.assertEqual(status, 1)
        self.assertTrue(stderr.startswith('gate budget: the budget differs'))
        self.assertIsNone(out)
        self.assertIn('refused: the budget differs', issue)

    def test_another_repository_refuses(self):
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            status = BUDGET.main(['check', '--version', V, '--repo', 'someone/else', '--budget-file',
                                  str(ROOT / 'release/gate-budget.json'), '--variable', '1800',
                                  '--out', '/nonexistent/x'], environ={}, client=release())
        self.assertEqual((status, stderr.getvalue()), (1, 'gate budget: the repository is not ' + REPO + '\n'))


if __name__ == '__main__':
    unittest.main()
