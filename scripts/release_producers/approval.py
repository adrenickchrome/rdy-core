"""The founder approval producer: `.github/workflows/release-approval.yml` and its receipt bridge.

  prepare   job `prepare-approval`: authenticates the run and the request, then shows the request's kind,
            scope, policy commit and SHA-256 and the exact comment the founder must give. It has no
            environment and uploads nothing.
  accept    job `accept-founder-approval`, after the founder's review in `release-exception-approval`:
            reads the same input bytes, repeats the authentication, requires the completed preparation and
            exactly one founder review with the exact comment, then writes the original request bytes for
            upload as `approval-request-<run_id>-1`. The run is still active here: its final conclusion is
            not required inside its own job.
  receipt   after the run completed: rebuilds the schema 2 receipt and provider projection through the
            unchanged `eligibility._approval_projection`, and prints the caller's approval locator
            `{receipt_sha256, run_id, artifact_id}`. It writes no envelope, acceptance or archive record.
  dispatch  for the caller: checks a request file and prints the complete dispatch request.

The founder is enrollment data from the protected evidence policy on main, never a source constant. GitHub
proves that the enrolled account dispatched and reviewed, not that a person reviewed in person: the founder
accepted this shared-credential limit (2026-11-08).

Run as `/usr/bin/python3 -I -B <P>/scripts/release_producers/approval.py --trusted <checkout of H> <command>`.
A refusal prints `release producer: <reason>` and exits 1.
"""
import argparse
import hashlib
import html
import json
from pathlib import Path
import shutil
import sys
import tempfile

if __name__ == '__main__':
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from release_producers import common
    _ARGV = common.trust(sys.argv)
else:
    from release_producers import common

cand, ev, gh, elig = common._release()
ControllerError = cand.ControllerError

WORKFLOW_PATH = '.github/workflows/release-approval.yml'
JOBS = ('prepare-approval', 'accept-founder-approval')
ENVIRONMENT = 'release-exception-approval'
MEMBER = elig.APPROVAL_MEMBER                 # 'approval-request.json'
REQUEST_MAX = ev.REQUEST_MAX                  # 60000 bytes
INPUT_KEYS = ('request_body',)
EVIDENCE_POLICY_PATH = elig.EVIDENCE_POLICY_PATH
LOCATOR_KEYS = elig.APPROVAL_RUN_KEYS         # the release caller's `approvals` entries
RECEIPT_FILE = 'approval-receipt.json'
PROJECTION_FILE = 'provider-projection.json'
LOCATOR_FILE = 'approval-run.json'


def artifact_name(run_id):
    """The one approval artifact the consumer accepts: attempt 1 only."""
    return 'approval-request-' + str(run_id) + '-1'


def required_comment(request_bytes):
    """The exact review comment: the SHA-256 of the exact request bytes, final LF included. No LF follows."""
    return 'APPROVE sha256:' + hashlib.sha256(request_bytes).hexdigest()


# ---------------------------------------------------------------------------------------------
# The request input
# ---------------------------------------------------------------------------------------------

def request_bytes(inputs):
    """The `request_body` input as exact bytes. The bound counts UTF-8 bytes and holds before any JSON
    parsing; nothing here runs or interpolates the text."""
    cand.closed(inputs, INPUT_KEYS, 'the approval inputs')
    body = common.utf8(inputs['request_body'], 'request_body')
    if len(body) > REQUEST_MAX:
        raise ControllerError('request_body is larger than ' + str(REQUEST_MAX) + ' bytes')
    if not body.endswith(b'\n'):
        raise ControllerError('request_body does not end with LF')
    return body


def prepare_dispatch(data):
    """For the caller: the dispatch of exact canonical request bytes, checked against both bounds."""
    request = ev.parse_approval_request(data)
    inputs = {'request_body': data.decode('utf-8')}
    if request_bytes(inputs) != data:
        raise ControllerError('the request does not survive as a dispatch input')
    common.check_dispatch(common.MAIN_REF, inputs)
    return {'ref': common.MAIN_REF, 'inputs': inputs}


