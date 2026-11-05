"""The protected reusable signer, with injected fake AWS, verifier and transport functions only.

No test reaches the network, GitHub, AWS or a key. The vendored adapter runs for real over fake KMS
replies: its `verify` and `fingerprint` functions are injected, so no key or signature exists anywhere.
A "public key" and a "signature" here are fixed filler bytes. The journal is the controller tests'
`FakeTable`: it survives every restart, and each `controller()` is a fresh process.
"""
import ast
import base64
import dataclasses
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import warnings
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_release_controller as ctl  # noqa: E402

base = ctl.base
CONTROLLER, ELIGIBILITY, GITHUB, CANDIDATE, EVIDENCE = ctl.CONTROLLER, base.ELIGIBILITY, base.GITHUB, base.CANDIDATE, base.EVIDENCE
sys.modules['controller'] = CONTROLLER
Refused, ControllerError = CONTROLLER.Refused, base.ControllerError
ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / 'scripts/release'
SIGN = base.load('sign')
INSTALL = base.load('install_aws')
ADAPTER = SIGN.load_adapter()
WORKFLOW = (ROOT / '.github/workflows/release-sign.yml').read_text() if (
    ROOT / '.github/workflows/release-sign.yml').exists() else ''
ADAPTER_SHA256 = '97e5e19171bd997830c2c565d12a52f80d1b24ac9fe62494cc11c1077d42d8ef'
AWS_ZIP_SHA256 = '87e0e8fe3b73ff174eeef3b3ddf376fa4182a3e6af654dd989f1ffd2cef44a0c'
AWS_ZIP_SIZE = 69664276

KEY_ARN, FINGERPRINT, S, H = ctl.KEY_ARN, ctl.FINGERPRINT, ctl.S, ctl.H
RAW_PUBLIC = bytes(range(32))           # filler: no key exists
BLOB = ADAPTER.pubblob(RAW_PUBLIC)
RAW_SIGNATURE = b'\x07' * 64            # filler: no signature exists
ARMORED = ADAPTER.armor(BLOB, RAW_SIGNATURE)
TOKEN_SECRET = 'synthetic-oidc-secret-part'
GITHUB_SECRET = 'synthetic-github-token-value'
SESSION_SECRET = 'SyntheticSessionTokenValue'
ACCESS_SECRET = 'SyntheticSecretAccessKey'
SECRETS = (TOKEN_SECRET, GITHUB_SECRET, SESSION_SECRET, ACCESS_SECRET)


class Death(BaseException):
    """The fake process dies here; nothing below it runs. Only the remote state survives."""


class Rig(ctl.Rig):
    """A release whose archives are real bytes, so the real adapter can hash them."""

    def __init__(self):
        self.archive_bytes = {}

        def archive(version, target):
            name = 'rdy-' + version + '-preview-' + target + '.tar.gz'
            data = ('filler archive ' + target).encode()
            self.archive_bytes[name] = data
            return {'name': name, 'sha256': hashlib.sha256(data).hexdigest(), 'size': len(data)}

        with mock.patch.object(base, 'archive', archive):
            world = base.World()
        ctl.Rig.__init__(self, world)
        self.verifier.good.add(ARMORED)
        self.events = []
        self.directories = []
        self.git_ref = self.git.ref
        self.git.ref = self.logged_ref
        self.table_write, self.table_read = self.table.transact_write_items, self.table.transact_get_items
        self.table.transact_write_items = self.logged_write
        self.table.transact_get_items = self.logged_read

    def logged_ref(self, name):
        if name.startswith('tags/reserve/'):
            self.events.append('reservation readback')
        return self.git_ref(name)

    @staticmethod
    def sort_keys(keys):
        return sorted(k[1] if k[0].startswith(('TUPLE#', 'VERSION#')) else k[0].split('#')[0] for k in keys)

    def logged_write(self, actions, token, timeout):
        keys = [self.table._key(next(iter(a.values()))) for a in actions]
        ack = self.table_write(actions, token, timeout)
        self.events.append('write ' + '+'.join(self.sort_keys(keys)))
        return ack

    def logged_read(self, keys, timeout):
        answer = self.table_read(keys, timeout)
        self.events.append('read ' + '+'.join(self.sort_keys(keys)))
        return answer

    def prepare(self, release):
        """A fresh process's candidate directory."""
        directory = tempfile.mkdtemp(prefix='rsig6-')
        os.chmod(directory, 0o700)
        self.directories.append(directory)
        for name, data in self.archive_bytes.items():
            Path(directory, name).write_bytes(data)
        Path(directory, 'SHA256SUMS').write_bytes(self.manifest_bytes())
        return directory

    def clean(self):
        for directory in self.directories:
            shutil.rmtree(directory, ignore_errors=True)

    def run(self, ctrl=None, inner=None, guard=None, provider=None):
        ctrl = ctrl or self.controller()
        inner = inner or Inner(self)
        guard = guard or SIGN.GuardedAws(inner, ctrl)
        return SIGN.run_signer(ctrl, provider or self.provider(), self.table, ADAPTER, guard, self.prepare)

    def later(self, attempt=2, stopped='before_attempt', **changes):
        """A later attempt as a fresh process. The history names the earlier attempts."""
        earlier = tuple(CONTROLLER.Invocation(ctl.DISPATCH_RUN, n, ctl.SIGN_JOB + n - 1, stopped)
                        for n in range(1, attempt))
        self.history.answer = CONTROLLER.History(complete=True, invocations=earlier)
        return self.rerun(attempt, **changes)


class Inner(object):
    """The fake `Aws`: it records each request the guarded client lets through."""

    def __init__(self, rig, die_at=None, error=None, reply=None):
        self.rig, self.die_at, self.error, self.reply = rig, die_at, error, reply
        self.calls, self.payloads = [], []

    def request(self, operation, arn, payload=None):
        self.calls.append(operation)
        self.rig.events.append('kms ' + operation)
        if operation == 'get-public-key':
            return {'KeyId': arn, 'KeySpec': ADAPTER.SPEC, 'KeyUsage': 'SIGN_VERIFY', 'SigningAlgorithms': [ADAPTER.ALG],
                    'PublicKey': base64.b64encode(ADAPTER.DER_PREFIX + RAW_PUBLIC).decode()}
        self.payloads.append(payload)
        if self.die_at == 'after dispatch':
            raise Death(self.die_at)
        if self.error is not None:
            raise self.error
        if self.reply is not None:
            return self.reply
        return {'KeyId': arn, 'SigningAlgorithm': ADAPTER.ALG, 'Signature': base64.b64encode(RAW_SIGNATURE).decode()}

    @property
    def signs(self):
        return self.calls.count('sign')


class Base(unittest.TestCase):
    def setUp(self):
        self.rig = Rig()
        self.addCleanup(self.rig.clean)
        verify = mock.patch.object(ADAPTER, 'verify', self.fake_verify)
        fingerprint = mock.patch.object(ADAPTER, 'fingerprint', self.fake_fingerprint)
        self.verify_hook = None
        verify.start()
        fingerprint.start()
        self.addCleanup(verify.stop)
        self.addCleanup(fingerprint.stop)
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix='rsig6-'))
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def fake_verify(self, blob, manifest, sig):
        if self.verify_hook is not None:
            self.verify_hook()
        if blob != BLOB or manifest != self.rig.manifest_bytes() or sig not in self.rig.verifier.good:
            raise ADAPTER.Refused('OpenSSH rejected the returned signature or existing signature')

    @staticmethod
    def fake_fingerprint(blob):
        return FINGERPRINT if blob == BLOB else 'SHA256:' + 'B' * 43

    def refused(self, code, call, *args, **kwargs):
        with self.assertRaises(Refused) as caught:
            call(*args, **kwargs)
        self.assertEqual(caught.exception.code, code, str(caught.exception))
        return str(caught.exception)

    def assert_nothing_secret(self, data):
        text = data if isinstance(data, str) else data.decode('utf-8', 'replace')
        for secret in SECRETS:
            self.assertNotIn(secret, text)


def jwt(claims):
    def part(value):
        return base64.urlsafe_b64encode(json.dumps(value).encode()).rstrip(b'=').decode()
    return part({'alg': 'RS256'}) + '.' + part(claims) + '.' + TOKEN_SECRET


def claims(**changes):
    value = {'iss': 'https://token.actions.githubusercontent.com', 'aud': 'sts.amazonaws.com',
             'sub': 'repo:adrenickchrome/rdy-core:environment:release', 'repository': 'adrenickchrome/rdy-core',
             'repository_id': '1403751388', 'repository_owner_id': '32169767', 'environment': 'release',
             'job_workflow_ref': 'adrenickchrome/rdy-core/.github/workflows/release-sign.yml@' + H,
             'job_workflow_sha': H, 'workflow_ref': 'adrenickchrome/rdy-core/.github/workflows/release.yml@refs/tags/candidate/v0.43.0',
             'ref': 'refs/tags/candidate/v0.43.0', 'ref_type': 'tag', 'ref_protected': 'true', 'sha': S,
             'event_name': 'workflow_dispatch', 'iat': ctl.T0, 'exp': ctl.T0 + 300, 'jti': 'not-reported',
             'actor': 'not-reported', 'run_id': '1'}
    value.update(changes)
    return value


class FakeOpener(object):
    def __init__(self, token):
        self.token, self.requests = token, []

    def __call__(self, request, timeout):
        self.requests.append((request.full_url, dict(request.header_items()), timeout))
        return io.BytesIO(json.dumps({'value': self.token}).encode())


OIDC_ENV = {'ACTIONS_ID_TOKEN_REQUEST_URL': 'https://token.example.invalid/idtoken?x=1',
            'ACTIONS_ID_TOKEN_REQUEST_TOKEN': 'request-bearer-value'}


# ---------------------------------------------------------------------------------------------
# The workflow, read as text
# ---------------------------------------------------------------------------------------------

def job_text(name):
    """One job's lines of the reusable workflow."""
    match = re.search(r'^  ' + re.escape(name) + r':\n((?:(?:    .*)?\n)+)', WORKFLOW, re.M)
    if match is None:
        raise AssertionError('no job ' + name)
    return match.group(1)


def steps_of(job):
    return re.split(r'(?m)^      - ', job)[1:]


# ---------------------------------------------------------------------------------------------
# The adapter
# ---------------------------------------------------------------------------------------------

