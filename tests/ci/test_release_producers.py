"""The approval producer and the release evidence (E) producer: their interface, pinned before the code.

Nothing here reaches GitHub, AWS or a key. Every identity is synthetic and every fixture is built here; no private
dossier, vector or handoff file is read. The workflow checks parse YAML with a real safe parser that keeps `on`
as a string key and refuses duplicate keys. That parser is PyYAML 6.0.3 from the hash-pinned, test-only
requirement in tests/ci/requirements-release-producers.txt. Without it every workflow check fails; none skips.

The producers live in scripts/release_producers/ and load as the package `release_producers`. They reuse the
unchanged release parsers of scripts/release/. A refusal is the release scripts' ControllerError (or a subclass).
"""
import ast
import base64
import gzip
import hashlib
import importlib
import json
from pathlib import Path
import random
import re
import subprocess
import sys
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'release'))
sys.path.insert(0, str(ROOT / 'scripts'))
import candidate as cand  # noqa: E402
import evidence as ev  # noqa: E402
import github as gh  # noqa: E402
import rehearse  # noqa: E402

try:
    import yaml
except ImportError:  # the workflow checks fail explicitly below; they never skip
    yaml = None

WORKFLOWS = ROOT / '.github' / 'workflows'
PRODUCERS = ROOT / 'scripts' / 'release_producers'
REQUIREMENTS = ROOT / 'tests' / 'ci' / 'requirements-release-producers.txt'
APPROVAL_WF = '.github/workflows/release-approval.yml'
EVIDENCE_WF = '.github/workflows/release-evidence.yml'
YAML_MISSING = ('PyYAML 6.0.3 is required: install it with '
                'pip install --require-hashes -r tests/ci/requirements-release-producers.txt')

# The reviewed signer revision and the pinned actions (the base already uses each of them).
H = '3dac36753d9512773f91a2650c79e2805941ecbd'
CHECKOUT = 'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1'
UPLOAD = 'actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9'
DOWNLOAD = 'actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333'
SHA40_RE = re.compile(r'[0-9a-f]{40}')
LANE = 'rdy-core-release-authority'

# Synthetic identities: never production values. The enrolled founder is data, not a source constant.
REPO = 'adrenickchrome/rdy-core'
REPO_ID = 1403751388
FORK_ID = 1403751399
FOUNDER = 4242
OTHER_FOUNDER = 9191
COORDINATOR = 5001
SECOND_COORDINATOR = 5003
STRANGER = 6666
APPROVAL_WF_ID = 70001
EVIDENCE_WF_ID = 70002
ENVIRONMENT_ID = 80001
ACCOUNT = '111122223333'
KEY_ARN = 'arn:aws:kms:us-east-2:' + ACCOUNT + ':key/00000000-0000-4000-8000-000000000001'
FINGERPRINT = 'SHA256:' + 'A' * 43
HEAD = 'c' * 40
POLICY = '3' * 40
P = '4' * 40
S = '1' * 40
VERSION = '0.46.0'
MAC = 'aarch64-apple-darwin'
LINUX = 'x86_64-unknown-linux-gnu'
ACTIONS = 'https://github.com/' + REPO + '/actions/runs/'

KIB = 1024
MIB = 1024 * KIB
DISPATCH_MAX = 61440
BASE64_MAX = 57344
GZIP_MAX = 43008
DOCUMENT_MAX = 1048576
REQUEST_BODY_MAX = 60000
APPROVAL_RUNS_MAX = 16
APPROVAL_RUNS_BYTES_MAX = 65536

E_INPUTS = ('version', 'candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id', 'attestation_gzip_b64',
            'attestation_sha256', 'approval_runs')
ATTESTATION_KEYS = ('tutorials', 'advisory_fetch', 'advisory_log', 'audit_reference', 'audit_body',
                    'export_receipt_body')
TRANSPORT_ONLY = ('advisory_log', 'audit_reference', 'audit_body', 'export_receipt_body')
E_JOBS = {'authenticate-evidence': 'Authenticate release evidence',
          'install-mac': 'Native evidence install (aarch64-apple-darwin)',
          'install-linux': 'Native evidence install (x86_64-unknown-linux-gnu)',
          'seal-release-evidence': 'Seal release evidence'}
NATIVE = {'install-mac': (MAC, 'macos-26'), 'install-linux': (LINUX, 'ubuntu-26.04')}

# The plan's field-source rows, word for word: every record E carries and its exact fields.
FIELD_SOURCES = {
    'file': ('name', 'sha256', 'size'),
    'job': ('run_id', 'run_attempt', 'job_id', 'job_url', 'conclusion'),
    'artifact': ('id', 'name', 'digest', 'size'),
    'source': ('notes_sha256', 'guide_sha256', 'source_json_sha256', 'export_receipt_sha256'),
    'candidate': ('ref', 'run_id', 'run_attempt', 'artifact', 'manifest_sha256', 'artifacts'),
    'target': ('target', 'archive', 'build', 'package', 'fast_gate', 'install'),
    'fast_gate': ('job', 'passed', 'failed', 'ignored', 'prerequisite_skipped'),
    'install': ('job', 'report_sha256', 'checks', 'skipped'),
    'security': ('jobs', 'codeql_open_high_critical', 'advisory_fetch'),
    'advisory_fetch': ('tool', 'fetched_at', 'log_sha256', 'result'),
    'audit': ('reference', 'sha256'),
    'tutorial': ('target', 'agent', 'model', 'effort', 'session', 'tutorial_revision', 'steps', 'archive_sha256',
                 'manifest_sha256', 'result'),
    'tutorial_step': ('id', 'result'),
    'approval_basis': ('mode', 'decision', 'policy_commit', 'scope', 'cards'),
    'card': ('kind', 'reference', 'sha256'),
    'release_record': ('schema', 'repository', 'version', 'commit', 'design_landing', 'source', 'candidate',
                       'targets', 'security', 'audit', 'tutorials', 'approval_basis'),
}
CONSUMER_FIELDS = {
    'file': ('name', 'sha256', 'size'), 'job': ev.JOB_KEYS, 'artifact': ev.ARTIFACT_KEYS,
    'source': ev.SOURCE_DIGEST_KEYS, 'candidate': ev.CANDIDATE_KEYS, 'target': ev.TARGET_KEYS,
    'fast_gate': ev.FAST_GATE_KEYS, 'install': ev.INSTALL_KEYS, 'security': ev.SECURITY_KEYS,
    'advisory_fetch': ev.ADVISORY_KEYS, 'audit': ev.AUDIT_KEYS, 'tutorial': rehearse.TUTORIAL_KEYS,
    'tutorial_step': ('id', 'result'), 'approval_basis': ev.BASIS_KEYS, 'card': ev.CARD_KEYS,
    'release_record': ev.E_KEYS,
}


# ---------------------------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------------------------

def sha(data):
    return hashlib.sha256(data).hexdigest()


def digest(n):
    return format(n, '064x')


def canon(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False) + '\n').encode('utf-8')


def producer(name):
    """One producer module. Until the producers exist, this import is the expected failure."""
    return importlib.import_module('release_producers.' + name)


def noise(n, seed=7):
    """`n` incompressible ASCII letters: deterministic, and never a path or credential shape."""
    rng = random.Random(seed)
    return ''.join(rng.choice('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ') for _ in range(n))


def pack(data, level=9):
    return gzip.compress(data, compresslevel=level, mtime=0)


def b64(data):
    return base64.b64encode(data).decode('ascii')


def archive(target):
    return {'name': cand.archive_name(VERSION, target), 'sha256': digest(10 + len(target)), 'size': 1000 + len(target)}


ARCHIVES = [archive(MAC), archive(LINUX)]
MANIFEST = sha(cand.manifest_bytes(ARCHIVES))
TUTORIALS = [{'target': t, 'agent': 'claude-code', 'model': 'claude-sonnet-test', 'effort': 'medium',
              'session': 'session-' + t, 'tutorial_revision': 'rev-1',
              'steps': [{'id': 'install', 'result': 'pass'}, {'id': 'first-use', 'result': 'pass'}],
              'archive_sha256': f['sha256'], 'manifest_sha256': MANIFEST, 'result': 'pass'}
             for t, f in zip((MAC, LINUX), ARCHIVES)]
ADVISORY_LOG = 'advisories: 0 found\n'


def document_value(audit_body='The audit dossier.\n', advisory_log=ADVISORY_LOG, export='{"export":"receipt"}\n'):
    return {'tutorials': TUTORIALS,
            'advisory_fetch': {'tool': 'cargo-deny 0.18.3', 'fetched_at': '2026-11-08T10:00:00Z',
                               'log_sha256': sha(advisory_log.encode('utf-8')), 'result': 'clean'},
            'advisory_log': advisory_log, 'audit_reference': 'audit/rdy-' + VERSION, 'audit_body': audit_body,
            'export_receipt_body': export}


def document(**changes):
    """The decompressed attestation: original JSON, not canonical on purpose."""
    return json.dumps(document_value(**changes), indent=1, ensure_ascii=False).encode('utf-8')


def exact_gzip(size):
    """A valid attestation whose stored (level 0) gzip is exactly `size` bytes."""
    pad = size - len(document(audit_body='')) - 23
    for _ in range(64):
        data = document(audit_body=noise(pad))
        packed = pack(data, level=0)
        if len(packed) == size:
            return data, packed
        pad += size - len(packed)
    raise AssertionError('no gzip of exactly ' + str(size) + ' bytes')


def exact_document(size):
    data = document(audit_body='')
    data = document(audit_body='a' * (size - len(data)))
    assert len(data) == size
    return data


def channel(founder=FOUNDER):
    return {'schema': 1, 'repository': REPO, 'repository_id': REPO_ID, 'founder_user_id': founder,
            'workflow_id': APPROVAL_WF_ID, 'workflow_path': APPROVAL_WF, 'implementation_sha': P,
            'workflow_file_sha256': digest(41), 'dispatch_ref': 'refs/heads/main',
            'required_job_name': 'accept-founder-approval', 'environment_id': ENVIRONMENT_ID,
            'environment_name': 'release-exception-approval', 'prevent_self_review': False,
            'can_admins_bypass': False,
            'archive': {'provider': 'dynamodb', 'account_id': ACCOUNT, 'region': 'us-east-2',
                        'table': 'rdy-test-approvals', 'pk_prefix': 'APPROVAL#', 'sk': 'RECORD'},
            'enabled': True, 'revoked_receipts': []}


