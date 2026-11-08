"""The release evidence producer (E): `.github/workflows/release-evidence.yml`.

  authenticate  job `authenticate-evidence`, outside the release authority lane: checks the closed dispatch
                inputs, authenticates the run, the candidate and the protected policy on main, and refuses an
                existing reservation, final tag or release. Only then does it decode the attestation and check
                it against the candidate. Native work starts only after this job.
  seal          job `seal-release-evidence`, the only job in the lane: repeats every check with fresh reads,
                rehashes the candidate archives, reads both native jobs and their sanitized receipts of this
                exact attempt through the API, the security jobs and the live alert read at S, rebuilds the
                exception receipts of completed founder approval runs, and writes E (`release-record.json`)
                after `parse_release_record` and the consumer's source check pass. The protected policy and
                the reservation state are read again just before E is written.
  dispatch      for the coordinator: the bounded attestation transport and the complete dispatch request.

E keeps schema 1: macOS arm64 and Linux x86-64 archives of a preview, sealed against the protected policy at
S. GitHub authenticates the coordinator and the submitted bytes; it cannot prove external tutorial execution
or export provenance. Inputs select facts and never assert authority: a failed read refuses.

Run as `/usr/bin/python3 -I -B <P>/scripts/release_producers/evidence.py --trusted <checkout of H> <command>`.
A refusal prints `release producer: <reason>` and exits 1.
"""
import argparse
import base64
import binascii
import gzip
import hashlib
import json
from pathlib import Path
import re
import shutil
import sys
import tempfile
import zlib

if __name__ == '__main__':
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from release_producers import common
    _ARGV = common.trust(sys.argv)
else:
    from release_producers import common

from release_producers import approval  # noqa: E402

cand, ev, gh, elig = common._release()
rehearse = ev.rehearse
ControllerError = cand.ControllerError
Refused = elig.Refused
INCOMPLETE, MISMATCH, UNAUTHORIZED, CONFLICT, LIMIT = elig.INCOMPLETE, elig.MISMATCH, elig.UNAUTHORIZED, \
    elig.CONFLICT, elig.LIMIT

WORKFLOW_PATH = '.github/workflows/release-evidence.yml'
JOB_NAMES = {'authenticate-evidence': 'Authenticate release evidence',
             'install-mac': 'Native evidence install (aarch64-apple-darwin)',
             'install-linux': 'Native evidence install (x86_64-unknown-linux-gnu)',
             'seal-release-evidence': 'Seal release evidence'}
NATIVE = {'aarch64-apple-darwin': 'macos-26', 'x86_64-unknown-linux-gnu': 'ubuntu-26.04'}
MEMBER = elig.EVIDENCE_MEMBER                  # 'release-record.json'
RECORD_MAX = ev.RELEASE_RECORD_MAX             # 1 MiB
INPUT_KEYS = ('version', 'candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id',
              'attestation_gzip_b64', 'attestation_sha256', 'approval_runs')
ATTESTATION_KEYS = ('tutorials', 'advisory_fetch', 'advisory_log', 'audit_reference', 'audit_body',
                    'export_receipt_body')
APPROVAL_RUN_KEYS = ('artifact_id', 'run_id')
LANE = 'rdy-core-release-authority'
# Only the sealing job joins the shared lane, after authentication and both native jobs. It never cancels a
# run in progress, and it waits behind a pending run instead of replacing it.
SEAL_CONCURRENCY = {'group': LANE, 'cancel-in-progress': False, 'queue': 'max'}

# The plan's field-source rows: every record E carries and its exact fields (the consumer's own tuples).
RECORD_FIELDS = {
    'file': ('name', 'sha256', 'size'), 'job': ev.JOB_KEYS, 'artifact': ev.ARTIFACT_KEYS,
    'source': ev.SOURCE_DIGEST_KEYS, 'candidate': ev.CANDIDATE_KEYS, 'target': ev.TARGET_KEYS,
    'fast_gate': ev.FAST_GATE_KEYS, 'install': ev.INSTALL_KEYS, 'security': ev.SECURITY_KEYS,
    'advisory_fetch': ev.ADVISORY_KEYS, 'audit': ev.AUDIT_KEYS, 'tutorial': rehearse.TUTORIAL_KEYS,
    'tutorial_step': ('id', 'result'), 'approval_basis': ev.BASIS_KEYS, 'card': ev.CARD_KEYS,
    'release_record': ev.E_KEYS,
}

# H's sanitized rehearsal receipt (`rehearse.rehearse_in`), written only after cleanup succeeded.
RECEIPT_KEYS = ('schema', 'version', 'commit', 'target', 'manifest_sha256', 'archive', 'install', 'session',
                'cleanup')
RECEIPT_INSTALL_KEYS = ('report_sha256', 'checks', 'skipped')
CANDIDATE_ZIP_MAX = 2 * cand.ARCHIVE_MAX + 2 * cand.MIB
TRANSITION_MAX = 4096
TUTORIALS_MAX = 16
DECIMAL_RE = re.compile(r'[1-9][0-9]{0,19}')
HEX64_RE = re.compile(r'[0-9a-f]{64}')
BASE64_RE = re.compile(r'(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?')
# Dispatch inputs are public-capable: no credential shape and no private runner or home path is submitted.
CREDENTIAL_RE = re.compile(r'-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|\bgh[opsur]_[A-Za-z0-9]{36}'
                           r'|\bgithub_pat_[A-Za-z0-9_]{22,}|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b')
