#!/usr/bin/env python3
"""The protected signer's entry point, at the reviewed revision H.

`sign.py claims` measures the OIDC identity: it requests one token for sts.amazonaws.com and reports only
the allowlisted nonsecret claims. It loads no adapter, controller or AWS tool and can never sign.

`sign.py live` runs one invocation of `controller.Controller` around the vendored adapter:

  admit -> the candidate bytes -> the sole OIDC and STS exchange -> open_session -> sign_preview, then
  retain_signature and authorization

The adapter is `release_sign.py`, unchanged; its digest is checked before its bytes run. `GuardedAws` wraps
its `Aws` at the `request` boundary, so GetPublicKey needs the reservation context and its readback, and
Sign needs the durable attempt's acknowledgement and one volatile permit. It sends Sign at most once and
never retries. A killed process records nothing and needs to record nothing: the attempt written before
Sign survives it. A later attempt recovers the real retained bytes and verifies them, or the version is
abandoned under D-KM1GB. An attempt finding phase RESERVED signs; any other phase can only recover.

The token and the session credentials live in temporary mode-0600 files. No child sees the job's tokens. No
ambient credential, config file or SSH agent is read. Every exit removes the session.

The live AWS and GitHub clients here are thin: every decision is the controller's. Failures use
`release controller: <reason>` on stderr and exit 1. Invalid arguments exit 2.
"""
import argparse
import base64
from dataclasses import dataclass
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import types
import urllib.parse
import urllib.request
import zipfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import candidate as cand  # noqa: E402
import evidence as ev  # noqa: E402
import github as gh  # noqa: E402

ControllerError = cand.ControllerError

ADAPTER_PATH = HERE / 'release_sign.py'
ADAPTER_SHA256 = '97e5e19171bd997830c2c565d12a52f80d1b24ac9fe62494cc11c1077d42d8ef'
ADAPTER_MAX = 1024 * 1024

AUDIENCE = 'sts.amazonaws.com'
ISSUER = 'https://token.actions.githubusercontent.com'
REPOSITORY_ID = '1403751388'
OWNER_ID = '32169767'
SIGNER_WORKFLOW = cand.REPOSITORY + '/.github/workflows/release-sign.yml'
ENVIRONMENT = 'release'
CANDIDATE_REF_RE = re.compile(r'refs/tags/candidate/v0\.[1-9][0-9]*\.0')
# Reported by claims mode. Nothing else from a token leaves it.
CLAIMS = ('iss', 'aud', 'sub', 'repository', 'repository_id', 'repository_owner_id', 'environment', 'job_workflow_ref',
          'job_workflow_sha', 'workflow_ref', 'ref', 'ref_type', 'ref_protected', 'sha', 'event_name')
TOKEN_MAX = 64 * 1024
NETWORK_DEADLINE = 60

# The reusable job's display name. GitHub shows it as `<caller job> / <this>`.
SIGN_JOB = 'Sign reserved manifest'
RELEASE_WORKFLOW = 'release.yml'
RELEASE_WORKFLOW_PATH = '.github/workflows/' + RELEASE_WORKFLOW
STOP_ARTIFACT = 'sign-stop-'
STOP_FILE = 'sign-stop.json'
EVIDENCE_FILE = 'sign-evidence.jsonl'
SIGNATURE_FILE = 'SHA256SUMS.sig'
AUTHORIZATION_FILE = 'authorization.json'
RECEIPT_FILE = 'sign-receipt.json'
STOP_KEYS = ('schema', 'version', 'tuple_sha256', 'run_id', 'run_attempt', 'job_id', 'stopped')
STOPPED = ('before_attempt', 'attempt_or_later')
RUNS_MAX = 256
ARTIFACT_MAX = 2 * cand.ARCHIVE_MAX + 2 * cand.RECORD_MAX
EVIDENCE_MAX = 1024 * 1024
EVIDENCE_NAMES = ('pending', 'oidc_requested', 'oidc_issued', 'sts_requested', 'sts_issued')
EVIDENCE_KEYS = ('grant_id', 'tuple_sha256', 'epoch', 'signer_workflow_sha', 'policy_commit', 'audience', 'jwt_iat',
                 'jwt_exp', 'duration_seconds', 'sts_expiration', 'requested_duration', 'role_max_session_duration')
ENROLLMENT_KEYS = ('account_id', 'region', 'table', 'table_id', 'genesis_sha256', 'authority_sha256',
                   'role_max_session_duration')
ROLE_MAX_RANGE = (900, 43200)
KEYS_DIR = 'release-keys/'
RECORD_FILE_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}')
KEYS_OPTIONAL = ('policy.json', 'evidence-policy.json', 'versions.txt', 'README.txt', 'root-allowed-signers')
KEY_FILE_MAX = ev.RECORD_MAX
SECRET_VALUE_RE = re.compile(r'[A-Za-z0-9/+=_.-]{1,4096}')
SEND_RE = b'Sending http request:'
AWS_ERROR_RE = re.compile(rb'An error occurred \(([A-Za-z]+)\) when calling')
CANCEL_REASONS_RE = re.compile(rb'\[([A-Za-z, ]+)\]\s*$')


class Interrupted(BaseException):
    """A SIGTERM or SIGHUP. It passes through every refusal handler to the final cleanup."""


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _canon(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode('utf-8') + b'\n'


def _write_secret(path, data):
    """A new file that only this user can read. It never replaces a file."""
    fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


# ---------------------------------------------------------------------------------------------
# The adapter
# ---------------------------------------------------------------------------------------------

def load_adapter(path=ADAPTER_PATH):
    """The vendored adapter's exact bytes run, only after their digest matches. No search path is involved."""
    try:
        with open(str(path), 'rb') as handle:
            data = handle.read(ADAPTER_MAX + 1)
    except OSError:
        raise ControllerError('the vendored adapter is unreadable')
    if _sha(data) != ADAPTER_SHA256:
        raise ControllerError('the vendored adapter is not its pinned bytes')
    module = types.ModuleType('release_sign')
    module.__file__ = str(path)
    exec(compile(data, str(path), 'exec'), module.__dict__)  # nosec: the bytes match the reviewed digest
    return module


# ---------------------------------------------------------------------------------------------
# The identity token
# ---------------------------------------------------------------------------------------------

def _plain_opener(request, timeout):
    return urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=timeout)


