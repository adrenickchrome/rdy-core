"""Reservation, durable Sign attempt and agreed exclusion state, with fake remote services only.

No test reaches the network, GitHub, AWS, DynamoDB, a key or a signature. Every identity is synthetic.
The eligibility `World` supplies one consistent release. `FakeTable` keeps DynamoDB's conditional
semantics and survives every controller restart: it is the remote journal. Time, process death and
verifier outcomes are injected. A "signature" here is opaque bytes that only the fake verifier knows.
"""
import copy
import dataclasses
import hashlib
import io
import json
from pathlib import Path
import pickle
import struct
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_release_eligibility as base  # noqa: E402

# The release scripts import each other: one module, one error class.
for name, module in (('candidate', base.CANDIDATE), ('evidence', base.EVIDENCE), ('github', base.GITHUB),
                     ('eligibility', base.ELIGIBILITY)):
    sys.modules[name] = module
CONTROLLER = base.load('controller')
GITHUB = base.GITHUB
EVIDENCE = base.EVIDENCE
ELIGIBILITY = base.ELIGIBILITY
ControllerError = base.ControllerError
Refused = CONTROLLER.Refused
sha, canon, digest = base.sha, base.canon, base.digest
World, S, P, H, HEAD = base.World, base.S, base.P, base.H, base.HEAD
REPO, REPO_ID = base.REPO, base.REPO_ID
KEY_ARN, FINGERPRINT = base.KEY_ARN, base.FINGERPRINT
DISPATCH_RUN = base.DISPATCH_RUN

T0 = 1800000000
T0 = 1800000000
ADMISSION = 'release-keys/admission.json'
TABLE_ID = base.LINEAGE['table_id']
ACCOUNT, REGION, TABLE = base.ACCOUNT, 'us-east-2', 'rdy-test-release-control'
LANE = 'rdy-core-release-authority'
AUTHORITY = {'schema': 1, 'repository': REPO, 'repository_id': str(REPO_ID), 'owner_id': '32169767',
             'account_id': ACCOUNT, 'region': REGION, 'table': TABLE, 'table_id': TABLE_ID,
             'admission_ref': 'refs/heads/main', 'admission_path': ADMISSION, 'concurrency_group': LANE}
BOOTSTRAP = {'signer_workflow_sha': H, 'policy_commit': P, 'policy_sha256': digest(41), 'trust_sha256': digest(42),
             'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT, 'first_machine_tag': 'v0.42.0', 'epoch': '1'}
GENESIS = canon({'schema': 1, 'authority': AUTHORITY, 'bootstrap': BOOTSTRAP})
AUTHORITY_SHA, GENESIS_SHA = sha(canon(AUTHORITY)), sha(GENESIS)
CURRENT_KEY = ('CONTROL#' + str(REPO_ID), 'CURRENT')
GENESIS_KEY = ('CONTROL#' + str(REPO_ID), 'GENESIS')
EPOCH = '7'
SIGN_JOB, RESERVE_JOB_ID, RECEIPT_ART = 9100, 9099, 9200
SIGNATURE = b'-----BEGIN SSH SIGNATURE-----\nfake output bytes\n-----END SSH SIGNATURE-----\n'


class Death(BaseException):
    """The fake process dies here; nothing below it runs. Only the remote state survives."""


class Clock(object):
    def __init__(self):
        self.value = T0

    def now(self):
        return self.value


class FakeTable(object):
    """DynamoDB's semantics for the one table: strict about the wire, deliberately small otherwise."""

    def __init__(self):
        self.items = {}
        self.tokens = []
        self.writes = []
        self.reads = []
        self.faults = []
        self.read_faults = []
        self.unavailable = False
        self.hook = None
        self.arn = 'arn:aws:dynamodb:' + REGION + ':' + ACCOUNT + ':table/' + TABLE
        self.described = dict(
            TableArn=self.arn, TableId=TABLE_ID, TableStatus='ACTIVE', TableName=TABLE,
            KeySchema=[{'AttributeName': 'pk', 'KeyType': 'HASH'}, {'AttributeName': 'sk', 'KeyType': 'RANGE'}])

    def seed(self, pk, sk, body):
        self.items[(pk, sk)] = body

    def describe_table(self, arn, timeout):
        self._check_up(timeout)
        if arn != self.arn:
            raise AssertionError('a table other than the enrolled table was named')
        return copy.deepcopy(self.described)

    def _check_up(self, timeout):
        if not (isinstance(timeout, (int, float)) and timeout > 0):
            raise AssertionError('a provider request needs a positive timeout')
        if self.unavailable:
            raise CONTROLLER.JournalUnavailable('the journal is unavailable')

    @staticmethod
    def _item(key, body):
        return {'pk': {'S': key[0]}, 'sk': {'S': key[1]}, 'body': {'B': body}, 'body_sha256': {'S': sha(body)}}

    def get_item(self, pk, sk, timeout):
        self._check_up(timeout)
        self.reads.append(('get', pk, sk))
        if any(pk.startswith(prefix) for prefix in self.read_faults):
            raise CONTROLLER.JournalUnavailable('the journal is unavailable')
        body = self.items.get((pk, sk))
        return None if body is None else self._item((pk, sk), body)

    def transact_get_items(self, keys, timeout):
        self._check_up(timeout)
        self.reads.append(('snapshot', tuple(keys)))
        return [None if tuple(k) not in self.items else self._item(tuple(k), self.items[tuple(k)]) for k in keys]

    def fail_next(self, fragment, mode):
        self.faults.append((fragment, mode))

    def _key(self, action):
        if 'Item' in action:
            return (action['Item']['pk']['S'], action['Item']['sk']['S'])
        return (action['Key']['pk']['S'], action['Key']['sk']['S'])

    def _exact_values(self, action, new):
        if action['ExpressionAttributeNames'] != {'#b': 'body', '#d': 'body_sha256'}:
            raise AssertionError('wrong attribute aliases')
        values = action['ExpressionAttributeValues']
        expected = {':old', ':oldhash'} | ({':new', ':newhash'} if new else set())
        if set(values) != expected or sha(values[':old']['B']) != values[':oldhash']['S']:
            raise AssertionError('wrong expression values')
        if new and sha(values[':new']['B']) != values[':newhash']['S']:
            raise AssertionError('wrong new hash')
        return values

    def transact_write_items(self, items, token, timeout):
        self._check_up(timeout)
        if token in self.tokens or not (isinstance(token, str) and 1 <= len(token) <= 36):
            raise AssertionError('a request token was reused or malformed')
        self.tokens.append(token)
        keys = []
        for item in items:
            (kind, action), = item.items()
            keys.append(self._key(action))
            if action['TableName'] != TABLE or action.get('ReturnValuesOnConditionCheckFailure') != 'NONE':
                raise AssertionError('wrong table or return values')
        if len(set(keys)) != len(keys):
            raise AssertionError('one item appears twice in a transaction')
        mode = None
        for fault in list(self.faults):
            if any(fault[0] in key[0] or fault[0] == key[1] for key in keys):
                self.faults.remove(fault)
                mode = fault[1]
                break
        if self.hook is not None:
            hook, self.hook = self.hook, None
            hook()
        if mode == 'unsent':
            raise CONTROLLER.JournalOutcomeUnknown('the request timed out')
        if mode == 'down':
            raise CONTROLLER.JournalUnavailable('the journal is unavailable')
        for item in items:
            self._condition(item)
        for item in items:
            (kind, action), = item.items()
            if kind == 'Put':
                self.items[self._key(action)] = action['Item']['body']['B']
            elif kind == 'Update':
                self.items[self._key(action)] = action['ExpressionAttributeValues'][':new']['B']
        self.writes.append(tuple(keys))
        if mode == 'lost_ack':
            raise CONTROLLER.JournalOutcomeUnknown('the response was lost')
        return CONTROLLER.WriteAck(underlying_requests=2 if mode == 'replay' else 1)

    def _condition(self, item):
        (kind, action), = item.items()
        key = self._key(action)
        current = self.items.get(key)
        condition = action['ConditionExpression']
        if kind == 'Put':
            if condition != 'attribute_not_exists(pk) AND attribute_not_exists(sk)':
                raise AssertionError('a Put is not create-only')
            if set(action['Item']) != {'pk', 'sk', 'body', 'body_sha256'}:
                raise AssertionError('an item has other attributes')
            if sha(action['Item']['body']['B']) != action['Item']['body_sha256']['S']:
                raise AssertionError('wrong body hash')
            if current is not None:
                raise CONTROLLER.JournalConditionFailed('the item exists')
        elif kind == 'Update':
            if condition != '#b = :old AND #d = :oldhash' or action['UpdateExpression'] != 'SET #b = :new, #d = :newhash':
                raise AssertionError('an Update is not an exact-body compare-and-swap')
            if current != self._exact_values(action, True)[':old']['B']:
                raise CONTROLLER.JournalConditionFailed('the item changed')
        elif condition == 'attribute_not_exists(pk) AND attribute_not_exists(sk)':
            if current is not None:
                raise CONTROLLER.JournalConditionFailed('the item exists')
        elif condition == '#b = :old AND #d = :oldhash':
            if current != self._exact_values(action, False)[':old']['B']:
                raise CONTROLLER.JournalConditionFailed('the item changed')
        else:
            raise AssertionError('an unknown condition')


class FakeGit(base.FakeFacts):
    """GitHub for the reserve job and the signer: the eligibility facts plus the two create-only writes.
    It has no ref update or delete. Ancestry comes from the commit graph, unless a test pins an answer."""

    def __init__(self, world):
        base.FakeFacts.__init__(self, world)
        self.tag_faults = []
        self.ref_faults = []
        self.created = []
        self.after_ref = None
        self.next_tag = 0
        self.after_admission_read = None

    def contents(self, path, ref_sha, limit=GITHUB.CONTENTS_MAX):
        found = base.FakeFacts.contents(self, path, ref_sha, limit)
        if path == ADMISSION and self.after_admission_read:
            self.after_admission_read()
        return found

    def compare(self, base_sha, head):
        self._hit('compare', base_sha, head)
        status = self.w.compares.get((base_sha, head))
        if status is None:
            status = self._graph(base_sha, head)
        return GITHUB.CompareFact(base_sha, head, status, 1, 0, base_sha, digest(93))

    def _graph(self, base_sha, head):
        if base_sha == head:
            return 'identical'
        if self._reaches(head, base_sha):
            return 'ahead'
        if self._reaches(base_sha, head):
            return 'behind'
        return 'diverged'

    def _reaches(self, start, target):
        seen, stack = set(), [start]
        while stack:
            sha_ = stack.pop()
            if sha_ == target:
                return True
            if sha_ not in seen:
                seen.add(sha_)
                stack.extend(self.w.commits.get(sha_, ()))
        return False

    def create_tag_object(self, tag, message, commit_sha):
        self.created.append('tag object')
        if self.tag_faults:
            fault = self.tag_faults.pop(0)
            if fault[1]:
                fault[1]()
            raise GITHUB.ReconcileRequired('create tag object', fault[0])
        self.next_tag += 1
        tag_sha = format(0xeee000 + self.next_tag, '040x')
        fact = dict(sha=tag_sha, tag=tag, message_bytes=message.encode(), object_type='commit', object_sha=commit_sha,
                    sha256=digest(94))
        self.w.tags[tag_sha] = fact
        return GITHUB.TagFact(**fact)

    def create_ref(self, ref, object_sha):
        self.created.append('ref')
        if self.ref_faults:
            fault = self.ref_faults.pop(0)
            if fault[1]:
                fault[1]()
            raise GITHUB.ReconcileRequired('create ref', fault[0])
        name = ref[len('refs/'):]
        if name in self.w.refs:
            raise GITHUB.ReconcileRequired('create ref', 'HTTP 422', 422)
        self.w.refs[name] = ('tag', object_sha)
        if self.after_ref:
            self.after_ref()
        return GITHUB.RefFact(ref, 'tag', object_sha, digest(91))


class FakeVerifier(object):
    def __init__(self):
        self.calls = []
        self.good = {SIGNATURE}

    def verify(self, signature, manifest, signer_line, principal, namespace):
        self.calls.append((signature, manifest, signer_line, principal, namespace))
        return signature in self.good


class FakeKms(object):
    def __init__(self):
        self.calls = []

    def get_public_key(self):
        self.calls.append('get-public-key')

    def sign(self):
        self.calls.append('sign')
        return SIGNATURE

    @property
    def signs(self):
        return self.calls.count('sign')


class FakeProvider(object):
    """The OIDC issuer and STS. The token is opaque; the controller keeps only nonsecret receipts."""

    def __init__(self, clock, lifetime=900, role_max=3600, role_arn=base.ROLE_ARN):
        self.clock, self.lifetime, self.role_max, self.role_arn = clock, lifetime, role_max, role_arn
        self.oidc, self.sts = [], []
        self.token = 'synthetic-oidc-token-value'

    def request_oidc(self, audience):
        self.oidc.append(audience)
        return CONTROLLER.OidcToken(token=self.token, iat=self.clock.now(), exp=self.clock.now() + 300)

    def assume_role(self, token, duration_seconds):
        self.sts.append((token, duration_seconds))
        return CONTROLLER.StsReceipt(expiration=self.clock.now() + self.lifetime, role_arn=self.role_arn,
                                     requested_duration=duration_seconds, role_max_session_duration=self.role_max)


class FakeEvidence(object):
    def __init__(self):
        self.rows = []
        self.broken = False

    def boundaries(self, holder):
        if self.broken:
            raise ControllerError('run evidence is unavailable')
        return [r for r in self.rows if r[0] == holder]

    def record(self, holder, name, facts):
        if self.broken:
            raise ControllerError('run evidence is unavailable')
        self.rows.append((holder, name, dict(facts)))


class FakeHistory(object):
    def __init__(self):
        self.answer = CONTROLLER.History(complete=True, invocations=())
        self.error = None

    def invocations(self, version, tuple_sha256):
        if self.error:
            raise self.error
        return self.answer


class FakeWriter(object):
    """The protected admission writer of planned maintenance and emergency containment. Each write is
    a new protected commit on main that carries the other protected files forward."""

    def __init__(self, world):
        self.w = world
        self.fail = False
        self.commits = []
        self.before = None

    def commit(self, data):
        if self.fail:
            raise ControllerError('the admission commit failed')
        if self.before:
            self.before(data)
        old = self.w.refs['heads/main'][1]
        new = format(0xa000 + len(self.w.commits), '040x')
        self.w.commits[new] = (old,)
        for (path, commit), body in list(self.w.files.items()):
            if commit == old:
                self.w.files[(path, new)] = body
        self.w.put(new, ADMISSION, data)
        self.w.refs['heads/main'] = ('commit', new)
        self.commits.append(data)
        return new


class FakePublished(object):
    """The exact-byte verifier, by its interface: it answers what it read and verified, or it refuses."""

    def __init__(self):
        self.answer = None
        self.error = ControllerError('the published signature is missing')
        self.calls = []

    def verify_published(self, reconciliation):
        self.calls.append(reconciliation.tuple_sha256)
        if self.answer is None:
            raise self.error
        return self.answer


class Rig(object):
    """One release, one table, one clock. `controller()` is a fresh process over the same remote state."""

    def __init__(self, world=None):
        self.w = world or World()
        w = self.w
        self.git = FakeGit(w)
        self.clock = Clock()
        self.table = FakeTable()
        self.kms = FakeKms()
        self.verifier = FakeVerifier()
        self.evidence = FakeEvidence()
        self.history = FakeHistory()
        self.published = FakePublished()
        self.providers = []
        self.enrollment = CONTROLLER.Enrollment(
            account_id=ACCOUNT, region=REGION, table=TABLE, table_id=TABLE_ID, genesis_sha256=GENESIS_SHA,
            authority_sha256=AUTHORITY_SHA, signer_workflow_sha=H)
        w.job(DISPATCH_RUN, 1, RESERVE_JOB_ID, CONTROLLER.RESERVE_JOB)
        self.add_job(DISPATCH_RUN, 1, SIGN_JOB)
        self.table.seed(*GENESIS_KEY, GENESIS)
        self.table.seed('FENCE#' + str(REPO_ID) + '#' + EPOCH, 'COUNTER', self.fence_body(EPOCH, '1'))
        self.install()

    # -- building blocks ---------------------------------------------------------------------

    def add_job(self, run_id, attempt, job_id, status='in_progress'):
        self.w.runs.setdefault((run_id, attempt), dict(self.w.runs[(run_id, 1)], run_attempt=attempt))
        self.w.job(run_id, attempt, job_id, 'Sign')
        self.w.find_job(job_id).update(status=status, conclusion=None)

    def fence_body(self, epoch, fence):
        return canon({'schema': 1, 'repository_id': str(REPO_ID), 'epoch': epoch, 'fence': fence})

    def head(self):
        return self.w.refs['heads/main'][1]

    def record(self, state='ACTIVE', epoch=EPOCH, holder=None, **changes):
        policy = self.w.files[('release-keys/policy.json', P)]
        value = {'schema': 1, 'repository_id': str(REPO_ID), 'epoch': epoch, 'state': state,
                 'signer_workflow_sha': H, 'key_arn': self.w.policy['key_arn'], 'fingerprint': FINGERPRINT,
                 'policy_commit': P, 'policy_sha256': sha(policy), 'trust_sha256': self.w.policy['records_sha256'],
                 'table_id': TABLE_ID, 'genesis_sha256': GENESIS_SHA, 'draining_holder': holder}
        value.update(changes)
        return value

    def install(self, state='ACTIVE', epoch=EPOCH, holder=None, mirror=True, github=True, revision='1', floor=P,
                **changes):
        admission = self.record(state, epoch, holder, **changes)
        data = canon(admission)
        if github:
            self.w.put(self.head(), ADMISSION, data)
        if mirror:
            self.table.items[CURRENT_KEY] = canon({'schema': 1, 'revision': revision, 'admission': admission,
                                                   'admission_sha256': sha(data), 'admission_commit_floor': floor})

    def current(self):
        return json.loads(self.table.items[CURRENT_KEY])

    def admission(self):
        return json.loads(self.w.files[(ADMISSION, self.head())])

    def holder(self, run_id=DISPATCH_RUN, attempt=1, job_id=SIGN_JOB):
        return CONTROLLER.Holder(run_id, attempt, job_id)

    def named(self, ctrl, **changes):
        value = {'run_id': str(ctrl.holder.run_id), 'run_attempt': str(ctrl.holder.run_attempt),
                 'job_id': str(ctrl.holder.job_id), 'grant_id': ctrl.grant_id}
        value.update(changes)
        return value

    def request(self):
        return self.w.request()

    def qualify(self):
        return ELIGIBILITY.qualify_new(self.request(), self.git)

    def reserve_version(self, holder=None):
        """The reserve job: qualifies, creates the reservation and uploads its receipt."""
        data = CONTROLLER.create_reservation(self.git, self.qualify(), holder or self.holder(job_id=RESERVE_JOB_ID))
        self.upload_receipt(data)
        return data

    def upload_receipt(self, data, attempt=1, art=RECEIPT_ART):
        self.w.artifacts[art] = self.w.artifact(art, 'reserve-receipt-' + str(attempt), DISPATCH_RUN, digest(19), 256)
        self.w.members[(art, CONTROLLER.RESERVE_MEMBER)] = data

    def controller(self, holder=None, artifact=RECEIPT_ART, request=None, **changes):
        arguments = dict(request=request or self.request(), facts=self.git, enrollment=self.enrollment,
                         holder=holder or self.holder(), clock=self.clock, verifier=self.verifier,
                         history=self.history, run_evidence=self.evidence, reserve_artifact_id=artifact,
                         published_verifier=self.published)
        arguments.update(changes)
        return CONTROLLER.Controller(**arguments)

    def provider(self, **changes):
        provider = FakeProvider(self.clock, **changes)
        self.providers.append(provider)
        return provider

    def session(self, ctrl=None, provider=None):
        ctrl = ctrl or self.controller()
        ctrl.admit()
        ctrl.issue_credentials(provider or self.provider())
        ctrl.open_session(self.table)
        return ctrl

    def ready(self):
        self.reserve_version()
        return self.session()

    def rerun(self, attempt=2, **changes):
        """A later attempt of the release run, as a fresh process with its own job."""
        job_id = SIGN_JOB + attempt - 1
        self.add_job(DISPATCH_RUN, attempt, job_id)
        return self.controller(self.holder(DISPATCH_RUN, attempt, job_id), artifact=None,
                               request=self.w.request(dispatch_run_attempt=attempt), **changes)

    def adapter_args(self):
        return dict(repository=REPO, version=self.w.v, manifest_sha256=self.w.manifest,
                    key_arn=self.w.policy['key_arn'], fingerprint=FINGERPRINT,
                    first_machine_tag=self.w.policy['first_machine_tag'])

    def manifest_bytes(self):
        return ''.join(f['sha256'] + '  ' + f['name'] + '\n' for f in self.w.archives).encode()

    def payload_digest(self):
        def field(data):
            return struct.pack('>I', len(data)) + data
        preimage = (b'SSHSIG' + field(b'rdy-release-v1') + field(b'') + field(b'sha512')
                    + field(hashlib.sha512(self.manifest_bytes()).digest()))
        return sha(preimage)

    def sign(self, ctrl, die_at=None):
        """What the wrapped adapter does, with a process death at a named boundary."""
        def die(point):
            if die_at == point:
                raise Death(point)
        with ctrl.reserve(**self.adapter_args()):
            ctrl.before_public_key()
            self.kms.get_public_key()
            die('before attempt')
            permit = ctrl.before_sign(self.payload_digest())
            die('after attempt acknowledgement')
            permit.consume()
            self.kms.calls.append('sign')
            die('after dispatch')
            signature = SIGNATURE
            die('after response')
        die('before signature persistence')
        ctrl.retain_signature(signature)
        return signature

    def tuple_bytes(self):
        return canon(self.w.expected_tuple())

    def tuple_sha(self):
        return sha(self.tuple_bytes())

    def tuple_pk(self):
        return 'TUPLE#' + str(REPO_ID) + '#' + self.w.v + '#' + self.tuple_sha()

    def version_key(self):
        return ('VERSION#' + str(REPO_ID) + '#' + self.w.v, 'RESERVATION')

    def row(self):
        return json.loads(self.table.items[self.version_key()])

    def seed_archive(self, receipt):
        """An accepted archive in the enrolled table, with its enrolled lineage."""
        base.accept(self.w, receipt, genesis_sha256=GENESIS_SHA, authority_sha256=AUTHORITY_SHA)
        self.table.seed('APPROVAL#' + receipt, 'RECORD', self.w.archive[receipt])
        return self.w.archive[receipt]

    def publish(self):
        """The version is published at S, and the exact-byte path verifies it against the tuple."""
        w = self.w
        w.refs['tags/v' + w.v] = ('commit', S)
        release = dict(release_id=500, tag_name='v' + w.v, target_commitish=S, draft=False, prerelease=True, assets=(1,))
        w.releases.append(release)
        self.published.answer = CONTROLLER.PublishedBytes(
            release_id=500, tag_name='v' + w.v, commit=S, draft=False, manifest_sha256=w.manifest,
            archives=tuple((f['name'], f['sha256'], f['size']) for f in w.archives),
            notes_sha256=sha(w.notes), signature_sha256=sha(SIGNATURE), fingerprint=FINGERPRINT,
            principal='rdy-release', namespace='rdy-release-v1', signature_verified=True, assets_complete=True)
        return release


class Base(unittest.TestCase):
    def refused(self, text, call, *args, **kwargs):
        with self.assertRaises((Refused, ELIGIBILITY.Refused)) as caught:
            call(*args, **kwargs)
        self.assertEqual(str(caught.exception), text)

    def refused_code(self, code, call, *args, **kwargs):
        with self.assertRaises((Refused, ELIGIBILITY.Refused)) as caught:
            call(*args, **kwargs)
        self.assertEqual(caught.exception.code, code, str(caught.exception))
        return caught.exception


class Reservation(Base):
    def test_rsig5_admission_fixture_is_the_core_control_file(self):
        # The Rust trust loader's test copies this exact file into a complete enrolled directory.
        fixture = (Path(__file__).resolve().parents[2]
                   / 'crates/rdy-qa/fixtures/release-keys/control/admission.json')
        self.assertEqual(CONTROLLER.ADMISSION_PATH, 'release-keys/' + fixture.name)
        value = CONTROLLER._check_admission(
            CONTROLLER._strict(fixture.read_bytes(), CONTROLLER.ADMISSION_KEYS, 'the admission record'))
        self.assertEqual(value['state'], 'ACTIVE')

    def test_rsig5_create_only_reservation_race(self):
        rig = Rig()
        qualified = rig.qualify()
        loser = rig.qualify()
        # Object creation alone reserves nothing; a new reservation is still possible.
        rig.git.create_tag_object('reserve/v' + rig.w.v, qualified.tuple_bytes.decode(), S)
        self.assertNotIn('tags/reserve/v' + rig.w.v, rig.w.refs)
        # Both qualified before either wrote. One ref create wins; the other reconciles to the winner.
        first = CONTROLLER.create_reservation(rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID))
        winner = json.loads(first)
        self.assertEqual(winner['created'], True)
        object_sha = rig.w.refs['tags/reserve/v' + rig.w.v][1]
        self.assertEqual(winner['reservation_object'], object_sha)
        rig.git.created = []
        second = json.loads(CONTROLLER.create_reservation(rig.git, loser, rig.holder(DISPATCH_RUN, 2, 9098)))
        self.assertEqual((second['created'], second['reservation_object']), (False, object_sha))
        self.assertEqual(rig.git.created, [], 'the existing exact reservation needs no write')
        self.assertEqual(rig.w.refs['tags/reserve/v' + rig.w.v], ('tag', object_sha))
        # A race the loser reaches only at the ref write: the same exact winner, one write attempt.
        rig = Rig()
        qualified = rig.qualify()
        winner_sha = format(0xabc, '040x')

        def win(data=None):
            rig.w.refs['tags/reserve/v' + rig.w.v] = ('tag', winner_sha)
            rig.w.tags[winner_sha] = dict(sha=winner_sha, tag='reserve/v' + rig.w.v, object_type='commit', object_sha=S,
                                          message_bytes=data or qualified.tuple_bytes, sha256=digest(94))
        rig.git.ref_faults.append(('HTTP 422', win))
        answer = json.loads(CONTROLLER.create_reservation(rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID)))
        self.assertEqual((answer['created'], answer['reservation_object']), (False, winner_sha))
        self.assertEqual(rig.git.created, ['tag object', 'ref'])
        # A winner with another tuple is a conflict.
        rig = Rig()
        qualified = rig.qualify()
        other = rig.w.expected_tuple()
        other['release_evidence_sha256'] = digest(77)

        def win_other():
            rig.w.refs['tags/reserve/v' + rig.w.v] = ('tag', winner_sha)
            rig.w.tags[winner_sha] = dict(sha=winner_sha, tag='reserve/v' + rig.w.v, object_type='commit', object_sha=S,
                                          message_bytes=canon(other), sha256=digest(94))
        rig.git.ref_faults.append(('HTTP 422', win_other))
        self.refused('reservation conflict: the reservation message is not the qualified tuple',
                     CONTROLLER.create_reservation, rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID))

    def changes(self):
        def tag(rig, **fields):
            rig.w.tags[rig.w.refs['tags/reserve/v' + rig.w.v][1]].update(fields)

        def drop(rig, path):
            del rig.w.files[(path, S)]
        return (
            ('ref missing', lambda r: r.w.refs.pop('tags/reserve/v' + r.w.v),
             'reservation conflict: the reservation of v0.43.0 is missing'),
            ('ref is lightweight', lambda r: r.w.refs.update({'tags/reserve/v' + r.w.v: ('commit', S)}),
             'reservation conflict: the reservation of v0.43.0 is not an annotated tag'),
            ('ref names another object', lambda r: r.w.refs.update({'tags/reserve/v' + r.w.v: ('tag', digest(1)[:40])}),
             'reservation conflict: the reservation object is unavailable'),
            ('object missing', lambda r: r.w.tags.pop(r.w.refs['tags/reserve/v' + r.w.v][1]),
             'reservation conflict: the reservation object is unavailable'),
            ('object has another name', lambda r: tag(r, tag='reserve/v0.99.0'),
             'reservation conflict: the reservation object is not reserve/v0.43.0'),
            ('object targets a tag', lambda r: tag(r, object_type='tag'),
             'reservation conflict: the reservation object does not target a commit directly'),
            ('object targets another commit', lambda r: tag(r, object_sha=base.OTHER),
             'reservation conflict: the reservation object does not target the qualified commit'),
            ('message differs', lambda r: tag(r, message_bytes=b'{}\n'),
             'reservation conflict: the reservation message is not the qualified tuple'),
            ('source commit missing', lambda r: r.w.commits.pop(S),
             'reservation conflict: the reserved commit is unavailable'),
            ('Cargo version differs', lambda r: r.w.put(S, 'Cargo.toml', b'[workspace.package]\nversion = "0.99.0"\n'),
             'evidence mismatch: the Cargo version at the release commit is not 0.43.0'),
            ('source export differs', lambda r: r.w.put(S, 'release/source.json', canon(dict(r.w.source, version='0.99.0'))),
             'evidence mismatch: release/source.json does not match the release record'),
            ('notes differ', lambda r: r.w.put(S, 'release/notes.md', b'other\n'),
             'evidence mismatch: release/notes.md does not match its digest'),
            ('guide differs', lambda r: (r.w.put(S, 'release/AGENTS.md', b'o\n'), r.w.put(S, 'release/CLAUDE.md', b'o\n')),
             'evidence mismatch: the release guide does not match its digest'),
            ('source file missing', lambda r: drop(r, 'release/notes.md'),
             'eligibility incomplete: GitHub contents: not found'),
        )

    def test_rsig5_successful_create_requires_exact_readback_before_any_kms(self):
        for label, change, message in self.changes():
            rig = Rig()
            qualified = rig.qualify()
            rig.git.after_ref = lambda: change(rig)
            with self.subTest(step='after create', change=label):
                self.refused(message, CONTROLLER.create_reservation, rig.git, qualified,
                             rig.holder(job_id=RESERVE_JOB_ID))
                self.assertEqual(rig.kms.calls, [])
        # The same readback runs inside the signer context, before any KMS request, GetPublicKey included.
        for label, change, message in self.changes():
            rig = Rig()
            ctrl = rig.ready()
            before = dict(rig.table.items)
            change(rig)
            with self.subTest(step='inside reserve', change=label):
                with self.assertRaises(Refused):
                    rig.sign(ctrl)
                self.assertEqual(rig.kms.calls, [])
                self.assertEqual(rig.table.items, before)
        # The controller refuses a KMS read outside the reservation context.
        rig = Rig()
        ctrl = rig.ready()
        self.refused('authority unavailable: a KMS request needs the reservation context', ctrl.before_public_key)

    def test_rsig5_timeout_409_422_reconcile_without_blind_write_retry(self):
        for reason in ('timeout', 'HTTP 409', 'HTTP 422'):
            for write in ('tag', 'ref'):
                def faults(rig, effect=None):
                    (rig.git.tag_faults if write == 'tag' else rig.git.ref_faults).append((reason, effect))

                def written(rig):
                    return ['tag object'] if write == 'tag' else ['tag object', 'ref']
                # Unresolved absence stops. Nothing is written again.
                rig = Rig()
                qualified = rig.qualify()
                faults(rig)
                with self.subTest(reason=reason, write=write, outcome='absent'):
                    self.refused('reservation conflict: the reservation outcome is unresolved after ' + reason
                                 + '; reconcile again before any creation',
                                 CONTROLLER.create_reservation, rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID))
                    self.assertEqual(rig.git.created, written(rig))
                # An exact winner continues, marked as not freshly created.
                rig = Rig()
                qualified = rig.qualify()
                winner = digest(55)[:40]

                def exact(rig=rig, winner=winner, qualified=qualified):
                    rig.w.refs['tags/reserve/v' + rig.w.v] = ('tag', winner)
                    rig.w.tags[winner] = dict(sha=winner, tag='reserve/v' + rig.w.v, message_bytes=qualified.tuple_bytes,
                                              object_type='commit', object_sha=S, sha256=digest(94))
                faults(rig, exact)
                with self.subTest(reason=reason, write=write, outcome='exact winner'):
                    answer = json.loads(CONTROLLER.create_reservation(rig.git, qualified,
                                                                      rig.holder(job_id=RESERVE_JOB_ID)))
                    self.assertEqual((answer['created'], answer['reservation_object']), (False, winner))
                    self.assertEqual(rig.git.created, written(rig))
                # A lightweight tag or an unavailable read stops.
                rig = Rig()
                qualified = rig.qualify()
                faults(rig, lambda rig=rig: rig.w.refs.update({'tags/reserve/v' + rig.w.v: ('commit', S)}))
                with self.subTest(reason=reason, write=write, outcome='lightweight'):
                    self.refused('reservation conflict: the reservation of v0.43.0 is not an annotated tag',
                                 CONTROLLER.create_reservation, rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID))
                rig = Rig()
                qualified = rig.qualify()
                faults(rig, lambda rig=rig: rig.w.errors.update(ref=GITHUB.Uncertain('ref', 'timeout')))
                with self.subTest(reason=reason, write=write, outcome='unavailable read'):
                    self.refused('reservation conflict: the reservation read is unavailable',
                                 CONTROLLER.create_reservation, rig.git, qualified, rig.holder(job_id=RESERVE_JOB_ID))
                    self.assertEqual(rig.git.created, written(rig))
                self.assertEqual(rig.kms.calls, [])

    def test_rsig5_crash_never_releases_a_reservation(self):
        rig = Rig()
        rig.reserve_version()
        ref = rig.w.refs['tags/reserve/v' + rig.w.v]
        ctrl = rig.session()
        row = dict(rig.table.items)
        # An exception or process death inside the context leaves the reservation and the journal.
        with self.assertRaises(ValueError):
            with ctrl.reserve(**rig.adapter_args()):
                raise ValueError('the adapter failed')
        with self.assertRaises(Death):
            with ctrl.reserve(**rig.adapter_args()):
                raise Death('dead')
        self.assertEqual(rig.table.items, row)
        self.assertEqual(rig.w.refs['tags/reserve/v' + rig.w.v], ref)
        for name in ('delete', 'release', 'unlock', 'delete_ref', 'update_ref'):
            self.assertFalse(hasattr(CONTROLLER.Controller, name), name)
        self.assertFalse(hasattr(rig.table, 'delete_item'))
        # A later new-version run must recover, not reserve again.
        self.refused('reservation conflict: v0.43.0 is already reserved; recover its reservation', rig.qualify)
        # Absence after an observed reservation is lost durable state, and it never becomes a new one.
        recovered = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        del rig.w.refs['tags/reserve/v' + rig.w.v]
        rig.git.created = []
        self.refused('reservation conflict: the reservation of v0.43.0 was observed and is gone: durable state is lost',
                     CONTROLLER.create_reservation, rig.git, recovered, rig.holder(job_id=RESERVE_JOB_ID))
        self.assertEqual(rig.git.created, [])
        fresh = rig.controller()
        self.refused('eligibility incomplete: v0.43.0 has no reservation to recover', fresh.admit)
        self.assertEqual(rig.kms.calls, [])