PRIVATE_PATHS = ('/Users/', '/home/', '\\Users\\')


class DoesNotFit(ControllerError):
    """The attestation does not fit the dispatch transport; `sizes` holds every measured size."""

    def __init__(self, sizes):
        ControllerError.__init__(
            self, 'the attestation does not fit: document ' + str(sizes['document']) + ' bytes (max '
            + str(common.DOCUMENT_MAX) + '), gzip ' + str(sizes['gzip']) + ' bytes (max ' + str(common.GZIP_MAX)
            + '), base64 ' + str(sizes['base64']) + ' characters (max ' + str(common.BASE64_MAX) + '), dispatch '
            + str(sizes['dispatch']) + ' bytes (max ' + str(common.DISPATCH_MAX) + ')')
        self.sizes = dict(sizes)


def artifact_name(run_id, run_attempt):
    """E's artifact: one per run attempt, never overwritten."""
    return 'release-evidence-' + str(run_id) + '-a' + str(run_attempt)


def receipt_artifact_name(target, run_attempt):
    """One native job's sanitized receipt artifact in one run attempt."""
    return 'rehearsal-' + target + '-a' + str(run_attempt)


def receipt_member(target):
    return 'rehearsal-' + target + '.json'


def _sha(data):
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------------------------------------
# Control inputs
# ---------------------------------------------------------------------------------------------

def _decimal(value, key):
    if not (isinstance(value, str) and value.isascii() and DECIMAL_RE.fullmatch(value)):
        raise ControllerError(key + ' is not a positive decimal of at most ' + str(common.ID_DIGITS_MAX)
                              + ' digits')
    return int(value)


def _approval_runs(text):
    """The bounded JSON list of `{run_id, artifact_id}` locators. They select facts; they assert nothing."""
    data = common.utf8(text, 'approval_runs')
    if len(data) > common.APPROVAL_RUNS_BYTES_MAX:
        raise ControllerError('approval_runs is larger than ' + str(common.APPROVAL_RUNS_BYTES_MAX) + ' bytes')
    try:
        value = cand.loads(data, common.APPROVAL_RUNS_BYTES_MAX)
    except RecursionError:
        raise ControllerError('approval_runs is not JSON')
    if not isinstance(value, list):
        raise ControllerError('approval_runs is not a list')
    if len(value) > common.APPROVAL_RUNS_MAX:
        raise ControllerError('approval_runs holds more than ' + str(common.APPROVAL_RUNS_MAX) + ' entries')
    runs, seen = [], {key: set() for key in APPROVAL_RUN_KEYS}
    for entry in value:
        cand.closed(entry, APPROVAL_RUN_KEYS, 'an approval run')
        for key in APPROVAL_RUN_KEYS:
            if not (cand.is_int(entry[key], 1) and len(str(entry[key])) <= common.ID_DIGITS_MAX):
                raise ControllerError('an approval run ' + key.replace('_', ' ') + ' is not a positive integer')
            if entry[key] in seen[key]:
                raise ControllerError('approval_runs names one run or artifact twice')
            seen[key].add(entry[key])
        runs.append({'artifact_id': entry['artifact_id'], 'run_id': entry['run_id']})
    return runs


def dispatch_inputs(inputs):
    """The seven closed string inputs of one E dispatch, typed. The attestation is not decoded here."""
    cand.closed(inputs, INPUT_KEYS, 'the evidence inputs')
    version = inputs['version']
    if not (isinstance(version, str) and ev.VERSION_RE.fullmatch(version)):
        raise ControllerError('version is not a 0.N.0 preview')
    parsed = {'version': version}
    for key in ('candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id'):
        parsed[key] = _decimal(inputs[key], key)
    if not (isinstance(inputs['attestation_sha256'], str) and HEX64_RE.fullmatch(inputs['attestation_sha256'])):
        raise ControllerError('attestation_sha256 is not lowercase 64-hex')
    parsed['attestation_sha256'] = inputs['attestation_sha256']
    text = inputs['attestation_gzip_b64']
    if not isinstance(text, str):
        raise ControllerError('attestation_gzip_b64 is not a string')
    if len(text) > common.BASE64_MAX:
        raise ControllerError('attestation_gzip_b64 is longer than ' + str(common.BASE64_MAX) + ' characters')
    parsed['attestation_gzip_b64'] = text
    parsed['approval_runs'] = _approval_runs(inputs['approval_runs'])
    return parsed


# ---------------------------------------------------------------------------------------------
# The attestation transport: canonical base64 of one bounded gzip member
# ---------------------------------------------------------------------------------------------

def decode_base64(text):
    """Canonical padded standard base64 only; the length bound holds before decoding."""
    if not isinstance(text, str):
        raise ControllerError('attestation_gzip_b64 is not a string')
    if len(text) > common.BASE64_MAX:
        raise ControllerError('attestation_gzip_b64 is longer than ' + str(common.BASE64_MAX) + ' characters')
    noncanonical = ControllerError('attestation_gzip_b64 is not canonical padded standard base64')
    if not (text.isascii() and BASE64_RE.fullmatch(text)):
        raise noncanonical
    try:
        data = base64.b64decode(text.encode('ascii'), validate=True)
    except (binascii.Error, ValueError):
        raise noncanonical
    if base64.b64encode(data).decode('ascii') != text:
        raise noncanonical
    return data


