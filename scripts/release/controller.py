#!/usr/bin/env python3
"""Reservation, the durable Sign attempt and the agreed exclusion state.

The order of one signing run:

1. Before credentials, `qualify_new` authenticates a new version, and `create_reservation` (the reserve
   job, the only Git writer) makes `refs/tags/reserve/v<V>` once. Neither reads the archive or AWS.
2. `Controller.admit` authenticates the current caller, the reservation and the protected admission
   record. `issue_credentials` makes the sole OIDC request and the sole STS assumption.
3. `open_session` authenticates the actual table, GENESIS, the authority lineage and the CURRENT mirror.
   It resolves every founder card, archives a first use, initializes the reservation row from the fresh
   reserve-job receipt and finalizes the grant from the server's actual STS expiry.
4. `reserve` is the adapter's context manager. KMS calls happen inside it. `before_public_key` guards
   GetPublicKey. `before_sign` acknowledges the durable attempt and returns one volatile `Permit`.
5. `retain_signature` persists the real bytes and the completion receipt. `recover` can return only
   verified retained bytes, a terminal published observation, or a refusal.

A crash never releases anything. No workflow, retry or person sends a second Sign for a reserved tuple
(D-KM1GB). A reservation gives version uniqueness; the admission protocol gives cooperative authority
exclusion through the protected code, the fixed lane and trusted maintenance. Neither replaces the other.
It is not a cross-provider transaction or a defence against an administrator who rewrites trusted history.

GitHub reads go through `facts` (`github.GitHub`, plus `release_trust`). The journal goes through a
`table` with four calls, each sent once and never retried:

  describe_table(arn, timeout) -> dict
  get_item(pk, sk, timeout) -> item or None                 GetItem, ConsistentRead
  transact_get_items(keys, timeout) -> [item or None, ...]  TransactGetItems, one snapshot
  transact_write_items(actions, token, timeout) -> WriteAck TransactWriteItems, one underlying request

A table raises `JournalUnavailable` (nothing was sent or read), `JournalOutcomeUnknown` (a write may have
applied) or `JournalConditionFailed` (a condition refused it). `WriteAck.underlying_requests` lets the
caller prove that exactly one request went out: a replayed success is not a fresh create.

Failures use `release controller: <code>: <reason>`. A reason never holds an input body or credential.
"""
import contextlib
from dataclasses import dataclass
import datetime
import functools
import hashlib
from pathlib import Path
import re
import struct
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import eligibility as elig  # noqa: E402
import evidence as ev  # noqa: E402
import github as gh  # noqa: E402

ControllerError = cand.ControllerError

# Refusal codes (a closed set); the text is `<code>: <reason>`.
INCOMPLETE = elig.INCOMPLETE
MISMATCH = elig.MISMATCH
UNAUTHORIZED = elig.UNAUTHORIZED
CONFLICT = elig.CONFLICT
LIMIT = elig.LIMIT
AUTHORITY = 'authority unavailable'
REVOKED = 'signer revoked'
ROLLBACK = 'policy rollback'
UNKNOWN = 'signing outcome unknown'
CODES = (INCOMPLETE, MISMATCH, UNAUTHORIZED, CONFLICT, LIMIT, AUTHORITY, REVOKED, ROLLBACK, UNKNOWN)

REPOSITORY = cand.REPOSITORY
REPOSITORY_ID = str(ev.REPOSITORY_ID)
OWNER_ID = '32169767'
ADMISSION_HEAD = 'heads/main'
ADMISSION_REF = 'refs/' + ADMISSION_HEAD
ADMISSION_PATH = 'release-keys/admission.json'
POLICY_PATH = elig.POLICY_PATH
LANE = 'rdy-core-release-authority'

# The reserve job's frozen identity. The release workflow emits these literals.
RESERVE_WORKFLOW = '.github/workflows/release.yml'
RESERVE_JOB_KEY = 'reserve'
RESERVE_JOB = 'Reserve release tuple'
RESERVE_ARTIFACT = 'reserve-receipt-'
RESERVE_MEMBER = 'reserve-receipt.json'

PRINCIPAL = 'rdy-release'
NAMESPACE = 'rdy-release-v1'
VERIFICATION_CONTRACT = 'rdy-release principal; rdy-release-v1 namespace; authentic exact manifest and reserved key'
AUDIENCE = 'sts.amazonaws.com'
STS_DURATION = 900
CLOCK_SKEW = 60          # the proposed allowance; qualification validates it before activation
SIGN_BUDGET = 120        # one aggregate deadline for KMS and retention
KMS_MAX = 60
JOURNAL_TIMEOUT = 20
BODY_MAX = 65536
SIGNATURE_MAX = 4096
ITEM_KEYS = ('pk', 'sk', 'body', 'body_sha256')
PUT_CONDITION = 'attribute_not_exists(pk) AND attribute_not_exists(sk)'
CAS_CONDITION = '#b = :old AND #d = :oldhash'
CAS_UPDATE = 'SET #b = :new, #d = :newhash'
CAS_NAMES = {'#b': 'body', '#d': 'body_sha256'}
U64_MAX = 2 ** 64 - 1
UINT_RE = re.compile(r'[1-9][0-9]{0,19}')
UUID4_RE = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}')

STATES = ('ACTIVE', 'DRAINING', 'CLOSED')
AUTHORITY_KEYS = ('schema', 'repository', 'repository_id', 'owner_id', 'account_id', 'region', 'table', 'table_id',
                  'admission_ref', 'admission_path', 'concurrency_group')
BOOTSTRAP_KEYS = ('signer_workflow_sha', 'policy_commit', 'policy_sha256', 'trust_sha256', 'key_arn', 'fingerprint',
                  'first_machine_tag', 'epoch')
GENESIS_KEYS = ('schema', 'authority', 'bootstrap')
ADMISSION_KEYS = ('schema', 'repository_id', 'epoch', 'state', 'signer_workflow_sha', 'key_arn', 'fingerprint',
                  'policy_commit', 'policy_sha256', 'trust_sha256', 'table_id', 'genesis_sha256', 'draining_holder')
# Bound to one epoch: a same-epoch ACTIVE to DRAINING to CLOSED move changes only the last two.
STABLE_KEYS = tuple(k for k in ADMISSION_KEYS if k not in ('state', 'draining_holder'))
HOLDER_KEYS = ('run_id', 'run_attempt', 'job_id', 'grant_id')
CURRENT_KEYS = ('schema', 'revision', 'admission', 'admission_sha256', 'admission_commit_floor')
FENCE_KEYS = ('schema', 'repository_id', 'epoch', 'fence')
GRANT_KEYS = ('schema', 'grant_id', 'repository_id', 'version', 'tuple_sha256', 'epoch', 'fence',
              'signer_workflow_sha', 'key_arn', 'fingerprint', 'policy_commit', 'policy_sha256', 'trust_sha256',
              'admission_commit', 'admission_sha256', 'run_id', 'run_attempt', 'job_id', 'jwt_iat', 'jwt_exp',
              'sts_expiration', 'role_max_session_duration', 'deadline')
ROW_KEYS = ('schema', 'repository_id', 'version', 'tuple_sha256', 'reservation_object', 'commit', 'genesis_sha256',
            'phase', 'revision', 'run_id', 'run_attempt', 'job_id')
ATTEMPT_KEYS = ('schema', 'state', 'repository_id', 'version', 'tuple_sha256', 'reservation_object', 'commit',
                'release_evidence_sha256', 'manifest_sha256', 'key_arn', 'fingerprint', 'policy_commit',
                'signer_workflow_sha', 'run_id', 'run_attempt', 'job_id', 'grant_id', 'fence')
COMPLETION_KEYS = ('schema', 'repository_id', 'version', 'tuple_sha256', 'attempt_sha256', 'signature_sha256',
                   'manifest_sha256', 'key_arn', 'fingerprint', 'verification_contract', 'run_id', 'run_attempt',
                   'job_id', 'grant_id', 'fence')
RECEIPT_KEYS = ('schema', 'repository_id', 'version', 'tuple_sha256', 'reservation_object', 'commit', 'run_id',
                'run_attempt', 'job_id', 'created')
AUTHORIZATION_KEYS = ('schema', 'repository_id', 'version', 'tuple_sha256', 'grant_id', 'fence', 'attempt_sha256',
                      'signature_sha256', 'cards')
AUTHORIZATION_CARD_KEYS = ('kind', 'receipt_sha256', 'envelope_sha256')


# ---------------------------------------------------------------------------------------------
# Outcomes
# ---------------------------------------------------------------------------------------------

class Refused(ControllerError):
    """A named refusal: `<code>: <reason>`."""

    def __init__(self, code, reason):
        if code not in CODES:
            raise ValueError('unknown refusal code')
        ControllerError.__init__(self, code + ': ' + reason)
        self.code = code
        self.reason = reason


class JournalUnavailable(Exception):
    """The journal could not be read, or a write was not sent."""


class JournalOutcomeUnknown(Exception):
    """A journal write may have applied."""


class JournalConditionFailed(Exception):
    """A journal write's condition refused it."""


def lift(error):
    """An eligibility refusal keeps its name under the controller's type."""
    if isinstance(error, elig.Refused):
        return Refused(error.code, error.reason)
    return error


def report(error, stream=None):
    """Writes the refusal to stderr. Controller failures exit 1."""
    (stream or sys.stderr).write('release controller: ' + str(error) + '\n')
    return 1


def _eligibility(call, *args):
    try:
        return call(*args)
    except elig.Refused as error:
        raise lift(error) from None


# ---------------------------------------------------------------------------------------------
# Inputs and results
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Enrollment(object):
    """The reviewed activation configuration. Nothing defaults: enrollment supplies the actual TableId
    and lineage digests. `signer_workflow_sha` is the H of this running code."""
    account_id: str
    region: str
    table: str
    table_id: str
    genesis_sha256: str
    authority_sha256: str
    signer_workflow_sha: str


@dataclass(frozen=True)
class Holder(object):
    """The exact authenticated job that holds the lane."""
    run_id: int
    run_attempt: int
    job_id: int


@dataclass(frozen=True)
class WriteAck(object):
    underlying_requests: int


@dataclass(frozen=True)
class OidcToken(object):
    """The token is only passed to the one STS call. Only `iat` and `exp` are kept."""
    token: str
    iat: int
    exp: int

    def __repr__(self):
        return 'OidcToken(iat=' + str(self.iat) + ', exp=' + str(self.exp) + ')'


@dataclass(frozen=True)
class StsReceipt(object):
    """Nonsecret facts of the one STS assumption, from the server's response and the verified role."""
    expiration: int
    role_arn: str
    requested_duration: int
    role_max_session_duration: int


@dataclass(frozen=True)
class Invocation(object):
    """A prior invocation of this tuple. `stopped` is `before_attempt` only with proof of that."""
    run_id: int
    run_attempt: int
    job_id: int
    stopped: str


@dataclass(frozen=True)
class History(object):
    complete: bool
    invocations: tuple


@dataclass(frozen=True)
class RetainedSignature(object):
    """Real retained bytes, verified under the reserved key. Sign is never needed for them."""
    signature: bytes
    version: str
    tuple_sha256: str
    attempt_sha256: str


