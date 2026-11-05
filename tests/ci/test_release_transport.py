"""Frozen evidence schemas and the bounded GitHub transport, with fake transports only.

No test reaches the network, GitHub, AWS or a key. Every identity below is synthetic.
"""
import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/release' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # the release scripts import each other: one module, one error class
    spec.loader.exec_module(module)
    return module


CANDIDATE = load('candidate')
load('rehearse')  # evidence.py reuses its tutorial check; bind it to this candidate module
EVIDENCE = load('evidence')
GITHUB = load('github')
ControllerError = CANDIDATE.ControllerError

MAC = 'aarch64-apple-darwin'
LINUX = 'x86_64-unknown-linux-gnu'
V = '0.42.0'
S = '1' * 40
H = '2' * 40
P = '3' * 40
REPO = 'adrenickchrome/rdy-core'
REPO_ID = 1403751388
# Synthetic identities: never production values.
ACCOUNT = '111122223333'
KEY_ARN = 'arn:aws:kms:us-east-2:' + ACCOUNT + ':key/00000000-0000-4000-8000-000000000001'
ROLE_ARN = 'arn:aws:iam::' + ACCOUNT + ':role/rdy-test-signer'
FINGERPRINT = 'SHA256:' + 'A' * 43
FOUNDER = 4242
RUN_ID = 777001
ATTEMPT = 2
ACTIONS = 'https://github.com/' + REPO + '/actions/runs/'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canon(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False) + '\n').encode('utf-8')


def digest(n):
    return format(n, '064x')


def job(run_id, job_id, attempt=1):
    return {'run_id': run_id, 'run_attempt': attempt, 'job_id': job_id,
            'job_url': ACTIONS + str(run_id) + '/job/' + str(job_id), 'conclusion': 'success'}


def archive(target):
    return {'name': 'rdy-' + V + '-preview-' + target + '.tar.gz', 'sha256': digest(10 + len(target)),
            'size': 1000 + len(target)}


def manifest_sha(files):
    return sha(''.join(f['sha256'] + '  ' + f['name'] + '\n' for f in files).encode())


ARCHIVES = [archive(MAC), archive(LINUX)]
MANIFEST = manifest_sha(ARCHIVES)
CHECKS = [{'name': n, 'passed': True} for n in ('signature', 'checksums', 'install', 'version', 'daemon', 'mcp')]
SECURITY = sorted(['CodeQL (actions)', 'CodeQL (rust)', 'Core CI', 'Core security',
                   'Dependency, secret and workflow policy', 'Fast gate (Linux x86-64)'])
LANDING = 'f' * 64


def scope():
    return {'repository': REPO, 'version': V, 'commit': S, 'design_landing': LANDING, 'manifest_sha256': MANIFEST,
            'artifacts': copy.deepcopy(ARCHIVES), 'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT,
            'first_machine_tag': 'v' + V, 'transition_sha256': digest(7), 'targets': [MAC, LINUX]}


def release_record():
    targets = []
    for n, (target, file) in enumerate(zip((MAC, LINUX), ARCHIVES)):
        base = 100 + 10 * n
        targets.append({'target': target, 'archive': copy.deepcopy(file), 'build': job(RUN_ID, base + 1),
                        'package': job(RUN_ID, base + 2),
                        'fast_gate': {'job': job(RUN_ID, base + 3), 'passed': 900, 'failed': 0, 'ignored': 3,
                                      'prerequisite_skipped': 0},
                        'install': {'job': job(RUN_ID, base + 4), 'report_sha256': digest(base),
                                    'checks': copy.deepcopy(CHECKS), 'skipped': 0}})
    tutorials = [{'target': t, 'agent': 'claude-code', 'model': 'claude-sonnet-test', 'effort': 'medium',
                  'session': 'session-' + t, 'tutorial_revision': 'rev-1',
                  'steps': [{'id': 'install', 'result': 'pass'}, {'id': 'first-use', 'result': 'pass'}],
                  'archive_sha256': f['sha256'], 'manifest_sha256': MANIFEST, 'result': 'pass'}
                 for t, f in zip((MAC, LINUX), ARCHIVES)]
    return {
        'schema': 1, 'repository': REPO, 'version': V, 'commit': S, 'design_landing': LANDING,
        'source': {'notes_sha256': digest(1), 'guide_sha256': digest(2), 'source_json_sha256': digest(3),
                   'export_receipt_sha256': digest(4)},
        'candidate': {'ref': 'refs/tags/candidate/v' + V, 'run_id': RUN_ID, 'run_attempt': 1,
                      'artifact': {'id': 55, 'name': 'rdy-candidate-a1', 'digest': 'sha256:' + digest(5),
                                   'size': 4096},
                      'manifest_sha256': MANIFEST, 'artifacts': copy.deepcopy(ARCHIVES)},
        'targets': targets,
        'security': {'jobs': [{'name': name, 'job': job(880000 + i, 990000 + i)} for i, name in enumerate(SECURITY)],
                     'codeql_open_high_critical': 0,
                     'advisory_fetch': {'tool': 'cargo-deny 0.18.3', 'fetched_at': '2026-11-05T10:00:00Z',
                                        'log_sha256': digest(6), 'result': 'clean'}},
        'audit': {'reference': 'audit/rdy-' + V, 'sha256': digest(8)},
        'tutorials': tutorials,
        'approval_basis': {'mode': 'standing_preview', 'decision': 'D-PZKAP', 'policy_commit': P, 'scope': scope(),
                           'cards': []},
    }


def tuple_record():
    return {'schema': 1, 'repository': REPO, 'version': V, 'commit': S, 'release_evidence_sha256': digest(9),
            'manifest_sha256': MANIFEST, 'artifacts': copy.deepcopy(ARCHIVES),
            'signer': {'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT, 'first_machine_tag': 'v' + V,
                       'transition_sha256': digest(7), 'policy_commit': P, 'signer_workflow_sha': H}}


def policy():
    return {'schema': 1, 'repository': REPO, 'key_arn': KEY_ARN, 'role_arn': ROLE_ARN, 'fingerprint': FINGERPRINT,
            'first_machine_tag': 'v' + V, 'targets': [MAC, LINUX], 'records_sha256': digest(11),
            'evidence_policy_sha256': digest(12), 'signer_workflow_sha': H}


