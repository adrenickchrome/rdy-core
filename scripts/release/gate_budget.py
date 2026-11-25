#!/usr/bin/env python3
"""The release gate's evidence and time, read from GitHub only.

Subcommands (each refusal is one `gate budget: <reason>` line on stderr and exit 1; invalid
arguments exit 2):
  gate-run  the newest gate.yml run that passed on S: a push of S to rel/<V>. Its id goes to
            GITHUB_OUTPUT as `gate_run_id`; the candidate builds read its fast-gate artifacts.
  check     the gate time of release V against its budget (the candidate's budget job). It reads
            every attempt of every gate.yml and security.yml run on rel/<V> and of every
            release-candidate.yml run on candidate/v<V>, this one included, takes the length of the
            union of their jobs' run intervals (GitHub's own job start and end times; a running job
            ends now), adds the signing and install-check lines, and compares the sum with the budget.
            Time a job waits for a runner (a gate queued behind a nightly run) is not spent: the
            table shows it as queue time, for information only.
            The budget is one number in three places that must agree: release/gate-budget.json, the
            literal in tests/ci/test_gate_budget.py and the repository variable
            RDY_GATE_BUDGET_SECONDS. Only the founder sets that variable, and only he sets
            RDY_GATE_BUDGET_OVERRIDE (`<V>:<seconds>`, for one named version). It prints one table,
            writes gate-budget.json and exits 0, or exits 31 after `E_GATE_BUDGET: ...` when the
            release is over its budget.

It reads GitHub with the job's own token (GITHUB_TOKEN) through the read-only client in github.py.
"""
import argparse
from datetime import datetime, timezone
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import github as gh  # noqa: E402

ControllerError = cand.ControllerError
GATE_WORKFLOW = 'gate.yml'
# Release evidence comes from a push of S to rel/<V> only. A manual run (the hosted proof mode among
# them) or a run on another branch is not evidence.
GATE_EVENT = 'push'
CANDIDATE_WORKFLOW = 'release-candidate.yml'
SECURITY_WORKFLOW = 'security.yml'
BUDGET_VARIABLE = 'RDY_GATE_BUDGET_SECONDS'
OVERRIDE_VARIABLE = 'RDY_GATE_BUDGET_OVERRIDE'
CONTROL_TEST = 'tests/ci/test_gate_budget.py'
BREACH_EXIT = 31
# The seven lines of release/gate-budget.json, in path order. The last two run after this check.
LINES = ('gate', 'candidate_check', 'candidate_build', 'manifest', 'budget', 'sign', 'install_check')
FOLLOWING = ('sign', 'install_check')
BUDGET_MAX = 86400
ATTEMPTS_MAX = 64
SECONDS_RE = re.compile(r'[1-9][0-9]{0,4}')
OVERRIDE_RE = re.compile(r'(0\.[1-9][0-9]*\.0):([1-9][0-9]{0,4})')
LITERAL_RE = re.compile(r'^RELEASE_GATE_SECONDS = ([0-9]+)$', re.M)
UTC_FORMAT = '%Y-%m-%dT%H:%M:%SZ'


def gate_branch(version):
    if not (isinstance(version, str) and cand.VERSION_RE.fullmatch(version)):
        raise ControllerError('the version is not 0.<minor>.0')
    return 'rel/' + version


def gate_run(client, version, commit):
    """The id of the newest gate.yml run that passed on `commit`, pushed to rel/<version>."""
    branch = gate_branch(version)
    if not (isinstance(commit, str) and cand.COMMIT_RE.fullmatch(commit)):
        raise ControllerError('the commit is not a full 40-hex SHA')
    runs = client.workflow_runs(GATE_WORKFLOW, branch=branch, event=GATE_EVENT, head_sha=commit, status='success')
    passed = [run.run_id for run in runs
              if (run.head_sha, run.head_branch, run.event, run.status, run.conclusion)
              == (commit, branch, GATE_EVENT, 'completed', 'success')]
    if not passed:
        raise ControllerError('no gate run passed at ' + commit)
    return max(passed)


