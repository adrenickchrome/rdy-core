"""The release gate time: two lines, each one number in three places, and job limits that only stop a hang.

The target is 1,800 s and the ceiling 2,700 s. Each is held here as a literal, in release/gate-budget.json
and in a repository variable (RDY_GATE_TARGET_SECONDS, RDY_GATE_CEILING_SECONDS) that only the founder
sets. The candidate's budget job fails when the three differ. Over the target, the release goes on and
an issue asks for the fix; over the ceiling, nothing is signed. These tests fail when the file's lines do
not add up to the target, when a job limit is under its line, when a gate run has no clock inside its
hang guard, or when a job that reaches the key runs on our own machines.

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
# The release gate's two lines. The target is the time we work to: over it, the release goes on and an issue
# asks for the fix. The ceiling is the hard stop: over it, nothing is signed. Changing either here alone fails
# these tests; changing it with the budget file fails every release until the founder sets
# RDY_GATE_TARGET_SECONDS or RDY_GATE_CEILING_SECONDS to match.
RELEASE_GATE_TARGET_SECONDS = 1800
RELEASE_GATE_CEILING_SECONDS = 2700


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

    def test_attempt_job_times_reads_each_job_queue_start_and_end(self):
        page = ROOT_PATH + '/actions/runs/11/attempts/2/jobs?per_page=100&page=1'

        def job_row(**changes):
            row = {'id': 71, 'run_id': 11, 'run_attempt': 2, 'name': 'Release gate (x86_64-unknown-linux-gnu)',
                   'status': 'completed', 'conclusion': 'success', 'runner_id': 9, 'created_at': at(0),
                   'started_at': at(3), 'completed_at': at(12), 'html_url': 'u', 'head_sha': S}
            row.update(changes)
            return row

        client, transport = self.client({page: Reply(200, {'total_count': 2, 'jobs': [
            job_row(), job_row(id=72, status='queued', conclusion=None, runner_id=None, started_at=None,
                               completed_at=None)]})})
        facts = client.attempt_job_times(11, 2)
        self.assertEqual([(f.job_id, f.runner_id, f.created_at, f.started_at, f.completed_at) for f in facts],
                         [(71, 9, at(0), at(3), at(12)), (72, None, at(0), None, None)])
        self.assertIn(page, transport.calls)
        for change, reason in (({'run_attempt': 1}, 'a job is not from run 11 attempt 2'),
                               ({'started_at': '2026-11-25 10:00'}, 'a job time is not UTC YYYY-MM-DDTHH:MM:SSZ'),
                               ({'created_at': None}, 'the job queue time is malformed'),
                               ({'runner_id': 'x'}, 'the job runner id is malformed')):
            client, _ = self.client({page: Reply(200, {'total_count': 1, 'jobs': [job_row(**change)]})})
            with self.assertRaises(GITHUB.Malformed) as caught:
                client.attempt_job_times(11, 2)
            self.assertEqual(str(caught.exception), 'GitHub job times: ' + reason)


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
    '${{ inputs.hosted && 90 || 20 }}': 20,
}
# Job limits only stop a run that hangs; the budget job checks the time. Each job's line of the budget file
# is its target, and its limit is at least that line, so a slow but healthy run finishes.
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
HANG_GUARD_MINUTES = {'gate': 20, 'candidate_build': 20, 'candidate_check': 1, 'manifest': 1, 'budget': 1, 'sign': 1}


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
        self.assertEqual(value, {'schema': 1, 'target_seconds': RELEASE_GATE_TARGET_SECONDS,
                                 'ceiling_seconds': RELEASE_GATE_CEILING_SECONDS, 'lines': {
            'gate': 660, 'candidate_check': 60, 'candidate_build': 720, 'manifest': 60, 'budget': 60, 'sign': 60,
            'install_check': 180}})
        self.assertEqual(sum(value['lines'].values()), RELEASE_GATE_TARGET_SECONDS)
        self.assertEqual(BUDGET.read_budget((ROOT / 'release/gate-budget.json').read_bytes()), value)
        # the budget job reads this file's literals as the third place
        self.assertEqual(BUDGET.control_literals(Path(__file__).read_text()),
                         (RELEASE_GATE_TARGET_SECONDS, RELEASE_GATE_CEILING_SECONDS))

    def test_a_malformed_or_inconsistent_budget_file_refuses(self):
        good = budget_file()
        for change, reason in (
                ({'target_seconds': 1900}, 'the budget lines add up to 1800 s, not the target 1900 s'),
                ({'ceiling_seconds': 1700}, 'the ceiling is under the target'),
                ({'ceiling_seconds': 0}, 'the ceiling is not a number of seconds'),
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
        self.assertEqual(BUDGET.agreed_lines(good, (1800, 2700), '1800', '2700'), (1800, 2700))
        for literals, target, ceiling, reason in (
                ((1800, 2700), '', '2700', 'the repository variable RDY_GATE_TARGET_SECONDS is not set'),
                ((1800, 2700), '1800', '', 'the repository variable RDY_GATE_CEILING_SECONDS is not set'),
                ((1800, 2700), '1800s', '2700',
                 'the repository variable RDY_GATE_TARGET_SECONDS is not a number of seconds'),
                ((1800, 2700), '60', '2700', 'the target differs: release/gate-budget.json 1800 s, '
                                             'tests/ci/test_gate_budget.py 1800 s, RDY_GATE_TARGET_SECONDS 60 s'),
                ((1800, 3600), '1800', '3600', 'the ceiling differs: release/gate-budget.json 2700 s, '
                                               'tests/ci/test_gate_budget.py 3600 s, RDY_GATE_CEILING_SECONDS 3600 s')):
            with self.subTest(reason=reason):
                with self.assertRaises(ControllerError) as caught:
                    BUDGET.agreed_lines(good, literals, target, ceiling)
                self.assertEqual(str(caught.exception), reason)
        with self.assertRaises(ControllerError) as caught:
            BUDGET.control_literals('RELEASE_GATE_TARGET_SECONDS = 1800\n')
        self.assertEqual(str(caught.exception), 'the control test does not hold one RELEASE_GATE_CEILING_SECONDS literal')


class HardStops(unittest.TestCase):
    def test_job_limits_are_hang_guards_at_or_above_their_lines(self):
        lines = budget_file()['lines']
        seen = set()
        for name in ('gate.yml', 'release-candidate.yml'):
            for job_id, job in workflow(name)['jobs'].items():
                with self.subTest(workflow=name, job=job_id):
                    self.assertIn((name, job_id), JOB_LINES)
                    line = JOB_LINES[(name, job_id)]
                    self.assertEqual(timeout_seconds(job), 60 * HANG_GUARD_MINUTES[line])
                    self.assertGreaterEqual(timeout_seconds(job), lines[line])
                    seen.add((name, job_id))
        self.assertEqual(seen, set(JOB_LINES))

    def test_the_budget_job_not_a_job_limit_holds_the_ceiling(self):
        # The longest path of job limits is longer than the target, so the target never stops a healthy run;
        # the budget job alone refuses a release over the ceiling.
        path = longest_path(workflow('gate.yml')) + longest_path(workflow('release-candidate.yml'))
        self.assertGreater(path, RELEASE_GATE_TARGET_SECONDS)
        # check, the slowest build, assemble, budget, sign: the path the signature waits on
        self.assertEqual(longest_path(workflow('release-candidate.yml')), 60 + 1200 + 60 + 60 + 60)

    def test_every_gate_run_has_the_release_tier_and_a_clock_inside_its_hang_guard(self):
        gate_line = budget_file()['lines']['gate']
        doc = workflow('gate.yml')
        for job_id, job in doc['jobs'].items():
            with self.subTest(job=job_id):
                match = re.fullmatch(r"\$\{\{ inputs\.hosted && '([0-9]+)' \|\| '([0-9]+)' \}\}",
                                     job['env']['GATE_BUDGET_SECS'])
                self.assertIsNotNone(match)
                # The gate's own clock stops a hung run before the job limit, and never before the target.
                self.assertGreater(int(match.group(2)), gate_line)
                self.assertLessEqual(int(match.group(2)), timeout_seconds(job) - 120)
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
        self.assertEqual(run['id'], 'budget')
        self.assertEqual(run['env'], {'GITHUB_TOKEN': '${{ github.token }}',
                                      'TARGET_SECONDS': '${{ vars.RDY_GATE_TARGET_SECONDS }}',
                                      'CEILING_SECONDS': '${{ vars.RDY_GATE_CEILING_SECONDS }}',
                                      'BUDGET_OVERRIDE': '${{ vars.RDY_GATE_BUDGET_OVERRIDE }}'})
        self.assertIn('scripts/release/gate_budget.py check --version "$VERSION" --repo "$GITHUB_REPOSITORY"',
                      run['run'])
        self.assertIn('--budget-file release/gate-budget.json --target-variable "$TARGET_SECONDS" '
                      '--ceiling-variable "$CEILING_SECONDS"', run['run'])
        self.assertIn('--github-output "$GITHUB_OUTPUT"', run['run'])
        over, breach = budget['steps'][-2:]
        # Over the target: one issue, and the release goes on.
        self.assertEqual(over['if'], "steps.budget.outputs.state == 'over_target'")
        self.assertNotIn('continue-on-error', over)
        self.assertRegex(over['run'], r'if ! \{ gh label create gate-time-over-target')
        self.assertIn('::warning::', over['run'])
        self.assertIn('--label gate-time-over-target', over['run'])
        self.assertIn('gate-target-issue.md', over['run'])
        # Over the ceiling: the job fails, so sign does not start.
        self.assertEqual(breach['if'], 'failure()')
        self.assertIn('--label gate-budget-breach', breach['run'])
        # the budget script never runs where the key is
        self.assertNotIn('gate_budget', json.dumps(jobs['sign']))
        for job_id, job in jobs.items():
            self.assertEqual('vars.RDY_GATE_' in json.dumps(job), job_id == 'budget', job_id)

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
    """workflow_runs from a table, and attempt_job_times from (run id, attempt) -> job tuples
    (status, conclusion, queued at, started at, completed at, runner id)."""

    def __init__(self, runs, attempts):
        FakeClient.__init__(self, runs)
        self.attempts = attempts

    def attempt_job_times(self, run_id, attempt):
        return [GITHUB.JobTimeFact(run_id * 100 + n, run_id, attempt, 'job ' + str(n), status, conclusion, runner,
                                   created, started, completed)
                for n, (status, conclusion, created, started, completed, runner)
                in enumerate(self.attempts[(run_id, attempt)], 1)]


def release(gate=(), security=(), candidate=()):
    """Runs of one release; each entry is (run id, [the job list of each attempt])."""
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


def job(start, end, conclusion='success', queued=None):
    """One job that a runner ran from `start` to `end` (minutes), queued at `queued` (default: `start`)."""
    return ('completed', conclusion, at(*(queued or start)), at(*start), at(*end), 7)


def done(start, end, conclusion='success', queued=None):
    """An attempt of one job."""
    return [job(start, end, conclusion, queued)]


class GateTime(unittest.TestCase):
    def check(self, client, target='1800', ceiling='2700', override='', now=T0 + 3600):
        return BUDGET.check(client, V, budget_file(), (RELEASE_GATE_TARGET_SECONDS, RELEASE_GATE_CEILING_SECONDS),
                            target, ceiling, override, now)

    def test_two_runs_at_the_same_time_count_once(self):
        # the gate (0:00 to 0:09) and security (0:01 to 0:15) overlap; the candidate runs 0:20 to 0:34
        client = release(gate=[(1, [done((0,), (9,))])], security=[(2, [done((1,), (15,))])],
                         candidate=[(3, [done((20,), (34,))])])
        record = self.check(client)
        self.assertEqual(record['gate_seconds'], 15 * 60 + 14 * 60)
        self.assertEqual(record['following_seconds'], 240)
        self.assertEqual(record['total_seconds'], 29 * 60 + 240)
        self.assertEqual((record['state'], record['pass']), ('over_target', True))
        self.assertEqual([(line['line'], line['seconds']) for line in record['lines']][:1], [('gate', 9 * 60)])
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
                         candidate=[(3, [[('in_progress', None, at(16), at(16), None, 7)]])])
        record = self.check(client, now=T0 + 25 * 60)
        self.assertEqual(record['gate_seconds'], (5 + 9 + 9) * 60)
        self.assertEqual(record['runs'][-1]['ended_at'], at(25))
        self.assertEqual(record['runs'][-1]['result'], 'running')

    def test_time_in_the_queue_is_shown_and_not_spent(self):
        # The gate waits 0:00 to 0:20 behind a nightly run on our machine, then runs 0:20 to 0:29. A second job
        # of the same attempt is still queued at "now" (0:31), and a third was skipped and never took a runner.
        client = release(gate=[(1, [[job((20,), (29,), queued=(0,)),
                                     ('queued', None, at(25), None, None, None),
                                     ('completed', 'skipped', at(29), at(29), at(29), None)]])],
                         candidate=[(3, [done((30,), (31,), queued=(29, 30))])])
        record = self.check(client, now=T0 + 31 * 60)
        self.assertEqual(record['gate_seconds'], (9 + 1) * 60)
        self.assertEqual(record['queue_seconds'], (20 + 6) * 60)  # 0:00-0:20 and 0:25-0:31; 0:29:30-0:30 is inside
        gate = record['runs'][0]
        self.assertEqual((gate['jobs'], gate['seconds'], gate['queue_seconds'], gate['result']),
                         (3, 9 * 60, 26 * 60, 'running'))
        self.assertEqual((gate['started_at'], gate['ended_at']), (at(20), at(29)))
        self.assertTrue(record['pass'])
        self.assertIn('Queue time (jobs that waited for a runner; not counted): ' + str(record['queue_seconds']) + ' s.',
                      BUDGET.table(record))

    def test_a_job_cancelled_in_the_queue_spends_nothing(self):
        client = release(gate=[(1, [[('completed', 'cancelled', at(0), at(0), at(40), None)]])])
        record = self.check(client)
        self.assertEqual((record['gate_seconds'], record['queue_seconds'], record['pass']), (0, 40 * 60, True))

    def test_the_union_of_nested_and_touching_intervals(self):
        self.assertEqual(BUDGET.union_seconds([(0, 100), (10, 20), (100, 150), (200, 210)]), 160)
        self.assertEqual(BUDGET.union_seconds([]), 0)

    def test_the_target_and_the_ceiling_are_two_lines(self):
        # 240 s of signing and the install check follow; the gate time is the rest.
        for gate_end, state in (((26,), 'on_target'), ((26, 1), 'over_target'), ((41,), 'over_target'),
                                ((41, 1), 'over_ceiling')):
            with self.subTest(gate_end=gate_end):
                record = self.check(release(gate=[(1, [done((0,), gate_end)])]))
                self.assertEqual(record['state'], state)
                self.assertEqual(record['pass'], state != 'over_ceiling')
                self.assertEqual((record['target_seconds'], record['ceiling_seconds'], record['limit_seconds']),
                                 (1800, 2700, 2700))

    def test_the_table_shows_each_line_against_its_target(self):
        client = release(gate=[(1, [[job((0,), (12,))]])],
                         candidate=[(3, [[('completed', 'success', at(12), at(12), at(13), 7)]])])
        record = self.check(client)
        gate = [line for line in record['lines'] if line['line'] == 'gate'][0]
        self.assertEqual((gate['target_seconds'], gate['seconds']), (660, 720))
        self.assertEqual([line['seconds'] for line in record['lines'] if line['line'] in BUDGET.FOLLOWING], [None, None])
        text = BUDGET.table(record)
        self.assertIn('| gate | 660 | 720 (over) |', text)
        self.assertIn('| sign | 60 | after this check |', text)
        self.assertIn('Total: ' + str(record['total_seconds']) + ' s. Target 1800 s; ceiling 2700 s.', text)
        self.assertEqual(BUDGET.line_of('release-candidate.yml', 'Native candidate (x86_64-pc-windows-msvc)'),
                         'candidate_build')
        self.assertEqual(BUDGET.line_of('release-candidate.yml', 'Release gate budget'), 'budget')
        self.assertIsNone(BUDGET.line_of('security.yml', 'Core security'))

    def test_an_override_names_one_version_and_raises_its_ceiling(self):
        client = release(gate=[(1, [done((0,), (50,))])])
        record = self.check(client, override='0.58.0:4000')
        self.assertFalse(record['override'])
        self.assertEqual(record['limit_seconds'], 2700)
        self.assertEqual(record['state'], 'over_ceiling')
        record = self.check(client, override=V + ':4000')
        self.assertTrue(record['override'])
        self.assertEqual((record['limit_seconds'], record['ceiling_seconds']), (4000, 2700))
        self.assertEqual((record['state'], record['pass']), ('over_target', True))
        self.assertIn('OVERRIDE: RDY_GATE_BUDGET_OVERRIDE sets the ceiling of v0.59.0 to 4000 s', BUDGET.table(record))
        with self.assertRaises(ControllerError) as caught:
            self.check(client, override='all')
        self.assertEqual(str(caught.exception), 'the repository variable RDY_GATE_BUDGET_OVERRIDE is not <V>:<seconds>')

    def test_a_run_of_another_branch_or_a_backward_attempt_refuses(self):
        client = release(gate=[(1, [done((5,), (4,))])])
        with self.assertRaises(ControllerError) as caught:
            self.check(client)
        self.assertEqual(str(caught.exception), 'job 101 has times out of order')
        client = release(gate=[(1, [[('completed', 'success', at(0), at(1), None, 7)]])])
        with self.assertRaises(ControllerError) as caught:
            self.check(client)
        self.assertEqual(str(caught.exception), 'job 101 completed with no end time')
        client = release(gate=[(1, [done((0,), (4,))])])
        client.runs['gate.yml'] = [listed(1, branch='main')]
        with self.assertRaises(ControllerError) as caught:
            self.check(client)
        self.assertEqual(str(caught.exception), 'GitHub listed a gate.yml run of another branch')


class CheckCommand(unittest.TestCase):
    def run_check(self, client, target='1800', ceiling='2700', override=''):
        folder = Path(tempfile.mkdtemp())
        self.addCleanup(__import__('shutil').rmtree, str(folder))
        files = {name: folder / name for name in ('summary', 'breach.md', 'target.md', 'output')}
        out = folder / 'b' / 'gate-budget.json'
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = BUDGET.main(['check', '--version', V, '--repo', REPO, '--budget-file',
                                  str(ROOT / 'release/gate-budget.json'), '--target-variable', target,
                                  '--ceiling-variable', ceiling, '--override', override, '--out', str(out),
                                  '--summary', str(files['summary']), '--issue-body', str(files['breach.md']),
                                  '--target-issue-body', str(files['target.md']),
                                  '--github-output', str(files['output'])],
                                 environ={}, client=client, now=T0 + 3600)
        read = lambda path: path.read_text() if path.exists() else None  # noqa: E731
        return dict({name: read(path) for name, path in files.items()}, status=status, stdout=stdout.getvalue(),
                    stderr=stderr.getvalue(), out=read(out))

    def test_on_target_it_passes_silently(self):
        r = self.run_check(release(gate=[(1, [done((0,), (9,))])], candidate=[(3, [done((10,), (22,))])]))
        self.assertEqual((r['status'], r['stderr'], r['breach.md'], r['target.md']), (0, '', None, None))
        self.assertEqual(r['output'], 'state=on_target\n')
        record = json.loads(r['out'])
        self.assertEqual((record['total_seconds'], record['state'], record['pass']), (21 * 60 + 240, 'on_target', True))
        self.assertEqual(r['stdout'], r['summary'])
        self.assertIn('Total: 1500 s. Target 1800 s; ceiling 2700 s.', r['stdout'])

    def test_over_the_target_it_passes_and_writes_the_target_issue(self):
        r = self.run_check(release(gate=[(1, [done((0,), (30,))])]))
        line = 'Gate time over target: v0.59.0 spent 2040 s in gates; the target is 1800 s'
        self.assertEqual((r['status'], r['stderr'], r['breach.md']), (0, '', None))
        self.assertEqual(r['output'], 'state=over_target\n')
        self.assertTrue(r['target.md'].startswith(line + '\n\nThe release goes on: the ceiling is 2700 s.'))
        self.assertIn('| gate.yml | [1]', r['target.md'])
        self.assertIn('**' + line + '**', r['summary'])
        self.assertTrue(json.loads(r['out'])['pass'])

    def test_over_the_ceiling_it_exits_31_and_writes_the_breach_issue(self):
        r = self.run_check(release(gate=[(1, [done((0,), (50,))])]))
        line = 'E_GATE_BUDGET: v0.59.0 spent 3240 s in gates; the ceiling is 2700 s'
        self.assertEqual((r['status'], r['stderr'], r['target.md']), (31, line + '\n', None))
        self.assertEqual(r['output'], 'state=over_ceiling\n')
        self.assertFalse(json.loads(r['out'])['pass'])
        self.assertTrue(r['breach.md'].startswith(line + '\n\nNo signature was made'))
        self.assertIn('| gate.yml | [1]', r['breach.md'])
        self.assertIn('**' + line + '**', r['summary'])

    def test_a_variable_that_differs_fails_and_writes_the_issue(self):
        for target, ceiling, reason in (('60', '2700', 'the target differs'), ('1800', '1800', 'the ceiling differs')):
            with self.subTest(reason=reason):
                r = self.run_check(release(), target=target, ceiling=ceiling)
                self.assertEqual(r['status'], 1)
                self.assertTrue(r['stderr'].startswith('gate budget: ' + reason))
                self.assertIsNone(r['out'])
                self.assertIsNone(r['output'])
                self.assertIn('refused: ' + reason, r['breach.md'])

    def test_another_repository_refuses(self):
        stderr = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(stderr):
            status = BUDGET.main(['check', '--version', V, '--repo', 'someone/else', '--budget-file',
                                  str(ROOT / 'release/gate-budget.json'), '--target-variable', '1800',
                                  '--ceiling-variable', '2700', '--out', '/nonexistent/x'],
                                 environ={}, client=release())
        self.assertEqual((status, stderr.getvalue()), (1, 'gate budget: the repository is not ' + REPO + '\n'))


if __name__ == '__main__':
    unittest.main()