def decode_gzip(data):
    """Exactly one complete gzip member, decompressed while streaming under the document bound."""
    if len(data) > common.GZIP_MAX:
        raise ControllerError('the attestation gzip is larger than ' + str(common.GZIP_MAX) + ' bytes')
    larger = ControllerError('the attestation is larger than ' + str(common.DOCUMENT_MAX) + ' bytes')
    stream = zlib.decompressobj(31)
    out, total, pending = [], 0, data
    try:
        while not stream.eof:
            chunk = stream.decompress(pending, common.DOCUMENT_MAX + 1 - total)
            total += len(chunk)
            if total > common.DOCUMENT_MAX:
                raise larger
            out.append(chunk)
            pending = stream.unconsumed_tail
            if not chunk and not pending:
                break
    except zlib.error:
        raise ControllerError('the attestation gzip is corrupt')
    if not stream.eof:
        raise ControllerError('the attestation gzip is incomplete')
    if stream.unused_data:
        if stream.unused_data[:2] == b'\x1f\x8b':
            raise ControllerError('the attestation gzip holds more than one member')
        raise ControllerError('the attestation gzip has trailing bytes')
    return b''.join(out)


def decode_attestation(text, sha256):
    """The exact original attestation bytes, checked against `attestation_sha256` before any parsing."""
    if not (isinstance(sha256, str) and HEX64_RE.fullmatch(sha256)):
        raise ControllerError('attestation_sha256 is not lowercase 64-hex')
    data = decode_gzip(decode_base64(text))
    if _sha(data) != sha256:
        raise ControllerError('the attestation does not match attestation_sha256')
    return data


def encode_attestation(data):
    """One gzip member (no file name, time 0) in canonical padded standard base64."""
    return base64.b64encode(gzip.compress(data, compresslevel=9, mtime=0)).decode('ascii')


def _unicode(value):
    if isinstance(value, str):
        try:
            value.encode('utf-8')
        except UnicodeEncodeError:
            raise ControllerError('the attestation is not valid Unicode')
    elif isinstance(value, dict):
        for key, item in value.items():
            _unicode(key)
            _unicode(item)
    elif isinstance(value, list):
        for item in value:
            _unicode(item)


def _strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, item in value.items():
            yield key
            for text in _strings(item):
                yield text
    elif isinstance(value, list):
        for item in value:
            for text in _strings(item):
                yield text


def _document(data):
    if not isinstance(data, bytes):
        raise ControllerError('the attestation is not bytes')
    try:
        value = cand.loads(data, common.DOCUMENT_MAX)
    except RecursionError:
        raise ControllerError('the attestation is not JSON')
    if not isinstance(value, dict):
        raise ControllerError('the attestation is not an object')
    _unicode(value)
    return cand.closed(value, ATTESTATION_KEYS, 'the attestation')


def attestation_facts(data):
    """The E facts of the decompressed attestation: tutorials, advisory metadata, and the digests of the
    three original bodies (hashed as UTF-8, never normalized). No transport-only key leaves here."""
    value = _document(data)
    bodies = {}
    for key in ('advisory_log', 'audit_body', 'export_receipt_body'):
        if not (isinstance(value[key], str) and value[key]):
            raise ControllerError('the attestation ' + key.replace('_', ' ') + ' is not a non-empty string')
        bodies[key] = value[key].encode('utf-8')
    reference = ev.text(value['audit_reference'], 'the audit reference', ev.URL_MAX)
    advisory = cand.closed(value['advisory_fetch'], ev.ADVISORY_KEYS, 'the advisory fetch')
    ev.text(advisory['tool'], 'the advisory tool')
    ev.utc(advisory['fetched_at'], 'the advisory fetch time')
    ev.sha256(advisory['log_sha256'], 'the advisory log digest')
    if not ev.is_kind(advisory['result'], ('clean',)):
        raise ControllerError('the advisory fetch result is not clean')
    if advisory['log_sha256'] != _sha(bodies['advisory_log']):
        raise ControllerError('the advisory fetch does not name its log')
    tutorials = value['tutorials']
    if not (isinstance(tutorials, list) and 0 < len(tutorials) <= TUTORIALS_MAX):
        raise ControllerError('the tutorials need exactly one entry per target')
    for entry in tutorials:
        cand.closed(entry, rehearse.TUTORIAL_KEYS, 'a tutorial')
        steps = entry['steps']
        if not (isinstance(steps, list) and 0 < len(steps) <= rehearse.TUTORIAL_STEPS_MAX):
            raise ControllerError('the tutorial steps are not 1 to ' + str(rehearse.TUTORIAL_STEPS_MAX) + ' entries')
    return {'tutorials': tutorials, 'advisory_fetch': dict(advisory),
            'audit': {'reference': reference, 'sha256': _sha(bodies['audit_body'])},
            'export_receipt_sha256': _sha(bodies['export_receipt_body'])}


