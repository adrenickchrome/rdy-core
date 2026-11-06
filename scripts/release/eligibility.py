#!/usr/bin/env python3
"""Authenticated release eligibility and recovery facts, in stages.

Before AWS credentials there is no archive access. `qualify_new(request, facts)` checks a new
reservation of version V, and `preliminary_recovery(request, facts)` checks an existing one, its
original release record E and tuple T, and any publication, without the new-version predicate. The
caller selects recovery before new eligibility. Each returns an immutable `Preliminary`: its cards are
`ProvisionalCard`s (fresh GitHub proof that says nothing about the archive) or `PendingCard`s (an
archive reference that only the signer can resolve). It permits only the reservation bookkeeping. It
is not a `Qualified` value and it carries no signing or promotion capability.

`authenticate_recovery(request, facts)` is the signer phase. It runs only after the sole STS exchange,
over a fact source whose `approval_record` reads the authenticated table. It resolves every card and
returns a `Qualified`. Only a `Qualified` can reach KMS.

`authenticate_published(request, facts)` is historical reconciliation. It authenticates the reservation,
E, T, the caller and the publication state, and it reports the current approval status without acting
on it. Its `Reconciliation` result is not eligibility for anything.

A new reservation records how it knows the code scanning alerts in `alert_observation`. `FreshAlertRead` is
a real read by this job. `InheritedAlerts` is the reserve job's earlier observation: it has no alert
permission, so it names the exact successful eligibility job of this request and run attempt, and it holds
no alert list. Neither one permits a Sign. `check_alerts` is the live read that the signer repeats before a
new Sign. E stays the original. A retained signature or a published observation sends no Sign and needs no
new read.

None of them writes GitHub state, acquires a grant or contacts KMS. `facts` is a read-only source of
GitHub-owned facts (the read methods of `github.GitHub`, with its fact types and refusals) and a few
more reads:

  artifact_member(artifact_fact, name, limit) -> bytes   the one ZIP member `name` of an authenticated
                                                          artifact, after its digest check
  release_trust(version, policy_commit, signing) -> dict  the `qa-release-trust select` result for V
                                                          over release-keys/ at the policy commit
  approval_responses(run_id, artifact_id) -> list        the nine {kind, sha256, bytes} source-response
                                                          entries of a founder approval run (audit
                                                          data, never the approval's identity)
  approval_record(receipt_sha256) -> bytes or None        signer phase only: the archived approval
                                                          envelope, or None for authenticated absence
                                                          (read before any first-use proof)
  approval_lineage() -> dict                              signer phase only: the enrolled archive's
                                                          table_id, genesis_sha256 and authority_sha256

Authority never comes from E, a candidate file or the request. It comes from GitHub run, job and
artifact metadata checked against the protected evidence policy on main, and from founder receipts.

A card names a founder receipt by its SHA-256. The producer here writes the successor (schema 2)
receipt: it binds `stable_identity_sha256`, the hash of the typed provenance and scope facts only,
never provider response bytes, so the receipt E froze is rebuilt the same way later. The source
responses go in a separate audit section. A new first use needs a schema 2 receipt. An archived frozen
v1 record still authenticates with its v1 meaning (it binds the whole projection, responses included).
Before the first durable archive, a card needs fresh provider proof; once the archive holds an
accepted record, its original bytes are used and no fresh observation replaces them. Current authority
(the founder, the channel, revocation) is checked apart from the identity, from the current policy on
main. Later signing and promotion still check current authority.
"""
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import evidence as ev  # noqa: E402
import github as gh  # noqa: E402

ControllerError = cand.ControllerError
REPOSITORY = cand.REPOSITORY
REPOSITORY_ID = ev.REPOSITORY_ID
TARGETS = cand.TARGETS

# Refusal codes (a closed set); the text is `<code>: <reason>`.
INCOMPLETE = 'eligibility incomplete'
MISMATCH = 'evidence mismatch'
UNAUTHORIZED = 'not authorized'
CONFLICT = 'reservation conflict'
LIMIT = 'input limit exceeded'
CODES = (INCOMPLETE, MISMATCH, UNAUTHORIZED, CONFLICT, LIMIT)

CANDIDATE_WORKFLOW = '.github/workflows/release-candidate.yml'
CANDIDATE_EVENTS = ('push', 'workflow_dispatch')
CANDIDATE_JOBS = (('Candidate source and version',) + tuple('Native candidate (' + t + ')' for t in TARGETS)
                  + ('Candidate manifest',))
SECURITY_WORKFLOWS = {
    'CodeQL (actions)': '.github/workflows/security.yml',
    'CodeQL (rust)': '.github/workflows/security.yml',
    'Core CI': '.github/workflows/ci.yml',
    'Core security': '.github/workflows/security.yml',
    'Dependency, secret and workflow policy': '.github/workflows/security.yml',
    'Fast gate (Linux x86-64)': '.github/workflows/ci.yml',
}
# A pull_request run tests a merge commit, not S.
SECURITY_EVENTS = ('push', 'schedule', 'workflow_dispatch')
ALERT_REF = 'refs/heads/main'
BLOCKING_SEVERITIES = ('high', 'critical')

EVIDENCE_MEMBER = 'release-record.json'
CANDIDATE_MEMBER = 'candidate.json'
APPROVAL_MEMBER = 'approval-request.json'
POLICY_PATH = 'release-keys/policy.json'
EVIDENCE_POLICY_PATH = 'release-keys/evidence-policy.json'
RECORDS_PATH = 'release-keys/records.txt'

# The historical key signs through v0.41.0 (D-Z5ES3; `HISTORICAL_LAST` in rdy-qa's release_keys.rs).
HISTORICAL_LAST_MINOR = 41
FIRST_PARENT_MAX = 1000          # commits walked down main's first-parent chain
RUNS_MAX = 64                    # run attempts named by one release
APPROVALS_MAX = ev.RECEIPTS_MAX  # first-use approval runs named by one request
RECORDS_MAX = 65536              # records.txt
SOURCE_FILE_MAX = 1 * cand.MIB   # notes and guide files
CARGO_MAX = 64 * cand.KIB

REQUEST_KEYS = ('version', 'candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id', 'evidence_run_id',
                'evidence_run_attempt', 'evidence_artifact_id', 'evidence_sha256', 'dispatch_run_id',
                'dispatch_run_attempt', 'approvals')
APPROVAL_RUN_KEYS = ('receipt_sha256', 'run_id', 'artifact_id')
TRUST_KEYS = ('version', 'fingerprint', 'signer_line', 'transition_sha256', 'policy_commit')
LINEAGE_KEYS = ('table_id', 'genesis_sha256', 'authority_sha256')
TAG_RE = re.compile(r'v0\.([1-9][0-9]{0,8})\.0')


class Refused(ControllerError):
    """A named refusal: `<code>: <reason>`. The reason never holds an input body or credential."""

    def __init__(self, code, reason):
        if code not in CODES:
            raise ValueError('unknown refusal code')
        ControllerError.__init__(self, code + ': ' + reason)
        self.code = code
        self.reason = reason


class _Expired(Refused):
    """The artifact expired. Only preliminary recovery of a named approval run treats this apart."""


# ---------------------------------------------------------------------------------------------
# The request
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class ApprovalRun(object):
    """A first-use founder approval run that a card's receipt comes from."""
    receipt_sha256: str
    run_id: int
    artifact_id: int