class Attempt(Base):
    def test_rsig5_attempt_is_durable_before_sign_dispatch(self):
        rig = Rig()
        ctrl = rig.ready()
        self.assertEqual(rig.row()['phase'], 'RESERVED')
        order = []
        original = rig.table.transact_write_items

        def spy(items, token, timeout):
            order.append('attempt write')
            return original(items, token, timeout)
        rig.table.transact_write_items = spy
        original_get = rig.table.transact_get_items

        def get_spy(keys, timeout):
            order.append('snapshot')
            return original_get(keys, timeout)
        rig.table.transact_get_items = get_spy
        with ctrl.reserve(**rig.adapter_args()):
            ctrl.before_public_key()
            order.clear()
            permit = ctrl.before_sign(rig.payload_digest())
            self.assertEqual(order, ['snapshot', 'attempt write', 'snapshot'], 'the exact readback follows the one write')
            self.assertEqual(rig.kms.signs, 0)
            # The durable record has exactly the frozen fields, from authenticated state.
            attempt = json.loads(rig.table.items[(rig.tuple_pk(), 'ATTEMPT')])
            grant = json.loads(rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')])
            self.assertEqual(attempt, {
                'schema': 1, 'state': 'attempt_started', 'repository_id': str(REPO_ID), 'version': rig.w.v,
                'tuple_sha256': rig.tuple_sha(), 'reservation_object': rig.w.refs['tags/reserve/v' + rig.w.v][1],
                'commit': S, 'release_evidence_sha256': rig.w.evidence_sha, 'manifest_sha256': rig.w.manifest,
                'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT, 'policy_commit': P, 'signer_workflow_sha': H,
                'run_id': str(DISPATCH_RUN), 'run_attempt': '1', 'job_id': str(SIGN_JOB), 'grant_id': ctrl.grant_id,
                'fence': grant['fence']})
            self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
            # The permit is single-use, volatile and never serialized.
            for call in (pickle.dumps, copy.copy, copy.deepcopy):
                with self.assertRaises(Exception):
                    call(permit)
            permit.consume()
            self.refused('signing outcome unknown: the dispatch permit was already used', permit.consume)
            # One invocation gets one attempt.
            self.refused('signing outcome unknown: this invocation already used its one attempt', ctrl.before_sign,
                         rig.payload_digest())
        # The requested preimage must be the one computed from the qualified manifest.
        rig = Rig()
        ctrl = rig.ready()
        with ctrl.reserve(**rig.adapter_args()):
            before = dict(rig.table.items)
            self.refused('evidence mismatch: the signing preimage is not the one of the qualified manifest',
                         ctrl.before_sign, digest(3))
            self.assertEqual(rig.table.items, before)
        # An attempt request outside the context never reaches the journal.
        self.refused('authority unavailable: a KMS request needs the reservation context', ctrl.before_sign,
                     rig.payload_digest())
        self.assertEqual(rig.table.items, before)
        # A lost acknowledgement, an unavailable journal, a provider retry and a conflict give no permit.
        for mode, message in (('lost_ack', 'signing outcome unknown: the attempt write outcome is unknown'),
                              ('unsent', 'signing outcome unknown: the attempt write outcome is unknown'),
                              ('down', 'signing outcome unknown: the journal is unavailable'),
                              ('replay', 'signing outcome unknown: the attempt write was not one fresh request')):
            rig = Rig()
            ctrl = rig.ready()
            rig.table.fail_next('ATTEMPT', mode)
            with ctrl.reserve(**rig.adapter_args()):
                with self.subTest(mode=mode):
                    self.refused(message, ctrl.before_sign, rig.payload_digest())
            self.assertEqual(rig.kms.signs, 0)
        # Finding an identical existing record gives no permit either.
        rig = Rig()
        ctrl = rig.ready()
        rig.sign(ctrl)
        later = rig.session(rig.rerun())
        with later.reserve(**rig.adapter_args()):
            self.refused('signing outcome unknown: the reservation is already attempted', later.before_sign,
                         rig.payload_digest())
        self.assertEqual(rig.kms.signs, 1)
        # A failed readback gives no permit.
        rig = Rig()
        ctrl = rig.ready()
        original = rig.table.transact_write_items

        def corrupt(items, token, timeout):
            ack = original(items, token, timeout)
            rig.table.items[(rig.tuple_pk(), 'ATTEMPT')] = b'{}\n'
            return ack
        rig.table.transact_write_items = corrupt
        with ctrl.reserve(**rig.adapter_args()):
            self.refused('signing outcome unknown: the attempt readback is not the exact record', ctrl.before_sign,
                         rig.payload_digest())
        self.assertEqual(rig.kms.signs, 0)

    def test_rsig5_concurrent_identical_attempts_have_one_permit(self):
        rig = Rig()
        first = rig.ready()
        answers = []

        def rival():
            other = rig.session(rig.rerun())
            with other.reserve(**rig.adapter_args()):
                answers.append(other.before_sign(rig.payload_digest()))
        with first.reserve(**rig.adapter_args()):
            rig.table.hook = rival
            self.refused('signing outcome unknown: the attempt was not freshly created', first.before_sign,
                         rig.payload_digest())
        self.assertEqual(len(answers), 1)
        self.assertEqual(rig.row()['phase'], 'ATTEMPTED')

    def test_rsig5_process_death_at_each_sign_boundary(self):
        for point, signs in (('after attempt acknowledgement', 0), ('after dispatch', 1), ('after response', 1),
                             ('before signature persistence', 1)):
            rig = Rig()
            ctrl = rig.ready()
            with self.assertRaises(Death):
                rig.sign(ctrl, die_at=point)
            with self.subTest(point=point):
                self.assertEqual(rig.kms.signs, signs)
                self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
            # Restart: a fresh controller, the same remote journal, the same reservation.
            fresh = rig.session(rig.rerun())
            with self.subTest(point=point, step='restart'):
                self.refused('signing outcome unknown: the attempt has no retained signature', fresh.recover,
                             rig.tuple_bytes())
                with self.assertRaises(Refused) as caught:
                    rig.sign(fresh)
                self.assertEqual(str(caught.exception), 'signing outcome unknown: the reservation is already attempted')
                self.assertEqual(rig.kms.signs, signs, 'no restart adds a Sign call')
                self.assertIn('tags/reserve/v' + rig.w.v, rig.w.refs)
        # Death before the attempt write: the row stays RESERVED. With no proof of where the invocation
        # stopped, nothing more can happen. With that proof, the one attempt is still available.
        rig = Rig()
        ctrl = rig.ready()
        with self.assertRaises(Death):
            rig.sign(ctrl, die_at='before attempt')
        self.assertEqual(rig.row()['phase'], 'RESERVED')
        rig.history.answer = CONTROLLER.History(True, (CONTROLLER.Invocation(DISPATCH_RUN, 1, SIGN_JOB, 'unknown'),))
        fresh = rig.session(rig.rerun())
        with self.assertRaises(Refused) as caught:
            rig.sign(fresh)
        self.assertEqual(str(caught.exception),
                         'signing outcome unknown: a prior invocation is not proven to have stopped before its attempt')
        self.assertEqual(rig.kms.signs, 0)
        rig.history.answer = CONTROLLER.History(True, (CONTROLLER.Invocation(DISPATCH_RUN, 1, SIGN_JOB, 'before_attempt'),
                                                       CONTROLLER.Invocation(DISPATCH_RUN, 2, SIGN_JOB + 1, 'before_attempt')))
        clean = rig.session(rig.rerun(attempt=3))
        self.assertEqual(rig.sign(clean), SIGNATURE)
        self.assertEqual(rig.kms.signs, 1)
        # A lost attempt acknowledgement: the record may exist. Restart adds no Sign.
        rig = Rig()
        ctrl = rig.ready()
        rig.table.fail_next('ATTEMPT', 'lost_ack')
        with self.assertRaises(Refused):
            rig.sign(ctrl)
        fresh = rig.session(rig.rerun())
        with self.assertRaises(Refused):
            rig.sign(fresh)
        self.assertEqual(rig.kms.signs, 0)
        # Retention loss after the response: refused, recovered as unknown, no second Sign.
        rig = Rig()
        ctrl = rig.ready()
        rig.table.fail_next('SIGNATURE', 'unsent')
        self.refused('signing outcome unknown: the retention write outcome is unknown', rig.sign, ctrl)
        fresh = rig.session(rig.rerun())
        self.refused('signing outcome unknown: the attempt has no retained signature', fresh.recover, rig.tuple_bytes())
        self.assertEqual(rig.kms.signs, 1)
        # A lost retention acknowledgement with durable bytes: recovery finds them and verifies them.
        rig = Rig()
        ctrl = rig.ready()
        rig.table.fail_next('SIGNATURE', 'lost_ack')
        self.refused('signing outcome unknown: the retention write outcome is unknown', rig.sign, ctrl)
        fresh = rig.session(rig.rerun())
        self.assertEqual(fresh.recover(rig.tuple_bytes()).signature, SIGNATURE)
        self.assertEqual(rig.kms.signs, 1)

    def test_rsig5_missing_or_unavailable_attempt_history_refuses(self):
        def unavailable(rig):
            rig.table.unavailable = True

        def incomplete(rig):
            rig.history.answer = CONTROLLER.History(complete=False, invocations=())

        def history_down(rig):
            rig.history.error = ControllerError('the run history is unavailable')

        # Each case reaches the attempt step with a good session; only the history is uncertain.
        for change, message in ((incomplete, 'signing outcome unknown: the prior invocation history is incomplete'),
                                (history_down, 'signing outcome unknown: the prior invocation history is unavailable'),
                                (unavailable, 'signing outcome unknown: the journal is unavailable')):
            rig = Rig()
            ctrl = rig.ready()
            with ctrl.reserve(**rig.adapter_args()):
                change(rig)
                with self.subTest(message=message):
                    self.refused(message, ctrl.before_sign, rig.payload_digest())
            self.assertEqual(rig.kms.signs, 0)
        # An existing Git reservation with no journal row is uncertain history; nothing rebuilds a row.
        rig = Rig()
        rig.reserve_version()
        receipt = json.loads(rig.w.members[(RECEIPT_ART, CONTROLLER.RESERVE_MEMBER)])
        for label, artifact, change in (('no receipt', None, None),
                                        ('winner, not creator', RECEIPT_ART, dict(receipt, created=False))):
            if change:
                rig.upload_receipt(canon(change))
            with self.subTest(label):
                self.refused('signing outcome unknown: the reservation has no journal row and this holder did not '
                             'freshly create it', rig.session,
                             rig.controller(artifact=artifact, run_evidence=FakeEvidence()))
            self.assertNotIn(rig.version_key(), rig.table.items)
        # A partial or conflicting snapshot refuses: output with no attempt, a damaged digest.
        rig = Rig()
        ctrl = rig.ready()
        rig.table.items[(rig.tuple_pk(), 'SIGNATURE')] = SIGNATURE
        with ctrl.reserve(**rig.adapter_args()):
            self.refused('signing outcome unknown: the reservation has output and no attempt', ctrl.before_sign,
                         rig.payload_digest())
        self.refused('signing outcome unknown: the journal is partial', ctrl.recover, rig.tuple_bytes())
        rig = Rig()
        ctrl = rig.ready()
        original = rig.table._item

        def damaged(key, body):
            item = original(key, body)
            if key[1] == 'RESERVATION':
                item['body_sha256'] = {'S': digest(1)}
            return item
        rig.table._item = damaged
        self.refused('signing outcome unknown: the journal is unavailable', ctrl.recover, rig.tuple_bytes())
        # Another table, or a changed genesis, is not the journal that the attempts were recorded in.
        for label, change in (('table id', lambda t: t.described.update(TableId='11111111-89ab-4cde-8f01-23456789abcd')),
                              ('status', lambda t: t.described.update(TableStatus='UPDATING')),
                              ('arn', lambda t: t.described.update(TableArn=t.arn + 'x')),
                              ('key schema', lambda t: t.described.update(KeySchema=[])),
                              ('genesis', lambda t: t.items.update({('CONTROL#' + str(REPO_ID), 'GENESIS'): b'{}\n'})),
                              ('no genesis', lambda t: t.items.pop(('CONTROL#' + str(REPO_ID), 'GENESIS')))):
            rig = Rig()
            rig.reserve_version()
            change(rig.table)
            with self.subTest(label):
                self.refused_code('authority unavailable', rig.session)
            self.assertNotIn(rig.version_key(), rig.table.items)
        # An unavailable journal at recovery is unknown, not absent.
        rig = Rig()
        ctrl = rig.ready()
        rig.sign(ctrl)
        fresh = rig.controller(rig.holder(DISPATCH_RUN, 1, SIGN_JOB), artifact=None)
        fresh.attach_journal(rig.table)
        rig.table.unavailable = True
        self.refused('signing outcome unknown: the journal is unavailable', fresh.recover, rig.tuple_bytes())
        self.assertEqual(rig.kms.signs, 1)
        # A reservation with no attempt has nothing to recover; that is not proof of a clean history.
        rig = Rig()
        ctrl = rig.ready()
        self.refused('signing outcome unknown: the reservation has no attempt and no retained signature', ctrl.recover,
                     rig.tuple_bytes())
        self.assertEqual(ctrl.journal_phase(), 'RESERVED')

    def test_rsig5_recovery_requires_real_verified_signature_bytes(self):
        def retained():
            rig = Rig()
            ctrl = rig.ready()
            rig.sign(ctrl)
            return rig, rig.session(rig.rerun())
        rig, fresh = retained()
        answer = fresh.recover(rig.tuple_bytes())
        self.assertEqual(answer.signature, SIGNATURE)
        self.assertIsInstance(answer, CONTROLLER.RetainedSignature)
        self.assertEqual(rig.verifier.calls[-1][:2], (SIGNATURE, rig.manifest_bytes()))
        self.assertEqual(rig.verifier.calls[-1][2:], (rig.w.trust['signer_line'], 'rdy-release', 'rdy-release-v1'))
        writes = len(rig.table.writes)
        fresh.recover(rig.tuple_bytes())
        self.assertEqual(len(rig.table.writes), writes, 'recovery reads only')
        self.assertEqual(rig.kms.signs, 1)
        # The adapter then takes its already-verified path: reserve() still works and no attempt is written.
        with fresh.reserve(**rig.adapter_args()):
            fresh.before_public_key()
        # A retained signature that does not verify, or is not the receipt's bytes, stops.
        rig, fresh = retained()
        rig.verifier.good = set()
        self.refused('signing outcome unknown: the retained signature does not verify', fresh.recover, rig.tuple_bytes())
        for key, label, change in (
                ('SIGNATURE', 'the retained signature is not the completion receipt signature', lambda r: b'other bytes'),
                ('ATTEMPT', 'the journal attempt is not this tuple', lambda r: canon(dict(json.loads(
                    r.table.items[(r.tuple_pk(), 'ATTEMPT')]), manifest_sha256=digest(1)))),
                ('COMPLETION', 'the completion receipt is not this tuple', lambda r: canon(dict(json.loads(
                    r.table.items[(r.tuple_pk(), 'COMPLETION')]), manifest_sha256=digest(1)))),
                ('COMPLETION', 'the completion receipt does not bind its attempt', lambda r: canon(dict(json.loads(
                    r.table.items[(r.tuple_pk(), 'COMPLETION')]), attempt_sha256=digest(1))))):
            rig, fresh = retained()
            rig.table.items[(rig.tuple_pk(), key)] = change(rig)
            rig.verifier.good.add(b'other bytes')
            with self.subTest(label):
                self.refused('signing outcome unknown: ' + label, fresh.recover, rig.tuple_bytes())
        # Absent or partial output stops. The bytes must be the real signature; a hash is not one.
        for sk in ('SIGNATURE', 'COMPLETION', 'ATTEMPT'):
            rig, fresh = retained()
            del rig.table.items[(rig.tuple_pk(), sk)]
            with self.subTest(sk):
                self.refused('signing outcome unknown: the journal is partial', fresh.recover, rig.tuple_bytes())
        # A signature that does not verify is never retained.
        rig = Rig()
        ctrl = rig.ready()
        rig.verifier.good = set()
        self.refused('signing outcome unknown: the returned signature does not verify', rig.sign, ctrl)
        self.assertNotIn((rig.tuple_pk(), 'SIGNATURE'), rig.table.items)
        # A retained record never needs a Sign call. The tuple in the request must be the reserved one.
        rig, fresh = retained()
        other = rig.w.expected_tuple()
        other['signer']['transition_sha256'] = digest(1)
        self.refused('reservation conflict: the requested tuple is not the reserved tuple', fresh.recover, canon(other))
        # An abandoned tuple never resumes. The tombstone keeps every record.
        rig, fresh = retained()
        rig.table.items[('ABANDONMENT#' + str(REPO_ID) + '#' + rig.w.v + '#' + rig.tuple_sha(), 'RECORD')] = canon(
            {'schema': 1, 'reason': 'test'})
        kept = rig.table.items[(rig.tuple_pk(), 'SIGNATURE')]
        self.refused('reservation conflict: v0.43.0 is abandoned', fresh.recover, rig.tuple_bytes())
        self.assertEqual(rig.table.items[(rig.tuple_pk(), 'SIGNATURE')], kept)
        # The size bound holds before any write.
        rig = Rig()
        ctrl = rig.ready()
        with ctrl.reserve(**rig.adapter_args()):
            ctrl.before_sign(rig.payload_digest()).consume()
        writes = len(rig.table.writes)
        rig.verifier.good.add(b'x' * 4097)
        self.refused('input limit exceeded: the signature is larger than 4096 bytes', ctrl.retain_signature, b'x' * 4097)
        self.assertEqual(len(rig.table.writes), writes)

    def test_rsig5_retry_keeps_evidence_and_authority(self):
        rig = Rig()
        ctrl = rig.ready()
        e_bytes = rig.w.members[(base.EVID_ART, 'release-record.json')]
        reserved = rig.w.tags[rig.w.refs['tags/reserve/v' + rig.w.v][1]]['message_bytes']
        rig.sign(ctrl)
        attempt = rig.table.items[(rig.tuple_pk(), 'ATTEMPT')]
        # A rerun attempt authenticates its own actors; the run's first actor grants nothing.
        rig.add_job(DISPATCH_RUN, 2, SIGN_JOB + 1)
        rig.w.run(DISPATCH_RUN, 2, '.github/workflows/release.yml', 'workflow_dispatch', 101, actor=base.APP,
                  trigger=base.ATTACKER, status='in_progress', conclusion=None)
        rerun = rig.controller(rig.holder(DISPATCH_RUN, 2, SIGN_JOB + 1), artifact=None,
                               request=rig.w.request(dispatch_run_attempt=2))
        provider = rig.provider()
        self.refused('not authorized: the release run was triggered by an actor who is not a coordinator', rerun.admit)
        self.refused('authority unavailable: the attempt is not admitted', rerun.issue_credentials, provider)
        self.assertEqual(provider.oidc, [])
        # An authorized rerun keeps the original E and T, and the reservation.
        rig.add_job(DISPATCH_RUN, 3, SIGN_JOB + 2)
        rig.w.run(DISPATCH_RUN, 3, '.github/workflows/release.yml', 'workflow_dispatch', 101, actor=base.APP,
                  trigger=base.PERSON, status='in_progress', conclusion=None)
        again = rig.session(rig.controller(rig.holder(DISPATCH_RUN, 3, SIGN_JOB + 2), artifact=None,
                                           request=rig.w.request(dispatch_run_attempt=3)))
        self.assertEqual(again.qualified.evidence_bytes, e_bytes)
        self.assertEqual(again.qualified.tuple_bytes, reserved)
        self.assertEqual(again.recover(rig.tuple_bytes()).signature, SIGNATURE)
        self.assertEqual(rig.table.items[(rig.tuple_pk(), 'ATTEMPT')], attempt)
        # A new E never replaces the old one under the same reservation.
        rig.w.e['audit']['sha256'] = digest(66)
        rig.w.seal()
        replaced = rig.controller(rig.holder(DISPATCH_RUN, 3, SIGN_JOB + 2), artifact=None,
                                  request=rig.w.request(dispatch_run_attempt=3), run_evidence=FakeEvidence())
        self.refused('evidence mismatch: the requested release record is not the reserved one', replaced.admit)
        self.assertEqual(rig.kms.signs, 1)


class Authority(Base):
    def stages(self, rig):
        """Every stage of an unfinished signing, each with the change `apply` made just before it."""
        def admit(apply):
            ctrl = rig.controller()
            apply()
            ctrl.admit()

        def issue(apply):
            ctrl = rig.controller(run_evidence=FakeEvidence())
            ctrl.admit()
            apply()
            ctrl.issue_credentials(rig.provider())

        def reserve(apply):
            ctrl = rig.session(rig.controller(run_evidence=FakeEvidence()))
            apply()
            with ctrl.reserve(**rig.adapter_args()):
                raise AssertionError('reserve yielded')

        def before_sign(apply):
            ctrl = rig.session(rig.controller(run_evidence=FakeEvidence()))
            with ctrl.reserve(**rig.adapter_args()):
                apply()
                ctrl.before_sign(rig.payload_digest())
        return (('admit', admit), ('issue', issue), ('reserve', reserve), ('before_sign', before_sign))

    def refuse_everywhere(self, change, message):
        for stage in ('admit', 'issue', 'reserve', 'before_sign'):
            rig = Rig()
            rig.reserve_version()
            with self.subTest(stage=stage, message=message):
                self.refused(message, dict(self.stages(rig))[stage], lambda: change(rig))
                self.assertEqual(rig.kms.calls, [])
                self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)

    def test_rsig5_revocation_and_policy_rollback_refuse(self):
        revoked = ControllerError('release trust: the key for 0.43.0 is revoked for new signing')

        def revoke(rig):
            original = rig.git.release_trust

            def trust(version, commit, signing):
                if signing:
                    raise revoked
                return original(version, commit, signing)
            rig.git.release_trust = trust
        self.refuse_everywhere(revoke, 'signer revoked: the key for v0.43.0 is revoked for new signing')

        def rollback(rig):
            older = 'a' * 40
            rig.w.put(older, 'release-keys/policy.json', rig.w.files[('release-keys/policy.json', P)])
            rig.w.compares[(P, older)] = 'behind'
            rig.w.compares[(older, HEAD)] = 'ahead'
            rig.install(mirror=False, policy_commit=older)
        self.refuse_everywhere(rollback, 'policy rollback: the current policy commit does not descend from the '
                                         'reserved policy commit')

        def not_on_main(rig):
            sibling = 'a' * 40
            rig.w.put(sibling, 'release-keys/policy.json', rig.w.files[('release-keys/policy.json', P)])
            rig.w.compares[(P, sibling)] = 'ahead'
            rig.w.compares[(sibling, HEAD)] = 'diverged'
            rig.install(mirror=False, policy_commit=sibling)
        self.refuse_everywhere(not_on_main, "policy rollback: the current policy commit is not on main")
        # A changed policy, trust or key under the same authority refuses too.
        for field, value, message in (
                ('policy_sha256', digest(1), 'authority unavailable: the admission policy digest is not the protected policy'),
                ('trust_sha256', digest(1), 'authority unavailable: the admission trust digest is not the protected trust'),
                ('key_arn', base.NEW_KEY_ARN, 'authority unavailable: the admission record names another key'),
                ('fingerprint', 'SHA256:' + 'B' * 43, 'authority unavailable: the admission record names another key'),
                ('signer_workflow_sha', base.OTHER, 'authority unavailable: the admission record names another signer'),
                ('table_id', '11111111-89ab-4cde-8f01-23456789abcd',
                 'authority unavailable: the admission record names another journal'),
                ('genesis_sha256', digest(2), 'authority unavailable: the admission record names another journal')):
            self.refuse_everywhere(lambda r, field=field, value=value: r.install(mirror=False, **{field: value}), message)
        # Main's policy moved without maintenance: the channel may, anything else needs a new epoch.
        def moved(rig):
            policy = json.loads(rig.w.files[('release-keys/policy.json', HEAD)])
            policy['role_arn'] = base.ROLE_ARN + '-other'
            rig.w.put(HEAD, 'release-keys/policy.json', canon(policy))
        self.refuse_everywhere(moved, 'authority unavailable: the admission policy digest is not the protected policy')
        # Every unfinished recovery path checks current authority; each phase refuses.
        for phase in ('RESERVED', 'ATTEMPTED', 'COMPLETE'):
            for label, change, message in (
                    ('revoked', revoke, 'signer revoked: the key for v0.43.0 is revoked for new signing'),
                    ('closed', lambda r: r.install(state='CLOSED', mirror=False),
                     'authority unavailable: admission is CLOSED'),
                    ('rollback', rollback, 'policy rollback: the current policy commit does not descend from the '
                                           'reserved policy commit'),
                    ('missing', lambda r: r.w.files.pop((ADMISSION, HEAD)),
                     'authority unavailable: the admission record is unavailable')):
                rig = Rig()
                ctrl = rig.ready()
                if phase != 'RESERVED':
                    with ctrl.reserve(**rig.adapter_args()):
                        ctrl.before_sign(rig.payload_digest()).consume()
                if phase == 'COMPLETE':
                    ctrl.retain_signature(SIGNATURE)
                fresh = rig.controller(artifact=None, run_evidence=FakeEvidence())
                fresh.attach_journal(rig.table)
                # Recovery reads eligibility without the signing rule; authority then refuses by itself.
                change(rig)
                with self.subTest(phase=phase, change=label):
                    self.refused(message, fresh.recover, rig.tuple_bytes())

    def test_rsig5_expired_or_stale_fence_cannot_dispatch(self):
        # A grant deadline never exceeds the server's expiry, and needs the bounded budget left.
        rig = Rig()
        ctrl = rig.ready()
        grant = json.loads(rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')])
        self.assertEqual(int(grant['sts_expiration']), T0 + 900)
        self.assertEqual(int(grant['deadline']), T0 + 900 - CONTROLLER.CLOCK_SKEW)
        rig.clock.value = int(grant['deadline']) - CONTROLLER.SIGN_BUDGET + 1
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the grant does not leave the signing budget')
        self.assertEqual(rig.kms.calls, [])
        # Enough time at the attempt, not at dispatch: the right is consumed and nothing is sent.
        rig = Rig()
        ctrl = rig.ready()
        grant = json.loads(rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')])
        with ctrl.reserve(**rig.adapter_args()):
            permit = ctrl.before_sign(rig.payload_digest())
            rig.clock.value = int(grant['deadline'])
            self.refused('authority unavailable: the grant does not leave the signing budget', permit.consume)
        self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
        self.refused('signing outcome unknown: the dispatch permit was already used', permit.consume)
        # The dispatch returns the bounded KMS time; retention shares what is left.
        rig = Rig()
        ctrl = rig.ready()
        with ctrl.reserve(**rig.adapter_args()):
            permit = ctrl.before_sign(rig.payload_digest())
            self.assertEqual(permit.consume(), CONTROLLER.KMS_MAX)
        rig.clock.value += CONTROLLER.SIGN_BUDGET
        self.refused('signing outcome unknown: the signing budget has passed', ctrl.retain_signature, SIGNATURE)
        # A later grant advanced the fence: the older grant is stale.
        rig = Rig()
        ctrl = rig.ready()
        rig.session(rig.rerun())
        with ctrl.reserve(**rig.adapter_args()):
            self.refused('authority unavailable: the grant fence is stale', ctrl.before_sign, rig.payload_digest())
        self.assertEqual(rig.row()['phase'], 'RESERVED')
        # A short server lifetime refuses before any grant.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        self.refused('authority unavailable: the credentials do not leave the signing budget', ctrl.issue_credentials,
                     rig.provider(lifetime=CONTROLLER.SIGN_BUDGET + CONTROLLER.CLOCK_SKEW - 1))
        self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        # The grant cannot be changed underneath the holder.
        rig = Rig()
        ctrl = rig.ready()
        rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')] = canon(dict(json.loads(
            rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')]), deadline=str(T0 + 99999)))
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the stored grant is not this grant')
        self.assertEqual(rig.kms.calls, [])
        # The fence wire type is exact.
        rig = Rig()
        rig.reserve_version()
        rig.table.items[('FENCE#' + str(REPO_ID) + '#' + EPOCH, 'COUNTER')] = canon(
            {'schema': 1, 'repository_id': str(REPO_ID), 'epoch': EPOCH, 'fence': '01'})
        self.refused_code('authority unavailable', rig.session)
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_admission_reads_and_single_credential_issue(self):
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()

        def reads():
            return sum(1 for c in rig.w.calls if c[0] == 'contents' and c[1] == ADMISSION)
        rig.w.calls = []
        ctrl.admit()
        self.assertEqual(reads(), 1)
        self.assertEqual([c[1] for c in rig.w.calls if c[0] == 'ref' and c[1] == 'heads/main'][:2],
                         ['heads/main', 'heads/main'], 'the exact ref is read before and after the file')
        provider = rig.provider()
        self.assertEqual(provider.oidc, [])
        ctrl.issue_credentials(provider)
        self.assertEqual(reads(), 2, 'read again before credential issuance')
        self.assertEqual(provider.oidc, ['sts.amazonaws.com'])
        self.assertEqual(provider.sts, [(provider.token, 900)])
        # One per attempt, in this process or a restarted one.
        self.refused('authority unavailable: an attempt has one OIDC request and one STS assumption',
                     ctrl.issue_credentials, provider)
        restarted = rig.controller()
        restarted.admit()
        self.refused('authority unavailable: an attempt has one OIDC request and one STS assumption',
                     restarted.issue_credentials, provider)
        self.assertEqual((len(provider.oidc), len(provider.sts)), (1, 1))
        # Only nonsecret receipts remain; the token is nowhere in the evidence or the journal.
        self.assertNotIn(provider.token, repr(rig.evidence.rows))
        self.assertEqual([r[1] for r in rig.evidence.rows],
                         ['pending', 'oidc_requested', 'oidc_issued', 'sts_requested', 'sts_issued', 'pending'])
        ctrl.open_session(rig.table)
        for key, body in rig.table.items.items():
            self.assertNotIn(provider.token.encode(), body, key)
        self.assertNotIn(provider.token, repr(vars(ctrl)))
        self.assertEqual(json.loads(rig.table.items[('ISSUER#' + str(DISPATCH_RUN) + '#1#' + str(SIGN_JOB), 'RECORD')]),
                         {'schema': 1, 'repository_id': str(REPO_ID), 'run_id': str(DISPATCH_RUN), 'run_attempt': '1',
                          'job_id': str(SIGN_JOB), 'grant_id': ctrl.grant_id, 'jwt_iat': str(T0),
                          'jwt_exp': str(T0 + 300), 'requested_duration': '900', 'sts_expiration': str(T0 + 900),
                          'role_max_session_duration': '3600'})
        rig.w.calls = []
        with ctrl.reserve(**rig.adapter_args()):
            self.assertEqual(reads(), 1, 'read again before reserve')
        # A failed request is still the attempt's one request.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        provider = rig.provider()
        provider.request_oidc = lambda audience: (_ for _ in ()).throw(ControllerError('the issuer failed'))
        self.refused('authority unavailable: the credential request failed', ctrl.issue_credentials, provider)
        self.refused('authority unavailable: an attempt has one OIDC request and one STS assumption',
                     ctrl.issue_credentials, rig.provider())
        # The assumed role and the requested duration are exact.
        for change, message in ((dict(role_arn=base.ROLE_ARN + 'x'), 'the assumed role is not the policy role'),):
            rig = Rig()
            rig.reserve_version()
            ctrl = rig.controller()
            ctrl.admit()
            self.refused('authority unavailable: ' + message, ctrl.issue_credentials, rig.provider(**change))
        # Sessions and grants need the single STS exchange first.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        self.refused('authority unavailable: the session needs the sole STS exchange first', ctrl.open_session, rig.table)
        self.assertEqual(rig.table.writes, [])
        # Unavailable run evidence stops before any credential.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        rig.evidence.broken = True
        provider = rig.provider()
        self.refused('authority unavailable: the run evidence is unavailable', ctrl.issue_credentials, provider)
        self.assertEqual(provider.oidc, [])
        # Ref movement, a missing file, caller JSON or a candidate file never grant authority.
        rig = Rig()
        rig.reserve_version()
        rig.git.after_admission_read = lambda: rig.w.refs.update({'heads/main': ('commit', P)})
        self.refused('authority unavailable: main moved during the admission read', rig.controller().admit)
        rig = Rig()
        rig.reserve_version()
        rig.w.put(S, ADMISSION, canon(rig.record('ACTIVE')))
        rig.install(state='CLOSED')
        self.refused('authority unavailable: admission is CLOSED', rig.controller().admit)
        with self.assertRaises(TypeError):
            rig.controller(admission=canon(rig.record('ACTIVE')))
        for label, data, message in (
                ('float', canon(rig.record()).replace(b'"schema":1', b'"schema":1.0'), 'the admission record is malformed'),
                ('extra', canon(dict(rig.record(), approved=True)), "unknown field 'approved'"),
                ('spaces', b' ' + canon(rig.record()), 'the admission record is malformed'),
                ('epoch', canon(rig.record(epoch='007')), 'the admission record is malformed'),
                ('integer epoch', canon(rig.record(epoch=7)), 'the admission record is malformed'),
                ('schema', canon(dict(rig.record(), schema=2)), 'the admission record is malformed'),
                ('repository', canon(dict(rig.record(), repository_id='1')), 'the admission record is malformed'),
                ('state', canon(rig.record(state='OPEN')), 'the admission record is malformed'),
                ('active holder', canon(rig.record(holder={'run_id': '1', 'run_attempt': '1', 'job_id': '1',
                                                           'grant_id': '00000000-0000-4000-8000-000000000001'})),
                 'the admission record is malformed'),
                ('draining no holder', canon(rig.record(state='DRAINING')), 'the admission record is malformed')):
            rig.w.put(HEAD, ADMISSION, data)
            with self.subTest(label):
                self.refused('authority unavailable: ' + message, rig.controller().admit)
        rig = Rig()
        rig.reserve_version()
        rig.w.files.pop((ADMISSION, HEAD))
        self.refused('authority unavailable: the admission record is unavailable', rig.controller().admit)
        rig = Rig()
        rig.reserve_version()

        def admission_timeout():
            raise GITHUB.Uncertain('contents', 'timeout')
        rig.git.after_admission_read = admission_timeout
        self.refused('authority unavailable: the admission record is unavailable', rig.controller().admit)
        # The OIDC token never reaches a representation of the attempt.
        self.assertNotIn('secret-value', repr(CONTROLLER.OidcToken(token='secret-value', iat=1, exp=2)))
        # A job that is not the running holder stops.
        rig = Rig()
        rig.reserve_version()
        rig.w.find_job(SIGN_JOB).update(status='completed', conclusion='success')
        self.refused('authority unavailable: the job is not this run attempt\'s running job', rig.controller().admit)
        rig = Rig()
        rig.reserve_version()
        rig.w.runs[(DISPATCH_RUN, 1)]['path'] = '.github/workflows/other.yml'
        self.refused('authority unavailable: the run is not the release workflow', rig.controller().admit)

    def test_rsig5_stale_or_rerun_epoch_refuses(self):
        # A grant of the old epoch is refused after the epoch moved; same for the mirror.
        rig = Rig()
        ctrl = rig.ready()
        rig.install(epoch='8', revision='2')
        rig.table.seed('FENCE#' + str(REPO_ID) + '#8', 'COUNTER', rig.fence_body('8', '1'))
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the grant is from another epoch')
        self.assertEqual(rig.kms.calls, [])
        # The mirror and GitHub must agree.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        rig.install(epoch='8', github=False)
        self.refused('authority unavailable: the journal mirror disagrees with the admission record', ctrl.open_session,
                     rig.table)
        self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        # A rerun does not inherit the first attempt's grant, DRAINING name or reserve receipt.
        rig = Rig()
        ctrl = rig.ready()
        rig.install(state='DRAINING', holder=rig.named(ctrl))
        rerun = rig.rerun()
        self.refused('authority unavailable: admission is DRAINING for another holder', rerun.admit)
        rig = Rig()
        rig.reserve_version()
        rig.add_job(DISPATCH_RUN, 2, SIGN_JOB + 1)
        rerun = rig.controller(rig.holder(DISPATCH_RUN, 2, SIGN_JOB + 1), request=rig.w.request(dispatch_run_attempt=2))
        rerun.admit()
        rerun.issue_credentials(rig.provider())
        self.refused('signing outcome unknown: the reserve receipt is unavailable', rerun.open_session, rig.table)
        # Grant ids are lowercase UUIDv4 strings from trusted code.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller(new_id=lambda: 'NOT-A-UUID')
        self.refused('authority unavailable: the grant id is not a lowercase UUIDv4', ctrl.admit)

    def test_rsig5_new_epoch_invalidates_old_grants(self):
        rig = Rig()
        ctrl = rig.ready()
        grant = rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')]
        run = maintenance_to_ready(self, rig)
        rig.clock.value = run.not_before + CONTROLLER.CLOCK_SKEW + 1
        run.apply_update(approved(rig))
        run.verify_readback(approved(rig))
        run.reopen()
        admission = rig.admission()
        current = rig.current()
        self.assertEqual((admission['state'], admission['epoch'], current['admission']['state'], current['admission']['epoch']),
                         ('ACTIVE', '8', 'ACTIVE', '8'))
        self.assertIn(('CONTROL#' + str(REPO_ID), 'EVENT#8#' + current['revision']), rig.table.items)
        self.assertEqual(json.loads(rig.table.items[('FENCE#' + str(REPO_ID) + '#8', 'COUNTER')])['fence'], '1')
        # The old grant never becomes valid. The row and the old grant stay.
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the grant is from another epoch')
        self.assertEqual(rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')], grant)
        self.assertEqual(rig.kms.calls, [])
        # A new holder is admitted at the new epoch and gets the higher epoch's fence.
        fresh = rig.session(rig.rerun())
        self.assertEqual(json.loads(rig.table.items[('GRANT#' + fresh.grant_id, 'RECORD')])['epoch'], '8')


def approved(rig):
    policy = rig.w.files[('release-keys/policy.json', P)]
    return CONTROLLER.ApprovedUpdate(signer_workflow_sha=H, key_arn=rig.w.policy['key_arn'], fingerprint=FINGERPRINT,
                                     policy_commit=P, policy_sha256=sha(policy),
                                     trust_sha256=rig.w.policy['records_sha256'])


def census(rig, jobs=None, issuers=None, complete=True, role_max=3600):
    return CONTROLLER.Census(
        complete=complete, role_max_session_duration=role_max,
        jobs=jobs if jobs is not None else (
            CONTROLLER.CensusJob(DISPATCH_RUN, 1, SIGN_JOB, 'completed', 'issued'),),
        issuers=issuers if issuers is not None else (
            CONTROLLER.IssuerReceipt(DISPATCH_RUN, 1, SIGN_JOB, T0, T0 + 300, T0 + 900),))


def maintenance_run(rig, writer=None):
    return CONTROLLER.Maintenance(rig.git, writer or FakeWriter(rig.w),
                                  CONTROLLER.Journal(rig.table, rig.enrollment), rig.enrollment, rig.clock)


def maintenance_to_ready(test, rig):
    run = maintenance_run(rig)
    run.close()
    run.acquire_lane()
    run.prove_drained(census(rig))
    return run


class Maintenance(Base):
    def test_rsig5_signing_holder_drains_before_queued_maintenance(self):
        rig = Rig()
        holder = rig.ready()
        run = maintenance_run(rig)
        self.refused('authority unavailable: close admission before waiting for the lane', run.acquire_lane)
        run.close(holder=rig.named(holder))
        admission = rig.admission()
        current = rig.current()
        self.assertEqual((admission['state'], current['admission']['state']), ('DRAINING', 'DRAINING'))
        self.assertEqual((admission['epoch'], current['admission']['epoch']), (EPOCH, EPOCH))
        self.assertEqual(current['admission_sha256'], sha(rig.w.files[(ADMISSION, rig.head())]))
        run.acquire_lane()
        # New, pending and rerun attempts refuse before asking for credentials.
        queued = rig.rerun()
        provider = rig.provider()
        self.refused('authority unavailable: admission is DRAINING for another holder', queued.admit)
        self.refused('authority unavailable: the attempt is not admitted', queued.issue_credentials, provider)
        self.assertEqual(provider.oidc, [])
        # Drain waits for the running holder.
        running = census(rig, jobs=(CONTROLLER.CensusJob(DISPATCH_RUN, 1, SIGN_JOB, 'in_progress', 'issued'),))
        self.refused('authority unavailable: a credential-issuing job is not terminal; admission stays closed',
                     run.prove_drained, running)
        # Closing admission to drain does not revoke the valid holder's grant: it finishes.
        self.assertEqual(rig.sign(holder), SIGNATURE)
        self.assertEqual(rig.row()['phase'], 'COMPLETE')
        authorization = holder.authorization()
        holder_promotion = CONTROLLER.check_promotion(rig.git, rig.enrollment, rig.request(), DISPATCH_RUN, 1, authorization)
        self.assertEqual(holder_promotion.state, 'DRAINING')
        rig.add_job(DISPATCH_RUN, 2, SIGN_JOB + 9)
        self.refused('authority unavailable: admission is DRAINING for another holder', CONTROLLER.check_promotion,
                     rig.git, rig.enrollment, rig.w.request(dispatch_run_attempt=2), DISPATCH_RUN, 2, authorization)
        # The authorization binds this tuple and these exact cards.
        for label, data, message in (
                ('malformed', b'{}\n', 'evidence mismatch: the signer authorization is malformed'),
                ('unknown', canon(dict(json.loads(authorization), extra=1)), "evidence mismatch: unknown field 'extra'"),
                ('tuple', canon(dict(json.loads(authorization), tuple_sha256=digest(1))),
                 'evidence mismatch: the signer authorization is not this tuple'),
                ('cards', canon(dict(json.loads(authorization), cards=[{'kind': 'release_1', 'receipt_sha256': digest(1),
                                                                        'envelope_sha256': digest(2)}])),
                 'evidence mismatch: the signer authorization does not cover these cards')):
            with self.subTest(label):
                self.refused(message, CONTROLLER.check_promotion, rig.git, rig.enrollment, rig.request(), DISPATCH_RUN, 1, data)
        # Then the census passes.
        run.prove_drained(census(rig))
        self.assertEqual(run.not_before, T0 + 300 + 3600)
        # A named holder works under a partial close, but no new grant finalizes during one.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        rig.install(state='DRAINING', holder=rig.named(ctrl), mirror=False)
        self.refused('authority unavailable: the close is incomplete; no new grant finalizes', ctrl.open_session, rig.table)
        self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        rig = Rig()
        ctrl = rig.ready()
        rig.install(state='DRAINING', holder=rig.named(ctrl), mirror=False)
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        rig = Rig()
        ctrl = rig.ready()
        rig.install(state='DRAINING', holder=rig.named(ctrl), github=False)
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        # A holder that only the mirror's other name covers stops.
        rig = Rig()
        ctrl = rig.ready()
        rig.install(state='DRAINING', holder=rig.named(ctrl, run_id='1'), github=False)
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the journal mirror is DRAINING for another holder')
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_canceled_maintenance_keeps_admission_closed(self):
        rig = Rig()
        rig.reserve_version()
        run = maintenance_run(rig)
        run.close()
        run.acquire_lane()
        run.cancel()
        self.assertEqual(rig.admission()['state'], 'CLOSED')
        self.refused('authority unavailable: a canceled maintenance run never reopens admission', run.reopen)
        ctrl = rig.controller()
        self.refused('authority unavailable: admission is CLOSED', ctrl.admit)
        # A replacement cannot reopen by itself: it runs the whole protocol again.
        replacement = maintenance_run(rig)
        self.refused('authority unavailable: reopening needs the lane, the drain proof and the verified update',
                     replacement.reopen)
        replacement.close()
        replacement.acquire_lane()
        replacement.prove_drained(census(rig))
        self.refused('authority unavailable: reopening needs the lane, the drain proof and the verified update',
                     replacement.reopen)
        self.refused('authority unavailable: the drain boundary has not passed', replacement.apply_update, approved(rig))
        rig.clock.value = replacement.not_before + CONTROLLER.CLOCK_SKEW
        self.refused('authority unavailable: the drain boundary has not passed', replacement.apply_update, approved(rig))
        rig.clock.value += 1
        replacement.apply_update(approved(rig))
        other = approved(rig)
        self.refused('authority unavailable: the update readback is not the approved update',
                     replacement.verify_readback,
                     CONTROLLER.ApprovedUpdate(**dict(vars(other), trust_sha256=digest(1))))
        self.refused('authority unavailable: reopening needs the lane, the drain proof and the verified update',
                     replacement.reopen)
        replacement.verify_readback(approved(rig))
        replacement.reopen()
        self.assertEqual(rig.admission()['state'], 'ACTIVE')
        self.assertEqual(rig.admission()['epoch'], '8')
        # A failed close is partial: new work stays out, and the close is not complete.
        rig = Rig()
        rig.reserve_version()
        writer = FakeWriter(rig.w)
        run = maintenance_run(rig, writer)
        rig.table.fail_next('CURRENT', 'down')
        self.refused('authority unavailable: the close is incomplete', run.close)
        self.assertEqual(rig.admission()['state'], 'CLOSED')
        self.assertEqual(rig.current()['admission']['state'], 'ACTIVE')
        self.refused('authority unavailable: admission is CLOSED', rig.controller().admit)
        self.refused('authority unavailable: close admission before waiting for the lane', run.acquire_lane)
        # A failed reopen stays closed: the mirror's new epoch alone admits nobody.
        rig = Rig()
        rig.reserve_version()
        writer = FakeWriter(rig.w)
        run = maintenance_run(rig, writer)
        run.close()
        run.acquire_lane()
        run.prove_drained(census(rig))
        rig.clock.value = run.not_before + CONTROLLER.CLOCK_SKEW + 1
        run.apply_update(approved(rig))
        run.verify_readback(approved(rig))
        writer.fail = True
        self.refused('authority unavailable: the reopen is incomplete; admission stays closed', run.reopen)
        self.assertEqual(rig.admission()['state'], 'CLOSED')
        self.refused('authority unavailable: admission is CLOSED', rig.controller().admit)

    def test_rsig5_incomplete_census_or_expired_grant_refuses(self):
        rig = Rig()
        job = CONTROLLER.CensusJob
        receipt = CONTROLLER.IssuerReceipt
        good_job = job(DISPATCH_RUN, 1, SIGN_JOB, 'completed', 'issued')
        good = receipt(DISPATCH_RUN, 1, SIGN_JOB, T0, T0 + 300, T0 + 900)
        for label, kwargs, message in (
                ('incomplete pages', dict(complete=False), 'the census is incomplete'),
                ('queued job', dict(jobs=(job(1, 1, 2, 'queued', 'none'), good_job)), 'a credential-issuing job is not terminal'),
                ('environment waiting', dict(jobs=(job(1, 1, 2, 'waiting', 'none'), good_job)),
                 'a credential-issuing job is not terminal'),
                ('cancel requested', dict(jobs=(job(1, 1, 2, 'cancelling', 'issued'), good_job)),
                 'a credential-issuing job is not terminal'),
                ('in-flight issuance', dict(jobs=(job(1, 1, 2, 'completed', 'unresolved'), good_job)),
                 'an issuance request is unresolved'),
                ('issued with no receipt', dict(jobs=(job(1, 1, 2, 'completed', 'issued'), good_job)),
                 'an issued credential has no receipt'),
                ('receipt with no job', dict(jobs=()), 'an issuance receipt has no job in the census'),
                ('no receipt fields', dict(issuers=(receipt(DISPATCH_RUN, 1, SIGN_JOB, T0, T0 + 300, None),)),
                 'an issuance receipt has no server expiry'),
                ('no OIDC expiry', dict(issuers=(receipt(DISPATCH_RUN, 1, SIGN_JOB, T0, None, T0 + 900),)),
                 'an issuance receipt has no server expiry'),
                ('no role maximum', dict(role_max=None), 'the role maximum is not verified'),
                ('zero role maximum', dict(role_max=0), 'the role maximum is not verified')):
            with self.subTest(label):
                self.refused('authority unavailable: ' + message + '; admission stays closed',
                             CONTROLLER.drain_boundary, census(rig, **kwargs))
        # A job that proved it requested nothing needs no receipt. Cancellation is not termination.
        none = job(1, 1, 2, 'completed', 'none')
        self.assertEqual(CONTROLLER.drain_boundary(census(rig, jobs=(none, good_job))), T0 + 300 + 3600)
        self.assertEqual(CONTROLLER.drain_boundary(census(rig, jobs=(none,), issuers=())), None)
        # The maintenance state machine stays closed on any of these.
        rig = Rig()
        rig.reserve_version()
        run = maintenance_run(rig)
        self.refused('authority unavailable: close admission before waiting for the lane', run.prove_drained, census(rig))
        run.close()
        self.refused('authority unavailable: the drain proof needs the lane', run.prove_drained, census(rig))
        run.acquire_lane()
        self.refused('authority unavailable: the census is incomplete; admission stays closed', run.prove_drained,
                     census(rig, complete=False))
        self.assertEqual(rig.admission()['state'], 'CLOSED')
        # An expired grant cannot dispatch, and a grant cannot outlive the server's expiry.
        rig = Rig()
        ctrl = rig.ready()
        rig.clock.value = T0 + 900
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the grant does not leave the signing budget')
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_drain_uses_server_expiry_and_role_maximum(self):
        rig = Rig()
        job = CONTROLLER.CensusJob
        receipt = CONTROLLER.IssuerReceipt
        jobs = (job(1, 1, 1, 'completed', 'issued'), job(2, 1, 2, 'completed', 'issued'))

        def boundary(role_max, *issued):
            return CONTROLLER.drain_boundary(census(rig, jobs=jobs, issuers=issued, role_max=role_max))
        # The latest OIDC exp plus the role maximum wins over a requested 900 seconds.
        self.assertEqual(boundary(3600, receipt(1, 1, 1, T0, T0 + 300, T0 + 900), receipt(2, 1, 2, T0 + 50, T0 + 350, T0 + 950)),
                         T0 + 350 + 3600)
        # An observed STS expiry beyond that wins when it is later.
        self.assertEqual(boundary(900, receipt(1, 1, 1, T0, T0 + 300, T0 + 5000), receipt(2, 1, 2, T0, T0 + 300, T0 + 900)),
                         T0 + 5000)
        # A lowered role maximum does not shorten issued credentials: the observed maximum still counts.
        self.assertEqual(boundary(900, receipt(1, 1, 1, T0, T0 + 300, T0 + 900, role_max_session_duration=7200),
                                  receipt(2, 1, 2, T0, T0 + 300, T0 + 900)), T0 + 300 + 7200)
        # The wait is past the boundary plus the documented skew, by injected time only.
        self.assertEqual(CONTROLLER.CLOCK_SKEW, 60)
        rig = Rig()
        rig.reserve_version()
        run = maintenance_run(rig)
        run.close()
        run.acquire_lane()
        run.prove_drained(census(rig))
        self.assertEqual(run.not_before, T0 + 300 + 3600)
        rig.clock.value = T0 + 300 + 3600 + 60
        self.refused('authority unavailable: the drain boundary has not passed', run.apply_update, approved(rig))
        rig.clock.value += 1
        run.apply_update(approved(rig))
        self.assertEqual(CONTROLLER.STS_DURATION, 900)
        self.assertEqual(CONTROLLER.LANE, 'rdy-core-release-authority')

    def test_rsig5_emergency_containment_bypasses_queue(self):
        rig = Rig()
        holder = rig.ready()
        journal = CONTROLLER.Journal(rig.table, rig.enrollment)
        writer = FakeWriter(rig.w)
        with holder.reserve(**rig.adapter_args()):
            permit = holder.before_sign(rig.payload_digest())
            # No lane, no drain and no census: admission closes at once, whoever holds the lane.
            receipt = CONTROLLER.contain(rig.git, writer, journal, rig.enrollment, ((rig.w.v, rig.tuple_sha()),))
            self.assertEqual((receipt.github_closed, receipt.mirror_closed), (True, True))
            self.assertEqual(rig.admission()['state'], 'CLOSED')
            self.assertEqual(rig.current()['admission']['state'], 'CLOSED')
            self.assertEqual(rig.current()['admission_commit_floor'], rig.head())
            self.assertEqual(receipt.results, ((rig.w.v, rig.tuple_sha(), 'unknown'),), 'a dead runner proves nothing')
            # An in-flight dispatch may still retain its real output; it is reported as such.
            permit.consume()
            rig.kms.calls.append('sign')
        # Promotion and new work stop. The reservation stays, and no second Sign exists.
        self.refused('authority unavailable: admission is CLOSED', CONTROLLER.check_promotion, rig.git, rig.enrollment,
                     rig.request(), DISPATCH_RUN, 1, b'{}\n')
        self.refused('authority unavailable: admission is CLOSED', rig.controller(artifact=None).admit)
        with self.assertRaises(Refused):
            with holder.reserve(**rig.adapter_args()):
                raise AssertionError('reserve yielded')
        self.assertIn('tags/reserve/v' + rig.w.v, rig.w.refs)
        holder.retain_signature(SIGNATURE)
        receipt = CONTROLLER.contain(rig.git, writer, journal, rig.enrollment,
                                     ((rig.w.v, rig.tuple_sha()), ('0.99.0', digest(1))))
        self.assertEqual(receipt.results, ((rig.w.v, rig.tuple_sha(), 'retained'), ('0.99.0', digest(1), 'unknown')))
        self.assertEqual(rig.kms.signs, 1)
        # Containment does not wait for an unreachable source; it reports it, and everything stays unknown.
        rig = Rig()
        rig.ready()
        rig.table.unavailable = True
        receipt = CONTROLLER.contain(rig.git, FakeWriter(rig.w), CONTROLLER.Journal(rig.table, rig.enrollment),
                                     rig.enrollment, ((rig.w.v, rig.tuple_sha()),))
        self.assertEqual((receipt.github_closed, receipt.mirror_closed), (True, False))
        self.assertEqual(receipt.results, ((rig.w.v, rig.tuple_sha(), 'unknown'),))
        self.refused('authority unavailable: admission is CLOSED', CONTROLLER.check_promotion, rig.git, rig.enrollment,
                     rig.request(), DISPATCH_RUN, 1, b'{}\n')
        rig = Rig()
        rig.ready()
        writer = FakeWriter(rig.w)
        writer.fail = True
        receipt = CONTROLLER.contain(rig.git, writer, CONTROLLER.Journal(rig.table, rig.enrollment), rig.enrollment, ())
        self.assertEqual((receipt.github_closed, receipt.mirror_closed), (False, True))
        # After a partial reopen, containment closes both sides at the higher epoch: it never moves back.
        rig = Rig()
        rig.reserve_version()
        rig.install(epoch='8', github=False, revision='2', floor=HEAD)
        rig.install(state='CLOSED', mirror=False)
        receipt = CONTROLLER.contain(rig.git, FakeWriter(rig.w), CONTROLLER.Journal(rig.table, rig.enrollment),
                                     rig.enrollment, ())
        self.assertEqual((receipt.github_closed, receipt.mirror_closed), (True, True))
        self.assertEqual((rig.admission()['state'], rig.admission()['epoch']), ('CLOSED', '8'))
        self.assertEqual((rig.current()['admission']['state'], rig.current()['admission']['epoch']), ('CLOSED', '8'))


class Publication(Base):
    def published(self, rig):
        ctrl = rig.ready()
        rig.sign(ctrl)
        rig.publish()
        fresh = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence())
        rig.w.calls = []
        rig.kms.calls = []
        rig.table.reads = []
        return fresh

    def assert_zero_effect(self, rig, created, writes):
        self.assertEqual(rig.kms.calls, [])
        self.assertEqual(rig.git.created, created)
        self.assertEqual(rig.w.mutations, [])
        self.assertEqual(rig.table.reads, [], 'the observation needs no signer credentials or journal')
        self.assertEqual(len(rig.table.writes), writes)
        for provider in rig.providers[1:]:
            self.assertEqual((provider.oidc, provider.sts), ([], []))

    def test_rsig5_lost_publish_ack_is_read_only_success(self):
        rig = Rig()
        fresh = self.published(rig)
        created, writes = list(rig.git.created), len(rig.table.writes)
        answer = fresh.recover(rig.tuple_bytes())
        self.assertIsInstance(answer, CONTROLLER.AlreadyPublishedMatching)
        self.assertEqual((answer.version, answer.commit, answer.release_id, answer.tuple_sha256),
                         (rig.w.v, S, 500, rig.tuple_sha()))
        self.assertEqual((answer.manifest_sha256, answer.signature_sha256), (rig.w.manifest, sha(SIGNATURE)))
        self.assert_zero_effect(rig, created, writes)
        self.assertIsNone(fresh.journal)
        self.assertEqual(rig.published.calls, [rig.tuple_sha()], 'the exact-byte path ran')
        # Published bytes are authenticated first. Every mismatch refuses, still read-only.
        def bytes_change(rig, **changes):
            rig.published.answer = dataclasses.replace(rig.published.answer, **changes)
        for label, change, message in (
                ('archive digest', lambda r: bytes_change(r, archives=((r.w.archives[0]['name'], digest(1), r.w.archives[0]['size']),
                                                                       r.published.answer.archives[1])),
                 'reservation conflict: the published archives differ from the reserved tuple'),
                ('archive size', lambda r: bytes_change(r, archives=((r.w.archives[0]['name'], r.w.archives[0]['sha256'], 1),
                                                                     r.published.answer.archives[1])),
                 'reservation conflict: the published archives differ from the reserved tuple'),
                ('manifest', lambda r: bytes_change(r, manifest_sha256=digest(1)),
                 'reservation conflict: the published manifest differs from the reserved tuple'),
                ('notes', lambda r: bytes_change(r, notes_sha256=digest(1)),
                 'reservation conflict: the published notes differ from the release record'),
                ('fingerprint', lambda r: bytes_change(r, fingerprint='SHA256:' + 'B' * 43),
                 'reservation conflict: the published signature is not from the reserved signer'),
                ('principal', lambda r: bytes_change(r, principal='other'),
                 'reservation conflict: the published signature is not from the reserved signer'),
                ('namespace', lambda r: bytes_change(r, namespace='other'),
                 'reservation conflict: the published signature is not from the reserved signer'),
                ('signature', lambda r: bytes_change(r, signature_verified=False),
                 'reservation conflict: the published signature does not verify'),
                ('truthy verdict', lambda r: bytes_change(r, signature_verified='yes'),
                 'reservation conflict: the published signature does not verify'),
                ('asset set', lambda r: bytes_change(r, assets_complete=False),
                 'reservation conflict: the published asset set is not the expected set'),
                ('another release', lambda r: bytes_change(r, release_id=999),
                 'reservation conflict: the exact-byte proof is not of this release'),
                ('another tag', lambda r: bytes_change(r, tag_name='v0.99.0'),
                 'reservation conflict: the exact-byte proof is not of this release'),
                ('another commit', lambda r: bytes_change(r, commit=base.OTHER),
                 'reservation conflict: the exact-byte proof is not of this release'),
                ('a draft', lambda r: bytes_change(r, draft=True),
                 'reservation conflict: the exact-byte proof is not of this release'),
                ('target', lambda r: r.w.releases[-1].update(target_commitish=base.OTHER),
                 'reservation conflict: the release of v0.43.0 does not target the reserved commit'),
                ('final tag', lambda r: r.w.refs.update({'tags/v' + r.w.v: ('commit', base.OTHER)}),
                 'reservation conflict: the published v0.43.0 is not the reserved commit'),
                ('no final tag', lambda r: r.w.refs.pop('tags/v' + r.w.v),
                 'reservation conflict: the published v0.43.0 is not the reserved commit'),
                ('other reservation', lambda r: r.w.tags[r.w.refs['tags/reserve/v' + r.w.v][1]].update(message_bytes=b'{}\n'),
                 "reservation conflict: the reservation message is not a tuple: unknown field 'x'"),
                ('no reservation', lambda r: r.w.refs.pop('tags/reserve/v' + r.w.v),
                 'eligibility incomplete: v0.43.0 has no reservation to recover')):
            rig = Rig()
            fresh = self.published(rig)
            created, writes = list(rig.git.created), len(rig.table.writes)
            change(rig)
            with self.subTest(label):
                with self.assertRaises((Refused, ELIGIBILITY.Refused)) as caught:
                    fresh.recover(rig.tuple_bytes())
                if label == 'other reservation':
                    self.assertTrue(str(caught.exception).startswith('reservation conflict: the reservation message is not a tuple'))
                else:
                    self.assertEqual(str(caught.exception), message)
                self.assert_zero_effect(rig, created, writes)
        # A draft is unfinished: it needs current authority and gets no terminal observation.
        rig = Rig()
        fresh = self.published(rig)
        rig.w.releases[-1]['draft'] = True
        rig.w.refs.pop('tags/v' + rig.w.v)
        fresh.attach_journal(rig.table)
        self.assertIsInstance(fresh.recover(rig.tuple_bytes()), CONTROLLER.RetainedSignature)
        rig.install(state='CLOSED', mirror=False)
        self.refused('authority unavailable: admission is CLOSED', fresh.recover, rig.tuple_bytes())
        # The caller and E are authenticated before the published bytes.
        rig = Rig()
        fresh = self.published(rig)
        rig.w.runs[(DISPATCH_RUN, 1)]['actor_id'] = base.ATTACKER
        self.refused('not authorized: the release run was dispatched by an actor who is not a coordinator',
                     fresh.recover, rig.tuple_bytes())
        self.assertEqual(rig.published.calls, [])
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_recovery_after_later_publication_is_read_only(self):
        # A later version has shipped. The old published tuple is read-only success, with no new-version
        # rule, even with admission closed and the key revoked for new signing.
        rig = Rig()
        fresh = self.published(rig)
        created, writes = list(rig.git.created), len(rig.table.writes)
        rig.w.releases.append(dict(release_id=600, tag_name='v0.44.0', target_commitish=base.OTHER, draft=False,
                                   prerelease=True, assets=()))
        rig.install(state='CLOSED', epoch='9', mirror=False)
        original = rig.git.release_trust

        def trust(version, commit, signing):
            if signing:
                raise ControllerError('release trust: the key for ' + version + ' is revoked for new signing')
            return original(version, commit, signing)
        rig.git.release_trust = trust
        answer = fresh.recover(rig.tuple_bytes())
        self.assertIsInstance(answer, CONTROLLER.AlreadyPublishedMatching)
        self.assertEqual(answer.signer_revoked, True, 'the current status is reported apart from the result')
        self.assert_zero_effect(rig, created, writes)
        # An unpublished older version after a later publication: recovery works, signing needs authority.
        rig = Rig()
        ctrl = rig.ready()
        rig.sign(ctrl)
        rig.w.releases.append(dict(release_id=600, tag_name='v0.44.0', target_commitish=base.OTHER, draft=False,
                                   prerelease=True, assets=()))
        rig.kms.calls = []
        fresh = rig.session(rig.rerun())
        self.assertEqual(fresh.recover(rig.tuple_bytes()).signature, SIGNATURE)
        self.assertEqual(rig.kms.calls, [])
        # A publication that does not match the old tuple refuses.
        rig = Rig()
        fresh = self.published(rig)
        rig.w.releases.append(dict(release_id=600, tag_name='v0.44.0', target_commitish=base.OTHER, draft=False,
                                   prerelease=True, assets=()))
        rig.published.answer = dataclasses.replace(rig.published.answer, manifest_sha256=digest(1))
        self.refused('reservation conflict: the published manifest differs from the reserved tuple', fresh.recover,
                     rig.tuple_bytes())
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_already_published_matching_is_terminal(self):
        # A matching publication is a separate terminal observation, even with the approval revoked
        # and the channel disabled. It is history, not current eligibility.
        rig, receipt = Approval.archived(self)
        ctrl = rig.ready()
        rig.sign(ctrl)
        rig.publish()
        base.current_channel(rig.w, enabled=False, revoked_receipts=[receipt])
        rig.w.calls, rig.kms.calls, rig.table.reads = [], [], []
        created, writes = list(rig.git.created), len(rig.table.writes)
        fresh = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence())
        answer = fresh.recover(rig.tuple_bytes())
        self.assertIsInstance(answer, CONTROLLER.AlreadyPublishedMatching)
        self.assertEqual((answer.channel_enabled, answer.revoked_receipts, answer.signer_revoked),
                         (False, (receipt,), False))
        self.assertNotIn('approval_record', [c[0] for c in rig.w.calls])
        self.assert_zero_effect(rig, created, writes)
        # The type carries no signature bytes, no permit and no way on.
        for name in ('signature', 'consume', 'permit', 'promote', 'upload', 'notify'):
            self.assertFalse(hasattr(answer, name), name)
        self.assertNotIsInstance(answer, CONTROLLER.RetainedSignature)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            answer.release_id = 1
        # Nothing follows it: every path to credentials, a grant, a permit or Sign refuses.
        terminal = 'reservation conflict: v' + rig.w.v + ' is published; this observation is terminal'
        provider = rig.provider()
        self.refused(terminal, fresh.admit)
        self.refused(terminal, fresh.issue_credentials, provider)
        self.refused(terminal, fresh.open_session, rig.table)
        self.refused(terminal, lambda: fresh.reserve(**rig.adapter_args()).__enter__())
        self.refused(terminal, fresh.before_public_key)
        self.refused(terminal, fresh.before_sign, rig.payload_digest())
        self.refused(terminal, fresh.retain_signature, SIGNATURE)
        self.refused(terminal, fresh.authorization)
        self.assertEqual((provider.oidc, provider.sts), ([], []))
        self.assert_zero_effect(rig, created, writes)
        # A process that never asked also stops: a published version takes no credentials.
        other = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence())
        self.refused(terminal, other.admit)
        self.assertEqual(provider.oidc, [])
        # Missing or ambiguous proof is an incomplete read-only reconciliation, never success and never
        # a signing path. A metadata-only match has no proof at all.
        for change, message in (
                (lambda r: setattr(r.published, 'answer', None),
                 'eligibility incomplete: the published signature is missing'),
                (lambda r: setattr(r.published, 'error', ControllerError('the protected proof is unavailable')) or
                 setattr(r.published, 'answer', None),
                 'eligibility incomplete: the protected proof is unavailable')):
            rig, receipt = Approval.archived(self)
            rig.sign(rig.ready())
            rig.publish()
            change(rig)
            rig.kms.calls, rig.table.reads = [], []
            created, writes = list(rig.git.created), len(rig.table.writes)
            fresh = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence())
            with self.subTest(message=message):
                self.refused(message, fresh.recover, rig.tuple_bytes())
                self.assert_zero_effect(rig, created, writes)
                self.refused('reservation conflict: v' + rig.w.v + ' is published; this observation is terminal', fresh.admit)
        rig = Rig()
        fresh = self.published(rig)
        metadata_only = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence(), published_verifier=None)
        self.refused('eligibility incomplete: the published release has no exact-byte verification',
                     metadata_only.recover, rig.tuple_bytes())
        self.assertEqual(rig.kms.calls, [])
        # A missing protected record is not success: E's artifact has gone, and nothing opens a signing path.
        rig = Rig()
        fresh = self.published(rig)
        rig.w.artifacts[base.EVID_ART]['expired'] = True
        self.refused('eligibility incomplete: GitHub artifact: the artifact expired', fresh.recover, rig.tuple_bytes())
        self.assertEqual(rig.published.calls, [])
        # Every unfinished release keeps current authorization: the same revocation refuses a draft.
        rig, receipt = Approval.archived(self)
        rig.sign(rig.ready())
        rig.publish()
        rig.w.releases[-1]['draft'] = True
        rig.w.refs.pop('tags/v' + rig.w.v)
        base.current_channel(rig.w, enabled=False)
        fresh = rig.controller(rig.holder(), artifact=None, run_evidence=FakeEvidence())
        self.refused('not authorized: the founder approval channel is disabled', fresh.admit)
        fresh.attach_journal(rig.table)
        self.refused('not authorized: the founder approval channel is disabled', fresh.recover, rig.tuple_bytes())