def prepare_dispatch(data, classification, version, candidate_run_id, candidate_run_attempt,
                     candidate_artifact_id, approval_runs):
    """For the coordinator, before submission: the complete dispatch request of exactly `data`, which must
    be evidence already classified public. A document that does not fit refuses with every measured size;
    nothing is truncated, normalized or dropped to fit."""
    if not (isinstance(classification, str) and classification == 'public'):
        raise ControllerError('the attestation is not classified public')
    if not isinstance(data, bytes):
        raise ControllerError('the attestation is not bytes')
    packed = gzip.compress(data, compresslevel=9, mtime=0)
    text = base64.b64encode(packed).decode('ascii')
    inputs = {'version': version, 'candidate_run_id': str(candidate_run_id),
              'candidate_run_attempt': str(candidate_run_attempt), 'candidate_artifact_id': str(candidate_artifact_id),
              'attestation_gzip_b64': text, 'attestation_sha256': _sha(data),
              'approval_runs': json.dumps(approval_runs, sort_keys=True, separators=(',', ':'))}
    ref = 'refs/tags/candidate/v' + str(version)
    sizes = {'document': len(data), 'gzip': len(packed), 'base64': len(text),
             'dispatch': len(common.dispatch_request(ref, inputs))}
    if (sizes['document'] > common.DOCUMENT_MAX or sizes['gzip'] > common.GZIP_MAX
            or sizes['base64'] > common.BASE64_MAX or sizes['dispatch'] > common.DISPATCH_MAX):
        raise DoesNotFit(sizes)
    value = _document(data)
    attestation_facts(data)
    texts = list(_strings(value))
    if any(CREDENTIAL_RE.search(t) for t in texts):
        raise ControllerError('the attestation holds a credential')
    if any(p in t for t in texts for p in PRIVATE_PATHS):
        raise ControllerError('the attestation holds a private path')
    parsed = dispatch_inputs(inputs)
    if parsed['approval_runs'] != approval_runs or decode_attestation(text, inputs['attestation_sha256']) != data:
        raise ControllerError('the attestation does not survive as dispatch inputs')
    common.check_dispatch(ref, inputs)
    return {'ref': ref, 'inputs': inputs, 'sizes': sizes}


# ---------------------------------------------------------------------------------------------
# Run authentication (outside the lane, before the attestation is decoded)
# ---------------------------------------------------------------------------------------------

def authenticate_run(run, evidence_policy, version):
    """One E run attempt against the protected evidence policy: repository, pinned workflow, event,
    candidate branch, and both numeric actors in the coordinator allowlist (each attempt again)."""
    value = evidence_policy.value
    pinned = value['evidence_workflow']
    if run.repository_id != value['repository_id'] or run.head_repository_id != value['repository_id']:
        raise Refused(UNAUTHORIZED, 'the evidence run is not in rdy-core')
    if (run.workflow_id, run.path) != (pinned['id'], pinned['path']) or run.path != WORKFLOW_PATH:
        raise Refused(UNAUTHORIZED, 'the evidence run is not the pinned evidence workflow')
    if run.event != 'workflow_dispatch' or run.head_branch != 'candidate/v' + version:
        raise Refused(UNAUTHORIZED, 'the evidence run is not a workflow_dispatch on candidate/v' + version)
    elig._actors(run, evidence_policy, 'evidence run')
    return run


def card_reference(channel, receipt_sha256):
    """The future first-use archive location of a receipt in the enrolled channel. It claims no archive."""
    ev.sha256(receipt_sha256, 'the receipt digest')
    archive = channel['archive']
    reference = ('dynamodb://' + archive['account_id'] + '/' + archive['region'] + '/' + archive['table'] + '/'
                 + archive['pk_prefix'].replace('#', '%23') + receipt_sha256 + '/' + archive['sk'])
    ev.card_receipt_sha256({'kind': 'key_change', 'reference': reference, 'sha256': receipt_sha256}, channel)
    return reference


class Authenticated(object):
    def __init__(self, **fields):
        self.__dict__.update(fields)


def _conflicts(facts, version):
    """Reservation and publication state: an existing reservation, final tag or release refuses. A failed
    read refuses too; only an authenticated 404 is absence."""
    if facts.optional('ref', 'tags/reserve/v' + version) is not None:
        raise Refused(CONFLICT, 'v' + version + ' is already reserved')
    if facts.optional('ref', 'tags/v' + version) is not None:
        raise Refused(CONFLICT, 'the tag v' + version + ' already exists')
    releases = facts.call('releases')
    if any(r.tag_name == 'v' + version for r in releases):
        raise Refused(CONFLICT, 'a release of v' + version + ' already exists')
    if any(m >= elig._minor(version) for m in elig._published(releases)):
        raise Refused(CONFLICT, 'v' + version + ' is not newer than every published version')
    return releases


def _passed(job, commit, what):
    if (job.status, job.conclusion) != ('completed', 'success'):
        raise Refused(INCOMPLETE, 'the ' + what + ' did not conclude success')
    if job.head_sha != commit:
        raise Refused(MISMATCH, 'the ' + what + ' did not run on the release commit')
    return job


def _job_record(job):
    """E's `Job` from the exact API attempt and job."""
    return {'run_id': job.run_id, 'run_attempt': job.run_attempt, 'job_id': job.job_id, 'job_url': job.html_url,
            'conclusion': job.conclusion}