def evidence_policy(founder=FOUNDER, coordinators=(COORDINATOR, SECOND_COORDINATOR)):
    value = {'schema': 1, 'repository_id': REPO_ID,
             'coordinators': [{'actor_id': a, 'app_id': None, 'installation_id': None} for a in coordinators],
             'evidence_workflow': {'id': EVIDENCE_WF_ID, 'path': EVIDENCE_WF, 'sha': P},
             'release_workflow_path': '.github/workflows/release.yml', 'approval_channel': channel(founder)}
    return ev.parse_evidence_policy(canon(value))


def run_fact(**changes):
    value = dict(run_id=990001, run_attempt=1, workflow_id=APPROVAL_WF_ID, path=APPROVAL_WF, head_sha=HEAD,
                 head_branch='main', event='workflow_dispatch', status='in_progress', conclusion=None,
                 actor_id=FOUNDER, triggering_actor_id=FOUNDER, repository_id=REPO_ID, head_repository_id=REPO_ID,
                 html_url=ACTIONS + '990001', sha256=digest(7))
    value.update(changes)
    return gh.RunFact(**value)


def evidence_run(**changes):
    value = dict(run_id=888001, workflow_id=EVIDENCE_WF_ID, path=EVIDENCE_WF, head_sha=S,
                 head_branch='candidate/v' + VERSION, actor_id=COORDINATOR, triggering_actor_id=COORDINATOR,
                 html_url=ACTIONS + '888001')
    value.update(changes)
    return run_fact(**value)


def key_change_request(policy_commit=POLICY):
    scope = {'repository': REPO, 'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT, 'first_machine_tag': 'v' + VERSION,
             'transition_sha256': digest(31), 'targets': [MAC, LINUX]}
    return canon({'schema': 1, 'decision': 'D-PZKAP', 'policy_commit': policy_commit, 'kind': 'key_change',
                  'scope': scope})


def e_inputs(**changes):
    data = document()
    value = {'version': VERSION, 'candidate_run_id': '777001', 'candidate_run_attempt': '1',
             'candidate_artifact_id': '55', 'attestation_gzip_b64': b64(pack(data)), 'attestation_sha256': sha(data),
             'approval_runs': '[]'}
    value.update(changes)
    return value


# ---------------------------------------------------------------------------------------------
# The workflow parser
# ---------------------------------------------------------------------------------------------

def workflow_loader():
    """A safe loader where only true/false are booleans (so `on` stays a string) and a duplicate key refuses."""

    class Loader(yaml.SafeLoader):
        pass

    Loader.yaml_implicit_resolvers = {
        first: [(tag, regex) for tag, regex in resolvers if tag != 'tag:yaml.org,2002:bool']
        for first, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()}
    Loader.add_implicit_resolver('tag:yaml.org,2002:bool', re.compile(r'^(?:true|false)$'), list('tf'))

    def mapping(loader, node, deep=False):
        loader.flatten_mapping(node)
        seen = {}
        for key_node, value_node in node.value:
            key = loader.construct_object(key_node, deep=deep)
            if key in seen:
                raise yaml.constructor.ConstructorError(None, None, 'duplicate key ' + repr(key), key_node.start_mark)
            seen[key] = loader.construct_object(value_node, deep=deep)
        return seen

    Loader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, mapping)
    return Loader


def parse_yaml(text):
    return yaml.load(text, Loader=workflow_loader())


def as_list(value):
    if value is None:
        return []
    return [value] if isinstance(value, str) else list(value)


def strings(value, where=()):
    """Every string in a parsed workflow, with its key path."""
    if isinstance(value, dict):
        for key, item in value.items():
            yield from strings(item, where + (key,))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from strings(item, where + (index,))
    elif isinstance(value, str):
        yield where, value


class WorkflowCase(unittest.TestCase):
    def workflow(self, path):
        if yaml is None:
            self.fail(YAML_MISSING)
        source = ROOT / path
        self.assertTrue(source.is_file(), path + ' does not exist yet')
        value = parse_yaml(source.read_text(encoding='utf-8'))
        self.assertIsInstance(value, dict, path)
        return value

    def both(self):
        return {APPROVAL_WF: self.workflow(APPROVAL_WF), EVIDENCE_WF: self.workflow(EVIDENCE_WF)}

    @staticmethod
    def steps(job):
        return job.get('steps') or []

    @staticmethod
    def uploads(job):
        return [s for s in job.get('steps') or [] if str(s.get('uses', '')).startswith('actions/upload-artifact@')]


class Refusals(unittest.TestCase):
    def refuses(self, call, *args, message=None, **kwargs):
        """The call refuses with the release scripts' ControllerError (any subclass), and with `message`."""
        try:
            call(*args, **kwargs)
        except Exception as error:  # noqa: BLE001 - the class is checked by name below
            names = [k.__name__ for k in type(error).__mro__]
            self.assertIn('ControllerError', names, repr(error))
            if message is not None:
                self.assertEqual(str(error), message)
            return error
        self.fail('no refusal')


# ---------------------------------------------------------------------------------------------
# The test-only parser and the producer sources
# ---------------------------------------------------------------------------------------------

class Parser(WorkflowCase):
    def test_rprd1_yaml_parser_is_pinned_and_required(self):
        lines = [line for line in REQUIREMENTS.read_text().splitlines() if line and not line.startswith('#')]
        joined = ' '.join(line.rstrip('\\').strip() for line in lines).split()
        self.assertEqual(joined[0], 'PyYAML==6.0.3')
        hashes = joined[1:]
        self.assertTrue(hashes)
        for entry in hashes:
            self.assertRegex(entry, r'^--hash=sha256:[0-9a-f]{64}$')
        # CI (Python 3.13 on Linux x86-64) and the local gate (Python 3.13 on macOS arm64) both resolve.
        self.assertIn('--hash=sha256:0f29edc409a6392443abf94b9cf89ce99889a1dd5376d94316ae5145dfedd5d6', hashes)
        self.assertIn('--hash=sha256:2283a07e2c21a2aa78d9c4442724ec1eb15f5e42a723b99cb3d822d48f5f7ad1', hashes)
        if yaml is None:
            self.fail(YAML_MISSING)
        self.assertEqual(yaml.__version__, '6.0.3')
        self.assertEqual(parse_yaml('on:\n  workflow_dispatch:\n'), {'on': {'workflow_dispatch': None}})
        self.assertEqual(parse_yaml('a: yes\nb: off\nc: true\n'), {'a': 'yes', 'b': 'off', 'c': True})
        with self.assertRaises(yaml.YAMLError):
            parse_yaml('jobs:\n  a: 1\n  a: 2\n')

    def test_rprd1_producer_sources(self):
        helpers = ('common.py', 'approval.py', 'evidence.py', 'install.py', 'rehearsal_test_signer.py')
        for name in helpers:
            self.assertTrue((PRODUCERS / name).is_file(), name)
        for path in sorted(PRODUCERS.glob('*.py')):
            source = path.read_text(encoding='utf-8')
            with self.subTest(helper=path.name):
                # Runtime code runs on /usr/bin/python3 (3.9 on macOS) and never needs the test parser.
                ast.parse(source, feature_version=(3, 9))
                self.assertNotRegex(source, r'(?m)^\s*(?:import|from)\s+yaml\b')
                # The founder is enrollment data under every custody option, never a source constant.
                self.assertNotIn('32169767', source)
        for path in (WORKFLOWS / 'release-approval.yml', WORKFLOWS / 'release-evidence.yml'):
            self.assertNotIn('32169767', path.read_text(encoding='utf-8'))
        # The signer never imports a producer helper.
        for path in sorted((ROOT / 'scripts' / 'release').glob('*.py')):
            self.assertNotIn('release_producers', path.read_text(encoding='utf-8'), path.name)
        # H calls the disposable signer as one argv element: executable in git, with a fixed interpreter.
        signer = 'scripts/release_producers/rehearsal_test_signer.py'
        staged = subprocess.run(['git', '-C', str(ROOT), 'ls-files', '-s', signer], capture_output=True, text=True)
        self.assertTrue(staged.stdout.startswith('100755 '), staged.stdout)
        self.assertEqual((ROOT / signer).read_text().splitlines()[0], '#!/usr/bin/python3 -I -B')


# ---------------------------------------------------------------------------------------------
# The two workflows
# ---------------------------------------------------------------------------------------------