class AdapterPin(Base):
    def test_rsig6_adapter_digest_is_pinned(self):
        data = (SCRIPTS / 'release_sign.py').read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), ADAPTER_SHA256)
        self.assertEqual(SIGN.ADAPTER_SHA256, ADAPTER_SHA256)
        # The wrapper's digest is the digest of what the adapter sends to KMS.
        manifest = self.rig.manifest_bytes()
        self.assertEqual(hashlib.sha256(ADAPTER.preimage(manifest)).hexdigest(), CONTROLLER._preimage_digest(manifest))
        # The same principal, namespace and algorithm on both sides of the wrapper.
        self.assertEqual((ADAPTER.PRINCIPAL, ADAPTER.NS.decode()), (CONTROLLER.PRINCIPAL, CONTROLLER.NAMESPACE))
        # A changed copy never runs: the check is before any import or execution.
        marker = Path(self.tmp, 'ran')
        tampered = Path(self.tmp, 'release_sign.py')
        tampered.write_bytes(b'open(' + repr(str(marker)).encode() + b', "w").close()\n' + data)
        with self.assertRaises(ControllerError) as caught:
            SIGN.load_adapter(tampered)
        self.assertEqual(str(caught.exception), 'the vendored adapter is not its pinned bytes')
        self.assertFalse(marker.exists())
        self.assertNotIn('release_sign', sys.modules)
        missing = Path(self.tmp, 'absent.py')
        with self.assertRaises(ControllerError):
            SIGN.load_adapter(missing)
        # The running code is exactly the verified bytes, with its standalone CLI intact.
        self.assertIn('No standalone live signer', data.decode())
        # sign.py imports the adapter nowhere else.
        tree = ast.parse((SCRIPTS / 'sign.py').read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                self.assertNotIn('release_sign', [a.name for a in node.names])
            if isinstance(node, ast.ImportFrom):
                self.assertNotEqual(node.module, 'release_sign')

    def test_rsig6_first_machine_tag_stays_after_the_historical_range(self):
        # D-Z5ES3: the YubiKey owns v0.17.0..v0.41.0. The vendored adapter stays unchanged, and its floor is a
        # lower bound only. The authenticated trust selection is what refuses a cutover at or before v0.41.0.
        self.assertEqual(ADAPTER.LAST_KNOWN_HARDWARE_MINOR, 35)
        self.assertEqual(ELIGIBILITY.HISTORICAL_LAST_MINOR, 41)
        keys = (ROOT / 'crates/rdy-qa/src/release_keys.rs').read_text()
        self.assertIn('pub const HISTORICAL_LAST: u32 = 41;', keys)
        self.assertIn('the first machine epoch starts after v0.41.0', keys)
        # A trust answer that refuses reaches the adapter's caller before any KMS request.
        rig = self.rig
        rig.reserve_version()
        inner = Inner(rig)
        rig.w.errors['release_trust'] = ControllerError('the first machine epoch starts after v0.41.0')
        self.refused(CONTROLLER.INCOMPLETE, rig.run, rig.controller(), inner)
        self.assertEqual(inner.calls, [])


# ---------------------------------------------------------------------------------------------
# The wrapper: ordering and boundaries
# ---------------------------------------------------------------------------------------------

# Each boundary: where the process dies, the Sign calls so far, and what a fresh process gets.
BOUNDARIES = (
    ('before the attempt write', 0, 'refused'),
    ('after the attempt acknowledgement', 0, 'refused'),
    ('after dispatch', 1, 'refused'),
    ('after the response', 1, 'refused'),
    ('before signature persistence', 1, 'refused'),
    ('after the retention commit, before its acknowledgement', 1, 'recovered'),
    ('after retention', 1, 'recovered'),
)


class Boundaries(Base):
    def die_at(self, boundary, inner, ctrl):
        """Arms one process death. Returns the patches to hold during the run."""
        rig = self.rig

        def die():
            raise Death(boundary)
        patches = []
        if boundary == 'before the attempt write':
            answer = inner.request

            def request(operation, arn, payload=None):
                reply = answer(operation, arn, payload)
                rig.table.hook = die
                return reply
            inner.request = request
        elif boundary == 'after the attempt acknowledgement':
            patches.append(mock.patch.object(CONTROLLER.Permit, 'consume', side_effect=Death(boundary)))
        elif boundary == 'after dispatch':
            inner.die_at = boundary
        elif boundary == 'after the response':
            def hook():
                if inner.signs:
                    die()
            self.verify_hook = hook
        elif boundary == 'before signature persistence':
            answer = inner.request

            def request(operation, arn, payload=None):
                reply = answer(operation, arn, payload)
                if operation == 'sign':
                    rig.table.hook = die
                return reply
            inner.request = request
        elif boundary == 'after the retention commit, before its acknowledgement':
            write = rig.table.transact_write_items

            def committed(actions, token, timeout):
                ack = write(actions, token, timeout)
                if any('SIGNATURE' in json.dumps(rig.table._key(next(iter(a.values())))) for a in actions):
                    die()
                return ack
            rig.table.transact_write_items = committed
        else:
            patches.append(mock.patch.object(ctrl, 'authorization', side_effect=Death(boundary)))
        return patches

    def run_to_death(self, boundary):
        rig = self.rig
        rig.reserve_version()
        ctrl = rig.controller()
        inner = Inner(rig)
        guard = SIGN.GuardedAws(inner, ctrl)
        patches = self.die_at(boundary, inner, ctrl)
        for patch in patches:
            patch.start()
        try:
            with self.assertRaises(Death):
                rig.run(ctrl, inner, guard)
        finally:
            for patch in patches:
                patch.stop()
        self.verify_hook = None
        rig.table.hook = None
        return inner

    def check_every_process_death_boundary(self):
        for boundary, signs, after in BOUNDARIES:
            with self.subTest(boundary=boundary):
                self.rig = rig = Rig()
                self.addCleanup(rig.clean)
                first = self.run_to_death(boundary)
                self.assertEqual(first.signs, signs)
                attempt_kept = (rig.tuple_pk(), 'ATTEMPT') in rig.table.items
                self.assertEqual(attempt_kept, boundary != 'before the attempt write')
                # The reservation is never released.
                self.assertIn('tags/reserve/v' + rig.w.v, rig.w.refs)
                second = Inner(rig)
                if boundary == 'before the attempt write':
                    # Unproven, it stops; proved to have stopped first, the one Sign is still free.
                    ctrl = rig.later(2, 'unknown')
                    self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, second)
                    self.assertEqual(second.signs, 0)
                    ctrl = rig.later(3, 'before_attempt')
                    outcome = rig.run(ctrl, second)
                    self.assertEqual(outcome.result, 'signed and verified')
                    self.assertEqual(second.signs, 1)
                elif after == 'refused':
                    ctrl = rig.later(2, 'unknown')
                    message = self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, second)
                    self.assertTrue(message.startswith('signing outcome unknown: '))
                    self.assertEqual(second.signs, 0)
                    self.assertNotIn('sign', second.calls)
                    self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
                else:
                    ctrl = rig.later(2, 'unknown')
                    outcome = rig.run(ctrl, second)
                    self.assertEqual(outcome.result, 'already verified')
                    self.assertEqual(outcome.signature, ARMORED)
                    self.assertEqual(second.signs, 0)
                # The total never passes one for this tuple.
                self.assertLessEqual(first.signs + second.signs, 1)


class Wrapper(Boundaries):
    def test_rsig6_controller_wraps_every_kms_request(self):
        rig = self.rig
        rig.reserve_version()
        ctrl = rig.controller()
        inner = Inner(rig)
        guard = SIGN.GuardedAws(inner, ctrl)
        outcome = rig.run(ctrl, inner, guard)
        self.assertEqual(outcome.result, 'signed and verified')
        self.assertEqual(outcome.signature, ARMORED)
        self.assertEqual(inner.calls, ['get-public-key', 'sign'])
        self.assertEqual(inner.payloads, [ADAPTER.preimage(rig.manifest_bytes())])
        # The exact order: reservation readback, GetPublicKey, durable attempt and its exact readback, the
        # one Sign, then the real bytes and their receipt, read back.
        events = rig.events
        public_key, sign = events.index('kms get-public-key'), events.index('kms sign')
        self.assertLess(public_key, sign)
        self.assertIn('reservation readback', events[:public_key])
        attempt = next(i for i, e in enumerate(events) if e.startswith('write ') and 'ATTEMPT' in e)
        self.assertLess(public_key, attempt)
        self.assertLess(attempt, sign)
        self.assertEqual(events[attempt + 1], 'read ATTEMPT+RESERVATION')
        self.assertEqual(events[attempt + 2], 'kms sign')
        self.assertEqual(events[sign + 1], 'write ATTEMPT+COMPLETION+GRANT+RESERVATION+SIGNATURE')
        self.assertEqual(events[sign + 2], 'read ATTEMPT+COMPLETION+RESERVATION+SIGNATURE')
        tuple_pk = rig.tuple_pk()
        self.assertEqual(rig.table.items[(tuple_pk, 'SIGNATURE')], ARMORED)
        self.assertEqual(rig.row()['phase'], 'COMPLETE')
        # The attempt and the receipt carry the tuple and the run identity.
        attempt_record = json.loads(rig.table.items[(tuple_pk, 'ATTEMPT')])
        completion = json.loads(rig.table.items[(tuple_pk, 'COMPLETION')])
        self.assertEqual((attempt_record['tuple_sha256'], attempt_record['run_id'], attempt_record['job_id']),
                         (rig.tuple_sha(), str(ctl.DISPATCH_RUN), str(ctl.SIGN_JOB)))
        self.assertEqual((completion['tuple_sha256'], completion['signature_sha256']),
                         (rig.tuple_sha(), hashlib.sha256(ARMORED).hexdigest()))
        self.assertEqual(json.loads(outcome.authorization)['signature_sha256'], hashlib.sha256(ARMORED).hexdigest())
        # The same order holds through a process death at every boundary, with one fresh process after it.
        self.check_every_process_death_boundary()

    def test_rsig6_no_kms_request_outside_the_context(self):
        rig = self.rig
        rig.reserve_version()
        ctrl = rig.session()
        inner = Inner(rig)
        guard = SIGN.GuardedAws(inner, ctrl)
        payload = ADAPTER.preimage(rig.manifest_bytes())
        self.refused(CONTROLLER.AUTHORITY, guard.request, 'get-public-key', KEY_ARN)
        self.refused(CONTROLLER.AUTHORITY, guard.request, 'sign', KEY_ARN, payload)
        self.assertEqual(inner.calls, [])
        # A failed GetPublicKey is an authority failure before any attempt.
        ctrl3 = rig.later(3, 'before_attempt')
        ctrl3.admit()
        ctrl3.issue_credentials(rig.provider())
        ctrl3.open_session(rig.table)
        broken = Inner(rig)
        broken.request = mock.Mock(side_effect=ADAPTER.Refused('KMS request failed; inspect the trusted job/CloudTrail record'))
        with ctrl3.reserve(**rig.adapter_args()):
            self.refused(CONTROLLER.AUTHORITY, SIGN.GuardedAws(broken, ctrl3).request, 'get-public-key', KEY_ARN)
        # An operation outside the two the wrapper knows, a payload on GetPublicKey and another key.
        ctrl2 = rig.later(2, 'before_attempt')
        ctrl2.admit()
        ctrl2.issue_credentials(rig.provider())
        ctrl2.open_session(rig.table)
        guard2 = SIGN.GuardedAws(inner, ctrl2)
        with ctrl2.reserve(**rig.adapter_args()):
            self.refused(CONTROLLER.MISMATCH, guard2.request, 'encrypt', KEY_ARN, b'x')
            self.refused(CONTROLLER.MISMATCH, guard2.request, 'get-public-key', KEY_ARN, b'x')
            self.refused(CONTROLLER.MISMATCH, guard2.request, 'get-public-key', base.NEW_KEY_ARN)
            self.refused(CONTROLLER.MISMATCH, guard2.request, 'sign', KEY_ARN, 'text')
            self.assertEqual(inner.calls, [])

    def test_rsig6_get_public_key_needs_the_reservation_readback(self):
        def missing(rig, tag):
            rig.w.refs.pop(tag)

        def lightweight(rig, tag):
            rig.w.refs[tag] = ('commit', S)
        for change in (missing, lightweight):
            with self.subTest(change=change.__name__):
                rig = self.rig = Rig()
                self.addCleanup(rig.clean)
                rig.reserve_version()
                ctrl = rig.session()
                inner = Inner(rig)
                release = ctrl.qualified
                change(rig, 'tags/reserve/v' + rig.w.v)
                with self.assertRaises(Refused):
                    ADAPTER.sign_preview(SIGN.GuardedAws(inner, ctrl), rig.prepare(release), release.version,
                                         release.manifest_sha256, release.key_arn, release.fingerprint,
                                         release.first_machine_tag, controller=ctrl)
                self.assertEqual(inner.calls, [])

    def test_rsig6_sign_needs_the_durable_acknowledgement(self):
        for mode, code in (('unsent', CONTROLLER.UNKNOWN), ('lost_ack', CONTROLLER.UNKNOWN),
                           ('replay', CONTROLLER.UNKNOWN), ('down', CONTROLLER.UNKNOWN)):
            with self.subTest(mode=mode):
                rig = self.rig = Rig()
                self.addCleanup(rig.clean)
                rig.reserve_version()
                ctrl = rig.controller()
                inner = Inner(rig)
                guard = SIGN.GuardedAws(inner, ctrl)
                ctrl.admit()
                ctrl.issue_credentials(rig.provider())
                ctrl.open_session(rig.table)
                rig.table.fail_next('ATTEMPT', mode)
                release = ctrl.qualified
                self.refused(code, ADAPTER.sign_preview, guard, rig.prepare(release), release.version,
                             release.manifest_sha256, release.key_arn, release.fingerprint, release.first_machine_tag,
                             controller=ctrl)
                self.assertEqual(inner.signs, 0)
                self.assertEqual(inner.calls, ['get-public-key'])
                # The guard never asks again, in this process or through a retry.
                with ctrl.reserve(**rig.adapter_args()):
                    self.refused(CONTROLLER.UNKNOWN, guard.request, 'sign', KEY_ARN, ADAPTER.preimage(rig.manifest_bytes()))
                self.assertEqual(inner.signs, 0)
        # A wrong preimage gets no attempt at all. The wrapper hashes what the adapter sends.
        rig = self.rig = Rig()
        self.addCleanup(rig.clean)
        rig.reserve_version()
        ctrl = rig.session()
        inner = Inner(rig)
        guard = SIGN.GuardedAws(inner, ctrl)
        with ctrl.reserve(**rig.adapter_args()):
            self.refused(CONTROLLER.MISMATCH, guard.request, 'sign', KEY_ARN, ADAPTER.preimage(b'other manifest\n'))
        self.assertEqual(inner.signs, 0)
        self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)

# ---------------------------------------------------------------------------------------------
# Unknown outcomes
# ---------------------------------------------------------------------------------------------