def _candidate(facts, parsed, version, commit):
    """The candidate run, its four jobs, its artifact and candidate.json."""
    run = facts.call('run_attempt', parsed['candidate_run_id'], parsed['candidate_run_attempt'])
    if run.path != elig.CANDIDATE_WORKFLOW or run.event not in elig.CANDIDATE_EVENTS:
        raise Refused(UNAUTHORIZED, 'the candidate run is not the release candidate workflow')
    if run.repository_id != ev.REPOSITORY_ID or run.head_repository_id != ev.REPOSITORY_ID:
        raise Refused(UNAUTHORIZED, 'the candidate run is not in rdy-core')
    if run.head_branch != 'candidate/v' + version:
        raise Refused(UNAUTHORIZED, 'the candidate run is not on candidate/v' + version)
    if run.head_sha != commit:
        raise Refused(MISMATCH, 'the candidate run did not run on the release commit')
    elig._concluded(run, 'candidate run')
    jobs = elig._all_jobs_passed(facts, run.run_id, run.run_attempt, 'candidate run')
    names = [j.name for j in jobs]
    for name in elig.CANDIDATE_JOBS:
        if names.count(name) != 1:
            raise Refused(INCOMPLETE, 'the candidate run has no single job ' + name)
    for job in jobs:
        _passed(job, commit, job.name + ' job')
    artifact = facts.call('artifact', parsed['candidate_artifact_id'], run.run_id)
    if artifact.name != cand.candidate_artifact_name(run.run_attempt):
        raise Refused(MISMATCH, 'the candidate artifact is not ' + cand.candidate_artifact_name(run.run_attempt))
    if artifact.head_sha != commit:
        raise Refused(MISMATCH, 'the candidate artifact is not from the release commit')
    if artifact.size > CANDIDATE_ZIP_MAX:
        raise Refused(LIMIT, 'the candidate artifact is larger than ' + str(CANDIDATE_ZIP_MAX) + ' bytes')
    record = elig._parse(cand.load_candidate, facts.call('artifact_member', artifact, elig.CANDIDATE_MEMBER,
                                                         ev.RECORD_MAX))
    if (record['version'], record['commit'], record['run_id'], record['run_attempt']) != (
            version, commit, run.run_id, run.run_attempt):
        raise Refused(MISMATCH, 'candidate.json does not name this candidate run at the release commit')
    return run, jobs, artifact, record


def authenticate(facts, run_id, run_attempt, inputs):
    """Job `authenticate-evidence` (and the sealer's first step): every authority check before any
    attestation byte is decoded. `facts` is read-only; S comes from the candidate ref, V from the input and
    Cargo.toml, the policy from S and main."""
    parsed = dispatch_inputs(inputs)
    version = parsed['version']
    wrapped = elig._Facts(common.ReadOnly(facts))
    main = elig._Main(wrapped)
    releases = _conflicts(wrapped, version)
    ref = wrapped.call('ref', 'tags/candidate/v' + version)
    if ref.object_type != 'commit':
        raise Refused(MISMATCH, 'the candidate ref does not resolve directly to a commit')
    commit = ref.object_sha
    main.require(commit, 'release commit')
    policy, evidence_policy = elig._policy_at(wrapped, commit)
    current_policy, current_evidence_policy = elig._policy_at(wrapped, main.head)
    if (policy.sha256, evidence_policy.sha256) != (current_policy.sha256, current_evidence_policy.sha256):
        raise Refused(MISMATCH, 'the protected policy changed after the release commit')
    run = authenticate_run(wrapped.call('run_attempt', run_id, run_attempt), evidence_policy, version)
    if run.head_sha != commit:
        raise Refused(MISMATCH, 'the evidence run did not run on the release commit')
    pinned = evidence_policy.value['evidence_workflow']
    if wrapped.file(pinned['path'], commit, gh.CONTENTS_MAX).blob_sha != \
            wrapped.file(pinned['path'], pinned['sha'], gh.CONTENTS_MAX).blob_sha:
        raise Refused(UNAUTHORIZED, 'the evidence workflow at the release commit is not its pinned implementation')
    cargo = wrapped.file('Cargo.toml', commit, elig.CARGO_MAX).data
    if cand.cargo_version(cargo.decode('utf-8', 'replace')) != version:
        raise Refused(MISMATCH, 'the Cargo version at the release commit is not ' + version)
    if wrapped.call('compare', policy.value['signer_workflow_sha'], commit).status != 'ahead':
        raise Refused(MISMATCH, 'the release commit does not descend from the signer commit')
    if policy.value['targets'] != list(cand.TARGETS):
        raise Refused(UNAUTHORIZED, 'the policy targets are not the two preview targets')
    if elig._minor(version) < int(elig.TAG_RE.fullmatch(policy.value['first_machine_tag']).group(1)):
        raise Refused(MISMATCH, 'v' + version + ' is before the first machine tag')
    if not elig.preview_declared(wrapped.file(elig.PREVIEW_SOURCE, commit, elig.SOURCE_FILE_MAX).data):
        raise Refused(MISMATCH, 'the release commit does not declare PREVIEW; this producer seals previews only')
    candidate_run, candidate_jobs, artifact, record = _candidate(wrapped, parsed, version, commit)
    return Authenticated(facts=wrapped, main=main, parsed=parsed, version=version, commit=commit, run=run,
                         releases=releases, policy=policy, evidence_policy=evidence_policy,
                         current_evidence_policy=current_evidence_policy, candidate_run=candidate_run,
                         candidate_jobs=candidate_jobs, artifact=artifact, record=record)


def check_attestation(auth):
    """After authentication only: the decoded attestation and its tutorials bound to the candidate."""
    data = decode_attestation(auth.parsed['attestation_gzip_b64'], auth.parsed['attestation_sha256'])
    facts = attestation_facts(data)
    facts['tutorials'] = rehearse.check_tutorials(facts['tutorials'], auth.record)
    return facts


# ---------------------------------------------------------------------------------------------
# Sealing (the one job in the release authority lane)
# ---------------------------------------------------------------------------------------------