@dataclass(frozen=True)
class PublishedBytes(object):
    """What the exact-byte verifier read and verified from the published release."""
    release_id: int
    tag_name: str
    commit: str
    draft: bool
    manifest_sha256: str
    archives: tuple
    notes_sha256: str
    signature_sha256: str
    fingerprint: str
    principal: str
    namespace: str
    signature_verified: bool
    assets_complete: bool


@dataclass(frozen=True)
class AlreadyPublishedMatching(object):
    """A terminal, read-only observation: this exact tuple is published. It is history, not current
    eligibility or a claim that the release remains trusted. It holds no signature bytes and leads to no
    Sign, draft, upload, promotion, repair or notification. The current status is reported apart."""
    version: str
    commit: str
    tuple_sha256: str
    release_id: int
    manifest_sha256: str
    signature_sha256: str
    channel_enabled: bool
    revoked_receipts: tuple
    signer_revoked: object


@dataclass(frozen=True)
class AdmissionSnapshot(object):
    """The admission record at one authenticated commit."""
    commit: str
    data: bytes
    sha256: str
    value: dict

    @property
    def state(self):
        return self.value['state']

    @property
    def epoch(self):
        return self.value['epoch']


@dataclass(frozen=True)
class Mirror(object):
    """The CURRENT record."""
    body: bytes
    revision: str
    admission: dict
    sha256: str
    floor: str


@dataclass(frozen=True)
class CensusJob(object):
    """A credential-capable job. `issuance` is `none` (proved no request), `issued` or `unresolved`."""
    run_id: int
    run_attempt: int
    job_id: int
    status: str
    issuance: str


@dataclass(frozen=True)
class IssuerReceipt(object):
    """Nonsecret issuance receipts only."""
    run_id: int
    run_attempt: int
    job_id: int
    jwt_iat: object
    jwt_exp: object
    sts_expiration: object
    role_max_session_duration: object = None


@dataclass(frozen=True)
class Census(object):
    """Every prior credential-issuing attempt and job since the last drained epoch."""
    complete: bool
    role_max_session_duration: object
    jobs: tuple
    issuers: tuple


@dataclass(frozen=True)
class ApprovedUpdate(object):
    signer_workflow_sha: str
    key_arn: str
    fingerprint: str
    policy_commit: str
    policy_sha256: str
    trust_sha256: str


@dataclass(frozen=True)
class ContainmentReceipt(object):
    github_closed: bool
    mirror_closed: bool
    results: tuple


# ---------------------------------------------------------------------------------------------
# Wire types
# ---------------------------------------------------------------------------------------------

def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _canon(value):
    return cand.canonical(value)


def uint64(value, what):
    """A canonical decimal string in 1..2^64-1."""
    if not (isinstance(value, str) and UINT_RE.fullmatch(value) and int(value) <= U64_MAX):
        raise ControllerError(what + ' is not a canonical unsigned 64-bit decimal')
    return value


def _strict(data, keys, what):
    """The exact canonical bytes of a closed object. Duplicates, floats and spaces refuse."""
    try:
        value = cand.loads(data, BODY_MAX)
    except RecursionError:
        raise ControllerError('evidence is not JSON')
    cand.closed(value, keys, what)
    if _canon(value) != data:
        raise ControllerError(what + ' is not canonical')
    return value


def _malformed(error, what):
    """A closed-record refusal keeps its `unknown field` text. Nothing else of the input is echoed."""
    return str(error) if str(error).startswith('unknown field') else what + ' is malformed'


def _schema_one(value, what):
    if type(value['schema']) is not int or value['schema'] != 1:
        raise ControllerError(what + ' schema is not 1')


def authority_sha256(authority):
    """SHA-256 of the canonical `GENESIS.authority` bytes."""
    return _sha(_canon(authority))


def parse_genesis(data, enrollment):
    """GENESIS: immutable, closed, and neither body embeds a digest. Later maintenance never changes it.
    A recreated table or a changed projection needs explicit reenrollment."""
    value = _strict(data, GENESIS_KEYS, 'GENESIS')
    _schema_one(value, 'GENESIS')
    if _sha(data) != enrollment.genesis_sha256:
        raise ControllerError('GENESIS is not the enrolled genesis')
    authority = cand.closed(value['authority'], AUTHORITY_KEYS, 'the authority projection')
    _schema_one(authority, 'the authority projection')
    for key, expected in (('repository', REPOSITORY), ('repository_id', REPOSITORY_ID), ('owner_id', OWNER_ID),
                          ('account_id', enrollment.account_id), ('region', enrollment.region),
                          ('table', enrollment.table), ('table_id', enrollment.table_id),
                          ('admission_ref', ADMISSION_REF), ('admission_path', ADMISSION_PATH),
                          ('concurrency_group', LANE)):
        ev.const(authority[key], expected, 'the authority ' + key.replace('_', ' '))
    if authority_sha256(authority) != enrollment.authority_sha256:
        raise ControllerError('the authority projection is not the enrolled authority')
    bootstrap = cand.closed(value['bootstrap'], BOOTSTRAP_KEYS, 'the bootstrap')
    ev.commit(bootstrap['signer_workflow_sha'], 'the bootstrap signer')
    ev.commit(bootstrap['policy_commit'], 'the bootstrap policy commit')
    ev.sha256(bootstrap['policy_sha256'], 'the bootstrap policy digest')
    ev.sha256(bootstrap['trust_sha256'], 'the bootstrap trust digest')
    ev.key_arn(bootstrap['key_arn'], 'the bootstrap key ARN')
    ev.fingerprint(bootstrap['fingerprint'], 'the bootstrap fingerprint')
    ev.tag(bootstrap['first_machine_tag'], 'the bootstrap first machine tag')
    if int(bootstrap['first_machine_tag'].split('.')[1]) <= elig.HISTORICAL_LAST_MINOR:
        raise ControllerError('the bootstrap first machine tag is not after the historical range')
    uint64(bootstrap['epoch'], 'the bootstrap epoch')
    return value


def _check_admission(value):
    what = 'the admission record'
    cand.closed(value, ADMISSION_KEYS, what)
    _schema_one(value, what)
    ev.const(value['repository_id'], REPOSITORY_ID, what + ' repository id')
    uint64(value['epoch'], what + ' epoch')
    if value['state'] not in STATES:
        raise ControllerError(what + ' state is not ACTIVE, DRAINING or CLOSED')
    ev.commit(value['signer_workflow_sha'], what + ' signer')
    ev.key_arn(value['key_arn'], what + ' key ARN')
    ev.fingerprint(value['fingerprint'], what + ' fingerprint')
    ev.commit(value['policy_commit'], what + ' policy commit')
    ev.sha256(value['policy_sha256'], what + ' policy digest')
    ev.sha256(value['trust_sha256'], what + ' trust digest')
    ev.pattern(value['table_id'], ev.UUID_RE, what + ' table id', 'a table UUID')
    ev.sha256(value['genesis_sha256'], what + ' genesis digest')
    holder = value['draining_holder']
    if value['state'] == 'DRAINING':
        cand.closed(holder, HOLDER_KEYS, what + ' draining holder')
        for key in ('run_id', 'run_attempt', 'job_id'):
            uint64(holder[key], what + ' draining holder ' + key.replace('_', ' '))
        ev.pattern(holder['grant_id'], UUID4_RE, what + ' draining grant', 'a lowercase UUIDv4')
    elif holder is not None:
        raise ControllerError(what + ' names a draining holder outside DRAINING')
    return value


def parse_current(data):
    """The CURRENT successor: the exact admission document, its digest and an already-existing floor."""
    what = 'the journal mirror'
    value = _strict(data, CURRENT_KEYS, what)
    _schema_one(value, what)
    uint64(value['revision'], what + ' revision')
    admission = _check_admission(value['admission'])
    ev.sha256(value['admission_sha256'], what + ' digest')
    if value['admission_sha256'] != _sha(_canon(admission)):
        raise ControllerError(what + ' digest is not its admission document')
    ev.commit(value['admission_commit_floor'], what + ' floor')
    return Mirror(data, value['revision'], admission, value['admission_sha256'], value['admission_commit_floor'])


def _status(facts, base, head):
    """Authenticated graph reachability: `ahead` means `head` strictly descends from `base`."""
    try:
        return facts.compare(base, head).status
    except ControllerError:
        raise Refused(AUTHORITY, 'the commit ancestry is unavailable')


def read_admission(facts):
    """The protected record: the exact ref, the file at that commit, the ref again. The two ref reads
    must agree. GitHub promises no linearizable snapshot: the lane and maintenance supply exclusion."""
    try:
        first = facts.ref(ADMISSION_HEAD)
        found = facts.contents(ADMISSION_PATH, first.object_sha, ev.RECORD_MAX)
        second = facts.ref(ADMISSION_HEAD)
    except ControllerError:
        raise Refused(AUTHORITY, 'the admission record is unavailable')
    if first.object_type != 'commit' or (first.object_sha, first.object_type) != (second.object_sha, second.object_type):
        raise Refused(AUTHORITY, 'main moved during the admission read')
    try:
        value = _check_admission(_strict(found.data, ADMISSION_KEYS, 'the admission record'))
    except ControllerError as error:
        raise Refused(AUTHORITY, _malformed(error, 'the admission record'))
    return AdmissionSnapshot(first.object_sha, found.data, _sha(found.data), value)


def _signing_trust(facts, release, policy_commit):
    """The version's key for a new signing, at the current policy. A revoked key refuses."""
    try:
        answer = facts.release_trust(release.version, policy_commit, True)
        cand.closed(answer, elig.TRUST_KEYS, 'the release trust result')
    except ControllerError as error:
        if 'is revoked for new signing' in str(error):
            raise Refused(REVOKED, 'the key for v' + release.version + ' is revoked for new signing')
        raise Refused(AUTHORITY, 'the release trust is unavailable')
    if (answer['version'], answer['fingerprint'], answer['transition_sha256']) != (
            release.version, release.fingerprint, release.transition_sha256):
        raise Refused(AUTHORITY, 'the release trust is not the reserved signer')
    return answer


def check_authority(facts, enrollment, release, named):
    """Current authority for this release, from protected main only. Candidate files and caller JSON
    never reach this. `named(holder)` says whether a DRAINING record names this job. Returns the
    snapshot and the trust result."""
    snapshot = read_admission(facts)
    value = snapshot.value
    if snapshot.state == 'CLOSED':
        raise Refused(AUTHORITY, 'admission is CLOSED')
    if snapshot.state == 'DRAINING' and not named(value['draining_holder']):
        raise Refused(AUTHORITY, 'admission is DRAINING for another holder')
    if not (value['signer_workflow_sha'] == release.signer_workflow_sha == enrollment.signer_workflow_sha):
        raise Refused(AUTHORITY, 'the admission record names another signer')
    if (value['key_arn'], value['fingerprint']) != (release.key_arn, release.fingerprint):
        raise Refused(AUTHORITY, 'the admission record names another key')
    if (value['table_id'], value['genesis_sha256']) != (enrollment.table_id, enrollment.genesis_sha256):
        raise Refused(AUTHORITY, 'the admission record names another journal')
    if _status(facts, release.policy_commit, value['policy_commit']) not in ('ahead', 'identical'):
        raise Refused(ROLLBACK, 'the current policy commit does not descend from the reserved policy commit')
    if _status(facts, value['policy_commit'], snapshot.commit) not in ('ahead', 'identical'):
        raise Refused(ROLLBACK, 'the current policy commit is not on main')
    try:
        policy = facts.contents(POLICY_PATH, value['policy_commit'], ev.RECORD_MAX)
        parsed = ev.parse_policy(policy.data)
        current_policy = ev.parse_policy(facts.contents(POLICY_PATH, snapshot.commit, ev.RECORD_MAX).data)
    except ControllerError:
        raise Refused(AUTHORITY, 'the protected policy is unavailable')
    # Main may move the evidence policy (the channel and receipt revocations) with no maintenance: those
    # take effect at once through eligibility. Any other change of the policy needs a new epoch.
    if _sha(policy.data) != value['policy_sha256'] or _unmoved(parsed.value) != _unmoved(current_policy.value):
        raise Refused(AUTHORITY, 'the admission policy digest is not the protected policy')
    if parsed.value['records_sha256'] != value['trust_sha256']:
        raise Refused(AUTHORITY, 'the admission trust digest is not the protected trust')
    return snapshot, _signing_trust(facts, release, value['policy_commit'])