class Unknown(Boundaries):
    def test_rsig6_unknown_sign_outcome_never_retries(self):
        replies = (
            ('the CLI failed', dict(error=ADAPTER.Refused('KMS request failed; inspect the trusted job/CloudTrail record'))),
            ('a timeout', dict(error=subprocess.TimeoutExpired('aws', 45))),
            ('malformed output', dict(error=ValueError('not JSON'))),
            ('another key', dict(reply={'KeyId': base.NEW_KEY_ARN, 'SigningAlgorithm': ADAPTER.ALG,
                                        'Signature': base64.b64encode(RAW_SIGNATURE).decode()})),
            ('another algorithm', dict(reply={'KeyId': KEY_ARN, 'SigningAlgorithm': 'ED25519_PH_SHA_512',
                                              'Signature': base64.b64encode(RAW_SIGNATURE).decode()})),
            ('a short signature', dict(reply={'KeyId': KEY_ARN, 'SigningAlgorithm': ADAPTER.ALG,
                                              'Signature': base64.b64encode(b'x').decode()})),
            ('no signature', dict(reply={'KeyId': KEY_ARN, 'SigningAlgorithm': ADAPTER.ALG})),
            ('bad bytes', dict(reply={'KeyId': KEY_ARN, 'SigningAlgorithm': ADAPTER.ALG,
                                      'Signature': base64.b64encode(b'\x08' * 64).decode()})),
        )
        for name, shape in replies:
            with self.subTest(name=name):
                self.rig = rig = Rig()
                self.addCleanup(rig.clean)
                rig.reserve_version()
                ctrl = rig.controller()
                inner = Inner(rig, **shape)
                guard = SIGN.GuardedAws(inner, ctrl)
                message = self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, inner, guard)
                self.assertTrue(message.startswith('signing outcome unknown: '))
                self.assertEqual(inner.signs, 1)
                self.assertTrue(guard.sign_requested)
                # No wrapper retry: the same guard refuses, with nothing sent.
                with ctrl.reserve(**rig.adapter_args()):
                    self.refused(CONTROLLER.UNKNOWN, guard.request, 'sign', KEY_ARN, ADAPTER.preimage(rig.manifest_bytes()))
                self.assertEqual(inner.signs, 1)
                # Nothing was retained, the reservation stays, and a fresh process refuses with no Sign.
                self.assertNotIn((rig.tuple_pk(), 'SIGNATURE'), rig.table.items)
                self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
                fresh = Inner(rig)
                self.refused(CONTROLLER.UNKNOWN, rig.run, rig.later(2, 'unknown'), fresh)
                self.assertEqual(fresh.signs, 0)
        # A killed process leaves the same state as a failed one: every boundary, and no second Sign.
        self.check_every_process_death_boundary()

    def test_rsig6_lost_retention_recovers_or_stops_with_no_second_sign(self):
        for mode, expected in (('lost_ack', 'already verified'), ('unsent', None), ('down', None)):
            with self.subTest(mode=mode):
                self.rig = rig = Rig()
                self.addCleanup(rig.clean)
                rig.reserve_version()
                ctrl = rig.controller()
                inner = Inner(rig)
                rig.table.fail_next('SIGNATURE', mode)
                self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, inner)
                self.assertEqual(inner.signs, 1)
                fresh = Inner(rig)
                later = rig.later(2, 'unknown')
                if expected:
                    self.assertEqual(rig.run(later, fresh).result, expected)
                else:
                    self.refused(CONTROLLER.UNKNOWN, rig.run, later, fresh)
                self.assertEqual(fresh.signs, 0)

    def test_rsig6_unavailable_history_fails_closed(self):
        rig = self.rig
        rig.reserve_version()
        rig.session()  # the row exists; this attempt stopped before its Sign attempt
        for attempt, change in ((2, lambda: setattr(rig.history, 'error', ControllerError('the history read failed'))),
                                (3, lambda: setattr(rig.history, 'answer', CONTROLLER.History(complete=False, invocations=())))):
            ctrl = rig.later(attempt)
            change()
            inner = Inner(rig)
            message = self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, inner)
            self.assertIn('prior invocation history', message)
            self.assertEqual(inner.signs, 0)
            self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
            rig.history.error = None
            # It refused before its attempt, so its receipt can say so.
            out = Path(self.tmp, 'stop-' + str(attempt))
            SIGN.write_stop(out, ctrl)
            self.assertEqual(json.loads((out / 'sign-stop.json').read_text())['stopped'], 'before_attempt')
        # An unknown attempt write is not before the attempt.
        ctrl = rig.later(4)
        rig.table.fail_next('ATTEMPT', 'lost_ack')
        self.refused(CONTROLLER.UNKNOWN, rig.run, ctrl, Inner(rig))
        SIGN.write_stop(Path(self.tmp, 'stop-4'), ctrl)
        self.assertEqual(json.loads(Path(self.tmp, 'stop-4', 'sign-stop.json').read_text())['stopped'], 'attempt_or_later')
        # An unavailable journal before the session: no attempt and no KMS.
        rig = self.rig = Rig()
        self.addCleanup(rig.clean)
        rig.reserve_version()
        ctrl = rig.controller()
        rig.table.unavailable = True
        inner = Inner(rig)
        self.refused(CONTROLLER.AUTHORITY, rig.run, ctrl, inner)
        self.assertEqual(inner.calls, [])

    def test_rsig6_aws_max_attempts_stays_one(self):
        replies = [mock.Mock(returncode=1, stdout=b'', stderr=b'throttled')]
        with mock.patch.object(ADAPTER.subprocess, 'run', side_effect=replies) as run:
            with self.assertRaises(ADAPTER.Refused):
                ADAPTER.Aws().request('sign', KEY_ARN, b'preimage')
        self.assertEqual(run.call_count, 1)
        self.assertEqual(run.call_args.kwargs['env']['AWS_MAX_ATTEMPTS'], '1')
        self.assertEqual(run.call_args.kwargs['input'], b'preimage')
        self.assertLessEqual(run.call_args.kwargs['timeout'], CONTROLLER.KMS_MAX)
        # sign.py never replaces the setting in the process environment.
        env = SIGN.signing_env({'PATH': '/ambient'}, Path('/w/aws/bin'), Path('/w/session'))
        self.assertEqual(env['AWS_MAX_ATTEMPTS'], '1')
        self.assertEqual(env['AWS_RETRY_MODE'], 'standard')


# ---------------------------------------------------------------------------------------------
# Recovery
# ---------------------------------------------------------------------------------------------

class Retry(Base):
    def test_rsig6_retry_reuses_verified_signature(self):
        rig = self.rig
        rig.reserve_version()
        first = Inner(rig)
        rig.run(rig.controller(), first)
        self.assertEqual(first.signs, 1)
        # A later attempt: fresh process, the same journal. The retained bytes come back and the adapter takes
        # its verified path, with GetPublicKey only and the no-overwrite install intact.
        second = Inner(rig)
        ctrl = rig.later(2, 'unknown')
        directory = {}

        def prepare(release):
            directory['path'] = rig.prepare(release)
            return directory['path']
        outcome = SIGN.run_signer(ctrl, rig.provider(), rig.table, ADAPTER, SIGN.GuardedAws(second, ctrl), prepare)
        self.assertEqual(outcome.result, 'already verified')
        self.assertEqual(outcome.signature, ARMORED)
        self.assertEqual(second.calls, ['get-public-key'])
        self.assertEqual(Path(directory['path'], 'SHA256SUMS.sig').read_bytes(), ARMORED)
        self.assertEqual(json.loads(outcome.authorization)['signature_sha256'], hashlib.sha256(ARMORED).hexdigest())
        # The retained bytes must verify: a retained signature that stops verifying is never used or replaced.
        rig.verifier.good.discard(ARMORED)
        third = Inner(rig)
        self.refused(CONTROLLER.UNKNOWN, rig.run, rig.later(3, 'unknown'), third)
        self.assertEqual(third.signs, 0)
        rig.verifier.good.add(ARMORED)
        # A stale file in the candidate directory is never overwritten or preferred.
        fourth = Inner(rig)
        ctrl = rig.later(4, 'unknown')

        def stale(release):
            path = rig.prepare(release)
            Path(path, 'SHA256SUMS.sig').write_bytes(b'stale')
            return path
        with self.assertRaises(ControllerError) as caught:
            SIGN.run_signer(ctrl, rig.provider(), rig.table, ADAPTER, SIGN.GuardedAws(fourth, ctrl), stale)
        self.assertEqual(str(caught.exception), 'the file SHA256SUMS.sig already exists')
        self.assertEqual(fourth.signs, 0)
        # The journal keeps the first bytes.
        self.assertEqual(rig.table.items[(rig.tuple_pk(), 'SIGNATURE')], ARMORED)

    def test_rsig6_a_new_signing_refuses_a_signature_it_did_not_request(self):
        rig = self.rig
        rig.reserve_version()
        inner = Inner(rig)

        def planted(release):
            path = rig.prepare(release)
            Path(path, 'SHA256SUMS.sig').write_bytes(ARMORED)
            return path
        ctrl = rig.controller()
        message = self.refused(CONTROLLER.UNKNOWN, SIGN.run_signer, ctrl, rig.provider(), rig.table, ADAPTER,
                               SIGN.GuardedAws(inner, ctrl), planted)
        self.assertIn('did not come from this attempt', message)
        self.assertEqual(inner.signs, 0)
        self.assertNotIn((rig.tuple_pk(), 'SIGNATURE'), rig.table.items)

    def test_rsig6_abandoned_or_published_never_signs(self):
        rig = self.rig
        rig.reserve_version()
        rig.table.seed('ABANDONMENT#' + str(ctl.REPO_ID) + '#' + rig.w.v + '#' + rig.tuple_sha(), 'RECORD', b'{}\n')
        inner = Inner(rig)
        self.refused(CONTROLLER.CONFLICT, rig.run, rig.controller(), inner)
        self.assertEqual(inner.signs, 0)
        # A published release gets no credentials, journal session or KMS request from the signer.
        self.rig = rig = Rig()
        self.addCleanup(rig.clean)
        rig.reserve_version()
        rig.publish()
        inner, provider = Inner(rig), rig.provider()
        before = dict(rig.table.items)
        self.refused(CONTROLLER.CONFLICT, rig.run, rig.controller(), inner, None, provider)
        self.assertEqual(inner.calls, [])
        self.assertEqual((provider.oidc, provider.sts), ([], []))
        self.assertEqual(rig.table.items, before)

    def test_rsig6_arguments_come_from_the_qualified_release(self):
        rig = self.rig
        rig.reserve_version()
        ctrl = rig.controller()
        inner = Inner(rig)
        seen = {}
        real = ADAPTER.sign_preview

        def spy(client, directory, version, manifest_sha256, arn, pin, first_machine_tag, *, controller):
            seen.update(version=version, manifest_sha256=manifest_sha256, arn=arn, pin=pin,
                        first_machine_tag=first_machine_tag, client=client, controller=controller)
            return real(client, directory, version, manifest_sha256, arn, pin, first_machine_tag, controller=controller)
        guard = SIGN.GuardedAws(inner, ctrl)
        with mock.patch.object(ADAPTER, 'sign_preview', spy):
            rig.run(ctrl, inner, guard)
        release = ctrl.qualified
        self.assertEqual(seen, dict(version=release.version, manifest_sha256=release.manifest_sha256,
                                    arn=release.key_arn, pin=release.fingerprint,
                                    first_machine_tag=release.first_machine_tag, client=guard, controller=ctrl))


# ---------------------------------------------------------------------------------------------
# Claims-only mode
# ---------------------------------------------------------------------------------------------