@dataclass(frozen=True)
class Request(object):
    """The release dispatch's inputs. Recovery names the original E run, attempt and artifact."""
    version: str
    candidate_run_id: int
    candidate_run_attempt: int
    candidate_artifact_id: int
    evidence_run_id: int
    evidence_run_attempt: int
    evidence_artifact_id: int
    evidence_sha256: str
    dispatch_run_id: int
    dispatch_run_attempt: int
    approvals: tuple = ()


def _positive(value, what):
    if not cand.is_int(value, 1):
        raise Refused(MISMATCH, 'the request ' + what + ' is not a positive integer')
    return value


def parse_request(value):
    """A closed request mapping: unknown keys refuse with the nearest name."""
    try:
        cand.closed(value, REQUEST_KEYS, 'the request')
        runs = value['approvals']
        if not isinstance(runs, list):
            raise ControllerError('the request approvals are not a list')
        for entry in runs:
            cand.closed(entry, APPROVAL_RUN_KEYS, 'a request approval')
    except Refused:
        raise
    except ControllerError as error:
        raise Refused(MISMATCH, str(error))
    fields = dict(value)
    fields['approvals'] = tuple(ApprovalRun(e['receipt_sha256'], e['run_id'], e['artifact_id']) for e in runs)
    return check_request(Request(**fields))


def check_request(request):
    if not isinstance(request, Request):
        raise Refused(MISMATCH, 'the request is not a release request')
    if not (isinstance(request.version, str) and ev.VERSION_RE.fullmatch(request.version)):
        raise Refused(MISMATCH, 'the request version is not a 0.N.0 preview')
    for key in REQUEST_KEYS:
        if key.endswith('_id') or key.endswith('_attempt'):
            _positive(getattr(request, key), key.replace('_', ' '))
    if not (isinstance(request.evidence_sha256, str) and cand.SHA256_RE.fullmatch(request.evidence_sha256)):
        raise Refused(MISMATCH, 'the request evidence digest is not a 64-hex SHA-256')
    if not isinstance(request.approvals, tuple) or len(request.approvals) > APPROVALS_MAX:
        raise Refused(LIMIT, 'the request names more than ' + str(APPROVALS_MAX) + ' approval runs')
    for run in request.approvals:
        if not (isinstance(run, ApprovalRun) and isinstance(run.receipt_sha256, str)
                and cand.SHA256_RE.fullmatch(run.receipt_sha256)):
            raise Refused(MISMATCH, 'a request approval is malformed')
        _positive(run.run_id, 'approval run id')
        _positive(run.artifact_id, 'approval artifact id')
    if len({r.receipt_sha256 for r in request.approvals}) != len(request.approvals):
        raise Refused(MISMATCH, 'the request names one approval receipt twice')
    return request


# ---------------------------------------------------------------------------------------------
# The result
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class QualifiedCard(object):
    """One founder card, authenticated at the signer phase. A first-use card carries the exact request,
    receipt and provider-evidence bytes for the archive; an archived card carries its envelope."""
    kind: str
    receipt_sha256: str
    source: str
    request_bytes: bytes
    receipt_bytes: bytes
    projection_bytes: bytes
    envelope_bytes: object


@dataclass(frozen=True)
class ProvisionalCard(object):
    """Before credentials: fresh GitHub proof of a first-use card. It says nothing about the archive."""
    kind: str
    receipt_sha256: str
    source: str
    request_bytes: bytes
    receipt_bytes: bytes
    projection_bytes: bytes
    envelope_bytes: object


@dataclass(frozen=True)
class PendingCard(object):
    """Before credentials: a card that only the signer can resolve from the archive. It holds no
    approval bytes. `run_id` and `artifact_id` name a run that recovery deferred because its source
    artifact expired; expiry is not approval."""
    kind: str
    receipt_sha256: str
    source: str
    run_id: object
    artifact_id: object


@dataclass(frozen=True)
class HistoricalCard(object):
    """A card of a published release, with its current status reported apart. It authorizes nothing."""
    kind: str
    receipt_sha256: str
    revoked: bool


@dataclass(frozen=True)
class ReleaseFacts(object):
    """Immutable release facts. `tuple_bytes` is T; every T field is also here, so T can be rebuilt."""
    mode: str
    version: str
    commit: str
    design_landing: str
    evidence_sha256: str
    evidence_bytes: bytes
    manifest_sha256: str
    artifacts: tuple
    key_arn: str
    fingerprint: str
    first_machine_tag: str
    transition_sha256: str
    policy_commit: str
    signer_workflow_sha: str
    policy_sha256: str
    evidence_policy_sha256: str
    tuple_sha256: str
    tuple_bytes: bytes
    approval_mode: str
    cards: tuple
    dispatcher: tuple
    evidence_run: tuple
    candidate_run: tuple
    fast_gate_ignored: tuple
    install: tuple
    reservation: object
    publication: object
    alert_observation: object

    def tuple_value(self):
        """T as a new dict (the record itself stays immutable)."""
        return json.loads(self.tuple_bytes.decode('utf-8'))


@dataclass(frozen=True)
class Preliminary(ReleaseFacts):
    """The pre-credential result. It permits only the reservation bookkeeping, never KMS or promotion."""


@dataclass(frozen=True)
class Qualified(ReleaseFacts):
    """The signer-phase result: every card is resolved. Only this value reaches KMS."""


@dataclass(frozen=True)
class Reconciliation(ReleaseFacts):
    """Historical reconciliation of a reservation. It is not eligibility: `cards` report status only."""
    channel_enabled: bool


@dataclass(frozen=True)
class Reservation(object):
    ref_sha256: str
    tag_object: str


@dataclass(frozen=True)
class FreshAlertRead(object):
    """This job read the open code scanning alerts on `ref`, and none blocked."""
    ref: str


@dataclass(frozen=True)
class InheritedAlerts(object):
    """The eligibility job's earlier alert read, as the reserve job knows it from that job's success. It is not
    a fresh read and holds no alert list. It supports only the reservation bookkeeping of this exact request,
    candidate, workflow, run and attempt."""
    request: Request
    commit: str
    workflow_path: str
    run_id: int
    run_attempt: int
    job_id: int


@dataclass(frozen=True)
class Publication(object):
    """`state` is `unpublished`, `final_tag`, `draft` or `published`. `final_tag` is a final tag at the reserved
    commit with no release this reader can see: an unfinished release that gives no new authority."""
    state: str
    release_id: object


# ---------------------------------------------------------------------------------------------
# Reading facts
# ---------------------------------------------------------------------------------------------

def _code_for(message, default):
    if 'larger than' in message or 'more than' in message:
        return LIMIT
    return default