def channel_policy():
    return {'schema': 1, 'repository': REPO, 'repository_id': REPO_ID, 'founder_user_id': FOUNDER,
            'workflow_id': 31, 'workflow_path': '.github/workflows/release-approval.yml', 'implementation_sha': H,
            'workflow_file_sha256': digest(13), 'dispatch_ref': 'refs/heads/main',
            'required_job_name': 'accept-founder-approval', 'environment_id': 41,
            'environment_name': 'release-exception-approval', 'prevent_self_review': False,
            'can_admins_bypass': False,
            'archive': {'provider': 'dynamodb', 'account_id': ACCOUNT, 'region': 'us-east-2',
                        'table': 'rdy-test-release-control', 'pk_prefix': 'APPROVAL#', 'sk': 'RECORD'},
            'enabled': True, 'revoked_receipts': []}


def evidence_policy():
    return {'schema': 1, 'repository_id': REPO_ID,
            'coordinators': [{'actor_id': 5001, 'app_id': None, 'installation_id': None},
                             {'actor_id': 5002, 'app_id': 61, 'installation_id': 71}],
            'evidence_workflow': {'id': 81, 'path': '.github/workflows/release-evidence.yml', 'sha': H},
            'release_workflow_path': '.github/workflows/release.yml', 'approval_channel': channel_policy()}


def source():
    return {'schema': 1, 'version': V, 'design_landing': LANDING, 'notes_sha256': digest(1),
            'guide_sha256': digest(2)}


def request(kind='key_change'):
    full = scope()
    scopes = {
        'release_1': full,
        'key_change': {k: full[k] for k in ('repository', 'key_arn', 'fingerprint', 'first_machine_tag',
                                             'transition_sha256', 'targets')},
        'revocation': {'repository': REPO, 'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT,
                       'revocation_sha256': digest(14), 'last_good_tag': None},
        'new_platform': {'repository': REPO, 'targets': [MAC, LINUX]},
    }
    return {'schema': 1, 'decision': 'D-PZKAP', 'policy_commit': P, 'kind': kind, 'scope': scopes[kind]}


SOURCE_KINDS = ('artifact', 'implementation_source', 'job', 'protected_policy', 'repository', 'request_archive',
                'review_history', 'run_attempt', 'workflow_source')


def projection(request_bytes):
    return {'schema': 1, 'repository_id': REPO_ID, 'head_repository_id': REPO_ID, 'workflow_id': 31,
            'workflow_path': '.github/workflows/release-approval.yml', 'implementation_sha': H,
            'workflow_file_sha256': digest(13), 'run_id': 6001, 'run_attempt': 1, 'run_head_sha': '4' * 40,
            'ref': 'refs/heads/main', 'event': 'workflow_dispatch', 'status': 'completed', 'conclusion': 'success',
            'actor_id': FOUNDER, 'triggering_actor_id': FOUNDER,
            'job': {'id': 6101, 'name': 'accept-founder-approval', 'run_id': 6001, 'status': 'completed',
                    'conclusion': 'success'},
            'artifact': {'id': 6201, 'name': 'approval-request-6001-1', 'sha256': digest(15),
                         'workflow_run_id': 6001, 'repository_id': REPO_ID, 'head_repository_id': REPO_ID,
                         'head_sha': '4' * 40, 'expired': False, 'request_sha256': sha(request_bytes)},
            'review': {'state': 'approved', 'comment': 'APPROVE sha256:' + sha(request_bytes), 'user_id': FOUNDER,
                       'user_type': 'User', 'environment_id': 41, 'environment_name': 'release-exception-approval'},
            'source_responses': [{'kind': k, 'sha256': digest(20 + i), 'bytes': 100 + i}
                                 for i, k in enumerate(SOURCE_KINDS)]}


def receipt(request_value, request_bytes, projection_bytes):
    proj = json.loads(projection_bytes)
    return {'schema': 1, 'decision': 'D-PZKAP', 'kind': request_value['kind'], 'policy_commit': P,
            'workflow_scope_digest': sha(request_bytes), 'scope': copy.deepcopy(request_value['scope']),
            'issuer': {'repository_id': REPO_ID, 'workflow_id': 31,
                       'workflow_path': '.github/workflows/release-approval.yml', 'implementation_sha': H,
                       'run_id': 6001, 'run_attempt': 1, 'run_head_sha': '4' * 40, 'ref': 'refs/heads/main',
                       'actor_id': FOUNDER, 'triggering_actor_id': FOUNDER, 'job_id': 6101,
                       'job_name': 'accept-founder-approval', 'artifact_id': 6201, 'artifact_sha256': digest(15),
                       'reviewer_id': FOUNDER, 'environment_id': 41,
                       'environment_name': 'release-exception-approval',
                       'review_comment_sha256': sha(proj['review']['comment'].encode()),
                       'provider_evidence_sha256': sha(canon({k: v for k, v in proj.items()
                                                              if k != 'source_responses'}))}}


def envelope(kind='key_change'):
    req = request(kind)
    req_bytes = canon(req)
    proj_bytes = canon(projection(req_bytes))
    rec_bytes = canon(receipt(req, req_bytes, proj_bytes))
    acceptance = {'schema': 1, 'receipt_sha256': sha(rec_bytes), 'request_sha256': sha(req_bytes),
                  'provider_projection_sha256': sha(proj_bytes), 'signer_workflow_sha': H, 'run_id': 7001,
                  'run_attempt': 1, 'job_id': 7101, 'policy_commit': P, 'policy_sha256': digest(16),
                  'authority_sha256': digest(17), 'table_id': '01234567-89ab-4cde-8f01-23456789abcd',
                  'genesis_sha256': digest(18), 'accepted_at': '2026-11-05T10:00:00.123Z'}
    return {'schema': 1, 'request_body': req_bytes.decode(), 'receipt_body': rec_bytes.decode(),
            'provider_projection_body': proj_bytes.decode(), 'acceptance': acceptance}


def card(receipt_sha, kind='key_change'):
    return {'kind': kind, 'sha256': receipt_sha,
            'reference': 'dynamodb://' + ACCOUNT + '/us-east-2/rdy-test-release-control/APPROVAL%23'
                         + receipt_sha + '/RECORD'}


PARSERS = {
    'release record': (EVIDENCE.parse_release_record, release_record),
    'tuple': (EVIDENCE.parse_tuple, tuple_record),
    'policy': (EVIDENCE.parse_policy, policy),
    'evidence policy': (EVIDENCE.parse_evidence_policy, evidence_policy),
    'release source': (EVIDENCE.parse_source, source),
    'approval request': (EVIDENCE.parse_approval_request, request),
    'archive envelope': (EVIDENCE.parse_archive_envelope, envelope),
}


# ---------------------------------------------------------------------------------------------
# Fake transport
# ---------------------------------------------------------------------------------------------

class Reply(object):
    def __init__(self, status, value=None, body=None, headers=None):
        self.status = status
        self.headers = dict(headers or {})
        if body is None:
            body = b'' if value is None else json.dumps(value).encode()
        self.body = body