class Approval(Base):
    """The approval phases, through the real eligibility module."""

    def first_use(self):
        world = base.first_machine_world()
        rig = Rig(world)
        receipt = world.add_card('release_1', via='first_use')
        run_id = world.approval_runs[0]['run_id']
        artifact_id = world.approval_runs[0]['artifact_id']
        return rig, receipt, run_id, artifact_id

    def archived(self, schema=2):
        """Release 1 under an approval the signer archived earlier. The request names no run."""
        world = base.first_machine_world()
        rig = Rig(world)
        receipt = world.add_card('release_1', schema=schema)
        rig.seed_archive(receipt)
        return rig, receipt

    def approval_key(self, receipt):
        return ('APPROVAL#' + receipt, 'RECORD')

    def to_session(self, rig):
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        return ctrl

    def test_rsig5_no_precredential_archive_call_and_pending_cannot_sign(self):
        # Every read of the archive before credentials would fail this test loudly.
        rig, receipt = self.archived()
        rig.w.errors['approval_record'] = AssertionError('an archive read before credentials')
        rig.w.errors['approval_lineage'] = AssertionError('an archive read before credentials')
        preliminary = rig.qualify()
        self.assertIsInstance(preliminary, ELIGIBILITY.Preliminary)
        self.assertNotIsInstance(preliminary, ELIGIBILITY.Qualified)
        pending = preliminary.cards[0]
        self.assertIsInstance(pending, ELIGIBILITY.PendingCard)
        self.assertEqual((pending.kind, pending.receipt_sha256, pending.source), ('release_1', receipt, 'pending_archive'))
        for name in ('request_bytes', 'receipt_bytes', 'projection_bytes', 'envelope_bytes'):
            self.assertFalse(hasattr(pending, name), name)
        # It permits only the reservation bookkeeping. E, its card and T are not regenerated.
        e_bytes = rig.w.members[(base.EVID_ART, 'release-record.json')]
        self.assertEqual(preliminary.evidence_bytes, e_bytes)
        self.assertEqual(preliminary.tuple_bytes, rig.tuple_bytes())
        rig.reserve_version()
        self.assertEqual(rig.table.reads, [])
        self.assertEqual(rig.table.writes, [])
        # A preliminary recovery is pending too, and the signer's admission step reads no archive.
        again = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        self.assertIsInstance(again, ELIGIBILITY.Preliminary)
        self.assertEqual(again.cards[0].source, 'pending_archive')
        self.assertEqual(rig.table.reads, [])
        ctrl = rig.controller()
        ctrl.admit()
        self.assertIsNone(ctrl.qualified)
        self.assertEqual(rig.table.reads, [])
        # A pending result never reaches KMS or promotion. Nothing takes it as authorization.
        self.refused('authority unavailable: a KMS request needs the reservation context', ctrl.before_public_key)
        self.refused('authority unavailable: a KMS request needs the reservation context', ctrl.before_sign,
                     rig.payload_digest())
        self.refused('authority unavailable: the signing session is not open',
                     lambda: ctrl.reserve(**rig.adapter_args()).__enter__())
        self.refused('authority unavailable: the promotion needs the signer authorization', CONTROLLER.check_promotion,
                     rig.git, rig.enrollment, rig.request(), DISPATCH_RUN, 1, None)
        self.assertEqual(rig.kms.calls, [])
        for name in ('authorization', 'grant', 'permit', 'consume'):
            self.assertFalse(hasattr(preliminary, name), name)
        # A pending reference that the archive cannot resolve refuses after STS, before any grant.
        rig = Rig(base.first_machine_world())
        receipt = rig.w.add_card('release_1')
        rig.w.archive.clear()
        ctrl = self.to_session(rig)
        self.refused('not authorized: the release_1 card has no accepted founder receipt', ctrl.open_session, rig.table)
        self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        self.refused('authority unavailable: the signing session is not open',
                     lambda: ctrl.reserve(**rig.adapter_args()).__enter__())
        self.assertEqual(rig.kms.calls, [])
        # Known conflicts still refuse before credentials: an extra run, a revocation card, a disabled
        # channel, a revoked receipt, a missing required card. Pending never relabels them.
        rig, receipt = self.archived()
        self.refused('evidence mismatch: a requested approval run names no card of the release record',
                     ELIGIBILITY.qualify_new, rig.w.request(approvals=[{'receipt_sha256': digest(61), 'run_id': 6100,
                                                                        'artifact_id': 6200}]), rig.git)
        def channel(rig, **changes):
            rig.w.ep['approval_channel'].update(changes)
            rig.w.publish_policy(P)
            rig.w.publish_policy(HEAD)
        rig, receipt = self.archived()
        channel(rig, enabled=False)
        self.refused('not authorized: the founder approval channel is disabled', rig.qualify)
        rig, receipt = self.archived()
        channel(rig, revoked_receipts=[receipt])
        self.refused('not authorized: the release_1 card receipt is revoked', rig.qualify)
        rig = Rig(base.first_machine_world())
        rig.w.add_card('revocation')
        self.refused('not authorized: a revocation card never permits signing', rig.qualify)
        rig = Rig(base.first_machine_world())
        rig.w.add_card('key_change')
        self.refused('not authorized: the release needs a release_1 card', rig.qualify)
        # Standing previews keep their path: no card, nothing pending, nothing read.
        rig = Rig()
        preliminary = rig.qualify()
        self.assertEqual(preliminary.cards, ())
        self.assertEqual(rig.table.reads, [])

    def test_rsig5_first_use_after_real_absence(self):
        rig, receipt, run_id, artifact_id = self.first_use()
        preliminary = rig.qualify()
        card = preliminary.cards[0]
        self.assertIsInstance(card, ELIGIBILITY.ProvisionalCard)
        self.assertEqual(card.source, 'first_use')
        self.assertEqual(rig.table.reads, [])
        ctrl = self.to_session(rig)
        self.assertNotIn(self.approval_key(receipt), rig.table.items)
        rig.table.reads = []
        ctrl.open_session(rig.table)
        # After the sole STS exchange and before any KMS request: authenticated absence, fresh live
        # proof, a conditional create and the exact readback.
        self.assertEqual(rig.kms.calls, [])
        stored = rig.table.items[self.approval_key(receipt)]
        reads = [r for r in rig.table.reads if r[:2] == ('get', 'APPROVAL#' + receipt)]
        self.assertEqual(len(reads), 3, 'absence, readback, and the signer-phase read of the accepted record')
        self.assertTrue(rig.table.writes[0][0][0].startswith('APPROVAL#'), rig.table.writes)
        self.assertEqual(ctrl.qualified.cards[0].source, 'archive')
        self.assertEqual(ctrl.qualified.cards[0].envelope_bytes, stored)
        self.assertIn(('run_approvals', run_id), rig.w.calls)
        # The envelope has schema 2, the exact projection bytes and the frozen lineage.
        envelope = EVIDENCE.parse_archive_envelope(stored)
        self.assertEqual(envelope.value['schema'], 2)
        self.assertEqual(envelope.value['provider_projection_body'].encode(), card.projection_bytes)
        self.assertEqual(envelope.value['receipt_body'].encode(), card.receipt_bytes)
        self.assertEqual(envelope.value['request_body'].encode(), card.request_bytes)
        self.assertEqual(envelope.acceptance, {
            'schema': 1, 'receipt_sha256': receipt, 'request_sha256': sha(card.request_bytes),
            'provider_projection_sha256': sha(card.projection_bytes), 'signer_workflow_sha': H,
            'run_id': DISPATCH_RUN, 'run_attempt': 1, 'job_id': SIGN_JOB, 'policy_commit': P,
            'policy_sha256': preliminary.policy_sha256, 'authority_sha256': AUTHORITY_SHA,
            'table_id': TABLE_ID, 'genesis_sha256': GENESIS_SHA, 'accepted_at': '2027-01-15T08:00:00Z'})
        # An unreadable archive is neither absence nor a way to fresh proof.
        rig, receipt, run_id, artifact_id = self.first_use()
        ctrl = self.to_session(rig)
        rig.table.read_faults.append('APPROVAL#')
        rig.w.calls = []
        self.refused('eligibility incomplete: the approval archive is unavailable', ctrl.open_session, rig.table)
        self.assertNotIn(('run_approvals', run_id), rig.w.calls)
        self.assertNotIn(self.approval_key(receipt), rig.table.items)
        self.assertEqual(rig.kms.calls, [])
        # Expired first-use proof refuses: before credentials for a new reservation, and at the signer
        # phase when recovery deferred it, because absence needs fresh proof.
        rig, receipt, run_id, artifact_id = self.first_use()
        rig.w.artifacts[artifact_id]['expired'] = True
        self.refused('eligibility incomplete: GitHub artifact: the artifact expired', rig.qualify)
        rig, receipt, run_id, artifact_id = self.first_use()
        rig.reserve_version()
        rig.w.artifacts[artifact_id]['expired'] = True
        deferred = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        self.assertIsInstance(deferred.cards[0], ELIGIBILITY.PendingCard)
        self.assertEqual((deferred.cards[0].run_id, deferred.cards[0].artifact_id), (run_id, artifact_id))
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        self.refused('eligibility incomplete: GitHub artifact: the artifact expired', ctrl.open_session, rig.table)
        self.assertNotIn(self.approval_key(receipt), rig.table.items)
        # Only expiry defers. Malformed or conflicting evidence is not relabelled.
        for change, message in (
                (lambda r, a: r.w.artifacts[a].update(name='another-artifact'),
                 'evidence mismatch: the release_1 approval artifact is not the request of its run'),
                (lambda r, a: r.w.artifacts[a].update(run_id=1),
                 'evidence mismatch: GitHub artifact: the artifact is not from run ' + 'RUN'),
                (lambda r, a: r.w.artifacts.pop(a), 'eligibility incomplete: GitHub artifact: not found')):
            rig, receipt, run_id, artifact_id = self.first_use()
            rig.reserve_version()
            change(rig, artifact_id)
            with self.subTest(message=message):
                with self.assertRaises(ELIGIBILITY.Refused) as caught:
                    ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
                self.assertEqual(str(caught.exception), message.replace('RUN', str(run_id)).replace(
                    'evidence mismatch: GitHub', 'eligibility incomplete: GitHub'))
        # A missing named run refuses: no accepted record, no fresh proof.
        rig, receipt, run_id, artifact_id = self.first_use()
        rig.reserve_version()
        ctrl = rig.controller(request=rig.w.request(approvals=[]))
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        self.refused('not authorized: the release_1 card has no accepted founder receipt', ctrl.open_session, rig.table)
        self.assertEqual(rig.kms.calls, [])
        # An uncertain create reconciles only the exact accepted record. Absence stays uncertain.
        for mode in ('lost_ack', 'unsent', 'down', 'replay'):
            rig, receipt, _, _ = self.first_use()
            ctrl = self.to_session(rig)
            rig.table.fail_next('APPROVAL#', mode)
            with self.subTest(mode=mode):
                if mode in ('lost_ack', 'replay'):
                    ctrl.open_session(rig.table)
                    self.assertEqual(ctrl.qualified.cards[0].source, 'archive')
                else:
                    self.refused('authority unavailable: the approval archive outcome is unknown', ctrl.open_session,
                                 rig.table)
                    self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
                self.assertEqual(rig.kms.calls, [])
        # A racer archived other bytes: no overwrite, and they must pass as an accepted record.
        rig, receipt, _, _ = self.first_use()
        ctrl = self.to_session(rig)
        rig.table.hook = lambda: rig.table.seed('APPROVAL#' + receipt, 'RECORD', b'other\n')
        self.refused_code('evidence mismatch', ctrl.open_session, rig.table)
        self.assertEqual(rig.table.items[self.approval_key(receipt)], b'other\n')
        self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        # A racer archived the same card first: its original acceptance is kept, never rewritten.
        rig, receipt, _, _ = self.first_use()
        card = rig.qualify().cards[0]
        ctrl = self.to_session(rig)
        accepted = json.loads(rig.w.envelope(card.request_bytes, card.receipt_bytes, card.projection_bytes))
        accepted['acceptance'].update(genesis_sha256=GENESIS_SHA, authority_sha256=AUTHORITY_SHA, run_id=4321)
        earlier = canon(accepted)
        rig.table.hook = lambda: rig.table.seed('APPROVAL#' + receipt, 'RECORD', earlier)
        ctrl.open_session(rig.table)
        self.assertEqual(rig.table.items[self.approval_key(receipt)], earlier)
        self.assertEqual(ctrl.qualified.cards[0].envelope_bytes, earlier)
        # The aggregate bound holds before any write.
        rig, receipt, _, _ = self.first_use()
        ctrl = self.to_session(rig)
        original = CONTROLLER.ev.parse_archive_envelope

        def too_large(data):
            raise ControllerError('archive envelope is larger than 65536 bytes')
        CONTROLLER.ev.parse_archive_envelope = too_large
        try:
            self.refused('input limit exceeded: archive envelope is larger than 65536 bytes', ctrl.open_session,
                         rig.table)
        finally:
            CONTROLLER.ev.parse_archive_envelope = original
        self.assertEqual(rig.table.writes, [])

    def test_rsig5_accepted_archive_reuse_preserves_original_bytes(self):
        # An accepted first use; then the original artifact expires. The old named run stays in the request.
        rig, receipt, run_id, artifact_id = self.first_use()
        rig.reserve_version()
        ctrl = rig.session()
        rig.sign(ctrl)
        stored = rig.table.items[self.approval_key(receipt)]
        rig.w.artifacts[artifact_id]['expired'] = True
        for label, attempt, named in (('old named run', 2, rig.w.approval_runs), ('no named run', 3, [])):
            rig.w.calls = []
            writes = len(rig.table.writes)
            rig.add_job(DISPATCH_RUN, attempt, SIGN_JOB + attempt - 1)
            fresh = rig.controller(rig.holder(DISPATCH_RUN, attempt, SIGN_JOB + attempt - 1), artifact=None,
                                   request=rig.w.request(dispatch_run_attempt=attempt, approvals=named))
            rig.session(fresh)
            with self.subTest(label):
                card = fresh.qualified.cards[0]
                self.assertEqual((card.source, card.envelope_bytes), ('archive', stored))
                self.assertEqual(rig.table.items[self.approval_key(receipt)], stored)
                self.assertFalse([w for w in rig.table.writes[writes:] if w[0][0].startswith('APPROVAL#')])
                self.assertNotIn('run_approvals', [c[0] for c in rig.w.calls])
                self.assertNotIn('approval_responses', [c[0] for c in rig.w.calls])
                self.assertEqual(fresh.recover(rig.tuple_bytes()).signature, SIGNATURE)
        # A request that names another run for the accepted receipt refuses.
        other = [dict(rig.w.approval_runs[0], run_id=run_id + 50)]
        rig.add_job(DISPATCH_RUN, 4, SIGN_JOB + 3)
        fresh = rig.controller(rig.holder(DISPATCH_RUN, 4, SIGN_JOB + 3), artifact=None,
                               request=rig.w.request(dispatch_run_attempt=4, approvals=other))
        with self.assertRaises(Refused):
            rig.session(fresh)
        # A new reservation under an earlier accepted key card: pending first, then the original bytes.
        world = base.key_change_world()
        rig = Rig(world)
        scope = world.scope_for('key_change')
        scope['key_arn'] = base.NEW_KEY_ARN
        receipt = world.add_card('key_change', scope=scope)
        stored = rig.seed_archive(receipt)
        rig.install()
        self.assertEqual(rig.qualify().cards[0].source, 'pending_archive')
        rig.reserve_version()
        ctrl = rig.session()
        self.assertEqual(ctrl.qualified.cards[0].envelope_bytes, stored)
        self.assertEqual(rig.table.items[self.approval_key(receipt)], stored)
        # A damaged or foreign record refuses: its hashes and lineage are validated, not trusted.
        for label, change, code in (
                ('lineage', lambda e: e['acceptance'].update(genesis_sha256=digest(5)), 'not authorized'),
                ('table', lambda e: e['acceptance'].update(table_id='fedcba98-7654-4321-8fed-cba987654321'), 'not authorized'),
                ('policy digest', lambda e: e['acceptance'].update(policy_sha256=digest(5)), 'not authorized'),
                ('signer', lambda e: e['acceptance'].update(signer_workflow_sha=base.OTHER), 'not authorized'),
                ('audit', lambda e: e.update(provider_projection_body=e['provider_projection_body'].replace(
                    '"audit":{', '"audit":{ ')), 'evidence mismatch')):
            rig, receipt = self.archived()
            envelope = json.loads(rig.table.items[self.approval_key(receipt)])
            change(envelope)
            rig.table.items[self.approval_key(receipt)] = canon(envelope)
            ctrl = self.to_session(rig)
            with self.subTest(label):
                self.refused_code(code, ctrl.open_session, rig.table)
                self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)

    def test_rsig5_stable_identity_survives_drift_and_mismatch_refuses(self):
        # Irrelevant provider metadata drifts between the reservation and the signer phase: the same
        # receipt, a new audit observation, and the archive holds what the signer proved.
        rig, receipt, run_id, artifact_id = self.first_use()
        first = rig.qualify().cards[0]
        rig.reserve_version()
        base.respond(rig.w, run_id, repository=digest(95), run_attempt=digest(96), artifact=digest(97), job=digest(98))
        ctrl = rig.session()
        stored = EVIDENCE.parse_archive_envelope(rig.table.items[self.approval_key(receipt)])
        self.assertEqual(stored.receipt.sha256, first.receipt_sha256)
        self.assertEqual(stored.value['receipt_body'].encode(), first.receipt_bytes)
        self.assertEqual(json.loads(stored.value['provider_projection_body'])['identity'],
                         json.loads(first.projection_bytes)['identity'])
        self.assertEqual(ctrl.qualified.cards[0].envelope_bytes, rig.table.items[self.approval_key(receipt)])
        # After acceptance, later drift changes nothing: no fresh body has to equal the original.
        original = rig.table.items[self.approval_key(receipt)]
        base.respond(rig.w, run_id, repository=digest(101), job=digest(102))
        fresh = rig.session(rig.rerun())
        self.assertEqual(fresh.qualified.cards[0].envelope_bytes, original)
        # Typed provenance still refuses, each with its own reason.
        for label, change, message in (
                ('source', lambda r, a: r.w.put(HEAD, base.APPROVAL_WF, b'name: release-approval\nrun: forged\n'),
                 'not authorized: the release_1 approval workflow is not its pinned bytes'),
                ('scope', lambda r, a: base.restate(r.w, r.w.approval_runs[0]['run_id'], a,
                                                    scope=dict(r.w.full_scope(), design_landing=digest(77))),
                 'not authorized: the release_1 card does not name the founder-approved receipt'),
                ('reviewer', lambda r, a: r.w.reviews[r.w.approval_runs[0]['run_id']][0].update(user_id=base.PERSON),
                 'not authorized: the release_1 review is not the founder approval in the pinned environment'),
                ('run', lambda r, a: r.w.jobs[(r.w.approval_runs[0]['run_id'], 1)][0].update(job_id=9999),
                 'not authorized: the release_1 card does not name the founder-approved receipt')):
            rig, receipt, run_id, artifact_id = self.first_use()
            change(rig, artifact_id)
            with self.subTest(label, phase='before credentials'):
                self.refused(message, rig.qualify)
            rig, receipt, run_id, artifact_id = self.first_use()
            ctrl = self.to_session(rig)
            change(rig, artifact_id)
            with self.subTest(label, phase='signer'):
                self.refused(message, ctrl.open_session, rig.table)
                self.assertNotIn(self.approval_key(receipt), rig.table.items)
        # The archived receipt's scope must still cover the release.
        rig = Rig(base.first_machine_world())
        receipt = rig.w.add_card('release_1', scope=dict(rig.w.full_scope(), version='0.44.0'))
        rig.seed_archive(receipt)
        ctrl = self.to_session(rig)
        self.refused('not authorized: the release_1 card does not cover the release version', ctrl.open_session, rig.table)
        # Revocation and a disabled channel are checked apart from the identity, at every stage.
        for changes, message in (({'enabled': False}, 'not authorized: the founder approval channel is disabled'),
                                 ({'revoked_receipts': 'R'}, 'not authorized: the release_1 card receipt is revoked')):
            rig, receipt = self.archived()
            ctrl = self.to_session(rig)
            values = {k: ([receipt] if v == 'R' else v) for k, v in changes.items()}
            base.current_channel(rig.w, **values)
            with self.subTest(message=message):
                self.refused(message, ctrl.open_session, rig.table)
            rig, receipt = self.archived()
            ctrl = rig.ready()
            base.current_channel(rig.w, **{k: ([receipt] if v == 'R' else v) for k, v in changes.items()})
            with self.subTest(message=message, stage='reserve'):
                with self.assertRaises(Refused) as caught:
                    with ctrl.reserve(**rig.adapter_args()):
                        raise AssertionError('reserve yielded')
                self.assertEqual(str(caught.exception), message)
            self.assertEqual(rig.kms.calls, [])

    def test_rsig5_v1_first_use_refuses_and_retained_v1_is_kept(self):
        # New first-use archives need schema 2. A v1 receipt never takes a reservation.
        rig = Rig(base.first_machine_world())
        receipt = rig.w.add_card('release_1', via='first_use', schema=1)
        self.refused('not authorized: the release_1 card does not name the founder-approved receipt', rig.qualify)
        self.assertEqual(rig.git.created, [])
        self.assertNotIn(self.approval_key(receipt), rig.table.items)
        # An authentic retained v1 archive keeps its original meaning: no conversion and no rewrite.
        rig, receipt = self.archived(schema=1)
        e_bytes = rig.w.members[(base.EVID_ART, 'release-record.json')]
        stored = rig.table.items[self.approval_key(receipt)]
        self.assertEqual(rig.qualify().cards[0].source, 'pending_archive')
        rig.reserve_version()
        ctrl = rig.session()
        card = ctrl.qualified.cards[0]
        self.assertEqual((card.source, card.envelope_bytes), ('archive', stored))
        self.assertEqual(EVIDENCE.parse_approval_receipt(card.receipt_bytes).value['schema'], 1)
        self.assertEqual(EVIDENCE.parse_approval_receipt(card.receipt_bytes).value['issuer']['provider_evidence_sha256'],
                         sha(card.projection_bytes))
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        self.assertEqual(rig.table.items[self.approval_key(receipt)], stored)
        self.assertEqual(ctrl.qualified.evidence_bytes, e_bytes)
        self.assertFalse([w for w in rig.table.writes if w[0][0].startswith('APPROVAL#')])
        # v1 is byte-sensitive as frozen: a changed audit response refuses.
        rig, receipt = self.archived(schema=1)
        envelope = json.loads(rig.table.items[self.approval_key(receipt)])
        projection = json.loads(envelope['provider_projection_body'])
        projection['source_responses'][0]['sha256'] = digest(1)
        envelope['provider_projection_body'] = canon(projection).decode()
        rig.table.items[self.approval_key(receipt)] = canon(envelope)
        ctrl = self.to_session(rig)
        self.refused_code('evidence mismatch', ctrl.open_session, rig.table)

    def test_rsig5_grant_comes_from_sts_and_the_reserve_receipt_transfers(self):
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        # A pending grant is not durable and carries no deadline.
        self.assertEqual(rig.table.writes, [])
        pending = [r for r in rig.evidence.rows if r[1] == 'pending'][0][2]
        self.assertEqual(pending, {'grant_id': ctrl.grant_id, 'tuple_sha256': rig.tuple_sha(), 'epoch': EPOCH,
                                   'signer_workflow_sha': H, 'policy_commit': P})
        provider = rig.provider(lifetime=800, role_max=7200)
        ctrl.issue_credentials(provider)
        self.assertEqual(rig.table.writes, [])
        ctrl.open_session(rig.table)
        grant = json.loads(rig.table.items[('GRANT#' + ctrl.grant_id, 'RECORD')])
        self.assertEqual(sorted(grant), sorted((
            'schema', 'grant_id', 'repository_id', 'version', 'tuple_sha256', 'epoch', 'fence', 'signer_workflow_sha',
            'key_arn', 'fingerprint', 'policy_commit', 'policy_sha256', 'trust_sha256', 'admission_commit',
            'admission_sha256', 'run_id', 'run_attempt', 'job_id', 'jwt_iat', 'jwt_exp', 'sts_expiration',
            'role_max_session_duration', 'deadline')))
        self.assertEqual(grant['sts_expiration'], str(T0 + 800))
        self.assertEqual(grant['role_max_session_duration'], '7200')
        self.assertEqual(grant['fence'], '2')
        self.assertLessEqual(int(grant['deadline']), int(grant['sts_expiration']))
        # `admission_commit` keeps its meaning: the commit this run observed, and its document's digest.
        self.assertEqual(grant['admission_commit'], HEAD)
        self.assertEqual(grant['admission_sha256'], sha(rig.w.files[(ADMISSION, HEAD)]))
        self.assertEqual(json.loads(rig.table.items[('FENCE#' + str(REPO_ID) + '#' + EPOCH, 'COUNTER')])['fence'], '2')
        # The fresh reserve-job receipt initialized the row, within the same workflow holder.
        row = rig.row()
        self.assertEqual((row['phase'], row['tuple_sha256'], row['run_id'], row['job_id']),
                         ('RESERVED', rig.tuple_sha(), str(DISPATCH_RUN), str(RESERVE_JOB_ID)))
        self.assertEqual(row['genesis_sha256'], GENESIS_SHA)
        # A grant write with no acknowledged readback gives no KMS.
        for mode in ('lost_ack', 'unsent', 'down', 'replay'):
            rig = Rig()
            ctrl = self.to_session(rig)
            rig.table.fail_next('GRANT#', mode)
            with self.subTest(mode=mode):
                self.refused('authority unavailable: the grant has no fresh acknowledgement', ctrl.open_session, rig.table)
            with self.assertRaises(Refused):
                with ctrl.reserve(**rig.adapter_args()):
                    raise AssertionError('reserve yielded')
            self.assertEqual(rig.kms.calls, [])

    def test_rsig5_original_evidence_survives_archive_and_recovery(self):
        """At the signer level: the original E and audit bytes survive
        reservation, archive and recovery, with no overwrite and no second Sign."""
        rig, receipt, run_id, artifact_id = self.first_use()
        e_bytes = rig.w.members[(base.EVID_ART, 'release-record.json')]
        first = rig.qualify()
        card = first.cards[0]
        audit = json.loads(card.projection_bytes)['audit']
        rig.reserve_version()
        reservation = rig.w.tags[rig.w.refs['tags/reserve/v' + rig.w.v][1]]['message_bytes']
        self.assertEqual(reservation, first.tuple_bytes)
        ctrl = rig.session()
        stored = rig.table.items[self.approval_key(receipt)]
        approval_puts = [w for w in rig.table.writes if w[0][0].startswith('APPROVAL#')]
        self.assertEqual(len(approval_puts), 1)
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        self.assertEqual(rig.kms.signs, 1)
        retained = rig.table.items[(rig.tuple_pk(), 'SIGNATURE')]

        # Later the provider evidence changes or goes: responses re-render, the artifact expires and
        # the review history is gone.
        base.respond(rig.w, run_id, repository=digest(95), run_attempt=digest(96), artifact=digest(97), job=digest(98))
        rig.w.artifacts[artifact_id]['expired'] = True
        rig.w.reviews[run_id] = []
        rig.w.calls = []
        fresh = rig.session(rig.rerun())

        # Recovery returns the real retained bytes under the original E, T and archive record.
        answer = fresh.recover(first.tuple_bytes)
        self.assertEqual(answer.signature, retained)
        self.assertEqual(rig.kms.signs, 1)
        again = fresh.qualified
        self.assertEqual((again.evidence_bytes, again.evidence_sha256), (e_bytes, first.evidence_sha256))
        self.assertEqual(again.tuple_bytes, first.tuple_bytes)
        self.assertEqual(rig.w.tags[rig.w.refs['tags/reserve/v' + rig.w.v][1]]['message_bytes'], reservation)
        self.assertEqual(rig.w.members[(base.EVID_ART, 'release-record.json')], e_bytes)
        self.assertEqual(rig.table.items[self.approval_key(receipt)], stored)
        self.assertEqual(len([w for w in rig.table.writes if w[0][0].startswith('APPROVAL#')]), 1, 'no overwrite')
        card_again = again.cards[0]
        self.assertEqual((card_again.source, card_again.envelope_bytes), ('archive', stored))
        self.assertEqual((card_again.request_bytes, card_again.receipt_bytes, card_again.projection_bytes),
                         (card.request_bytes, card.receipt_bytes, card.projection_bytes))
        self.assertEqual(json.loads(card_again.projection_bytes)['audit'], audit)
        self.assertNotEqual(audit['source_responses'], rig.w.responses[run_id])
        # Nothing fresh replaced the accepted proof.
        for name in ('run_approvals', 'approval_responses'):
            self.assertNotIn(name, [c[0] for c in rig.w.calls])
        # A later attempt to sign this tuple still has no way to a second Sign call.
        with self.assertRaises(Refused):
            rig.sign(fresh)
        self.assertEqual(rig.kms.signs, 1)
        # Current authority stays separate: a channel disabled later refuses the unfinished recovery.
        base.current_channel(rig.w, enabled=False)
        self.refused('not authorized: the founder approval channel is disabled', fresh.recover, first.tuple_bytes)
        self.assertEqual(rig.kms.signs, 1)