class Workflows(WorkflowCase):
    def test_rprd1_approval_workflow_structure(self):
        flow = self.workflow(APPROVAL_WF)
        self.assertEqual(list(flow['on']), ['workflow_dispatch'])
        inputs = flow['on']['workflow_dispatch']['inputs']
        self.assertEqual(list(inputs), ['request_body'])
        self.assertLessEqual(set(inputs['request_body']), {'description', 'type', 'required'})
        self.assertEqual((inputs['request_body']['type'], inputs['request_body']['required']), ('string', True))
        self.assertEqual(flow['permissions'], {})
        jobs = flow['jobs']
        self.assertEqual(sorted(jobs), ['accept-founder-approval', 'prepare-approval'])
        for job_id, job in jobs.items():
            self.assertEqual(job.get('name'), job_id)
        self.assertEqual(as_list(jobs['prepare-approval'].get('needs')), [])
        self.assertEqual(as_list(jobs['accept-founder-approval'].get('needs')), ['prepare-approval'])
        # Preparation runs outside the review environment; only acceptance waits for the founder.
        self.assertNotIn('environment', jobs['prepare-approval'])
        environment = jobs['accept-founder-approval']['environment']
        if isinstance(environment, dict):
            self.assertEqual(environment, {'name': 'release-exception-approval'})
        else:
            self.assertEqual(environment, 'release-exception-approval')
        # Separate approval concurrency by run: a pending review never blocks E or the release lane.
        group = str(flow['concurrency']['group'])
        self.assertIn('${{ github.run_id }}', group)
        self.assertNotIn(LANE, group)
        for job in jobs.values():
            self.assertNotIn('concurrency', job)
        # Only acceptance uploads, and only the original request bytes.
        self.assertEqual(self.uploads(jobs['prepare-approval']), [])
        uploads = self.uploads(jobs['accept-founder-approval'])
        self.assertEqual(len(uploads), 1)
        upload = uploads[0]['with']
        self.assertEqual(upload['name'], 'approval-request-${{ github.run_id }}-1')
        self.assertTrue(str(upload['path']).endswith('/approval-request.json'), upload['path'])
        self.assertNotRegex(str(upload['path']), r'[*?\[\n]')
        self.assertEqual((upload['overwrite'], upload['if-no-files-found'], upload['retention-days']),
                         (False, 'error', 90))

    def test_rprd1_evidence_workflow_structure(self):
        flow = self.workflow(EVIDENCE_WF)
        self.assertEqual(list(flow['on']), ['workflow_dispatch'])
        inputs = flow['on']['workflow_dispatch']['inputs']
        self.assertEqual(sorted(inputs), sorted(E_INPUTS))
        for name, spec in inputs.items():
            with self.subTest(input=name):
                self.assertLessEqual(set(spec), {'description', 'type', 'required'})
                self.assertEqual((spec['type'], spec['required']), ('string', True))
        self.assertEqual(flow['permissions'], {})
        jobs = flow['jobs']
        self.assertEqual(sorted(jobs), sorted(E_JOBS))
        for job_id, job in jobs.items():
            with self.subTest(job=job_id):
                self.assertEqual(job.get('name'), E_JOBS[job_id])
                self.assertNotIn('environment', job)
                self.assertIsInstance(job.get('timeout-minutes'), int)
                self.assertTrue(1 <= job['timeout-minutes'] <= 60)
        self.assertEqual(as_list(jobs['authenticate-evidence'].get('needs')), [])
        self.assertEqual(as_list(jobs['install-mac']['needs']), ['authenticate-evidence'])
        self.assertEqual(as_list(jobs['install-linux']['needs']), ['authenticate-evidence'])
        self.assertEqual(sorted(as_list(jobs['seal-release-evidence']['needs'])),
                         ['authenticate-evidence', 'install-linux', 'install-mac'])
        # Only the final job uploads E: one attempt-specific artifact, never overwritten, kept 90 days.
        for job_id in ('authenticate-evidence', 'install-mac', 'install-linux'):
            for step in self.uploads(jobs[job_id]):
                self.assertNotIn('release-record.json', str(step['with']['path']), job_id)
        uploads = self.uploads(jobs['seal-release-evidence'])
        self.assertEqual(len(uploads), 1)
        upload = uploads[0]['with']
        self.assertEqual(upload['name'], 'release-evidence-${{ github.run_id }}-a${{ github.run_attempt }}')
        self.assertTrue(str(upload['path']).endswith('/release-record.json'), upload['path'])
        self.assertNotRegex(str(upload['path']), r'[*?\[\n]')
        self.assertEqual((upload['overwrite'], upload['if-no-files-found'], upload['retention-days']),
                         (False, 'error', 90))

    def test_rprd1_release_authority_lane(self):
        flow = self.workflow(EVIDENCE_WF)
        # The shared lane is never on the E workflow itself, and never on an unauthenticated or native job.
        self.assertNotIn(LANE, str(flow.get('concurrency', '')))
        jobs = flow['jobs']
        for job_id in ('authenticate-evidence', 'install-mac', 'install-linux'):
            self.assertNotIn(LANE, str(jobs[job_id].get('concurrency', '')), job_id)
        # Only the authenticated sealer enters it, after both native jobs; it never replaces a pending run.
        self.assertEqual(jobs['seal-release-evidence']['concurrency'],
                         {'group': LANE, 'cancel-in-progress': False, 'queue': 'max'})
        approval = self.workflow(APPROVAL_WF)
        self.assertNotIn(LANE, json.dumps(approval))

    def test_rprd1_native_jobs(self):
        jobs = self.workflow(EVIDENCE_WF)['jobs']
        for job_id, (target, label) in NATIVE.items():
            job = jobs[job_id]
            with self.subTest(job=job_id):
                # One fresh standard hosted VM per target; a timeout fails, never skips a check.
                self.assertEqual(job['runs-on'], label)
                self.assertEqual(job['timeout-minutes'], 60)
                runs = [s['run'] for s in self.steps(job) if 'run' in s]
                self.assertTrue(any('scripts/release_producers/install.py' in r and target in r for r in runs))
                # Unconditional cleanup of the task-owned paths comes last.
                last = self.steps(job)[-1]
                self.assertIn(last.get('if'), ('always()', '${{ always() }}'))
                self.assertIn('run', last)
                # Only the explicit sanitized receipt leaves the job: no globs, no failure bundles.
                uploads = self.uploads(job)
                self.assertEqual(len(uploads), 1)
                upload = uploads[0]['with']
                self.assertTrue(str(upload['path']).endswith('/out/rehearsal-' + target + '.json'), upload['path'])
                self.assertNotRegex(str(upload['path']), r'[*?\[\n]')
                self.assertEqual((upload['overwrite'], upload['if-no-files-found']), (False, 'error'))
                self.assertNotIn('always', str(uploads[0].get('if', '')))
        for job_id, job in jobs.items():
            self.assertIsInstance(job['runs-on'], str, job_id)
            self.assertNotIn('latest', job['runs-on'], job_id)
            self.assertNotIn('container', job, job_id)
            self.assertNotIn('services', job, job_id)

    def test_rprd1_workflow_permissions_and_credentials(self):
        for path, flow in self.both().items():
            self.assertEqual(flow['permissions'], {}, path)
            self.assertNotIn('env', flow, path)
            for job_id, job in flow['jobs'].items():
                with self.subTest(workflow=path, job=job_id):
                    allowed = {'contents', 'actions'}
                    if (path, job_id) == (EVIDENCE_WF, 'seal-release-evidence'):
                        allowed.add('security-events')  # E's one alert reader
                    permissions = job.get('permissions')
                    self.assertIsInstance(permissions, dict)
                    self.assertLessEqual(set(permissions), allowed)
                    self.assertEqual(set(permissions.values()) - {'read'}, set())
                    self.assertNotIn('secrets', job)
                    self.assertNotIn('uses', job)  # no reusable workflow call, so no inherited secrets
                    self.assertNotIn('github.token', json.dumps(job.get('env', {})))
            for where, value in strings(flow):
                with self.subTest(workflow=path, where=where):
                    lowered = value.lower()
                    self.assertNotRegex(lowered, r'\baws(?:\b|_)')
                    for word in ('secrets.', 'secrets:', 'id-token', 'aws-actions', 'amazonaws',
                                 'rdy_release_publisher', 'rdy_publisher_token', 'create-github-app-token',
                                 'rdy_release_key_arn', 'rdy_release_role_arn', ': write'):
                        self.assertNotIn(word, lowered)
                    self.assertNotIn('secrets', [str(k) for k in where])
            # The token appears only in the env of a bounded API step that runs a pinned producer helper.
            for job_id, job in flow['jobs'].items():
                for step in self.steps(job):
                    holds = 'github.token' in json.dumps({k: v for k, v in step.items() if k != 'env'})
                    self.assertFalse(holds, (path, job_id, step.get('name')))
                    if 'github.token' in json.dumps(step.get('env', {})):
                        self.assertIn('scripts/release_producers/', step.get('run', ''), (path, job_id))

    def test_rprd1_no_input_interpolation(self):
        for path, flow in self.both().items():
            for job_id, job in flow['jobs'].items():
                for where, value in strings(job):
                    with self.subTest(workflow=path, job=job_id, where=where):
                        # Inputs are data in the event file, never an expression anywhere in a job.
                        self.assertNotIn('inputs.', value)
                        self.assertNotIn('github.event.', value)
                for step in self.steps(job):
                    run = step.get('run')
                    if run is None:
                        continue
                    with self.subTest(workflow=path, job=job_id, step=step.get('name')):
                        self.assertNotIn('${{', run)
                        for line in run.splitlines():
                            if 'release_producers/' in line:
                                self.assertTrue(line.strip().startswith('/usr/bin/python3 -I -B '), line)

    def test_rprd1_workflow_action_pins_and_checkouts(self):
        for path, flow in self.both().items():
            refs = []
            for job_id, job in flow['jobs'].items():
                for step in self.steps(job):
                    uses = step.get('uses')
                    if uses is None:
                        continue
                    with self.subTest(workflow=path, job=job_id, uses=uses):
                        self.assertIn(uses, (CHECKOUT, UPLOAD, DOWNLOAD))
                        if uses != CHECKOUT:
                            continue
                        options = step['with']
                        self.assertIs(options['persist-credentials'], False)
                        self.assertNotIn('token', options)
                        self.assertIn(options.get('repository', REPO), (REPO,))
                        self.assertIn('path', options)
                        self.assertRegex(str(options['ref']), SHA40_RE)
                        refs.append(options['ref'])
            # Literal trusted parsers at H and literal producer helpers at P; nothing loads from S or a branch.
            self.assertIn(H, refs, path)
            others = set(refs) - {H}
            self.assertEqual(len(others), 1, path)
            self.assertNotIn(S, others)
        approval_refs = {s['with']['ref'] for j in self.workflow(APPROVAL_WF)['jobs'].values()
                         for s in self.steps(j) if s.get('uses') == CHECKOUT}
        evidence_refs = {s['with']['ref'] for j in self.workflow(EVIDENCE_WF)['jobs'].values()
                         for s in self.steps(j) if s.get('uses') == CHECKOUT}
        self.assertEqual(approval_refs, evidence_refs)
        # The comment style of release-sign.yml: `@<sha> # v<N>` on every action line.
        for path in (APPROVAL_WF, EVIDENCE_WF):
            for line in (ROOT / path).read_text().splitlines():
                if re.match(r'\s*(?:- )?uses: ', line):
                    self.assertRegex(line, r'@[0-9a-f]{40} # v\d', line)


# ---------------------------------------------------------------------------------------------
# The run contract and the field sources
# ---------------------------------------------------------------------------------------------