def _unmoved(policy):
    return {key: value for key, value in policy.items() if key != 'evidence_policy_sha256'}


def _stable(admission):
    return tuple(admission[key] for key in STABLE_KEYS)


def _check_floor(facts, floor, snapshot):
    """The observed commit descends from the floor; an ACTIVE document descends strictly."""
    allowed = ('ahead',) if snapshot.state == 'ACTIVE' else ('ahead', 'identical')
    if _status(facts, floor, snapshot.commit) not in allowed:
        raise Refused(AUTHORITY, 'the admission commit does not descend strictly from the journal floor')


def _agree_for_grant(facts, snapshot, mirror):
    """A new grant needs both sources on the exact same document. A partial close admits nobody."""
    if snapshot.data != _canon(mirror.admission):
        if _stable(snapshot.value) == _stable(mirror.admission):
            raise Refused(AUTHORITY, 'the close is incomplete; no new grant finalizes')
        raise Refused(AUTHORITY, 'the journal mirror disagrees with the admission record')
    _check_floor(facts, mirror.floor, snapshot)


# ---------------------------------------------------------------------------------------------
# The journal
# ---------------------------------------------------------------------------------------------

class Journal(object):
    """The enrolled table. Each write has a fresh private token and is sent once."""

    def __init__(self, table, enrollment):
        self.table = table
        self.enrollment = enrollment
        self.arn = ('arn:aws:dynamodb:' + enrollment.region + ':' + enrollment.account_id + ':table/'
                    + enrollment.table)

    def verify(self):
        """The actual table, status, key schema and GENESIS. A stored assertion alone is not enough."""
        try:
            described = self.table.describe_table(self.arn, JOURNAL_TIMEOUT)
            body = self.get('CONTROL#' + REPOSITORY_ID, 'GENESIS')
        except JournalUnavailable:
            raise Refused(AUTHORITY, 'the journal is unavailable')
        schema = [{'AttributeName': 'pk', 'KeyType': 'HASH'}, {'AttributeName': 'sk', 'KeyType': 'RANGE'}]
        if (described.get('TableArn'), described.get('TableId'), described.get('TableStatus'),
                described.get('KeySchema')) != (self.arn, self.enrollment.table_id, 'ACTIVE', schema):
            raise Refused(AUTHORITY, 'the journal table is not the enrolled table')
        if body is None:
            raise Refused(AUTHORITY, 'the journal has no GENESIS')
        try:
            parse_genesis(body, self.enrollment)
        except ControllerError:
            raise Refused(AUTHORITY, 'the journal GENESIS is not the enrolled GENESIS')

    def lineage(self):
        return {'table_id': self.enrollment.table_id, 'genesis_sha256': self.enrollment.genesis_sha256,
                'authority_sha256': self.enrollment.authority_sha256}

    @staticmethod
    def _body(item, key):
        if item is None:
            return None
        if not (isinstance(item, dict) and sorted(item) == sorted(ITEM_KEYS) and item['pk'] == {'S': key[0]}
                and item['sk'] == {'S': key[1]}):
            raise JournalUnavailable('the item is malformed')
        body = item['body'].get('B') if isinstance(item['body'], dict) else None
        digest = item['body_sha256'].get('S') if isinstance(item['body_sha256'], dict) else None
        if not isinstance(body, bytes) or len(body) > BODY_MAX or digest != _sha(body):
            raise JournalUnavailable('the item is malformed')
        return body

    def get(self, pk, sk, timeout=JOURNAL_TIMEOUT):
        return self._body(self.table.get_item(pk, sk, timeout), (pk, sk))

    def snapshot(self, keys, timeout=JOURNAL_TIMEOUT):
        """One atomic read of several items, as `{key: bytes or None}`."""
        items = self.table.transact_get_items(list(keys), timeout)
        if not isinstance(items, list) or len(items) != len(keys):
            raise JournalUnavailable('the snapshot is malformed')
        return {key: self._body(item, key) for key, item in zip(keys, items)}

    def write(self, actions, timeout=JOURNAL_TIMEOUT):
        """One TransactWriteItems with a new token. True only for one fresh underlying request."""
        ack = self.table.transact_write_items(actions, str(uuid.uuid4()), timeout)
        return isinstance(ack, WriteAck) and ack.underlying_requests == 1

    def _base(self, action):
        return dict(action, TableName=self.enrollment.table, ReturnValuesOnConditionCheckFailure='NONE')

    def put(self, key, body):
        return {'Put': self._base({
            'Item': {'pk': {'S': key[0]}, 'sk': {'S': key[1]}, 'body': {'B': body}, 'body_sha256': {'S': _sha(body)}},
            'ConditionExpression': PUT_CONDITION})}

    @staticmethod
    def _key(key):
        return {'pk': {'S': key[0]}, 'sk': {'S': key[1]}}

    def cas(self, key, old, new):
        return {'Update': self._base({
            'Key': self._key(key), 'UpdateExpression': CAS_UPDATE, 'ConditionExpression': CAS_CONDITION,
            'ExpressionAttributeNames': dict(CAS_NAMES),
            'ExpressionAttributeValues': {':old': {'B': old}, ':oldhash': {'S': _sha(old)}, ':new': {'B': new},
                                          ':newhash': {'S': _sha(new)}}})}

    def exact(self, key, old):
        return {'ConditionCheck': self._base({
            'Key': self._key(key), 'ConditionExpression': CAS_CONDITION, 'ExpressionAttributeNames': dict(CAS_NAMES),
            'ExpressionAttributeValues': {':old': {'B': old}, ':oldhash': {'S': _sha(old)}}})}

    def absent(self, key):
        return {'ConditionCheck': self._base({'Key': self._key(key), 'ConditionExpression': PUT_CONDITION})}


JOURNAL_ERRORS = (JournalUnavailable, JournalOutcomeUnknown, JournalConditionFailed)
CURRENT = ('CONTROL#' + REPOSITORY_ID, 'CURRENT')


def _fence_key(epoch):
    return ('FENCE#' + REPOSITORY_ID + '#' + epoch, 'COUNTER')


def _version_key(version):
    return ('VERSION#' + REPOSITORY_ID + '#' + version, 'RESERVATION')


def _tuple_key(version, tuple_sha256, sk):
    return ('TUPLE#' + REPOSITORY_ID + '#' + version + '#' + tuple_sha256, sk)


def _abandonment_key(version, tuple_sha256):
    return ('ABANDONMENT#' + REPOSITORY_ID + '#' + version + '#' + tuple_sha256, 'RECORD')


def _grant_key(grant_id):
    return ('GRANT#' + grant_id, 'RECORD')


class _SessionFacts(object):
    """The GitHub facts, plus the authenticated archive. It exists only after the sole STS exchange."""

    def __init__(self, facts, journal):
        self.facts = facts
        self.journal = journal

    def __getattr__(self, name):
        return getattr(self.facts, name)

    def approval_record(self, receipt_sha256):
        try:
            return self.journal.get('APPROVAL#' + receipt_sha256, 'RECORD')
        except JournalUnavailable:
            raise ControllerError('the approval archive is unavailable')

    def approval_lineage(self):
        return self.journal.lineage()


# ---------------------------------------------------------------------------------------------
# The reservation
# ---------------------------------------------------------------------------------------------

def _final_tag_present(facts, version):
    """The final tag, read now. It gives no first Sign, whatever else a reader sees beside it. Only an
    authenticated 404 is absence."""
    try:
        facts.ref('tags/v' + version)
    except gh.Absent:
        return False
    except ControllerError:
        raise Refused(UNKNOWN, 'the final tag read is unavailable')
    return True


def _readback(facts, preliminary):
    """The mandatory exact readback: the ref, its annotated object, the message bytes, the direct commit
    target, and Cargo and the committed release exports at that commit against E."""
    version = preliminary.version
    name = 'tags/reserve/v' + version
    try:
        ref = facts.ref(name)
    except gh.Absent:
        raise Refused(CONFLICT, 'the reservation of v' + version + ' is missing')
    except ControllerError:
        raise Refused(CONFLICT, 'the reservation read is unavailable')
    if ref.object_type != 'tag':
        raise Refused(CONFLICT, 'the reservation of v' + version + ' is not an annotated tag')
    try:
        tag = facts.tag(ref.object_sha)
    except ControllerError:
        raise Refused(CONFLICT, 'the reservation object is unavailable')
    if tag.sha != ref.object_sha or tag.tag != 'reserve/v' + version:
        raise Refused(CONFLICT, 'the reservation object is not reserve/v' + version)
    if tag.object_type != 'commit':
        raise Refused(CONFLICT, 'the reservation object does not target a commit directly')
    if tag.object_sha != preliminary.commit:
        raise Refused(CONFLICT, 'the reservation object does not target the qualified commit')
    if tag.message_bytes != preliminary.tuple_bytes:
        raise Refused(CONFLICT, 'the reservation message is not the qualified tuple')
    try:
        facts.commit(tag.object_sha)
    except ControllerError:
        raise Refused(CONFLICT, 'the reserved commit is unavailable')
    _eligibility(elig.check_source, facts, preliminary.evidence_bytes, version, preliminary.commit)
    return tag.sha


def _receipt(preliminary, holder, reservation_object, created):
    return _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'version': preliminary.version,
                   'tuple_sha256': preliminary.tuple_sha256, 'reservation_object': reservation_object,
                   'commit': preliminary.commit, 'run_id': str(holder.run_id), 'run_attempt': str(holder.run_attempt),
                   'job_id': str(holder.job_id), 'created': created})


def _reconcile(facts, preliminary, holder, reason):
    """A timeout, 409 or 422 on either create: read, never repeat the write. A 422 is not assumed to
    mean that the ref exists. An exact winner continues; it is not a fresh create."""
    name = 'tags/reserve/v' + preliminary.version
    try:
        facts.ref(name)
    except gh.Absent:
        raise Refused(CONFLICT, 'the reservation outcome is unresolved after ' + reason
                      + '; reconcile again before any creation')
    except ControllerError:
        raise Refused(CONFLICT, 'the reservation read is unavailable')
    return _receipt(preliminary, holder, _readback(facts, preliminary), False)