def parse_rehearsal_receipt(data, version, commit, target, manifest_sha256, archive):
    """H's sanitized receipt of one native rehearsal. Returns its install record unchanged: the original
    checker report's digest, the six passed checks and zero skips. The receipt is never rehashed as a report."""
    try:
        _, value = ev.read(data, 'rehearsal receipt', ev.RECORD_MAX)
    except ControllerError as error:
        raise Refused(MISMATCH, 'the ' + target + ' rehearsal receipt: ' + str(error))
    where = 'the ' + target + ' rehearsal receipt'
    try:
        cand.closed(value, RECEIPT_KEYS, where)
        install = cand.closed(value['install'], RECEIPT_INSTALL_KEYS, where + ' install record')
    except ControllerError as error:
        raise Refused(MISMATCH, str(error))
    want = {'schema': 1, 'version': version, 'commit': commit, 'target': target, 'manifest_sha256': manifest_sha256,
            'archive': dict(archive), 'session': None, 'cleanup': 'destroyed'}
    for key, expected in sorted(want.items()):
        if value[key] != expected or type(value[key]) is not type(expected):
            raise Refused(MISMATCH, where + ' does not match ' + key.replace('_', ' '))
    if not (isinstance(install['report_sha256'], str) and HEX64_RE.fullmatch(install['report_sha256'])):
        raise Refused(MISMATCH, where + ' has no report digest')
    checks = install['checks']
    passed = [{'name': name, 'passed': True} for name in cand.CHECK_NAMES]
    if checks != passed or any(type(c.get('passed')) is not bool for c in checks):
        raise Refused(MISMATCH, 'the ' + target + ' install check did not pass all six checks')
    if not (type(install['skipped']) is int and install['skipped'] == 0):
        raise Refused(MISMATCH, 'the ' + target + ' install check skipped a check')
    return {'report_sha256': install['report_sha256'], 'checks': passed, 'skipped': 0}


def _rehash_candidate(auth):
    """The exact candidate ZIP against its service digest, and every archive rehashed from its bytes."""
    path = auth.facts.call('artifact_path', auth.artifact)
    archive, record, _ = rehearse.open_candidate(path, auth.artifact.digest, auth.record['manifest_sha256'])
    with archive:
        if record != auth.record:
            raise Refused(MISMATCH, 'the candidate artifact changed while sealing')
        for entry in record['artifacts']:
            if rehearse.hash_member(archive, entry['name'], cand.ARCHIVE_MAX) != (entry['sha256'], entry['size']):
                raise Refused(MISMATCH, 'the candidate archives do not match their manifest')


def _native(auth, run_id, run_attempt):
    """Both native jobs of this exact attempt and their sanitized receipts, read through the API."""
    facts, commit = auth.facts, auth.commit
    jobs = facts.call('attempt_jobs', run_id, run_attempt)
    if sorted(j.name for j in jobs) != sorted(JOB_NAMES.values()):
        raise Refused(INCOMPLETE, 'the evidence run attempt is not exactly its four jobs; rerun all jobs')
    by_name = {j.name: j for j in jobs}
    for job in jobs:
        if (job.run_id, job.run_attempt) != (run_id, run_attempt):
            raise Refused(MISMATCH, 'the ' + job.name + ' job is from another attempt; rerun all jobs')
    _passed(by_name[JOB_NAMES['authenticate-evidence']], commit, 'evidence authentication job')
    if by_name[JOB_NAMES['seal-release-evidence']].status == 'completed':
        raise Refused(MISMATCH, 'the sealing job already completed')
    artifacts = facts.call('run_artifacts', run_id)
    if any(a.name == artifact_name(run_id, run_attempt) for a in artifacts):
        raise Refused(CONFLICT, 'this run attempt already produced a release record')
    installs = {}
    for target in cand.TARGETS:
        job = _passed(by_name['Native evidence install (' + target + ')'], commit, target + ' native install job')
        named = [a for a in artifacts if a.name == receipt_artifact_name(target, run_attempt)]
        if len(named) != 1:
            raise Refused(INCOMPLETE, 'the ' + target + ' rehearsal receipt of attempt ' + str(run_attempt)
                          + ' is missing or repeated')
        artifact = facts.call('artifact', named[0].artifact_id, run_id)
        if artifact.head_sha != commit:
            raise Refused(MISMATCH, 'the ' + target + ' rehearsal receipt is not from the release commit')
        data = facts.call('artifact_member', artifact, receipt_member(target), ev.RECORD_MAX)
        entry = [f for f in auth.record['artifacts'] if f['name'] == cand.archive_name(auth.version, target)][0]
        install = parse_rehearsal_receipt(data, auth.version, commit, target, auth.record['manifest_sha256'], entry)
        installs[target] = dict(install, job=_job_record(job))
    return installs


def _security(facts, commit):
    """The six required jobs at S, from the newest completed run of their own workflow at S."""
    chosen = {}
    for path in sorted(set(elig.SECURITY_WORKFLOWS.values())):
        runs = []
        for run_id, attempt in facts.call('workflow_runs', path, commit):
            run = facts.call('run_attempt', run_id, attempt)
            if run.path != path or run.head_sha != commit:
                raise Refused(MISMATCH, 'a ' + path + ' run is not at the release commit')
            if run.event in elig.SECURITY_EVENTS and run.status == 'completed':
                runs.append(run)
        if not runs:
            raise Refused(INCOMPLETE, 'no completed ' + path + ' run at the release commit')
        run = max(runs, key=lambda r: r.run_id)
        elig._concluded(run, path + ' run')
        chosen[path] = facts.call('attempt_jobs', run.run_id, run.run_attempt)
    entries = []
    for name in ev.SECURITY_JOBS:
        named = [j for j in chosen[elig.SECURITY_WORKFLOWS[name]] if j.name == name]
        if len(named) != 1:
            raise Refused(INCOMPLETE, 'the ' + name + ' job is missing or repeated')
        entries.append({'name': name, 'job': _job_record(_passed(named[0], commit, name + ' job'))})
    return entries