def read_budget(raw):
    """The budget file: schema 1, the budget, and the seven lines that add up to it."""
    value = cand.loads(raw)
    cand.closed(value, ('schema', 'release_gate_seconds', 'lines'), 'the budget file')
    if value['schema'] != 1 or type(value['schema']) is not int:
        raise ControllerError('the budget file is not schema 1')
    total = value['release_gate_seconds']
    if not cand.is_int(total, 1) or total > BUDGET_MAX:
        raise ControllerError('the budget is not a number of seconds')
    lines = value['lines']
    if not (isinstance(lines, dict) and list(lines) == list(LINES)):
        raise ControllerError('the budget lines are not exactly ' + ', '.join(LINES))
    if not all(cand.is_int(lines[name], 1) for name in LINES):
        raise ControllerError('a budget line is not a number of seconds')
    if sum(lines.values()) != total:
        raise ControllerError('the budget lines add up to ' + str(sum(lines.values())) + ' s, not ' + str(total) + ' s')
    return value


def control_literal(text):
    """The budget literal of the control test."""
    found = LITERAL_RE.findall(text)
    if len(found) != 1:
        raise ControllerError('the control test does not hold one RELEASE_GATE_SECONDS literal')
    return int(found[0])


def agreed_budget(budget, literal, variable):
    """The one budget, when the file, the control test and the repository variable agree."""
    if not (isinstance(variable, str) and variable):
        raise ControllerError('the repository variable ' + BUDGET_VARIABLE + ' is not set')
    if not SECONDS_RE.fullmatch(variable):
        raise ControllerError('the repository variable ' + BUDGET_VARIABLE + ' is not a number of seconds')
    seconds = budget['release_gate_seconds']
    if not (seconds == literal == int(variable)):
        raise ControllerError('the budget differs: release/gate-budget.json ' + str(seconds) + ' s, ' + CONTROL_TEST
                              + ' ' + str(literal) + ' s, ' + BUDGET_VARIABLE + ' ' + variable + ' s')
    return seconds


def override_for(version, override):
    """The override seconds when RDY_GATE_BUDGET_OVERRIDE names this version, else None."""
    if not override:
        return None
    match = OVERRIDE_RE.fullmatch(override)
    if not match:
        raise ControllerError('the repository variable ' + OVERRIDE_VARIABLE + ' is not <V>:<seconds>')
    return int(match.group(2)) if match.group(1) == version else None


def utc_seconds(text):
    return int(datetime.strptime(text, UTC_FORMAT).replace(tzinfo=timezone.utc).timestamp())


def utc_text(seconds):
    return datetime.fromtimestamp(seconds, timezone.utc).strftime(UTC_FORMAT)


def job_intervals(job, now):
    """(run interval or None, queue interval) of one job. A job counts from the second a runner
    started it to the second it completed, or to now while it runs. Before that it waits in the
    queue. A job that no runner took (skipped, or cancelled in the queue) has no run interval."""
    queued = utc_seconds(job.created_at)
    if job.runner_id is None or job.started_at is None or job.status not in ('in_progress', 'completed'):
        end = utc_seconds(job.completed_at) if job.completed_at is not None else now
        return None, (queued, max(queued, end))
    start = utc_seconds(job.started_at)
    if job.status == 'completed':
        if job.completed_at is None:
            raise ControllerError('job ' + str(job.job_id) + ' completed with no end time')
        end = utc_seconds(job.completed_at)
    else:
        end = max(now, start)
    if end < start or start < queued:
        raise ControllerError('job ' + str(job.job_id) + ' has times out of order')
    return (start, end), (queued, start)


def attempt_result(jobs):
    """One word for an attempt, from its jobs: running while a job has not completed."""
    if any(job.status != 'completed' for job in jobs):
        return 'running'
    if all(job.conclusion in ('success', 'skipped') for job in jobs):
        return 'success'
    return 'failure'