class Claims(Base):
    def test_rsig6_claims_mode_never_calls_sign(self):
        out = Path(self.tmp, 'claims.json')
        opener = FakeOpener(jwt(claims()))
        stdout = io.StringIO()
        report = SIGN.run_claims(OIDC_ENV, out, opener, stdout)
        # Exactly the allowlist: no other claim, the signature or any token.
        self.assertEqual(sorted(report), sorted(SIGN.CLAIMS))
        self.assertEqual(sorted(SIGN.CLAIMS), sorted((
            'iss', 'aud', 'sub', 'repository', 'repository_id', 'repository_owner_id', 'environment', 'job_workflow_ref',
            'job_workflow_sha', 'workflow_ref', 'ref', 'ref_type', 'ref_protected', 'sha', 'event_name')))
        written = out.read_bytes()
        self.assertEqual(json.loads(written), report)
        self.assertTrue(written.endswith(b'\n'))
        for data in (written.decode(), stdout.getvalue()):
            self.assert_nothing_secret(data)
            self.assertNotIn('not-reported', data)
            self.assertNotIn('request-bearer-value', data)
        self.assertEqual(stat.S_IMODE(out.stat().st_mode), 0o600)
        # The request: audience sts.amazonaws.com and the request token, once.
        (url, headers, timeout), = opener.requests
        self.assertIn('audience=sts.amazonaws.com', url)
        self.assertEqual(headers.get('Authorization'), 'Bearer request-bearer-value')
        self.assertLessEqual(timeout, 60)
        # A wrong audience, a missing claim, a broken token or no request URL refuses and reports nothing.
        for name, token, env in (
                ('audience', jwt(claims(aud='other')), OIDC_ENV),
                ('claim', jwt({k: v for k, v in claims().items() if k != 'ref_protected'}), OIDC_ENV),
                ('shape', 'not-a-token', OIDC_ENV),
                ('url', jwt(claims()), {}),
                ('http', jwt(claims()), dict(OIDC_ENV, ACTIONS_ID_TOKEN_REQUEST_URL='http://token.example.invalid/x'))):
            with self.subTest(name=name):
                target = Path(self.tmp, name + '.json')
                with self.assertRaises(ControllerError) as caught:
                    SIGN.run_claims(env, target, FakeOpener(token), io.StringIO())
                self.assert_nothing_secret(str(caught.exception))
                self.assertFalse(target.exists())
        # In a fresh interpreter, claims mode loads no adapter, controller or eligibility and no AWS tool.
        script = ('import sys, io, json\nsys.path.insert(0, ' + repr(str(SCRIPTS)) + ')\nimport sign\n'
                  'class O:\n'
                  '    def __call__(self, request, timeout):\n'
                  '        return io.BytesIO(json.dumps({"value": ' + repr(jwt(claims())) + '}).encode())\n'
                  'sign.run_claims(' + repr(OIDC_ENV) + ', ' + repr(str(Path(self.tmp, 'fresh.json'))) + ', O(), io.StringIO())\n'
                  'print(json.dumps(sorted(set(sys.modules) & {"release_sign", "controller", "eligibility", "install_aws"})))\n')
        done = subprocess.run([sys.executable, '-I', '-B', '-c', script], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              env={'PATH': os.environ.get('PATH', '')})
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(json.loads(done.stdout), [])
        # The claims function refers to nothing live.
        tree = ast.parse((SCRIPTS / 'sign.py').read_text())
        function = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'run_claims')
        names = {n.id for n in ast.walk(function) if isinstance(n, ast.Name)}
        self.assertFalse(names & {'run_signer', 'GuardedAws', 'load_adapter', 'Identity', 'live_main'})
        # The workflow's claims job: the one sign.py claims command, no install or AWS step.
        job = job_text('claims')
        self.assertIn("if: inputs.mode == 'claims'", job)
        self.assertIn('environment: release', job)
        self.assertIn('id-token: write', job)
        self.assertIn('scripts/release/sign.py claims', job)
        for word in ('install_aws', 'sign.py live', 'aws ', 'upload-artifact', 'verifier'):
            self.assertNotIn(word, job)

    def test_rsig6_token_claims_bind_the_exact_callee(self):
        SIGN.check_identity_claims(claims(), H)
        for change, name in ((dict(job_workflow_sha='9' * 40), 'job_workflow_sha'),
                             (dict(job_workflow_ref='adrenickchrome/rdy-core/.github/workflows/release-sign.yml@refs/heads/main'),
                              'job_workflow_ref'),
                             (dict(repository_id='1'), 'repository_id'), (dict(repository_owner_id='1'), 'repository_owner_id'),
                             (dict(environment='other'), 'environment'), (dict(event_name='push'), 'event_name'),
                             (dict(ref='refs/heads/main'), 'ref'), (dict(ref_type='branch'), 'ref_type'),
                             (dict(aud='other'), 'aud'), (dict(iss='https://example.invalid'), 'iss'),
                             (dict(iat='1'), 'iat'), (dict(exp=None), 'exp')):
            with self.subTest(name=name):
                with self.assertRaises(ControllerError) as caught:
                    SIGN.check_identity_claims(claims(**change), H)
                self.assertEqual(str(caught.exception), 'the token claim ' + name + ' is not the qualified identity')


# ---------------------------------------------------------------------------------------------
# Trusted code only
# ---------------------------------------------------------------------------------------------

class Trusted(Base):
    def test_rsig6_signer_runs_only_trusted_revision_code(self):
        text = WORKFLOW
        # workflow_call only.
        trigger = re.search(r'^on:\n((?:  .*\n)+)', text, re.M).group(1)
        self.assertEqual(re.findall(r'^  (\S+):', trigger, re.M), ['workflow_call'])
        self.assertIn('permissions: {}\n', text)
        self.assertEqual(re.findall(r'^  (\S+):$', text[text.index('\njobs:\n'):], re.M), ['revision', 'claims', 'verifier', 'sign'])
        # Nothing here may lock the lane again, inherit secrets, or go on after a failure.
        for word in ('concurrency', 'secrets', 'always()', 'continue-on-error', 'pull_request', '@main', 'ssh-agent',
                     'toJSON', 'candidate/'):
            self.assertNotIn(word, text)
        # Every Actions reference is a full SHA, and only actions the repository already pins.
        references = re.findall(r'uses:\s*(\S+)', text)
        self.assertTrue(references)
        for reference in references:
            self.assertRegex(reference, r'^actions/(checkout|upload-artifact|download-artifact)@[0-9a-f]{40}$')
        # The exact job permissions.
        sign, claims_job, verifier, revision = job_text('sign'), job_text('claims'), job_text('verifier'), job_text('revision')

        def permissions(job):
            block = re.search(r'^    permissions:\n((?:      .*\n)+)', job, re.M).group(1)
            return sorted(line.strip() for line in block.splitlines())
        self.assertEqual(permissions(sign), ['actions: read', 'contents: read', 'id-token: write', 'security-events: read'])
        self.assertEqual(permissions(claims_job), ['actions: read', 'contents: read', 'id-token: write'])
        self.assertEqual(permissions(verifier), ['actions: read', 'contents: read'])
        self.assertEqual(permissions(revision), ['actions: read'])
        self.assertIn('environment: release', sign)
        self.assertNotIn('environment:', verifier)
        self.assertNotIn('environment:', revision)
        # Every checkout is this repository at the bound revision, with no stored credential, and no
        # job checks out a caller or candidate tree.
        for job in (sign, claims_job, verifier):
            checkouts = [s for s in steps_of(job) if 'actions/checkout@' in s]
            self.assertEqual(len(checkouts), 1)
            self.assertIn('repository: adrenickchrome/rdy-core', checkouts[0])
            self.assertIn('ref: ${{ needs.revision.outputs.sha }}', checkouts[0])
            self.assertIn('path: trusted', checkouts[0])
            self.assertIn('persist-credentials: false', checkouts[0])
            self.assertIn('needs: revision' if job is claims_job else 'revision', job)
        self.assertNotIn('actions/checkout', revision)
        # Each Python step is isolated and runs only the trusted tree.
        for job in (sign, claims_job):
            commands = re.findall(r'(/usr/bin/python3[^\n]*)', job)
            self.assertTrue(commands)
            for command in commands:
                self.assertIn('/usr/bin/python3 -I -B trusted/scripts/release/', command)
        # No build or candidate code runs in the signing job. It reads the verifier by digest.
        for word in ('cargo', 'rustup', 'make ', 'npm', 'pip'):
            self.assertNotIn(word, sign)
        self.assertIn('VERIFIER_SHA256: ${{ needs.verifier.outputs.sha256 }}', sign)
        self.assertIn('cargo build --locked --release -p rdy-qa --bin qa-release-trust', verifier)
        # The AWS CLI comes only from the separate pinned installer, before the signer step.
        steps = steps_of(sign)
        order = [i for i, s in enumerate(steps) if 'install_aws.py' in s] + [i for i, s in enumerate(steps) if 'sign.py live' in s]
        self.assertEqual(len(order), 2)
        self.assertLess(order[0], order[1])
        # sign.py live reads credentials only through its own step environment.
        live = steps[order[1]]
        self.assertIn('GITHUB_TOKEN: ${{ github.token }}', live)
        self.assertIn('RDY_RELEASE_REQUEST: ${{ inputs.request }}', live)
        self.assertIn('RDY_RELEASE_ENROLLMENT: ${{ vars.RDY_RELEASE_ENROLLMENT }}', live)
        self.assertIn('RDY_RELEASE_ROLE_ARN: ${{ vars.RDY_RELEASE_ROLE_ARN }}', live)
        for step in steps[:order[0]] + steps[order[0]:order[1]]:
            self.assertNotIn('id-token', step)
        # Every job has a bound.
        for job in (sign, claims_job, verifier, revision):
            self.assertRegex(job, r'timeout-minutes: \d+')
            self.assertIn('runs-on: ubuntu-24.04', job)

    def run_binding(self, sha, run_answer, mode='live'):
        """Runs the workflow's own binding script over a fake run answer."""
        step = next(s for s in steps_of(job_text('revision')) if 'referenced_workflows' in s)
        script = re.search(r"<<'PY'\n(.*?)\n\s*PY\n", step, re.S).group(1)
        script = '\n'.join(line[10:] for line in script.split('\n'))
        out = Path(self.tmp, 'github-output')
        environ = {'SIGNER_SHA': sha, 'SIGNER_MODE': mode, 'GITHUB_TOKEN': GITHUB_SECRET, 'GITHUB_RUN_ID': '7',
                   'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_OUTPUT': str(out)}
        calls = []

        def urlopen(request, timeout):
            calls.append(request.full_url)
            return io.BytesIO(json.dumps(run_answer).encode())
        with mock.patch.dict(os.environ, environ), mock.patch('urllib.request.urlopen', urlopen):
            exec(compile(script, 'binding', 'exec'), {'__name__': '__main__'})
        self.assertEqual(calls, ['https://api.github.com/repos/adrenickchrome/rdy-core/actions/runs/7/attempts/2'])
        return out.read_text()

    def test_rsig6_revision_input_must_be_the_referenced_callee(self):
        path = 'adrenickchrome/rdy-core/.github/workflows/release-sign.yml@'
        good = {'referenced_workflows': [{'path': path + H, 'sha': H, 'ref': H}]}
        self.assertEqual(self.run_binding(H, good), 'sha=' + H + '\n')
        for name, sha, answer, mode in (
                ('another revision', '9' * 40, good, 'live'),
                ('short', H[:12], good, 'live'),
                ('a movable ref', H, {'referenced_workflows': [{'path': path + 'refs/heads/main', 'sha': H}]}, 'live'),
                ('none', H, {'referenced_workflows': []}, 'live'),
                ('no field', H, {}, 'live'),
                ('twice', H, {'referenced_workflows': good['referenced_workflows'] * 2}, 'live'),
                ('another mode', H, good, 'rehearse')):
            with self.subTest(name=name):
                with self.assertRaises(SystemExit) as caught:
                    self.run_binding(sha, answer, mode)
                self.assertNotEqual(caught.exception.code, 0)
                self.assertNotEqual(caught.exception.code, None)

    def test_rsig6_enrollment_is_closed_and_h_is_not_part_of_it(self):
        value = {'account_id': ctl.ACCOUNT, 'region': 'us-east-2', 'table': ctl.TABLE, 'table_id': ctl.TABLE_ID,
                 'genesis_sha256': ctl.GENESIS_SHA, 'authority_sha256': ctl.AUTHORITY_SHA,
                 'role_max_session_duration': 3600}
        enrollment, role_max = SIGN.parse_enrollment(json.dumps(value), H)
        self.assertEqual(enrollment, self.rig.enrollment)
        self.assertEqual(role_max, 3600)
        for name, change, message in (
                ('H', dict(value, signer_workflow_sha=H), "unknown field 'signer_workflow_sha'"),
                ('close', dict(value, tabel='x'), "unknown field 'tabel'; did you mean 'table'?"),
                ('missing', {k: v for k, v in value.items() if k != 'table_id'}, None),
                ('float', dict(value, role_max_session_duration=3600.0), None),
                ('short', dict(value, role_max_session_duration=899), None),
                ('account', dict(value, account_id='1'), None),
                ('digest', dict(value, genesis_sha256='x'), None)):
            with self.subTest(name=name):
                with self.assertRaises(ControllerError) as caught:
                    SIGN.parse_enrollment(json.dumps(change), H)
                if message:
                    self.assertEqual(str(caught.exception), message)
        with self.assertRaises(ControllerError):
            SIGN.parse_enrollment('', H)
        with self.assertRaises(ControllerError):
            SIGN.parse_enrollment(json.dumps(value), 'main')


# ---------------------------------------------------------------------------------------------
# AWS CLI distribution
# ---------------------------------------------------------------------------------------------