class Contract(Refusals):
    def test_rprd1_run_contract(self):
        approval = producer('approval')
        self.assertEqual(approval.WORKFLOW_PATH, APPROVAL_WF)
        self.assertEqual(approval.JOBS, ('prepare-approval', 'accept-founder-approval'))
        self.assertEqual(approval.ENVIRONMENT, 'release-exception-approval')
        self.assertEqual(approval.MEMBER, 'approval-request.json')
        self.assertEqual(approval.REQUEST_MAX, REQUEST_BODY_MAX)
        self.assertEqual(approval.artifact_name(4321), 'approval-request-4321-1')
        pev = producer('evidence')
        self.assertEqual(pev.WORKFLOW_PATH, EVIDENCE_WF)
        self.assertEqual(pev.JOB_NAMES, E_JOBS)
        self.assertEqual(pev.NATIVE, {MAC: 'macos-26', LINUX: 'ubuntu-26.04'})
        self.assertEqual(pev.MEMBER, 'release-record.json')
        self.assertEqual(pev.RECORD_MAX, 1048576)
        self.assertEqual(pev.INPUT_KEYS, E_INPUTS)
        self.assertEqual(pev.artifact_name(888001, 2), 'release-evidence-888001-a2')
        common = producer('common')
        self.assertEqual((common.DISPATCH_MAX, common.BASE64_MAX, common.GZIP_MAX, common.DOCUMENT_MAX),
                         (DISPATCH_MAX, BASE64_MAX, GZIP_MAX, DOCUMENT_MAX))
        self.assertEqual((common.APPROVAL_RUNS_MAX, common.APPROVAL_RUNS_BYTES_MAX, common.ID_DIGITS_MAX),
                         (APPROVAL_RUNS_MAX, APPROVAL_RUNS_BYTES_MAX, 20))

    def test_rprd1_field_sources(self):
        # The plan's rows are the consumer's fields; the producer declares exactly those rows.
        for name, fields in FIELD_SOURCES.items():
            self.assertEqual(tuple(CONSUMER_FIELDS[name]), fields, name)
        pev = producer('evidence')
        self.assertEqual({k: tuple(v) for k, v in pev.RECORD_FIELDS.items()}, FIELD_SOURCES)
        # The transport keys stay out of every release schema.
        schemas = set().union(*[set(v) for v in CONSUMER_FIELDS.values()], ev.REQUEST_KEYS, ev.RECEIPT_KEYS,
                              ev.ISSUER_V2_KEYS, ev.PROJECTION_V2_KEYS, ev.IDENTITY_KEYS, ev.ENVELOPE_KEYS,
                              ev.ACCEPTANCE_KEYS, ev.EVIDENCE_POLICY_KEYS, ev.CHANNEL_KEYS)
        for key in TRANSPORT_ONLY + ('attestation_gzip_b64', 'attestation_sha256', 'approval_runs'):
            self.assertNotIn(key, schemas)

    def test_rprd1_approval_run_authentication(self):
        approval = producer('approval')
        for founder in (FOUNDER, OTHER_FOUNDER):
            enrolled = evidence_policy(founder=founder).value['approval_channel']
            with self.subTest(founder=founder):
                # The same source accepts whichever founder is enrolled, for both dispatch and trigger.
                approval.authenticate_run(run_fact(actor_id=founder, triggering_actor_id=founder), enrolled)
                stranger = OTHER_FOUNDER if founder == FOUNDER else FOUNDER
                for change in ({'actor_id': stranger}, {'triggering_actor_id': stranger},
                               {'actor_id': stranger, 'triggering_actor_id': stranger}):
                    fact = dict(actor_id=founder, triggering_actor_id=founder)
                    fact.update(change)
                    self.refuses(approval.authenticate_run, run_fact(**fact), enrolled)
        enrolled = channel()
        for change in ({'run_attempt': 2}, {'head_branch': 'feature'}, {'head_branch': None},
                       {'event': 'push'}, {'event': 'workflow_call'}, {'workflow_id': APPROVAL_WF_ID + 1},
                       {'path': EVIDENCE_WF}, {'head_repository_id': FORK_ID}, {'repository_id': FORK_ID}):
            with self.subTest(change=change):
                self.refuses(approval.authenticate_run, run_fact(**change), enrolled)

    def test_rprd1_evidence_run_authentication(self):
        pev = producer('evidence')
        policy = evidence_policy()
        pev.authenticate_run(evidence_run(), policy, VERSION)
        # A rerun by another authorized coordinator authenticates both actors again.
        pev.authenticate_run(evidence_run(run_attempt=2, triggering_actor_id=SECOND_COORDINATOR), policy, VERSION)
        for change in ({'actor_id': STRANGER}, {'triggering_actor_id': STRANGER}, {'actor_id': FOUNDER},
                       {'head_branch': 'main'}, {'head_branch': 'candidate/v0.47.0'}, {'event': 'push'},
                       {'workflow_id': EVIDENCE_WF_ID + 1}, {'path': APPROVAL_WF},
                       {'head_repository_id': FORK_ID}, {'repository_id': FORK_ID}):
            with self.subTest(change=change):
                self.refuses(pev.authenticate_run, evidence_run(**change), policy, VERSION)

    def test_rprd1_digest_domains(self):
        approval = producer('approval')
        request = key_change_request()
        self.assertEqual(ev.parse_approval_request(request).sha256, sha(request))
        # The comment names the exact request bytes, final LF included; the comment itself has no LF.
        self.assertEqual(approval.required_comment(request), 'APPROVE sha256:' + sha(request))
        self.assertNotEqual(approval.required_comment(request), 'APPROVE sha256:' + sha(request[:-1]))
        pev = producer('evidence')
        receipt_sha256 = digest(77)
        reference = pev.card_reference(channel(), receipt_sha256)
        self.assertEqual(reference, 'dynamodb://' + ACCOUNT + '/us-east-2/rdy-test-approvals/APPROVAL%23'
                         + receipt_sha256 + '/RECORD')
        self.assertEqual(ev.card_receipt_sha256({'kind': 'key_change', 'reference': reference,
                                                 'sha256': receipt_sha256}, channel()), receipt_sha256)


# ---------------------------------------------------------------------------------------------
# Control inputs
# ---------------------------------------------------------------------------------------------