def _transition_sha256(facts, policy, commit):
    """The SHA-256 of the exact transition JSON of the policy's first machine tag, from the protected records."""
    tag = policy.value['first_machine_tag']
    name = 'transition-' + tag + '.json'
    records = facts.file(elig.RECORDS_PATH, commit, elig.RECORDS_MAX).data
    if (name + ' ' + name + '.sig') not in records.decode('utf-8', 'replace').split('\n'):
        raise Refused(INCOMPLETE, 'the trust records hold no transition for ' + tag)
    return _sha(facts.file('release-keys/' + name, commit, TRANSITION_MAX).data)


def _cards(auth, scope):
    """The exception cards of completed founder approval runs, rebuilt through the unchanged consumer
    projection; then every kind the release needs must be present."""
    channel = auth.evidence_policy.value['approval_channel']
    authority = auth.current_evidence_policy.value['approval_channel']
    cards = []
    for entry in auth.parsed['approval_runs']:
        artifact = auth.facts.call('artifact', entry['artifact_id'], entry['run_id'])
        request = elig._parse(ev.parse_approval_request, auth.facts.call('artifact_member', artifact, approval.MEMBER,
                                                                        ev.REQUEST_MAX))
        kind = request.value['kind']
        if kind == 'revocation':
            raise Refused(UNAUTHORIZED, 'a revocation approval never grants release eligibility')
        result = approval.rebuild(auth.facts, auth.main, auth.evidence_policy, authority, entry['run_id'],
                                  entry['artifact_id'], kind)
        receipt = elig._parse(ev.parse_approval_receipt, result.receipt_bytes)
        card = {'kind': kind, 'reference': card_reference(channel, result.receipt_sha256),
                'sha256': result.receipt_sha256}
        elig._card_authority(card, channel, authority)
        elig._card_scope(kind, receipt, scope)
        cards.append(card)
    kinds = {c['kind'] for c in cards}
    for kind in elig._required_cards(auth.version, auth.policy, auth.releases, True):
        if kind not in kinds:
            raise Refused(UNAUTHORIZED, 'the release needs a ' + kind + ' card')
    return cards


def seal_bytes(value):
    """Canonical E, bounded and parsed by the unchanged consumer parser."""
    try:
        data = cand.canonical(value)
    except UnicodeEncodeError:
        raise ControllerError('the release record is not canonical JSON')
    if len(data) > RECORD_MAX:
        raise ControllerError('the release record is larger than ' + str(RECORD_MAX) + ' bytes')
    ev.parse_release_record(data)
    return data


def _recheck(facts, auth):
    """Fresh reads just before E is written: reservation and publication state, S on main, and the policy."""
    fresh = elig._Facts(common.ReadOnly(facts))
    main = elig._Main(fresh)
    _conflicts(fresh, auth.version)
    main.require(auth.commit, 'release commit')
    policy, evidence_policy = elig._policy_at(fresh, main.head)
    if (policy.sha256, evidence_policy.sha256) != (auth.policy.sha256, auth.evidence_policy.sha256):
        raise Refused(MISMATCH, 'the protected policy changed while sealing')


def seal(facts, run_id, run_attempt, inputs):
    """Job `seal-release-evidence`. Returns E's exact bytes; at most one E per successful run attempt."""
    auth = authenticate(facts, run_id, run_attempt, inputs)
    attestation = check_attestation(auth)
    version, commit, record = auth.version, auth.commit, auth.record
    _rehash_candidate(auth)
    installs = _native(auth, run_id, run_attempt)
    security = _security(auth.facts, commit)
    elig._read_alerts(auth.facts)
    raw = auth.facts.file('release/source.json', commit, ev.RECORD_MAX).data
    source = elig._parse(ev.parse_source, raw)
    if source.value['version'] != version:
        raise Refused(MISMATCH, 'release/source.json does not name v' + version)
    landing = source.value['design_landing']
    policy = auth.policy.value
    scope = {'repository': cand.REPOSITORY, 'version': version, 'commit': commit, 'design_landing': landing,
             'manifest_sha256': record['manifest_sha256'], 'artifacts': [dict(f) for f in record['artifacts']],
             'key_arn': policy['key_arn'], 'fingerprint': policy['fingerprint'],
             'first_machine_tag': policy['first_machine_tag'],
             'transition_sha256': _transition_sha256(auth.facts, auth.policy, commit), 'targets': list(policy['targets'])}
    cards = _cards(auth, scope)
    counts = {b['target']: b['fast_gate'] for b in record['builds']}
    native = {j.name: j for j in auth.candidate_jobs}
    targets = []
    for target in cand.TARGETS:
        job = _job_record(native['Native candidate (' + target + ')'])
        entry = [f for f in record['artifacts'] if f['name'] == cand.archive_name(version, target)][0]
        install = installs[target]
        targets.append({'target': target, 'archive': dict(entry), 'build': dict(job), 'package': dict(job),
                        'fast_gate': dict(counts[target], job=dict(job)),
                        'install': {'job': install['job'], 'report_sha256': install['report_sha256'],
                                    'checks': install['checks'], 'skipped': install['skipped']}})
    artifact = auth.artifact
    value = {
        'schema': 1, 'repository': cand.REPOSITORY, 'version': version, 'commit': commit, 'design_landing': landing,
        'source': {'notes_sha256': source.value['notes_sha256'], 'guide_sha256': source.value['guide_sha256'],
                   'source_json_sha256': source.sha256,
                   'export_receipt_sha256': attestation['export_receipt_sha256']},
        'candidate': {'ref': 'refs/tags/candidate/v' + version, 'run_id': auth.candidate_run.run_id,
                      'run_attempt': auth.candidate_run.run_attempt,
                      'artifact': {'id': artifact.artifact_id, 'name': artifact.name,
                                   'digest': 'sha256:' + artifact.digest, 'size': artifact.size},
                      'manifest_sha256': record['manifest_sha256'],
                      'artifacts': [dict(f) for f in record['artifacts']]},
        'targets': targets,
        'security': {'jobs': security, 'codeql_open_high_critical': 0,
                     'advisory_fetch': attestation['advisory_fetch']},
        'audit': attestation['audit'],
        'tutorials': attestation['tutorials'],
        'approval_basis': {'mode': 'founder_card' if cards else 'standing_preview', 'decision': ev.DECISION,
                           'policy_commit': commit, 'scope': scope, 'cards': cards},
    }
    data = seal_bytes(value)
    elig.check_source(common.ReadOnly(facts), data, version, commit)
    _recheck(facts, auth)
    return data