class FakeTransport(object):
    """Answers each (method, path) from a queue; a queued exception is raised. Records every call."""

    def __init__(self, routes=None, downloads=None):
        self.routes = {}
        self.calls = []
        self.downloads = dict(downloads or {})
        for key, replies in (routes or {}).items():
            self.add(key, *replies)

    def add(self, key, *replies):
        self.routes.setdefault(key, []).extend(replies)

    def request(self, method, path, body=None, limit=None):
        self.calls.append((method, path, body))
        queue = self.routes.get((method, path))
        if not queue:
            return Reply(404, {'message': 'Not Found'})
        reply = queue.pop(0) if len(queue) > 1 else queue[0]
        if isinstance(reply, BaseException):
            raise reply
        if limit is not None and len(reply.body) > limit:
            return GITHUB.Response(reply.status, reply.headers, reply.body[:limit + 1])
        return GITHUB.Response(reply.status, reply.headers, reply.body)

    def download(self, url, sink, limit):
        self.calls.append(('DOWNLOAD', url, None))
        data = self.downloads[url]
        if isinstance(data, BaseException):
            raise data
        sink.write(data[:limit + 1])
        return 200


def repo_reply(repo_id=REPO_ID, name=REPO):
    return Reply(200, {'id': repo_id, 'full_name': name, 'fork': False})


def run_value(run_id=RUN_ID, attempt=ATTEMPT, **changes):
    value = {'id': run_id, 'run_attempt': attempt, 'workflow_id': 81, 'path': '.github/workflows/release.yml',
             'head_sha': S, 'head_branch': 'candidate/v' + V, 'event': 'workflow_dispatch', 'status': 'completed',
             'conclusion': 'success', 'actor': {'id': 5001}, 'triggering_actor': {'id': 5002},
             'repository': {'id': REPO_ID}, 'head_repository': {'id': REPO_ID},
             'html_url': ACTIONS + str(run_id), 'name': 'release'}
    value.update(changes)
    return value


def job_value(job_id, run_id=RUN_ID, attempt=ATTEMPT, name='build-mac'):
    return {'id': job_id, 'run_id': run_id, 'run_attempt': attempt, 'name': name, 'status': 'completed',
            'conclusion': 'success', 'html_url': ACTIONS + str(run_id) + '/job/' + str(job_id), 'head_sha': S}


def artifact_value(artifact_id=55, run_id=RUN_ID, data=b'zip-bytes', **changes):
    value = {'id': artifact_id, 'name': 'rdy-candidate-a2', 'size_in_bytes': len(data),
             'digest': 'sha256:' + sha(data), 'expired': False,
             'workflow_run': {'id': run_id, 'repository_id': REPO_ID, 'head_repository_id': REPO_ID,
                              'head_branch': 'candidate/v' + V, 'head_sha': S}}
    value.update(changes)
    return value


API = '/repos/' + REPO


def NO_PAUSE(_attempt):
    return None


REPO_PATH = ('GET', API)


def client(routes=None, downloads=None):
    transport = FakeTransport(routes, downloads)
    transport.add(REPO_PATH, repo_reply())
    return GITHUB.GitHub(transport, NO_PAUSE), transport


def calls_to(transport, method, path):
    return [c for c in transport.calls if c[0] == method and c[1] == path]