INSTALLER = b"""#!/bin/sh
# A fake bundled installer: records its arguments and environment and installs a fake aws.
set -e
INSTALL_DIR=$2
BIN_DIR=$4
mkdir -p "$INSTALL_DIR" "$BIN_DIR"
env > "$INSTALL_DIR/installer-env"
echo "$@" > "$INSTALL_DIR/installer-args"
printf '#!/bin/sh\\necho "aws-cli/%s Python/3.13 Linux/6 exe/x86_64"\\n' "$(cat "$(dirname "$0")/VERSION")" > "$BIN_DIR/aws"
chmod 755 "$BIN_DIR/aws"
"""


def aws_zip(path, version='2.34.25', extra=(), installer=INSTALLER):
    with zipfile.ZipFile(str(path), 'w') as archive:
        def member(name, data, mode=0o644):
            info = zipfile.ZipInfo(name)
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | mode) << 16
            archive.writestr(info, data)
        member('aws/install', installer, 0o755)
        member('aws/VERSION', version.encode())
        member('aws/dist/aws', b'fake binary', 0o755)
        for name, data, attr in extra:
            info = zipfile.ZipInfo(name)
            info.create_system = 3
            info.external_attr = attr << 16
            archive.writestr(info, data)
    return hashlib.sha256(Path(path).read_bytes()).hexdigest(), Path(path).stat().st_size