# ---------------------------------------------------------------------------------------------
# Run authentication
# ---------------------------------------------------------------------------------------------

def check_channel(channel):
    """The enrolled channel, and that it describes this producer."""
    ev.check_channel_policy(channel)
    if channel['enabled'] is not True:
        raise ControllerError('the founder approval channel is disabled')
    if (channel['workflow_path'], channel['required_job_name'], channel['environment_name']) != (
            WORKFLOW_PATH, JOBS[1], ENVIRONMENT):
        raise ControllerError('the approval channel does not name this approval workflow')
    return channel


def authenticate_run(run, channel):
    """One run attempt against the enrolled channel: repository, workflow, event, ref, attempt, and the
    enrolled founder as both the dispatching and the triggering actor."""
    check_channel(channel)
    if run.repository_id != channel['repository_id'] or run.head_repository_id != channel['repository_id']:
        raise ControllerError('the approval run is not in rdy-core')
    if (run.workflow_id, run.path) != (channel['workflow_id'], channel['workflow_path']):
        raise ControllerError('the approval run is not the pinned founder approval workflow')
    if run.event != 'workflow_dispatch':
        raise ControllerError('the approval run is not a workflow_dispatch')
    if not isinstance(run.head_branch, str) or 'refs/heads/' + run.head_branch != channel['dispatch_ref']:
        raise ControllerError('the approval run is not on main')
    if run.run_attempt != 1:
        raise ControllerError('the approval run is a rerun')
    founder = channel['founder_user_id']
    if run.actor_id != founder:
        raise ControllerError('the approval run was not dispatched by the founder')
    if run.triggering_actor_id != founder:
        raise ControllerError('the approval run was not triggered by the founder')
    return run


class Authenticated(object):
    def __init__(self, facts, run, channel, request, body):
        self.facts = facts
        self.run = run
        self.channel = channel
        self.request = request
        self.body = body


def authenticate(facts, run_id, run_attempt, inputs):
    """This run, its pinned workflow bytes and its request, from protected main. `facts` is read-only."""
    body = request_bytes(inputs)
    wrapped = elig._Facts(common.ReadOnly(facts))
    main = elig._Main(wrapped)
    current = elig._parse(ev.parse_evidence_policy, wrapped.file(EVIDENCE_POLICY_PATH, main.head, ev.RECORD_MAX).data)
    channel = current.value['approval_channel']
    run = authenticate_run(wrapped.call('run_attempt', run_id, run_attempt), channel)
    main.require(run.head_sha, 'approval run head')
    workflow = wrapped.file(channel['workflow_path'], run.head_sha, gh.CONTENTS_MAX)
    if workflow.sha256 != channel['workflow_file_sha256']:
        raise ControllerError('the approval workflow is not its pinned bytes')
    source = wrapped.file(channel['workflow_path'], channel['implementation_sha'], gh.CONTENTS_MAX)
    if source.sha256 != workflow.sha256:
        raise ControllerError('the approval workflow is not its implementation pin')
    request = elig._parse(ev.parse_approval_request, body)
    main.require(request.value['policy_commit'], 'approval request policy commit')
    return Authenticated(wrapped, run, channel, request, body)


def _escape(text):
    return html.escape(text, quote=True).replace('`', '&#96;')


def summary(authenticated):
    """What the founder reviews, escaped: rendered as preformatted text, never as HTML or Markdown."""
    request = authenticated.request
    scope = json.dumps(request.value['scope'], sort_keys=True, indent=2, ensure_ascii=True)
    lines = ['kind: ' + request.value['kind'], 'policy commit: ' + request.value['policy_commit'],
             'request sha256: ' + request.sha256, 'request bytes: ' + str(request.size), 'scope: ' + scope,
             '', 'The exact review comment:', required_comment(authenticated.body)]
    return '## Founder approval request\n\n<pre>\n' + _escape('\n'.join(lines)) + '\n</pre>\n'