class Lineage(Base):
    """Immutable lineage, the bound admission mirror and the reserve job constants."""

    def test_rsig5_genesis_and_authority_digests_are_stable(self):
        rig = Rig()
        # No body embeds a digest; nothing mutable is in either body; no hash depends on the other way.
        self.assertEqual(sorted(json.loads(GENESIS)), ['authority', 'bootstrap', 'schema'])
        self.assertNotIn(GENESIS_SHA.encode(), GENESIS)
        self.assertNotIn(AUTHORITY_SHA.encode(), GENESIS)
        self.assertEqual(CONTROLLER.authority_sha256(AUTHORITY), AUTHORITY_SHA)
        for name in ('state', 'revision', 'admission_commit', 'admission_sha256', 'admission_commit_floor', 'grant_id'):
            self.assertNotIn(name.encode(), GENESIS)
        policy = rig.w.files[('release-keys/policy.json', P)]
        self.assertNotIn(GENESIS_SHA.encode(), policy, 'the initial policy holds no GENESIS digest')
        parsed = CONTROLLER.parse_genesis(GENESIS, rig.enrollment)
        self.assertEqual(parsed['bootstrap']['epoch'], '1')
        # Normal maintenance leaves both digests and the stored body unchanged, while H, policy and epoch move.
        ctrl = rig.ready()
        run = maintenance_to_ready(self, rig)
        rig.clock.value = run.not_before + CONTROLLER.CLOCK_SKEW + 1
        run.apply_update(approved(rig))
        run.verify_readback(approved(rig))
        run.reopen()
        self.assertEqual(rig.table.items[GENESIS_KEY], GENESIS)
        self.assertEqual(rig.current()['admission']['genesis_sha256'], GENESIS_SHA)
        self.assertEqual(rig.current()['admission']['epoch'], '8')
        self.assertEqual(rig.enrollment.genesis_sha256, GENESIS_SHA)
        self.assertEqual(rig.session(rig.rerun()).journal.lineage(),
                         {'table_id': TABLE_ID, 'genesis_sha256': GENESIS_SHA, 'authority_sha256': AUTHORITY_SHA})
        # The closed bodies refuse every other shape: unknown keys, floats, non-canonical bytes.
        for label, body in (
                ('unknown', canon({'schema': 1, 'authority': AUTHORITY, 'bootstrap': BOOTSTRAP, 'revision': '1'})),
                ('authority unknown', canon({'schema': 1, 'authority': dict(AUTHORITY, state='ACTIVE'), 'bootstrap': BOOTSTRAP})),
                ('bootstrap unknown', canon({'schema': 1, 'authority': AUTHORITY, 'bootstrap': dict(BOOTSTRAP, x=1)})),
                ('no bootstrap field', canon({'schema': 1, 'authority': AUTHORITY,
                                              'bootstrap': {k: v for k, v in BOOTSTRAP.items() if k != 'epoch'}})),
                ('schema', canon({'schema': 2, 'authority': AUTHORITY, 'bootstrap': BOOTSTRAP})),
                ('spaces', b' ' + GENESIS),
                ('float', GENESIS.replace(b'"schema":1', b'"schema":1.0'))):
            with self.subTest(label):
                with self.assertRaises(ControllerError):
                    CONTROLLER.parse_genesis(body, dataclasses.replace(rig.enrollment, genesis_sha256=sha(body)))
        # Each literal authority field is fixed. A changed projection needs explicit reenrollment.
        for key, value in (('repository', 'other/rdy-core'), ('repository_id', '1'), ('owner_id', '1'),
                           ('account_id', '999999999999'), ('region', 'us-west-2'), ('table', 'other-table'),
                           ('table_id', '11111111-89ab-4cde-8f01-23456789abcd'), ('admission_ref', 'refs/heads/other'),
                           ('admission_path', 'other.json'), ('concurrency_group', 'other')):
            changed = dict(AUTHORITY, **{key: value})
            body = canon({'schema': 1, 'authority': changed, 'bootstrap': BOOTSTRAP})
            with self.subTest(key):
                with self.assertRaises(ControllerError):
                    CONTROLLER.parse_genesis(body, dataclasses.replace(
                        rig.enrollment, genesis_sha256=sha(body), authority_sha256=sha(canon(changed))))
        # The authority digest and the genesis digest are separately pinned.
        with self.assertRaises(ControllerError):
            CONTROLLER.parse_genesis(GENESIS, dataclasses.replace(rig.enrollment, authority_sha256=digest(1)))
        with self.assertRaises(ControllerError):
            CONTROLLER.parse_genesis(GENESIS, dataclasses.replace(rig.enrollment, genesis_sha256=digest(1)))
        # The session authenticates the actual table: a recreated table or changed GENESIS refuses.
        for label, change in (('table id', lambda t: t.described.update(TableId='11111111-89ab-4cde-8f01-23456789abcd')),
                              ('status', lambda t: t.described.update(TableStatus='UPDATING')),
                              ('arn', lambda t: t.described.update(TableArn=t.arn + 'x')),
                              ('key schema', lambda t: t.described.update(KeySchema=[])),
                              ('genesis', lambda t: t.items.update({GENESIS_KEY: b'{}\n'})),
                              ('no genesis', lambda t: t.items.pop(GENESIS_KEY))):
            rig = Rig()
            rig.reserve_version()
            change(rig.table)
            ctrl = rig.controller()
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            with self.subTest(label):
                self.refused_code('authority unavailable', ctrl.open_session, rig.table)
            self.assertNotIn(rig.version_key(), rig.table.items)
            self.assertEqual(rig.kms.calls, [])
        # An archive accepted under another table refuses rather than converting.
        rig, receipt = Approval.archived(self)
        rig.table.items[('APPROVAL#' + receipt, 'RECORD')] = rig.w.archive[receipt]
        base.accept(rig.w, receipt, table_id='fedcba98-7654-4321-8fed-cba987654321')
        rig.table.items[('APPROVAL#' + receipt, 'RECORD')] = rig.w.archive[receipt]
        ctrl = Approval.to_session(self, rig)
        self.refused('not authorized: the archived release_1 approval is not from the enrolled archive', ctrl.open_session,
                     rig.table)

    def test_rsig5_mirror_prepared_before_active_commit(self):
        # The reopen order: CLOSED at F, the mirror holds A and F, then A is published last.
        rig = Rig()
        rig.ready()
        writer = FakeWriter(rig.w)
        run = maintenance_run(rig, writer)
        run.close()
        close_commit = rig.head()
        self.assertEqual(rig.current()['admission_commit_floor'], close_commit, 'the close mirrors its known commit')
        self.assertEqual(rig.current()['admission'], rig.admission())
        self.assertEqual(rig.current()['revision'], '2')
        run.acquire_lane()
        run.prove_drained(census(rig))
        rig.clock.value = run.not_before + CONTROLLER.CLOCK_SKEW + 1
        run.apply_update(approved(rig))
        run.verify_readback(approved(rig))
        seen = {}

        def at_commit(data):
            seen['mirror'] = rig.current()
            seen['github'] = rig.admission()
            seen['data'] = data
        writer.before = at_commit
        run.reopen()
        self.assertEqual(seen['mirror']['admission']['state'], 'ACTIVE', 'the mirror is prepared first')
        self.assertEqual(seen['github']['state'], 'CLOSED', 'GitHub stays closed until it is last')
        self.assertEqual(canon(seen['mirror']['admission']), seen['data'])
        self.assertEqual(seen['mirror']['admission_sha256'], sha(seen['data']))
        self.assertEqual(seen['mirror']['admission_commit_floor'], close_commit, 'the floor already exists')
        self.assertEqual(rig.current()['admission_commit_floor'], close_commit)
        self.assertEqual(rig.current()['revision'], '3')
        self.assertEqual((rig.admission()['state'], rig.admission()['epoch']), ('ACTIVE', '8'))
        self.assertEqual(rig.w.files[(ADMISSION, rig.head())], canon(rig.current()['admission']))
        self.assertIn(('CONTROL#' + str(REPO_ID), 'EVENT#8#3'), rig.table.items)
        # The observed commit descends strictly from the floor, so new work proceeds.
        fresh = rig.session(rig.rerun())
        grant = json.loads(rig.table.items[('GRANT#' + fresh.grant_id, 'RECORD')])
        self.assertEqual(grant['admission_commit'], rig.head())
        self.assertEqual(grant['epoch'], '8')
        # An unrelated later commit with the exact admission bytes keeps working.
        later = format(0xb001, '040x')
        rig.w.commits[later] = (rig.head(),)
        for (path, commit), body in list(rig.w.files.items()):
            if commit == rig.head():
                rig.w.files[(path, later)] = body
        rig.w.refs['heads/main'] = ('commit', later)
        again = rig.session(rig.rerun(attempt=3))
        self.assertEqual(json.loads(rig.table.items[('GRANT#' + again.grant_id, 'RECORD')])['admission_commit'], later)

        def closed_prepared():
            """Interrupted after the mirror took A and before A reached GitHub."""
            rig = Rig()
            rig.reserve_version()
            writer = FakeWriter(rig.w)
            run = maintenance_run(rig, writer)
            run.close()
            run.acquire_lane()
            run.prove_drained(census(rig))
            rig.clock.value = (run.not_before or T0) + CONTROLLER.CLOCK_SKEW + 1
            run.apply_update(approved(rig))
            run.verify_readback(approved(rig))
            writer.fail = True
            self.refused('authority unavailable: the reopen is incomplete; admission stays closed', run.reopen)
            return rig, writer
        # Boundary 1: mirror prepared, ACTIVE not committed. GitHub stays closed; nobody is admitted.
        rig, writer = closed_prepared()
        self.assertEqual((rig.current()['admission']['state'], rig.admission()['state']), ('ACTIVE', 'CLOSED'))
        self.refused('authority unavailable: admission is CLOSED', rig.controller().admit)
        # A replacement recovers with the whole protocol and every reopen advances the epoch again.
        writer.fail = False
        replacement = maintenance_run(rig, writer)
        replacement.close()
        self.assertEqual((rig.admission()['state'], rig.admission()['epoch'], rig.current()['admission']['epoch']),
                         ('CLOSED', '8', '8'), 'the prepared epoch is never reused')
        replacement.acquire_lane()
        replacement.prove_drained(census(rig))
        rig.clock.value = (replacement.not_before or T0) + CONTROLLER.CLOCK_SKEW + 1
        replacement.apply_update(approved(rig))
        replacement.verify_readback(approved(rig))
        replacement.reopen()
        self.assertEqual((rig.admission()['state'], rig.admission()['epoch']), ('ACTIVE', '9'))
        # Boundary 2: the close wrote GitHub and not the mirror: nothing new finalizes, either way.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        run = maintenance_run(rig)
        rig.table.fail_next('CURRENT', 'down')
        self.refused('authority unavailable: the close is incomplete', run.close)
        self.refused('authority unavailable: admission is CLOSED', ctrl.open_session, rig.table)
        # Boundary 3: the mirror CAS itself has an unknown outcome; readback decides, and a lost
        # acknowledgement with durable bytes is the same document.
        rig = Rig()
        rig.reserve_version()
        run = maintenance_run(rig)
        run.close()
        run.acquire_lane()
        run.prove_drained(census(rig, jobs=(CONTROLLER.CensusJob(DISPATCH_RUN, 1, SIGN_JOB, 'completed', 'none'),), issuers=()))
        run.apply_update(approved(rig))
        run.verify_readback(approved(rig))
        rig.table.fail_next('CURRENT', 'unsent')
        self.refused('authority unavailable: the reopen is incomplete; admission stays closed', run.reopen)
        self.assertEqual((rig.current()['admission']['state'], rig.admission()['state']), ('CLOSED', 'CLOSED'))
        # A signer never repairs maintenance: the session reads and refuses, and writes nothing to CONTROL.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        rig.install(epoch='8', github=False)
        before = rig.table.items[CURRENT_KEY]
        self.refused('authority unavailable: the journal mirror disagrees with the admission record', ctrl.open_session,
                     rig.table)
        self.assertEqual(rig.table.items[CURRENT_KEY], before)
        self.assertEqual(rig.table.writes, [])
        # Stale floors, rollback and stale grants refuse.
        for label, change, message in (
                ('floor equals an ACTIVE commit', lambda r: r.install(floor=HEAD),
                 'the admission commit does not descend strictly from the journal floor'),
                ('floor ahead of the commit', lambda r: (r.w.commits.__setitem__(format(0xc1, '040x'), (HEAD,)),
                                                         r.install(floor=format(0xc1, '040x'))),
                 'the admission commit does not descend strictly from the journal floor'),
                ('floor on another line', lambda r: r.install(floor=base.OTHER),
                 'the admission commit does not descend strictly from the journal floor'),
                ('digest', lambda r: r.table.items.update({CURRENT_KEY: canon(dict(r.current(), admission_sha256=digest(1)))}),
                 'the journal mirror is malformed'),
                ('unknown field', lambda r: r.table.items.update({CURRENT_KEY: canon(dict(r.current(), extra=1))}),
                 "unknown field 'extra'"),
                ('no floor', lambda r: r.table.items.update({CURRENT_KEY: canon(
                    {k: v for k, v in r.current().items() if k != 'admission_commit_floor'})}),
                 'the journal mirror is malformed'),
                ('revision', lambda r: r.table.items.update({CURRENT_KEY: canon(dict(r.current(), revision='0'))}),
                 'the journal mirror is malformed'),
                ('old epoch mirror', lambda r: r.install(epoch='6', github=False),
                 'the journal mirror disagrees with the admission record'),
                ('old epoch on GitHub', lambda r: r.install(epoch='6', mirror=False),
                 'the journal mirror disagrees with the admission record')):
            rig = Rig()
            rig.reserve_version()
            ctrl = rig.controller()
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            change(rig)
            with self.subTest(label):
                self.refused('authority unavailable: ' + message, ctrl.open_session, rig.table)
                self.assertNotIn(('GRANT#' + ctrl.grant_id, 'RECORD'), rig.table.items)
        # A mirror that moves backwards under a running holder refuses, though the bytes are valid.
        rig = Rig()
        ctrl = rig.ready()
        rig.install(floor=S)
        with self.assertRaises(Refused) as caught:
            with ctrl.reserve(**rig.adapter_args()):
                raise AssertionError('reserve yielded')
        self.assertEqual(str(caught.exception), 'authority unavailable: the journal floor regressed')
        # A draining close keeps the exact-holder rule; the floor may equal the commit while closing.
        rig = Rig()
        ctrl = rig.ready()
        run = maintenance_run(rig)
        run.close(holder=rig.named(ctrl))
        self.assertEqual(rig.current()['admission_commit_floor'], rig.head())
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        # A pre-close ACTIVE document republished after the close never revives an old grant.
        rig = Rig()
        ctrl = rig.ready()
        old = rig.w.files[(ADMISSION, HEAD)]
        writer = FakeWriter(rig.w)
        run = maintenance_run(rig, writer)
        run.close()
        writer.commit(old)
        with self.assertRaises(Refused) as caught:
            rig.sign(ctrl)
        self.assertEqual(str(caught.exception), 'authority unavailable: the journal mirror is CLOSED')
        self.assertEqual(rig.kms.calls, [])

    def test_rsig5_reserve_job_identity_is_exact(self):
        self.assertEqual((CONTROLLER.RESERVE_WORKFLOW, CONTROLLER.RESERVE_JOB_KEY, CONTROLLER.RESERVE_JOB),
                         ('.github/workflows/release.yml', 'reserve', 'Reserve release tuple'))

        def attempt(receipt=None, jobs=None, run=None, message=None, code=None):
            rig = Rig()
            data = rig.reserve_version()
            if receipt:
                rig.upload_receipt(canon(dict(json.loads(data), **receipt)))
            if jobs:
                jobs(rig)
            if run:
                rig.w.runs[(DISPATCH_RUN, 1)].update(run)
            ctrl = rig.controller()
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            with self.subTest(message=message, receipt=receipt):
                self.refused(message, ctrl.open_session, rig.table)
                self.assertNotIn(rig.version_key(), rig.table.items)
                self.assertEqual(rig.kms.calls, [])
        not_this_run = 'signing outcome unknown: the reserve receipt is not from this run attempt'
        attempt({'run_id': '1'}, message=not_this_run)
        attempt({'run_attempt': '2'}, message=not_this_run)
        other = 'reservation conflict: the reserve receipt names another reservation'
        attempt({'tuple_sha256': digest(1)}, message=other)
        attempt({'reservation_object': digest(1)[:40]}, message=other)
        attempt({'commit': base.OTHER}, message=other)
        attempt({'version': '0.99.0'}, message=other)
        attempt({'extra': True}, message="evidence mismatch: unknown field 'extra'")
        attempt({'run_id': 1}, message='evidence mismatch: the reserve receipt is malformed')
        no_job = 'signing outcome unknown: the reserve job is not the one completed reserve job'
        # A spoofed job name, another job's id, a different result, a matrix and a second job.
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(name='Other job'), message=no_job)
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(name='reserve'), message=no_job)
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(name=CONTROLLER.RESERVE_JOB + ' (a)'), message=no_job)
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(conclusion='failure'), message=no_job)
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(status='in_progress', conclusion=None), message=no_job)
        attempt(jobs=lambda r: r.w.find_job(RESERVE_JOB_ID).update(head_sha=base.OTHER), message=no_job)
        attempt({'job_id': str(SIGN_JOB)}, message=no_job)
        attempt(jobs=lambda r: r.w.job(DISPATCH_RUN, 1, 9098, CONTROLLER.RESERVE_JOB), message=no_job)
        attempt(jobs=lambda r: r.w.job(DISPATCH_RUN, 1, 9098, CONTROLLER.RESERVE_JOB + ' (b)'), message=no_job)
        # The right name in another run or attempt grants nothing.
        rig = Rig()
        rig.reserve_version()
        rig.w.jobs[(DISPATCH_RUN, 1)] = [j for j in rig.w.jobs[(DISPATCH_RUN, 1)] if j['job_id'] != RESERVE_JOB_ID]
        rig.w.runs[(DISPATCH_RUN, 2)] = dict(rig.w.runs[(DISPATCH_RUN, 1)], run_attempt=2)
        rig.w.job(DISPATCH_RUN, 2, RESERVE_JOB_ID, CONTROLLER.RESERVE_JOB)
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        self.refused(no_job, ctrl.open_session, rig.table)
        # A winner that did not create the reservation, or no receipt, is uncertain history.
        rig = Rig()
        data = rig.reserve_version()
        for label, artifact, change in (('no receipt', None, None),
                                        ('winner, not creator', RECEIPT_ART, dict(json.loads(data), created=False))):
            if change:
                rig.upload_receipt(canon(change))
            ctrl = rig.controller(artifact=artifact, run_evidence=FakeEvidence())
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            with self.subTest(label):
                self.refused('signing outcome unknown: the reservation has no journal row and this holder did not '
                             'freshly create it', ctrl.open_session, rig.table)
            self.assertNotIn(rig.version_key(), rig.table.items)
        # The receipt artifact must come from this run and carry its attempt's name.
        for label, change in (('another run', lambda r: r.w.artifacts[RECEIPT_ART].update(run_id=1)),
                              ('another attempt', lambda r: r.w.artifacts[RECEIPT_ART].update(name='reserve-receipt-2')),
                              ('another source', lambda r: r.w.artifacts[RECEIPT_ART].update(head_sha=base.OTHER)),
                              ('expired', lambda r: r.w.artifacts[RECEIPT_ART].update(expired=True)),
                              ('no member', lambda r: r.w.members.pop((RECEIPT_ART, CONTROLLER.RESERVE_MEMBER)))):
            rig = Rig()
            rig.reserve_version()
            change(rig)
            ctrl = rig.controller()
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            with self.subTest(label):
                self.refused('signing outcome unknown: the reserve receipt is unavailable', ctrl.open_session, rig.table)
        # The workflow run itself is exact. A rerun's own receipt names its attempt; the first
        # attempt's receipt does not transfer to it.
        rig = Rig()
        rig.reserve_version()
        rig.add_job(DISPATCH_RUN, 2, SIGN_JOB + 1)
        rerun = rig.controller(rig.holder(DISPATCH_RUN, 2, SIGN_JOB + 1), artifact=RECEIPT_ART,
                               request=rig.w.request(dispatch_run_attempt=2))
        rerun.admit()
        rerun.issue_credentials(rig.provider())
        self.refused('signing outcome unknown: the reserve receipt is unavailable', rerun.open_session, rig.table)