class _Facts(object):
    """One run's reads, memoized; every failure is a named refusal."""

    def __init__(self, facts):
        self.facts = facts
        self.cache = {}

    def _read(self, name):
        read = getattr(self.facts, name, None)
        if read is None or name.startswith('create'):
            raise Refused(INCOMPLETE, 'the fact source cannot read ' + name.replace('_', ' '))
        return read

    def call(self, name, *args):
        key = (name,) + args
        if key in self.cache:
            return self.cache[key]
        try:
            value = self._read(name)(*args)
        except Refused:
            raise
        except gh.ArtifactExpired as error:
            raise _Expired(INCOMPLETE, str(error))
        except ControllerError as error:
            raise Refused(_code_for(str(error), INCOMPLETE), str(error))
        self.cache[key] = value
        return value

    def optional(self, name, *args):
        """The fact, or None for authenticated absence (an authenticated 404)."""
        key = (name,) + args
        if key in self.cache:
            return self.cache[key]
        try:
            value = self._read(name)(*args)
        except gh.Absent:
            value = None
        except Refused:
            raise
        except ControllerError as error:
            raise Refused(_code_for(str(error), INCOMPLETE), str(error))
        self.cache[key] = value
        return value

    def file(self, path, sha, limit):
        return self.call('contents', path, sha, limit)


def _parse(parse, data):
    try:
        return parse(data)
    except Refused:
        raise
    except RecursionError:
        raise Refused(MISMATCH, 'evidence is not JSON')
    except ControllerError as error:
        message = str(error)
        default = INCOMPLETE if message.startswith('missing field') else MISMATCH
        raise Refused(_code_for(message, default), message)


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _minor(version):
    return int(version.split('.')[1])


class _Main(object):
    """Main's first-parent chain, walked on demand (at most FIRST_PARENT_MAX commits)."""

    def __init__(self, facts):
        self.facts = facts
        ref = facts.call('ref', 'heads/main')
        if ref.object_type != 'commit':
            raise Refused(MISMATCH, 'main does not point at a commit')
        self.head = ref.object_sha
        self.chain = [self.head]
        self.seen = {self.head}
        self.ended = False

    def contains(self, sha):
        while sha not in self.seen and not self.ended:
            if len(self.chain) >= FIRST_PARENT_MAX:
                raise Refused(LIMIT, "main's first-parent chain is longer than " + str(FIRST_PARENT_MAX)
                              + ' commits before the commit it needs')
            commit = self.facts.call('commit', self.chain[-1])
            if not commit.parents:
                self.ended = True
                break
            parent = commit.parents[0]
            self.chain.append(parent)
            self.seen.add(parent)
        return sha in self.seen

    def require(self, sha, what):
        if not self.contains(sha):
            raise Refused(MISMATCH, 'the ' + what + " is not on main's first-parent chain")


def _policy_at(facts, sha):
    """`policy.json` and `evidence-policy.json` at one commit; the records index and evidence policy
    must match their policy digests."""
    policy = _parse(ev.parse_policy, facts.file(POLICY_PATH, sha, ev.RECORD_MAX).data)
    evidence_policy = _parse(ev.parse_evidence_policy, facts.file(EVIDENCE_POLICY_PATH, sha, ev.RECORD_MAX).data)
    if evidence_policy.sha256 != policy.value['evidence_policy_sha256']:
        raise Refused(MISMATCH, 'the evidence policy does not match its policy digest')
    records = facts.file(RECORDS_PATH, sha, RECORDS_MAX).data
    if _sha(records) != policy.value['records_sha256']:
        raise Refused(MISMATCH, 'the trust records do not match their policy digest')
    return policy, evidence_policy


def _coordinators(evidence_policy):
    return {entry['actor_id'] for entry in evidence_policy.value['coordinators']}


def _actors(run, evidence_policy, what):
    """Both the original actor and the triggering actor of this exact attempt must be coordinators."""
    allowed = _coordinators(evidence_policy)
    if run.actor_id not in allowed:
        raise Refused(UNAUTHORIZED, 'the ' + what + ' was dispatched by an actor who is not a coordinator')
    if run.triggering_actor_id not in allowed:
        raise Refused(UNAUTHORIZED, 'the ' + what + ' was triggered by an actor who is not a coordinator')


def _on_candidate(run, version, commit, what):
    if run.event != 'workflow_dispatch' or run.head_branch != 'candidate/v' + version:
        raise Refused(UNAUTHORIZED, 'the ' + what + ' is not a workflow_dispatch on candidate/v' + version)
    if run.head_sha != commit:
        raise Refused(MISMATCH, 'the ' + what + ' did not run on the release commit')


def _concluded(run, what):
    if run.status != 'completed' or run.conclusion != 'success':
        raise Refused(INCOMPLETE, 'the ' + what + ' did not conclude success')


def _all_jobs_passed(facts, run_id, attempt, what):
    jobs = facts.call('attempt_jobs', run_id, attempt)
    if not jobs or any(j.status != 'completed' or j.conclusion != 'success' for j in jobs):
        raise Refused(INCOMPLETE, 'a job of the ' + what + ' did not conclude success')
    return jobs


def _job(facts, entry, commit, what):
    """E's Job against the jobs of its exact run attempt."""
    jobs = facts.call('attempt_jobs', entry['run_id'], entry['run_attempt'])
    found = [j for j in jobs if j.job_id == entry['job_id']]
    if not found:
        raise Refused(INCOMPLETE, 'the ' + what + ' is not a job of run ' + str(entry['run_id']) + ' attempt '
                      + str(entry['run_attempt']))
    job = found[0]
    if job.html_url != entry['job_url']:
        raise Refused(MISMATCH, 'the ' + what + ' URL is not its job')
    if job.status != 'completed' or job.conclusion != 'success':
        raise Refused(INCOMPLETE, 'the ' + what + ' did not conclude success')
    if job.head_sha != commit:
        raise Refused(MISMATCH, 'the ' + what + ' did not run on the release commit')
    return job


# ---------------------------------------------------------------------------------------------
# Authentication steps
# ---------------------------------------------------------------------------------------------

def _dispatcher(facts, request, evidence_policy, version, commit):
    run = facts.call('run_attempt', request.dispatch_run_id, request.dispatch_run_attempt)
    if run.path != evidence_policy.value['release_workflow_path']:
        raise Refused(UNAUTHORIZED, 'the release run is not the release workflow')
    _on_candidate(run, version, commit, 'release run')
    _actors(run, evidence_policy, 'release run')
    return (run.run_id, run.run_attempt, run.actor_id, run.triggering_actor_id)


def _evidence(facts, request, evidence_policy, version, commit):
    """The evidence run, its attempt, its artifact and E's exact bytes."""
    pinned = evidence_policy.value['evidence_workflow']
    run = facts.call('run_attempt', request.evidence_run_id, request.evidence_run_attempt)
    if run.workflow_id != pinned['id'] or run.path != pinned['path']:
        raise Refused(UNAUTHORIZED, 'the evidence run is not the pinned evidence workflow')
    _on_candidate(run, version, commit, 'evidence run')
    _actors(run, evidence_policy, 'evidence run')
    _concluded(run, 'evidence run')
    _all_jobs_passed(facts, run.run_id, run.run_attempt, 'evidence run')
    at_commit = facts.file(pinned['path'], commit, gh.CONTENTS_MAX)
    reviewed = facts.file(pinned['path'], pinned['sha'], gh.CONTENTS_MAX)
    if at_commit.blob_sha != reviewed.blob_sha:
        raise Refused(UNAUTHORIZED, 'the evidence workflow at the release commit is not its pinned implementation')
    artifact = facts.call('artifact', request.evidence_artifact_id, run.run_id)
    if artifact.head_sha != commit:
        raise Refused(MISMATCH, 'the release record artifact is not from the release commit')
    data = facts.call('artifact_member', artifact, EVIDENCE_MEMBER, ev.RELEASE_RECORD_MAX)
    if _sha(data) != request.evidence_sha256:
        raise Refused(MISMATCH, 'the release record does not match its requested digest')
    record = _parse(ev.parse_release_record, data)
    if record.value['version'] != version or record.value['commit'] != commit:
        raise Refused(MISMATCH, 'the release record does not name v' + version + ' at the release commit')
    return run, data, record