def prepare(facts, run_id, run_attempt, inputs):
    """Job `prepare-approval`. Returns the summary text; uploads nothing."""
    return summary(authenticate(facts, run_id, run_attempt, inputs))


def _jobs(facts, run):
    """The completed successful preparation and this one acceptance job of the same attempt."""
    jobs = facts.call('attempt_jobs', run.run_id, run.run_attempt)
    if sorted(j.name for j in jobs) != sorted(JOBS):
        raise ControllerError('the approval run is not exactly ' + JOBS[0] + ' and ' + JOBS[1])
    by_name = {j.name: j for j in jobs}
    for job in jobs:
        if job.head_sha != run.head_sha:
            raise ControllerError('an approval job did not run on the run head')
    preparation = by_name[JOBS[0]]
    if (preparation.status, preparation.conclusion) != ('completed', 'success'):
        raise ControllerError('the approval preparation did not conclude success')
    if by_name[JOBS[1]].status == 'completed':
        raise ControllerError('the approval acceptance job already completed')
    return by_name[JOBS[1]]


def check_review(reviews, channel, body):
    """Exactly one review: the enrolled founder, a User, approved, in exactly the enrolled environment, with
    exactly the required comment (no LF or added text)."""
    if len(reviews) != 1:
        raise ControllerError('the approval run needs exactly one founder review')
    review = reviews[0]
    if (review.state, review.user_id, review.user_type) != ('approved', channel['founder_user_id'], 'User'):
        raise ControllerError('the review is not the founder approval')
    if tuple(review.environments) != ((channel['environment_id'], channel['environment_name']),):
        raise ControllerError('the review is not in the pinned environment')
    if review.comment != required_comment(body):
        raise ControllerError('the founder review does not approve this request')
    return review


def accept(facts, run_id, run_attempt, inputs):
    """Job `accept-founder-approval`. Returns the original request bytes to upload, after every check."""
    authenticated = authenticate(facts, run_id, run_attempt, inputs)
    _jobs(authenticated.facts, authenticated.run)
    reviews = authenticated.facts.call('run_approvals', authenticated.run.run_id)
    check_review(reviews, authenticated.channel, authenticated.body)
    return authenticated.body


# ---------------------------------------------------------------------------------------------
# The receipt bridge (after the run completed)
# ---------------------------------------------------------------------------------------------

class RunRef(object):
    """The run reference `_approval_projection` reads: `.run_id` and `.artifact_id` only."""

    def __init__(self, run_id, artifact_id):
        self.run_id = run_id
        self.artifact_id = artifact_id


class Receipt(object):
    def __init__(self, request_bytes, receipt_bytes, receipt_sha256, projection_bytes, locator):
        self.request_bytes = request_bytes
        self.receipt_bytes = receipt_bytes
        self.receipt_sha256 = receipt_sha256
        self.projection_bytes = projection_bytes
        self.locator = locator


def rebuild(facts, main, evidence_policy, authority, run_id, artifact_id, kind):
    """The schema 2 receipt and projection of one completed approval run, through the unchanged
    `eligibility._approval_projection`. `facts` is an `eligibility._Facts`; `evidence_policy` is the parsed
    protected policy the request names; `authority` is the current channel. Nothing is archived."""
    for value, what in ((run_id, 'run id'), (artifact_id, 'artifact id')):
        if not cand.is_int(value, 1):
            raise ControllerError('the approval ' + what + ' is not a positive integer')
    ev.check_kind(kind, 'the approval')
    data, receipt, projection = elig._approval_projection(facts, main, evidence_policy, authority,
                                                          RunRef(run_id, artifact_id), {'kind': kind})
    if receipt.value['kind'] != kind:
        raise ControllerError('the approval run approved a ' + receipt.value['kind'] + ' request, not ' + kind)
    receipt_bytes = elig._canonical(receipt.value)
    locator = {'receipt_sha256': receipt.sha256, 'run_id': run_id, 'artifact_id': artifact_id}
    return Receipt(data, receipt_bytes, receipt.sha256, projection, locator)