class Schemas(unittest.TestCase):
    def assertRefuses(self, parse, data, message):
        with self.assertRaises(ControllerError) as caught:
            parse(data)
        self.assertEqual(str(caught.exception), message)

    def test_rsig5_schemas_reject_unknown_duplicate_and_untyped_fields(self):
        # Every frozen record parses from its canonical bytes and is identified by the hash of those bytes.
        for kind, (parse, make) in PARSERS.items():
            with self.subTest(kind=kind):
                data = canon(make())
                record = parse(data)
                self.assertEqual(record.sha256, sha(data))
                self.assertEqual(record.size, len(data))
                self.assertEqual(record.value, make())
        for kind in ('release_1', 'key_change', 'revocation', 'new_platform'):
            with self.subTest(request=kind):
                self.assertEqual(EVIDENCE.parse_approval_request(canon(request(kind))).value['kind'], kind)
                self.assertEqual(EVIDENCE.parse_archive_envelope(canon(envelope(kind))).receipt.value['kind'], kind)

        self.assertEqual(EVIDENCE.tuple_bytes(tuple_record()), canon(tuple_record()))
        e = release_record()
        e['candidate']['artifact']['name'] = 'rdy-candidate-a2'
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), 'the candidate artifact is not rdy-candidate-a1')

        # Unknown fields at every depth refuse, with the nearest frozen name.
        e = release_record()
        e['candidate']['artifact']['digset'] = e['candidate']['artifact'].pop('digest')
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), "unknown field 'digset'; did you mean 'digest'?")
        e = release_record()
        e['targets'][0]['install']['job']['job_urll'] = 'x'
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e),
                           "unknown field 'job_urll'; did you mean 'job_url'?")
        e = release_record()
        e['extra'] = 1
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), "unknown field 'extra'")
        t = tuple_record()
        t['signer']['policy_comit'] = t['signer'].pop('policy_commit')
        self.assertRefuses(EVIDENCE.parse_tuple, canon(t),
                           "unknown field 'policy_comit'; did you mean 'policy_commit'?")
        ep = evidence_policy()
        ep['approval_channel']['archive']['bucket'] = 'x'
        self.assertRefuses(EVIDENCE.parse_evidence_policy, canon(ep), "unknown field 'bucket'")
        # A scope takes exactly its kind's fields: a key card cannot carry a version.
        r = request('key_change')
        r['scope']['version'] = V
        self.assertRefuses(EVIDENCE.parse_approval_request, canon(r), "unknown field 'version'")
        r = request('new_platform')
        del r['scope']['targets']
        self.assertRefuses(EVIDENCE.parse_approval_request, canon(r), "missing field 'targets'")
        e = release_record()
        del e['approval_basis']['scope']['targets']
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), "missing field 'targets'")

        # Duplicate keys refuse, even when both values agree.
        data = canon(policy()).replace(b'"schema":1', b'"schema":1,"schema":1')
        self.assertRefuses(EVIDENCE.parse_policy, data, "duplicate field 'schema'")

        # No float, no boolean for an integer, no string for an integer, no integer for a boolean.
        for change, message in (
                (lambda v: v.update(schema=True), 'the release record schema is not 1'),
                (lambda v: v['candidate'].update(run_id=True), 'the candidate run id is not a positive integer'),
                (lambda v: v['candidate'].update(run_id='777001'), 'the candidate run id is not a positive integer'),
                (lambda v: v['targets'][0]['fast_gate'].update(failed=False),
                 'the aarch64-apple-darwin fast gate failed count is not 0'),
                (lambda v: v['targets'][1]['install']['checks'][0].update(passed=1),
                 'the x86_64-unknown-linux-gnu install checks are not the six passed checks'),
                (lambda v: v['security'].update(codeql_open_high_critical=False),
                 'the open high or critical CodeQL alert count is not 0'),
                (lambda v: v['targets'][0]['archive'].update(size='1019'),
                 'the aarch64-apple-darwin archive is malformed'),
        ):
            value = release_record()
            change(value)
            self.assertRefuses(EVIDENCE.parse_release_record, canon(value), message)
        floats = canon(release_record()).replace(b'"passed":900', b'"passed":900.0')
        self.assertRefuses(EVIDENCE.parse_release_record, floats, 'evidence holds a float')
        cp = evidence_policy()
        cp['approval_channel']['enabled'] = 1
        self.assertRefuses(EVIDENCE.parse_evidence_policy, canon(cp), 'the approval channel enabled flag is not a boolean')
        cp = evidence_policy()
        cp['coordinators'][1]['installation_id'] = None
        self.assertRefuses(EVIDENCE.parse_evidence_policy, canon(cp), 'a coordinator is malformed')

        # Only canonical bytes: sorted keys, compact separators, one final LF.
        good = canon(tuple_record())
        unsorted = (json.dumps(tuple_record(), separators=(',', ':')) + '\n').encode()
        for data in (good[:-1], good + b'\n', json.dumps(tuple_record(), indent=1, sort_keys=True).encode() + b'\n',
                     unsorted, good.replace(b'"', b'"', 1).replace(b'{', b'{ ', 1)):
            self.assertRefuses(EVIDENCE.parse_tuple, data, 'the tuple is not canonical JSON')

        # Limits refuse before parsing; a limit error is a refusal, never dropped evidence.
        self.assertRefuses(EVIDENCE.parse_release_record, b' ' * (1024 * 1024 + 1),
                           'the release record is larger than 1048576 bytes')
        self.assertRefuses(EVIDENCE.parse_policy, b'{' * (64 * 1024 + 1), 'the policy is larger than 65536 bytes')
        self.assertRefuses(EVIDENCE.parse_approval_request, b' ' * 60001,
                           'the approval request is larger than 60000 bytes')
        self.assertRefuses(EVIDENCE.parse_archive_envelope, b' ' * 65537,
                           'the archive envelope is larger than 65536 bytes')
        self.assertRefuses(EVIDENCE.parse_release_record, b'[' * 200000, 'evidence is not JSON')
        self.assertRefuses(EVIDENCE.parse_tuple, b'\xff\n', 'evidence is not UTF-8')
        cp = evidence_policy()
        cp['coordinators'] = [{'actor_id': 6000 + i, 'app_id': None, 'installation_id': None} for i in range(17)]
        self.assertRefuses(EVIDENCE.parse_evidence_policy, canon(cp), 'the coordinators are not 1 to 16 entries')
        e = release_record()
        e['tutorials'][0]['steps'] = [{'id': 's' + str(i), 'result': 'pass'} for i in range(257)]
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e),
                           'the aarch64-apple-darwin tutorial steps are not 1 to 256 entries')

        # Exact identities: hex case, versions, ordering and the fields E repeats.
        for change, message in (
                (lambda v: v.update(commit='A' * 40), 'the commit is not a 40-hex Git SHA'),
                (lambda v: v.update(version='0.42.1'), 'the version is not a 0.N.0 preview'),
                (lambda v: v['candidate'].update(ref='refs/tags/candidate/v0.43.0'),
                 'the candidate ref is not refs/tags/candidate/v0.42.0'),
                (lambda v: v['candidate']['artifact'].update(digest=digest(5)),
                 'the candidate artifact digest is not sha256:<64 hex>'),
                (lambda v: v.update(targets=list(reversed(v['targets']))),
                 'the targets are not exactly the release targets in order'),
                (lambda v: v['candidate'].update(artifacts=list(reversed(v['candidate']['artifacts']))),
                 'the candidate artifacts are not exactly the two archives in name order'),
                (lambda v: v['candidate'].update(manifest_sha256=digest(99)),
                 'the candidate manifest digest does not match its artifacts'),
                (lambda v: v['targets'][0]['build'].update(job_url='https://example.invalid/x'),
                 'the aarch64-apple-darwin build job URL is not this run in rdy-core'),
                (lambda v: v['security']['jobs'].pop(), 'the security jobs are not exactly the six required jobs'),
                (lambda v: v['security']['advisory_fetch'].update(fetched_at='2026-11-05 10:00:00'),
                 'the advisory fetch time is not UTC YYYY-MM-DDTHH:MM:SSZ'),
                (lambda v: v['approval_basis']['scope'].update(commit='9' * 40),
                 'the approval scope commit does not match the release record'),
                (lambda v: v['approval_basis'].update(mode='founder_card'),
                 'the founder_card approval basis has no card'),
                (lambda v: v['approval_basis'].update(cards=[card(digest(30))]),
                 'the standing_preview approval basis holds a card'),
                (lambda v: v['approval_basis'].update(decision='D-NFJ3D'),
                 'the approval decision is not D-PZKAP'),
                (lambda v: v['tutorials'][1].update(archive_sha256=digest(98)),
                 'the x86_64-unknown-linux-gnu tutorial did not use its candidate archive'),
        ):
            value = release_record()
            change(value)
            self.assertRefuses(EVIDENCE.parse_release_record, canon(value), message)
        t = tuple_record()
        t['artifacts'][0]['name'] = '../rdy.tar.gz'
        self.assertRefuses(EVIDENCE.parse_tuple, canon(t), 'a tuple artifact is malformed')
        t = tuple_record()
        t['signer']['fingerprint'] = 'SHA256:short'
        self.assertRefuses(EVIDENCE.parse_tuple, canon(t), 'the signer fingerprint is not SHA256:<43 base64>')
        p = policy()
        p['targets'] = [LINUX, MAC]
        self.assertRefuses(EVIDENCE.parse_policy, canon(p), 'the policy targets are not sorted and unique')

        # Cards: the reference names the backend grammar and its digest; a different digest refuses.
        e = release_record()
        e['approval_basis'].update(mode='founder_card', cards=[card(digest(30))])
        self.assertEqual(EVIDENCE.parse_release_record(canon(e)).value['approval_basis']['cards'][0]['sha256'],
                         digest(30))
        bad = card(digest(30))
        bad['sha256'] = digest(31)
        e['approval_basis']['cards'] = [bad]
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), 'a card reference does not name its digest')
        bad = card(digest(30))
        bad['reference'] = 'https://example.invalid/APPROVAL%23' + digest(30)
        e['approval_basis']['cards'] = [bad]
        self.assertRefuses(EVIDENCE.parse_release_record, canon(e), 'a card reference is not an approval record')
        self.assertEqual(EVIDENCE.card_receipt_sha256(card(digest(30)), channel_policy()), digest(30))
        other = channel_policy()
        other['archive']['table'] = 'another-table'
        with self.assertRaises(ControllerError) as caught:
            EVIDENCE.card_receipt_sha256(card(digest(30)), other)
        self.assertEqual(str(caught.exception), 'a card reference is not in the pinned approval archive')

        # The archive envelope: one bounded record whose three bodies join by hash.
        env = envelope()
        record = EVIDENCE.parse_archive_envelope(canon(env))
        self.assertEqual(record.receipt.sha256, env['acceptance']['receipt_sha256'])
        self.assertEqual(record.request.sha256, record.receipt.value['workflow_scope_digest'])
        for change, message in (
                (lambda v: v['acceptance'].update(receipt_sha256=digest(40)),
                 'the archive acceptance does not bind its receipt'),
                (lambda v: v.update(request_body=v['request_body'].replace(P, '5' * 40)),
                 'the approval receipt does not bind its request'),
                (lambda v: v.update(request_body=v['request_body'][:-1]), 'the approval request is not canonical JSON'),
                (lambda v: v['acceptance'].update(accepted_at='yesterday'),
                 'the archive acceptance time is not UTC'),
        ):
            value = envelope()
            change(value)
            self.assertRefuses(EVIDENCE.parse_archive_envelope, canon(value), message)
        # The issuer maps back to the projection field by field.
        value = envelope()
        rec = json.loads(value['receipt_body'])
        rec['issuer']['job_id'] = 6102
        value['receipt_body'] = canon(rec).decode()
        value['acceptance']['receipt_sha256'] = sha(canon(rec))
        self.assertRefuses(EVIDENCE.parse_archive_envelope, canon(value),
                           'the approval issuer job id does not match its provider evidence')
        value = envelope()
        proj = json.loads(value['provider_projection_body'])
        proj['source_responses'].reverse()
        value['provider_projection_body'] = canon(proj).decode()
        self.assertRefuses(EVIDENCE.parse_archive_envelope, canon(value),
                           'the provider evidence source responses are not the nine kinds in order')
        # The receipt binds the approval identity, not the recorded response digests; the acceptance
        # binds the exact projection bytes that were archived.
        value = envelope()
        proj = json.loads(value['provider_projection_body'])
        proj['source_responses'][4]['sha256'] = digest(41)
        value['provider_projection_body'] = canon(proj).decode()
        self.assertRefuses(EVIDENCE.parse_archive_envelope, canon(value),
                           'the approval receipt does not bind its provider evidence')
        value['acceptance']['provider_projection_sha256'] = sha(canon(proj))
        self.assertEqual(EVIDENCE.parse_archive_envelope(canon(value)).receipt.sha256,
                         envelope()['acceptance']['receipt_sha256'])
        self.assertEqual(EVIDENCE.provider_evidence_sha256(proj),
                         EVIDENCE.provider_evidence_sha256(json.loads(envelope()['provider_projection_body'])))
        # Escaping inside the envelope still counts towards its 64 KiB bound.
        value = envelope()
        value['request_body'] = value['request_body'] + '\u0001' * 11000
        self.assertRefuses(EVIDENCE.parse_archive_envelope, canon(value),
                           'the archive envelope is larger than 65536 bytes')
        # Parsing grants no authority: a parsed receipt carries no approval flag.
        self.assertFalse(hasattr(record, 'approved'))