def create_reservation(facts, preliminary, holder):
    """The reserve job's one write path. CREATE of the ref is the commit point; object creation alone
    reserves nothing. Never updates or deletes. Returns the canonical receipt for the signer job, whose
    `created` is true only for a fresh create with an exact readback. It needs no KMS, OIDC or archive."""
    if not isinstance(preliminary, elig.Preliminary):
        raise Refused(AUTHORITY, 'a reservation needs the preliminary result')
    version = preliminary.version
    name = 'tags/reserve/v' + version
    try:
        facts.ref(name)
    except gh.Absent:
        if preliminary.mode != 'new':
            raise Refused(CONFLICT, 'the reservation of v' + version + ' was observed and is gone: durable state is lost')
    except ControllerError:
        raise Refused(CONFLICT, 'the reservation read is unavailable')
    else:
        return _receipt(preliminary, holder, _readback(facts, preliminary), False)
    try:
        tag = facts.create_tag_object('reserve/v' + version, preliminary.tuple_bytes.decode('utf-8'), preliminary.commit)
    except gh.ReconcileRequired as error:
        return _reconcile(facts, preliminary, holder, error.reason)
    try:
        facts.create_ref('refs/' + name, tag.sha)
    except gh.ReconcileRequired as error:
        return _reconcile(facts, preliminary, holder, error.reason)
    if _readback(facts, preliminary) != tag.sha:
        raise Refused(CONFLICT, 'the reservation object is not the object this job created')
    return _receipt(preliminary, holder, tag.sha, True)


# ---------------------------------------------------------------------------------------------
# The signer
# ---------------------------------------------------------------------------------------------

def _staged(stage):
    """Lifts eligibility refusals and keeps each refusal's nonsecret reason in the receipts."""
    def wrap(method):
        @functools.wraps(method)
        def inner(self, *args, **kwargs):
            try:
                return method(self, *args, **kwargs)
            except elig.Refused as error:
                lifted = lift(error)
                self._note(stage, lifted)
                raise lifted from None
            except Refused as error:
                self._note(stage, error)
                raise
        return inner
    return wrap


def _manifest_bytes(release):
    return ''.join(digest + '  ' + name + '\n' for name, digest, _ in release.artifacts).encode('utf-8')


def _preimage_digest(manifest):
    """SHA-256 of the SSHSIG signing preimage of the manifest: what the adapter asks KMS to sign."""
    def field(data):
        return struct.pack('>I', len(data)) + data
    return _sha(b'SSHSIG' + field(NAMESPACE.encode()) + field(b'') + field(b'sha512')
                + field(hashlib.sha512(manifest).digest()))


class Permit(object):
    """One volatile dispatch right. It is never serialized or rebuilt after a restart."""

    __slots__ = ('_owner', '_used')

    def __init__(self, owner):
        self._owner = owner
        self._used = False

    def consume(self):
        """Immediately before `Aws.request("sign", ...)`. Returns the KMS timeout in seconds."""
        if self._used:
            raise Refused(UNKNOWN, 'the dispatch permit was already used')
        self._used = True
        return self._owner._dispatching()

    def __reduce_ex__(self, protocol):
        raise TypeError('a dispatch permit cannot be serialized')

    def __copy__(self):
        raise TypeError('a dispatch permit cannot be copied')

    def __deepcopy__(self, memo):
        raise TypeError('a dispatch permit cannot be copied')