def receipt(facts, run_id, artifact_id, kind, policy_commit):
    """`rebuild` from protected main: the evidence policy at the request's policy commit, and current
    authority from the policy at main's head."""
    ev.commit(policy_commit, 'the approval policy commit')
    wrapped = elig._Facts(common.ReadOnly(facts))
    main = elig._Main(wrapped)
    main.require(policy_commit, 'approval policy commit')
    evidence_policy = elig._parse(ev.parse_evidence_policy,
                                  wrapped.file(EVIDENCE_POLICY_PATH, policy_commit, ev.RECORD_MAX).data)
    current = elig._parse(ev.parse_evidence_policy, wrapped.file(EVIDENCE_POLICY_PATH, main.head, ev.RECORD_MAX).data)
    authority = current.value['approval_channel']
    result = rebuild(wrapped, main, evidence_policy, authority, run_id, artifact_id, kind)
    if ev.parse_approval_request(result.request_bytes).value['policy_commit'] != policy_commit:
        raise ControllerError('the approval request names another policy commit')
    return result


# ---------------------------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------------------------

def _decimal(text):
    if not (isinstance(text, str) and text.isascii() and text.isdigit() and text[0] != '0'
            and len(text) <= common.ID_DIGITS_MAX):
        raise argparse.ArgumentTypeError('not a positive decimal')
    return int(text)


def parser():
    top = argparse.ArgumentParser(prog='approval.py')
    commands = top.add_subparsers(dest='command', required=True)
    for name in ('prepare', 'accept'):
        command = commands.add_parser(name)
        command.add_argument('--event', required=True)
        if name == 'prepare':
            command.add_argument('--summary', required=True)
        else:
            command.add_argument('--out', required=True)
    command = commands.add_parser('receipt')
    command.add_argument('--run-id', type=_decimal, required=True)
    command.add_argument('--artifact-id', type=_decimal, required=True)
    command.add_argument('--kind', choices=ev.CARD_KINDS, required=True)
    command.add_argument('--policy-commit', required=True)
    command.add_argument('--out', required=True)
    command = commands.add_parser('dispatch')
    command.add_argument('--request', required=True)
    return top


def _event_inputs(path):
    event = common.read_event(path)
    if event.get('ref') != common.MAIN_REF:
        raise ControllerError('the event is not a dispatch on main')
    return event['inputs']


def run(argv):
    args = parser().parse_args(argv[1:])
    if args.command == 'dispatch':
        with open(args.request, 'rb') as handle:
            data = handle.read(REQUEST_MAX + 1)
        prepared = prepare_dispatch(data)
        sys.stdout.write(common.dispatch_request(prepared['ref'], prepared['inputs']).decode('utf-8'))
        return 0
    work = tempfile.mkdtemp(prefix='rdy-approval-')
    try:
        facts = common.github(work)
        if args.command == 'receipt':
            result = receipt(facts, args.run_id, args.artifact_id, args.kind, args.policy_commit)
            common.write_record(args.out, RECEIPT_FILE, result.receipt_bytes)
            common.write_record(args.out, PROJECTION_FILE, result.projection_bytes)
            common.write_record(args.out, LOCATOR_FILE, cand.canonical(result.locator))
            sys.stdout.write(cand.canonical(result.locator).decode('utf-8'))
            return 0
        run_id, run_attempt = common.run_identity()
        inputs = _event_inputs(args.event)
        if args.command == 'prepare':
            text = prepare(facts, run_id, run_attempt, inputs)
            with open(args.summary, 'a', encoding='utf-8') as handle:
                handle.write(text)
            sys.stdout.write(text)
            return 0
        body = accept(facts, run_id, run_attempt, inputs)
        common.write_record(args.out, MEMBER, body)
        sys.stdout.write('approved request sha256: ' + hashlib.sha256(body).hexdigest() + '\n')
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(common.main_guard(lambda: run(_ARGV)))