class Transport(unittest.TestCase):
    def assertRefuses(self, call, kind, message):
        with self.assertRaises(kind) as caught:
            call()
        self.assertEqual(str(caught.exception), message)
        self.assertIsInstance(caught.exception, ControllerError)

    def test_rsig5_bounded_api_errors_fail_closed(self):
        ref_path = ('GET', API + '/git/ref/tags/reserve/v' + V)
        # A 404 before the repository is authenticated is an authorization failure, never absence.
        transport = FakeTransport({REPO_PATH: [Reply(404, {'message': 'Not Found'})]})
        gh = GITHUB.GitHub(transport, NO_PAUSE)
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Unauthorized,
                           'GitHub repository: not authorized (HTTP 404)')
        self.assertEqual(calls_to(transport, *ref_path), [])
        # A wrong repository identity refuses before any other read.
        transport = FakeTransport({REPO_PATH: [repo_reply(repo_id=99)]})
        self.assertRefuses(lambda: GITHUB.GitHub(transport, NO_PAUSE).ref('tags/reserve/v' + V), GITHUB.Malformed,
                           'GitHub repository: the repository is not adrenickchrome/rdy-core (1403751388)')
        # After authentication, 404 is authenticated absence; 401 and 403 are authorization failures.
        gh, transport = client({ref_path: [Reply(404, {'message': 'Not Found'})]})
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Absent, 'GitHub ref: not found')
        self.assertEqual(len(calls_to(transport, *ref_path)), 1)
        for status in (401, 403):
            gh, transport = client({ref_path: [Reply(status, {'message': 'Bad credentials'})]})
            self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Unauthorized,
                               'GitHub ref: not authorized (HTTP ' + str(status) + ')')
            self.assertEqual(len(calls_to(transport, *ref_path)), 1)
        # Other statuses are their own refusal, not absence and not retried.
        gh, transport = client({ref_path: [Reply(410, {'message': 'Gone'})]})
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.StatusError, 'GitHub ref: HTTP 410')
        self.assertEqual(len(calls_to(transport, *ref_path)), 1)
        # Transport uncertainty: three read attempts, then a refusal that names the uncertainty.
        limited = Reply(403, {'message': 'API rate limit exceeded'}, headers={'x-ratelimit-remaining': '0'})
        for reply, reason in ((Reply(502, {}), 'HTTP 502'), (GITHUB.Timeout(), 'timeout'),
                              (GITHUB.TransportFailure(), 'transport failure'), (limited, 'rate limited'),
                              (Reply(429, {}), 'rate limited')):
            gh, transport = client({ref_path: [reply]})
            self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Uncertain,
                               'GitHub ref: outcome unknown (' + reason + ')')
            self.assertEqual(len(calls_to(transport, *ref_path)), 3)
        good = Reply(200, {'ref': 'refs/tags/reserve/v' + V, 'object': {'type': 'tag', 'sha': '6' * 40}})
        gh, transport = client({ref_path: [Reply(500, {}), GITHUB.Timeout(), good]})
        self.assertEqual(gh.ref('tags/reserve/v' + V).object_sha, '6' * 40)
        self.assertEqual(len(calls_to(transport, *ref_path)), 3)
        # Size limits: a body over 4 MiB refuses and is not retried. Malformed bodies refuse.
        gh, transport = client({ref_path: [Reply(200, body=b'{' + b' ' * (4 * 1024 * 1024) + b'}')]})
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Malformed,
                           'GitHub ref: the response is larger than 4194304 bytes')
        self.assertEqual(len(calls_to(transport, *ref_path)), 1)
        for body in (b'not json', b'{"ref":"refs/tags/reserve/v0.42.0","object":{"type":"tag","sha":1.5}}',
                     b'[]', b'{"ref":"refs/tags/reserve/v0.42.0","object":{"type":"tag","sha":true}}'):
            gh, _ = client({ref_path: [Reply(200, body=body)]})
            with self.assertRaises(GITHUB.Malformed):
                gh.ref('tags/reserve/v' + V)

        # Pagination: at most 100 rows a page and 100 pages; any loss or truncation refuses.
        jobs = API + '/actions/runs/' + str(RUN_ID) + '/attempts/' + str(ATTEMPT) + '/jobs'

        def page(n):
            return ('GET', jobs + '?per_page=100&page=' + str(n))

        rows = [job_value(1000 + i) for i in range(3)]
        gh, transport = client({page(1): [Reply(200, {'total_count': 3, 'jobs': rows})]})
        self.assertEqual([j.job_id for j in gh.attempt_jobs(RUN_ID, ATTEMPT)], [1000, 1001, 1002])
        cases = (
            ({page(1): [Reply(200, {'total_count': 4, 'jobs': rows})], page(2): [Reply(200, {'total_count': 4,
                                                                                          'jobs': []})]},
             'GitHub jobs: the pages lost rows'),
            ({page(1): [Reply(200, {'total_count': 2, 'jobs': rows})]}, 'GitHub jobs: the pages returned extra rows'),
            ({page(1): [Reply(200, {'total_count': 3, 'jobs': rows[:2]})],
              page(2): [Reply(200, {'total_count': 3, 'jobs': rows[1:2]})]},
             'GitHub jobs: the pages returned a row twice'),
            ({page(1): [Reply(200, {'total_count': 3, 'jobs': rows[:2]})],
              page(2): [Reply(200, {'total_count': 4, 'jobs': rows[2:]})]},
             'GitHub jobs: the total changed between pages'),
            ({page(1): [Reply(200, {'total_count': 65, 'jobs': rows})]},
             'GitHub jobs: more than 64 rows'),
            ({page(1): [Reply(200, {'total_count': 3, 'jobs': rows})], },
             None),
        )
        for routes, message in cases:
            gh, transport = client(routes)
            if message is None:
                gh.attempt_jobs(RUN_ID, ATTEMPT)
                continue
            self.assertRefuses(lambda: gh.attempt_jobs(RUN_ID, ATTEMPT), GITHUB.Malformed, message)
        # A page larger than 100 rows refuses.
        many = [{'id': i, 'tag_name': 'v0.' + str(i) + '.0', 'target_commitish': S, 'draft': False,
                 'prerelease': True, 'assets': []} for i in range(1, 102)]
        releases = API + '/releases'
        gh, _ = client({('GET', releases + '?per_page=100&page=1'): [Reply(200, many)]})
        self.assertRefuses(gh.releases, GITHUB.Malformed, 'GitHub releases: a page holds more than 100 rows')
        # A list endpoint without a total follows its next link, and refuses past 100 pages.
        link = {'link': '<https://api.github.com/x?page=2>; rel="next"'}
        transport = FakeTransport()
        transport.add(REPO_PATH, repo_reply())
        for n in range(1, 101):
            transport.add(('GET', releases + '?per_page=100&page=' + str(n)),
                          Reply(200, [dict(many[0], id=100 * n + i) for i in range(100)], headers=link))
        self.assertRefuses(GITHUB.GitHub(transport, NO_PAUSE).releases, GITHUB.Malformed,
                           'GitHub releases: more than 100 pages')
        transport = FakeTransport()
        transport.add(REPO_PATH, repo_reply())
        transport.add(('GET', releases + '?per_page=100&page=1'), Reply(200, many[:2], headers=link))
        self.assertRefuses(GITHUB.GitHub(transport, NO_PAUSE).releases, GITHUB.Malformed,
                           'GitHub releases: a short page has a next page')

        # Contents over 1 MiB and a blob whose bytes do not match its Git id refuse.
        contents = ('GET', API + '/contents/release-keys/policy.json?ref=' + P)
        data = canon(policy())
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        reply = {'type': 'file', 'encoding': 'base64', 'path': 'release-keys/policy.json', 'size': len(data),
                 'sha': blob, 'content': base64.b64encode(data).decode()}
        gh, _ = client({contents: [Reply(200, reply)]})
        fact = gh.contents('release-keys/policy.json', P, 64 * 1024)
        self.assertEqual((fact.data, fact.sha256, fact.blob_sha), (data, sha(data), blob))
        gh, _ = client({contents: [Reply(200, dict(reply, size=2 * 1024 * 1024))]})
        self.assertRefuses(lambda: gh.contents('release-keys/policy.json', P, 64 * 1024), GITHUB.Malformed,
                           'GitHub contents: the file is larger than 65536 bytes')
        gh, _ = client({contents: [Reply(200, dict(reply, sha='0' * 40))]})
        self.assertRefuses(lambda: gh.contents('release-keys/policy.json', P, 64 * 1024), GITHUB.Malformed,
                           'GitHub contents: the bytes do not match their Git blob id')
        self.assertRefuses(lambda: gh.contents('../etc/passwd', P, 64 * 1024), ControllerError,
                           'the repository path is not allowlisted')

        # Artifact downloads are bounded and must match the artifact-service digest; a failure leaves no file.
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp, True)
        zip_path = ('GET', API + '/actions/artifacts/55/zip')
        location = 'https://blob.example.invalid/artifact-55'
        for data, limit, message in ((b'other-bytes', 1024, 'GitHub artifact download: the bytes do not match '
                                      'the artifact digest'),
                                     (b'zip-bytes', 4, 'GitHub artifact download: larger than 4 bytes')):
            gh, transport = client({('GET', API + '/actions/artifacts/55'): [Reply(200, artifact_value())],
                                    zip_path: [Reply(302, headers={'location': location})]},
                                   {location: data})
            fact = gh.artifact(55, RUN_ID)
            out = tmp / ('a-' + str(limit))
            self.assertRefuses(lambda: gh.download_artifact(fact, out, limit), GITHUB.Malformed, message)
            self.assertFalse(out.exists())
        gh, _ = client({('GET', API + '/actions/artifacts/55'): [Reply(200, artifact_value(expired=True))]})
        self.assertRefuses(lambda: gh.artifact(55, RUN_ID), GITHUB.Malformed, 'GitHub artifact: the artifact expired')
        gh, _ = client({('GET', API + '/actions/artifacts/55'): [Reply(200, artifact_value(digest=None))]})
        self.assertRefuses(lambda: gh.artifact(55, RUN_ID), GITHUB.Malformed,
                           'GitHub artifact: the artifact has no sha256 digest')

    def test_rsig5_transport_preserves_run_attempt_and_artifact_identity(self):
        run_path = ('GET', API + '/actions/runs/' + str(RUN_ID) + '/attempts/' + str(ATTEMPT))
        gh, transport = client({run_path: [Reply(200, run_value())]})
        run = gh.run_attempt(RUN_ID, ATTEMPT)
        self.assertEqual((run.run_id, run.run_attempt, run.workflow_id, run.path, run.head_sha, run.event,
                          run.actor_id, run.triggering_actor_id, run.repository_id, run.head_repository_id,
                          run.status, run.conclusion),
                         (RUN_ID, ATTEMPT, 81, '.github/workflows/release.yml', S, 'workflow_dispatch', 5001, 5002,
                          REPO_ID, REPO_ID, 'completed', 'success'))
        self.assertEqual(run.sha256, sha(json.dumps(run_value()).encode()))
        self.assertEqual(transport.calls[0][:2], REPO_PATH)
        # The answer must be the attempt asked for, in this repository, from this repository's head.
        for changes, message in (({'run_attempt': 1}, 'GitHub run attempt: the answer is not run 777001 attempt 2'),
                                 ({'id': RUN_ID + 1}, 'GitHub run attempt: the answer is not run 777001 attempt 2'),
                                 ({'repository': {'id': 99}}, 'GitHub run attempt: the run is not in rdy-core'),
                                 ({'head_repository': {'id': 99}}, 'GitHub run attempt: the run head is a fork'),
                                 ({'actor': {'id': True}}, 'GitHub run attempt: the actor id is malformed'),
                                 ({'head_sha': 'abc'}, 'GitHub run attempt: the head SHA is malformed')):
            gh, _ = client({run_path: [Reply(200, run_value(**changes))]})
            self.assertRefuses(lambda: gh.run_attempt(RUN_ID, ATTEMPT), GITHUB.Malformed, message)
        self.assertRefuses(lambda: gh.run_attempt(RUN_ID, 0), ControllerError, 'the run attempt is not a positive integer')
        self.assertRefuses(lambda: gh.run_attempt(True, 1), ControllerError, 'the run id is not a positive integer')
        # Jobs come from that exact attempt; a job of another attempt or run refuses.
        jobs = ('GET', API + '/actions/runs/' + str(RUN_ID) + '/attempts/' + str(ATTEMPT) + '/jobs?per_page=100&page=1')
        for row, message in ((job_value(1, attempt=1), 'GitHub jobs: a job is not from run 777001 attempt 2'),
                             (job_value(1, run_id=RUN_ID + 1), 'GitHub jobs: a job is not from run 777001 attempt 2')):
            gh, _ = client({jobs: [Reply(200, {'total_count': 1, 'jobs': [row]})]})
            self.assertRefuses(lambda: gh.attempt_jobs(RUN_ID, ATTEMPT), GITHUB.Malformed, message)
        gh, _ = client({jobs: [Reply(200, {'total_count': 1, 'jobs': [job_value(9)]})]})
        fact = gh.attempt_jobs(RUN_ID, ATTEMPT)[0]
        self.assertEqual((fact.job_id, fact.run_id, fact.run_attempt, fact.name, fact.conclusion, fact.html_url),
                         (9, RUN_ID, ATTEMPT, 'build-mac', 'success', ACTIONS + str(RUN_ID) + '/job/9'))
        one = ('GET', API + '/actions/jobs/9')
        gh, _ = client({one: [Reply(200, job_value(10))]})
        self.assertRefuses(lambda: gh.job(9), GITHUB.Malformed, 'GitHub job: the answer is not job 9')
        # Artifacts: exact id, originating run and repository, and the service digest without its prefix.
        art = ('GET', API + '/actions/artifacts/55')
        gh, _ = client({art: [Reply(200, artifact_value())]})
        fact = gh.artifact(55, RUN_ID)
        self.assertEqual((fact.artifact_id, fact.name, fact.run_id, fact.digest, fact.size, fact.head_sha,
                          fact.repository_id, fact.head_repository_id),
                         (55, 'rdy-candidate-a2', RUN_ID, sha(b'zip-bytes'), 9, S, REPO_ID, REPO_ID))
        for value, message in ((artifact_value(artifact_id=56), 'GitHub artifact: the answer is not artifact 55'),
                               (artifact_value(run_id=RUN_ID + 1), 'GitHub artifact: the artifact is not from run 777001'),
                               (artifact_value(workflow_run={'id': RUN_ID, 'repository_id': 99,
                                                             'head_repository_id': REPO_ID, 'head_sha': S}),
                                'GitHub artifact: the artifact is not in rdy-core')):
            gh, _ = client({art: [Reply(200, value)]})
            self.assertRefuses(lambda: gh.artifact(55, RUN_ID), GITHUB.Malformed, message)
        # A download goes to the redirect without credentials and returns the exact bytes' hash.
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp, True)
        location = 'https://blob.example.invalid/artifact-55'
        gh, transport = client({art: [Reply(200, artifact_value())],
                                ('GET', API + '/actions/artifacts/55/zip'): [Reply(302, headers={'location': location})]},
                               {location: b'zip-bytes'})
        fact = gh.artifact(55, RUN_ID)
        got = gh.download_artifact(fact, tmp / 'a.zip', 1024)
        self.assertEqual((got.sha256, got.size, got.artifact_id, got.run_id), (sha(b'zip-bytes'), 9, 55, RUN_ID))
        self.assertEqual((tmp / 'a.zip').read_bytes(), b'zip-bytes')
        self.assertEqual(transport.calls[-1], ('DOWNLOAD', location, None))
        self.assertRefuses(lambda: gh.download_artifact(fact, tmp / 'a.zip', 1024), ControllerError,
                           'the file a.zip already exists')
        # Refs: the exact ref, never a prefix match; the object type and SHA are returned for readback.
        ref = ('GET', API + '/git/ref/tags/reserve/v' + V)
        gh, _ = client({ref: [Reply(200, {'ref': 'refs/tags/reserve/v' + V + '-x',
                                          'object': {'type': 'tag', 'sha': '6' * 40}})]})
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Malformed,
                           'GitHub ref: the answer is not refs/tags/reserve/v0.42.0')
        gh, _ = client({ref: [Reply(200, [{'ref': 'refs/tags/reserve/v' + V, 'object': {'type': 'tag',
                                                                                        'sha': '6' * 40}}])]})
        self.assertRefuses(lambda: gh.ref('tags/reserve/v' + V), GITHUB.Malformed, 'GitHub ref: the answer is malformed')
        # Tag objects: the exact message bytes, tag name and direct target.
        message = canon(tuple_record()).decode()
        tag = ('GET', API + '/git/tags/' + '6' * 40)
        gh, _ = client({tag: [Reply(200, {'sha': '6' * 40, 'tag': 'reserve/v' + V, 'message': message,
                                          'object': {'type': 'commit', 'sha': S}})]})
        fact = gh.tag('6' * 40)
        self.assertEqual((fact.sha, fact.tag, fact.message_bytes, fact.object_type, fact.object_sha),
                         ('6' * 40, 'reserve/v' + V, message.encode(), 'commit', S))
        gh, _ = client({tag: [Reply(200, {'sha': '7' * 40, 'tag': 'reserve/v' + V, 'message': message,
                                          'object': {'type': 'commit', 'sha': S}})]})
        self.assertRefuses(lambda: gh.tag('6' * 40), GITHUB.Malformed, 'GitHub tag: the answer is not object ' + '6' * 40)
        # Commits name their tree and parents.
        commit = ('GET', API + '/git/commits/' + S)
        gh, _ = client({commit: [Reply(200, {'sha': S, 'tree': {'sha': '8' * 40}, 'parents': [{'sha': P}]})]})
        fact = gh.commit(S)
        self.assertEqual((fact.sha, fact.tree_sha, fact.parents), (S, '8' * 40, (P,)))
        # Every read path carries the exact numeric identity it asked for.
        paths = [c[1] for c in transport.calls]
        self.assertIn(API + '/actions/artifacts/55', paths)

    def test_rsig5_mutations_are_not_retried_on_timeout_409_422(self):
        tags = ('POST', API + '/git/tags')
        refs = ('POST', API + '/git/refs')
        message = canon(tuple_record()).decode()

        def create_tag(gh):
            return gh.create_tag_object('reserve/v' + V, message, S)

        def create_ref(gh):
            return gh.create_ref('refs/tags/reserve/v' + V, '6' * 40)

        for route, create, name in ((tags, create_tag, 'create tag object'), (refs, create_ref, 'create ref')):
            for reply, reason in ((GITHUB.Timeout(), 'timeout'), (Reply(409, {'message': 'conflict'}), 'HTTP 409'),
                                  (Reply(422, {'message': 'Reference already exists'}), 'HTTP 422'),
                                  (Reply(500, {}), 'HTTP 500'), (GITHUB.TransportFailure(), 'transport failure'),
                                  (Reply(429, {}), 'HTTP 429')):
                with self.subTest(operation=name, reason=reason):
                    gh, transport = client({route: [reply]})
                    with self.assertRaises(GITHUB.ReconcileRequired) as caught:
                        create(gh)
                    self.assertEqual(str(caught.exception), 'GitHub ' + name + ': outcome unknown (' + reason
                                     + '); read the exact ref and object before any further write')
                    self.assertEqual(len(calls_to(transport, *route)), 1)
            for status in (401, 403, 404):
                gh, transport = client({route: [Reply(status, {})]})
                self.assertRefuses(lambda: create(gh), GITHUB.Unauthorized,
                                   'GitHub ' + name + ': not authorized (HTTP ' + str(status) + ')')
                self.assertEqual(len(calls_to(transport, *route)), 1)
        # A success returns what GitHub created, for exact readback; a boolean is not enough.
        created = {'sha': '6' * 40, 'tag': 'reserve/v' + V, 'message': message, 'object': {'type': 'commit', 'sha': S}}
        gh, transport = client({tags: [Reply(201, created)]})
        fact = create_tag(gh)
        self.assertEqual((fact.sha, fact.tag, fact.message_bytes, fact.object_type, fact.object_sha),
                         ('6' * 40, 'reserve/v' + V, message.encode(), 'commit', S))
        sent = json.loads(calls_to(transport, *tags)[0][2])
        self.assertEqual(sent, {'tag': 'reserve/v' + V, 'message': message, 'object': S, 'type': 'commit'})
        gh, transport = client({refs: [Reply(201, {'ref': 'refs/tags/reserve/v' + V,
                                                   'object': {'type': 'tag', 'sha': '6' * 40}})]})
        fact = create_ref(gh)
        self.assertEqual((fact.ref, fact.object_type, fact.object_sha), ('refs/tags/reserve/v' + V, 'tag', '6' * 40))
        self.assertEqual(json.loads(calls_to(transport, *refs)[0][2]), {'ref': 'refs/tags/reserve/v' + V,
                                                                        'sha': '6' * 40})
        # A 201 whose answer differs from the request is not success: reconcile.
        gh, transport = client({tags: [Reply(201, dict(created, message=message + 'x'))]})
        with self.assertRaises(GITHUB.ReconcileRequired) as caught:
            create_tag(gh)
        self.assertEqual(str(caught.exception), 'GitHub create tag object: outcome unknown (the answer differs from '
                         'the request); read the exact ref and object before any further write')
        gh, transport = client({refs: [Reply(201, {'ref': 'refs/tags/reserve/v' + V,
                                                   'object': {'type': 'commit', 'sha': '6' * 40}})]})
        with self.assertRaises(GITHUB.ReconcileRequired):
            create_ref(gh)
        self.assertEqual(len(calls_to(transport, *refs)), 1)
        # Only exact names: no lightweight or other-namespace ref, no non-commit tag target.
        gh, transport = client()
        self.assertRefuses(lambda: gh.create_ref('refs/heads/main', '6' * 40), ControllerError,
                           'the ref is not refs/tags/reserve/v<0.N.0>')
        self.assertRefuses(lambda: gh.create_tag_object('v' + V, message, S), ControllerError,
                           'the tag is not reserve/v<0.N.0>')
        self.assertEqual([c for c in transport.calls if c[0] == 'POST'], [])
        # The mutation never runs before the repository identity is authenticated.
        transport = FakeTransport({REPO_PATH: [Reply(404, {})]})
        self.assertRefuses(lambda: create_ref(GITHUB.GitHub(transport, NO_PAUSE)), GITHUB.Unauthorized,
                           'GitHub repository: not authorized (HTTP 404)')
        self.assertEqual([c for c in transport.calls if c[0] == 'POST'], [])


if __name__ == '__main__':
    unittest.main()