class Refusals(Base):
    def test_rsig5_refusal_names_keep_the_controller_prefix(self):
        for code, reason in (('eligibility incomplete', 'a'), ('evidence mismatch', 'b'), ('reservation conflict', 'c'),
                             ('authority unavailable', 'd'), ('signer revoked', 'e'), ('policy rollback', 'f'),
                             ('input limit exceeded', 'g'), ('signing outcome unknown', 'h')):
            stream = io.StringIO()
            self.assertEqual(CONTROLLER.report(Refused(code, reason), stream), 1)
            self.assertEqual(stream.getvalue(), 'release controller: ' + code + ': ' + reason + '\n')
        with self.assertRaises(ValueError):
            Refused('it failed', 'x')
        # Eligibility's named refusals keep their names under the controller's type.
        lifted = CONTROLLER.lift(ELIGIBILITY.Refused('eligibility incomplete', 'x'))
        self.assertIsInstance(lifted, Refused)
        self.assertEqual(lifted.code, 'eligibility incomplete')
        stream = io.StringIO()
        self.assertEqual(CONTROLLER.report(ControllerError('plain'), stream), 1)
        self.assertEqual(stream.getvalue(), 'release controller: plain\n')
        # Each named refusal comes from a real path.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        rig.w.errors['commit'] = GITHUB.Uncertain('commit', 'timeout')
        self.refused_code('eligibility incomplete', ctrl.open_session, rig.table)
        rig = Rig()
        ctrl = rig.ready()
        self.refused('evidence mismatch: the adapter request version is not the qualified version', rig_reserve(ctrl, rig),
                     version='0.99.0')
        for key, value in (('repository', 'other/repo'), ('manifest_sha256', digest(1)), ('key_arn', base.NEW_KEY_ARN),
                           ('fingerprint', 'SHA256:' + 'B' * 43), ('first_machine_tag', 'v0.99.0')):
            with self.subTest(key=key):
                self.refused('evidence mismatch: the adapter request ' + key.replace('_', ' ')
                             + ' is not the qualified ' + key.replace('_', ' '), rig_reserve(ctrl, rig), **{key: value})
        # Nonsecret reasons are kept in the invocation's receipts.
        receipts = ctrl.refusals()
        self.assertEqual(receipts[0]['code'], 'evidence mismatch')
        self.assertEqual(receipts[0]['stage'], 'reserve')
        self.assertIn('version', receipts[0]['reason'])
        self.assertNotIn('0.99.0', repr(receipts))
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        rig.install(state='CLOSED')
        self.refused_code('authority unavailable', ctrl.admit)
        self.assertEqual(ctrl.refusals()[0],
                         {'stage': 'admit', 'code': 'authority unavailable', 'reason': 'admission is CLOSED'})
        self.assertEqual(json.loads(CONTROLLER.canonical_refusals(ctrl)), ctrl.refusals())

    def test_rsig5_journal_wire_matches_the_contract(self):
        rig = Rig()
        ctrl = rig.ready()
        self.assertEqual(rig.sign(ctrl), SIGNATURE)
        # Every write ran through the strict fake; the tuple keys are the frozen keys.
        for sk in ('ATTEMPT', 'SIGNATURE', 'COMPLETION'):
            self.assertIn((rig.tuple_pk(), sk), rig.table.items)
        self.assertEqual(rig.table.items[(rig.tuple_pk(), 'SIGNATURE')], SIGNATURE)
        completion = json.loads(rig.table.items[(rig.tuple_pk(), 'COMPLETION')])
        attempt = rig.table.items[(rig.tuple_pk(), 'ATTEMPT')]
        self.assertEqual(completion, {
            'schema': 1, 'repository_id': str(REPO_ID), 'version': rig.w.v, 'tuple_sha256': rig.tuple_sha(),
            'attempt_sha256': sha(attempt), 'signature_sha256': sha(SIGNATURE), 'manifest_sha256': rig.w.manifest,
            'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT,
            'verification_contract': 'rdy-release principal; rdy-release-v1 namespace; authentic exact manifest and '
                                     'reserved key',
            'run_id': str(DISPATCH_RUN), 'run_attempt': '1', 'job_id': str(SIGN_JOB), 'grant_id': ctrl.grant_id,
            'fence': json.loads(attempt)['fence']})
        self.assertEqual(rig.row()['phase'], 'COMPLETE')
        self.assertEqual(rig.row()['revision'], '3')
        # Journal bodies are canonical, and each write used a new private token.
        self.assertEqual(len(set(rig.table.tokens)), len(rig.table.tokens))
        for key, body in rig.table.items.items():
            if key[1] != 'SIGNATURE':
                self.assertEqual(canon(json.loads(body)), body, key)
        # Wire types are refused before use.
        for call, value in ((CONTROLLER.uint64, '0'), (CONTROLLER.uint64, '01'), (CONTROLLER.uint64, 1),
                            (CONTROLLER.uint64, str(2 ** 64)), (CONTROLLER.uint64, '+1'), (CONTROLLER.uint64, '1.0')):
            with self.assertRaises(ControllerError):
                call(value, 'a value')
        self.assertEqual(CONTROLLER.uint64(str(2 ** 64 - 1), 'a value'), str(2 ** 64 - 1))