def _candidate(facts, request, record, version, commit):
    """The candidate run, its jobs, its artifact and the candidate.json inside it."""
    e = record.value
    sealed = e['candidate']
    if (request.candidate_run_id, request.candidate_run_attempt, request.candidate_artifact_id) != (
            sealed['run_id'], sealed['run_attempt'], sealed['artifact']['id']):
        raise Refused(MISMATCH, 'the requested candidate is not the candidate of the release record')
    run = facts.call('run_attempt', sealed['run_id'], sealed['run_attempt'])
    if run.path != CANDIDATE_WORKFLOW or run.event not in CANDIDATE_EVENTS:
        raise Refused(UNAUTHORIZED, 'the candidate run is not the release candidate workflow')
    if run.head_branch != 'candidate/v' + version:
        raise Refused(UNAUTHORIZED, 'the candidate run is not on candidate/v' + version)
    if run.head_sha != commit:
        raise Refused(MISMATCH, 'the candidate run did not run on the release commit')
    _concluded(run, 'candidate run')
    jobs = _all_jobs_passed(facts, run.run_id, run.run_attempt, 'candidate run')
    names = [j.name for j in jobs]
    for name in CANDIDATE_JOBS:
        if names.count(name) != 1:
            raise Refused(INCOMPLETE, 'the candidate run has no single job ' + name)
    artifact = facts.call('artifact', sealed['artifact']['id'], run.run_id)
    if (artifact.name, 'sha256:' + artifact.digest, artifact.size) != (
            sealed['artifact']['name'], sealed['artifact']['digest'], sealed['artifact']['size']):
        raise Refused(MISMATCH, 'the candidate artifact is not the one the release record names')
    if artifact.head_sha != commit:
        raise Refused(MISMATCH, 'the candidate artifact is not from the release commit')
    raw = facts.call('artifact_member', artifact, CANDIDATE_MEMBER, ev.RECORD_MAX)
    built = _parse(cand.load_candidate, raw)
    if (built['version'], built['commit'], built['run_id'], built['run_attempt']) != (
            version, commit, run.run_id, run.run_attempt):
        raise Refused(MISMATCH, 'candidate.json does not name this candidate run at the release commit')
    if built['manifest_sha256'] != sealed['manifest_sha256'] or built['artifacts'] != sealed['artifacts']:
        raise Refused(MISMATCH, 'the release record archives are not the authenticated candidate archives')
    counts = {b['target']: b['fast_gate'] for b in built['builds']}
    native = {j.name: j for j in jobs}
    for entry in e['targets']:
        target = entry['target']
        gate = entry['fast_gate']
        if {k: gate[k] for k in ('passed', 'failed', 'ignored', 'prerequisite_skipped')} != counts[target]:
            raise Refused(MISMATCH, 'the ' + target + ' fast gate counts are not its build receipt counts')
        job = native['Native candidate (' + target + ')']
        for step, value in (('build', entry['build']), ('package', entry['package']), ('fast gate', gate['job'])):
            if (value['run_id'], value['run_attempt'], value['job_id']) != (run.run_id, run.run_attempt, job.job_id):
                raise Refused(MISMATCH, 'the ' + target + ' ' + step + ' job is not its native candidate job')
            _job(facts, value, commit, target + ' ' + step + ' job')
    return run, artifact


def _install(facts, record, runs, commit):
    """Each target's install check is a job of the candidate or evidence run attempt."""
    install = []
    for entry in record.value['targets']:
        target = entry['target']
        value = entry['install']['job']
        if (value['run_id'], value['run_attempt']) not in runs:
            raise Refused(MISMATCH, 'the ' + target + ' install job is not from the candidate or evidence run')
        _job(facts, value, commit, target + ' install job')
        install.append((target, value['job_id'], entry['install']['report_sha256']))
    return tuple(install)


def _guide_names(bundle):
    """The edition and design landing a guide bundle names, or None when it names none. The build
    refuses any other flaw of its shape, and the digests above tie these bytes to that build."""
    try:
        value = cand.loads(bundle, limit=SOURCE_FILE_MAX)
    except (ControllerError, RecursionError):
        return None
    return (value.get('edition'), value.get('design_landing')) if isinstance(value, dict) else None


def _source(facts, record, version, commit):
    """Cargo and the committed release/ exports at S against E."""
    e = record.value
    cargo = facts.file('Cargo.toml', commit, CARGO_MAX).data
    if cand.cargo_version(cargo.decode('utf-8', 'replace')) != version:
        raise Refused(MISMATCH, 'the Cargo version at the release commit is not ' + version)
    raw = facts.file('release/source.json', commit, ev.RECORD_MAX).data
    source = _parse(ev.parse_source, raw)
    if source.sha256 != e['source']['source_json_sha256']:
        raise Refused(MISMATCH, 'release/source.json does not match the release record')
    if source.value['version'] != version or source.value['design_landing'] != e['design_landing']:
        raise Refused(MISMATCH, 'release/source.json does not name this version and design landing')
    notes = facts.file('release/notes.md', commit, SOURCE_FILE_MAX).data
    if not (_sha(notes) == source.value['notes_sha256'] == e['source']['notes_sha256']):
        raise Refused(MISMATCH, 'release/notes.md does not match its digest')
    bundle = facts.file('release/agent-guide.json', commit, SOURCE_FILE_MAX).data
    sidecar = facts.file('release/agent-guide.sha256', commit, SOURCE_FILE_MAX).data
    digest = _sha(bundle)
    if not (sidecar == digest.encode() + b'\n' and digest == source.value['guide_sha256'] == e['source']['guide_sha256']):
        raise Refused(MISMATCH, 'the release guide does not match its digest')
    if _guide_names(bundle) != (version, e['design_landing']):
        raise Refused(MISMATCH, 'the release guide does not name this version and design landing')


def _security(facts, record, commit):
    """The six required jobs ran on S in their own workflows."""
    for entry in record.value['security']['jobs']:
        name, value = entry['name'], entry['job']
        run = facts.call('run_attempt', value['run_id'], value['run_attempt'])
        if run.path != SECURITY_WORKFLOWS[name] or run.event not in SECURITY_EVENTS:
            raise Refused(MISMATCH, 'the ' + name + ' job is not from its workflow on main')
        if run.head_sha != commit:
            raise Refused(MISMATCH, 'the ' + name + ' job is stale: it did not run on the release commit')
        job = _job(facts, value, commit, name + ' job')
        if job.name != name:
            raise Refused(MISMATCH, 'the ' + name + ' job is another job')


def _read_alerts(facts):
    """No open high or critical code scanning alert on main, read now."""
    alerts = facts.call('open_code_scanning_alerts', ALERT_REF)
    blocking = [a for a in alerts if a.state == 'open' and a.security_severity in BLOCKING_SEVERITIES]
    if blocking:
        raise Refused(INCOMPLETE, str(len(blocking)) + ' open high or critical code scanning alert'
                      + ('' if len(blocking) == 1 else 's'))
    return FreshAlertRead(ALERT_REF)