def request_oidc_token(env, audience, opener=None):
    """One request to the runner's token service. The token is returned, never logged."""
    url, bearer = env.get('ACTIONS_ID_TOKEN_REQUEST_URL'), env.get('ACTIONS_ID_TOKEN_REQUEST_TOKEN')
    if not (url and bearer and url.startswith('https://')):
        raise ControllerError('this job has no identity token service')
    joiner = '&' if '?' in url else '?'
    request = urllib.request.Request(url + joiner + urllib.parse.urlencode({'audience': audience}),
                                     headers={'Authorization': 'Bearer ' + bearer, 'Accept': 'application/json',
                                              'User-Agent': 'rdy-release-signer'})
    try:
        with (opener or _plain_opener)(request, timeout=NETWORK_DEADLINE) as stream:
            body = stream.read(TOKEN_MAX + 1)
    except (OSError, http.client.HTTPException):
        raise ControllerError('the identity token request failed')
    if len(body) > TOKEN_MAX:
        raise ControllerError('the identity token answer is too large')
    value = cand.loads(body, TOKEN_MAX)
    token = value.get('value') if isinstance(value, dict) else None
    if not isinstance(token, str):
        raise ControllerError('the identity token answer is malformed')
    return token


def token_claims(token):
    """The claims of a JWT from the runner's token service. STS, not this code, checks its signature."""
    parts = token.split('.')
    if len(parts) != 3 or not all(parts):
        raise ControllerError('the identity token is malformed')
    try:
        payload = base64.urlsafe_b64decode(parts[1] + '=' * (-len(parts[1]) % 4))
    except ValueError:
        raise ControllerError('the identity token is malformed')
    try:
        claims = cand.loads(payload, TOKEN_MAX)
    except ControllerError:
        raise ControllerError('the identity token is malformed')
    if not isinstance(claims, dict):
        raise ControllerError('the identity token is malformed')
    return claims


def run_claims(env, out, opener=None, stream=None):
    """Claims mode: one token, the allowlisted claims, no AWS and no signing path."""
    claims = token_claims(request_oidc_token(env, AUDIENCE, opener))
    report = {}
    for name in CLAIMS:
        value = claims.get(name)
        if not isinstance(value, str) or len(value) > 512:
            raise ControllerError('the identity token has no claim ' + name)
        report[name] = value
    if report['aud'] != AUDIENCE:
        raise ControllerError('the identity token is not for ' + AUDIENCE)
    data = _canon(report)
    cand.write_new(out, data, 0o600)
    (stream or sys.stdout).write(data.decode('utf-8'))
    return report


def check_identity_claims(claims, signer_sha):
    """The token must be this exact callee's, before it goes to STS. IAM checks the same facts again."""
    expected = {'iss': ISSUER, 'aud': AUDIENCE, 'repository_id': REPOSITORY_ID, 'repository_owner_id': OWNER_ID,
                'environment': ENVIRONMENT, 'job_workflow_ref': SIGNER_WORKFLOW + '@' + signer_sha,
                'job_workflow_sha': signer_sha, 'event_name': 'workflow_dispatch', 'ref_type': 'tag'}
    for name, value in expected.items():
        if claims.get(name) != value:
            raise ControllerError('the token claim ' + name + ' is not the qualified identity')
    if not (isinstance(claims.get('ref'), str) and CANDIDATE_REF_RE.fullmatch(claims['ref'])):
        raise ControllerError('the token claim ref is not the qualified identity')
    for name in ('iat', 'exp'):
        if not cand.is_int(claims.get(name), 1):
            raise ControllerError('the token claim ' + name + ' is not the qualified identity')


# ---------------------------------------------------------------------------------------------
# The guarded KMS client
# ---------------------------------------------------------------------------------------------

class GuardedAws(object):
    """The adapter's `client`. Every KMS request passes the controller first. The controller gives one
    invocation one attempt and one permit, and nothing here calls Sign again after a failure."""

    def __init__(self, inner, gate):
        self.inner = inner
        self.gate = gate
        self.sign_requested = False

    def _refuse(self, code, reason):
        import controller as ctl
        return ctl.Refused(getattr(ctl, code), reason)

    def _check_key(self, arn):
        release = self.gate.qualified
        if release is None:
            raise self._refuse('AUTHORITY', 'a KMS request needs the reservation context')
        if arn != release.key_arn:
            raise self._refuse('MISMATCH', 'a KMS request names another key')

    def request(self, operation, arn, payload=None):
        if operation == 'get-public-key':
            if payload is not None:
                raise self._refuse('MISMATCH', 'GetPublicKey takes no payload')
            self.gate.before_public_key()
            self._check_key(arn)
            try:
                return self.inner.request(operation, arn)
            except Exception:
                raise self._refuse('AUTHORITY', 'the KMS public key request failed') from None
        if operation != 'sign':
            raise self._refuse('MISMATCH', 'the only KMS requests are GetPublicKey and Sign')
        if not isinstance(payload, bytes):
            raise self._refuse('MISMATCH', 'Sign takes the signing preimage bytes')
        self._check_key(arn)
        self.sign_requested = True
        permit = self.gate.before_sign(_sha(payload))
        permit.consume()
        try:
            return self.inner.request('sign', arn, payload)
        except Exception:
            raise self._refuse('UNKNOWN', 'the Sign request has no known outcome') from None


@dataclass(frozen=True)
class Outcome(object):
    result: str
    signature: bytes
    authorization: bytes