class Controller(object):
    """One invocation of the signer: one authenticated job, one STS session, at most one attempt."""

    def __init__(self, request, facts, enrollment, holder, clock, verifier, history, run_evidence,
                 reserve_artifact_id=None, published_verifier=None, new_id=None):
        self.request = request
        self.facts = facts
        self.enrollment = enrollment
        self.holder = holder
        self.clock = clock
        self.verifier = verifier
        self.history = history
        self.run_evidence = run_evidence
        self.reserve_artifact_id = reserve_artifact_id
        self.published_verifier = published_verifier
        self.new_id = new_id or (lambda: str(uuid.uuid4()))
        self.journal = None
        self.preliminary = None
        self.qualified = None
        self.grant_id = None
        self.grant_bytes = None
        self.grant = None
        self.sts = None
        self.oidc = None
        self.signer_line = None
        self.terminal = None
        self._seen = None
        self._inside = False
        self._attempted = False
        self._attempt_bytes = None
        self._row_bytes = None
        self._sign_deadline = None
        self._dispatched = False
        self._retained = None
        self._refusals = []

    # -- receipts ------------------------------------------------------------------------------

    def _note(self, stage, error):
        self._refusals.append({'stage': stage, 'code': error.code, 'reason': error.reason})

    def refusals(self):
        """The nonsecret reasons of this invocation's refusals."""
        return [dict(entry) for entry in self._refusals]

    # -- shared checks -------------------------------------------------------------------------

    def _terminal_text(self, version):
        return 'v' + version + ' is published; this observation is terminal'

    def _not_terminal(self):
        if self.terminal is not None:
            raise Refused(CONFLICT, self._terminal_text(self.terminal.version))

    def _named(self, holder):
        return holder == {'run_id': str(self.holder.run_id), 'run_attempt': str(self.holder.run_attempt),
                          'job_id': str(self.holder.job_id), 'grant_id': self.grant_id}

    def _authority(self, release):
        snapshot, trust = check_authority(self.facts, self.enrollment, release, self._named)
        self.signer_line = trust['signer_line']
        return snapshot

    def _check_holder(self):
        """The running job of this exact run attempt, in the release workflow."""
        request = self.request
        if (request.dispatch_run_id, request.dispatch_run_attempt) != (self.holder.run_id, self.holder.run_attempt):
            raise Refused(AUTHORITY, 'the request is not this run attempt')
        try:
            run = self.facts.run_attempt(self.holder.run_id, self.holder.run_attempt)
            jobs = self.facts.attempt_jobs(self.holder.run_id, self.holder.run_attempt)
        except ControllerError:
            raise Refused(AUTHORITY, 'the run attempt is unavailable')
        if run.path != RESERVE_WORKFLOW:
            raise Refused(AUTHORITY, 'the run is not the release workflow')
        found = [j for j in jobs if j.job_id == self.holder.job_id]
        if len(found) != 1 or found[0].status != 'in_progress':
            raise Refused(AUTHORITY, "the job is not this run attempt's running job")

    def _authorize(self):
        """The signer phase of eligibility, over the authenticated archive. It reads the reservation, E,
        T, the source and every card again, so it is the full readback too."""
        return _eligibility(elig.authenticate_recovery, self.request, _SessionFacts(self.facts, self.journal))

    def _read_mirror(self, body):
        if body is None:
            raise Refused(AUTHORITY, 'the journal mirror is missing')
        try:
            mirror = parse_current(body)
        except ControllerError as error:
            raise Refused(AUTHORITY, _malformed(error, 'the journal mirror'))
        if self._seen is not None:
            revision, floor = self._seen
            if int(mirror.revision) < revision or _status(self.facts, floor, mirror.floor) not in ('ahead', 'identical'):
                raise Refused(AUTHORITY, 'the journal floor regressed')
        self._seen = (int(mirror.revision), mirror.floor)
        return mirror

    def _agree_for_holder(self, snapshot, mirror, grant):
        """An existing exact holder may finish under the same epoch and policy, during a close. Closing
        admission does not revoke its grant. A closed mirror, or another name, stops it."""
        if grant is not None and not (grant['epoch'] == snapshot.epoch == mirror.admission['epoch']):
            raise Refused(AUTHORITY, 'the grant is from another epoch')
        if _stable(snapshot.value) != _stable(mirror.admission):
            raise Refused(AUTHORITY, 'the journal mirror disagrees with the admission record')
        if mirror.admission['state'] == 'CLOSED':
            raise Refused(AUTHORITY, 'the journal mirror is CLOSED')
        if mirror.admission['state'] == 'DRAINING' and not self._named(mirror.admission['draining_holder']):
            raise Refused(AUTHORITY, 'the journal mirror is DRAINING for another holder')
        _check_floor(self.facts, mirror.floor, snapshot)

    def _need_context(self):
        self._not_terminal()
        if not self._inside:
            raise Refused(AUTHORITY, 'a KMS request needs the reservation context')

    def _check_budget(self):
        if self.clock.now() + SIGN_BUDGET > int(self.grant['deadline']):
            raise Refused(AUTHORITY, 'the grant does not leave the signing budget')

    def _keys(self, release):
        version, digest = release.version, release.tuple_sha256
        return {'current': CURRENT, 'row': _version_key(version), 'attempt': _tuple_key(version, digest, 'ATTEMPT'),
                'signature': _tuple_key(version, digest, 'SIGNATURE'),
                'completion': _tuple_key(version, digest, 'COMPLETION'),
                'abandonment': _abandonment_key(version, digest)}

    # -- before credentials --------------------------------------------------------------------

    @_staged('admit')
    def admit(self):
        """Binds the pending facts of this attempt. They are not durable, not a grant and not a permit."""
        self._not_terminal()
        self._check_holder()
        grant_id = self.new_id()
        if not (isinstance(grant_id, str) and UUID4_RE.fullmatch(grant_id)):
            raise Refused(AUTHORITY, 'the grant id is not a lowercase UUIDv4')
        reconciliation = _eligibility(elig.authenticate_published, self.request, self.facts)
        if reconciliation.publication.state == 'published':
            raise Refused(CONFLICT, self._terminal_text(reconciliation.version))
        preliminary = _eligibility(elig.preliminary_recovery, self.request, self.facts)
        self.grant_id = grant_id
        snapshot = self._authority(preliminary)
        self._evidence('pending', {'grant_id': grant_id, 'tuple_sha256': preliminary.tuple_sha256,
                                   'epoch': snapshot.epoch, 'signer_workflow_sha': preliminary.signer_workflow_sha,
                                   'policy_commit': preliminary.policy_commit})
        self.preliminary = preliminary

    def _evidence(self, name, facts):
        try:
            self.run_evidence.record(self.holder, name, facts)
        except ControllerError:
            raise Refused(AUTHORITY, 'the run evidence is unavailable')

    @_staged('issue')
    def issue_credentials(self, provider):
        """One OIDC request and one STS assumption per attempt, with no refresh. A failed request is
        still the one request. The token is passed on and never kept."""
        self._not_terminal()
        if self.preliminary is None:
            raise Refused(AUTHORITY, 'the attempt is not admitted')
        self._authority(self.preliminary)
        try:
            rows = self.run_evidence.boundaries(self.holder)
        except ControllerError:
            raise Refused(AUTHORITY, 'the run evidence is unavailable')
        if any(row[1] == 'oidc_requested' for row in rows):
            raise Refused(AUTHORITY, 'an attempt has one OIDC request and one STS assumption')
        self._evidence('oidc_requested', {'audience': AUDIENCE})
        try:
            token = provider.request_oidc(AUDIENCE)
            self._evidence('oidc_issued', {'jwt_iat': token.iat, 'jwt_exp': token.exp})
            self._evidence('sts_requested', {'duration_seconds': STS_DURATION})
            sts = provider.assume_role(token.token, STS_DURATION)
            oidc = (token.iat, token.exp)
        except Refused:
            raise
        except ControllerError:
            raise Refused(AUTHORITY, 'the credential request failed')
        del token
        self._evidence('sts_issued', {'sts_expiration': sts.expiration, 'requested_duration': sts.requested_duration,
                                      'role_max_session_duration': sts.role_max_session_duration})
        if sts.role_arn != self._policy_of(self.preliminary).value['role_arn']:
            raise Refused(AUTHORITY, 'the assumed role is not the policy role')
        if sts.requested_duration != STS_DURATION:
            raise Refused(AUTHORITY, 'the credentials are not the requested duration')
        if not (cand.is_int(sts.expiration, 1) and cand.is_int(sts.role_max_session_duration, 1)):
            raise Refused(AUTHORITY, 'the credentials have no server expiry')
        if self.clock.now() + SIGN_BUDGET > sts.expiration - CLOCK_SKEW:
            raise Refused(AUTHORITY, 'the credentials do not leave the signing budget')
        self.sts, self.oidc = sts, oidc

    def _policy_of(self, release):
        try:
            return ev.parse_policy(self.facts.contents(POLICY_PATH, release.policy_commit, ev.RECORD_MAX).data)
        except ControllerError:
            raise Refused(AUTHORITY, 'the protected policy is unavailable')

    # -- after the sole STS exchange -----------------------------------------------------------

    @_staged('session')
    def open_session(self, table):
        """Authenticates the table, the lineage and the control snapshot; resolves every card; initializes
        the reservation row; finalizes the grant. It is the only path to a `Qualified` value."""
        self._not_terminal()
        if self.sts is None:
            raise Refused(AUTHORITY, 'the session needs the sole STS exchange first')
        journal = Journal(table, self.enrollment)
        journal.verify()
        self.journal = journal
        snapshot = self._authority(self.preliminary)
        try:
            body = journal.get(*CURRENT)
        except JournalUnavailable:
            raise Refused(AUTHORITY, 'the journal is unavailable')
        _agree_for_grant(self.facts, snapshot, self._read_mirror(body))
        qualified = self._authorize()
        if any(card.source == 'first_use' for card in qualified.cards):
            for card in qualified.cards:
                if card.source == 'first_use':
                    self._archive(card, snapshot)
            # What the archive now holds is the accepted record, whoever wrote it.
            qualified = self._authorize()
        if qualified.tuple_bytes != self.preliminary.tuple_bytes:
            raise Refused(CONFLICT, 'the reservation is not the admitted reservation')
        self._ensure_row(qualified)
        self._finalize_grant(qualified, snapshot, body)
        self.qualified = qualified

    def attach_journal(self, table):
        """Read-only recovery of an unfinished release over an authenticated journal."""
        self._not_terminal()
        journal = Journal(table, self.enrollment)
        journal.verify()
        self.journal = journal

    def _archive(self, card, snapshot):
        """The first accepted archive: one conditional create and an exact readback, before any grant or
        KMS. A conflict or an uncertain create can only find an accepted record. It never overwrites one."""
        policy = snapshot.value
        accepted_at = datetime.datetime.fromtimestamp(self.clock.now(), datetime.timezone.utc)
        data = _canon({
            'schema': 2, 'request_body': card.request_bytes.decode('utf-8'),
            'receipt_body': card.receipt_bytes.decode('utf-8'),
            'provider_projection_body': card.projection_bytes.decode('utf-8'),
            'acceptance': {'schema': 1, 'receipt_sha256': card.receipt_sha256, 'request_sha256': _sha(card.request_bytes),
                           'provider_projection_sha256': _sha(card.projection_bytes),
                           'signer_workflow_sha': policy['signer_workflow_sha'], 'run_id': self.holder.run_id,
                           'run_attempt': self.holder.run_attempt, 'job_id': self.holder.job_id,
                           'policy_commit': policy['policy_commit'], 'policy_sha256': policy['policy_sha256'],
                           'authority_sha256': self.enrollment.authority_sha256,
                           'table_id': self.enrollment.table_id, 'genesis_sha256': self.enrollment.genesis_sha256,
                           'accepted_at': accepted_at.strftime('%Y-%m-%dT%H:%M:%SZ')}})
        try:
            ev.parse_archive_envelope(data)
        except ControllerError as error:
            raise Refused(LIMIT if 'larger than' in str(error) else MISMATCH, str(error))
        key = ('APPROVAL#' + card.receipt_sha256, 'RECORD')
        fresh = False
        try:
            fresh = self.journal.write([self.journal.put(key, data)])
        except JOURNAL_ERRORS:
            pass
        try:
            stored = self.journal.get(*key)
        except JournalUnavailable:
            stored = None
        if stored is None:
            raise Refused(AUTHORITY, 'the approval archive outcome is unknown')
        if fresh and stored != data:
            raise Refused(AUTHORITY, 'the approval archive readback is not the exact record')

    def _reserve_receipt(self, release):
        """The fresh reserve-job receipt, transferred within this exact run attempt. The job name alone
        grants nothing: the numeric job, its result, its source and the receipt must all agree."""
        unavailable = Refused(UNKNOWN, 'the reserve receipt is unavailable')
        none = Refused(UNKNOWN, 'the reservation has no journal row and this holder did not freshly create it')
        if self.reserve_artifact_id is None:
            raise none
        try:
            artifact = self.facts.artifact(self.reserve_artifact_id, self.holder.run_id)
            data = self.facts.artifact_member(artifact, RESERVE_MEMBER, ev.RECORD_MAX)
        except ControllerError:
            raise unavailable
        if artifact.name != RESERVE_ARTIFACT + str(self.holder.run_attempt) or artifact.head_sha != release.commit:
            raise unavailable
        try:
            receipt = _strict(data, RECEIPT_KEYS, 'the reserve receipt')
            _schema_one(receipt, 'the reserve receipt')
            for key in ('run_id', 'run_attempt', 'job_id'):
                uint64(receipt[key], 'the reserve receipt ' + key)
        except ControllerError as error:
            raise Refused(MISMATCH, _malformed(error, 'the reserve receipt'))
        if (receipt['run_id'], receipt['run_attempt']) != (str(self.holder.run_id), str(self.holder.run_attempt)):
            raise Refused(UNKNOWN, 'the reserve receipt is not from this run attempt')
        if (receipt['repository_id'], receipt['version'], receipt['tuple_sha256'], receipt['reservation_object'],
                receipt['commit']) != (REPOSITORY_ID, release.version, release.tuple_sha256,
                                       release.reservation.tag_object, release.commit):
            raise Refused(CONFLICT, 'the reserve receipt names another reservation')
        if receipt['created'] is not True:
            raise none
        try:
            jobs = self.facts.attempt_jobs(self.holder.run_id, self.holder.run_attempt)
        except ControllerError:
            raise unavailable
        # A matrix job is `<name> (<value>)`. The frozen job is one non-matrix job.
        named = [j for j in jobs if j.name == RESERVE_JOB or j.name.startswith(RESERVE_JOB + ' (')]
        if not (len(named) == 1 and named[0].name == RESERVE_JOB and str(named[0].job_id) == receipt['job_id']
                and (named[0].status, named[0].conclusion) == ('completed', 'success')
                and named[0].head_sha == release.commit):
            raise Refused(UNKNOWN, 'the reserve job is not the one completed reserve job')
        return receipt

    def _ensure_row(self, release):
        """Only this workflow holder, with its fresh receipt, initializes a missing row. An existing
        reservation with no row is uncertain history: nothing rebuilds a clean row."""
        key = _version_key(release.version)
        try:
            body = self.journal.get(*key)
        except JournalUnavailable:
            raise Refused(AUTHORITY, 'the journal is unavailable')
        if body is not None:
            self._parse_row(body, release)
            return
        receipt = self._reserve_receipt(release)
        new = _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'version': release.version,
                      'tuple_sha256': release.tuple_sha256, 'reservation_object': release.reservation.tag_object,
                      'commit': release.commit, 'genesis_sha256': self.enrollment.genesis_sha256, 'phase': 'RESERVED',
                      'revision': '1', 'run_id': receipt['run_id'], 'run_attempt': receipt['run_attempt'],
                      'job_id': receipt['job_id']})
        try:
            self.journal.write([self.journal.put(key, new)])
        except JOURNAL_ERRORS:
            pass
        try:
            stored = self.journal.get(*key)
        except JournalUnavailable:
            stored = None
        if stored != new:
            raise Refused(UNKNOWN, 'the reservation row has no exact acknowledgement')

    def _parse_row(self, body, release):
        try:
            row = _strict(body, ROW_KEYS, 'the reservation row')
        except ControllerError:
            raise Refused(UNKNOWN, 'the reservation row is malformed')
        if row['phase'] not in ('RESERVED', 'ATTEMPTED', 'COMPLETE') or (
                row['repository_id'], row['version'], row['tuple_sha256'], row['reservation_object'], row['commit'],
                row['genesis_sha256']) != (REPOSITORY_ID, release.version, release.tuple_sha256,
                                           release.reservation.tag_object, release.commit,
                                           self.enrollment.genesis_sha256):
            raise Refused(UNKNOWN, 'the reservation row is not this reservation')
        return row

    def _finalize_grant(self, release, snapshot, current_body):
        """The final grant binds the exact authority and the server's actual expiry. It is durable only
        with a fresh acknowledgement and an exact atomic readback."""
        epoch = snapshot.epoch
        fence_key = _fence_key(epoch)
        try:
            records = self.journal.snapshot((CURRENT, fence_key))
        except JournalUnavailable:
            raise Refused(AUTHORITY, 'the journal is unavailable')
        if records[CURRENT] != current_body:
            raise Refused(AUTHORITY, 'the journal mirror disagrees with the admission record')
        if records[fence_key] is None:
            raise Refused(AUTHORITY, 'the fence counter is unavailable')
        try:
            counter = _strict(records[fence_key], FENCE_KEYS, 'the fence counter')
            uint64(counter['fence'], 'the fence')
        except ControllerError:
            raise Refused(AUTHORITY, 'the fence counter is malformed')
        if (counter['repository_id'], counter['epoch']) != (REPOSITORY_ID, epoch):
            raise Refused(AUTHORITY, 'the fence counter is malformed')
        if int(counter['fence']) >= U64_MAX:
            raise Refused(AUTHORITY, 'the fence counter is exhausted')
        fence = str(int(counter['fence']) + 1)
        value = snapshot.value
        grant = {'schema': 1, 'grant_id': self.grant_id, 'repository_id': REPOSITORY_ID, 'version': release.version,
                 'tuple_sha256': release.tuple_sha256, 'epoch': epoch, 'fence': fence,
                 'signer_workflow_sha': value['signer_workflow_sha'], 'key_arn': value['key_arn'],
                 'fingerprint': value['fingerprint'], 'policy_commit': value['policy_commit'],
                 'policy_sha256': value['policy_sha256'], 'trust_sha256': value['trust_sha256'],
                 'admission_commit': snapshot.commit, 'admission_sha256': snapshot.sha256,
                 'run_id': str(self.holder.run_id), 'run_attempt': str(self.holder.run_attempt),
                 'job_id': str(self.holder.job_id), 'jwt_iat': str(self.oidc[0]), 'jwt_exp': str(self.oidc[1]),
                 'sts_expiration': str(self.sts.expiration),
                 'role_max_session_duration': str(self.sts.role_max_session_duration),
                 'deadline': str(self.sts.expiration - CLOCK_SKEW)}
        grant_bytes = _canon(grant)
        issuer = _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'run_id': grant['run_id'],
                         'run_attempt': grant['run_attempt'], 'job_id': grant['job_id'], 'grant_id': self.grant_id,
                         'jwt_iat': grant['jwt_iat'], 'jwt_exp': grant['jwt_exp'],
                         'requested_duration': str(self.sts.requested_duration),
                         'sts_expiration': grant['sts_expiration'],
                         'role_max_session_duration': grant['role_max_session_duration']})
        issuer_key = ('ISSUER#' + grant['run_id'] + '#' + grant['run_attempt'] + '#' + grant['job_id'], 'RECORD')
        new_counter = _canon(dict(counter, fence=fence))
        grant_key = _grant_key(self.grant_id)
        no_ack = Refused(AUTHORITY, 'the grant has no fresh acknowledgement')
        try:
            fresh = self.journal.write([self.journal.exact(CURRENT, current_body),
                                        self.journal.cas(fence_key, records[fence_key], new_counter),
                                        self.journal.put(grant_key, grant_bytes),
                                        self.journal.put(issuer_key, issuer)])
            back = self.journal.snapshot((grant_key, issuer_key, fence_key, CURRENT))
        except JOURNAL_ERRORS:
            raise no_ack
        if not fresh or (back[grant_key], back[issuer_key], back[fence_key], back[CURRENT]) != (
                grant_bytes, issuer, new_counter, current_body):
            raise no_ack
        self.grant, self.grant_bytes = grant, grant_bytes

    # -- the adapter's context -----------------------------------------------------------------

    @_staged('reserve')
    def _enter(self, repository, version, manifest_sha256, key_arn, fingerprint, first_machine_tag):
        self._not_terminal()
        if self.grant_bytes is None or self.qualified is None:
            raise Refused(AUTHORITY, 'the signing session is not open')
        qualified = self.qualified
        for key, value, expected in (('repository', repository, REPOSITORY), ('version', version, qualified.version),
                                     ('manifest_sha256', manifest_sha256, qualified.manifest_sha256),
                                     ('key_arn', key_arn, qualified.key_arn),
                                     ('fingerprint', fingerprint, qualified.fingerprint),
                                     ('first_machine_tag', first_machine_tag, qualified.first_machine_tag)):
            if value != expected:
                name = key.replace('_', ' ')
                raise Refused(MISMATCH, 'the adapter request ' + name + ' is not the qualified ' + name)
        again = self._authorize()
        if again.tuple_bytes != qualified.tuple_bytes:
            raise Refused(CONFLICT, 'the reservation is not the admitted reservation')
        _readback(self.facts, again)
        snapshot = self._authority(again)
        grant_key = _grant_key(self.grant_id)
        try:
            records = self.journal.snapshot((CURRENT, grant_key))
        except JournalUnavailable:
            raise Refused(AUTHORITY, 'the journal is unavailable')
        self._agree_for_holder(snapshot, self._read_mirror(records[CURRENT]), self.grant)
        if records[grant_key] != self.grant_bytes:
            raise Refused(AUTHORITY, 'the stored grant is not this grant')
        self._check_budget()

    @contextlib.contextmanager
    def reserve(self, repository, version, manifest_sha256, key_arn, fingerprint, first_machine_tag):
        """Authenticates the full tuple, the reservation readback and the authority, then holds the
        exclusion through the KMS calls. Leaving it, by any exception or death, releases nothing."""
        self._enter(repository, version, manifest_sha256, key_arn, fingerprint, first_machine_tag)
        self._inside = True
        try:
            yield self
        finally:
            self._inside = False

    @_staged('public key')
    def before_public_key(self):
        """No KMS request, GetPublicKey included, happens outside the context."""
        self._need_context()

    def _prior_history(self, release):
        """Finding no attempt is not proof alone: every prior invocation must have stopped before one."""
        try:
            history = self.history.invocations(release.version, release.tuple_sha256)
        except ControllerError:
            raise Refused(UNKNOWN, 'the prior invocation history is unavailable')
        if history.complete is not True:
            raise Refused(UNKNOWN, 'the prior invocation history is incomplete')
        for entry in history.invocations:
            if (entry.run_id, entry.run_attempt, entry.job_id) == (
                    self.holder.run_id, self.holder.run_attempt, self.holder.job_id):
                continue
            if entry.stopped != 'before_attempt':
                raise Refused(UNKNOWN, 'a prior invocation is not proven to have stopped before its attempt')

    @_staged('attempt')
    def before_sign(self, payload_digest):
        """The durable attempt. Only one fresh create, acknowledged to this invocation with an exact
        atomic readback, returns the one in-memory permit. An identical existing record gives none."""
        self._need_context()
        if self._attempted:
            raise Refused(UNKNOWN, 'this invocation already used its one attempt')
        release = self.qualified
        manifest = _manifest_bytes(release)
        if _sha(manifest) != release.manifest_sha256 or payload_digest != _preimage_digest(manifest):
            raise Refused(MISMATCH, 'the signing preimage is not the one of the qualified manifest')
        self._check_budget()
        snapshot = self._authority(release)
        keys = self._keys(release)
        grant_key = _grant_key(self.grant_id)
        fence_key = _fence_key(self.grant['epoch'])
        try:
            records = self.journal.snapshot(tuple(keys.values()) + (grant_key, fence_key))
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the journal is unavailable')
        self._agree_for_holder(snapshot, self._read_mirror(records[CURRENT]), self.grant)
        if records[grant_key] != self.grant_bytes:
            raise Refused(AUTHORITY, 'the stored grant is not this grant')
        if records[fence_key] != _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'epoch': self.grant['epoch'],
                                         'fence': self.grant['fence']}):
            raise Refused(AUTHORITY, 'the grant fence is stale')
        if records[keys['abandonment']] is not None:
            raise Refused(CONFLICT, 'v' + release.version + ' is abandoned')
        if records[keys['row']] is None:
            raise Refused(UNKNOWN, 'the reservation has no journal row')
        row = self._parse_row(records[keys['row']], release)
        if row['phase'] != 'RESERVED':
            raise Refused(UNKNOWN, 'the reservation is already attempted')
        if any(records[keys[name]] is not None for name in ('attempt', 'signature', 'completion')):
            raise Refused(UNKNOWN, 'the reservation has output and no attempt')
        self._prior_history(release)
        if _final_tag_present(self.facts, release.version):
            raise Refused(CONFLICT, 'the tag v' + release.version + ' exists without its release')
        attempt = _canon({'schema': 1, 'state': 'attempt_started', 'repository_id': REPOSITORY_ID,
                          'version': release.version, 'tuple_sha256': release.tuple_sha256,
                          'reservation_object': release.reservation.tag_object, 'commit': release.commit,
                          'release_evidence_sha256': release.evidence_sha256,
                          'manifest_sha256': release.manifest_sha256, 'key_arn': release.key_arn,
                          'fingerprint': release.fingerprint, 'policy_commit': release.policy_commit,
                          'signer_workflow_sha': release.signer_workflow_sha, 'run_id': self.grant['run_id'],
                          'run_attempt': self.grant['run_attempt'], 'job_id': self.grant['job_id'],
                          'grant_id': self.grant_id, 'fence': self.grant['fence']})
        new_row = _canon(dict(row, phase='ATTEMPTED', revision=str(int(row['revision']) + 1)))
        self._attempted = True
        try:
            fresh = self.journal.write([self.journal.exact(CURRENT, records[CURRENT]),
                                        self.journal.exact(grant_key, self.grant_bytes),
                                        self.journal.cas(keys['row'], records[keys['row']], new_row),
                                        self.journal.put(keys['attempt'], attempt),
                                        self.journal.absent(keys['abandonment'])])
        except JournalConditionFailed:
            raise Refused(UNKNOWN, 'the attempt was not freshly created')
        except JournalOutcomeUnknown:
            raise Refused(UNKNOWN, 'the attempt write outcome is unknown')
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the journal is unavailable')
        if not fresh:
            raise Refused(UNKNOWN, 'the attempt write was not one fresh request')
        try:
            back = self.journal.snapshot((keys['attempt'], keys['row']))
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the attempt readback is unavailable')
        if (back[keys['attempt']], back[keys['row']]) != (attempt, new_row):
            raise Refused(UNKNOWN, 'the attempt readback is not the exact record')
        self._check_budget()
        self._attempt_bytes, self._row_bytes = attempt, new_row
        return Permit(self)

    def _dispatching(self):
        """The right is consumed even if the budget refuses here: nothing is sent, and nothing retries."""
        if not self._inside:
            raise Refused(AUTHORITY, 'a KMS request needs the reservation context')
        self._check_budget()
        self._sign_deadline = self.clock.now() + SIGN_BUDGET
        self._dispatched = True
        return KMS_MAX

    @_staged('retain')
    def retain_signature(self, signature):
        """Persists the actual bytes and the completion receipt in one transaction and reads all of it
        back. A bad signature is never retained. An unknown outcome never sends another Sign."""
        self._not_terminal()
        if not self._dispatched or self._retained is not None:
            raise Refused(UNKNOWN, 'this invocation has no dispatch to retain')
        if not isinstance(signature, bytes) or len(signature) > SIGNATURE_MAX:
            raise Refused(LIMIT, 'the signature is larger than ' + str(SIGNATURE_MAX) + ' bytes')
        now = self.clock.now()
        if now >= self._sign_deadline:
            raise Refused(UNKNOWN, 'the signing budget has passed')
        release = self.qualified
        if not self._verified(signature, release):
            raise Refused(UNKNOWN, 'the returned signature does not verify')
        keys = self._keys(release)
        attempt_sha256 = _sha(self._attempt_bytes)
        completion = _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'version': release.version,
                             'tuple_sha256': release.tuple_sha256, 'attempt_sha256': attempt_sha256,
                             'signature_sha256': _sha(signature), 'manifest_sha256': release.manifest_sha256,
                             'key_arn': release.key_arn, 'fingerprint': release.fingerprint,
                             'verification_contract': VERIFICATION_CONTRACT, 'run_id': self.grant['run_id'],
                             'run_attempt': self.grant['run_attempt'], 'job_id': self.grant['job_id'],
                             'grant_id': self.grant_id, 'fence': self.grant['fence']})
        row = _strict(self._row_bytes, ROW_KEYS, 'the reservation row')
        new_row = _canon(dict(row, phase='COMPLETE', revision=str(int(row['revision']) + 1)))
        grant_key = _grant_key(self.grant_id)
        unknown = Refused(UNKNOWN, 'the retention write outcome is unknown')
        remaining = max(1, self._sign_deadline - now)
        try:
            fresh = self.journal.write([self.journal.put(keys['signature'], signature),
                                        self.journal.put(keys['completion'], completion),
                                        self.journal.exact(keys['attempt'], self._attempt_bytes),
                                        self.journal.exact(grant_key, self.grant_bytes),
                                        self.journal.cas(keys['row'], self._row_bytes, new_row)], remaining)
            back = self.journal.snapshot((keys['signature'], keys['completion'], keys['attempt'], keys['row']), remaining)
        except JOURNAL_ERRORS:
            raise unknown
        if not fresh or (back[keys['signature']], back[keys['completion']], back[keys['attempt']],
                         back[keys['row']]) != (signature, completion, self._attempt_bytes, new_row):
            raise unknown
        self._retained = RetainedSignature(signature, release.version, release.tuple_sha256, attempt_sha256)
        return signature

    def _verified(self, signature, release):
        return self.verifier.verify(signature, _manifest_bytes(release), self.signer_line, PRINCIPAL, NAMESPACE) is True

    @_staged('authorization')
    def authorization(self):
        """The signer's proof for the promotion job, which has no journal access. A pending card cannot
        reach promotion: this exists only after every card resolved and real bytes were retained."""
        self._not_terminal()
        if self._retained is None:
            raise Refused(AUTHORITY, 'nothing was retained in this invocation')
        retained = self._retained
        release = self.qualified
        if any(card.source != 'archive' for card in release.cards):
            raise Refused(AUTHORITY, 'a card has no accepted archive')
        try:
            value = _strict(self.journal.get(*self._keys(release)['attempt']) or b'', ATTEMPT_KEYS, 'the journal attempt')
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the journal is unavailable')
        except ControllerError:
            raise Refused(UNKNOWN, 'the journal is partial')
        return _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'version': release.version,
                       'tuple_sha256': release.tuple_sha256, 'grant_id': value['grant_id'], 'fence': value['fence'],
                       'attempt_sha256': retained.attempt_sha256, 'signature_sha256': _sha(retained.signature),
                       'cards': [{'kind': c.kind, 'receipt_sha256': c.receipt_sha256,
                                  'envelope_sha256': _sha(c.envelope_bytes)} for c in release.cards]})

    # -- recovery ------------------------------------------------------------------------------

    @_staged('recover')
    def recover(self, tuple_bytes):
        """Only verified retained bytes, a terminal published observation or a refusal. Never a second
        Sign. A published release needs no credentials and no journal; everything unfinished needs
        current authority."""
        reconciliation = _eligibility(elig.authenticate_published, self.request, self.facts)
        if reconciliation.tuple_bytes != tuple_bytes:
            raise Refused(CONFLICT, 'the requested tuple is not the reserved tuple')
        if reconciliation.publication.state == 'published':
            self.terminal = self._observe(reconciliation)
            return self.terminal
        if self.journal is None:
            raise Refused(AUTHORITY, 'recovery of an unfinished release needs the journal session')
        release = self._authorize()
        self.qualified = release
        snapshot = self._authority(release)
        keys = self._keys(release)
        try:
            records = self.journal.snapshot(tuple(keys.values()))
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the journal is unavailable')
        self._agree_for_holder(snapshot, self._read_mirror(records[CURRENT]), None)
        if records[keys['abandonment']] is not None:
            raise Refused(CONFLICT, 'v' + release.version + ' is abandoned')
        self._retained = self._retained_bytes(release, keys, records)
        return self._retained

    def _retained_bytes(self, release, keys, records):
        partial = Refused(UNKNOWN, 'the journal is partial')
        if records[keys['row']] is None:
            raise partial
        row = self._parse_row(records[keys['row']], release)
        attempt, signature, completion = (records[keys[name]] for name in ('attempt', 'signature', 'completion'))
        if attempt is None:
            if signature is not None or completion is not None or row['phase'] != 'RESERVED':
                raise partial
            raise Refused(UNKNOWN, 'the reservation has no attempt and no retained signature')
        if signature is None and completion is None:
            if row['phase'] != 'ATTEMPTED':
                raise partial
            raise Refused(UNKNOWN, 'the attempt has no retained signature')
        if signature is None or completion is None or row['phase'] != 'COMPLETE':
            raise partial
        try:
            attempt_value = _strict(attempt, ATTEMPT_KEYS, 'the journal attempt')
            completion_value = _strict(completion, COMPLETION_KEYS, 'the completion receipt')
        except ControllerError:
            raise partial
        if (attempt_value['state'], attempt_value['repository_id'], attempt_value['version'],
                attempt_value['tuple_sha256'], attempt_value['reservation_object'], attempt_value['commit'],
                attempt_value['release_evidence_sha256'], attempt_value['manifest_sha256'], attempt_value['key_arn'],
                attempt_value['fingerprint'], attempt_value['policy_commit'], attempt_value['signer_workflow_sha']) != (
                    'attempt_started', REPOSITORY_ID, release.version, release.tuple_sha256,
                    release.reservation.tag_object, release.commit, release.evidence_sha256, release.manifest_sha256,
                    release.key_arn, release.fingerprint, release.policy_commit, release.signer_workflow_sha):
            raise Refused(UNKNOWN, 'the journal attempt is not this tuple')
        if (completion_value['repository_id'], completion_value['version'], completion_value['tuple_sha256'],
                completion_value['manifest_sha256'], completion_value['key_arn'], completion_value['fingerprint'],
                completion_value['verification_contract']) != (
                    REPOSITORY_ID, release.version, release.tuple_sha256, release.manifest_sha256, release.key_arn,
                    release.fingerprint, VERIFICATION_CONTRACT):
            raise Refused(UNKNOWN, 'the completion receipt is not this tuple')
        if completion_value['attempt_sha256'] != _sha(attempt) or (
                completion_value['grant_id'], completion_value['fence']) != (attempt_value['grant_id'],
                                                                             attempt_value['fence']):
            raise Refused(UNKNOWN, 'the completion receipt does not bind its attempt')
        if completion_value['signature_sha256'] != _sha(signature):
            raise Refused(UNKNOWN, 'the retained signature is not the completion receipt signature')
        self._check_attempt_grant(attempt_value, release)
        if not self._verified(signature, release):
            raise Refused(UNKNOWN, 'the retained signature does not verify')
        return RetainedSignature(signature, release.version, release.tuple_sha256, _sha(attempt))

    def _check_attempt_grant(self, attempt, release):
        """The attempt's epoch comes from its immutable grant: exact fence, tuple and identity."""
        try:
            body = self.journal.get(*_grant_key(attempt['grant_id']))
        except JournalUnavailable:
            raise Refused(UNKNOWN, 'the journal is unavailable')
        if body is None:
            raise Refused(UNKNOWN, 'the journal is partial')
        try:
            grant = _strict(body, GRANT_KEYS, 'the attempt grant')
        except ControllerError:
            raise Refused(UNKNOWN, 'the journal is partial')
        if (grant['fence'], grant['tuple_sha256'], grant['run_id'], grant['run_attempt'], grant['job_id']) != (
                attempt['fence'], release.tuple_sha256, attempt['run_id'], attempt['run_attempt'], attempt['job_id']):
            raise Refused(UNKNOWN, 'the journal attempt is not this tuple')

    def _observe(self, reconciliation):
        """The exact published bytes against the original tuple, through the exact-byte verifier. Metadata
        alone never gives this result, and missing proof is an incomplete reconciliation."""
        if self.published_verifier is None:
            raise Refused(INCOMPLETE, 'the published release has no exact-byte verification')
        try:
            proof = self.published_verifier.verify_published(reconciliation)
        except Refused:
            raise
        except ControllerError as error:
            raise Refused(INCOMPLETE, str(error))
        if not isinstance(proof, PublishedBytes):
            raise Refused(INCOMPLETE, 'the exact-byte verification gave no proof')
        version = reconciliation.version
        if proof.release_id != reconciliation.publication.release_id or proof.tag_name != 'v' + version \
                or proof.commit != reconciliation.commit or proof.draft is not False:
            raise Refused(CONFLICT, 'the exact-byte proof is not of this release')
        if proof.manifest_sha256 != reconciliation.manifest_sha256:
            raise Refused(CONFLICT, 'the published manifest differs from the reserved tuple')
        if tuple(proof.archives) != reconciliation.artifacts:
            raise Refused(CONFLICT, 'the published archives differ from the reserved tuple')
        if proof.notes_sha256 != ev.parse_release_record(reconciliation.evidence_bytes).value['source']['notes_sha256']:
            raise Refused(CONFLICT, 'the published notes differ from the release record')
        if proof.assets_complete is not True:
            raise Refused(CONFLICT, 'the published asset set is not the expected set')
        if (proof.fingerprint, proof.principal, proof.namespace) != (reconciliation.fingerprint, PRINCIPAL, NAMESPACE):
            raise Refused(CONFLICT, 'the published signature is not from the reserved signer')
        if proof.signature_verified is not True:
            raise Refused(CONFLICT, 'the published signature does not verify')
        return AlreadyPublishedMatching(
            version=version, commit=reconciliation.commit, tuple_sha256=reconciliation.tuple_sha256,
            release_id=proof.release_id, manifest_sha256=proof.manifest_sha256,
            signature_sha256=proof.signature_sha256, channel_enabled=reconciliation.channel_enabled,
            revoked_receipts=tuple(c.receipt_sha256 for c in reconciliation.cards if c.revoked),
            signer_revoked=self._signer_revoked(reconciliation))

    def _signer_revoked(self, release):
        """The current status, reported apart. None means the read failed: it changes no result."""
        try:
            self.facts.release_trust(release.version, release.policy_commit, True)
        except ControllerError as error:
            return True if 'is revoked for new signing' in str(error) else None
        return False

    def journal_phase(self):
        """The reservation row's phase, for receipts."""
        release = self.qualified or self.preliminary
        body = self.journal.get(*_version_key(release.version))
        return None if body is None else _strict(body, ROW_KEYS, 'the reservation row')['phase']


