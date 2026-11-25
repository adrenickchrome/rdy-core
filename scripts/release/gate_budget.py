#!/usr/bin/env python3
"""The release gate's evidence and time, read from GitHub only.

Subcommands (each refusal is one `gate budget: <reason>` line on stderr and exit 1; invalid
arguments exit 2):
  gate-run  the newest gate.yml run that passed on S: a push of S to rel/<V>. Its id goes to
            GITHUB_OUTPUT as `gate_run_id`; the candidate builds read its fast-gate artifacts.

It reads GitHub with the job's own token (GITHUB_TOKEN) through the read-only client in github.py.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import github as gh  # noqa: E402

ControllerError = cand.ControllerError
GATE_WORKFLOW = 'gate.yml'
# Release evidence comes from a push of S to rel/<V> only. A manual run (the hosted proof mode among
# them) or a run on another branch is not evidence.
GATE_EVENT = 'push'


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


def client_from(environ):
    return gh.GitHub(gh.UrllibTransport(environ.get('GITHUB_TOKEN', '')))


def main(argv=None, environ=None, client=None):
    environ = os.environ if environ is None else environ
    parser = argparse.ArgumentParser(prog='gate_budget.py', description='The release gate evidence and time.')
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    p = sub.add_parser('gate-run')
    p.add_argument('--version', required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--github-output', type=Path)
    args = parser.parse_args(argv)
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


if __name__ == '__main__':
    sys.exit(main())