class Distribution(Base):
    def pin(self, digest, size):
        return INSTALL.Tool(version='2.34.25', url=INSTALL.OFFICIAL_PREFIX + 'awscli-exe-linux-x86_64-2.34.25.zip',
                            sha256=digest, size=size, max_archive=1 << 20, max_expanded=1 << 20, max_entries=64)

    def install(self, archive, tool, work=None):
        work = Path(work or tempfile.mkdtemp(dir=self.tmp))
        ambient = {'GITHUB_TOKEN': GITHUB_SECRET, 'AWS_PROFILE': 'ambient', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN': 'x'}
        with mock.patch.dict(os.environ, ambient):
            return INSTALL.install(tool, archive, work), work

    def test_rsig6_aws_distribution_is_verified_before_execution(self):
        # The manifest pins the qualified version by the official package digest.
        tools = json.loads((SCRIPTS / 'tools.json').read_text())
        tool = INSTALL.load_tool(SCRIPTS / 'tools.json', 'Linux-x86_64')
        self.assertEqual(sorted(tools['tools']), ['aws-cli'])
        self.assertEqual((tool.version, tool.sha256, tool.size),
                         ('2.34.25', AWS_ZIP_SHA256, AWS_ZIP_SIZE))
        self.assertEqual(tool.url, 'https://awscli.amazonaws.com/awscli-exe-linux-x86_64-2.34.25.zip')
        self.assertEqual((tool.max_archive, tool.max_expanded), (128 << 20, 512 << 20))
        with self.assertRaises(ControllerError):
            INSTALL.load_tool(SCRIPTS / 'tools.json', 'Darwin-arm64')
        # A good package installs into the work directory only, with a clean environment.
        path = Path(self.tmp, 'good.zip')
        digest, size = aws_zip(path)
        binary, work = self.install(path, self.pin(digest, size))
        self.assertEqual(binary, work / 'bin' / 'aws')
        self.assertEqual(subprocess.run([str(binary)], stdout=subprocess.PIPE).stdout.split()[0], b'aws-cli/2.34.25')
        installed_env = (work / 'install' / 'installer-env').read_text()
        for word in ('GITHUB_TOKEN', 'AWS_PROFILE', 'ACTIONS_ID_TOKEN'):
            self.assertNotIn(word, installed_env)
        self.assertEqual((work / 'install' / 'installer-args').read_text().split(),
                         ['--install-dir', str(work / 'install'), '--bin-dir', str(work / 'bin')])
        # A wrong digest or size: nothing is extracted or executed.
        for name, tool_pin in (('digest', self.pin('0' * 64, size)), ('size', self.pin(digest, size + 1))):
            with self.subTest(name=name):
                work = Path(tempfile.mkdtemp(dir=self.tmp))
                with mock.patch.object(INSTALL.subprocess, 'run') as run, self.assertRaises(ControllerError):
                    self.install(path, tool_pin, work)
                run.assert_not_called()
                self.assertEqual(list(work.iterdir()), [])
        # The archive and expansion limits, before anything runs.
        small = self.pin(digest, size)
        for name, limited in (('archive', dataclasses.replace(small, max_archive=size - 1)),
                              ('expanded', dataclasses.replace(small, max_expanded=4)),
                              ('entries', dataclasses.replace(small, max_entries=1))):
            with self.subTest(name=name):
                work = Path(tempfile.mkdtemp(dir=self.tmp))
                with mock.patch.object(INSTALL.subprocess, 'run') as run, self.assertRaises(ControllerError):
                    self.install(path, limited, work)
                run.assert_not_called()
        # Hostile members refuse before anything runs.
        regular = stat.S_IFREG | 0o644
        for name, extra in (('absolute', ('/etc/evil', b'x', regular)), ('traversal', ('aws/../../evil', b'x', regular)),
                            ('backslash', ('aws\\evil', b'x', regular)), ('outside', ('other/file', b'x', regular)),
                            ('symlink', ('aws/link', b'/etc/passwd', stat.S_IFLNK | 0o777)),
                            ('device', ('aws/dev', b'', stat.S_IFCHR | 0o644)),
                            ('duplicate', ('aws/VERSION', b'x', regular))):
            with self.subTest(name=name):
                hostile = Path(self.tmp, name + '.zip')
                with warnings.catch_warnings():
                    warnings.simplefilter('ignore')  # zipfile warns about the duplicate name it is asked to write
                    digest2, size2 = aws_zip(hostile, extra=[extra])
                work = Path(tempfile.mkdtemp(dir=self.tmp))
                with mock.patch.object(INSTALL.subprocess, 'run') as run, self.assertRaises(ControllerError):
                    self.install(hostile, self.pin(digest2, size2), work)
                run.assert_not_called()
                self.assertFalse((work / 'install').exists())
        # A setuid bit is dropped: an extracted file is at most 0755.
        odd = Path(self.tmp, 'odd.zip')
        digest3, size3 = aws_zip(odd, extra=[('aws/dist/odd', b'x', stat.S_IFREG | 0o4777)])
        binary, work = self.install(odd, self.pin(digest3, size3))
        self.assertEqual(stat.S_IMODE((work / 'unpacked' / 'aws/dist/odd').stat().st_mode), 0o755)
        # A package that reports another version is refused, with no fallback to an ambient CLI.
        other = Path(self.tmp, 'other.zip')
        digest4, size4 = aws_zip(other, version='2.0.0')
        ambient = Path(self.tmp, 'ambient')
        ambient.mkdir()
        (ambient / 'aws').write_text('#!/bin/sh\necho aws-cli/2.34.25\n')
        (ambient / 'aws').chmod(0o755)
        with mock.patch.dict(os.environ, {'PATH': str(ambient) + os.pathsep + os.environ.get('PATH', '')}):
            with self.assertRaises(ControllerError) as caught:
                self.install(other, self.pin(digest4, size4))
        self.assertEqual(str(caught.exception), 'the installed AWS CLI is not the pinned version')
        # A broken installer has no fallback either.
        broken = Path(self.tmp, 'broken.zip')
        digest5, size5 = aws_zip(broken, installer=b'#!/bin/sh\nexit 3\n')
        with self.assertRaises(ControllerError):
            self.install(broken, self.pin(digest5, size5))
        # The source has no system path, no sudo and no lookup of an existing aws.
        source = (SCRIPTS / 'install_aws.py').read_text()
        for word in ('/usr/local', 'sudo', 'which(', "['aws'", "'--update'"):
            self.assertNotIn(word, source)

    def test_rsig6_download_is_bounded_and_official_only(self):
        tool = self.pin('0' * 64, 10)
        out = Path(self.tmp, 'download.zip')

        class Stream(io.BytesIO):
            def geturl(self):
                return tool.url
        with self.assertRaises(ControllerError):
            INSTALL.download(dataclasses.replace(tool, url='https://example.invalid/x.zip'), out, lambda *a, **k: Stream(b'x'))
        with self.assertRaises(ControllerError):
            INSTALL.download(dataclasses.replace(tool, url='http://awscli.amazonaws.com/x.zip'), out, lambda *a, **k: Stream(b'x'))
        with self.assertRaises(ControllerError):
            INSTALL.download(tool, out, lambda *a, **k: Stream(b'x' * 11))
        self.assertFalse(out.exists())
        self.assertEqual(INSTALL.download(tool, out, lambda *a, **k: Stream(b'x' * 10)), out)
        self.assertEqual(out.read_bytes(), b'x' * 10)


# ---------------------------------------------------------------------------------------------
# Secrets and test roots
# ---------------------------------------------------------------------------------------------

STS_REPLY = {'Credentials': {'AccessKeyId': 'ASIASYNTHETIC', 'SecretAccessKey': ACCESS_SECRET, 'SessionToken': SESSION_SECRET,
                             'Expiration': '2027-01-15T08:00:00+00:00'},
             'AssumedRoleUser': {'Arn': 'arn:aws:sts::' + ctl.ACCOUNT + ':assumed-role/rdy-test-signer/rdy-core-signer-77-2'}}


class FakeRunner(object):
    """The fake `aws` child: one queued (status, stdout, stderr) per call. It records what it saw."""

    def __init__(self, *replies):
        self.replies, self.calls = list(replies), []
        self.on_call = None

    def __call__(self, argv, deadline=None, stdin=None, env=None, **_):
        self.calls.append(dict(argv=list(argv), deadline=deadline, stdin=stdin, env=dict(env or {})))
        if self.on_call:
            self.on_call(argv)
        reply = self.replies.pop(0)
        if isinstance(reply, BaseException):
            raise reply
        return reply


class Secrets(Base):
    def session(self, runner=None, opener=None, role_arn=base.ROLE_ARN):
        directory = Path(self.tmp, 'session')
        directory.mkdir(mode=0o700)
        runner = runner or FakeRunner((0, json.dumps(STS_REPLY).encode(), b''))
        aws = SIGN.AwsCli(Path('/w/aws/bin/aws'), 'us-east-2', runner)
        identity = SIGN.Identity(OIDC_ENV, role_arn, H, directory, aws, 3600, '77', '2', opener or FakeOpener(jwt(claims())))
        return identity, runner, directory

    def test_rsig6_secrets_and_test_roots_never_leave_the_job(self):
        # The environment: only named values reach any child. Tokens, ambient AWS and ssh-agent are gone.
        ambient = {'PATH': '/ambient/bin', 'GITHUB_TOKEN': GITHUB_SECRET, 'ACTIONS_ID_TOKEN_REQUEST_URL': 'u',
                   'ACTIONS_ID_TOKEN_REQUEST_TOKEN': 't', 'AWS_ACCESS_KEY_ID': 'a', 'AWS_SECRET_ACCESS_KEY': 's',
                   'AWS_SESSION_TOKEN': 's', 'AWS_PROFILE': 'p', 'AWS_CONFIG_FILE': '/home/x/.aws/config',
                   'AWS_SHARED_CREDENTIALS_FILE': '/home/x/.aws/credentials', 'SSH_AUTH_SOCK': '/tmp/agent',
                   'SSH_AGENT_PID': '1', 'HOME': '/home/x', 'LANG': 'C.UTF-8', 'RDY_RELEASE_REQUEST': '{}'}
        session = Path('/w/session')
        env = SIGN.signing_env(ambient, Path('/w/aws/bin'), session)
        self.assertEqual(env['PATH'], '/w/aws/bin:/usr/bin:/bin')
        self.assertEqual(env['HOME'], str(session))
        self.assertEqual(env['AWS_SHARED_CREDENTIALS_FILE'], str(session / 'aws-credentials'))
        self.assertEqual(env['AWS_CONFIG_FILE'], str(session / 'aws-config'))
        self.assertEqual(env['AWS_EC2_METADATA_DISABLED'], 'true')
        self.assertEqual(env['AWS_IGNORE_CONFIGURED_ENDPOINT_URLS'], 'true')
        for name in ('GITHUB_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_URL', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'AWS_ACCESS_KEY_ID',
                     'AWS_SECRET_ACCESS_KEY', 'AWS_SESSION_TOKEN', 'AWS_PROFILE', 'SSH_AUTH_SOCK', 'SSH_AGENT_PID',
                     'RDY_RELEASE_REQUEST'):
            self.assertNotIn(name, env)
        # The OIDC token lives in a 0600 file for the one STS call. It is never on a command line.
        identity, runner, directory = self.session()
        seen = {}

        def on_call(argv):
            path = next(a for a in argv if a.startswith('file://'))[len('file://'):]
            seen['mode'] = stat.S_IMODE(os.stat(path).st_mode)
            seen['token'] = Path(path).read_text()
            seen['directory'] = stat.S_IMODE(os.stat(directory).st_mode)
        runner.on_call = on_call
        with mock.patch.dict(os.environ, {}, clear=False):
            token = identity.request_oidc('sts.amazonaws.com')
            receipt = identity.assume_role(token.token, 900)
            credentials = directory / 'aws-credentials'
            self.assertEqual(stat.S_IMODE(credentials.stat().st_mode), 0o600)
            self.assertIn(SESSION_SECRET, credentials.read_text())
        self.assertEqual(seen['mode'], 0o600)
        self.assertEqual(seen['directory'], 0o700)
        self.assertEqual(seen['token'], token.token)
        self.assertEqual(sorted(p.name for p in directory.iterdir()), ['aws-config', 'aws-credentials'])
        call = runner.calls[0]
        self.assertNotIn(token.token, ' '.join(call['argv']))
        for word in ('sts', 'assume-role-with-web-identity', '--no-sign-request', '--role-arn', base.ROLE_ARN):
            self.assertIn(word, call['argv'])
        self.assertEqual(call['argv'][call['argv'].index('--duration-seconds') + 1], '900')
        self.assertEqual(call['env']['AWS_MAX_ATTEMPTS'], '1')
        # Only nonsecret facts come back, and repr hides the token.
        self.assertEqual((receipt.role_arn, receipt.requested_duration, receipt.role_max_session_duration),
                         (base.ROLE_ARN, 900, 3600))
        self.assert_nothing_secret(repr(receipt))
        self.assert_nothing_secret(repr(token))
        # One token and one assumption: no refresh, and the file is gone after a failed call too.
        with self.assertRaises(ControllerError):
            identity.request_oidc('sts.amazonaws.com')
        with self.assertRaises(ControllerError):
            identity.assume_role(token.token, 900)
        identity.cleanup()
        self.assertEqual(sorted(p.name for p in directory.iterdir()), [])
        shutil.rmtree(directory)
        for reply, name in (((1, b'', ('An error occurred ' + ACCESS_SECRET).encode()), 'failed'),
                            ((0, b'not json', b''), 'malformed'),
                            ((0, json.dumps(dict(STS_REPLY, AssumedRoleUser={'Arn': 'arn:aws:sts::1:assumed-role/other/s'})).encode(), b''),
                             'another role'),
                            ((0, json.dumps(dict(STS_REPLY, Credentials=dict(STS_REPLY['Credentials'], SessionToken='a\nb'))).encode(), b''),
                             'a value with a newline')):
            with self.subTest(name=name):
                identity, runner, directory = self.session(FakeRunner(reply))
                token = identity.request_oidc('sts.amazonaws.com')
                with self.assertRaises(ControllerError) as caught:
                    identity.assume_role(token.token, 900)
                self.assert_nothing_secret(str(caught.exception))
                self.assertEqual(list(directory.iterdir()), [])
                shutil.rmtree(directory)
        # A token for another callee never reaches STS.
        runner = FakeRunner()
        identity, runner, directory = self.session(runner, FakeOpener(jwt(claims(job_workflow_sha='9' * 40))))
        with self.assertRaises(ControllerError):
            identity.request_oidc('sts.amazonaws.com')
        self.assertEqual(runner.calls, [])
        shutil.rmtree(directory)
        # The outputs are exactly the named nonsecret files.
        out = Path(self.tmp, 'out')
        rig = self.rig
        rig.reserve_version()
        ctrl = rig.controller()
        guard = SIGN.GuardedAws(Inner(rig), ctrl)
        outcome = SIGN.run_signer(ctrl, rig.provider(), rig.table, ADAPTER, guard, rig.prepare)
        versions = {'python': '3.13.0', 'openssh': 'OpenSSH_9.6', 'aws_cli': '2.34.25'}
        SIGN.write_outputs(out, outcome, ctrl, versions)
        SIGN.write_stop(out, ctrl)
        self.assertEqual(sorted(p.name for p in out.iterdir()),
                         ['SHA256SUMS.sig', 'authorization.json', 'sign-receipt.json', 'sign-stop.json'])
        for path in out.iterdir():
            self.assert_nothing_secret(path.read_bytes())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        receipt = json.loads((out / 'sign-receipt.json').read_text())
        self.assertEqual({k: receipt[k] for k in ('python', 'openssh', 'aws_cli')}, versions)
        self.assertEqual((receipt['tuple_sha256'], receipt['version'], receipt['result'], receipt['run_id'], receipt['job_id']),
                         (rig.tuple_sha(), rig.w.v, 'signed and verified', str(ctl.DISPATCH_RUN), str(ctl.SIGN_JOB)))
        self.assertEqual(json.loads((out / 'sign-stop.json').read_text())['stopped'], 'attempt_or_later')
        # A candidate with anything beyond its three files (a test signature or root) refuses.
        self.check_candidate_members()
        # The workflow uploads exact files, never the session directory, and always cleans up.
        sign = job_text('sign')
        uploads = [s for s in steps_of(sign) if 'upload-artifact' in s]
        self.assertEqual(len(uploads), 2)
        self.assertIn('rdy-signer-out/SHA256SUMS.sig', uploads[0])
        self.assertIn('rdy-signer-out/authorization.json', uploads[0])
        self.assertIn('rdy-signer-out/sign-receipt.json', uploads[0])
        self.assertIn('rdy-signer-out/sign-stop.json', uploads[1])
        for upload in uploads:
            self.assertNotIn('rdy-signer-work', upload)
            self.assertNotIn('credentials', upload)
        cleanup = next(s for s in steps_of(sign) if 'rm -rf' in s)
        self.assertIn('if: success() || failure()', cleanup)
        self.assertIn('rdy-signer-work', cleanup)
        self.assertLess(steps_of(sign).index(uploads[1]), steps_of(sign).index(cleanup))
        self.assertIn('if: success() || failure()', uploads[1])
        # No step prints a token or sets one in the job environment.
        self.assertNotIn('add-mask', WORKFLOW)
        self.assertNotRegex(WORKFLOW, r'(?i)echo[^\n]*token')
        self.assertNotIn('$GITHUB_ENV', WORKFLOW)

    def check_candidate_members(self):
        rig = self.rig
        version = rig.w.v
        release = mock.Mock(version=version, artifacts=tuple((f['name'], f['sha256'], f['size']) for f in rig.w.archives),
                            manifest_sha256=rig.w.manifest)
        good = dict(rig.archive_bytes)
        good['SHA256SUMS'] = rig.manifest_bytes()
        good['candidate.json'] = b'{}\n'
        request = mock.Mock(candidate_artifact_id=44, candidate_run_id=45)

        def facts(members):
            path = Path(tempfile.mkdtemp(dir=self.tmp), 'a.zip')
            with zipfile.ZipFile(str(path), 'w') as archive:
                for name, data in members.items():
                    archive.writestr(name, data)
            value = mock.Mock()
            value.artifact.return_value = 'fact'
            value.artifact_path.return_value = path
            return value
        directory = Path(self.tmp, 'candidate')
        SIGN.prepare_candidate(facts(good), request, release, directory)
        self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
        self.assertEqual(sorted(p.name for p in directory.iterdir()),
                         sorted(list(rig.archive_bytes) + ['SHA256SUMS']))
        for name, change in (('a test signature', dict(good, **{'SHA256SUMS.sig': b'x'})),
                             ('a test root', dict(good, **{'allowed-signers': b'x'})),
                             ('a missing archive', {k: v for k, v in good.items() if k != sorted(rig.archive_bytes)[0]}),
                             ('another manifest', dict(good, SHA256SUMS=b'other\n')),
                             ('changed bytes', dict(good, **{sorted(rig.archive_bytes)[0]: b'changed'}))):
            with self.subTest(name=name):
                target = Path(tempfile.mkdtemp(dir=self.tmp), 'candidate')
                with self.assertRaises(ControllerError):
                    SIGN.prepare_candidate(facts(change), request, release, target)
                self.assertFalse((target / 'SHA256SUMS.sig').exists())

    def test_rsig6_cleanup_runs_after_failure(self):
        # live_main's session directory goes away on a refusal and on an interrupt; a stop receipt is written.
        work, out = Path(self.tmp, 'work'), Path(self.tmp, 'out')
        environ = self.live_environ()

        def stage(*_):
            self.assertTrue(work.exists())
            Path(work, 'session', 'aws-credentials').write_text(SESSION_SECRET)
            raise Refused(CONTROLLER.AUTHORITY, 'admission is CLOSED')
        stderr = io.StringIO()
        status = SIGN.live_main(self.live_args(work, out), environ, stage=stage, stderr=stderr)
        self.assertEqual(status, 1)
        self.assertEqual(stderr.getvalue(), 'release controller: authority unavailable: admission is CLOSED\n')
        self.assertFalse(work.exists())
        stop = json.loads((out / 'sign-stop.json').read_text())
        self.assertEqual(stop, {'schema': 1, 'version': None, 'tuple_sha256': None, 'run_id': '7', 'run_attempt': '1',
                                'job_id': None, 'stopped': 'before_attempt'})
        shutil.rmtree(out)

        def interrupted(*_):
            Path(work, 'session', 'aws-credentials').write_text(SESSION_SECRET)
            raise SIGN.Interrupted()
        stderr = io.StringIO()
        self.assertEqual(SIGN.live_main(self.live_args(work, out), environ, stage=interrupted, stderr=stderr), 1)
        self.assertFalse(work.exists())
        # A bug never echoes its text or sends a retry: the fixed line only.
        shutil.rmtree(out)

        def bug(*_):
            raise KeyError(GITHUB_SECRET)
        stderr = io.StringIO()
        self.assertEqual(SIGN.live_main(self.live_args(work, out), environ, stage=bug, stderr=stderr), 1)
        self.assert_nothing_secret(stderr.getvalue())
        self.assertEqual(stderr.getvalue(), 'release controller: signing outcome unknown: the signer stopped unexpectedly\n')

    @unittest.skipIf(sys.version_info < (3, 10), 'the adapter needs Python 3.10 or later')
    def test_rsig6_live_stage_wires_only_checked_inputs(self):
        rig = self.rig
        value = {'account_id': ctl.ACCOUNT, 'region': 'us-east-2', 'table': ctl.TABLE, 'table_id': ctl.TABLE_ID,
                 'genesis_sha256': ctl.GENESIS_SHA, 'authority_sha256': ctl.AUTHORITY_SHA,
                 'role_max_session_duration': 3600}
        request = dataclasses.asdict(rig.request())
        request['approvals'] = []
        work, out = Path(self.tmp, 'work'), Path(self.tmp, 'out')
        environ = dict(self.live_environ(), RDY_RELEASE_ENROLLMENT=json.dumps(value), RDY_RELEASE_REQUEST=json.dumps(request),
                       GITHUB_RUN_ID=str(ctl.DISPATCH_RUN), RDY_RESERVE_ARTIFACT_ID='9200', SSH_AUTH_SOCK='/tmp/agent',
                       AWS_PROFILE='ambient')
        seen = {}

        def fake_run(controller, identity, table, adapter, guard, prepare):
            seen.update(controller=controller, identity=identity, table=table, adapter=adapter, guard=guard,
                        environ=dict(os.environ))
            raise Refused(CONTROLLER.AUTHORITY, 'stop here')
        with mock.patch.dict(os.environ, {'GITHUB_TOKEN': GITHUB_SECRET}), \
                mock.patch.object(SIGN, '_check_trusted') as trusted, \
                mock.patch.object(SIGN, '_install_verifier', return_value=Path('/w/qa-release-trust')) as verifier, \
                mock.patch.object(SIGN, 'tool_versions', return_value={}), \
                mock.patch.object(SIGN, 'find_holder', return_value=CONTROLLER.Holder(ctl.DISPATCH_RUN, 1, ctl.SIGN_JOB)), \
                mock.patch.object(SIGN, 'run_signer', fake_run):
            stderr = io.StringIO()
            args = self.live_args(work, out)
            self.assertEqual(SIGN.live_main(args, environ, stderr=stderr), 1)
        self.assertEqual(stderr.getvalue(), 'release controller: authority unavailable: stop here\n')
        trusted.assert_called_once_with(args.trusted, H)
        verifier.assert_called_once()
        controller = seen['controller']
        self.assertEqual(controller.enrollment.signer_workflow_sha, H)
        self.assertEqual(controller.enrollment, rig.enrollment)
        self.assertEqual(controller.reserve_artifact_id, 9200)
        self.assertEqual(seen['identity'].role_arn, base.ROLE_ARN)
        self.assertEqual(seen['table'].table, ctl.TABLE)
        self.assertIs(seen['guard'].gate, controller)
        self.assertIsInstance(seen['guard'].inner, seen['adapter'].Aws)
        for name in ('GITHUB_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_URL', 'SSH_AUTH_SOCK',
                     'AWS_PROFILE', 'RDY_RELEASE_REQUEST'):
            self.assertNotIn(name, seen['environ'])
        self.assertEqual(seen['environ']['PATH'], '/w/aws/bin:/usr/bin:/bin')

    def test_rsig6_trusted_checks(self):
        # The checkout must be exactly H; the verifier must be the bytes built at H.
        with mock.patch.object(SIGN.cand, 'run_bounded', return_value=(0, ('9' * 40 + '\n').encode(), b'')):
            with self.assertRaises(ControllerError) as caught:
                SIGN._check_trusted(ROOT, H)
        self.assertEqual(str(caught.exception), 'the trusted checkout is not the signer revision')
        with mock.patch.object(SIGN.cand, 'run_bounded', return_value=(0, (H + '\n').encode(), b'')):
            SIGN._check_trusted(ROOT, H)
            with self.assertRaises(ControllerError) as caught:
                SIGN._check_trusted(Path(self.tmp), H)
        self.assertEqual(str(caught.exception), 'the running signer is not in the trusted checkout')
        built = Path(self.tmp, 'built')
        built.write_bytes(b'verifier')
        work = Path(tempfile.mkdtemp(dir=self.tmp))
        with self.assertRaises(ControllerError):
            SIGN._install_verifier(built, '0' * 64, work)
        self.assertEqual(list(work.iterdir()), [])
        copy = SIGN._install_verifier(built, hashlib.sha256(b'verifier').hexdigest(), work)
        self.assertEqual(stat.S_IMODE(copy.stat().st_mode), 0o700)
        self.assertEqual(copy.parent, work)

    def live_args(self, work, out):
        return mock.Mock(signer_sha=H, trusted=Path('/w/trusted'), aws_bin=Path('/w/aws/bin'), verifier=Path('/w/verifier'), verifier_sha256='0' * 64,
                         work=work, out=out)

    def live_environ(self):
        return dict(OIDC_ENV, GITHUB_TOKEN=GITHUB_SECRET, GITHUB_RUN_ID='7', GITHUB_RUN_ATTEMPT='1',
                    RDY_RELEASE_REQUEST='{}', RDY_RELEASE_ENROLLMENT='{}', RDY_RELEASE_ROLE_ARN=base.ROLE_ARN, PATH='/bin')


# ---------------------------------------------------------------------------------------------
# The live clients, over fake children and transports
# ---------------------------------------------------------------------------------------------

def stored(pk, sk, body):
    return {'pk': {'S': pk}, 'sk': {'S': sk}, 'body': {'B': base64.b64encode(body).decode()},
            'body_sha256': {'S': hashlib.sha256(body).hexdigest()}}


class Clients(Base):
    def table(self, *replies):
        runner = FakeRunner(*replies)
        return SIGN.CliTable(SIGN.AwsCli(Path('/w/aws/bin/aws'), 'us-east-2', runner), ctl.TABLE), runner

    def test_rsig6_cli_table_reads(self):
        table, runner = self.table((0, json.dumps({'Table': {'TableId': 'x'}}).encode(), b''),
                                   (0, json.dumps({'Item': stored('p', 's', b'body')}).encode(), b''),
                                   (0, b'{}', b''),
                                   (0, json.dumps({'Responses': [{'Item': stored('p', 's', b'body')}, {}]}).encode(), b''))
        self.assertEqual(table.describe_table('arn:table', 20), {'TableId': 'x'})
        self.assertEqual(table.get_item('p', 's', 20)['body'], {'B': b'body'})
        self.assertIsNone(table.get_item('p', 'other', 20))
        answer = table.transact_get_items([('p', 's'), ('p', 'none')], 20)
        self.assertEqual((answer[0]['body'], answer[1]), ({'B': b'body'}, None))
        describe, get, _, snapshot = runner.calls
        for call in runner.calls:
            self.assertEqual(call['argv'][0], '/w/aws/bin/aws')
            self.assertEqual(call['argv'][call['argv'].index('--region') + 1], 'us-east-2')
            self.assertEqual(call['env']['AWS_MAX_ATTEMPTS'], '1')
            self.assertLessEqual(call['deadline'], 20)
        self.assertEqual(describe['argv'][describe['argv'].index('--table-name') + 1], 'arn:table')
        self.assertEqual(json.loads(get['stdin']), {'TableName': ctl.TABLE, 'Key': {'pk': {'S': 'p'}, 'sk': {'S': 's'}},
                                                    'ConsistentRead': True})
        self.assertEqual(json.loads(snapshot['stdin']),
                         {'TransactItems': [{'Get': {'TableName': ctl.TABLE, 'Key': {'pk': {'S': 'p'}, 'sk': {'S': 's'}}}},
                                            {'Get': {'TableName': ctl.TABLE, 'Key': {'pk': {'S': 'p'}, 'sk': {'S': 'none'}}}}]})
        # A failed or malformed read is unavailable, never absence.
        for reply in ((1, b'', b'failure'), (0, b'not json', b''), (0, b'{"Item": {"body": {"B": "!"}}}', b''),
                      ControllerError('a helper passed its deadline')):
            table, _ = self.table(reply)
            with self.assertRaises(CONTROLLER.JournalUnavailable):
                table.get_item('p', 's', 20)
        table, _ = self.table((0, b'{"Responses": [{}]}', b''))
        with self.assertRaises(CONTROLLER.JournalUnavailable):
            table.transact_get_items([('p', 's'), ('p', 't')], 20)

    def test_rsig6_cli_table_write_counts_the_one_request(self):
        journal = CONTROLLER.Journal(None, self.rig.enrollment)
        action = journal.put(('p', 's'), b'body')
        one = b'x\nDEBUG - MainThread - botocore.endpoint - Sending http request: <AWSPreparedRequest>\ny\n'
        table, runner = self.table((0, b'{}', one))
        ack = table.transact_write_items([action], 'token-1', 20)
        self.assertEqual(ack, CONTROLLER.WriteAck(underlying_requests=1))
        sent = json.loads(runner.calls[0]['stdin'])
        self.assertEqual(sent['ClientRequestToken'], 'token-1')
        put = sent['TransactItems'][0]['Put']
        self.assertEqual(put['Item']['body'], {'B': base64.b64encode(b'body').decode()})
        self.assertEqual(put['ConditionExpression'], CONTROLLER.PUT_CONDITION)
        self.assertIn('--debug', runner.calls[0]['argv'])
        # A hidden retry shows as two requests: the journal gives no fresh create for it. No line proves none.
        table, _ = self.table((0, b'{}', one + one))
        self.assertEqual(table.transact_write_items([action], 't', 20).underlying_requests, 2)
        self.assertFalse(CONTROLLER.Journal(self.table((0, b'{}', one + one))[0], self.rig.enrollment).write([action]))
        table, _ = self.table((0, b'{}', b''))
        self.assertEqual(table.transact_write_items([action], 't', 20).underlying_requests, 0)
        # A refused condition, and everything uncertain.
        cancelled = (b'An error occurred (TransactionCanceledException) when calling the TransactWriteItems operation: '
                     b'Transaction cancelled, please refer cancellation reasons for specific reasons '
                     b'[None, ConditionalCheckFailed]\n')
        table, _ = self.table((254, b'', one + cancelled))
        with self.assertRaises(CONTROLLER.JournalConditionFailed):
            table.transact_write_items([action], 't', 20)
        for reply in ((254, b'', one + b'An error occurred (TransactionCanceledException) when calling the '
                                       b'TransactWriteItems operation: reasons [ThrottlingError, None]\n'),
                      (254, b'', one + b'An error occurred (TransactionInProgressException) when calling x\n'),
                      (254, b'', one + b'An error occurred (ExpiredTokenException) when calling x\n'),
                      (255, b'', b'Connection was closed\n'), ControllerError('a helper passed its deadline')):
            table, _ = self.table(reply)
            with self.assertRaises(CONTROLLER.JournalOutcomeUnknown):
                table.transact_write_items([action], 't', 20)

    def test_rsig6_ssh_verifier_command(self):
        seen = {}

        def on_call(argv):
            allowed = argv[argv.index('-f') + 1]
            seen['allowed'] = Path(allowed).read_text()
            seen['signature'] = Path(argv[argv.index('-s') + 1]).read_bytes()
        runner = FakeRunner((0, b'', b''), (255, b'', b''), ControllerError('a helper passed its deadline'))
        runner.on_call = on_call
        verifier = SIGN.SshVerifier(runner)
        line = 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAA'
        self.assertIs(verifier.verify(ARMORED, b'manifest\n', line, 'rdy-release', 'rdy-release-v1'), True)
        call = runner.calls[0]
        self.assertEqual(call['argv'][:3], ['ssh-keygen', '-Y', 'verify'])
        self.assertEqual(call['argv'][call['argv'].index('-I') + 1], 'rdy-release')
        self.assertEqual(call['argv'][call['argv'].index('-n') + 1], 'rdy-release-v1')
        self.assertEqual(call['stdin'], b'manifest\n')
        self.assertEqual(seen, {'allowed': line + '\n', 'signature': ARMORED})
        self.assertIs(verifier.verify(ARMORED, b'manifest\n', line, 'rdy-release', 'rdy-release-v1'), False)
        self.assertIs(verifier.verify(ARMORED, b'manifest\n', line, 'rdy-release', 'rdy-release-v1'), False)

    def test_rsig6_run_evidence_keeps_nonsecret_boundaries(self):
        holder = CONTROLLER.Holder(7, 1, 9)
        other = CONTROLLER.Holder(7, 2, 10)
        stream = io.StringIO()
        path = Path(self.tmp, 'evidence.jsonl')
        evidence = SIGN.RunEvidence(path, stream)
        evidence.record(holder, 'oidc_requested', {'audience': 'sts.amazonaws.com'})
        evidence.record(other, 'pending', {'grant_id': 'g'})
        rows = evidence.boundaries(holder)
        self.assertEqual([r[1] for r in rows], ['oidc_requested'])
        self.assertIn('oidc_requested', stream.getvalue())
        for bad in ({'token': 'x'}, {'audience': 1.5}, {'audience': {'nested': 1}}):
            with self.assertRaises(ControllerError):
                evidence.record(holder, 'sts_issued', bad)
        with self.assertRaises(ControllerError):
            evidence.record(holder, 'something else', {})
        # A real controller's evidence over this class: one request per attempt across processes.
        path.unlink()
        rig = self.rig
        rig.reserve_version()
        evidence = SIGN.RunEvidence(path, io.StringIO())
        ctrl = rig.controller(run_evidence=evidence)
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        restarted = rig.controller(run_evidence=SIGN.RunEvidence(path, io.StringIO()))
        restarted.admit()
        provider = rig.provider()
        self.refused(CONTROLLER.AUTHORITY, restarted.issue_credentials, provider)
        self.assertEqual(provider.oidc, [])
        # A broken file refuses rather than passing as empty.
        path.write_text('not json\n')
        with self.assertRaises(ControllerError):
            SIGN.RunEvidence(path, io.StringIO()).boundaries(holder)

    def history(self, runs, jobs, artifacts=None, members=None):
        facts = mock.Mock()
        facts.release_runs.return_value = runs
        facts.attempt_jobs.side_effect = lambda run, attempt: jobs[(run, attempt)]
        facts.run_artifacts.side_effect = lambda run: (artifacts or {}).get(run, [])

        def member(fact, name, limit):
            data = (members or {}).get(fact.artifact_id)
            if data is None:
                raise ControllerError('the artifact expired')
            return data
        facts.artifact_member.side_effect = member
        return SIGN.GitHubHistory(facts)

    @staticmethod
    def job(job_id, name=None, status='completed', conclusion='failure', head=S):
        return _Job(job_id, name or ('Sign / ' + SIGN.SIGN_JOB), status, conclusion, head)

    @staticmethod
    def artifact(ident, attempt, expired=False):
        return _Artifact(ident, 'sign-stop-' + str(attempt), expired, S)

    @staticmethod
    def stop(run, attempt, job, stopped, version='0.43.0', tuple_sha256='a' * 64):
        return CONTROLLER._canon({'schema': 1, 'version': version, 'tuple_sha256': tuple_sha256, 'run_id': str(run),
                                  'run_attempt': str(attempt), 'job_id': None if job is None else str(job), 'stopped': stopped})

    def test_rsig6_history_proves_each_stop_or_says_unknown(self):
        tuple_sha = 'a' * 64
        history = self.history(
            [(70, 3), (71, 1)],
            {(70, 1): [self.job(1, conclusion='skipped'), self.job(5, name='Other job')],
             (70, 2): [self.job(2)],
             (70, 3): [self.job(3)],
             (71, 1): [self.job(4)]},
            {70: [self.artifact(92, 2), self.artifact(93, 3)], 71: [self.artifact(94, 1, expired=True)]},
            {92: self.stop(70, 2, 2, 'before_attempt'), 93: self.stop(70, 3, 3, 'attempt_or_later')})
        answer = history.invocations('0.43.0', tuple_sha)
        self.assertIs(answer.complete, True)
        self.assertEqual([(i.run_id, i.run_attempt, i.job_id, i.stopped) for i in answer.invocations],
                         [(70, 1, 1, 'before_attempt'), (70, 2, 2, 'before_attempt'), (70, 3, 3, 'attempt_or_later'),
                          (71, 1, 4, 'unknown')])
        # A receipt that names another job, attempt or tuple proves nothing; a null tuple stopped before one was known.
        for receipt, expected in ((self.stop(70, 1, 99, 'before_attempt'), 'unknown'),
                                  (self.stop(70, 2, 1, 'before_attempt'), 'unknown'),
                                  (self.stop(70, 1, 1, 'before_attempt', tuple_sha256='b' * 64), 'unknown'),
                                  (self.stop(70, 1, 1, 'before_attempt', version=None, tuple_sha256=None), 'before_attempt'),
                                  (self.stop(70, 1, 1, 'attempt_or_later', version=None, tuple_sha256=None), 'unknown'),
                                  (self.stop(70, 1, None, 'before_attempt', version=None, tuple_sha256=None), 'before_attempt'),
                                  (self.stop(70, 1, None, 'before_attempt'), 'unknown'),
                                  (b'not json', 'unknown')):
            history = self.history([(70, 1)], {(70, 1): [self.job(1)]}, {70: [self.artifact(92, 1)]}, {92: receipt})
            self.assertEqual(history.invocations('0.43.0', tuple_sha).invocations[0].stopped, expected, receipt)
        # A job with no receipt, a second receipt and a running job prove nothing.
        history = self.history([(70, 1)], {(70, 1): [self.job(1, status='in_progress', conclusion=None)]})
        self.assertEqual(history.invocations('0.43.0', tuple_sha).invocations[0].stopped, 'unknown')
        history = self.history([(70, 1)], {(70, 1): [self.job(1)]}, {70: [self.artifact(92, 1), self.artifact(93, 1)]},
                               {92: self.stop(70, 1, 1, 'before_attempt'), 93: self.stop(70, 1, 1, 'before_attempt')})
        self.assertEqual(history.invocations('0.43.0', tuple_sha).invocations[0].stopped, 'unknown')
        # An unreadable list is unavailable, never an empty history.
        facts = mock.Mock()
        facts.release_runs.side_effect = ControllerError('the run list is truncated')
        with self.assertRaises(ControllerError):
            SIGN.GitHubHistory(facts).invocations('0.43.0', tuple_sha)

    def transport_facts(self, routes=None, downloads=None):
        replies = {('GET', '/repos/adrenickchrome/rdy-core'):
                   [GITHUB.Response(200, {}, json.dumps({'id': GITHUB.REPOSITORY_ID, 'full_name': GITHUB.REPOSITORY}).encode())]}
        for key, value in (routes or {}).items():
            replies[('GET', key)] = [value]

        class Transport(object):
            def __init__(self):
                self.calls = []

            def request(self, method, path, body=None, limit=None):
                self.calls.append(path)
                reply = replies.get((method, path))
                return reply[0] if reply else GITHUB.Response(404, {}, b'{}')

            def download(self, url, sink, limit):
                self.calls.append(url)
                sink.write(downloads[url])
                return 200
        transport = Transport()
        verifier = Path(self.tmp, 'qa-release-trust')
        return SIGN.LiveFacts(transport, Path(tempfile.mkdtemp(dir=self.tmp)), verifier, lambda attempt: None, FakeRunner()), transport

    def test_rsig6_live_facts_read_runs_and_artifacts(self):
        run = {'id': 70, 'run_attempt': 2, 'head_branch': 'candidate/v0.43.0', 'path': '.github/workflows/release.yml'}
        other = {'id': 71, 'run_attempt': 1, 'head_branch': 'candidate/v0.43.0', 'path': '.github/workflows/other.yml'}
        path = ('/repos/adrenickchrome/rdy-core/actions/workflows/release.yml/runs?branch=candidate%2Fv0.43.0'
                '&per_page=100&page=1')
        facts, transport = self.transport_facts(
            {path: GITHUB.Response(200, {}, json.dumps({'total_count': 2, 'workflow_runs': [run, other]}).encode())})
        with self.assertRaises(ControllerError):
            facts.release_runs('0.43.0')  # a run of another workflow under this name is refused
        facts, _ = self.transport_facts(
            {path: GITHUB.Response(200, {}, json.dumps({'total_count': 1, 'workflow_runs': [run]}).encode())})
        self.assertEqual(facts.release_runs('0.43.0'), [(70, 2)])
        # artifact_member: one download per artifact, exact member, bounded.
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w') as archive:
            archive.writestr('candidate.json', b'{}\n')
        data = buffer.getvalue()
        digest = hashlib.sha256(data).hexdigest()
        artifact = GITHUB.ArtifactFact(44, 'rdy-candidate-a1', len(data), digest, False, 45, GITHUB.REPOSITORY_ID,
                                       GITHUB.REPOSITORY_ID, S, digest)
        url = 'https://example.invalid/download'
        facts, transport = self.transport_facts(downloads={url: data})
        with mock.patch.object(facts, '_redirect', return_value=url):
            self.assertEqual(facts.artifact_member(artifact, 'candidate.json', 64), b'{}\n')
            self.assertEqual(facts.artifact_member(artifact, 'candidate.json', 64), b'{}\n')
            self.assertEqual(transport.calls.count(url), 1)
            with self.assertRaises(ControllerError):
                facts.artifact_member(artifact, 'candidate.json', 1)
            with self.assertRaises(ControllerError):
                facts.artifact_member(artifact, 'absent', 64)

    def test_rsig6_release_trust_reads_the_policy_commit_through_the_verifier(self):
        records = {'release-keys/records.txt': b'transition-v0.42.0.json transition-v0.42.0.json.sig\n',
                   'release-keys/transition-v0.42.0.json': b'{"t":1}\n',
                   'release-keys/transition-v0.42.0.json.sig': b'sig\n', 'release-keys/policy.json': b'{"p":1}\n',
                   'release-keys/evidence-policy.json': b'{"e":1}\n'}
        answer = {'version': '0.43.0', 'fingerprint': FINGERPRINT, 'signer_line': 'line',
                  'transition_sha256': 'a' * 64, 'policy_commit': ctl.P}
        verifier = Path(self.tmp, 'qa-release-trust')
        seen = {}

        def on_call(argv):
            directory = Path(argv[argv.index('--keys') + 1])
            seen['files'] = {p.name: p.read_bytes() for p in directory.iterdir()}
            seen['mode'] = stat.S_IMODE(directory.stat().st_mode)
        runner = FakeRunner((0, json.dumps(answer).encode(), b''))
        runner.on_call = on_call
        facts = SIGN.LiveFacts(mock.Mock(), Path(tempfile.mkdtemp(dir=self.tmp)), verifier, lambda attempt: None, runner)

        def contents(path, sha, limit):
            self.assertEqual(sha, ctl.P)
            if path not in records:
                raise GITHUB.Absent('contents', 'not found', 404)
            return mock.Mock(data=records[path])
        with mock.patch.object(facts, 'contents', contents):
            self.assertEqual(facts.release_trust('0.43.0', ctl.P, True), answer)
            argv = runner.calls[0]['argv']
            self.assertEqual(argv[:2], [str(verifier), 'select'])
            self.assertIn('--signing', argv)
            self.assertEqual(argv[argv.index('--policy-commit') + 1], ctl.P)
            self.assertEqual(seen['files'], {k.split('/')[1]: v for k, v in records.items()})
            self.assertEqual(seen['mode'], 0o700)
            # A revocation keeps its text, so the controller names it.
            runner.replies.append((1, b'', b'release trust: the key for v0.43.0 is revoked for new signing\n'))
            with self.assertRaises(ControllerError) as caught:
                facts.release_trust('0.43.0', ctl.P, True)
            self.assertIn('is revoked for new signing', str(caught.exception))
            # A hostile index name never leaves the directory.
            records['release-keys/records.txt'] = b'../evil.json ../evil.json.sig\n'
            with self.assertRaises(ControllerError):
                facts.release_trust('0.43.0', ctl.P, True)

    def test_rsig6_approval_responses_are_the_nine_kinds_in_order(self):
        channel = {'workflow_path': '.github/workflows/release-approval.yml', 'implementation_sha': '4' * 40}
        files = {('release-keys/evidence-policy.json', ctl.P): b'{"e":1}\n',
                 (channel['workflow_path'], '5' * 40): b'workflow\n', (channel['workflow_path'], '4' * 40): b'implementation\n'}
        artifact = mock.Mock(digest='a' * 64, size=321, artifact_id=44, head_sha='5' * 40)
        facts = SIGN.LiveFacts(mock.Mock(), Path(tempfile.mkdtemp(dir=self.tmp)), Path('/w/v'), lambda attempt: None, FakeRunner())

        def raw(operation, path, limit):
            return mock.Mock(body=path.encode())
        with mock.patch.object(facts, '_send', raw), \
                mock.patch.object(facts, 'repository'), \
                mock.patch.object(facts, 'artifact', return_value=artifact), \
                mock.patch.object(facts, 'artifact_member', return_value=b'request'), \
                mock.patch.object(facts, 'run_attempt', return_value=mock.Mock(path=channel['workflow_path'], head_sha='5' * 40)), \
                mock.patch.object(facts, 'contents', lambda path, sha, limit: mock.Mock(data=files[(path, sha)])), \
                mock.patch.object(SIGN.ev, 'parse_approval_request', return_value=mock.Mock(value={'policy_commit': ctl.P})), \
                mock.patch.object(SIGN.ev, 'parse_evidence_policy', return_value=mock.Mock(value={'approval_channel': channel})):
            answer = facts.approval_responses(6001, 44)
        self.assertEqual(EVIDENCE.check_source_responses(answer, 'the test'), answer)
        by_kind = {entry['kind']: entry for entry in answer}
        self.assertEqual((by_kind['request_archive']['sha256'], by_kind['request_archive']['bytes']), ('a' * 64, 321))
        self.assertEqual(by_kind['protected_policy']['sha256'], hashlib.sha256(b'{"e":1}\n').hexdigest())
        self.assertEqual(by_kind['workflow_source']['sha256'], hashlib.sha256(b'workflow\n').hexdigest())
        self.assertEqual(by_kind['implementation_source']['sha256'], hashlib.sha256(b'implementation\n').hexdigest())

    def test_rsig6_holder_is_the_one_running_signer_job(self):
        facts = mock.Mock()
        facts.attempt_jobs.return_value = [self.job(5, status='in_progress', conclusion=None), self.job(6, name='Reserve release tuple')]
        self.assertEqual(SIGN.find_holder(facts, 7, 1), CONTROLLER.Holder(7, 1, 5))
        for jobs in ([], [self.job(5)], [self.job(5, status='in_progress', conclusion=None)] * 2):
            facts.attempt_jobs.return_value = jobs
            with self.assertRaises(ControllerError):
                SIGN.find_holder(facts, 7, 1)

    def test_rsig6_cli_refuses_bad_arguments_and_exits_two(self):
        for argv in ([], ['rehearse'], ['live'], ['live', '--signer-sha', 'x'], ['claims', '--unknown', '1']):
            with self.subTest(argv=argv):
                done = subprocess.run([sys.executable, '-I', '-B', str(SCRIPTS / 'sign.py')] + argv, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, env={'PATH': os.environ.get('PATH', '')})
                self.assertEqual(done.returncode, 2, done.stderr)
        # The adapter's own entry stays closed.
        done = subprocess.run([sys.executable, '-I', '-B', str(SCRIPTS / 'release_sign.py')], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, env={'PATH': os.environ.get('PATH', '')})
        self.assertNotEqual(done.returncode, 0)
        self.assertIn(b'No standalone live signer', done.stderr)


@dataclasses.dataclass(frozen=True)
class _Job(object):
    job_id: int
    name: str
    status: str
    conclusion: object
    head_sha: str


@dataclasses.dataclass(frozen=True)
class _Artifact(object):
    artifact_id: int
    name: str
    expired: bool
    head_sha: str


if __name__ == '__main__':
    unittest.main()