def check_alerts(facts):
    """The live alert read before a new Sign. A blocking alert or a failed read refuses."""
    return _read_alerts(_Facts(facts))


def _check_inherited(inherited, request, commit, evidence_policy):
    """An inherited observation stands only for this exact request and its release run attempt."""
    if not (isinstance(inherited, InheritedAlerts) and inherited.request == request and inherited.commit == commit
            and inherited.workflow_path == evidence_policy.value['release_workflow_path']
            and (inherited.run_id, inherited.run_attempt) == (request.dispatch_run_id, request.dispatch_run_attempt)):
        raise Refused(INCOMPLETE, 'the inherited alert observation is not this exact release run')
    return inherited


def _trust(facts, version, policy, policy_commit, signing):
    answer = facts.call('release_trust', version, policy_commit, signing)
    try:
        cand.closed(answer, TRUST_KEYS, 'the release trust result')
    except ControllerError as error:
        raise Refused(MISMATCH, str(error))
    if (answer['version'], answer['policy_commit']) != (version, policy_commit):
        raise Refused(MISMATCH, 'the release trust result is not for v' + version + ' at the policy commit')
    if answer['fingerprint'] != policy.value['fingerprint']:
        raise Refused(MISMATCH, 'the release key of v' + version + ' is not the policy key')
    if not (isinstance(answer['transition_sha256'], str) and cand.SHA256_RE.fullmatch(answer['transition_sha256'])):
        raise Refused(MISMATCH, 'the release key of v' + version + ' has no machine transition')
    return answer


def _published(releases):
    """The minors of every published release; a published tag must be a v0.N.0 preview."""
    minors = []
    for release in releases:
        if release.draft:
            continue
        match = TAG_RE.fullmatch(release.tag_name)
        if not match:
            raise Refused(MISMATCH, 'a published release tag is not a v0.N.0 preview')
        minors.append(int(match.group(1)))
    return minors


def _required_cards(version, policy, releases):
    """Current policy and the published history decide which exception cards a release needs."""
    minor = _minor(version)
    earlier = [m for m in _published(releases) if m < minor]
    first = int(TAG_RE.fullmatch(policy.value['first_machine_tag']).group(1))
    if not any(m > HISTORICAL_LAST_MINOR for m in earlier):
        return ('release_1',)
    if not any(m >= first for m in earlier):
        return ('key_change',)
    return ()


def _channel_receipt(card, channel):
    try:
        return ev.card_receipt_sha256(card, channel)
    except ControllerError:
        raise Refused(UNAUTHORIZED, 'the ' + card['kind'] + ' card is not in the pinned approval archive')


def _approval_projection(facts, main, evidence_policy, authority, run_ref, card):
    """Authenticates a first-use founder approval run on GitHub and rebuilds its receipt from those
    facts. The producer writes the successor (schema 2) record only. The pins come from E's evidence
    policy; the founder is current authority."""
    channel = evidence_policy.value['approval_channel']
    founder = authority['founder_user_id']
    kind = card['kind']
    run = facts.call('run_attempt', run_ref.run_id, 1)
    if (run.workflow_id, run.path, run.event, run.head_branch) != (
            channel['workflow_id'], channel['workflow_path'], 'workflow_dispatch', 'main'):
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' approval run is not the pinned founder approval workflow')
    if run.actor_id != founder or run.triggering_actor_id != founder:
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' approval run was not dispatched by the founder')
    _concluded(run, kind + ' approval run')
    main.require(run.head_sha, kind + ' approval run head')
    workflow = facts.file(channel['workflow_path'], run.head_sha, gh.CONTENTS_MAX)
    if workflow.sha256 != channel['workflow_file_sha256']:
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' approval workflow is not its pinned bytes')
    jobs = _all_jobs_passed(facts, run.run_id, 1, kind + ' approval run')
    named = [j for j in jobs if j.name == channel['required_job_name']]
    if len(named) != 1:
        raise Refused(INCOMPLETE, 'the ' + kind + ' approval run has no single ' + channel['required_job_name'] + ' job')
    job = named[0]
    artifact = facts.call('artifact', run_ref.artifact_id, run.run_id)
    if artifact.name != 'approval-request-' + str(run.run_id) + '-1' or artifact.head_sha != run.head_sha:
        raise Refused(MISMATCH, 'the ' + kind + ' approval artifact is not the request of its run')
    request_bytes = facts.call('artifact_member', artifact, APPROVAL_MEMBER, ev.REQUEST_MAX)
    request = _parse(ev.parse_approval_request, request_bytes)
    main.require(request.value['policy_commit'], kind + ' approval policy commit')
    reviews = facts.call('run_approvals', run.run_id)
    if len(reviews) != 1:
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' approval run needs exactly one founder review')
    review = reviews[0]
    if (review.state, review.user_id, review.user_type, review.environments) != (
            'approved', founder, 'User', ((channel['environment_id'], channel['environment_name']),)):
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' review is not the founder approval in the pinned environment')
    if review.comment != 'APPROVE sha256:' + request.sha256:
        raise Refused(UNAUTHORIZED, 'the founder review does not approve the ' + kind + ' request')
    responses = facts.call('approval_responses', run.run_id, artifact.artifact_id)
    # Entries that hash exact file or ZIP bytes must equal the live facts. The others hash API responses
    # that GitHub re-renders (pushes, counters, timestamps): they are audit data only.
    known = {'protected_policy': (evidence_policy.sha256, evidence_policy.size),
             'request_archive': (artifact.digest, artifact.size),
             'workflow_source': (workflow.sha256, len(workflow.data))}
    if not (isinstance(responses, list) and all(isinstance(r, dict) for r in responses)):
        raise Refused(MISMATCH, 'the ' + kind + ' approval source responses are malformed')
    for entry in responses:
        expected = known.get(entry.get('kind'))
        if expected and (entry.get('sha256'), entry.get('bytes')) != expected:
            raise Refused(MISMATCH, 'the ' + kind + ' approval source responses do not match the authenticated facts')
    facts_value = {
        'repository_id': run.repository_id, 'head_repository_id': run.head_repository_id,
        'workflow_id': run.workflow_id, 'workflow_path': run.path, 'implementation_sha': channel['implementation_sha'],
        'workflow_file_sha256': workflow.sha256, 'run_id': run.run_id, 'run_attempt': run.run_attempt,
        'run_head_sha': run.head_sha, 'ref': ev.MAIN_REF, 'event': run.event, 'status': run.status,
        'conclusion': run.conclusion, 'actor_id': run.actor_id, 'triggering_actor_id': run.triggering_actor_id,
        'job': {'id': job.job_id, 'name': job.name, 'run_id': job.run_id, 'status': job.status,
                'conclusion': job.conclusion},
        'artifact': {'id': artifact.artifact_id, 'name': artifact.name, 'sha256': artifact.digest,
                     'workflow_run_id': artifact.run_id, 'repository_id': artifact.repository_id,
                     'head_repository_id': artifact.head_repository_id, 'head_sha': artifact.head_sha,
                     'expired': artifact.expired, 'request_sha256': request.sha256},
        'review': {'state': review.state, 'comment': review.comment, 'user_id': review.user_id,
                   'user_type': review.user_type, 'environment_id': review.environments[0][0],
                   'environment_name': review.environments[0][1]},
    }
    issuer = {'repository_id': run.repository_id, 'workflow_id': run.workflow_id, 'workflow_path': run.path,
              'implementation_sha': channel['implementation_sha'], 'run_id': run.run_id,
              'run_attempt': run.run_attempt, 'run_head_sha': run.head_sha, 'ref': ev.MAIN_REF,
              'actor_id': run.actor_id, 'triggering_actor_id': run.triggering_actor_id, 'job_id': job.job_id,
              'job_name': job.name, 'artifact_id': artifact.artifact_id, 'artifact_sha256': artifact.digest,
              'reviewer_id': review.user_id, 'environment_id': review.environments[0][0],
              'environment_name': review.environments[0][1],
              'review_comment_sha256': _sha(review.comment.encode('utf-8'))}
    audit = [dict(r) for r in responses]
    # The identity holds no response digest, so it is stable.
    identity = dict(facts_value, schema=2, decision=ev.DECISION, kind=request.value['kind'],
                    policy_commit=request.value['policy_commit'], scope=request.value['scope'])
    projection = _parse(ev.parse_provider_evidence,
                        _canonical({'schema': 2, 'identity': identity, 'audit': {'source_responses': audit}}))
    receipt = {'schema': 2, 'decision': ev.DECISION, 'kind': request.value['kind'],
               'policy_commit': request.value['policy_commit'], 'workflow_scope_digest': request.sha256,
               'scope': request.value['scope'],
               'issuer': dict(issuer, stable_identity_sha256=ev.stable_identity_sha256(identity))}
    return request_bytes, _parse(ev.parse_approval_receipt, _canonical(receipt)), _canonical(projection.value)