class Inputs(Refusals):
    def test_rprd1_event_file_is_closed_json(self):
        common = producer('common')
        event = canon({'inputs': {'request_body': 'x\n'}, 'ref': 'refs/heads/main'})
        self.assertEqual(common.load_event(event)['inputs'], {'request_body': 'x\n'})
        self.refuses(common.load_event, b'{"inputs":{"request_body":"a","request_body":"b"}}',
                     message="duplicate field 'request_body'")
        self.refuses(common.load_event, b'{"inputs":{"n":1.5}}')
        self.refuses(common.load_event, b'{"inputs":{"request_body":"\xff"}}')

    def test_rprd1_approval_input(self):
        approval = producer('approval')
        request = key_change_request()
        body = approval.request_bytes({'request_body': request.decode('utf-8')})
        self.assertEqual(body, request)
        self.assertEqual(ev.parse_approval_request(body).value['kind'], 'key_change')
        # The bound counts UTF-8 bytes and holds before any JSON parsing.
        self.assertEqual(len(approval.request_bytes({'request_body': 'x' * 59999 + '\n'})), REQUEST_BODY_MAX)
        self.assertEqual(len(approval.request_bytes({'request_body': 'é' * 29999 + 'a\n'})), REQUEST_BODY_MAX)
        for oversize in ('x' * 60000 + '\n', 'é' * 30000 + '\n'):
            self.refuses(approval.request_bytes, {'request_body': oversize},
                         message='request_body is larger than 60000 bytes')
        self.refuses(approval.request_bytes, {'request_body': request.decode('utf-8')[:-1]},
                     message='request_body does not end with LF')
        self.refuses(approval.request_bytes, {'request_bdy': request.decode('utf-8')},
                     message="unknown field 'request_bdy'; did you mean 'request_body'?")
        self.refuses(approval.request_bytes, {}, message="missing field 'request_body'")
        self.refuses(approval.request_bytes, {'request_body': 7})
        # A shell metacharacter payload stays data: it is refused as a request, never run.
        hostile = '$(touch /tmp/rdy-rprd1-owned)`id`;\n'
        self.assertEqual(approval.request_bytes({'request_body': hostile}), hostile.encode('utf-8'))
        self.refuses(ev.parse_approval_request, hostile.encode('utf-8'))

    def test_rprd1_approval_dispatch(self):
        approval = producer('approval')
        common = producer('common')
        request = key_change_request()
        prepared = approval.prepare_dispatch(request)
        self.assertEqual(prepared, {'ref': 'refs/heads/main', 'inputs': {'request_body': request.decode('utf-8')}})
        self.assertLessEqual(len(common.dispatch_request(prepared['ref'], prepared['inputs'])), DISPATCH_MAX)
        self.refuses(approval.prepare_dispatch, request[:-1])
        self.refuses(approval.prepare_dispatch, request.replace(b'"schema":1', b'"schema": 1'))

    def test_rprd1_dispatch_request_bound(self):
        common = producer('common')
        ref = 'refs/tags/candidate/v' + VERSION
        self.assertEqual(common.dispatch_request(ref, {'a': 'b'}), canon({'inputs': {'a': 'b'}, 'ref': ref}))
        overhead = len(common.dispatch_request(ref, {'a': ''}))
        exact = {'a': 'x' * (DISPATCH_MAX - overhead)}
        self.assertEqual(common.check_dispatch(ref, exact), DISPATCH_MAX)
        self.refuses(common.check_dispatch, ref, {'a': 'x' * (DISPATCH_MAX - overhead + 1)},
                     message='the dispatch request is larger than 61440 bytes')
        # Multi-byte text counts in UTF-8 bytes.
        self.refuses(common.check_dispatch, ref, {'a': 'é' * ((DISPATCH_MAX - overhead) // 2 + 1)},
                     message='the dispatch request is larger than 61440 bytes')

    def test_rprd1_evidence_inputs(self):
        pev = producer('evidence')
        parsed = pev.dispatch_inputs(e_inputs(approval_runs='[{"artifact_id":12,"run_id":34}]'))
        self.assertEqual(parsed['version'], VERSION)
        self.assertEqual((parsed['candidate_run_id'], parsed['candidate_run_attempt'],
                          parsed['candidate_artifact_id']), (777001, 1, 55))
        self.assertEqual(parsed['approval_runs'], [{'artifact_id': 12, 'run_id': 34}])
        self.assertEqual(sorted(parsed), sorted(E_INPUTS))
        self.assertEqual(pev.dispatch_inputs(e_inputs(candidate_run_id='9' * 20))['candidate_run_id'], int('9' * 20))
        # Closed keys, with the nearest name.
        wrong = e_inputs()
        wrong['atestation_sha256'] = wrong.pop('attestation_sha256')
        self.refuses(pev.dispatch_inputs, wrong,
                     message="unknown field 'atestation_sha256'; did you mean 'attestation_sha256'?")
        missing = e_inputs()
        del missing['approval_runs']
        self.refuses(pev.dispatch_inputs, missing, message="missing field 'approval_runs'")
        for key in ('candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id'):
            for bad in ('0', '-1', '01', '1.0', ' 1', '+1', '1e3', '', '9' * 21):
                with self.subTest(key=key, value=bad):
                    self.refuses(pev.dispatch_inputs, e_inputs(**{key: bad}),
                                 message=key + ' is not a positive decimal of at most 20 digits')
        for bad in ('0.46.1', '0.046.0', '1.0.0', 'v0.46.0', ''):
            self.refuses(pev.dispatch_inputs, e_inputs(version=bad))
        good = sha(document())
        for bad in (good.upper(), 'sha256:' + good, good[:63], good + '0'):
            self.refuses(pev.dispatch_inputs, e_inputs(attestation_sha256=bad),
                         message='attestation_sha256 is not lowercase 64-hex')
        self.refuses(pev.dispatch_inputs, e_inputs(attestation_gzip_b64='A' * (BASE64_MAX + 4)),
                     message='attestation_gzip_b64 is longer than 57344 characters')
        self.refuses(pev.dispatch_inputs, e_inputs(version=46))

    def test_rprd1_approval_runs_input(self):
        pev = producer('evidence')
        entry = '{"artifact_id":%d,"run_id":%d}'
        sixteen = '[' + ','.join(entry % (n + 1, n + 100) for n in range(16)) + ']'
        self.assertEqual(len(pev.dispatch_inputs(e_inputs(approval_runs=sixteen))['approval_runs']), 16)
        seventeen = '[' + ','.join(entry % (n + 1, n + 100) for n in range(17)) + ']'
        self.refuses(pev.dispatch_inputs, e_inputs(approval_runs=seventeen),
                     message='approval_runs holds more than 16 entries')
        exact = '[' + ' ' * (APPROVAL_RUNS_BYTES_MAX - 2) + ']'
        self.assertEqual(pev.dispatch_inputs(e_inputs(approval_runs=exact))['approval_runs'], [])
        over = '[' + ' ' * (APPROVAL_RUNS_BYTES_MAX - 1) + ']'
        self.refuses(pev.dispatch_inputs, e_inputs(approval_runs=over),
                     message='approval_runs is larger than 65536 bytes')
        self.refuses(pev.dispatch_inputs, e_inputs(approval_runs='[{"artifact_ids":1,"run_id":2}]'),
                     message="unknown field 'artifact_ids'; did you mean 'artifact_id'?")
        self.refuses(pev.dispatch_inputs, e_inputs(approval_runs='[{"artifact_id":1,"run_id":2,"run_id":3}]'),
                     message="duplicate field 'run_id'")
        for bad in ('{}', '[1]', '[{"artifact_id":1.0,"run_id":2}]', '[{"artifact_id":true,"run_id":2}]',
                    '[{"artifact_id":"1","run_id":2}]', '[{"artifact_id":0,"run_id":2}]',
                    '[{"artifact_id":1,"run_id":2},{"artifact_id":1,"run_id":2}]', 'not json'):
            with self.subTest(approval_runs=bad):
                self.refuses(pev.dispatch_inputs, e_inputs(approval_runs=bad))


# ---------------------------------------------------------------------------------------------
# The attestation transport
# ---------------------------------------------------------------------------------------------

class Transport(Refusals):
    def test_rprd1_attestation_bounds(self):
        # The fixtures first: they are independent of the producers.
        data, packed = exact_gzip(GZIP_MAX)
        self.assertEqual(len(packed), GZIP_MAX)
        self.assertEqual(zlib.decompress(packed, 31), data)
        self.assertEqual(rehearse.check_tutorials(TUTORIALS, {'version': VERSION, 'artifacts': ARCHIVES,
                                                              'manifest_sha256': MANIFEST}), TUTORIALS)
        pev = producer('evidence')
        # The gzip and base64 bounds meet exactly: 43008 bytes are 57344 canonical characters.
        text = b64(packed)
        self.assertEqual(len(text), BASE64_MAX)
        self.assertEqual(pev.decode_base64(text), packed)
        self.assertEqual(pev.decode_gzip(packed), data)
        self.assertEqual(pev.decode_attestation(text, sha(data)), data)
        over_data, over_packed = exact_gzip(GZIP_MAX + 1)
        self.refuses(pev.decode_gzip, over_packed, message='the attestation gzip is larger than 43008 bytes')
        self.refuses(pev.decode_base64, b64(over_packed),
                     message='attestation_gzip_b64 is longer than 57344 characters')
        self.refuses(pev.decode_attestation, b64(over_packed), sha(over_data),
                     message='attestation_gzip_b64 is longer than 57344 characters')
        # The decompressed document: exactly 1 MiB passes; one byte more refuses while streaming.
        whole = exact_document(DOCUMENT_MAX)
        self.assertEqual(pev.decode_attestation(b64(pack(whole)), sha(whole)), whole)
        more = exact_document(DOCUMENT_MAX + 1)
        self.refuses(pev.decode_attestation, b64(pack(more)), sha(more),
                     message='the attestation is larger than 1048576 bytes')
        bomb = pack(b'\0' * (32 * MIB))
        self.assertLessEqual(len(bomb), GZIP_MAX)
        self.refuses(pev.decode_gzip, bomb, message='the attestation is larger than 1048576 bytes')

    def test_rprd1_attestation_refusals(self):
        pev = producer('evidence')
        noncanonical = 'attestation_gzip_b64 is not canonical padded standard base64'
        for bad in ('-_8=', 'AB==', 'AAA', 'AAAA\nAAAA', 'AAAA AAAA', ' AAAA', 'AAAA=', 'AA=A', 'éAAA'):
            with self.subTest(base64=bad):
                self.refuses(pev.decode_base64, bad, message=noncanonical)
        data = document()
        packed = pack(data)
        self.refuses(pev.decode_gzip, packed + pack(b'{}'), message='the attestation gzip holds more than one member')
        self.refuses(pev.decode_gzip, packed + b'\0junk', message='the attestation gzip has trailing bytes')
        for offset in (-8, -1):  # the CRC-32 and the length
            broken = bytearray(packed)
            broken[offset] ^= 0x01
            self.refuses(pev.decode_gzip, bytes(broken), message='the attestation gzip is corrupt')
        self.refuses(pev.decode_gzip, b'\x1f\x8b\x08\x00' + b'\0' * 6 + b'\xff\xff\xff',
                     message='the attestation gzip is corrupt')
        for cut in (packed[:-4], packed[:len(packed) // 2], packed[:10], b''):
            self.refuses(pev.decode_gzip, cut, message='the attestation gzip is incomplete')
        # The digest is checked over the exact decompressed bytes before any JSON parsing.
        text = b64(packed)
        mismatch = 'the attestation does not match attestation_sha256'
        for wrong in (sha(packed), sha(text.encode('ascii')), sha(canon(json.loads(data))), sha(data + b'\n')):
            self.refuses(pev.decode_attestation, text, wrong, message=mismatch)
        not_json = b'this is not JSON'
        self.refuses(pev.decode_attestation, b64(pack(not_json)), sha(b'other bytes'), message=mismatch)
        self.assertEqual(pev.decode_attestation(b64(pack(not_json)), sha(not_json)), not_json)
        for wrong in (sha(data).upper(), 'sha256:' + sha(data)):
            self.refuses(pev.decode_attestation, text, wrong, message='attestation_sha256 is not lowercase 64-hex')

    def test_rprd1_attestation_bytes_survive(self):
        pev = producer('evidence')
        # NFC and NFD stay distinct; CRLF, tabs, trailing blanks and separators stay as written.
        audit = 'café café\r\n\ttrailing  \n \U0001f600 end'
        log = 'advisory log\r\nline two  \n'
        export = '{"b": 1, "a": 2}\n'
        data = document(audit_body=audit, advisory_log=log, export=export)
        text = pev.encode_attestation(data)
        self.assertEqual(text, b64(base64.b64decode(text)))
        self.assertEqual(pev.decode_attestation(text, sha(data)), data)
        facts = pev.attestation_facts(data)
        self.assertEqual(sorted(facts), ['advisory_fetch', 'audit', 'export_receipt_sha256', 'tutorials'])
        self.assertEqual(facts['audit'], {'reference': 'audit/rdy-' + VERSION, 'sha256': sha(audit.encode('utf-8'))})
        self.assertNotEqual(facts['audit']['sha256'], sha(audit.replace('café', 'café').encode('utf-8')))
        self.assertEqual(facts['export_receipt_sha256'], sha(export.encode('utf-8')))
        self.assertEqual(facts['advisory_fetch']['log_sha256'], sha(log.encode('utf-8')))
        self.assertEqual(facts['tutorials'], TUTORIALS)
        for key in TRANSPORT_ONLY:
            self.assertNotIn(key, json.dumps(facts))

    def test_rprd1_attestation_document_closed(self):
        pev = producer('evidence')
        value = document_value()
        value['audit_bdy'] = value.pop('audit_body')
        self.refuses(pev.attestation_facts, canon(value),
                     message="unknown field 'audit_bdy'; did you mean 'audit_body'?")
        for key in ATTESTATION_KEYS:
            short = document_value()
            del short[key]
            self.refuses(pev.attestation_facts, canon(short), message="missing field '" + key + "'")
        doubled = document()[:-1] + b',"audit_body":"again"}'
        self.refuses(pev.attestation_facts, doubled, message="duplicate field 'audit_body'")
        self.refuses(pev.attestation_facts, document().replace(b'"The audit', b'"\xff The audit'))
        self.refuses(pev.attestation_facts, document().replace(b'"The audit', b'"\\ud800 The audit'))
        self.refuses(pev.attestation_facts, document().replace(b'"tutorial_revision": "rev-1"',
                                                               b'"tutorial_revision": 1.5'))
        self.refuses(pev.attestation_facts, canon(dict(document_value(), audit_body=7)))
        self.refuses(pev.attestation_facts, b'this is not JSON')
        self.refuses(pev.attestation_facts, exact_document(DOCUMENT_MAX + 1))
        # The advisory metadata must name the exact log it travels with.
        self.refuses(pev.attestation_facts, canon(dict(document_value(), advisory_log='another log\n')))
        bad_fetch = document_value()
        bad_fetch['advisory_fetch'] = dict(bad_fetch['advisory_fetch'], result='dirty')
        self.refuses(pev.attestation_facts, canon(bad_fetch))

    def test_rprd1_attestation_classification(self):
        pev = producer('evidence')
        common = producer('common')
        data = document()
        runs = [{'artifact_id': 12, 'run_id': 34}]
        prepared = pev.prepare_dispatch(data, 'public', VERSION, 777001, 1, 55, runs)
        self.assertEqual(sorted(prepared), ['inputs', 'ref', 'sizes'])
        self.assertEqual(prepared['ref'], 'refs/tags/candidate/v' + VERSION)
        inputs = prepared['inputs']
        self.assertEqual(sorted(inputs), sorted(E_INPUTS))
        self.assertTrue(all(isinstance(v, str) for v in inputs.values()))
        self.assertEqual(json.loads(inputs['approval_runs']), runs)
        self.assertEqual(pev.decode_attestation(inputs['attestation_gzip_b64'], inputs['attestation_sha256']), data)
        self.assertEqual(pev.dispatch_inputs(inputs)['candidate_run_id'], 777001)
        self.assertEqual(prepared['sizes'], {
            'document': len(data), 'gzip': len(base64.b64decode(inputs['attestation_gzip_b64'])),
            'base64': len(inputs['attestation_gzip_b64']),
            'dispatch': len(common.dispatch_request(prepared['ref'], inputs))})
        # Dispatch inputs are public-capable: only evidence classified public is ever submitted.
        for label in ('private', 'unclassified', '', None, 'Public', 'public '):
            with self.subTest(classification=label):
                self.refuses(pev.prepare_dispatch, data, label, VERSION, 777001, 1, 55, [],
                             message='the attestation is not classified public')
        for body in ('see /Users/someone/notes.txt', 'ran in /home/runner/work/x'):
            self.refuses(pev.prepare_dispatch, document(audit_body=body), 'public', VERSION, 777001, 1, 55, [],
                         message='the attestation holds a private path')
        for body in ('-----BEGIN OPENSSH PRIVATE KEY-----', 'token ghp_' + 'A1b2' * 9,
                     'token github_pat_' + 'A1b2' * 20, 'key AKIA' + 'ABCDEFGHIJKLMNOP'):
            self.refuses(pev.prepare_dispatch, document(audit_body=body), 'public', VERSION, 777001, 1, 55, [],
                         message='the attestation holds a credential')
        # A dossier that does not fit refuses before dispatch and reports every measured size.
        large = document(audit_body=noise(100000))
        error = self.refuses(pev.prepare_dispatch, large, 'public', VERSION, 777001, 1, 55, [])
        self.assertTrue(str(error).startswith('the attestation does not fit'), str(error))
        self.assertEqual(sorted(error.sizes), ['base64', 'dispatch', 'document', 'gzip'])
        self.assertEqual(error.sizes['document'], len(large))
        self.assertGreater(error.sizes['gzip'], GZIP_MAX)
        whole = exact_document(DOCUMENT_MAX + 1)
        error = self.refuses(pev.prepare_dispatch, whole, 'public', VERSION, 777001, 1, 55, [])
        self.assertEqual(error.sizes['document'], DOCUMENT_MAX + 1)



# ---------------------------------------------------------------------------------------------
# The approval producer and its receipt bridge (rprd2)
# ---------------------------------------------------------------------------------------------

APPROVAL_RUN = 990001
PREPARE_JOB, ACCEPT_JOB, APPROVAL_ARTIFACT = 991001, 991002, 991003
OTHER_COMMIT = '9' * 40
APPROVAL_WF_BYTES = b'name: release-approval\non: workflow_dispatch\n'
SOURCE_KINDS = ('artifact', 'implementation_source', 'job', 'protected_policy', 'repository', 'request_archive',
                'review_history', 'run_attempt', 'workflow_source')


def full_scope():
    return {'repository': REPO, 'version': VERSION, 'commit': S, 'design_landing': 'L-test',
            'manifest_sha256': MANIFEST, 'artifacts': sorted(ARCHIVES, key=lambda f: f['name']),
            'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT, 'first_machine_tag': 'v' + VERSION,
            'transition_sha256': digest(31), 'targets': [MAC, LINUX]}


def scope_for(kind):
    full = full_scope()
    if kind == 'release_1':
        return full
    if kind == 'key_change':
        return {k: full[k] for k in ('repository', 'key_arn', 'fingerprint', 'first_machine_tag',
                                     'transition_sha256', 'targets')}
    if kind == 'revocation':
        return {'repository': REPO, 'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT,
                'revocation_sha256': digest(32), 'last_good_tag': None}
    return {'repository': REPO, 'targets': [MAC, LINUX]}


def approval_request(kind, policy_commit=POLICY):
    return canon({'schema': 1, 'decision': 'D-PZKAP', 'policy_commit': policy_commit, 'kind': kind,
                  'scope': scope_for(kind)})


class ApprovalWorld(object):
    """One founder approval run on protected main, with the facts GitHub would hold. Every identity is
    synthetic. Main's first-parent chain is HEAD -> POLICY -> P; the run ran at HEAD."""

    def __init__(self, founder=FOUNDER):
        self.founder = founder
        self.attempt = 1
        self.calls, self.blocked = [], []
        self.commits = {HEAD: (POLICY,), POLICY: (P,), P: ()}
        self.channel = dict(channel(founder), workflow_file_sha256=sha(APPROVAL_WF_BYTES))
        self.policy = canon({'schema': 1, 'repository_id': REPO_ID,
                             'coordinators': [{'actor_id': COORDINATOR, 'app_id': None, 'installation_id': None}],
                             'evidence_workflow': {'id': EVIDENCE_WF_ID, 'path': EVIDENCE_WF, 'sha': P},
                             'release_workflow_path': '.github/workflows/release.yml',
                             'approval_channel': self.channel})
        self.files = {('release-keys/evidence-policy.json', HEAD): self.policy,
                      ('release-keys/evidence-policy.json', POLICY): self.policy,
                      (APPROVAL_WF, HEAD): APPROVAL_WF_BYTES, (APPROVAL_WF, P): APPROVAL_WF_BYTES}
        self.runs = {(APPROVAL_RUN, 1): dict(
            run_id=APPROVAL_RUN, run_attempt=1, workflow_id=APPROVAL_WF_ID, path=APPROVAL_WF, head_sha=HEAD,
            head_branch='main', event='workflow_dispatch', status='in_progress', conclusion=None, actor_id=founder,
            triggering_actor_id=founder, repository_id=REPO_ID, head_repository_id=REPO_ID,
            html_url=ACTIONS + str(APPROVAL_RUN), sha256=digest(7))}
        self.jobs = {(APPROVAL_RUN, 1): [self.job(PREPARE_JOB, 'prepare-approval', 'in_progress', None),
                                         self.job(ACCEPT_JOB, 'accept-founder-approval', 'queued', None)]}
        self.artifacts, self.members, self.reviews, self.responses = {}, {}, {}, {}

    @staticmethod
    def job(job_id, name, status, conclusion, attempt=1):
        return dict(job_id=job_id, run_id=APPROVAL_RUN, run_attempt=attempt, name=name, status=status,
                    conclusion=conclusion, html_url=ACTIONS + str(APPROVAL_RUN) + '/job/' + str(job_id), head_sha=HEAD)

    def event(self, body):
        return canon({'inputs': {'request_body': body.decode('utf-8') if isinstance(body, bytes) else body},
                      'ref': 'refs/heads/main', 'repository': {'id': REPO_ID}})

    def review(self, body, **changes):
        value = dict(state='approved', comment='APPROVE sha256:' + sha(body), user_id=self.founder, user_type='User',
                     environments=((ENVIRONMENT_ID, 'release-exception-approval'),))
        value.update(changes)
        return value

    def reviewed(self, body):
        """The founder approved; preparation succeeded and acceptance is running."""
        self.reviews[APPROVAL_RUN] = [self.review(body)]
        self.jobs[(APPROVAL_RUN, 1)] = [self.job(PREPARE_JOB, 'prepare-approval', 'completed', 'success'),
                                        self.job(ACCEPT_JOB, 'accept-founder-approval', 'in_progress', None)]

    def uploaded(self, body):
        """Acceptance uploaded exactly `body`, and the run completed."""
        zip_digest = digest(500 + len(body))
        self.artifacts[APPROVAL_ARTIFACT] = dict(
            artifact_id=APPROVAL_ARTIFACT, name='approval-request-' + str(APPROVAL_RUN) + '-1', size=400 + len(body),
            digest=zip_digest, expired=False, run_id=APPROVAL_RUN, repository_id=REPO_ID, head_repository_id=REPO_ID,
            head_sha=HEAD, sha256=digest(8))
        self.members[(APPROVAL_ARTIFACT, 'approval-request.json')] = body
        self.runs[(APPROVAL_RUN, 1)].update(status='completed', conclusion='success')
        self.jobs[(APPROVAL_RUN, 1)] = [self.job(PREPARE_JOB, 'prepare-approval', 'completed', 'success'),
                                        self.job(ACCEPT_JOB, 'accept-founder-approval', 'completed', 'success')]
        art = self.artifacts[APPROVAL_ARTIFACT]
        known = {'protected_policy': (sha(self.policy), len(self.policy)),
                 'request_archive': (art['digest'], art['size']),
                 'workflow_source': (sha(APPROVAL_WF_BYTES), len(APPROVAL_WF_BYTES)),
                 'implementation_source': (sha(APPROVAL_WF_BYTES), len(APPROVAL_WF_BYTES))}
        self.responses[APPROVAL_RUN] = [{'kind': k, 'sha256': known.get(k, (digest(600 + i), 0))[0],
                                         'bytes': known.get(k, (None, 700 + i))[1]}
                                        for i, k in enumerate(SOURCE_KINDS)]


class ApprovalFacts(object):
    """The read-only fact source over an ApprovalWorld. Its write and trust-tool methods only record that a
    producer reached them, which the ReadOnly wrapper must prevent."""

    def __init__(self, world, gh_module):
        self.w = world
        self.gh = gh_module

    def _absent(self, operation):
        return self.gh.Absent(operation, 'not found', 404)

    def repository(self):
        self.w.calls.append(('repository',))
        return self.gh.RepositoryFact(REPO_ID, REPO, digest(90))

    def run_attempt(self, run_id, attempt):
        self.w.calls.append(('run_attempt', run_id, attempt))
        if (run_id, attempt) not in self.w.runs:
            raise self._absent('run attempt')
        return self.gh.RunFact(**self.w.runs[(run_id, attempt)])

    def attempt_jobs(self, run_id, attempt):
        self.w.calls.append(('attempt_jobs', run_id, attempt))
        return [self.gh.JobFact(**j) for j in self.w.jobs.get((run_id, attempt), [])]

    def artifact(self, artifact_id, run_id):
        self.w.calls.append(('artifact', artifact_id, run_id))
        if artifact_id not in self.w.artifacts:
            raise self._absent('artifact')
        fact = self.gh.ArtifactFact(**self.w.artifacts[artifact_id])
        if fact.run_id != run_id:
            raise self.gh.Malformed('artifact', 'the artifact is not from run ' + str(run_id))
        if fact.expired:
            raise self.gh.ArtifactExpired('artifact', 'the artifact expired')
        return fact

    def artifact_member(self, fact, name, limit):
        data = self.w.members.get((fact.artifact_id, name))
        if data is None or len(data) > limit:
            raise self.gh.ControllerError('the artifact does not hold ' + name)
        return data

    def run_approvals(self, run_id):
        self.w.calls.append(('run_approvals', run_id))
        return [self.gh.ApprovalFact(**r) for r in self.w.reviews.get(run_id, [])]

    def approval_responses(self, run_id, artifact_id):
        return [dict(r) for r in self.w.responses[run_id]]

    def ref(self, name):
        return self.gh.RefFact('refs/' + name, 'commit', HEAD, digest(91))

    def commit(self, sha_):
        if sha_ not in self.w.commits:
            raise self._absent('commit')
        return self.gh.CommitFact(sha_, '8' * 40, tuple(self.w.commits[sha_]), digest(92))

    def contents(self, path, ref_sha, limit=1048576):
        data = self.w.files.get((path, ref_sha))
        if data is None:
            raise self._absent('contents')
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        return self.gh.FileFact(path, ref_sha, blob, data, sha(data))

    def release_trust(self, *args):
        self.w.blocked.append('release_trust')
        raise AssertionError('a producer called release_trust')

    def create_ref(self, *args):
        self.w.blocked.append('create_ref')
        raise AssertionError('a producer wrote a ref')


class ApprovalProducer(Refusals):
    def setUp(self):
        self.approval = producer('approval')
        self.common = producer('common')
        self.elig = self.approval.elig

    def facts(self, world):
        return ApprovalFacts(world, self.approval.gh)

    def inputs(self, world, body):
        return self.common.load_event(world.event(body))['inputs']

    def produce(self, kind, founder=FOUNDER, world=None):
        """The whole producer path: dispatch, preparation, review, acceptance, upload, receipt."""
        world = world or ApprovalWorld(founder)
        request = approval_request(kind)
        prepared = self.approval.prepare_dispatch(request)
        self.assertEqual(prepared['ref'], 'refs/heads/main')
        inputs = self.inputs(world, prepared['inputs']['request_body'])
        facts = self.facts(world)
        text = self.approval.prepare(facts, APPROVAL_RUN, 1, inputs)
        self.assertIn('APPROVE sha256:' + sha(request), text)
        self.assertIn(kind, text)
        world.reviewed(request)
        body = self.approval.accept(facts, APPROVAL_RUN, 1, inputs)
        self.assertEqual(body, request)
        world.uploaded(body)
        result = self.approval.receipt(facts, APPROVAL_RUN, APPROVAL_ARTIFACT, kind, POLICY)
        self.assertEqual(world.blocked, [])
        return world, request, result

    def consumer(self, world):
        """The consumer's own view: wrapped facts, main, E's evidence policy and current authority."""
        wrapped = self.elig._Facts(self.facts(world))
        main = self.elig._Main(wrapped)
        policy = self.approval.ev.parse_evidence_policy(world.policy)
        return wrapped, main, policy, policy.value['approval_channel']

    def card(self, kind, receipt_sha256):
        return {'kind': kind, 'sha256': receipt_sha256,
                'reference': 'dynamodb://' + ACCOUNT + '/us-east-2/rdy-test-approvals/APPROVAL%23' + receipt_sha256
                             + '/RECORD'}

    def test_rprd2_approval_round_trip(self):
        ev_ = self.approval.ev
        for founder in (FOUNDER, OTHER_FOUNDER):
            for kind in ('release_1', 'key_change', 'revocation', 'new_platform'):
                with self.subTest(founder=founder, kind=kind):
                    world, request, result = self.produce(kind, founder)
                    # Only the existing schema 2 records and the caller's locator shape.
                    receipt = ev_.parse_approval_receipt(result.receipt_bytes)
                    self.assertEqual(receipt.value['schema'], 2)
                    self.assertEqual(sorted(receipt.value['issuer']), sorted(ev_.ISSUER_V2_KEYS))
                    self.assertEqual(receipt.sha256, result.receipt_sha256)
                    self.assertEqual(receipt.value['workflow_scope_digest'], sha(request))
                    self.assertEqual(receipt.value['issuer']['review_comment_sha256'],
                                     sha(('APPROVE sha256:' + sha(request)).encode('utf-8')))
                    self.assertEqual((receipt.value['issuer']['actor_id'], receipt.value['issuer']['reviewer_id']),
                                     (founder, founder))
                    projection = ev_.parse_provider_evidence(result.projection_bytes)
                    self.assertEqual(sorted(projection.value), sorted(ev_.PROJECTION_V2_KEYS))
                    self.assertEqual([r['kind'] for r in projection.value['audit']['source_responses']],
                                     list(SOURCE_KINDS))
                    self.assertEqual(receipt.value['issuer']['stable_identity_sha256'],
                                     sha(canon(projection.value['identity'])))
                    self.assertEqual(result.request_bytes, request)
                    self.assertEqual(sorted(result.locator), sorted(self.elig.APPROVAL_RUN_KEYS))
                    self.assertEqual(result.locator, {'receipt_sha256': receipt.sha256, 'run_id': APPROVAL_RUN,
                                                      'artifact_id': APPROVAL_ARTIFACT})
                    # The locator is the release caller's existing `approvals` entry.
                    call = self.elig.parse_request(
                        {'version': VERSION, 'candidate_run_id': 1, 'candidate_run_attempt': 1,
                         'candidate_artifact_id': 2, 'evidence_run_id': 3, 'evidence_run_attempt': 1,
                         'evidence_artifact_id': 4, 'evidence_sha256': digest(5), 'dispatch_run_id': 6,
                         'dispatch_run_attempt': 1, 'approvals': [result.locator]})
                    # The unchanged consumer rebuilds the same receipt from the same facts on first use.
                    wrapped, main, policy, authority = self.consumer(world)
                    card = self.card(kind, receipt.sha256)
                    self.assertEqual(ev_.card_receipt_sha256(card, authority), receipt.sha256)
                    qualified, rebuilt = self.elig._first_use(wrapped, main, policy, authority, call.approvals[0],
                                                              card, receipt.sha256)
                    self.assertEqual((qualified.receipt_bytes, qualified.request_bytes, qualified.projection_bytes),
                                     (result.receipt_bytes, request, result.projection_bytes))
                    self.assertEqual(qualified.source, 'first_use')
                    self.elig._card_scope(kind, rebuilt, scope_for(kind))
                    if kind == 'revocation':
                        # A revocation receipt is a valid record, and it never qualifies a release.
                        self.refuses(self.elig._card_authority, card, authority, authority,
                                     message='not authorized: a revocation card never permits signing')
                    else:
                        self.assertEqual(self.elig._card_authority(card, authority, authority), receipt.sha256)
        # No source change: the founder is enrollment data, so the other enrolled founder needs no constant.
        source = (PRODUCERS / 'approval.py').read_text(encoding='utf-8') + \
            (PRODUCERS / 'common.py').read_text(encoding='utf-8')
        for founder in (str(FOUNDER), str(OTHER_FOUNDER), '32169767'):
            self.assertNotIn(founder, source)

    def test_rprd2_custody_limit_same_account(self):
        # Custody limit (founder ruling 2026-11-08): when one agent holds the founder's credential and both
        # dispatches and reviews, every fact is the enrolled account's, so the producer accepts. This proves
        # account use, not that a person reviewed.
        world, request, result = self.produce('key_change')
        issuer = self.approval.ev.parse_approval_receipt(result.receipt_bytes).value['issuer']
        self.assertEqual({issuer['actor_id'], issuer['triggering_actor_id'], issuer['reviewer_id']}, {FOUNDER})

    def test_rprd2_review_and_request_are_exact(self):
        approval = self.approval
        request = approval_request('key_change')
        self.assertEqual(approval.required_comment(request), 'APPROVE sha256:' + sha(request))

        def accept_with(review_changes=None, reviews=None, body=request):
            world = ApprovalWorld()
            world.reviewed(request)
            if reviews is not None:
                world.reviews[APPROVAL_RUN] = reviews
            elif review_changes:
                world.reviews[APPROVAL_RUN] = [world.review(request, **review_changes)]
            return approval.accept(self.facts(world), APPROVAL_RUN, 1, self.inputs(world, body))

        self.assertEqual(accept_with(), request)
        comment = 'APPROVE sha256:' + sha(request)
        for wrong in (comment + '\n', comment + ' ok', ' ' + comment, comment.upper(),
                      'APPROVE sha256:' + sha(request[:-1]), 'APPROVE ' + sha(request), ''):
            with self.subTest(comment=wrong):
                self.refuses(accept_with, {'comment': wrong},
                             message='the founder review does not approve this request')
        # The input must be the exact canonical request bytes the comment names.
        for body in (request[:-1] + b'\n\n', request.replace(b'"schema":1', b'"schema": 1'),
                     request.replace(b'"kind":"key_change"', b'"kind":"key_change" ')):
            with self.subTest(body=body[:40]):
                self.refuses(accept_with, None, None, body)
        # A different request that the founder did not approve.
        other = approval_request('new_platform')
        self.refuses(accept_with, None, None, other, message='the founder review does not approve this request')
        # The summary is escaped text: no input renders as HTML or Markdown.
        world = ApprovalWorld()
        text = approval.prepare(self.facts(world), APPROVAL_RUN, 1, self.inputs(world, request))
        self.assertTrue(text.startswith('## Founder approval request\n\n<pre>\n'))
        self.assertTrue(text.endswith('\n</pre>\n'))
        inner = text[len('## Founder approval request\n\n<pre>\n'):-len('\n</pre>\n')]
        for raw in ('<', '>', '`', '"'):
            self.assertNotIn(raw, inner)
        self.assertIn('policy commit: ' + POLICY, inner)
        self.assertIn('request sha256: ' + sha(request), inner)
        self.assertIn('kind: key_change', inner)

    def test_rprd2_authority_refusals(self):
        approval = self.approval
        request = approval_request('key_change')

        def accept_after(change):
            world = ApprovalWorld()
            world.reviewed(request)
            change(world)
            return approval.accept(self.facts(world), APPROVAL_RUN, world.attempt, self.inputs(world, request))

        def receipt_after(change):
            world, _, _ = self.produce('key_change')
            change(world)
            return approval.receipt(self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'key_change', POLICY)

        def run(**changes):
            return lambda w: w.runs[(APPROVAL_RUN, 1)].update(changes)

        def review(**changes):
            return lambda w: w.reviews.__setitem__(APPROVAL_RUN, [w.review(request, **changes)])

        def rerun(w):
            w.runs[(APPROVAL_RUN, 2)] = dict(w.runs[(APPROVAL_RUN, 1)], run_attempt=2)
            w.jobs[(APPROVAL_RUN, 2)] = [dict(j, run_attempt=2) for j in w.jobs[(APPROVAL_RUN, 1)]]
            w.attempt = 2

        def jobs(*entries):
            return lambda w: w.jobs.__setitem__((APPROVAL_RUN, 1), [ApprovalWorld.job(*e) for e in entries])

        def forged_workflow(w):
            w.files[(APPROVAL_WF, HEAD)] = b'name: release-approval\nrun: forged\n'

        stranger_env = ((ENVIRONMENT_ID + 1, 'release-exception-approval'),)
        accepting = (
            ('job: preparation failed', jobs((PREPARE_JOB, 'prepare-approval', 'completed', 'failure'),
                                             (ACCEPT_JOB, 'accept-founder-approval', 'in_progress', None))),
            ('job: preparation missing', jobs((ACCEPT_JOB, 'accept-founder-approval', 'in_progress', None))),
            ('job: an extra job', jobs((PREPARE_JOB, 'prepare-approval', 'completed', 'success'),
                                       (ACCEPT_JOB, 'accept-founder-approval', 'in_progress', None),
                                       (ACCEPT_JOB + 9, 'upload', 'in_progress', None))),
            ('job: renamed acceptance', jobs((PREPARE_JOB, 'prepare-approval', 'completed', 'success'),
                                             (ACCEPT_JOB, 'accept', 'in_progress', None))),
            ('environment id', review(environments=stranger_env)),
            ('environment name', review(environments=((ENVIRONMENT_ID, 'release'),))),
            ('two environments', review(environments=((ENVIRONMENT_ID, 'release-exception-approval'),
                                                      (ENVIRONMENT_ID + 1, 'release')))),
            ('reviewer', review(user_id=STRANGER)),
            ('reviewer type', review(user_type='Bot')),
            ('rejected', review(state='rejected')),
            ('comment', review(comment='APPROVE sha256:' + sha(request) + '\n')),
            ('zero reviews', lambda w: w.reviews.__setitem__(APPROVAL_RUN, [])),
            ('two reviews', lambda w: w.reviews.__setitem__(APPROVAL_RUN, [w.review(request), w.review(request)])),
            ('workflow bytes', forged_workflow),
            ('implementation pin', lambda w: w.files.__setitem__((APPROVAL_WF, P), b'name: other\n')),
            ('main ref', run(head_branch='feature')),
            ('no branch', run(head_branch=None)),
            ('head off main', run(head_sha=OTHER_COMMIT)),
            ('fork', run(head_repository_id=FORK_ID)),
            ('repository', run(repository_id=FORK_ID)),
            ('dispatcher', run(actor_id=STRANGER)),
            ('trigger', run(triggering_actor_id=STRANGER)),
            ('event', run(event='push')),
            ('workflow id', run(workflow_id=APPROVAL_WF_ID + 1)),
            ('attempt', rerun),
            ('channel disabled', lambda w: w.files.__setitem__(
                ('release-keys/evidence-policy.json', HEAD),
                w.policy.replace(b'"enabled":true', b'"enabled":false'))),
        )
        for name, change in accepting:
            with self.subTest(stage='accept', case=name):
                self.refuses(accept_after, change)
        receiving = (
            ('job', lambda w: w.jobs[(APPROVAL_RUN, 1)][1].update(name='accept')),
            ('environment', review(environments=stranger_env)),
            ('reviewer', review(user_id=STRANGER)),
            ('comment', review(comment='APPROVE sha256:' + sha(request) + '\n')),
            ('zero reviews', lambda w: w.reviews.__setitem__(APPROVAL_RUN, [])),
            ('two reviews', lambda w: w.reviews.__setitem__(APPROVAL_RUN, [w.review(request), w.review(request)])),
            ('workflow bytes', forged_workflow),
            ('main ref', run(head_branch='feature')),
            ('fork', run(head_repository_id=FORK_ID)),
            ('dispatcher', run(actor_id=STRANGER)),
            ('trigger', run(triggering_actor_id=STRANGER)),
            ('not completed', run(status='in_progress', conclusion=None)),
            ('failed', run(conclusion='failure')),
            ('artifact name', lambda w: w.artifacts[APPROVAL_ARTIFACT].update(
                name='approval-request-' + str(APPROVAL_RUN) + '-2')),
            ('expired artifact', lambda w: w.artifacts[APPROVAL_ARTIFACT].update(expired=True)),
            ('artifact head', lambda w: w.artifacts[APPROVAL_ARTIFACT].update(head_sha=OTHER_COMMIT)),
            ('changed bytes', lambda w: w.members.__setitem__((APPROVAL_ARTIFACT, 'approval-request.json'),
                                                              approval_request('new_platform'))),
        )
        for name, change in receiving:
            with self.subTest(stage='receipt', case=name):
                self.refuses(receipt_after, change)
        # The receipt names the kind its caller asked for, and the policy commit its request names.
        world, _, _ = self.produce('key_change')
        self.refuses(approval.receipt, self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'new_platform', POLICY,
                     message='the approval run approved a key_change request, not new_platform')
        self.refuses(approval.receipt, self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'key_change', P)
        world.files[('release-keys/evidence-policy.json', P)] = world.policy
        self.refuses(approval.receipt, self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'key_change', P,
                     message='the approval request names another policy commit')
        self.refuses(approval.receipt, self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'key_change', OTHER_COMMIT)
        self.refuses(approval.receipt, self.facts(world), APPROVAL_RUN, APPROVAL_ARTIFACT, 'release', POLICY)
        self.assertEqual(world.blocked, [])
        # Oversize input refuses before JSON parsing; a shell payload stays data and never runs.
        world = ApprovalWorld()
        world.reviewed(request)
        self.refuses(approval.accept, self.facts(world), APPROVAL_RUN, 1,
                     {'request_body': ' ' * 60000 + '\n'}, message='request_body is larger than 60000 bytes')
        self.refuses(approval.prepare, self.facts(world), APPROVAL_RUN, 1,
                     {'request_body': ' ' * 60000 + '\n'}, message='request_body is larger than 60000 bytes')
        marker = Path(self.scratch()) / 'owned'
        for payload in ('$(touch ' + str(marker) + ')\n', '`touch ' + str(marker) + '`\n',
                        '"; touch ' + str(marker) + '; echo "\n', "'; touch " + str(marker) + "; '\n"):
            with self.subTest(payload=payload):
                inputs = self.inputs(world, payload)
                self.assertEqual(inputs['request_body'], payload)
                self.refuses(approval.prepare, self.facts(world), APPROVAL_RUN, 1, inputs)
                self.refuses(approval.accept, self.facts(world), APPROVAL_RUN, 1, inputs)
                self.assertFalse(marker.exists())
        self.assertEqual(world.blocked, [])

    def test_rprd2_cli_writes_only_records(self):
        # The command line runs through `--trusted` with isolated Python and refuses without credentials or
        # a run identity; it never echoes the input.
        approval_py = str(PRODUCERS / 'approval.py')
        env = {'PATH': '/usr/bin:/bin', 'GITHUB_RUN_ID': str(APPROVAL_RUN), 'GITHUB_RUN_ATTEMPT': '1'}
        hostile = '$(id) SECRET-INPUT-TEXT\n'
        world = ApprovalWorld()
        out = Path(self.scratch()) / 'event.json'
        out.write_bytes(world.event(hostile))
        done = subprocess.run([sys.executable, '-I', '-B', approval_py, '--trusted', str(ROOT), 'accept',
                               '--event', str(out), '--out', str(out.parent / 'out')],
                              capture_output=True, text=True, env=env)
        self.assertEqual(done.returncode, 1)
        self.assertEqual(done.stderr, 'release producer: the step has no GITHUB_TOKEN\n')
        self.assertNotIn('SECRET-INPUT-TEXT', done.stdout + done.stderr)
        self.assertFalse((out.parent / 'out').exists())
        missing = subprocess.run([sys.executable, '-I', '-B', approval_py, 'accept'], capture_output=True, text=True,
                                 env=env)
        self.assertEqual(missing.returncode, 2)
        request_file = out.parent / 'request.json'
        request = approval_request('key_change')
        request_file.write_bytes(request)
        dispatch = subprocess.run([sys.executable, '-I', '-B', approval_py, '--trusted', str(ROOT), 'dispatch',
                                   '--request', str(request_file)], capture_output=True, env=env)
        self.assertEqual(dispatch.returncode, 0, dispatch.stderr)
        self.assertEqual(dispatch.stdout, canon({'inputs': {'request_body': request.decode('utf-8')},
                                                 'ref': 'refs/heads/main'}))
        # Records go to a new private folder, never over an existing file.
        folder = out.parent / 'records'
        self.common.write_record(folder, 'approval-request.json', request)
        self.assertEqual((folder / 'approval-request.json').read_bytes(), request)
        self.refuses(self.common.write_record, folder, 'approval-request.json', request)

    def scratch(self):
        import tempfile
        folder = tempfile.mkdtemp(prefix='rdy-rprd2-')
        self.addCleanup(__import__('shutil').rmtree, folder, True)
        return folder


if __name__ == '__main__':
    unittest.main()