def release_attempts(client, version, now):
    """Every attempt of every run that counts for `version`, with the run and queue intervals of its jobs."""
    branch = gate_branch(version)
    rows = []
    for workflow, ref in ((GATE_WORKFLOW, branch), (SECURITY_WORKFLOW, branch),
                          (CANDIDATE_WORKFLOW, 'candidate/v' + version)):
        for run in client.workflow_runs(workflow, branch=ref):
            if run.head_branch != ref:
                raise ControllerError('GitHub listed a ' + workflow + ' run of another branch')
            if run.run_attempt > ATTEMPTS_MAX:
                raise ControllerError('run ' + str(run.run_id) + ' has more than ' + str(ATTEMPTS_MAX) + ' attempts')
            for attempt in range(1, run.run_attempt + 1):
                jobs = client.attempt_job_times(run.run_id, attempt)
                ran, waited = [], []
                for job in jobs:
                    interval, queue = job_intervals(job, now)
                    if interval is not None:
                        ran.append(interval)
                    waited.append(queue)
                rows.append({'workflow': workflow, 'run_id': run.run_id, 'attempt': attempt, 'jobs': len(jobs),
                             'ran': ran, 'waited': waited, 'result': attempt_result(jobs), 'url': run.html_url})
    return sorted(rows, key=lambda row: (min([s for s, _ in row['ran'] + row['waited']] or [0]),
                                         row['run_id'], row['attempt']))


def union_seconds(intervals):
    """The length of the union of [start, end] intervals: time two runs share counts once."""
    total, last_end = 0, None
    for start, end in sorted(intervals):
        if last_end is None or start > last_end:
            total += end - start
            last_end = end
        elif end > last_end:
            total += end - last_end
            last_end = end
    return total


def check(client, version, budget, literal, variable, override, now):
    """The release's gate time against its budget: the record, with `pass` false when it is over."""
    gate_branch(version)
    seconds = agreed_budget(budget, literal, variable)
    allowed = override_for(version, override)
    limit = seconds if allowed is None else allowed
    rows = release_attempts(client, version, now)
    spent = union_seconds([interval for row in rows for interval in row['ran']])
    queued = union_seconds([interval for row in rows for interval in row['waited']])
    following = sum(budget['lines'][name] for name in FOLLOWING)
    total = spent + following
    runs = []
    for row in rows:
        times = row['ran'] or row['waited']
        runs.append({'workflow': row['workflow'], 'run_id': row['run_id'], 'attempt': row['attempt'],
                     'jobs': row['jobs'],
                     'started_at': utc_text(min(s for s, _ in times)) if times else None,
                     'ended_at': utc_text(max(e for _, e in times)) if times else None,
                     'seconds': union_seconds(row['ran']), 'queue_seconds': union_seconds(row['waited']),
                     'result': row['result'], 'url': row['url']})
    record = {
        'schema': 1, 'version': version, 'budget_seconds': seconds, 'limit_seconds': limit,
        'override': allowed is not None, 'gate_seconds': spent, 'queue_seconds': queued,
        'following_seconds': following, 'total_seconds': total, 'pass': total <= limit,
        'checked_at': utc_text(now), 'runs': runs,
    }
    return record


def table(record):
    """The run summary: one row per attempt, then the sums."""
    lines = ['## Release gate budget: v' + record['version'], '',
             '| Workflow | Run | Attempt | Jobs | Started | Ended | Job seconds | Queue seconds | Result |',
             '|---|---|---|---|---|---|---|---|---|']
    for run in record['runs']:
        lines.append('| ' + ' | '.join([run['workflow'], '[' + str(run['run_id']) + '](' + run['url'] + ')',
                                        str(run['attempt']), str(run['jobs']), run['started_at'] or '-',
                                        run['ended_at'] or '-', str(run['seconds']), str(run['queue_seconds']),
                                        run['result']]) + ' |')
    lines += ['', 'Gate time (the union of the job times above): ' + str(record['gate_seconds']) + ' s.',
              'Queue time (jobs that waited for a runner; not counted): ' + str(record['queue_seconds']) + ' s.',
              'Signing and the install check: ' + str(record['following_seconds']) + ' s.',
              'Total: ' + str(record['total_seconds']) + ' s of ' + str(record['limit_seconds']) + ' s.']
    if record['override']:
        lines.append('OVERRIDE: ' + OVERRIDE_VARIABLE + ' sets the limit of v' + record['version'] + ' to '
                     + str(record['limit_seconds']) + ' s (the budget is ' + str(record['budget_seconds']) + ' s).')
    return '\n'.join(lines) + '\n'