def _canonical(value):
    try:
        return cand.canonical(value)
    except ControllerError as error:
        raise Refused(MISMATCH, str(error))


def _first_use(facts, main, evidence_policy, authority, run_ref, card, receipt_sha256, result=QualifiedCard):
    """Fresh provider proof, before the first durable archive. The card must name the successor
    (schema 2) receipt rebuilt from the authenticated facts: a v1 card never takes a new first use."""
    request_bytes, receipt, projection_bytes = _approval_projection(facts, main, evidence_policy, authority, run_ref,
                                                                    card)
    if receipt.sha256 != receipt_sha256:
        raise Refused(UNAUTHORIZED, 'the ' + card['kind'] + ' card does not name the founder-approved receipt')
    return result(card['kind'], receipt_sha256, 'first_use', request_bytes, _canonical(receipt.value),
                  projection_bytes, None), receipt


def _archived(facts, main, authority, card, receipt_sha256, data, run_ref):
    """A founder receipt accepted earlier, read from the enrolled archive through protected lineage.
    Its original bytes are kept: no fresh provider observation replaces them."""
    kind = card['kind']
    envelope = _parse(ev.parse_archive_envelope, data)
    if envelope.receipt.sha256 != receipt_sha256:
        raise Refused(MISMATCH, 'the archived approval is not the ' + kind + ' card receipt')
    issuer = envelope.receipt.value['issuer']
    if run_ref is not None and (run_ref.run_id, run_ref.artifact_id) != (issuer['run_id'], issuer['artifact_id']):
        raise Refused(MISMATCH, 'the requested ' + kind + ' approval run is not the accepted one')
    lineage = facts.call('approval_lineage')
    if not isinstance(lineage, dict) or sorted(lineage) != sorted(LINEAGE_KEYS):
        raise Refused(INCOMPLETE, 'the approval archive lineage is unavailable')
    acceptance = envelope.acceptance
    if any(acceptance[key] != lineage[key] for key in LINEAGE_KEYS):
        raise Refused(UNAUTHORIZED, 'the archived ' + kind + ' approval is not from the enrolled archive')
    main.require(acceptance['policy_commit'], 'archived ' + kind + ' approval policy commit')
    accepted = facts.file(POLICY_PATH, acceptance['policy_commit'], ev.RECORD_MAX).data
    if _sha(accepted) != acceptance['policy_sha256']:
        raise Refused(UNAUTHORIZED, 'the archived ' + kind + ' approval policy is not the protected policy')
    if _parse(ev.parse_policy, accepted).value['signer_workflow_sha'] != acceptance['signer_workflow_sha']:
        raise Refused(UNAUTHORIZED, 'the archived ' + kind + ' approval was not accepted by its policy signer')
    founder = authority['founder_user_id']
    if not (issuer['reviewer_id'] == issuer['actor_id'] == issuer['triggering_actor_id'] == founder):
        raise Refused(UNAUTHORIZED, 'the archived ' + kind + ' approval is not the founder approval')
    bodies = envelope.value
    return QualifiedCard(kind, receipt_sha256, 'archive', bodies['request_body'].encode('utf-8'),
                         bodies['receipt_body'].encode('utf-8'), bodies['provider_projection_body'].encode('utf-8'),
                         data), envelope.receipt


def _card_authority(card, channel, authority):
    """The checks that need no approval bytes. The card's receipt identity is checked against E's
    evidence policy. Current authority comes from the current policy on main."""
    kind = card['kind']
    if kind == 'revocation':
        raise Refused(UNAUTHORIZED, 'a revocation card never permits signing')
    receipt_sha256 = _channel_receipt(card, channel)
    if authority['enabled'] is not True:
        raise Refused(UNAUTHORIZED, 'the founder approval channel is disabled')
    if receipt_sha256 in authority['revoked_receipts']:
        raise Refused(UNAUTHORIZED, 'the ' + kind + ' card receipt is revoked')
    return receipt_sha256


def _card_scope(kind, receipt, expected):
    if receipt.value['kind'] != kind:
        raise Refused(MISMATCH, 'the ' + kind + ' card names a ' + receipt.value['kind'] + ' receipt')
    scope = receipt.value['scope']
    for key in ev.SCOPE_KEYS[kind]:
        if scope[key] != expected[key]:
            raise Refused(UNAUTHORIZED, 'the ' + kind + ' card does not cover the release ' + key.replace('_', ' '))


def _preliminary_cards(mode):
    """Before credentials. A card with a named run gets fresh GitHub proof; a card without one is
    pending. Nothing here reads the archive or infers its state. Recovery alone defers a named run
    whose source artifact expired: every other refusal of that proof stays a refusal."""
    def resolve(facts, main, request, record, evidence_policy, current_evidence_policy, expected, required):
        basis = record.value['approval_basis']
        channel = evidence_policy.value['approval_channel']
        authority = current_evidence_policy.value['approval_channel']
        if basis['mode'] == 'standing_preview' and required:
            raise Refused(UNAUTHORIZED, 'a standing preview cannot cover a ' + required[0] + ' release')
        runs = {r.receipt_sha256: r for r in request.approvals}
        named = set()
        cards = []
        for card in basis['cards']:
            kind = card['kind']
            receipt_sha256 = _card_authority(card, channel, authority)
            run = runs.get(receipt_sha256)
            if run is None:
                cards.append(PendingCard(kind, receipt_sha256, 'pending_archive', None, None))
                continue
            named.add(receipt_sha256)
            try:
                provisional, receipt = _first_use(facts, main, evidence_policy, authority, run, card, receipt_sha256,
                                                  ProvisionalCard)
            except _Expired:
                if mode != 'recovery':
                    raise
                cards.append(PendingCard(kind, receipt_sha256, 'pending_archive', run.run_id, run.artifact_id))
                continue
            _card_scope(kind, receipt, expected)
            cards.append(provisional)
        _closed_cards(runs, named, cards, required)
        return tuple(cards)
    return resolve