def _read_signature(directory):
    fd = os.open(os.path.join(str(directory), SIGNATURE_FILE), os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as handle:
        return handle.read(cand.RECORD_MAX)


def _call_adapter(adapter, guard, directory, release):
    import controller as ctl
    try:
        return adapter.sign_preview(guard, directory, release.version, release.manifest_sha256, release.key_arn,
                                    release.fingerprint, release.first_machine_tag, controller=guard.gate)
    except (adapter.Refused, OSError, KeyError, ValueError, TypeError, AttributeError,
            subprocess.SubprocessError) as error:
        # Text from the adapter's own fixed refusals only. Anything else has no reason worth echoing.
        reason = str(error) if isinstance(error, adapter.Refused) else 'the adapter stopped'
        code = ctl.UNKNOWN if guard.sign_requested else ctl.MISMATCH
        raise ctl.Refused(code, reason) from None


def run_signer(controller, provider, table, adapter, guard, prepare):
    """One invocation, in the fixed order. `prepare(preliminary)` returns the candidate directory: its bytes
    are read before the credentials start their clock. Every refusal raises, and no step repeats."""
    import controller as ctl
    controller.admit()
    directory = prepare(controller.preliminary)
    controller.issue_credentials(provider)
    controller.open_session(table)
    release = controller.qualified
    try:
        phase = controller.journal_phase()
    except ctl.JournalUnavailable:
        raise ctl.Refused(ctl.UNKNOWN, 'the journal is unavailable')
    if phase == 'RESERVED':
        result = _call_adapter(adapter, guard, directory, release)
        if result != 'signed and verified':
            raise ctl.Refused(ctl.UNKNOWN, 'the signature in the candidate directory did not come from this attempt')
        try:
            signature = _read_signature(directory)
        except OSError:
            raise ctl.Refused(ctl.UNKNOWN, 'the returned signature is unreadable')
        controller.retain_signature(signature)
    else:
        retained = controller.recover(release.tuple_bytes)
        if not isinstance(retained, ctl.RetainedSignature):
            raise ctl.Refused(ctl.CONFLICT, 'recovery gave no retained signature')
        signature = retained.signature
        # The adapter takes its verified path over these bytes: it rehashes the manifest and both archives,
        # authenticates the key, and sends no Sign. It never overwrites a file.
        cand.write_new(os.path.join(str(directory), SIGNATURE_FILE), signature, 0o600)
        result = _call_adapter(adapter, guard, directory, release)
        if result != 'already verified':
            raise ctl.Refused(ctl.UNKNOWN, 'the retained signature was not the verified signature')
    return Outcome(result, signature, controller.authorization())


# ---------------------------------------------------------------------------------------------
# The AWS CLI
# ---------------------------------------------------------------------------------------------

def signing_env(environ, aws_bin, session):
    """The whole environment of the signer and its children. Named values only: the job's tokens, any
    ambient AWS setting and any SSH agent are left out. The credential files are this run's own."""
    session = Path(session)
    return {'PATH': str(aws_bin) + ':/usr/bin:/bin', 'HOME': str(session), 'TMPDIR': str(session), 'LANG': 'C.UTF-8',
            'AWS_SHARED_CREDENTIALS_FILE': str(session / 'aws-credentials'), 'AWS_CONFIG_FILE': str(session / 'aws-config'),
            'AWS_MAX_ATTEMPTS': '1', 'AWS_RETRY_MODE': 'standard', 'AWS_EC2_METADATA_DISABLED': 'true',
            'AWS_IGNORE_CONFIGURED_ENDPOINT_URLS': 'true', 'AWS_CLI_AUTO_PROMPT': 'off', 'AWS_PAGER': '',
            'AWS_STS_REGIONAL_ENDPOINTS': 'regional'}


class AwsCli(object):
    """The pinned CLI, by its full path, one try per call."""

    def __init__(self, binary, region, runner=None):
        self.binary = str(binary)
        self.region = region
        self.runner = runner or cand.run_bounded

    def run(self, arguments, deadline, stdin=None):
        """(status, stdout, stderr). A child that cannot run or outlives `deadline` raises ControllerError."""
        env = dict(os.environ, AWS_MAX_ATTEMPTS='1', AWS_RETRY_MODE='standard', AWS_PAGER='')
        seconds = str(max(1, int(deadline)))
        argv = [self.binary, '--region', self.region, '--no-cli-pager', '--output', 'json', '--cli-connect-timeout',
                seconds, '--cli-read-timeout', seconds] + list(arguments)
        return self.runner(argv, deadline=deadline, stdin=stdin if stdin is not None else b'', env=env)


class Identity(object):
    """The provider for `Controller.issue_credentials`: one token, one assumption, no refresh."""

    def __init__(self, env, role_arn, signer_sha, session, aws, role_max, run_id, run_attempt, opener=None):
        self._env = {k: env.get(k) for k in ('ACTIONS_ID_TOKEN_REQUEST_URL', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN')}
        self.role_arn = role_arn
        self.signer_sha = signer_sha
        self.session = Path(session)
        self.aws = aws
        self.role_max = role_max
        self.session_name = 'rdy-core-signer-' + str(run_id) + '-' + str(run_attempt)
        self.opener = opener
        self._requested = False
        self._assumed = False

    def request_oidc(self, audience):
        import controller as ctl
        if self._requested:
            raise ControllerError('this attempt already requested its one identity token')
        self._requested = True
        token = request_oidc_token(self._env, audience, self.opener)
        self._env = {}
        claims = token_claims(token)
        check_identity_claims(claims, self.signer_sha)
        return ctl.OidcToken(token=token, iat=claims['iat'], exp=claims['exp'])

    def _call_sts(self, path, duration_seconds):
        try:
            status, out, _ = self.aws.run(['sts', 'assume-role-with-web-identity', '--no-sign-request', '--role-arn',
                                           self.role_arn, '--role-session-name', self.session_name,
                                           '--web-identity-token', 'file://' + str(path), '--duration-seconds',
                                           str(duration_seconds)], NETWORK_DEADLINE)
        except ControllerError:
            raise ControllerError('the STS assumption failed')
        if status != 0:
            raise ControllerError('the STS assumption failed')
        return out

    def assume_role(self, token, duration_seconds):
        import controller as ctl
        if self._assumed:
            raise ControllerError('this attempt already made its one STS assumption')
        self._assumed = True
        path = self.session / 'web-identity-token'
        _write_secret(path, token.encode('utf-8'))
        try:
            out = self._call_sts(path, duration_seconds)
        finally:
            path.unlink()
        malformed = ControllerError('the STS answer is malformed')
        try:
            answer = cand.loads(out, cand.RECORD_MAX)
            credentials = answer['Credentials']
            assumed = answer['AssumedRoleUser']['Arn']
            values = [credentials[k] for k in ('AccessKeyId', 'SecretAccessKey', 'SessionToken')]
            expiry = datetime.datetime.fromisoformat(credentials['Expiration'].replace('Z', '+00:00'))
        except (ControllerError, KeyError, TypeError, ValueError, AttributeError):
            raise malformed from None
        if expiry.tzinfo is None or not all(isinstance(v, str) and SECRET_VALUE_RE.fullmatch(v) for v in values):
            raise malformed
        # `arn:aws:iam::<account>:role/<path/name>` assumes as `arn:aws:sts::<account>:assumed-role/<name>/<session>`.
        account, role_name = self.role_arn.split(':')[4], self.role_arn.rsplit('/', 1)[-1]
        if assumed != 'arn:aws:sts::' + account + ':assumed-role/' + role_name + '/' + self.session_name:
            raise ControllerError('the STS answer is not the requested role')
        text = '[default]\naws_access_key_id = {}\naws_secret_access_key = {}\naws_session_token = {}\n'.format(*values)
        _write_secret(self.session / 'aws-credentials', text.encode('ascii'))
        _write_secret(self.session / 'aws-config', b'')
        return ctl.StsReceipt(expiration=int(expiry.timestamp()), role_arn=self.role_arn,
                              requested_duration=duration_seconds, role_max_session_duration=self.role_max)

    def cleanup(self):
        for name in ('web-identity-token', 'aws-credentials', 'aws-config'):
            try:
                (self.session / name).unlink()
            except FileNotFoundError:
                pass


def _wire(value):
    if isinstance(value, bytes):
        return base64.b64encode(value).decode('ascii')
    if isinstance(value, dict):
        return {k: _wire(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_wire(v) for v in value]
    return value


class CliTable(object):
    """The journal's `table`, through the pinned CLI. One process per call, one try per process."""

    def __init__(self, aws, table):
        self.aws = aws
        self.table = table

    def _read(self, operation, stdin, timeout, arguments=()):
        import controller as ctl
        unavailable = ctl.JournalUnavailable('the journal is unavailable')
        argv = ['dynamodb', operation] + list(arguments)
        if stdin is not None:
            argv += ['--cli-input-json', 'file:///dev/stdin']
        try:
            status, out, _ = self.aws.run(argv, timeout, None if stdin is None else json.dumps(stdin).encode('utf-8'))
        except ControllerError:
            raise unavailable
        if status != 0:
            raise unavailable
        try:
            value = cand.loads(out, cand.OUTPUT_MAX, floats=True)
        except ControllerError:
            raise unavailable
        if not isinstance(value, dict):
            raise unavailable
        return value

    @staticmethod
    def _item(raw):
        import controller as ctl
        if raw is None:
            return None
        try:
            item = dict(raw)
            item['body'] = {'B': base64.b64decode(raw['body']['B'], validate=True)}
            return item
        except (KeyError, TypeError, ValueError):
            raise ctl.JournalUnavailable('the item is malformed') from None

    def _key(self, key):
        return {'pk': {'S': key[0]}, 'sk': {'S': key[1]}}

    def describe_table(self, arn, timeout):
        import controller as ctl
        table = self._read('describe-table', None, timeout, ('--table-name', arn)).get('Table')
        if not isinstance(table, dict):
            raise ctl.JournalUnavailable('the table description is malformed')
        return table

    def get_item(self, pk, sk, timeout):
        value = self._read('get-item', {'TableName': self.table, 'Key': self._key((pk, sk)), 'ConsistentRead': True},
                           timeout)
        return self._item(value.get('Item'))

    def transact_get_items(self, keys, timeout):
        import controller as ctl
        value = self._read('transact-get-items', {'TransactItems': [
            {'Get': {'TableName': self.table, 'Key': self._key(k)}} for k in keys]}, timeout)
        responses = value.get('Responses')
        if not isinstance(responses, list) or len(responses) != len(keys):
            raise ctl.JournalUnavailable('the snapshot is malformed')
        return [self._item(r.get('Item') if isinstance(r, dict) else {'bad': 1}) for r in responses]

    def transact_write_items(self, actions, token, timeout):
        """One process, one transaction. The CLI's own log counts the requests it really sent: a hidden retry
        shows as two, and no line proves none. The caller gets no fresh create from either."""
        import controller as ctl
        payload = json.dumps({'TransactItems': _wire(list(actions)), 'ClientRequestToken': token}).encode('utf-8')
        try:
            status, _, err = self.aws.run(['dynamodb', 'transact-write-items', '--cli-input-json', 'file:///dev/stdin',
                                           '--debug'], timeout, payload)
        except ControllerError:
            raise ctl.JournalOutcomeUnknown('the write outcome is unknown')
        if status == 0:
            return ctl.WriteAck(underlying_requests=err.count(SEND_RE))
        codes = AWS_ERROR_RE.findall(err)
        if codes and codes[-1] == b'TransactionCanceledException':
            reasons = CANCEL_REASONS_RE.search(err)
            names = [n.strip() for n in reasons.group(1).split(b',')] if reasons else []
            if b'ConditionalCheckFailed' in names and set(names) <= {b'ConditionalCheckFailed', b'None'}:
                raise ctl.JournalConditionFailed('a condition refused the write')
        raise ctl.JournalOutcomeUnknown('the write outcome is unknown')


# ---------------------------------------------------------------------------------------------
# Verifier, evidence and history
# ---------------------------------------------------------------------------------------------

class SshVerifier(object):
    """OpenSSH over the signer line that trust selection authenticated for this version."""

    def __init__(self, runner=None):
        self.runner = runner or cand.run_bounded

    def verify(self, signature, manifest, signer_line, principal, namespace):
        with tempfile.TemporaryDirectory() as folder:
            allowed, sig = Path(folder, 'allowed'), Path(folder, 'signature')
            allowed.write_text(signer_line + '\n', encoding='ascii')
            sig.write_bytes(signature)
            try:
                status, _, _ = self.runner(['ssh-keygen', '-Y', 'verify', '-f', str(allowed), '-I', principal, '-n',
                                            namespace, '-s', str(sig)], deadline=15, stdin=manifest, env=dict(os.environ))
            except ControllerError:
                return False
        return status == 0


class RunEvidence(object):
    """The issuance boundaries of this job, as nonsecret lines: in the job log, which GitHub holds, and in a
    file for this process and the receipt upload. A closed set of names and fields keeps a secret out."""

    def __init__(self, path, stream=None):
        self.path = Path(path)
        self.stream = stream or sys.stdout

    def record(self, holder, name, facts):
        if name not in EVIDENCE_NAMES:
            raise ControllerError('the run evidence has no boundary ' + name)
        for key, value in facts.items():
            if key not in EVIDENCE_KEYS or not isinstance(value, (str, int)) or isinstance(value, bool):
                raise ControllerError('the run evidence holds an unknown field')
        line = _canon({'run_id': holder.run_id, 'run_attempt': holder.run_attempt, 'job_id': holder.job_id,
                       'boundary': name, 'facts': dict(facts)})
        try:
            fd = os.open(str(self.path), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            with os.fdopen(fd, 'ab') as handle:
                handle.write(line)
                handle.flush()
                os.fsync(handle.fileno())
        except OSError:
            raise ControllerError('the run evidence is unavailable')
        self.stream.write('release evidence: ' + line.decode('utf-8'))
        self.stream.flush()

    def boundaries(self, holder):
        try:
            with open(str(self.path), 'rb') as handle:
                data = handle.read(EVIDENCE_MAX + 1)
        except FileNotFoundError:
            return []
        except OSError:
            raise ControllerError('the run evidence is unavailable')
        if len(data) > EVIDENCE_MAX:
            raise ControllerError('the run evidence is too large')
        rows = []
        for line in data.splitlines():
            value = cand.loads(line, EVIDENCE_MAX)
            if (value['run_id'], value['run_attempt'], value['job_id']) == (holder.run_id, holder.run_attempt, holder.job_id):
                rows.append((holder, value['boundary'], value['facts']))
        return rows


def is_signer_job(name):
    return name == SIGN_JOB or name.endswith(' / ' + SIGN_JOB)


def find_holder(facts, run_id, run_attempt):
    """This running job's numeric identity: the one signer job in progress in this run attempt."""
    import controller as ctl
    running = [j for j in facts.attempt_jobs(run_id, run_attempt) if is_signer_job(j.name) and j.status == 'in_progress']
    if len(running) != 1:
        raise ControllerError('this run attempt has no single running signer job')
    return ctl.Holder(run_id, run_attempt, running[0].job_id)


class GitHubHistory(object):
    """Every prior signer job of this version, from GitHub. A job's stop is `before_attempt` only with proof: it
    never ran, or its own receipt, which H writes as it exits, says so. Anything else is `unknown`, and the
    controller refuses on it. A failed list raises: it is never an empty history."""

    def __init__(self, facts):
        self.facts = facts

    def invocations(self, version, tuple_sha256):
        import controller as ctl
        rows = []
        for run_id, attempts in self.facts.release_runs(version):
            for attempt in range(1, attempts + 1):
                for job in self.facts.attempt_jobs(run_id, attempt):
                    if is_signer_job(job.name):
                        stopped = self._stopped(job, run_id, attempt, version, tuple_sha256)
                        rows.append(ctl.Invocation(run_id, attempt, job.job_id, stopped))
        return ctl.History(complete=True, invocations=tuple(rows))

    def _stopped(self, job, run_id, attempt, version, tuple_sha256):
        if (job.status, job.conclusion) == ('completed', 'skipped'):
            return 'before_attempt'
        found = [a for a in self.facts.run_artifacts(run_id) if a.name == STOP_ARTIFACT + str(attempt)]
        if len(found) != 1 or found[0].expired or found[0].head_sha != job.head_sha:
            return 'unknown'
        try:
            data = self.facts.artifact_member(found[0], STOP_FILE, cand.RECORD_MAX)
            receipt = cand.closed(cand.loads(data), STOP_KEYS, 'the stop receipt')
        except ControllerError:
            return 'unknown'
        if (receipt['schema'], receipt['run_id'], receipt['run_attempt']) != (1, str(run_id), str(attempt)) \
                or receipt['job_id'] not in (None, str(job.job_id)) or receipt['stopped'] not in STOPPED:
            return 'unknown'
        if (receipt['version'], receipt['tuple_sha256']) == (None, None):
            # It stopped before it knew a tuple, so it cannot have reached an attempt.
            return 'before_attempt' if receipt['stopped'] == 'before_attempt' else 'unknown'
        if (receipt['version'], receipt['tuple_sha256']) != (version, tuple_sha256) or receipt['job_id'] is None:
            return 'unknown'
        return receipt['stopped']


# ---------------------------------------------------------------------------------------------
# GitHub facts
# ---------------------------------------------------------------------------------------------

class LiveFacts(gh.GitHub):
    """`github.GitHub` plus the reads that eligibility and the controller name beyond its methods."""

    def __init__(self, transport, work, verifier, pause=None, runner=None):
        gh.GitHub.__init__(self, transport, pause)
        self.work = Path(work)
        self.verifier = str(verifier)
        self.runner = runner or cand.run_bounded
        self._downloads = {}

    def artifact_path(self, fact):
        """The artifact's ZIP, downloaded once and checked against the artifact-service digest."""
        path = self._downloads.get((fact.artifact_id, fact.digest))
        if path is None:
            path = self.work / ('artifact-' + str(fact.artifact_id) + '.zip')
            self.download_artifact(fact, path, ARTIFACT_MAX)
            self._downloads[(fact.artifact_id, fact.digest)] = path
        return path

    def artifact_member(self, fact, name, limit):
        try:
            with zipfile.ZipFile(str(self.artifact_path(fact))) as archive:
                return cand.read_member(archive, name, limit)
        except (zipfile.BadZipFile, KeyError, OSError):
            raise ControllerError('the artifact does not hold ' + name)

    def release_runs(self, version):
        """(run id, latest attempt) of every run of the release workflow on this candidate."""
        branch = urllib.parse.quote('candidate/v' + version, safe='')
        path = self.root + '/actions/workflows/' + RELEASE_WORKFLOW + '/runs?branch=' + branch
        runs = []
        for row in self._pages('workflow runs', path, 'workflow_runs', RUNS_MAX):
            if row.get('path') != RELEASE_WORKFLOW_PATH or row.get('head_branch') != 'candidate/v' + version \
                    or not cand.is_int(row.get('run_attempt'), 1):
                raise gh.Malformed('workflow runs', 'a run is not the release workflow on this candidate')
            runs.append((row['id'], row['run_attempt']))
        return runs

    def release_trust(self, version, policy_commit, signing):
        """`qa-release-trust select` over release-keys/ at the policy commit, read through the API."""
        folder = Path(tempfile.mkdtemp(dir=str(self.work)))
        try:
            os.chmod(str(folder), 0o700)
            self._keys_directory(folder, policy_commit)
            argv = [self.verifier, 'select', '--version', version, '--keys', str(folder), '--policy-commit', policy_commit]
            if signing:
                argv.append('--signing')
            status, out, err = self.runner(argv, deadline=cand.HELPER_DEADLINE, env=dict(os.environ))
        finally:
            shutil.rmtree(str(folder), ignore_errors=True)
        if status != 0:
            line = err.decode('utf-8', 'replace').strip().split('\n')[0][:512]
            raise ControllerError(line.replace('release trust: ', '', 1))
        return cand.loads(out)

    def _keys_directory(self, folder, sha):
        index = self.contents(KEYS_DIR + 'records.txt', sha, cand.RECORD_MAX).data
        (folder / 'records.txt').write_bytes(index)
        for line in index.decode('utf-8', 'replace').splitlines():
            names = line.split(' ')
            if len(names) != 2 or not all(RECORD_FILE_RE.fullmatch(n) for n in names):
                raise ControllerError('the release key index is malformed')
            for name in names:
                (folder / name).write_bytes(self.contents(KEYS_DIR + name, sha, KEY_FILE_MAX).data)
        for name in KEYS_OPTIONAL:
            try:
                (folder / name).write_bytes(self.contents(KEYS_DIR + name, sha, KEY_FILE_MAX).data)
            except gh.Absent:
                pass

    def _observed(self, kind, path):
        body = self._send('approval source', path, gh.BODY_MAX).body
        return {'kind': kind, 'sha256': _sha(body), 'bytes': len(body)}

    def approval_responses(self, run_id, artifact_id):
        """The nine {kind, sha256, bytes} entries of a founder approval run. Exact file and ZIP bytes for the
        kinds that eligibility compares with live facts, the digest and length of the API answer for the rest."""
        self.repository()
        artifact = self.artifact(artifact_id, run_id)
        request = ev.parse_approval_request(self.artifact_member(artifact, 'approval-request.json', ev.REQUEST_MAX))
        policy = self.contents('release-keys/evidence-policy.json', request.value['policy_commit'], ev.RECORD_MAX).data
        channel = ev.parse_evidence_policy(policy).value['approval_channel']
        run = self.run_attempt(run_id, 1)
        workflow = self.contents(run.path, run.head_sha, gh.CONTENTS_MAX).data
        source = self.contents(channel['workflow_path'], channel['implementation_sha'], gh.CONTENTS_MAX).data
        attempt = self.root + '/actions/runs/' + str(run_id)

        def exact(kind, data):
            return {'kind': kind, 'sha256': _sha(data), 'bytes': len(data)}
        entries = [
            self._observed('artifact', self.root + '/actions/artifacts/' + str(artifact_id)),
            exact('implementation_source', source),
            self._observed('job', attempt + '/attempts/1/jobs'),
            exact('protected_policy', policy),
            self._observed('repository', self.root),
            {'kind': 'request_archive', 'sha256': artifact.digest, 'bytes': artifact.size},
            self._observed('review_history', attempt + '/approvals'),
            self._observed('run_attempt', attempt + '/attempts/1'),
            exact('workflow_source', workflow)]
        return entries


# ---------------------------------------------------------------------------------------------
# Inputs and outputs
# ---------------------------------------------------------------------------------------------

def parse_enrollment(text, signer_sha):
    """The reviewed activation configuration, from the protected environment. Unknown keys refuse. H is not in
    it: H is this running revision. GENESIS, read from the table, must match these digests before any use."""
    import controller as ctl
    if not (isinstance(signer_sha, str) and cand.COMMIT_RE.fullmatch(signer_sha)):
        raise ControllerError('the signer revision is not a full commit')
    value = cand.loads(text.encode('utf-8'), cand.RECORD_MAX)
    if not isinstance(value, dict):
        raise ControllerError('the enrollment is malformed')
    cand.closed(value, ENROLLMENT_KEYS, 'the enrollment')
    if not (isinstance(value['account_id'], str) and re.fullmatch(r'[0-9]{12}', value['account_id'])
            and isinstance(value['region'], str) and re.fullmatch(r'us-[a-z]+-[1-9][0-9]*', value['region'])
            and isinstance(value['table'], str) and re.fullmatch(r'[A-Za-z0-9_.-]{3,255}', value['table'])
            and isinstance(value['table_id'], str) and len(value['table_id']) <= 64
            and all(isinstance(value[k], str) and cand.SHA256_RE.fullmatch(value[k])
                    for k in ('genesis_sha256', 'authority_sha256'))
            and cand.is_int(value['role_max_session_duration'], ROLE_MAX_RANGE[0])
            and value['role_max_session_duration'] <= ROLE_MAX_RANGE[1]):
        raise ControllerError('the enrollment is malformed')
    enrollment = ctl.Enrollment(account_id=value['account_id'], region=value['region'], table=value['table'],
                                table_id=value['table_id'], genesis_sha256=value['genesis_sha256'],
                                authority_sha256=value['authority_sha256'], signer_workflow_sha=signer_sha)
    return enrollment, value['role_max_session_duration']


def prepare_candidate(facts, request, release, directory):
    """Exactly the candidate's files, as data, in a new private directory. An artifact that holds anything else,
    such as a test signature or a test root, refuses. The adapter rehashes the same bytes before KMS."""
    artifact = facts.artifact(request.candidate_artifact_id, request.candidate_run_id)
    wanted = {name: cand.ARCHIVE_MAX for name, _, _ in release.artifacts}
    wanted['SHA256SUMS'] = cand.MANIFEST_MAX
    wanted['candidate.json'] = cand.RECORD_MAX
    directory = Path(directory)
    directory.mkdir(mode=0o700)
    try:
        with zipfile.ZipFile(str(facts.artifact_path(artifact))) as archive:
            if not cand.exact_members(archive, wanted):
                raise ControllerError('the candidate artifact is not exactly its archives, manifest and record')
            for name, digest, size in release.artifacts:
                if cand.extract_member(archive, name, directory / name, cand.ARCHIVE_MAX) != (digest, size):
                    raise ControllerError('the candidate archive ' + name + ' is not its reserved bytes')
            sums = cand.extract_member(archive, 'SHA256SUMS', directory / 'SHA256SUMS', cand.MANIFEST_MAX)
            if sums[0] != release.manifest_sha256:
                raise ControllerError('the candidate manifest is not its reserved bytes')
    except (zipfile.BadZipFile, OSError):
        raise ControllerError('the candidate artifact is unreadable')
    return str(directory)


def _openssh_version():
    try:
        done = subprocess.run(['ssh', '-V'], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              timeout=15, check=False)
    except (OSError, subprocess.SubprocessError):
        raise ControllerError('OpenSSH is unavailable')
    return done.stderr.decode('ascii', 'replace').strip().split(',')[0][:128]


def tool_versions(aws):
    """Nonsecret toolchain facts, kept in the receipt."""
    try:
        status, out, _ = aws.run(['--version'], 15)
    except ControllerError:
        raise ControllerError('the pinned AWS CLI does not run')
    if status != 0:
        raise ControllerError('the pinned AWS CLI does not run')
    return {'python': platform.python_version(), 'openssh': _openssh_version(),
            'aws_cli': out.decode('ascii', 'replace').split(' ')[0].replace('aws-cli/', '')[:32]}


def write_outputs(out, outcome, controller, versions):
    """Exactly three files for the transfer: the signature, the promotion proof and the receipt."""
    out = Path(out)
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    release, holder = controller.qualified, controller.holder
    receipt = dict(versions, schema=1, repository_id=REPOSITORY_ID, version=release.version, commit=release.commit,
                   tuple_sha256=release.tuple_sha256, manifest_sha256=release.manifest_sha256,
                   signer_workflow_sha=release.signer_workflow_sha, policy_commit=release.policy_commit,
                   run_id=str(holder.run_id), run_attempt=str(holder.run_attempt), job_id=str(holder.job_id),
                   result=outcome.result, signature_sha256=_sha(outcome.signature))
    cand.write_new(out / SIGNATURE_FILE, outcome.signature, 0o600)
    cand.write_new(out / AUTHORIZATION_FILE, outcome.authorization, 0o600)
    cand.write_new(out / RECEIPT_FILE, _canon(receipt), 0o600)


def write_stop(out, controller, environ=None):
    """How far this invocation got, written as it exits, for a later attempt's history. `before_attempt` means
    this process never wrote or sent an attempt, and it can no longer. A killed process writes nothing, which
    is `unknown` to the reader. Before the job's numeric identity is known, it names the run attempt, which
    has one signer job."""
    out = Path(out)
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    environ = environ or {}
    release = None if controller is None else (controller.qualified or controller.preliminary)
    holder = None if controller is None else controller.holder
    # The controller marks the attempt just before its write, so an unknown write outcome counts.
    stopped = 'attempt_or_later' if controller is not None and controller._attempted else 'before_attempt'
    cand.write_new(out / STOP_FILE, _canon({
        'schema': 1, 'version': None if release is None else release.version,
        'tuple_sha256': None if release is None else release.tuple_sha256,
        'run_id': environ.get('GITHUB_RUN_ID') if holder is None else str(holder.run_id),
        'run_attempt': environ.get('GITHUB_RUN_ATTEMPT') if holder is None else str(holder.run_attempt),
        'job_id': None if holder is None else str(holder.job_id), 'stopped': stopped}), 0o600)


# ---------------------------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------------------------

class SystemClock(object):
    @staticmethod
    def now():
        return int(time.time())


class _Run(object):
    controller = None


def _check_trusted(trusted, signer_sha):
    """This code's own checkout is exactly H."""
    status, out, _ = cand.run_bounded(['git', '-C', trusted, 'rev-parse', 'HEAD'], env=dict(os.environ))
    if status != 0 or out.decode('ascii', 'replace').strip() != signer_sha:
        raise ControllerError('the trusted checkout is not the signer revision')
    if Path(__file__).resolve() != (Path(trusted).resolve() / 'scripts/release/sign.py'):
        raise ControllerError('the running signer is not in the trusted checkout')


def _install_verifier(path, expected, work):
    """A private copy of the verifier built at H. Its digest is checked before anything runs it."""
    limit = 256 * cand.MIB
    actual, _ = cand.sha256_file(path, limit)
    if actual != expected:
        raise ControllerError('the trusted verifier is not the one built at the signer revision')
    target = Path(work) / 'qa-release-trust'
    shutil.copyfile(str(path), str(target))
    os.chmod(str(target), stat.S_IRWXU)
    if cand.sha256_file(target, limit)[0] != expected:
        raise ControllerError('the trusted verifier is not the one built at the signer revision')
    return target


def _live_stage(args, environ, run, work, out):
    if sys.version_info < (3, 10):
        raise ControllerError('the adapter needs Python 3.10 or later')
    import controller as ctl
    import eligibility as elig
    session = work / 'session'
    # From here no child sees a token, and no ambient AWS or SSH setting is read.
    os.environ.clear()
    os.environ.update(signing_env(environ, args.aws_bin, session))
    _check_trusted(args.trusted, args.signer_sha)
    adapter = load_adapter()
    verifier = _install_verifier(args.verifier, args.verifier_sha256, work)
    enrollment, role_max = parse_enrollment(environ.get('RDY_RELEASE_ENROLLMENT', ''), args.signer_sha)
    request = elig.parse_request(cand.loads(environ.get('RDY_RELEASE_REQUEST', '').encode('utf-8')))
    role_arn = environ.get('RDY_RELEASE_ROLE_ARN', '')
    run_id, attempt = environ.get('GITHUB_RUN_ID', ''), environ.get('GITHUB_RUN_ATTEMPT', '')
    if not (run_id.isdigit() and attempt.isdigit() and role_arn):
        raise ControllerError('the job environment is incomplete')
    reserve = environ.get('RDY_RESERVE_ARTIFACT_ID', '')
    if reserve and not reserve.isdigit():
        raise ControllerError('the reserve artifact id is not a number')
    facts = LiveFacts(gh.UrllibTransport(environ.get('GITHUB_TOKEN', '')), work, verifier)
    holder = find_holder(facts, int(run_id), int(attempt))
    aws = AwsCli(Path(args.aws_bin) / 'aws', enrollment.region)
    versions = tool_versions(aws)
    identity = Identity(environ, role_arn, args.signer_sha, session, aws, role_max, run_id, attempt)
    run.controller = controller = ctl.Controller(
        request, facts, enrollment, holder, SystemClock(), SshVerifier(), GitHubHistory(facts),
        RunEvidence(Path(out) / EVIDENCE_FILE), reserve_artifact_id=int(reserve) if reserve else None)
    guard = GuardedAws(adapter.Aws(), controller)
    outcome = run_signer(controller, identity, CliTable(aws, enrollment.table), adapter, guard,
                         lambda release: prepare_candidate(facts, request, release, work / 'candidate'))
    write_outputs(out, outcome, controller, versions)
    sys.stdout.write(_canon({'result': outcome.result, 'tuple_sha256': controller.qualified.tuple_sha256,
                             'signature_sha256': _sha(outcome.signature)}).decode('utf-8'))


def _on_signal(_number, _frame):
    raise Interrupted()


def live_main(args, environ, stage=None, stderr=None):
    """One live invocation. Every exit removes the session and leaves its stop receipt."""
    stderr = stderr or sys.stderr
    work, out = Path(args.work), Path(args.out)
    run = _Run()
    status = 1
    try:
        try:
            out.mkdir(mode=0o700, parents=True, exist_ok=True)
            work.mkdir(mode=0o700, parents=True)
            (work / 'session').mkdir(mode=0o700)
            (stage or _live_stage)(args, environ, run, work, out)
            status = 0
        except ControllerError as error:
            stderr.write('release controller: ' + str(error) + '\n')
        except Interrupted:
            stderr.write('release controller: signing outcome unknown: the signer was interrupted\n')
        except Exception:
            # A defect may hold anything in its text. The fixed line is all that leaves the job.
            stderr.write('release controller: signing outcome unknown: the signer stopped unexpectedly\n')
        try:
            write_stop(out, run.controller, environ)
        except (ControllerError, OSError):
            stderr.write('release controller: the stop receipt was not written\n')
    finally:
        shutil.rmtree(str(work), ignore_errors=True)
    return status


def main(argv=None):
    parser = argparse.ArgumentParser(prog='sign.py', description='The protected reusable signer.')
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    p = sub.add_parser('claims')
    p.add_argument('--out', type=Path, required=True)
    p = sub.add_parser('live')
    p.add_argument('--signer-sha', required=True)
    p.add_argument('--trusted', type=Path, required=True)
    p.add_argument('--aws-bin', type=Path, required=True)
    p.add_argument('--verifier', type=Path, required=True)
    p.add_argument('--verifier-sha256', required=True)
    p.add_argument('--work', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    args = parser.parse_args(argv)
    if args.command == 'claims':
        try:
            run_claims(os.environ, args.out)
        except ControllerError as error:
            sys.stderr.write('release controller: ' + str(error) + '\n')
            return 1
        except OSError:
            sys.stderr.write('release controller: a file operation failed\n')
            return 1
        return 0
    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGHUP, _on_signal)
    environ = dict(os.environ)
    return live_main(args, environ)


if __name__ == '__main__':
    sys.exit(main())