def rig_reserve(ctrl, rig):
    def enter(**changes):
        with ctrl.reserve(**dict(rig.adapter_args(), **changes)):
            raise AssertionError('reserve yielded')
    return enter


class Repairs(Base):
    """The signer repairs. Each case keeps every other fact valid, so only the named fact refuses."""

    MOVED = 'authority unavailable: the evidence policy moved beyond a revocation in this epoch'
    HIGH = {'number': 2, 'state': 'open', 'tool': 'CodeQL', 'security_severity': 'high'}
    BUDGET = 'signing outcome unknown: the signing budget has passed'

    @staticmethod
    def advance(rig):
        """A new protected commit on main that carries every file forward."""
        old = rig.head()
        new = format(0xb000 + len(rig.w.commits), '040x')
        rig.w.commits[new] = (old,)
        for (path, commit), body in list(rig.w.files.items()):
            if commit == old:
                rig.w.files[(path, new)] = body
        rig.w.refs['heads/main'] = ('commit', new)
        return new

    def land(self, rig, change):
        """A protected commit on main whose evidence policy has `change` applied. P keeps its own."""
        commit = self.advance(rig)
        change(rig.w.ep)
        rig.w.publish_policy(commit)
        return commit

    def reserved(self, *baseline):
        """A reservation, with `baseline` changes in the epoch's policy at P and at main."""
        rig = Rig()
        for change in baseline:
            change(rig.w.ep)
        rig.w.publish_policy(P)
        rig.w.publish_policy(HEAD)
        rig.install()
        rig.reserve_version()
        return rig

    @staticmethod
    def authority(rig):
        release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        return CONTROLLER.check_authority(rig.git, rig.enrollment, release, lambda holder: False)

    @staticmethod
    def channel(**changes):
        return lambda ep: ep['approval_channel'].update(changes)

    # -- A: the evidence policy inside one epoch ------------------------------------------------

    def test_rsig12_a_added_coordinator_refuses_in_the_same_epoch(self):
        # A coordinator added on main only, with the admission record unchanged.
        rig = self.reserved()
        prior = copy.deepcopy(rig.admission())
        kept = rig.w.files[('release-keys/policy.json', P)]
        rig.w.ep['coordinators'].append({'actor_id': 812345, 'app_id': None, 'installation_id': None})
        rig.w.publish_policy(HEAD)
        self.refused(self.MOVED, rig.controller().admit)
        self.assertEqual(rig.admission(), prior)
        self.assertEqual(rig.w.files[('release-keys/policy.json', P)], kept)
        self.assertEqual((rig.kms.calls, rig.providers), ([], []))
        # The promotion path reads the same rule before anything else.
        self.refused(self.MOVED, CONTROLLER.check_promotion, rig.git, rig.enrollment, rig.request(), DISPATCH_RUN, 1, b'{}')

    def test_rsig12_a_every_other_policy_change_refuses(self):
        def coordinator_gone(ep):
            ep['coordinators'].pop(0)

        def coordinator_changed(ep):
            ep['coordinators'][1]['app_id'] = 99

        def archive(ep):
            ep['approval_channel']['archive']['table'] = 'rdy-other-table'

        def workflow(ep):
            ep['evidence_workflow']['sha'] = base.OTHER
        cases = [('coordinator removed', coordinator_gone), ('coordinator changed', coordinator_changed),
                 ('archive', archive), ('evidence workflow', workflow)]
        for key, value in (('founder_user_id', 99), ('workflow_id', 99), ('workflow_path', '.github/workflows/other.yml'),
                           ('implementation_sha', base.OTHER), ('workflow_file_sha256', digest(99)),
                           ('required_job_name', 'other'), ('environment_id', 99), ('environment_name', 'other')):
            cases.append((key, self.channel(**{key: value})))
        for label, change in cases:
            rig = self.reserved()
            release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
            change(rig.w.ep)
            rig.w.publish_policy(HEAD)
            with self.subTest(label):
                self.refused(self.MOVED, CONTROLLER.check_authority, rig.git, rig.enrollment, release, lambda h: False)
        # Re-enabling, losing a revocation and reordering need a new epoch.
        one, two = digest(61), digest(62)
        for label, start, change in (
                ('re-enabled', self.channel(enabled=False), self.channel(enabled=True)),
                ('revocation lost', self.channel(revoked_receipts=[one, two]), self.channel(revoked_receipts=[two])),
                ('revocations emptied', self.channel(revoked_receipts=[one]), self.channel(revoked_receipts=[])),
                ('swap', self.channel(revoked_receipts=[one]), self.channel(revoked_receipts=[two])),
                ('reorder', self.channel(revoked_receipts=[one, two]), self.channel(revoked_receipts=[two, one]))):
            rig = self.reserved(start)
            change(rig.w.ep)
            rig.w.publish_policy(HEAD)
            with self.subTest(label):
                self.refused(self.MOVED, self.authority, rig)
        # Any other change of the policy file itself keeps its text.
        for label, key, value in (('trust', 'records_sha256', digest(99)), ('role', 'role_arn', base.ROLE_ARN + '-other'),
                                  ('signer', 'signer_workflow_sha', base.OTHER)):
            rig = self.reserved()
            release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
            rig.w.policy[key] = value
            rig.w.publish_policy(HEAD)
            with self.subTest(label):
                self.refused('authority unavailable: the admission policy digest is not the protected policy',
                             CONTROLLER.check_authority, rig.git, rig.enrollment, release, lambda h: False)

    def test_rsig12_a_authority_reducing_changes_pass(self):
        one, two = digest(61), digest(62)
        for label, changes in (('identical', ()),
                               ('disabled', (self.channel(enabled=False),)),
                               ('revocation added', (self.channel(revoked_receipts=[one]),)),
                               ('one by one', (self.channel(revoked_receipts=[one]),
                                               self.channel(revoked_receipts=[one, two]),
                                               self.channel(enabled=False)))):
            rig = self.reserved()
            for change in changes:
                self.land(rig, change)
            with self.subTest(label):
                snapshot, _ = self.authority(rig)
                self.assertEqual(snapshot.commit, rig.head())
                rig.controller().admit()
        # A list that gains an entry may name the older entries anywhere.
        rig = self.reserved(self.channel(revoked_receipts=[one]))
        self.land(rig, self.channel(revoked_receipts=[two, one]))
        self.authority(rig)
        # A change that only reduces authority takes effect at once: the next unfinished act refuses on it.
        rig = Rig(base.first_machine_world())
        receipt = rig.w.add_card('release_1')
        rig.seed_archive(receipt)
        rig.install()
        rig.reserve_version()
        self.land(rig, self.channel(revoked_receipts=[receipt]))
        self.refused('not authorized: the release_1 card receipt is revoked', rig.controller().admit)

    def test_rsig12_a_a_lost_revocation_refuses_against_history(self):
        # Each end equals the epoch's first baseline. Only the protected history shows what happened.
        one = digest(61)
        for label, first, second in (('receipt', self.channel(revoked_receipts=[one]), self.channel(revoked_receipts=[])),
                                     ('channel', self.channel(enabled=False), self.channel(enabled=True))):
            rig = self.reserved()
            self.land(rig, first)
            self.land(rig, second)
            with self.subTest(label):
                self.assertEqual(rig.w.files[('release-keys/evidence-policy.json', rig.head())],
                                 rig.w.files[('release-keys/evidence-policy.json', P)])
                self.refused(self.MOVED, self.authority, rig)
        # The same holds for a later step, after an accepted reduction.
        rig = self.reserved()
        self.land(rig, self.channel(revoked_receipts=[one]))
        self.authority(rig)
        self.land(rig, self.channel(revoked_receipts=[]))
        self.refused(self.MOVED, self.authority, rig)

    def test_rsig12_a_incomplete_evidence_refuses(self):
        one = digest(61)
        unavailable = 'authority unavailable: the protected policy is unavailable'

        def prepared():
            rig = self.reserved()
            release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
            middle = self.land(rig, self.channel(revoked_receipts=[one]))
            self.land(rig, self.channel(revoked_receipts=[one, digest(62)]))
            return rig, release, middle

        def check(rig, release):
            return CONTROLLER.check_authority(rig.git, rig.enrollment, release, lambda h: False)

        ep_path, policy_path = 'release-keys/evidence-policy.json', 'release-keys/policy.json'
        # Both referenced bodies and their hashes.
        for label, commit, path, data, message in (
                ('baseline body gone', P, ep_path, None, unavailable),
                ('head body gone', 'head', ep_path, None, unavailable),
                ('baseline body malformed', P, ep_path, canon({'x': 1}), unavailable),
                ('middle body gone', 'middle', ep_path, None, unavailable),
                ('middle policy gone', 'middle', policy_path, None, unavailable)):
            rig, release, middle = prepared()
            key = (path, {'head': rig.head(), 'middle': middle}.get(commit, commit))
            if data is None:
                del rig.w.files[key]
            else:
                rig.w.files[key] = data
            with self.subTest(label):
                self.refused(message, check, rig, release)
        # A well-formed body that is not the one its policy names.
        for label, commit in (('baseline', P), ('head', None)):
            rig, release, _ = prepared()
            other = copy.deepcopy(rig.w.ep)
            other['coordinators'].append({'actor_id': 812345, 'app_id': None, 'installation_id': None})
            rig.w.files[(ep_path, commit or rig.head())] = canon(other)
            with self.subTest('forged ' + label):
                self.refused('authority unavailable: the evidence policy does not match its policy digest',
                             check, rig, release)
        # A failed history read, and a policy commit off main's first-parent chain.
        rig, release, _ = prepared()
        rig.w.errors['commit'] = GITHUB.Uncertain('commit', 'outcome unknown (HTTP 502)')
        self.refused(unavailable, check, rig, release)
        rig = self.reserved()
        release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        side = self.advance(rig)
        rig.w.commits[side] = (base.BASE, P)
        self.refused("authority unavailable: the policy commit is not on main's first-parent chain", check, rig, release)

    def test_rsig12_a_history_read_is_bounded(self):
        rig = self.reserved()
        release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        limit = CONTROLLER.POLICY_HISTORY_MAX
        self.assertEqual(limit, ELIGIBILITY.FIRST_PARENT_MAX)
        last = rig.head()
        for n in range(limit + 1):
            rig.w.commits[format(0xf00000 + n, '040x')] = (last,)
            last = format(0xf00000 + n, '040x')
        rig.w.put(last, ADMISSION, canon(rig.record()))
        rig.w.refs['heads/main'] = ('commit', last)
        for name in ('release-keys/policy.json', 'release-keys/evidence-policy.json'):
            rig.w.files[(name, last)] = rig.w.files[(name, P)]
        rig.w.calls = []
        self.refused('input limit exceeded: the protected policy history is longer than ' + str(limit) + ' commits',
                     CONTROLLER.check_authority, rig.git, rig.enrollment, release, lambda h: False)
        self.assertLessEqual(len([c for c in rig.w.calls if c[0] == 'commit']), limit + 1)
        # One invocation reads each older commit of the history once. Main's head is read every time.
        rig = self.reserved()
        release = ELIGIBILITY.preliminary_recovery(rig.request(), rig.git)
        self.land(rig, self.channel(enabled=False))
        self.land(rig, self.channel(revoked_receipts=[digest(61)]))
        ctrl = rig.controller()
        ctrl.admit()
        rig.w.calls = []
        ctrl._authority(ctrl.preliminary)
        self.assertEqual([c for c in rig.w.calls if c[0] == 'commit'], [])
        older = rig.w.commits[rig.head()][0]
        self.assertEqual([c for c in rig.w.calls if c[0] == 'contents' and c[2] == older], [])
        self.assertIn(('contents', 'release-keys/evidence-policy.json', rig.head()), rig.w.calls)
        # A later head adds one step to what was read.
        newest = self.land(rig, self.channel(revoked_receipts=[digest(61), digest(62)]))
        rig.w.calls = []
        ctrl._authority(ctrl.preliminary)
        self.assertEqual([c[1] for c in rig.w.calls if c[0] == 'commit'], [newest])

    # -- B: the present status of the signer ----------------------------------------------------

    def observed(self, rig=None):
        rig = rig or Rig()
        rig.reserve_version()
        rig.publish()
        self.advance(rig)
        rig.w.calls = []
        return rig, rig.controller(artifact=None, run_evidence=FakeEvidence())

    def test_rsig12_b_later_revocation_on_main_reads_true(self):
        rig, fresh = self.observed()
        original, calls = rig.git.release_trust, []

        def trust(version, commit, signing):
            calls.append((commit, signing))
            if signing and commit == rig.head():
                raise ControllerError('release trust: the key for ' + version + ' is revoked for new signing')
            return original(version, commit, signing)
        rig.git.release_trust = trust
        answer = fresh.recover(rig.tuple_bytes())
        self.assertIsInstance(answer, CONTROLLER.AlreadyPublishedMatching)
        self.assertIs(answer.signer_revoked, True)
        self.assertEqual(answer.signer_status, 'revoked')
        self.assertEqual([c for c in calls if c[1]], [(rig.head(), True)], 'present status reads current main')
        self.assertIn((P, False), calls, 'the original verification keeps the historical policy')
        self.assertEqual((rig.kms.calls, rig.providers, rig.table.writes, rig.git.created), ([], [], [], ['tag object', 'ref']))

    def test_rsig12_b_false_needs_a_valid_current_check(self):
        rig, fresh = self.observed()
        answer = fresh.recover(rig.tuple_bytes())
        self.assertIs(answer.signer_revoked, False)
        self.assertEqual(answer.signer_status, 'not revoked')
        self.assertIn(('release_trust', rig.w.v, rig.head(), True), rig.w.calls)
        self.assertNotIn(('release_trust', rig.w.v, P, True), rig.w.calls)
        # A current answer about another signer, version or commit proves nothing.
        for label, changes in (('fingerprint', {'fingerprint': 'SHA256:' + 'B' * 43}),
                               ('transition', {'transition_sha256': digest(77)}),
                               ('version', {'version': '0.44.0'}),
                               ('commit', {'policy_commit': P}),
                               ('extra', {'extra': 1})):
            rig, fresh = self.observed()
            original = rig.git.release_trust

            def trust(version, commit, signing, original=original, changes=changes):
                answer = original(version, commit, signing)
                return dict(answer, **changes) if signing else answer
            rig.git.release_trust = trust
            with self.subTest(label):
                found = fresh.recover(rig.tuple_bytes())
                self.assertIsInstance(found, CONTROLLER.AlreadyPublishedMatching)
                self.assertIsNone(found.signer_revoked)
                self.assertEqual(found.signer_status, 'unknown')

    def test_rsig12_b_unavailable_status_is_unknown(self):
        def trust_down(rig):
            original = rig.git.release_trust

            def trust(version, commit, signing):
                if signing:
                    raise ControllerError('current trust unavailable')
                return original(version, commit, signing)
            rig.git.release_trust = trust

        def gone(path):
            def change(rig):
                self.advance(rig)
                del rig.w.files[(path, rig.head())]
            return change

        def forged_revocation(rig):
            # A revocation answer from a snapshot whose records are not the ones its policy names.
            self.advance(rig)
            rig.w.files[('release-keys/records.txt', rig.head())] = b'other records\n'
            original = rig.git.release_trust

            def trust(version, commit, signing):
                if signing:
                    raise ControllerError('release trust: the key for ' + version + ' is revoked for new signing')
                return original(version, commit, signing)
            rig.git.release_trust = trust

        def moved(rig):
            original = rig.git.release_trust

            def trust(version, commit, signing):
                answer = original(version, commit, signing)
                if signing:
                    self.advance(rig)
                return answer
            rig.git.release_trust = trust

        def main_ref_gone(rig):
            original = rig.git.release_trust

            def trust(version, commit, signing):
                answer = original(version, commit, signing)
                if signing:
                    rig.w.refs.pop('heads/main')
                return answer
            rig.git.release_trust = trust
        for label, change in (('trust', trust_down), ('policy', gone('release-keys/policy.json')),
                              ('evidence', gone('release-keys/evidence-policy.json')),
                              ('records', gone('release-keys/records.txt')),
                              ('forged revocation', forged_revocation), ('moved', moved), ('ref', main_ref_gone)):
            rig, fresh = self.observed()
            verify = rig.published.verify_published

            # Main changes after the historical proof is read, so only the present status can see it.
            def proved(reconciliation, verify=verify, change=change, rig=rig):
                answer = verify(reconciliation)
                change(rig)
                return answer
            rig.published.verify_published = proved
            with self.subTest(label):
                answer = fresh.recover(rig.tuple_bytes())
                self.assertIsInstance(answer, CONTROLLER.AlreadyPublishedMatching)
                self.assertIsNone(answer.signer_revoked)
                self.assertEqual(answer.signer_status, 'unknown')
                self.assertEqual((rig.kms.calls, rig.providers, rig.table.writes), ([], [], []))
        # Unknown gives no authority: the terminal observation stays terminal and nothing follows it.
        self.refused('reservation conflict: v' + rig.w.v + ' is published; this observation is terminal',
                     fresh.issue_credentials, rig.provider())
        # Missing historical proof gives no matching result, whatever the present status reads.
        rig, fresh = self.observed()
        rig.published.answer = None
        trust_down(rig)
        with self.assertRaises(Refused) as caught:
            fresh.recover(rig.tuple_bytes())
        self.assertEqual(caught.exception.code, CONTROLLER.INCOMPLETE)
        self.assertIsNone(fresh.terminal)

    # -- C: the alert read before a new Sign ----------------------------------------------------

    def test_rsig12_c_alert_after_reservation_refuses_a_new_sign(self):
        rig = Rig()
        ctrl = rig.ready()
        evidence = rig.w.members[(base.EVID_ART, 'release-record.json')]
        reservation = rig.w.refs['tags/reserve/v' + rig.w.v]
        rig.w.alerts.append(dict(self.HIGH))
        rig.w.calls = []
        with ctrl.reserve(**rig.adapter_args()):
            self.refused('eligibility incomplete: 1 open high or critical code scanning alert', ctrl.before_sign,
                         rig.payload_digest())
        self.assertIn(('open_code_scanning_alerts', 'refs/heads/main'), rig.w.calls)
        self.assertEqual(rig.row()['phase'], 'RESERVED', 'the refused invocation wrote no attempt')
        self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
        self.assertIs(ctrl._attempted, False, 'its stop receipt can say before_attempt')
        self.assertEqual(rig.kms.signs, 0)
        self.assertEqual(ctrl.refusals()[-1]['stage'], 'attempt')
        # E and the reservation stay the originals, and the same version goes on once the alert closes.
        self.assertEqual(rig.w.members[(base.EVID_ART, 'release-record.json')], evidence)
        self.assertEqual(rig.w.refs['tags/reserve/v' + rig.w.v], reservation)
        rig.w.alerts.pop()
        with ctrl.reserve(**rig.adapter_args()):
            self.assertIsInstance(ctrl.before_sign(rig.payload_digest()), CONTROLLER.Permit)

    def test_rsig12_c_unavailable_alert_api_refuses_a_new_sign(self):
        for error, message in (
                (GITHUB.Unauthorized('code scanning alerts', 'not authorized (HTTP 403)', 403),
                 'eligibility incomplete: GitHub code scanning alerts: not authorized (HTTP 403)'),
                (GITHUB.Uncertain('code scanning alerts', 'outcome unknown (HTTP 502)'),
                 'eligibility incomplete: GitHub code scanning alerts: outcome unknown (HTTP 502)')):
            rig = Rig()
            ctrl = rig.ready()
            rig.w.errors['open_code_scanning_alerts'] = error
            with self.subTest(message), ctrl.reserve(**rig.adapter_args()):
                self.refused(message, ctrl.before_sign, rig.payload_digest())
            self.assertEqual(rig.row()['phase'], 'RESERVED')
            self.assertEqual(rig.kms.signs, 0)

    def test_rsig12_c_no_new_sign_means_no_alert_read(self):
        unavailable = GITHUB.Unauthorized('code scanning alerts', 'not authorized (HTTP 403)', 403)
        # A terminal historical observation.
        rig, fresh = self.observed()
        rig.w.alerts.append(dict(self.HIGH))
        rig.w.errors['open_code_scanning_alerts'] = unavailable
        self.assertIsInstance(fresh.recover(rig.tuple_bytes()), CONTROLLER.AlreadyPublishedMatching)
        self.assertNotIn('open_code_scanning_alerts', [c[0] for c in rig.w.calls])
        # A retained signature sends no Sign, so it needs no new alert read.
        rig = Rig()
        rig.sign(rig.ready())
        rig.w.alerts.append(dict(self.HIGH))
        rig.w.errors['open_code_scanning_alerts'] = unavailable
        rig.w.calls = []
        again = rig.session(rig.rerun())
        self.assertEqual(again.recover(rig.tuple_bytes()).signature, SIGNATURE)
        self.assertNotIn('open_code_scanning_alerts', [c[0] for c in rig.w.calls])
        self.assertEqual(rig.kms.signs, 1)

    # -- D: the one signing deadline ------------------------------------------------------------

    def dispatched(self):
        rig = Rig()
        ctrl = rig.ready()
        with ctrl.reserve(**rig.adapter_args()):
            ctrl.before_sign(rig.payload_digest()).consume()
        rig.kms.calls.append('sign')
        return rig, ctrl

    @staticmethod
    def slow(rig, ctrl, write, read):
        """Journal calls that take `write` and `read` seconds, each inside the timeout it was given."""
        calls, do_write, do_read = [], ctrl.journal.write, ctrl.journal.snapshot

        def slow_write(actions, timeout):
            calls.append(('write', timeout))
            rig.clock.value += write
            return do_write(actions, timeout)

        def slow_read(keys, timeout):
            calls.append(('read', timeout))
            rig.clock.value += read
            return do_read(keys, timeout)
        ctrl.journal.write, ctrl.journal.snapshot = slow_write, slow_read
        return calls

    def assert_recoverable(self, rig):
        """Durable output stays. A later attempt returns those bytes, with no second Sign."""
        self.assertEqual(rig.table.items[(rig.tuple_pk(), 'SIGNATURE')], SIGNATURE)
        self.assertEqual(rig.row()['phase'], 'COMPLETE')
        fresh = rig.controller(artifact=None, run_evidence=FakeEvidence())
        fresh.attach_journal(rig.table)
        self.assertEqual(fresh.recover(rig.tuple_bytes()).signature, SIGNATURE)
        self.assertEqual(rig.kms.signs, 1)

    def test_rsig12_d_success_past_the_deadline_is_unknown(self):
        # KMS 60, write 59, readback 59, each within the timeout it was given.
        rig, ctrl = self.dispatched()
        start = rig.clock.value
        rig.clock.value += 60
        calls = self.slow(rig, ctrl, 59, 59)
        self.refused(self.BUDGET, ctrl.retain_signature, SIGNATURE)
        self.assertEqual(rig.clock.value - start, 178)
        self.assertEqual(calls, [('write', 60), ('read', 1)], 'each operation has what is left, not what was left')
        self.assertIsNone(ctrl._retained)
        self.refused('authority unavailable: nothing was retained in this invocation', ctrl.authorization)
        self.assert_recoverable(rig)

    def test_rsig12_d_deadline_is_read_before_each_step(self):
        # The write used all of it: no readback is sent, and the write stays.
        rig, ctrl = self.dispatched()
        rig.clock.value += 60
        calls = self.slow(rig, ctrl, 60, 0)
        self.refused(self.BUDGET, ctrl.retain_signature, SIGNATURE)
        self.assertEqual(calls, [('write', 60)])
        self.assert_recoverable(rig)
        # The readback ends exactly at the deadline: not a success.
        rig, ctrl = self.dispatched()
        calls = self.slow(rig, ctrl, 60, 60)
        self.refused(self.BUDGET, ctrl.retain_signature, SIGNATURE)
        self.assertEqual(calls, [('write', 120), ('read', 60)])
        self.assert_recoverable(rig)
        # The verification used all of it: nothing is written, nothing is deleted, nothing is sent again.
        rig, ctrl = self.dispatched()
        calls = self.slow(rig, ctrl, 0, 0)
        verify = rig.verifier.verify

        def slow_verify(*args):
            rig.clock.value += CONTROLLER.SIGN_BUDGET
            return verify(*args)
        rig.verifier.verify = slow_verify
        self.refused(self.BUDGET, ctrl.retain_signature, SIGNATURE)
        self.assertEqual(calls, [])
        self.assertNotIn((rig.tuple_pk(), 'SIGNATURE'), rig.table.items)
        self.assertEqual(rig.row()['phase'], 'ATTEMPTED')
        self.assertEqual(rig.kms.signs, 1)
        # A retained signature keeps its success inside the budget.
        rig, ctrl = self.dispatched()
        rig.clock.value += 60
        calls = self.slow(rig, ctrl, 20, 20)
        self.assertEqual(ctrl.retain_signature(SIGNATURE), SIGNATURE)
        self.assertEqual(calls, [('write', 60), ('read', 40)])

    # -- E: the VERSION initializer -------------------------------------------------------------

    def test_rsig12_e_initializer_is_create_only_with_exact_readback(self):
        rig = Rig()
        rig.reserve_version()
        seen = []
        write = rig.table.transact_write_items

        def capture(actions, token, timeout):
            seen.extend(actions)
            return write(actions, token, timeout)
        rig.table.transact_write_items = capture
        rig.session()
        puts = [a['Put'] for a in seen if 'Put' in a and a['Put']['Item']['pk']['S'].startswith('VERSION#')]
        self.assertEqual(len(puts), 1)
        self.assertEqual(puts[0]['ConditionExpression'], 'attribute_not_exists(pk) AND attribute_not_exists(sk)')
        self.assertEqual((puts[0]['Item']['pk']['S'], puts[0]['Item']['sk']['S']), rig.version_key())
        self.assertEqual(rig.row()['phase'], 'RESERVED')
        self.assertEqual(rig.row()['revision'], '1')
        # A different row found by the readback is never accepted, and never replaced.
        rig = Rig()
        rig.reserve_version()
        ctrl = rig.controller()
        ctrl.admit()
        ctrl.issue_credentials(rig.provider())
        other = canon({'schema': 1, 'other': True})
        rig.table.hook = lambda: rig.table.items.__setitem__(rig.version_key(), other)
        rig.table.fail_next('VERSION#', 'unsent')
        self.refused('signing outcome unknown: the reservation row has no exact acknowledgement', ctrl.open_session, rig.table)
        self.assertEqual(rig.table.items[rig.version_key()], other)
        # A lost answer with nothing stored is no row.
        rig = Rig()
        rig.reserve_version()
        rig.table.fail_next('VERSION#', 'unsent')
        self.refused('signing outcome unknown: the reservation row has no exact acknowledgement', rig.session)
        self.assertNotIn(rig.version_key(), rig.table.items)

    def test_rsig12_e_old_reservation_gets_no_clean_row(self):
        none = 'signing outcome unknown: the reservation has no journal row and this holder did not freshly create it'
        rig = Rig()
        data = rig.reserve_version()
        self.assertIs(json.loads(data)['created'], True)
        # A later attempt's reserve job only finds the reservation. Its receipt says so.
        winner = canon(dict(json.loads(data), created=False, run_attempt='2'))
        rig.upload_receipt(winner, attempt=2)
        rig.add_job(DISPATCH_RUN, 2, 9095)
        job = rig.w.find_job(9095)
        job.update(name=CONTROLLER.RESERVE_JOB, status='completed', conclusion='success')
        self.refused(none, rig.session, rig.rerun(2, reserve_artifact_id=RECEIPT_ART))
        self.assertNotIn(rig.version_key(), rig.table.items)
        # Attempt 1's fresh receipt is not this attempt's history.
        rig = Rig()
        data = rig.reserve_version()
        rig.add_job(DISPATCH_RUN, 2, 9095)
        job = rig.w.find_job(9095)
        job.update(name=CONTROLLER.RESERVE_JOB, status='completed', conclusion='success')
        self.refused('signing outcome unknown: the reserve receipt is unavailable', rig.session,
                     rig.rerun(2, reserve_artifact_id=RECEIPT_ART))
        self.assertNotIn(rig.version_key(), rig.table.items)
        # A fresh receipt that a failed or other reserve job would have to vouch for.
        rig = Rig()
        rig.reserve_version()
        rig.w.find_job(RESERVE_JOB_ID).update(conclusion='failure')
        self.refused('signing outcome unknown: the reserve job is not the one completed reserve job', rig.session)
        self.assertNotIn(rig.version_key(), rig.table.items)

    def test_rsig12_e_a_row_is_never_a_sign_permit(self):
        # A clean row, an unproven prior invocation: no attempt and no Sign.
        rig = Rig()
        ctrl = rig.ready()
        self.assertEqual(rig.row()['phase'], 'RESERVED')
        rig.history.answer = CONTROLLER.History(complete=True, invocations=(
            CONTROLLER.Invocation(DISPATCH_RUN, 9, 9999, 'unknown'),))
        with ctrl.reserve(**rig.adapter_args()):
            self.refused('signing outcome unknown: a prior invocation is not proven to have stopped before its attempt',
                         ctrl.before_sign, rig.payload_digest())
        self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
        self.assertEqual(rig.kms.signs, 0)
        # A row alone leaves no permit: a process with the row and no grant has no attempt.
        rig = Rig()
        rig.ready()
        later = rig.controller(artifact=None, run_evidence=FakeEvidence())
        self.refused('authority unavailable: a KMS request needs the reservation context', later.before_sign,
                     rig.payload_digest())
        self.refused('authority unavailable: the signing session is not open',
                     lambda: later.reserve(**rig.adapter_args()).__enter__())
        self.assertEqual(rig.kms.signs, 0)

    # -- F: reserved H, key and fingerprint against current admission and running Enrollment ----

    @staticmethod
    def f_cases():
        other_key = base.NEW_KEY_ARN
        return (('admission H', lambda r, c: r.install(mirror=False, signer_workflow_sha=base.OTHER),
                 'authority unavailable: the admission record names another signer'),
                ('admission key', lambda r, c: r.install(mirror=False, key_arn=other_key),
                 'authority unavailable: the admission record names another key'),
                ('admission fingerprint', lambda r, c: r.install(mirror=False, fingerprint='SHA256:' + 'B' * 43),
                 'authority unavailable: the admission record names another key'),
                ('running H', lambda r, c: setattr(c, 'enrollment', dataclasses.replace(
                    c.enrollment, signer_workflow_sha=base.OTHER)),
                 'authority unavailable: the admission record names another signer'))

    def assert_unrewritten(self, rig, reservation, row):
        self.assertEqual(rig.w.refs['tags/reserve/v' + rig.w.v], reservation)
        self.assertEqual(rig.w.tags[reservation[1]]['message_bytes'], rig.tuple_bytes())
        self.assertEqual(rig.table.items.get(rig.version_key()), row)
        self.assertEqual(rig.kms.signs, 0)

    def test_rsig12_f_every_unfinished_signing_path_refuses_a_mismatch(self):
        held = []

        def admit(rig):
            return rig.controller(), lambda c: c.admit()

        def issue(rig):
            ctrl = rig.controller()
            ctrl.admit()
            return ctrl, lambda c: c.issue_credentials(rig.provider())

        def session(rig):
            ctrl = rig.controller()
            ctrl.admit()
            ctrl.issue_credentials(rig.provider())
            return ctrl, lambda c: c.open_session(rig.table)

        def enter(rig):
            ctrl = rig.session()
            return ctrl, lambda c: c.reserve(**rig.adapter_args()).__enter__()

        def attempt(rig):
            ctrl = rig.session()
            held.append(ctrl.reserve(**rig.adapter_args()))
            held[-1].__enter__()
            return ctrl, lambda c: c.before_sign(rig.payload_digest())
        for stage, prepare in (('admit', admit), ('issue', issue), ('session', session), ('reserve', enter),
                               ('attempt', attempt)):
            for label, change, message in self.f_cases():
                rig = Rig()
                rig.reserve_version()
                ctrl, step = prepare(rig)
                reservation = rig.w.refs['tags/reserve/v' + rig.w.v]
                row = rig.table.items.get(rig.version_key())
                change(rig, ctrl)
                with self.subTest(stage=stage, change=label):
                    self.refused(message, step, ctrl)
                    self.assert_unrewritten(rig, reservation, row)

    def test_rsig12_f_recovery_and_promotion_refuse_a_mismatch(self):
        for label, change, message in self.f_cases():
            # Recovery of retained bytes.
            rig = Rig()
            rig.sign(rig.ready())
            kept = rig.table.items[(rig.tuple_pk(), 'SIGNATURE')]
            fresh = rig.controller(artifact=None, run_evidence=FakeEvidence())
            fresh.attach_journal(rig.table)
            reservation = rig.w.refs['tags/reserve/v' + rig.w.v]
            change(rig, fresh)
            with self.subTest('recover', change=label):
                self.refused(message, fresh.recover, rig.tuple_bytes())
                self.assertEqual(rig.w.refs['tags/reserve/v' + rig.w.v], reservation)
                self.assertEqual(rig.table.items[(rig.tuple_pk(), 'SIGNATURE')], kept)
            # Promotion.
            rig = Rig()
            rig.reserve_version()
            holder = mock.Mock(enrollment=rig.enrollment)
            change(rig, holder)
            with self.subTest('promotion', change=label):
                self.refused(message, CONTROLLER.check_promotion, rig.git, holder.enrollment, rig.request(), DISPATCH_RUN, 1,
                             b'{}')

    def test_rsig12_f_candidate_input_cannot_select_h(self):
        rig = Rig()
        value = {f.name: getattr(rig.request(), f.name) for f in dataclasses.fields(ELIGIBILITY.Request)}
        value['approvals'] = []
        with self.assertRaises(ELIGIBILITY.Refused) as caught:
            ELIGIBILITY.parse_request(dict(value, signer_workflow_sha=base.OTHER))
        self.assertIn("unknown field 'signer_workflow_sha'", str(caught.exception))
        # E's candidate names no signer. T's H comes from the protected policy, and a tuple that names another
        # H is not the tuple of the authenticated facts.
        self.assertNotIn('signer_workflow_sha', rig.w.candidate)
        forged = rig.w.expected_tuple()
        forged['signer']['signer_workflow_sha'] = base.OTHER
        rig.w.reserve(forged)
        self.refused('evidence mismatch: the reservation tuple is not the tuple of the authenticated facts',
                     rig.controller().admit)


if __name__ == '__main__':
    unittest.main()