def _closed_cards(runs, named, cards, required):
    if set(runs) - named:
        raise Refused(MISMATCH, 'a requested approval run names no card of the release record')
    kinds = {c.kind for c in cards}
    for kind in required:
        if kind not in kinds:
            raise Refused(UNAUTHORIZED, 'the release needs a ' + kind + ' card')


def _historical_cards(facts, main, request, record, evidence_policy, current_evidence_policy, expected, required):
    """Status only, for a release that already published. Nothing here decides anything."""
    channel = evidence_policy.value['approval_channel']
    revoked = current_evidence_policy.value['approval_channel']['revoked_receipts']
    cards = []
    for card in record.value['approval_basis']['cards']:
        receipt_sha256 = _channel_receipt(card, channel)
        cards.append(HistoricalCard(card['kind'], receipt_sha256, receipt_sha256 in revoked))
    return tuple(cards)


def _cards(facts, main, request, record, evidence_policy, current_evidence_policy, expected, required):
    """The signer phase: every card E names, authenticated and in scope; then every required kind is
    present. Current authority (the channel is enabled, the receipt is not revoked, the founder) comes
    from the current policy on main."""
    basis = record.value['approval_basis']
    channel = evidence_policy.value['approval_channel']
    authority = current_evidence_policy.value['approval_channel']
    if basis['mode'] == 'standing_preview' and required:
        raise Refused(UNAUTHORIZED, 'a standing preview cannot cover a ' + required[0] + ' release')
    runs = {r.receipt_sha256: r for r in request.approvals}
    named = set()
    cards = []
    for card in basis['cards']:
        kind = card['kind']
        receipt_sha256 = _card_authority(card, channel, authority)
        # The archive first: after the first durable archive its original bytes win. Fresh provider
        # proof is used only when the archive authenticates that no accepted record exists.
        data = facts.call('approval_record', receipt_sha256)
        if receipt_sha256 in runs:
            named.add(receipt_sha256)
        if data is not None:
            qualified, receipt = _archived(facts, main, authority, card, receipt_sha256, data,
                                           runs.get(receipt_sha256))
        elif receipt_sha256 in runs:
            qualified, receipt = _first_use(facts, main, evidence_policy, authority, runs[receipt_sha256], card,
                                            receipt_sha256)
        else:
            raise Refused(UNAUTHORIZED, 'the ' + kind + ' card has no accepted founder receipt')
        _card_scope(kind, receipt, expected)
        cards.append(qualified)
    _closed_cards(runs, named, cards, required)
    return tuple(cards)


# ---------------------------------------------------------------------------------------------
# Qualification
# ---------------------------------------------------------------------------------------------

def _authenticate(facts, request, mode, releases, reserved, resolve_cards, inherited=None):
    """Every release fact except the cards, which the stage's `resolve_cards` decides. Returns the
    fields of a `ReleaseFacts` and the current evidence policy."""
    version = request.version
    main = _Main(facts)
    candidate_ref = facts.call('ref', 'tags/candidate/v' + version)
    if candidate_ref.object_type != 'commit':
        raise Refused(MISMATCH, 'the candidate ref does not resolve directly to a commit')
    commit = candidate_ref.object_sha
    if reserved is not None and reserved['commit'] != commit:
        raise Refused(MISMATCH, 'the candidate ref is not the reserved commit')
    main.require(commit, 'release commit')
    current_policy, current_evidence_policy = _policy_at(facts, main.head)
    dispatcher = _dispatcher(facts, request, current_evidence_policy, version, commit)

    if reserved is None:
        sealing_policy = current_evidence_policy
    else:
        main.require(reserved['signer']['policy_commit'], 'reserved policy commit')
        sealing_policy = _policy_at(facts, reserved['signer']['policy_commit'])[1]
    evidence_run, evidence_bytes, record = _evidence(facts, request, sealing_policy, version, commit)
    e = record.value
    policy_commit = e['approval_basis']['policy_commit']
    if reserved is not None and policy_commit != reserved['signer']['policy_commit']:
        raise Refused(MISMATCH, 'the release record policy commit is not the reserved policy commit')
    main.require(policy_commit, 'policy commit')
    policy, evidence_policy = _policy_at(facts, policy_commit)
    if reserved is None and (policy.sha256, evidence_policy.sha256) != (current_policy.sha256,
                                                                        current_evidence_policy.sha256):
        raise Refused(MISMATCH, 'the release record policy commit is not the current policy')
    signer_sha = policy.value['signer_workflow_sha']
    if facts.call('compare', signer_sha, policy_commit).status != 'ahead':
        raise Refused(MISMATCH, 'the policy commit does not descend from the signer commit')
    if policy.value['targets'] != list(TARGETS):
        raise Refused(UNAUTHORIZED, 'the policy targets need a new_platform card and a qualified adapter')
    if _minor(version) < int(TAG_RE.fullmatch(policy.value['first_machine_tag']).group(1)):
        raise Refused(MISMATCH, 'v' + version + ' is before the first machine tag')

    candidate_run, candidate_artifact = _candidate(facts, request, record, version, commit)
    runs = {(candidate_run.run_id, candidate_run.run_attempt), (evidence_run.run_id, evidence_run.run_attempt)}
    install = _install(facts, record, runs, commit)
    named_runs = {(j['run_id'], j['run_attempt']) for t in e['targets']
                  for j in (t['build'], t['package'], t['fast_gate']['job'], t['install']['job'])}
    named_runs |= {(j['job']['run_id'], j['job']['run_attempt']) for j in e['security']['jobs']}
    if len(named_runs | runs) > RUNS_MAX:
        raise Refused(LIMIT, 'the release names more than ' + str(RUNS_MAX) + ' run attempts')
    _source(facts, record, version, commit)
    _security(facts, record, commit)
    if reserved is not None:
        observation = None
    elif inherited is not None:
        observation = _check_inherited(inherited, request, commit, current_evidence_policy)
    else:
        observation = _read_alerts(facts)

    trust = _trust(facts, version, policy, policy_commit, reserved is None)
    expected = {'repository': REPOSITORY, 'version': version, 'commit': commit,
                'design_landing': e['design_landing'], 'manifest_sha256': e['candidate']['manifest_sha256'],
                'artifacts': e['candidate']['artifacts'], 'key_arn': policy.value['key_arn'],
                'fingerprint': policy.value['fingerprint'], 'first_machine_tag': policy.value['first_machine_tag'],
                'transition_sha256': trust['transition_sha256'], 'targets': policy.value['targets']}
    scope = e['approval_basis']['scope']
    for key in ev.FULL_SCOPE_KEYS:
        if scope[key] != expected[key]:
            raise Refused(MISMATCH, 'the approval scope ' + key.replace('_', ' ')
                          + ' does not match the authenticated release facts')
    cards = resolve_cards(facts, main, request, record, evidence_policy, current_evidence_policy, expected,
                          _required_cards(version, policy, releases))

    signer = {'key_arn': policy.value['key_arn'], 'fingerprint': policy.value['fingerprint'],
              'first_machine_tag': policy.value['first_machine_tag'],
              'transition_sha256': trust['transition_sha256'], 'policy_commit': policy_commit,
              'signer_workflow_sha': signer_sha}
    rebuilt = {'schema': 1, 'repository': REPOSITORY, 'version': version, 'commit': commit,
               'release_evidence_sha256': record.sha256, 'manifest_sha256': e['candidate']['manifest_sha256'],
               'artifacts': e['candidate']['artifacts'], 'signer': signer}
    try:
        tuple_bytes = ev.tuple_bytes(rebuilt)
    except ControllerError as error:
        raise Refused(_code_for(str(error), MISMATCH), str(error))
    return dict(
        mode=mode, version=version, commit=commit, design_landing=e['design_landing'],
        evidence_sha256=record.sha256, evidence_bytes=evidence_bytes,
        manifest_sha256=e['candidate']['manifest_sha256'],
        artifacts=tuple((f['name'], f['sha256'], f['size']) for f in e['candidate']['artifacts']),
        key_arn=signer['key_arn'], fingerprint=signer['fingerprint'], first_machine_tag=signer['first_machine_tag'],
        transition_sha256=signer['transition_sha256'], policy_commit=policy_commit, signer_workflow_sha=signer_sha,
        policy_sha256=policy.sha256, evidence_policy_sha256=evidence_policy.sha256,
        tuple_sha256=_sha(tuple_bytes), tuple_bytes=tuple_bytes, approval_mode=e['approval_basis']['mode'],
        cards=cards, dispatcher=dispatcher,
        evidence_run=(evidence_run.run_id, evidence_run.run_attempt, request.evidence_artifact_id),
        candidate_run=(candidate_run.run_id, candidate_run.run_attempt, candidate_artifact.artifact_id,
                       candidate_artifact.digest),
        fast_gate_ignored=tuple((t['target'], t['fast_gate']['ignored']) for t in e['targets']),
        install=install, reservation=None, publication=None, alert_observation=observation), current_evidence_policy


