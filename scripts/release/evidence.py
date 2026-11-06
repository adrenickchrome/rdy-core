#!/usr/bin/env python3
"""Frozen, closed schemas for the release evidence records.

Each parser takes the bounded original bytes of one record, checks the size before it parses,
hashes those exact bytes, requires the canonical form (sorted keys, compact separators, UTF-8,
one final LF), and checks every field's type and shape. Unknown and duplicate keys refuse.
Integers are JSON integers, never booleans or floats; hex is lower case.

Records: E (the sealed release record), T (the reservation tuple), `release-keys/policy.json`,
`release-keys/evidence-policy.json` with its approval channel, `release/source.json`, and the
founder approval channel's request, receipt, provider-evidence projection and archive envelope.

Parsing grants no authority. A parsed record is well-formed bytes with a digest; whether it is
authentic, current and in scope is checked against GitHub-owned facts and current policy by the
eligibility step. The attempt and journal records belong to the controller.
"""
from dataclasses import dataclass
import datetime
import hashlib
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import rehearse  # noqa: E402

ControllerError = cand.ControllerError
REPOSITORY = cand.REPOSITORY
REPOSITORY_ID = 1403751388
TARGETS = cand.TARGETS
CHECK_NAMES = cand.CHECK_NAMES
DECISION = 'D-PZKAP'

KIB, MIB = cand.KIB, cand.MIB
RELEASE_RECORD_MAX = 1 * MIB      # E
RECORD_MAX = 64 * KIB             # a policy, approval receipt, tuple or archive envelope
REQUEST_MAX = 60000               # an approval request (it travels as one workflow_dispatch input)
ARCHIVE_SIZE_MAX = 512 * MIB
ASSETS_MAX = 32
COORDINATORS_MAX = 16
RECEIPTS_MAX = 16
TUTORIAL_STEPS_MAX = rehearse.TUTORIAL_STEPS_MAX
SOURCE_RESPONSE_MAX = 4 * MIB
TEXT_MAX = 256
URL_MAX = 512
NAME_MAX = 128

SECURITY_JOBS = ('CodeQL (actions)', 'CodeQL (rust)', 'Core CI', 'Core security',
                 'Dependency, secret and workflow policy', 'Fast gate (Linux x86-64)')
CARD_KINDS = ('release_1', 'key_change', 'revocation', 'new_platform')
SOURCE_KINDS = ('artifact', 'implementation_source', 'job', 'protected_policy', 'repository', 'request_archive',
                'review_history', 'run_attempt', 'workflow_source')

VERSION_RE = re.compile(r'0\.[1-9][0-9]{0,8}\.0')
TAG_RE = re.compile(r'v0\.[1-9][0-9]{0,8}\.0')
COMMIT_RE = cand.COMMIT_RE
SHA256_RE = cand.SHA256_RE
ARTIFACT_DIGEST_RE = re.compile(r'sha256:[0-9a-f]{64}')
FILE_NAME_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}')
TARGET_RE = re.compile(r'[a-z0-9_]+(?:-[a-z0-9_]+){1,4}')
KEY_ARN_RE = re.compile(r'arn:aws:kms:[a-z]{2}(?:-[a-z]+)+-[0-9]:[0-9]{12}:key/[0-9a-f-]{36}')
ROLE_ARN_RE = re.compile(r'arn:aws:iam::[0-9]{12}:role/[A-Za-z0-9+=,.@_/-]{1,200}')
FINGERPRINT_RE = re.compile(r'SHA256:[A-Za-z0-9+/]{43}')
WORKFLOW_PATH_RE = re.compile(r'\.github/workflows/[A-Za-z0-9_-]+\.ya?ml')
ACCOUNT_RE = re.compile(r'[0-9]{12}')
REGION_RE = re.compile(r'[a-z]{2}(?:-[a-z]+)+-[0-9]')
TABLE_RE = re.compile(r'[A-Za-z0-9_.-]{3,255}')
CARD_REFERENCE_RE = re.compile(r'dynamodb://([0-9]{12})/([a-z0-9-]{1,32})/([A-Za-z0-9_.-]{3,255})/'
                               r'APPROVAL%23([0-9a-f]{64})/RECORD')