def summary(auth, attestation_sha256):
    """Nonsecret addresses and digests only: never an attestation body or a credential."""
    lines = ['version: ' + auth.version, 'commit: ' + auth.commit,
             'candidate run: ' + str(auth.candidate_run.run_id) + ' attempt ' + str(auth.candidate_run.run_attempt),
             'candidate artifact: ' + str(auth.artifact.artifact_id) + ' sha256:' + auth.artifact.digest,
             'manifest sha256: ' + auth.record['manifest_sha256'], 'attestation sha256: ' + attestation_sha256,
             'approval runs: ' + str(len(auth.parsed['approval_runs']))]
    return '## Release evidence\n\n<pre>\n' + approval._escape('\n'.join(lines)) + '\n</pre>\n'


# ---------------------------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------------------------

def _decimal_argument(text):
    if not (isinstance(text, str) and text.isascii() and DECIMAL_RE.fullmatch(text)):
        raise argparse.ArgumentTypeError('not a positive decimal')
    return int(text)


def parser():
    top = argparse.ArgumentParser(prog='evidence.py')
    commands = top.add_subparsers(dest='command', required=True)
    command = commands.add_parser('authenticate')
    command.add_argument('--event', required=True)
    command.add_argument('--summary')
    command = commands.add_parser('seal')
    command.add_argument('--event', required=True)
    command.add_argument('--out', required=True)
    command = commands.add_parser('dispatch')
    command.add_argument('--attestation', required=True)
    command.add_argument('--classification', required=True)
    command.add_argument('--version', required=True)
    for name in ('--candidate-run-id', '--candidate-run-attempt', '--candidate-artifact-id'):
        command.add_argument(name, type=_decimal_argument, required=True)
    command.add_argument('--approval-runs', required=True)
    return top


def event_inputs(path):
    """The dispatch inputs from the event file, as data. The event ref must be this candidate's tag."""
    event = common.read_event(path)
    inputs = event['inputs']
    version = inputs.get('version')
    if not (isinstance(version, str) and event.get('ref') == 'refs/tags/candidate/v' + version):
        raise ControllerError('the event is not a dispatch on its candidate tag')
    return inputs


def run(argv):
    args = parser().parse_args(argv[1:])
    if args.command == 'dispatch':
        with open(args.attestation, 'rb') as handle:
            data = handle.read(16 * common.DOCUMENT_MAX + 1)
        if len(data) > 16 * common.DOCUMENT_MAX:
            raise ControllerError('the attestation file is larger than ' + str(16 * common.DOCUMENT_MAX) + ' bytes')
        prepared = prepare_dispatch(data, args.classification, args.version, args.candidate_run_id,
                                    args.candidate_run_attempt, args.candidate_artifact_id,
                                    _approval_runs(args.approval_runs))
        sys.stdout.write(common.dispatch_request(prepared['ref'], prepared['inputs']).decode('utf-8'))
        return 0
    work = tempfile.mkdtemp(prefix='rdy-evidence-')
    try:
        facts = common.github(work)
        run_id, run_attempt = common.run_identity()
        inputs = event_inputs(args.event)
        if args.command == 'authenticate':
            auth = authenticate(facts, run_id, run_attempt, inputs)
            check_attestation(auth)
            text = summary(auth, auth.parsed['attestation_sha256'])
            if args.summary:
                with open(args.summary, 'a', encoding='utf-8') as handle:
                    handle.write(text)
            sys.stdout.write(text)
            return 0
        data = seal(facts, run_id, run_attempt, inputs)
        common.write_record(args.out, MEMBER, data, RECORD_MAX)
        sys.stdout.write('release record sha256: ' + _sha(data) + ' size: ' + str(len(data)) + '\n')
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(common.main_guard(lambda: run(_ARGV)))