def canonical_refusals(controller):
    """The invocation's refusal receipts as canonical JSON bytes."""
    return _canon(controller.refusals())


# ---------------------------------------------------------------------------------------------
# Promotion
# ---------------------------------------------------------------------------------------------

def check_promotion(facts, enrollment, request, run_id, run_attempt, authorization):
    """Before promotion: the current caller, reservation, E, T, channel, revocations and admission, read
    again from GitHub, plus the signer's authorization of this tuple and these exact cards. The promotion
    job has no AWS identity. It cannot read the archive, and a pending card never reaches it. It writes
    nothing. Returns the admission snapshot."""
    if authorization is None:
        raise Refused(AUTHORITY, 'the promotion needs the signer authorization')
    if (request.dispatch_run_id, request.dispatch_run_attempt) != (run_id, run_attempt):
        raise Refused(AUTHORITY, 'the request is not this run attempt')
    preliminary = _eligibility(elig.preliminary_recovery, request, facts)

    def named(holder):
        return (holder['run_id'], holder['run_attempt']) == (str(run_id), str(run_attempt))
    snapshot, _ = check_authority(facts, enrollment, preliminary, named)
    try:
        value = _strict(authorization, AUTHORIZATION_KEYS, 'the signer authorization')
        _schema_one(value, 'the signer authorization')
        if not isinstance(value['cards'], list):
            raise ControllerError('the signer authorization cards are not a list')
        cards = {}
        for card in value['cards']:
            cand.closed(card, AUTHORIZATION_CARD_KEYS, 'an authorized card')
            cards[card['kind'], card['receipt_sha256']] = card['envelope_sha256']
    except ControllerError as error:
        raise Refused(MISMATCH, _malformed(error, 'the signer authorization'))
    if (value['repository_id'], value['version'], value['tuple_sha256']) != (
            REPOSITORY_ID, preliminary.version, preliminary.tuple_sha256):
        raise Refused(MISMATCH, 'the signer authorization is not this tuple')
    if set(cards) != {(c.kind, c.receipt_sha256) for c in preliminary.cards}:
        raise Refused(MISMATCH, 'the signer authorization does not cover these cards')
    return snapshot