def breach_line(record):
    return ('E_GATE_BUDGET: v' + record['version'] + ' spent ' + str(record['total_seconds'])
            + ' s in gates; the budget is ' + str(record['limit_seconds']) + ' s')


def client_from(environ):
    return gh.GitHub(gh.UrllibTransport(environ.get('GITHUB_TOKEN', '')))


def main(argv=None, environ=None, client=None, now=None):
    environ = os.environ if environ is None else environ
    parser = argparse.ArgumentParser(prog='gate_budget.py', description='The release gate evidence and time.')
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    p = sub.add_parser('gate-run')
    p.add_argument('--version', required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--github-output', type=Path)
    p = sub.add_parser('check')
    p.add_argument('--version', required=True)
    p.add_argument('--repo', required=True)
    p.add_argument('--budget-file', type=Path, required=True)
    p.add_argument('--control-test', type=Path)
    p.add_argument('--variable', required=True)
    p.add_argument('--override', default='')
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--summary', type=Path)
    p.add_argument('--issue-body', type=Path)
    args = parser.parse_args(argv)
    if args.command == 'gate-run':
        try:
            client = client or client_from(environ)
            run_id = gate_run(client, args.version, args.commit)
            if args.github_output:
                with open(str(args.github_output), 'a') as output:
                    output.write('gate_run_id=' + str(run_id) + '\n')
            sys.stdout.write('gate run ' + str(run_id) + ' passed at ' + args.commit + '\n')
        except ControllerError as error:
            sys.stderr.write('gate budget: ' + str(error) + '\n')
            return 1
        except OSError:
            sys.stderr.write('gate budget: a file operation failed\n')
            return 1
        return 0
    return main_check(args, environ, client, now)


def write_text(path, text):
    if path is not None:
        with open(str(path), 'a', encoding='utf-8') as handle:
            handle.write(text)


def main_check(args, environ, client, now):
    """`check`: exit 0 within the budget, 31 over it, 1 on any other refusal. Every failure writes the
    issue body for the founder."""
    try:
        try:
            if args.repo != gh.REPOSITORY:
                raise ControllerError('the repository is not ' + gh.REPOSITORY)
            budget = read_budget(args.budget_file.read_bytes()[:cand.RECORD_MAX + 1])
            control = args.control_test or args.budget_file.resolve().parent.parent / CONTROL_TEST
            literal = control_literal(Path(control).read_text(encoding='utf-8'))
            client = client or client_from(environ)
            moment = int(datetime.now(timezone.utc).timestamp()) if now is None else now
            record = check(client, args.version, budget, literal, args.variable, args.override, moment)
        except OSError:
            raise ControllerError('a file operation failed')
        text = table(record)
        sys.stdout.write(text)
        write_text(args.summary, text)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        cand.write_new(args.out, cand.canonical(record))
        if not record['pass']:
            line = breach_line(record)
            sys.stderr.write(line + '\n')
            write_text(args.summary, '\n**' + line + '**\n')
            write_text(args.issue_body, line + '\n\nNo signature was made, so v' + args.version
                       + ' is not released. Only the founder closes this issue.\n\n' + text)
            return BREACH_EXIT
        return 0
    except ControllerError as error:
        line = 'gate budget: ' + str(error)
        sys.stderr.write(line + '\n')
        try:
            write_text(args.issue_body, 'The release gate budget check of v' + args.version + ' refused: '
                       + str(error) + '\n\nNo signature was made. Only the founder closes this issue.\n')
        except OSError:
            pass
        return 1


if __name__ == '__main__':
    sys.exit(main())