def qualify_new(request, facts, inherited=None):
    """The pre-credential check of a new reservation of the requested version: V must be free and newer
    than every published version. It reads no archive and writes nothing. Its `Preliminary` permits
    only the reservation bookkeeping. `inherited` replaces the alert read for a job with no alert
    permission."""
    request = check_request(request)
    facts = _Facts(facts)
    version = request.version
    if facts.optional('ref', 'tags/reserve/v' + version) is not None:
        raise Refused(CONFLICT, 'v' + version + ' is already reserved; recover its reservation')
    if facts.optional('ref', 'tags/v' + version) is not None:
        raise Refused(CONFLICT, 'the tag v' + version + ' already exists')
    releases = facts.call('releases')
    if any(r.tag_name == 'v' + version for r in releases):
        raise Refused(CONFLICT, 'a release of v' + version + ' already exists')
    if any(m >= _minor(version) for m in _published(releases)):
        raise Refused(CONFLICT, 'v' + version + ' is not newer than every published version')
    fields, _ = _authenticate(facts, request, 'new', releases, None, _preliminary_cards('new'), inherited)
    return Preliminary(**fields)


def preliminary_recovery(request, facts):
    """The pre-credential check of an existing reservation. It reads no archive and writes nothing."""
    return _recover(request, facts, Preliminary, _preliminary_cards('recovery'))


def authenticate_recovery(request, facts):
    """The signer phase, after the sole STS exchange: authenticates the existing reservation of the
    requested version, its original E and T, and any publication, without the new-version predicate,
    and resolves every card through the authenticated archive. Writes nothing."""
    return _recover(request, facts, Qualified, _cards)


def authenticate_published(request, facts):
    """Historical reconciliation: the same reservation, E, T, caller and publication, with no current
    approval decision. The approval status is reported, never acted on. Writes nothing."""
    return _recover(request, facts, Reconciliation, _historical_cards)


def protected_main(facts):
    """Main's commit now, with its policy, evidence policy and trust records authenticated together."""
    facts = _Facts(facts)
    head = _Main(facts).head
    _policy_at(facts, head)
    return head


def check_source(facts, evidence_bytes, version, commit):
    """Cargo and the committed release exports at the reserved commit against E."""
    _source(_Facts(facts), _parse(ev.parse_release_record, evidence_bytes), version, commit)


def _recover(request, facts, result, resolve_cards):
    request = check_request(request)
    facts = _Facts(facts)
    version = request.version
    ref = facts.optional('ref', 'tags/reserve/v' + version)
    if ref is None:
        raise Refused(INCOMPLETE, 'v' + version + ' has no reservation to recover')
    if ref.object_type != 'tag':
        raise Refused(CONFLICT, 'the reservation of v' + version + ' is not an annotated tag')
    tag = facts.call('tag', ref.object_sha)
    if (tag.sha, tag.tag, tag.object_type) != (ref.object_sha, 'reserve/v' + version, 'commit'):
        raise Refused(CONFLICT, 'the reservation tag object is not reserve/v' + version + ' on a commit')
    try:
        reserved = ev.parse_tuple(tag.message_bytes).value
    except ControllerError as error:
        raise Refused(CONFLICT, 'the reservation message is not a tuple: ' + str(error))
    if reserved['version'] != version or reserved['commit'] != tag.object_sha:
        raise Refused(CONFLICT, 'the reservation tuple does not name its tag')
    if reserved['release_evidence_sha256'] != request.evidence_sha256:
        raise Refused(MISMATCH, 'the requested release record is not the reserved one')
    releases = facts.call('releases')
    fields, current_evidence_policy = _authenticate(facts, request, 'recovery', releases, reserved, resolve_cards)
    if fields['tuple_bytes'] != tag.message_bytes:
        raise Refused(MISMATCH, 'the reservation tuple is not the tuple of the authenticated facts')
    fields.update(reservation=Reservation(ref.sha256, tag.sha),
                  publication=_publication(facts, releases, version, fields['commit']))
    if result is Reconciliation:
        fields['channel_enabled'] = current_evidence_policy.value['approval_channel']['enabled'] is True
    return result(**fields)


def _publication(facts, releases, version, commit):
    mine = [r for r in releases if r.tag_name == 'v' + version]
    final = facts.optional('ref', 'tags/v' + version)
    if not mine:
        if final is None:
            return Publication('unpublished', None)
        if _tag_commit(facts, final) != commit:
            raise Refused(CONFLICT, 'the tag v' + version + ' exists without its release')
        return Publication('final_tag', None)
    if len(mine) != 1:
        raise Refused(CONFLICT, 'more than one release names v' + version)
    release = mine[0]
    if release.target_commitish != commit:
        raise Refused(CONFLICT, 'the release of v' + version + ' does not target the reserved commit')
    if release.draft:
        if final is not None and _tag_commit(facts, final) != commit:
            raise Refused(CONFLICT, 'the tag v' + version + ' is not the reserved commit')
        return Publication('draft', release.release_id)
    if final is None or _tag_commit(facts, final) != commit:
        raise Refused(CONFLICT, 'the published v' + version + ' is not the reserved commit')
    return Publication('published', release.release_id)


def _tag_commit(facts, ref):
    if ref.object_type == 'commit':
        return ref.object_sha
    tag = facts.call('tag', ref.object_sha)
    return tag.object_sha if tag.object_type == 'commit' else None