# ---------------------------------------------------------------------------------------------
# Planned maintenance and emergency containment
# ---------------------------------------------------------------------------------------------

def _stay_closed(reason):
    return Refused(AUTHORITY, reason + '; admission stays closed')


def drain_boundary(census):
    """`max(all observed STS Expiration, latest issued OIDC exp + verified role MaxSessionDuration)`, or
    None when the census proves that nothing was issued. Any uncertainty keeps admission closed. A live
    OIDC token can be exchanged near its expiry, and a lowered role maximum does not shorten issued
    credentials, so the largest observed maximum counts. Cancellation is not termination."""
    if census.complete is not True:
        raise _stay_closed('the census is incomplete')
    if not cand.is_int(census.role_max_session_duration, 1):
        raise _stay_closed('the role maximum is not verified')
    receipts = {(r.run_id, r.run_attempt, r.job_id): r for r in census.issuers}
    jobs = set()
    for job in census.jobs:
        key = (job.run_id, job.run_attempt, job.job_id)
        jobs.add(key)
        if job.status != 'completed':
            raise _stay_closed('a credential-issuing job is not terminal')
        if job.issuance not in ('none', 'issued'):
            raise _stay_closed('an issuance request is unresolved')
        if job.issuance == 'issued' and key not in receipts:
            raise _stay_closed('an issued credential has no receipt')
    if set(receipts) - jobs:
        raise _stay_closed('an issuance receipt has no job in the census')
    if not receipts:
        return None
    for receipt in receipts.values():
        if not (cand.is_int(receipt.jwt_exp, 1) and cand.is_int(receipt.sts_expiration, 1)):
            raise _stay_closed('an issuance receipt has no server expiry')
    maximum = max([census.role_max_session_duration]
                  + [r.role_max_session_duration for r in receipts.values() if r.role_max_session_duration])
    return max(max(r.sts_expiration for r in receipts.values()), max(r.jwt_exp for r in receipts.values()) + maximum)