UUID_RE = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
UTC_RE = re.compile(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z')
ACCEPTED_AT_RE = re.compile(r'([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\.[0-9]{1,6})?Z')
COMMENT_RE = re.compile(r'APPROVE sha256:([0-9a-f]{64})')
JOB_URL_PREFIX = 'https://github.com/' + REPOSITORY + '/actions/runs/'


@dataclass(frozen=True)
class Record(object):
    """One parsed record: its kind, the SHA-256 and size of its original bytes, and its value."""
    kind: str
    sha256: str
    size: int
    value: object


@dataclass(frozen=True)
class Envelope(object):
    """A parsed archive envelope and the three records its bodies hold."""
    kind: str
    sha256: str
    size: int
    value: object
    request: Record
    receipt: Record
    projection: Record
    acceptance: object


# ---------------------------------------------------------------------------------------------
# Bytes and field checks
# ---------------------------------------------------------------------------------------------

def read(data, kind, limit):
    """Checks the size, hashes the original bytes, parses them and requires the canonical form."""
    if not isinstance(data, bytes):
        raise ControllerError('the ' + kind + ' is not bytes')
    if len(data) > limit:
        raise ControllerError('the ' + kind + ' is larger than ' + str(limit) + ' bytes')
    digest = hashlib.sha256(data).hexdigest()
    try:
        value = cand.loads(data, limit)
    except RecursionError:
        raise ControllerError('evidence is not JSON')
    try:
        canonical = cand.canonical(value)
    except (UnicodeEncodeError, RecursionError):
        raise ControllerError('the ' + kind + ' is not canonical JSON')
    if canonical != data:
        raise ControllerError('the ' + kind + ' is not canonical JSON')
    return digest, value


def closed(value, keys, where):
    return cand.closed(value, keys, where)


def pos_int(value, what):
    if not cand.is_int(value, 1):
        raise ControllerError(what + ' is not a positive integer')
    return value


def zero(value, what):
    if not (type(value) is int and value == 0):
        raise ControllerError(what + ' is not 0')
    return value


def const(value, expected, what):
    if type(value) is not type(expected) or value != expected:
        raise ControllerError(what + ' is not ' + str(expected))
    return value


def boolean(value, what):
    if type(value) is not bool:
        raise ControllerError(what + ' is not a boolean')
    return value


def false(value, what):
    if value is not False:
        raise ControllerError(what + ' is not false')
    return value


def pattern(value, regex, what, shape):
    if not (isinstance(value, str) and regex.fullmatch(value)):
        raise ControllerError(what + ' is not ' + shape)
    return value


def commit(value, what):
    return pattern(value, COMMIT_RE, what, 'a 40-hex Git SHA')


def sha256(value, what):
    return pattern(value, SHA256_RE, what, 'a 64-hex SHA-256')


def version(value, what='the version'):
    return pattern(value, VERSION_RE, what, 'a 0.N.0 preview')


def tag(value, what):
    return pattern(value, TAG_RE, what, 'a v0.N.0 tag')


def text(value, what, limit=TEXT_MAX):
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > limit:
        raise ControllerError(what + ' is not a string of 1 to ' + str(limit) + ' bytes')
    return value


def key_arn(value, what):
    return pattern(value, KEY_ARN_RE, what, 'a KMS key ARN')


def fingerprint(value, what):
    return pattern(value, FINGERPRINT_RE, what, 'SHA256:<43 base64>')


def workflow_path(value, what):
    return pattern(value, WORKFLOW_PATH_RE, what, 'a workflow path')


def utc(value, what):
    pattern(value, UTC_RE, what, 'UTC YYYY-MM-DDTHH:MM:SSZ')
    try:
        datetime.datetime.strptime(value, '%Y-%m-%dT%H:%M:%SZ')
    except ValueError:
        raise ControllerError(what + ' is not UTC YYYY-MM-DDTHH:MM:SSZ')
    return value


def is_kind(value, kinds):
    return isinstance(value, str) and value in kinds


def file_entry(value, where):
    """T's `{name, sha256, size}` with an allowlisted file name and the archive size bound."""
    cand.file_entry(value, where)
    if not (FILE_NAME_RE.fullmatch(value['name']) and '..' not in value['name']
            and value['size'] <= ARCHIVE_SIZE_MAX):
        raise ControllerError(where + ' is malformed')
    return value


def files(value, what, where):
    """A name-sorted, unique list of 1 to 32 file entries."""
    if not (isinstance(value, list) and 0 < len(value) <= ASSETS_MAX):
        raise ControllerError(what + ' are not 1 to ' + str(ASSETS_MAX) + ' entries')
    names = [file_entry(entry, where)['name'] for entry in value]
    if names != sorted(set(names)):
        raise ControllerError(what + ' are not sorted by name and unique')
    return value


def targets(value, what):
    """A sorted, unique list of 1 to 32 target triples."""
    if not (isinstance(value, list) and 0 < len(value) <= ASSETS_MAX):
        raise ControllerError(what + ' are not 1 to ' + str(ASSETS_MAX) + ' targets')
    for entry in value:
        if not (isinstance(entry, str) and len(entry) <= NAME_MAX and TARGET_RE.fullmatch(entry)):
            raise ControllerError(what + ' hold a malformed target')
    if value != sorted(set(value)):
        raise ControllerError(what + ' are not sorted and unique')
    return value


def manifest_matches(manifest_sha256, entries, what):
    if hashlib.sha256(cand.manifest_bytes(entries)).hexdigest() != manifest_sha256:
        raise ControllerError(what + ' does not match its artifacts')


# ---------------------------------------------------------------------------------------------
# E: the sealed release record
# ---------------------------------------------------------------------------------------------

E_KEYS = ('schema', 'repository', 'version', 'commit', 'design_landing', 'source', 'candidate', 'targets',
          'security', 'audit', 'tutorials', 'approval_basis')
JOB_KEYS = ('run_id', 'run_attempt', 'job_id', 'job_url', 'conclusion')
ARTIFACT_KEYS = ('id', 'name', 'digest', 'size')
# guide_sha256, here and in release/source.json: the SHA-256 of the exact bytes of release/agent-guide.json
# at S, the public guide bundle that the binary embeds. Evidence sealed before the first embedding release
# keeps its own meaning, the shared digest of the two guide assets of that release.
SOURCE_DIGEST_KEYS = ('notes_sha256', 'guide_sha256', 'source_json_sha256', 'export_receipt_sha256')
CANDIDATE_KEYS = ('ref', 'run_id', 'run_attempt', 'artifact', 'manifest_sha256', 'artifacts')
TARGET_KEYS = ('target', 'archive', 'build', 'package', 'fast_gate', 'install')
FAST_GATE_KEYS = ('job', 'passed', 'failed', 'ignored', 'prerequisite_skipped')
INSTALL_KEYS = ('job', 'report_sha256', 'checks', 'skipped')
SECURITY_KEYS = ('jobs', 'codeql_open_high_critical', 'advisory_fetch')
ADVISORY_KEYS = ('tool', 'fetched_at', 'log_sha256', 'result')
AUDIT_KEYS = ('reference', 'sha256')
BASIS_KEYS = ('mode', 'decision', 'policy_commit', 'scope', 'cards')
FULL_SCOPE_KEYS = ('repository', 'version', 'commit', 'design_landing', 'manifest_sha256', 'artifacts', 'key_arn',
                   'fingerprint', 'first_machine_tag', 'transition_sha256', 'targets')
CARD_KEYS = ('kind', 'reference', 'sha256')


def check_job(value, what):
    """E's `Job`: one GitHub job of one run attempt in rdy-core, concluded success."""
    closed(value, JOB_KEYS, what)
    run_id = pos_int(value['run_id'], what + ' run id')
    pos_int(value['run_attempt'], what + ' run attempt')
    pos_int(value['job_id'], what + ' job id')
    url = value['job_url']
    if not (isinstance(url, str) and len(url.encode('utf-8')) <= URL_MAX
            and url.startswith(JOB_URL_PREFIX + str(run_id) + '/')):
        raise ControllerError(what + ' URL is not this run in rdy-core')
    if not is_kind(value['conclusion'], ('success',)):
        raise ControllerError(what + ' conclusion is not success')
    return value


def check_artifact(value, what):
    """E's `Artifact`: one Actions artifact with its artifact-service digest."""
    closed(value, ARTIFACT_KEYS, what)
    pos_int(value['id'], what + ' id')
    text(value['name'], what + ' name')
    pattern(value['digest'], ARTIFACT_DIGEST_RE, what + ' digest', 'sha256:<64 hex>')
    pos_int(value['size'], what + ' size')
    return value


def check_full_scope(value, what):
    """The full release scope (approval_basis.scope and a release_1 card's scope)."""
    closed(value, FULL_SCOPE_KEYS, what)
    const(value['repository'], REPOSITORY, what + ' repository')
    version(value['version'], what + ' version')
    commit(value['commit'], what + ' commit')
    text(value['design_landing'], what + ' design landing', NAME_MAX)
    sha256(value['manifest_sha256'], what + ' manifest digest')
    files(value['artifacts'], what + ' artifacts', 'an artifact of ' + what)
    manifest_matches(value['manifest_sha256'], value['artifacts'], what + ' manifest digest')
    key_arn(value['key_arn'], what + ' key ARN')
    fingerprint(value['fingerprint'], what + ' fingerprint')
    tag(value['first_machine_tag'], what + ' first machine tag')
    sha256(value['transition_sha256'], what + ' transition digest')
    targets(value['targets'], what + ' targets')
    return value


def check_card(value):
    """A card reference `{kind, reference, sha256}`; returns the receipt digest it names."""
    closed(value, CARD_KEYS, 'a card reference')
    if not is_kind(value['kind'], CARD_KINDS):
        raise ControllerError('a card reference kind is not release_1, key_change, revocation or new_platform')
    sha256(value['sha256'], 'a card reference digest')
    match = isinstance(value['reference'], str) and CARD_REFERENCE_RE.fullmatch(value['reference'])
    if not match:
        raise ControllerError('a card reference is not an approval record')
    if match.group(4) != value['sha256']:
        raise ControllerError('a card reference does not name its digest')
    return value['sha256']


def card_receipt_sha256(card, channel_policy):
    """The receipt digest a card names, when its reference is in the pinned approval archive.
    `channel_policy` is a parsed evidence policy's `approval_channel`."""
    receipt_sha256 = check_card(card)
    archive = channel_policy['archive']
    expected = ('dynamodb://' + archive['account_id'] + '/' + archive['region'] + '/' + archive['table'] + '/'
                + archive['pk_prefix'].replace('#', '%23') + receipt_sha256 + '/' + archive['sk'])
    if card['reference'] != expected:
        raise ControllerError('a card reference is not in the pinned approval archive')
    return receipt_sha256


def check_approval_basis(value, record):
    closed(value, BASIS_KEYS, 'the approval basis')
    if not is_kind(value['mode'], ('standing_preview', 'founder_card')):
        raise ControllerError('the approval mode is not standing_preview or founder_card')
    if not is_kind(value['decision'], (DECISION,)):
        raise ControllerError('the approval decision is not ' + DECISION)
    commit(value['policy_commit'], 'the approval policy commit')
    scope = check_full_scope(value['scope'], 'the approval scope')
    cards = value['cards']
    if not (isinstance(cards, list) and len(cards) <= RECEIPTS_MAX):
        raise ControllerError('the approval cards are not 0 to ' + str(RECEIPTS_MAX) + ' entries')
    digests = [check_card(card) for card in cards]
    if len(set(digests)) != len(digests):
        raise ControllerError('the approval cards repeat a receipt')
    if value['mode'] == 'founder_card' and not cards:
        raise ControllerError('the founder_card approval basis has no card')
    if value['mode'] == 'standing_preview' and cards:
        raise ControllerError('the standing_preview approval basis holds a card')
    # The scope repeats facts E states elsewhere; inside one record they must agree.
    repeated = (('repository', record['repository']), ('version', record['version']), ('commit', record['commit']),
                ('design_landing', record['design_landing']),
                ('manifest_sha256', record['candidate']['manifest_sha256']),
                ('artifacts', record['candidate']['artifacts']),
                ('targets', [entry['target'] for entry in record['targets']]))
    for key, expected in repeated:
        if scope[key] != expected:
            raise ControllerError('the approval scope ' + key.replace('_', ' ') + ' does not match the release record')
    return value


def check_target(entry, record_version):
    closed(entry, TARGET_KEYS, 'a target entry')
    target = entry['target']
    file_entry(entry['archive'], 'the ' + target + ' archive')
    if entry['archive']['name'] != cand.archive_name(record_version, target):
        raise ControllerError('the ' + target + ' archive is not its preview archive')
    check_job(entry['build'], 'the ' + target + ' build job')
    check_job(entry['package'], 'the ' + target + ' package job')
    gate = closed(entry['fast_gate'], FAST_GATE_KEYS, 'the ' + target + ' fast gate')
    check_job(gate['job'], 'the ' + target + ' fast gate job')
    pos_int(gate['passed'], 'the ' + target + ' fast gate passed count')
    zero(gate['failed'], 'the ' + target + ' fast gate failed count')
    if not cand.is_int(gate['ignored'], 0):
        raise ControllerError('the ' + target + ' fast gate ignored count is not an integer')
    zero(gate['prerequisite_skipped'], 'the ' + target + ' fast gate prerequisite skip count')
    install = closed(entry['install'], INSTALL_KEYS, 'the ' + target + ' install check')
    check_job(install['job'], 'the ' + target + ' install job')
    sha256(install['report_sha256'], 'the ' + target + ' install report digest')
    checks = install['checks']
    if not (isinstance(checks, list) and len(checks) == len(CHECK_NAMES)
            and all(isinstance(c, dict) and sorted(c) == ['name', 'passed'] for c in checks)
            and [c['name'] for c in checks] == list(CHECK_NAMES)
            and all(c['passed'] is True for c in checks)):
        raise ControllerError('the ' + target + ' install checks are not the six passed checks')
    zero(install['skipped'], 'the ' + target + ' install skip count')


def check_release_record(record):
    closed(record, E_KEYS, 'the release record')
    const(record['schema'], 1, 'the release record schema')
    const(record['repository'], REPOSITORY, 'the release record repository')
    record_version = version(record['version'])
    commit(record['commit'], 'the commit')
    text(record['design_landing'], 'the design landing', NAME_MAX)
    source = closed(record['source'], SOURCE_DIGEST_KEYS, 'the release source digests')
    for key in SOURCE_DIGEST_KEYS:
        sha256(source[key], 'the source ' + key.replace('_sha256', '').replace('_', ' ') + ' digest')

    candidate = closed(record['candidate'], CANDIDATE_KEYS, 'the candidate')
    if candidate['ref'] != 'refs/tags/candidate/v' + record_version:
        raise ControllerError('the candidate ref is not refs/tags/candidate/v' + record_version)
    pos_int(candidate['run_id'], 'the candidate run id')
    pos_int(candidate['run_attempt'], 'the candidate run attempt')
    check_artifact(candidate['artifact'], 'the candidate artifact')
    if candidate['artifact']['name'] != cand.candidate_artifact_name(candidate['run_attempt']):
        raise ControllerError('the candidate artifact is not ' + cand.candidate_artifact_name(candidate['run_attempt']))
    sha256(candidate['manifest_sha256'], 'the candidate manifest digest')
    archives = candidate['artifacts']
    if not (isinstance(archives, list)
            and [file_entry(f, 'a candidate archive')['name'] for f in archives]
            == [cand.archive_name(record_version, t) for t in TARGETS]):
        raise ControllerError('the candidate artifacts are not exactly the two archives in name order')
    manifest_matches(candidate['manifest_sha256'], archives, 'the candidate manifest digest')

    entries = record['targets']
    if not (isinstance(entries, list) and [closed(e, TARGET_KEYS, 'a target entry')['target'] for e in entries]
            == list(TARGETS)):
        raise ControllerError('the targets are not exactly the release targets in order')
    by_name = {f['name']: f for f in archives}
    for entry in entries:
        check_target(entry, record_version)
        if entry['archive'] != by_name[entry['archive']['name']]:
            raise ControllerError('the ' + entry['target'] + ' archive is not its candidate archive')

    security = closed(record['security'], SECURITY_KEYS, 'the security record')
    jobs = security['jobs']
    if not (isinstance(jobs, list) and all(isinstance(j, dict) and sorted(j) == ['job', 'name'] for j in jobs)
            and [j['name'] for j in jobs] == list(SECURITY_JOBS)):
        raise ControllerError('the security jobs are not exactly the six required jobs')
    for entry in jobs:
        check_job(entry['job'], 'the ' + entry['name'] + ' job')
    zero(security['codeql_open_high_critical'], 'the open high or critical CodeQL alert count')
    advisory = closed(security['advisory_fetch'], ADVISORY_KEYS, 'the advisory fetch')
    text(advisory['tool'], 'the advisory tool')
    utc(advisory['fetched_at'], 'the advisory fetch time')
    sha256(advisory['log_sha256'], 'the advisory log digest')
    if not is_kind(advisory['result'], ('clean',)):
        raise ControllerError('the advisory fetch result is not clean')

    audit = closed(record['audit'], AUDIT_KEYS, 'the audit')
    text(audit['reference'], 'the audit reference', URL_MAX)
    sha256(audit['sha256'], 'the audit digest')

    tutorials = record['tutorials']
    if not isinstance(tutorials, list):
        raise ControllerError('the tutorials need exactly one entry per target')
    # The existing tutorial check (rehearse.py) binds each entry to its candidate archive.
    sealed = rehearse.check_tutorials(tutorials, {'version': record_version, 'artifacts': archives,
                                                  'manifest_sha256': candidate['manifest_sha256']})
    if sealed != tutorials:
        raise ControllerError('the tutorials are not one per release target in order')
    for entry in tutorials:
        sha256(entry['archive_sha256'], 'the ' + entry['target'] + ' tutorial archive digest')

    check_approval_basis(record['approval_basis'], record)
    return record


def parse_release_record(data):
    """E: the sealed eligibility release record, at most 1 MiB."""
    digest, value = read(data, 'release record', RELEASE_RECORD_MAX)
    return Record('release record', digest, len(data), check_release_record(value))


# ---------------------------------------------------------------------------------------------
# T, policy, evidence policy and release source
# ---------------------------------------------------------------------------------------------

T_KEYS = ('schema', 'repository', 'version', 'commit', 'release_evidence_sha256', 'manifest_sha256', 'artifacts',
          'signer')
SIGNER_KEYS = ('key_arn', 'fingerprint', 'first_machine_tag', 'transition_sha256', 'policy_commit',
               'signer_workflow_sha')
POLICY_KEYS = ('schema', 'repository', 'key_arn', 'role_arn', 'fingerprint', 'first_machine_tag', 'targets',
               'records_sha256', 'evidence_policy_sha256', 'signer_workflow_sha')
EVIDENCE_POLICY_KEYS = ('schema', 'repository_id', 'coordinators', 'evidence_workflow', 'release_workflow_path',
                        'approval_channel')
COORDINATOR_KEYS = ('actor_id', 'app_id', 'installation_id')
EVIDENCE_WORKFLOW_KEYS = ('id', 'path', 'sha')
CHANNEL_KEYS = ('schema', 'repository', 'repository_id', 'founder_user_id', 'workflow_id', 'workflow_path',
                'implementation_sha', 'workflow_file_sha256', 'dispatch_ref', 'required_job_name', 'environment_id',
                'environment_name', 'prevent_self_review', 'can_admins_bypass', 'archive', 'enabled',
                'revoked_receipts')
ARCHIVE_KEYS = ('provider', 'account_id', 'region', 'table', 'pk_prefix', 'sk')
SOURCE_KEYS = ('schema', 'version', 'design_landing', 'notes_sha256', 'guide_sha256')
RELEASE_WORKFLOW_PATH = '.github/workflows/release.yml'
MAIN_REF = 'refs/heads/main'


def check_tuple(value):
    closed(value, T_KEYS, 'the tuple')
    const(value['schema'], 1, 'the tuple schema')
    const(value['repository'], REPOSITORY, 'the tuple repository')
    version(value['version'])
    commit(value['commit'], 'the commit')
    sha256(value['release_evidence_sha256'], 'the release evidence digest')
    sha256(value['manifest_sha256'], 'the manifest digest')
    files(value['artifacts'], 'the tuple artifacts', 'a tuple artifact')
    manifest_matches(value['manifest_sha256'], value['artifacts'], 'the manifest digest')
    signer = closed(value['signer'], SIGNER_KEYS, 'the signer')
    key_arn(signer['key_arn'], 'the signer key ARN')
    fingerprint(signer['fingerprint'], 'the signer fingerprint')
    tag(signer['first_machine_tag'], 'the signer first machine tag')
    sha256(signer['transition_sha256'], 'the signer transition digest')
    commit(signer['policy_commit'], 'the signer policy commit')
    commit(signer['signer_workflow_sha'], 'the signer workflow SHA')
    return value


def parse_tuple(data):
    """T: the canonical reservation tuple, at most 64 KiB."""
    digest, value = read(data, 'tuple', RECORD_MAX)
    return Record('tuple', digest, len(data), check_tuple(value))


def tuple_bytes(value):
    """The canonical bytes of a checked T: the reservation tag's message."""
    data = cand.canonical(check_tuple(value))
    if len(data) > RECORD_MAX:
        raise ControllerError('the tuple is larger than ' + str(RECORD_MAX) + ' bytes')
    return data


def parse_policy(data):
    """`release-keys/policy.json`, at most 64 KiB."""
    digest, value = read(data, 'policy', RECORD_MAX)
    closed(value, POLICY_KEYS, 'the policy')
    const(value['schema'], 1, 'the policy schema')
    const(value['repository'], REPOSITORY, 'the policy repository')
    key_arn(value['key_arn'], 'the policy key ARN')
    pattern(value['role_arn'], ROLE_ARN_RE, 'the policy role ARN', 'an IAM role ARN')
    fingerprint(value['fingerprint'], 'the policy fingerprint')
    tag(value['first_machine_tag'], 'the policy first machine tag')
    targets(value['targets'], 'the policy targets')
    sha256(value['records_sha256'], 'the policy records digest')
    sha256(value['evidence_policy_sha256'], 'the policy evidence-policy digest')
    commit(value['signer_workflow_sha'], 'the policy signer workflow SHA')
    return Record('policy', digest, len(data), value)


def check_channel_policy(value):
    """The founder approval channel policy (evidence policy's `approval_channel`). Its production
    pins come from enrollment; the shape is fixed here and nothing defaults."""
    what = 'the approval channel'
    closed(value, CHANNEL_KEYS, what)
    const(value['schema'], 1, what + ' schema')
    const(value['repository'], REPOSITORY, what + ' repository')
    const(value['repository_id'], REPOSITORY_ID, what + ' repository id')
    pos_int(value['founder_user_id'], what + ' founder user id')
    pos_int(value['workflow_id'], what + ' workflow id')
    workflow_path(value['workflow_path'], what + ' workflow path')
    commit(value['implementation_sha'], what + ' implementation SHA')
    sha256(value['workflow_file_sha256'], what + ' workflow file digest')
    const(value['dispatch_ref'], MAIN_REF, what + ' dispatch ref')
    text(value['required_job_name'], what + ' required job name', NAME_MAX)
    pos_int(value['environment_id'], what + ' environment id')
    text(value['environment_name'], what + ' environment name', NAME_MAX)
    false(value['prevent_self_review'], what + ' prevent_self_review setting')
    false(value['can_admins_bypass'], what + ' admin bypass setting')
    archive = closed(value['archive'], ARCHIVE_KEYS, 'the approval archive')
    const(archive['provider'], 'dynamodb', 'the approval archive provider')
    pattern(archive['account_id'], ACCOUNT_RE, 'the approval archive account', 'a 12-digit account id')
    pattern(archive['region'], REGION_RE, 'the approval archive region', 'a region')
    pattern(archive['table'], TABLE_RE, 'the approval archive table', 'a table name')
    const(archive['pk_prefix'], 'APPROVAL#', 'the approval archive key prefix')
    const(archive['sk'], 'RECORD', 'the approval archive sort key')
    boolean(value['enabled'], what + ' enabled flag')
    revoked = value['revoked_receipts']
    if not (isinstance(revoked, list) and len(revoked) <= RECEIPTS_MAX):
        raise ControllerError('the revoked receipts are not 0 to ' + str(RECEIPTS_MAX) + ' entries')
    for entry in revoked:
        sha256(entry, 'a revoked receipt')
    if len(set(revoked)) != len(revoked):
        raise ControllerError('the revoked receipts repeat a receipt')
    return value


def parse_evidence_policy(data):
    """`release-keys/evidence-policy.json`, at most 64 KiB."""
    digest, value = read(data, 'evidence policy', RECORD_MAX)
    closed(value, EVIDENCE_POLICY_KEYS, 'the evidence policy')
    const(value['schema'], 1, 'the evidence policy schema')
    const(value['repository_id'], REPOSITORY_ID, 'the evidence policy repository id')
    coordinators = value['coordinators']
    if not (isinstance(coordinators, list) and 0 < len(coordinators) <= COORDINATORS_MAX):
        raise ControllerError('the coordinators are not 1 to ' + str(COORDINATORS_MAX) + ' entries')
    for entry in coordinators:
        closed(entry, COORDINATOR_KEYS, 'a coordinator')
        person = entry['app_id'] is None and entry['installation_id'] is None
        app = cand.is_int(entry['app_id'], 1) and cand.is_int(entry['installation_id'], 1)
        if not (cand.is_int(entry['actor_id'], 1) and (person or app)):
            raise ControllerError('a coordinator is malformed')
    if len({entry['actor_id'] for entry in coordinators}) != len(coordinators):
        raise ControllerError('the coordinators repeat an actor')
    workflow = closed(value['evidence_workflow'], EVIDENCE_WORKFLOW_KEYS, 'the evidence workflow')
    pos_int(workflow['id'], 'the evidence workflow id')
    workflow_path(workflow['path'], 'the evidence workflow path')
    commit(workflow['sha'], 'the evidence workflow SHA')
    const(value['release_workflow_path'], RELEASE_WORKFLOW_PATH, 'the release workflow path')
    check_channel_policy(value['approval_channel'])
    return Record('evidence policy', digest, len(data), value)


def parse_source(data):
    """`release/source.json`, at most 64 KiB."""
    digest, value = read(data, 'release source', RECORD_MAX)
    closed(value, SOURCE_KEYS, 'the release source')
    const(value['schema'], 1, 'the release source schema')
    version(value['version'])
    text(value['design_landing'], 'the design landing', NAME_MAX)
    sha256(value['notes_sha256'], 'the notes digest')
    sha256(value['guide_sha256'], 'the guide digest')
    return Record('release source', digest, len(data), value)


# ---------------------------------------------------------------------------------------------
# The founder approval channel
# ---------------------------------------------------------------------------------------------

REQUEST_KEYS = ('schema', 'decision', 'policy_commit', 'kind', 'scope')
RECEIPT_KEYS = ('schema', 'decision', 'kind', 'policy_commit', 'workflow_scope_digest', 'scope', 'issuer')
SCOPE_KEYS = {
    'release_1': FULL_SCOPE_KEYS,
    'key_change': ('repository', 'key_arn', 'fingerprint', 'first_machine_tag', 'transition_sha256', 'targets'),
    'revocation': ('repository', 'key_arn', 'fingerprint', 'revocation_sha256', 'last_good_tag'),
    'new_platform': ('repository', 'targets'),
}
# Approval records have two versions. Schema 1 is the frozen approval-channel-v1 wire: its receipt's
# `issuer.provider_evidence_sha256` hashes the whole projection, source responses included. Schema 2 is
# the successor: its receipt's `issuer.stable_identity_sha256` hashes only the projection's `identity`,
# and the source responses are a separate `audit` section that the archive acceptance binds.
RECORD_SCHEMAS = (1, 2)
ISSUER_FACT_KEYS = ('repository_id', 'workflow_id', 'workflow_path', 'implementation_sha', 'run_id', 'run_attempt',
                    'run_head_sha', 'ref', 'actor_id', 'triggering_actor_id', 'job_id', 'job_name', 'artifact_id',
                    'artifact_sha256', 'reviewer_id', 'environment_id', 'environment_name',
                    'review_comment_sha256')
ISSUER_KEYS = ISSUER_FACT_KEYS + ('provider_evidence_sha256',)
ISSUER_V2_KEYS = ISSUER_FACT_KEYS + ('stable_identity_sha256',)
PROVIDER_FACT_KEYS = ('repository_id', 'head_repository_id', 'workflow_id', 'workflow_path', 'implementation_sha',
                      'workflow_file_sha256', 'run_id', 'run_attempt', 'run_head_sha', 'ref', 'event', 'status',
                      'conclusion', 'actor_id', 'triggering_actor_id', 'job', 'artifact', 'review')
PROJECTION_KEYS = ('schema',) + PROVIDER_FACT_KEYS + ('source_responses',)
PROJECTION_V2_KEYS = ('schema', 'identity', 'audit')
IDENTITY_KEYS = ('schema', 'decision', 'kind', 'policy_commit', 'scope') + PROVIDER_FACT_KEYS
PROVIDER_AUDIT_KEYS = ('source_responses',)
PROJECTION_JOB_KEYS = ('id', 'name', 'run_id', 'status', 'conclusion')
PROJECTION_ARTIFACT_KEYS = ('id', 'name', 'sha256', 'workflow_run_id', 'repository_id', 'head_repository_id',
                            'head_sha', 'expired', 'request_sha256')
REVIEW_KEYS = ('state', 'comment', 'user_id', 'user_type', 'environment_id', 'environment_name')
SOURCE_RESPONSE_KEYS = ('kind', 'sha256', 'bytes')
ENVELOPE_KEYS = ('schema', 'request_body', 'receipt_body', 'provider_projection_body', 'acceptance')
ACCEPTANCE_KEYS = ('schema', 'receipt_sha256', 'request_sha256', 'provider_projection_sha256', 'signer_workflow_sha',
                   'run_id', 'run_attempt', 'job_id', 'policy_commit', 'policy_sha256', 'authority_sha256',
                   'table_id', 'genesis_sha256', 'accepted_at')


def check_scope(kind, value):
    what = 'the ' + kind + ' scope'
    if kind == 'release_1':
        return check_full_scope(value, what)
    closed(value, SCOPE_KEYS[kind], what)
    const(value['repository'], REPOSITORY, what + ' repository')
    if kind == 'key_change':
        key_arn(value['key_arn'], what + ' key ARN')
        fingerprint(value['fingerprint'], what + ' fingerprint')
        tag(value['first_machine_tag'], what + ' first machine tag')
        sha256(value['transition_sha256'], what + ' transition digest')
        targets(value['targets'], what + ' targets')
    elif kind == 'revocation':
        key_arn(value['key_arn'], what + ' key ARN')
        fingerprint(value['fingerprint'], what + ' fingerprint')
        sha256(value['revocation_sha256'], what + ' revocation digest')
        if value['last_good_tag'] is not None:
            tag(value['last_good_tag'], what + ' last good tag')
    else:
        targets(value['targets'], what + ' targets')
    return value


def check_kind(value, what):
    if not is_kind(value, CARD_KINDS):
        raise ControllerError(what + ' kind is not release_1, key_change, revocation or new_platform')
    return value


def parse_approval_request(data):
    """A founder approval request, at most 60000 bytes. Its SHA-256 is the workflow scope digest."""
    digest, value = read(data, 'approval request', REQUEST_MAX)
    closed(value, REQUEST_KEYS, 'the approval request')
    const(value['schema'], 1, 'the approval request schema')
    const(value['decision'], DECISION, 'the approval request decision')
    commit(value['policy_commit'], 'the approval request policy commit')
    check_scope(check_kind(value['kind'], 'the approval request'), value['scope'])
    return Record('approval request', digest, len(data), value)


def record_schema(value, what):
    """The version of an approval record: 1 (the frozen v1 wire) or 2 (the successor)."""
    if not isinstance(value, dict):
        raise ControllerError(what + ' is not an object')
    if 'schema' not in value:
        raise ControllerError("missing field 'schema'")
    schema = value['schema']
    if not (type(schema) is int and schema in RECORD_SCHEMAS):
        raise ControllerError(what + ' schema is not 1 or 2')
    return schema


def check_issuer(value, schema=1):
    what = 'the approval issuer'
    closed(value, ISSUER_KEYS if schema == 1 else ISSUER_V2_KEYS, what)
    const(value['repository_id'], REPOSITORY_ID, what + ' repository id')
    for key in ('workflow_id', 'run_id', 'actor_id', 'triggering_actor_id', 'job_id', 'artifact_id', 'reviewer_id',
                'environment_id'):
        pos_int(value[key], what + ' ' + key.replace('_', ' '))
    workflow_path(value['workflow_path'], what + ' workflow path')
    commit(value['implementation_sha'], what + ' implementation SHA')
    const(value['run_attempt'], 1, what + ' run attempt')
    commit(value['run_head_sha'], what + ' run head SHA')
    const(value['ref'], MAIN_REF, what + ' ref')
    text(value['job_name'], what + ' job name', NAME_MAX)
    sha256(value['artifact_sha256'], what + ' artifact digest')
    text(value['environment_name'], what + ' environment name', NAME_MAX)
    sha256(value['review_comment_sha256'], what + ' review comment digest')
    if schema == 1:
        sha256(value['provider_evidence_sha256'], what + ' provider evidence digest')
    else:
        sha256(value['stable_identity_sha256'], what + ' stable identity digest')
    return value


def parse_approval_receipt(data):
    """An accepted founder approval receipt, at most 64 KiB. A card names its SHA-256. Schema 1 binds
    `issuer.provider_evidence_sha256` (the whole v1 projection); schema 2 binds
    `issuer.stable_identity_sha256` (the projection's identity only)."""
    digest, value = read(data, 'approval receipt', RECORD_MAX)
    closed(value, RECEIPT_KEYS, 'the approval receipt')
    schema = record_schema(value, 'the approval receipt')
    const(value['decision'], DECISION, 'the approval receipt decision')
    commit(value['policy_commit'], 'the approval receipt policy commit')
    sha256(value['workflow_scope_digest'], 'the approval receipt workflow scope digest')
    check_scope(check_kind(value['kind'], 'the approval receipt'), value['scope'])
    check_issuer(value['issuer'], schema)
    return Record('approval receipt', digest, len(data), value)


def stable_identity_sha256(identity):
    """The successor approval identity: the SHA-256 of the canonical JSON (sorted keys, compact
    separators, UTF-8, one final LF) of a schema 2 projection's `identity` object. The identity holds
    typed provenance and scope facts only: never a response digest, ETag, JSON order or other provider
    metadata. GitHub's review history has no review id, time or attempt, so none is in it."""
    return hashlib.sha256(cand.canonical(identity)).hexdigest()


def check_provider_facts(value, what):
    """The typed facts of one completed founder approval run (a v1 projection, or a v2 identity)."""
    const(value['repository_id'], REPOSITORY_ID, what + ' repository id')
    const(value['head_repository_id'], REPOSITORY_ID, what + ' head repository id')
    for key in ('workflow_id', 'run_id', 'actor_id', 'triggering_actor_id'):
        pos_int(value[key], what + ' ' + key.replace('_', ' '))
    workflow_path(value['workflow_path'], what + ' workflow path')
    commit(value['implementation_sha'], what + ' implementation SHA')
    sha256(value['workflow_file_sha256'], what + ' workflow file digest')
    const(value['run_attempt'], 1, what + ' run attempt')
    commit(value['run_head_sha'], what + ' run head SHA')
    const(value['ref'], MAIN_REF, what + ' ref')
    const(value['event'], 'workflow_dispatch', what + ' event')
    const(value['status'], 'completed', what + ' status')
    const(value['conclusion'], 'success', what + ' conclusion')
    job = closed(value['job'], PROJECTION_JOB_KEYS, what + ' job')
    pos_int(job['id'], what + ' job id')
    text(job['name'], what + ' job name', NAME_MAX)
    pos_int(job['run_id'], what + ' job run id')
    const(job['status'], 'completed', what + ' job status')
    const(job['conclusion'], 'success', what + ' job conclusion')
    artifact = closed(value['artifact'], PROJECTION_ARTIFACT_KEYS, what + ' artifact')
    pos_int(artifact['id'], what + ' artifact id')
    text(artifact['name'], what + ' artifact name', NAME_MAX)
    sha256(artifact['sha256'], what + ' artifact digest')
    pos_int(artifact['workflow_run_id'], what + ' artifact run id')
    const(artifact['repository_id'], REPOSITORY_ID, what + ' artifact repository id')
    const(artifact['head_repository_id'], REPOSITORY_ID, what + ' artifact head repository id')
    commit(artifact['head_sha'], what + ' artifact head SHA')
    false(artifact['expired'], what + ' artifact expired flag')
    sha256(artifact['request_sha256'], what + ' request digest')
    review = closed(value['review'], REVIEW_KEYS, what + ' review')
    const(review['state'], 'approved', what + ' review state')
    pattern(review['comment'], COMMENT_RE, what + ' review comment', 'APPROVE sha256:<64 hex>')
    pos_int(review['user_id'], what + ' reviewer id')
    const(review['user_type'], 'User', what + ' reviewer type')
    pos_int(review['environment_id'], what + ' environment id')
    text(review['environment_name'], what + ' environment name', NAME_MAX)
    # One run: its job, artifact and reviewer belong to it.
    if job['run_id'] != value['run_id'] or artifact['workflow_run_id'] != value['run_id']:
        raise ControllerError(what + ' job or artifact is not from its run')
    if artifact['head_sha'] != value['run_head_sha']:
        raise ControllerError(what + ' artifact is not from the run head')
    if not (value['actor_id'] == value['triggering_actor_id'] == review['user_id']):
        raise ControllerError(what + ' actors and reviewer differ')
    if COMMENT_RE.fullmatch(review['comment']).group(1) != artifact['request_sha256']:
        raise ControllerError(what + ' review comment does not name its request')
    return value


def check_source_responses(responses, what):
    """The nine {kind, sha256, bytes} digests of the original responses, sorted by kind."""
    if not (isinstance(responses, list) and all(isinstance(r, dict) for r in responses)
            and [r.get('kind') for r in responses] == list(SOURCE_KINDS)):
        raise ControllerError(what + ' source responses are not the nine kinds in order')
    for entry in responses:
        closed(entry, SOURCE_RESPONSE_KEYS, what + ' source response')
        sha256(entry['sha256'], what + ' source response digest')
        if not (cand.is_int(entry['bytes'], 1) and entry['bytes'] <= SOURCE_RESPONSE_MAX):
            raise ControllerError(what + ' source response size is not 1 to ' + str(SOURCE_RESPONSE_MAX) + ' bytes')
    return responses


def parse_provider_evidence(data):
    """The canonical provider-facts projection of a founder approval, at most 64 KiB. It holds digests
    and lengths of the original responses, not the responses.

    Schema 1 (frozen v1) is flat: the typed facts and `source_responses`; a v1 receipt binds the SHA-256
    of these exact bytes. Schema 2 splits it: `identity` (the typed facts and the request's decision,
    kind, policy commit and scope), which a v2 receipt binds through `stable_identity_sha256`, and
    `audit` (`source_responses`), which only the archive acceptance binds, through the exact bytes."""
    digest, value = read(data, 'provider evidence', RECORD_MAX)
    what = 'the provider evidence'
    schema = record_schema(value, what)
    if schema == 1:
        closed(value, PROJECTION_KEYS, what)
        check_provider_facts(value, what)
        check_source_responses(value['source_responses'], what)
        return Record('provider evidence', digest, len(data), value)
    closed(value, PROJECTION_V2_KEYS, what)
    identity = closed(value['identity'], IDENTITY_KEYS, what + ' identity')
    const(identity['schema'], 2, what + ' identity schema')
    const(identity['decision'], DECISION, what + ' identity decision')
    commit(identity['policy_commit'], what + ' identity policy commit')
    check_scope(check_kind(identity['kind'], what + ' identity'), identity['scope'])
    check_provider_facts(identity, what)
    audit = closed(value['audit'], PROVIDER_AUDIT_KEYS, what + ' audit')
    check_source_responses(audit['source_responses'], what)
    return Record('provider evidence', digest, len(data), value)


ISSUER_FROM_PROJECTION = (
    ('repository_id', ('repository_id',)), ('workflow_id', ('workflow_id',)), ('workflow_path', ('workflow_path',)),
    ('implementation_sha', ('implementation_sha',)), ('run_id', ('run_id',)), ('run_attempt', ('run_attempt',)),
    ('run_head_sha', ('run_head_sha',)), ('ref', ('ref',)), ('actor_id', ('actor_id',)),
    ('triggering_actor_id', ('triggering_actor_id',)), ('job_id', ('job', 'id')), ('job_name', ('job', 'name')),
    ('artifact_id', ('artifact', 'id')), ('artifact_sha256', ('artifact', 'sha256')),
    ('reviewer_id', ('review', 'user_id')), ('environment_id', ('review', 'environment_id')),
    ('environment_name', ('review', 'environment_name')),
)


def body(value, what):
    """An envelope body: a string whose UTF-8 bytes are the original record."""
    if not isinstance(value, str) or not value:
        raise ControllerError('the archive ' + what + ' body is not a string')
    try:
        return value.encode('utf-8')
    except UnicodeEncodeError:
        raise ControllerError('the archive ' + what + ' body is not UTF-8')


def check_acceptance(value):
    what = 'the archive acceptance'
    closed(value, ACCEPTANCE_KEYS, what)
    const(value['schema'], 1, what + ' schema')
    for key in ('receipt_sha256', 'request_sha256', 'provider_projection_sha256', 'policy_sha256',
                'authority_sha256', 'genesis_sha256'):
        sha256(value[key], what + ' ' + key.replace('_sha256', '').replace('_', ' ') + ' digest')
    commit(value['signer_workflow_sha'], what + ' signer workflow SHA')
    for key in ('run_id', 'run_attempt', 'job_id'):
        pos_int(value[key], what + ' ' + key.replace('_', ' '))
    commit(value['policy_commit'], what + ' policy commit')
    pattern(value['table_id'], UUID_RE, what + ' table id', 'a table UUID')
    match = isinstance(value['accepted_at'], str) and ACCEPTED_AT_RE.fullmatch(value['accepted_at'])
    if match:
        try:
            datetime.datetime.strptime(match.group(1), '%Y-%m-%dT%H:%M:%S')
        except ValueError:
            match = None
    if not match:
        raise ControllerError(what + ' time is not UTC')
    return value


def parse_archive_envelope(data):
    """The one bounded (64 KiB) archive record of an accepted founder approval. Its three bodies
    are parsed from their exact bytes and must join by hash; the issuer must map back to the
    provider evidence field by field. The envelope, receipt and provider evidence share one schema.

    Schema 1 is the frozen v1 record: the receipt's `provider_evidence_sha256` and the acceptance's
    `provider_projection_sha256` both hash the exact projection bytes. Schema 2: the receipt's
    `stable_identity_sha256` hashes the projection's identity, which must bind the request's decision,
    kind, policy commit and scope; the acceptance's `provider_projection_sha256` hashes the exact
    projection bytes, so the archived audit section cannot change without detection.

    This checks the record against itself only: the backend, lineage and current policy are checked
    by eligibility."""
    digest, value = read(data, 'archive envelope', RECORD_MAX)
    closed(value, ENVELOPE_KEYS, 'the archive envelope')
    schema = record_schema(value, 'the archive envelope')
    request = parse_approval_request(body(value['request_body'], 'request'))
    receipt = parse_approval_receipt(body(value['receipt_body'], 'receipt'))
    projection = parse_provider_evidence(body(value['provider_projection_body'], 'provider evidence'))
    acceptance = check_acceptance(value['acceptance'])
    if not (receipt.value['schema'] == projection.value['schema'] == schema):
        raise ControllerError('the archive envelope, receipt and provider evidence schemas differ')
    facts = projection.value if schema == 1 else projection.value['identity']
    if acceptance['receipt_sha256'] != receipt.sha256:
        raise ControllerError('the archive acceptance does not bind its receipt')
    issuer = receipt.value['issuer']
    if receipt.value['workflow_scope_digest'] != request.sha256 or acceptance['request_sha256'] != request.sha256 \
            or facts['artifact']['request_sha256'] != request.sha256:
        raise ControllerError('the approval receipt does not bind its request')
    if schema == 1:
        bound = issuer['provider_evidence_sha256'] == projection.sha256
    else:
        bound = issuer['stable_identity_sha256'] == stable_identity_sha256(facts)
    if not bound or acceptance['provider_projection_sha256'] != projection.sha256:
        raise ControllerError('the approval receipt does not bind its provider evidence')
    for key in ('kind', 'scope', 'policy_commit', 'decision'):
        if request.value[key] != receipt.value[key]:
            raise ControllerError('the approval receipt ' + key.replace('_', ' ') + ' does not match its request')
    if schema == 2:
        for key in ('kind', 'scope', 'policy_commit', 'decision'):
            if request.value[key] != facts[key]:
                raise ControllerError('the approval identity ' + key.replace('_', ' ') + ' does not match its request')
    for key, path in ISSUER_FROM_PROJECTION:
        fact = facts
        for part in path:
            fact = fact[part]
        if issuer[key] != fact or type(issuer[key]) is not type(fact):
            raise ControllerError('the approval issuer ' + key.replace('_', ' ')
                                  + ' does not match its provider evidence')
    comment = facts['review']['comment'].encode('utf-8')
    if issuer['review_comment_sha256'] != hashlib.sha256(comment).hexdigest():
        raise ControllerError('the approval issuer review comment digest does not match its provider evidence')
    return Envelope('archive envelope', digest, len(data), value, request, receipt, projection, acceptance)