def _closed_document(snapshot, mirror, state, holder):
    """The same-epoch closing document. A mirror that a partial reopen moved ahead keeps its higher epoch:
    that epoch is never reused, and nothing moves an epoch backwards."""
    base = snapshot.value if snapshot is not None else mirror.admission
    if mirror is not None and int(mirror.admission['epoch']) > int(base['epoch']):
        base = mirror.admission
    return _check_admission(dict(base, state=state, draining_holder=holder))


def _read_current(journal):
    """The mirror, or None if the journal cannot give it."""
    try:
        body = journal.get(*CURRENT)
    except JournalUnavailable:
        return None
    if body is None:
        return None
    try:
        return parse_current(body)
    except ControllerError:
        return None


def _mirror_write(facts, journal, mirror, admission, floor, transition, extra=()):
    """Compare-and-swap of CURRENT, with its immutable EVENT, and an exact readback. The floor and the
    revision never regress. Raises ControllerError or a journal error if the document is not installed."""
    if int(mirror.revision) >= U64_MAX:
        raise ControllerError('the mirror revision is exhausted')
    if _status(facts, mirror.floor, floor) not in ('ahead', 'identical'):
        raise ControllerError('the journal floor would regress')
    revision = str(int(mirror.revision) + 1)
    digest = _sha(_canon(admission))
    new = _canon({'schema': 1, 'revision': revision, 'admission': admission, 'admission_sha256': digest,
                  'admission_commit_floor': floor})
    event = _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'epoch': admission['epoch'], 'revision': revision,
                    'transition': transition, 'admission_sha256': digest, 'admission_commit_floor': floor})
    actions = [journal.cas(CURRENT, mirror.body, new),
               journal.put(('CONTROL#' + REPOSITORY_ID, 'EVENT#' + admission['epoch'] + '#' + revision), event)]
    actions.extend(journal.put(key, body) for key, body in extra)
    try:
        journal.write(actions)
    except JournalOutcomeUnknown:
        pass
    if journal.get(*CURRENT) != new:
        raise ControllerError('the journal mirror readback is not the exact document')


class Maintenance(object):
    """Planned maintenance, in the one fixed lane the release path uses. Close admission first, then wait
    for the lane; the current holder finishes. Reopen only at a new epoch, with the mirror prepared first
    and GitHub last. A canceled or partial run leaves admission closed until a whole new run recovers it.
    It adds no broker and claims no AWS fence. Administrators remain a trusted boundary outside the lane."""

    def __init__(self, facts, writer, journal, enrollment, clock):
        self.facts = facts
        self.writer = writer
        self.journal = journal
        self.enrollment = enrollment
        self.clock = clock
        self.closed = False
        self.lane = False
        self.drained = False
        self.not_before = None
        self.update = None
        self.verified = False
        self.canceled = False

    def _close_to(self, state, holder):
        """GitHub first, then the mirror holds that exact document with its now-known commit as floor."""
        snapshot = read_admission(self.facts)
        mirror = _read_current(self.journal)
        target = _closed_document(snapshot, mirror, state, holder)
        data = _canon(target)
        incomplete = Refused(AUTHORITY, 'the close is incomplete')
        commit = snapshot.commit
        if snapshot.data != data:
            try:
                commit = self.writer.commit(data)
            except ControllerError:
                raise incomplete
        if mirror is None:
            raise incomplete
        if _canon(mirror.admission) != data:
            try:
                _mirror_write(self.facts, self.journal, mirror, target, commit, 'close')
            except JOURNAL_ERRORS + (ControllerError,):
                raise incomplete

    def close(self, holder=None):
        """DRAINING names the one existing holder; otherwise CLOSED. Same-epoch: no H, key or policy
        change, and the holder's grant stays valid."""
        self._close_to('DRAINING' if holder else 'CLOSED', holder)
        self.closed = True

    def acquire_lane(self):
        """The lane itself is the workflow's concurrency group. Admission closes before it waits."""
        if not self.closed:
            raise Refused(AUTHORITY, 'close admission before waiting for the lane')
        self.lane = True

    def prove_drained(self, census):
        if not self.closed:
            raise Refused(AUTHORITY, 'close admission before waiting for the lane')
        if not self.lane:
            raise Refused(AUTHORITY, 'the drain proof needs the lane')
        self.not_before = drain_boundary(census)
        self._close_to('CLOSED', None)
        self.drained = True

    def apply_update(self, approved):
        """After the boundary plus the documented skew, by the injected clock. Never a sleep."""
        if not self.drained:
            raise Refused(AUTHORITY, 'the drain boundary has not passed')
        if self.not_before is not None and self.clock.now() <= self.not_before + CLOCK_SKEW:
            raise Refused(AUTHORITY, 'the drain boundary has not passed')
        self.update = approved
        self.verified = False

    def verify_readback(self, approved):
        """The approved update against protected main: the policy bytes, key, signer and trust."""
        mismatch = Refused(AUTHORITY, 'the update readback is not the approved update')
        if self.update is None or approved != self.update:
            raise mismatch
        try:
            data = self.facts.contents(POLICY_PATH, approved.policy_commit, ev.RECORD_MAX).data
            policy = ev.parse_policy(data).value
        except ControllerError:
            raise mismatch
        if (_sha(data), policy['signer_workflow_sha'], policy['key_arn'], policy['fingerprint'],
                policy['records_sha256']) != (approved.policy_sha256, approved.signer_workflow_sha, approved.key_arn,
                                              approved.fingerprint, approved.trust_sha256):
            raise mismatch
        self.verified = True

    def cancel(self):
        """Cancellation changes nothing remote: admission stays where the close left it."""
        self.canceled = True

    def reopen(self):
        """Authenticate CLOSED at F; build A at a higher epoch; install and read back CURRENT with A, its
        digest and floor F; publish A to GitHub last; read both sides again."""
        if self.canceled:
            raise Refused(AUTHORITY, 'a canceled maintenance run never reopens admission')
        if not (self.lane and self.drained and self.verified):
            raise Refused(AUTHORITY, 'reopening needs the lane, the drain proof and the verified update')
        try:
            self._reopen()
        except JOURNAL_ERRORS + (ControllerError,):
            raise _stay_closed('the reopen is incomplete')

    def _reopen(self):
        snapshot = read_admission(self.facts)
        mirror = _read_current(self.journal)
        if mirror is None or snapshot.state != 'CLOSED' or _canon(mirror.admission) != snapshot.data:
            raise ControllerError('the closed snapshots differ')
        floor = snapshot.commit
        if int(snapshot.epoch) >= U64_MAX:
            raise ControllerError('the epoch is exhausted')
        epoch = str(int(snapshot.epoch) + 1)
        update = self.update
        target = _check_admission({
            'schema': 1, 'repository_id': REPOSITORY_ID, 'epoch': epoch, 'state': 'ACTIVE',
            'signer_workflow_sha': update.signer_workflow_sha, 'key_arn': update.key_arn,
            'fingerprint': update.fingerprint, 'policy_commit': update.policy_commit,
            'policy_sha256': update.policy_sha256, 'trust_sha256': update.trust_sha256,
            'table_id': self.enrollment.table_id, 'genesis_sha256': self.enrollment.genesis_sha256,
            'draining_holder': None})
        data = _canon(target)
        counter = _canon({'schema': 1, 'repository_id': REPOSITORY_ID, 'epoch': epoch, 'fence': '1'})
        _mirror_write(self.facts, self.journal, mirror, target, floor, 'reopen', ((_fence_key(epoch), counter),))
        commit = self.writer.commit(data)
        after = read_admission(self.facts)
        back = _read_current(self.journal)
        if after.data != data or back is None or _canon(back.admission) != data or back.floor != floor:
            raise ControllerError('the reopened snapshots differ')
        if _status(self.facts, floor, commit) != 'ahead':
            raise ControllerError('the reopened commit does not descend from its floor')


def contain(facts, writer, journal, enrollment, targets):
    """Emergency containment starts outside the queue: it needs no lane, drain or census. Closes both
    sources as far as each can be reached and reports every in-flight tuple. A dead runner or a free
    lane never proves that Sign did nothing, so only retained output is `retained`. Everything else is
    `unknown`: keep the reservation, keep the output unpublished and apply D-KM1GB, with no second Sign."""
    mirror = _read_current(journal)
    github_closed = False
    commit = None
    target = None
    try:
        snapshot = read_admission(facts)
        target = _closed_document(snapshot, mirror, 'CLOSED', None)
        commit = snapshot.commit
        if snapshot.data != _canon(target):
            commit = writer.commit(_canon(target))
        github_closed = True
    except ControllerError:
        pass
    mirror_closed = False
    if mirror is not None:
        try:
            admission = target if github_closed else _closed_document(None, mirror, 'CLOSED', None)
            if _canon(mirror.admission) != _canon(admission):
                _mirror_write(facts, journal, mirror, admission, commit if github_closed else mirror.floor, 'contain')
            mirror_closed = True
        except JOURNAL_ERRORS + (ControllerError,):
            pass
    results = []
    for version, tuple_sha256 in targets:
        state = 'unknown'
        try:
            keys = (_tuple_key(version, tuple_sha256, 'SIGNATURE'), _tuple_key(version, tuple_sha256, 'COMPLETION'))
            records = journal.snapshot(keys)
            if records[keys[0]] is not None and records[keys[1]] is not None:
                state = 'retained'
        except JournalUnavailable:
            pass
        results.append((version, tuple_sha256, state))
    return ContainmentReceipt(github_closed, mirror_closed, tuple(results))
