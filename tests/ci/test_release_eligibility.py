"""Authenticated release eligibility and recovery facts, with fake fact sources only.

No test reaches the network, GitHub, AWS, a key or a signature. Every identity below is synthetic.
Each forged case keeps every other fact valid and every digest consistent: E is re-sealed and the
request names its new digest, so only the forged authority or binding can cause the refusal.
"""
import copy
import dataclasses
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/release' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # the release scripts import each other: one module, one error class
    spec.loader.exec_module(module)
    return module


CANDIDATE = load('candidate')
load('rehearse')
EVIDENCE = load('evidence')
GITHUB = load('github')
ELIGIBILITY = load('eligibility')
ControllerError = CANDIDATE.ControllerError
Refused = ELIGIBILITY.Refused

MAC = 'aarch64-apple-darwin'
LINUX = 'x86_64-unknown-linux-gnu'
REPO = 'adrenickchrome/rdy-core'
REPO_ID = 1403751388
ACTIONS = 'https://github.com/' + REPO + '/actions/runs/'
HEAD = 'c' * 40     # main's head
P = '3' * 40        # the policy commit
S = '1' * 40        # the release commit
H = '2' * 40        # the reviewed signer commit
BASE = 'b' * 40
OTHER = '9' * 40
RESERVE_TAG = 'd' * 40
# Synthetic identities: never production values.
ACCOUNT = '111122223333'
KEY_ARN = 'arn:aws:kms:us-east-2:' + ACCOUNT + ':key/00000000-0000-4000-8000-000000000001'
NEW_KEY_ARN = 'arn:aws:kms:us-east-2:' + ACCOUNT + ':key/00000000-0000-4000-8000-000000000002'
ROLE_ARN = 'arn:aws:iam::' + ACCOUNT + ':role/rdy-test-signer'
FINGERPRINT = 'SHA256:' + 'A' * 43
FOUNDER = 4242
PERSON = 5001       # a coordinator who is a person
APP = 5002          # a coordinator that is an App
ATTACKER = 6666
LANDING = 'f' * 64
CAND_RUN, EVID_RUN, CI_RUN, SEC_RUN, DISPATCH_RUN = 777001, 888001, 880001, 880002, 990001
CAND_ART, EVID_ART = 55, 8901
APPROVAL_WF = '.github/workflows/release-approval.yml'
EVIDENCE_WF = '.github/workflows/release-evidence.yml'
LINEAGE = {'table_id': '01234567-89ab-4cde-8f01-23456789abcd', 'genesis_sha256': format(18, '064x'),
           'authority_sha256': format(17, '064x')}
SOURCE_KINDS = ('artifact', 'implementation_source', 'job', 'protected_policy', 'repository', 'request_archive',
                'review_history', 'run_attempt', 'workflow_source')
SECURITY = {'CodeQL (actions)': SEC_RUN, 'CodeQL (rust)': SEC_RUN, 'Core CI': CI_RUN, 'Core security': SEC_RUN,
            'Dependency, secret and workflow policy': SEC_RUN, 'Fast gate (Linux x86-64)': CI_RUN}
CHECKS = ('signature', 'checksums', 'install', 'version', 'daemon', 'mcp')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canon(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False) + '\n').encode('utf-8')


def digest(n):
    return format(n, '064x')


def archive(version, target):
    return {'name': 'rdy-' + version + '-preview-' + target + '.tar.gz', 'sha256': digest(10 + len(target)),
            'size': 1000 + len(target)}


def manifest_sha(files):
    return sha(''.join(f['sha256'] + '  ' + f['name'] + '\n' for f in files).encode())


def absent(operation):
    return GITHUB.Absent(operation, 'not found', 404)


class FakeFacts(object):
    """The read-only fact source over a World. It has no write method: a write attempt is recorded."""

    def __init__(self, world):
        self.w = world

    def __getattr__(self, name):
        if name.startswith('create') or name in ('download_artifact', 'archive', 'put_item'):
            def refuse(*args, **kwargs):
                self.w.mutations.append(name)
                raise AssertionError('eligibility tried to write: ' + name)
            return refuse
        raise AttributeError(name)

    def _hit(self, name, *args):
        self.w.calls.append((name,) + args)
        error = self.w.errors.get(name)
        if error is not None:
            raise error

    def repository(self):
        self._hit('repository')
        return GITHUB.RepositoryFact(REPO_ID, REPO, self.w.repository_sha)

    def run_attempt(self, run_id, attempt):
        self._hit('run_attempt', run_id, attempt)
        if (run_id, attempt) not in self.w.runs:
            raise absent('run attempt')
        return GITHUB.RunFact(**self.w.runs[(run_id, attempt)])

    def attempt_jobs(self, run_id, attempt):
        self._hit('attempt_jobs', run_id, attempt)
        if (run_id, attempt) not in self.w.runs:
            raise absent('jobs')
        return [GITHUB.JobFact(**j) for j in self.w.jobs.get((run_id, attempt), [])]

    def artifact(self, artifact_id, run_id):
        self._hit('artifact', artifact_id, run_id)
        if artifact_id not in self.w.artifacts:
            raise absent('artifact')
        fact = GITHUB.ArtifactFact(**self.w.artifacts[artifact_id])
        if fact.run_id != run_id:
            raise GITHUB.Malformed('artifact', 'the artifact is not from run ' + str(run_id))
        if fact.expired:
            raise GITHUB.ArtifactExpired('artifact', 'the artifact expired')
        return fact

    def artifact_member(self, fact, name, limit):
        self._hit('artifact_member', fact.artifact_id, name)
        data = self.w.members.get((fact.artifact_id, name))
        if data is None:
            raise ControllerError('the artifact does not hold ' + name)
        if len(data) > limit:
            raise ControllerError(name + ' is larger than ' + str(limit) + ' bytes')
        return data

    def ref(self, name):
        self._hit('ref', name)
        if name not in self.w.refs:
            raise absent('ref')
        kind, target = self.w.refs[name]
        return GITHUB.RefFact('refs/' + name, kind, target, digest(91))

    def tag(self, sha_):
        self._hit('tag', sha_)
        if sha_ not in self.w.tags:
            raise absent('tag')
        return GITHUB.TagFact(**self.w.tags[sha_])

    def commit(self, sha_):
        self._hit('commit', sha_)
        if sha_ not in self.w.commits:
            raise absent('commit')
        return GITHUB.CommitFact(sha_, '8' * 40, tuple(self.w.commits[sha_]), digest(92))

    def contents(self, path, ref_sha, limit=GITHUB.CONTENTS_MAX):
        self._hit('contents', path, ref_sha)
        data = self.w.files.get((path, ref_sha))
        if data is None:
            raise absent('contents')
        if len(data) > limit:
            raise GITHUB.Malformed('contents', 'the file is larger than ' + str(limit) + ' bytes')
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        return GITHUB.FileFact(path, ref_sha, blob, data, sha(data))

    def compare(self, base, head):
        self._hit('compare', base, head)
        status = self.w.compares.get((base, head), 'diverged')
        return GITHUB.CompareFact(base, head, status, 1, 0, base, digest(93))

    def releases(self):
        self._hit('releases')
        return [GITHUB.ReleaseFact(**r) for r in self.w.releases]

    def open_code_scanning_alerts(self, ref):
        self._hit('open_code_scanning_alerts', ref)
        return [GITHUB.AlertFact(**a) for a in self.w.alerts]

    def run_approvals(self, run_id):
        self._hit('run_approvals', run_id)
        return [GITHUB.ApprovalFact(**a) for a in self.w.reviews.get(run_id, [])]

    def approval_responses(self, run_id, artifact_id):
        self._hit('approval_responses', run_id, artifact_id)
        return copy.deepcopy(self.w.responses[run_id])

    def approval_record(self, receipt_sha256):
        self._hit('approval_record', receipt_sha256)
        return self.w.archive.get(receipt_sha256)

    def approval_lineage(self):
        self._hit('approval_lineage')
        return dict(self.w.lineage)

    def release_trust(self, version, policy_commit, signing):
        self._hit('release_trust', version, policy_commit, signing)
        return dict(self.w.trust, version=version)


class World(object):
    """One consistent release of `version` at S with every fact GitHub, the archive and the trust
    tool would hold. Tests change one fact and re-seal."""

    def __init__(self, version='0.43.0', published=('0.17.0', '0.41.0', '0.42.0'), first_tag='v0.42.0'):
        self.v = version
        self.mutations, self.calls, self.errors = [], [], {}
        self.files, self.runs, self.jobs, self.artifacts, self.members = {}, {}, {}, {}, {}
        self.tags, self.reviews, self.responses, self.archive = {}, {}, {}, {}
        self.lineage = dict(LINEAGE)
        self.repository_sha = digest(90)  # the repository response digest: it changes with every push
        self.alerts = [{'number': 1, 'state': 'open', 'tool': 'CodeQL', 'security_severity': 'low'}]
        self.commits = {HEAD: (P,), P: (S,), S: (H,), H: (BASE,), BASE: ()}
        self.compares = {(H, P): 'ahead'}
        self.refs = {'heads/main': ('commit', HEAD), 'tags/candidate/v' + version: ('commit', S)}
        self.releases = [{'release_id': 100 + i, 'tag_name': 'v' + p, 'target_commitish': digest(200 + i)[:40],
                          'draft': False, 'prerelease': True, 'assets': ()} for i, p in enumerate(published)]
        self.approval_runs = []
        self.next_run = 6001

        # Release sources at S.
        self.notes = ('# rdy ' + version + '\n\nRelease notes.\n').encode()
        self.guide = b'# Agent guide\n\nUse rdy.\n'
        self.put(S, 'Cargo.toml', ('[workspace]\nmembers = []\n\n[workspace.package]\nversion = "' + version
                                   + '"\n').encode())
        self.put(S, 'release/notes.md', self.notes)
        self.put(S, 'release/AGENTS.md', self.guide)
        self.put(S, 'release/CLAUDE.md', self.guide)
        self.source = {'schema': 1, 'version': version, 'design_landing': LANDING, 'notes_sha256': sha(self.notes),
                       'guide_sha256': sha(self.guide)}
        self.put(S, 'release/source.json', canon(self.source))
        self.evidence_wf = b'name: release-evidence\non: workflow_dispatch\n'
        self.put(S, EVIDENCE_WF, self.evidence_wf)
        self.put(H, EVIDENCE_WF, self.evidence_wf)
        self.approval_wf = b'name: release-approval\non: workflow_dispatch\n'
        self.put(HEAD, APPROVAL_WF, self.approval_wf)

        # Protected policy at P, unchanged at main's head.
        self.records = b'transition-v0.42.0.json\ntransition-v0.42.0.json.sig\n'
        self.ep = {'schema': 1, 'repository_id': REPO_ID,
                   'coordinators': [{'actor_id': PERSON, 'app_id': None, 'installation_id': None},
                                    {'actor_id': APP, 'app_id': 61, 'installation_id': 71}],
                   'evidence_workflow': {'id': 81, 'path': EVIDENCE_WF, 'sha': H},
                   'release_workflow_path': '.github/workflows/release.yml',
                   'approval_channel': {
                       'schema': 1, 'repository': REPO, 'repository_id': REPO_ID, 'founder_user_id': FOUNDER,
                       'workflow_id': 31, 'workflow_path': APPROVAL_WF, 'implementation_sha': H,
                       'workflow_file_sha256': sha(self.approval_wf), 'dispatch_ref': 'refs/heads/main',
                       'required_job_name': 'accept-founder-approval', 'environment_id': 41,
                       'environment_name': 'release-exception-approval', 'prevent_self_review': False,
                       'can_admins_bypass': False,
                       'archive': {'provider': 'dynamodb', 'account_id': ACCOUNT, 'region': 'us-east-2',
                                   'table': 'rdy-test-release-control', 'pk_prefix': 'APPROVAL#', 'sk': 'RECORD'},
                       'enabled': True, 'revoked_receipts': []}}
        self.policy = {'schema': 1, 'repository': REPO, 'key_arn': KEY_ARN, 'role_arn': ROLE_ARN,
                       'fingerprint': FINGERPRINT, 'first_machine_tag': first_tag, 'targets': [MAC, LINUX],
                       'records_sha256': sha(self.records), 'evidence_policy_sha256': '', 'signer_workflow_sha': H}
        self.publish_policy(P)
        self.publish_policy(HEAD)
        self.trust = {'version': version, 'fingerprint': FINGERPRINT,
                      'signer_line': 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAtest',
                      'transition_sha256': digest(7), 'policy_commit': P}

        # The candidate run and its artifact.
        self.archives = [archive(version, MAC), archive(version, LINUX)]
        self.manifest = manifest_sha(self.archives)
        self.run(CAND_RUN, 1, '.github/workflows/release-candidate.yml', 'push', 71)
        self.job(CAND_RUN, 1, 7701, 'Candidate source and version')
        native = {MAC: self.job(CAND_RUN, 1, 7702, 'Native candidate (' + MAC + ')'),
                  LINUX: self.job(CAND_RUN, 1, 7703, 'Native candidate (' + LINUX + ')')}
        self.job(CAND_RUN, 1, 7704, 'Candidate manifest')
        self.artifacts[CAND_ART] = self.artifact(CAND_ART, 'rdy-candidate-a1', CAND_RUN, digest(5), 4096)
        counts = {'passed': 900, 'failed': 0, 'ignored': 3, 'prerequisite_skipped': 0}
        self.candidate = {
            'schema': 1, 'repository': REPO, 'ref': 'refs/tags/candidate/v' + version, 'version': version,
            'commit': S, 'run_id': CAND_RUN, 'run_attempt': 1, 'manifest_sha256': self.manifest,
            'artifacts': copy.deepcopy(self.archives),
            'builds': [{'target': t, 'artifact': {'id': 100 + i, 'name': 'rdy-candidate-' + t + '-a1',
                                                  'digest': 'sha256:' + digest(60 + i), 'size': 2048},
                        'receipt_sha256': digest(70 + i), 'fast_gate': dict(counts),
                        'sources': [{'name': 'rdy-mpl-0.1.0.crate', 'sha256': digest(80), 'size': 10}]}
                       for i, t in enumerate((MAC, LINUX))]}
        self.members[(CAND_ART, 'candidate.json')] = canon(self.candidate)

        # The evidence run: the coordinator's sealing run, with the native install checks.
        self.run(EVID_RUN, 1, EVIDENCE_WF, 'workflow_dispatch', 81)
        self.job(EVID_RUN, 1, 8801, 'Seal release record')
        install = {MAC: self.job(EVID_RUN, 1, 8802, 'Install check (' + MAC + ')'),
                   LINUX: self.job(EVID_RUN, 1, 8803, 'Install check (' + LINUX + ')')}
        self.artifacts[EVID_ART] = self.artifact(EVID_ART, 'rdy-release-record-a1', EVID_RUN, digest(6), 2048)

        # The security and CI runs on S.
        self.run(CI_RUN, 1, '.github/workflows/ci.yml', 'push', 91, branch='main')
        self.run(SEC_RUN, 1, '.github/workflows/security.yml', 'push', 92, branch='main')
        security = []
        for n, name in enumerate(sorted(SECURITY)):
            security.append({'name': name, 'job': self.job(SECURITY[name], 1, 9901 + n, name)})

        # The current release dispatch, by the App coordinator.
        self.run(DISPATCH_RUN, 1, '.github/workflows/release.yml', 'workflow_dispatch', 101, actor=APP,
                 status='in_progress', conclusion=None)

        targets = []
        for n, (target, file) in enumerate(zip((MAC, LINUX), self.archives)):
            targets.append({'target': target, 'archive': copy.deepcopy(file), 'build': dict(native[target]),
                            'package': dict(native[target]),
                            'fast_gate': dict(counts, job=dict(native[target])),
                            'install': {'job': dict(install[target]), 'report_sha256': digest(30 + n),
                                        'checks': [{'name': c, 'passed': True} for c in CHECKS], 'skipped': 0}})
        self.e = {
            'schema': 1, 'repository': REPO, 'version': version, 'commit': S, 'design_landing': LANDING,
            'source': {'notes_sha256': sha(self.notes), 'guide_sha256': sha(self.guide),
                       'source_json_sha256': sha(canon(self.source)), 'export_receipt_sha256': digest(4)},
            'candidate': {'ref': 'refs/tags/candidate/v' + version, 'run_id': CAND_RUN, 'run_attempt': 1,
                          'artifact': {'id': CAND_ART, 'name': 'rdy-candidate-a1', 'digest': 'sha256:' + digest(5),
                                       'size': 4096},
                          'manifest_sha256': self.manifest, 'artifacts': copy.deepcopy(self.archives)},
            'targets': targets,
            'security': {'jobs': security, 'codeql_open_high_critical': 0,
                         'advisory_fetch': {'tool': 'cargo-deny 0.18.3', 'fetched_at': '2026-11-05T10:00:00Z',
                                            'log_sha256': digest(8), 'result': 'clean'}},
            'audit': {'reference': 'audit/rdy-' + version, 'sha256': digest(9)},
            'tutorials': [{'target': t, 'agent': 'claude-code', 'model': 'claude-sonnet-test', 'effort': 'medium',
                           'session': 'session-' + t, 'tutorial_revision': 'rev-1',
                           'steps': [{'id': 'install', 'result': 'pass'}, {'id': 'first-use', 'result': 'pass'}],
                           'archive_sha256': f['sha256'], 'manifest_sha256': self.manifest, 'result': 'pass'}
                          for t, f in zip((MAC, LINUX), self.archives)],
            'approval_basis': {'mode': 'standing_preview', 'decision': 'D-PZKAP', 'policy_commit': P,
                               'scope': self.full_scope(), 'cards': []},
        }
        self.seal()

    # -- building blocks ---------------------------------------------------------------------

    def put(self, commit, path, data):
        self.files[(path, commit)] = data

    def publish_policy(self, commit):
        ep = canon(self.ep)
        self.policy['evidence_policy_sha256'] = sha(ep)
        self.put(commit, 'release-keys/evidence-policy.json', ep)
        self.put(commit, 'release-keys/policy.json', canon(self.policy))
        self.put(commit, 'release-keys/records.txt', self.records)

    def run(self, run_id, attempt, path, event, workflow_id, actor=PERSON, trigger=None, head=S, branch=None,
            status='completed', conclusion='success'):
        self.runs[(run_id, attempt)] = dict(
            run_id=run_id, run_attempt=attempt, workflow_id=workflow_id, path=path, head_sha=head,
            head_branch=branch or 'candidate/v' + self.v, event=event, status=status, conclusion=conclusion,
            actor_id=actor, triggering_actor_id=trigger or actor, repository_id=REPO_ID, head_repository_id=REPO_ID,
            html_url=ACTIONS + str(run_id), sha256=digest(run_id % 997 + 300))

    def job(self, run_id, attempt, job_id, name, head=S, conclusion='success'):
        url = ACTIONS + str(run_id) + '/job/' + str(job_id)
        self.jobs.setdefault((run_id, attempt), []).append(dict(
            job_id=job_id, run_id=run_id, run_attempt=attempt, name=name, status='completed', conclusion=conclusion,
            html_url=url, head_sha=head))
        return {'run_id': run_id, 'run_attempt': attempt, 'job_id': job_id, 'job_url': url, 'conclusion': 'success'}

    def find_job(self, job_id):
        for jobs in self.jobs.values():
            for entry in jobs:
                if entry['job_id'] == job_id:
                    return entry
        raise KeyError(job_id)

    def artifact(self, artifact_id, name, run_id, zip_digest, size, head=S):
        return dict(artifact_id=artifact_id, name=name, size=size, digest=zip_digest, expired=False, run_id=run_id,
                    repository_id=REPO_ID, head_repository_id=REPO_ID, head_sha=head,
                    sha256=digest(artifact_id % 997 + 400))

    def full_scope(self):
        return {'repository': REPO, 'version': self.v, 'commit': S, 'design_landing': LANDING,
                'manifest_sha256': self.manifest, 'artifacts': copy.deepcopy(self.archives), 'key_arn': KEY_ARN,
                'fingerprint': FINGERPRINT, 'first_machine_tag': self.policy['first_machine_tag'],
                'transition_sha256': self.trust['transition_sha256'], 'targets': [MAC, LINUX]}

    def scope_for(self, kind):
        full = self.full_scope()
        if kind == 'release_1':
            return full
        if kind == 'key_change':
            return {k: full[k] for k in ('repository', 'key_arn', 'fingerprint', 'first_machine_tag',
                                         'transition_sha256', 'targets')}
        if kind == 'revocation':
            return {'repository': REPO, 'key_arn': KEY_ARN, 'fingerprint': FINGERPRINT,
                    'revocation_sha256': digest(14), 'last_good_tag': None}
        return {'repository': REPO, 'targets': [MAC, LINUX]}

    def seal(self):
        """Seals E: its exact bytes go into the evidence artifact and the request names their digest."""
        data = canon(self.e)
        self.members[(EVID_ART, 'release-record.json')] = data
        self.evidence_sha = sha(data)
        return data

    def request(self, **changes):
        value = {'version': self.v, 'candidate_run_id': CAND_RUN, 'candidate_run_attempt': 1,
                 'candidate_artifact_id': CAND_ART, 'evidence_run_id': EVID_RUN, 'evidence_run_attempt': 1,
                 'evidence_artifact_id': EVID_ART, 'evidence_sha256': self.evidence_sha,
                 'dispatch_run_id': DISPATCH_RUN, 'dispatch_run_attempt': 1,
                 'approvals': copy.deepcopy(self.approval_runs)}
        value.update(changes)
        return ELIGIBILITY.parse_request(value)

    def facts(self):
        return FakeFacts(self)

    def expected_tuple(self):
        return {'schema': 1, 'repository': REPO, 'version': self.v, 'commit': S,
                'release_evidence_sha256': self.evidence_sha, 'manifest_sha256': self.manifest,
                'artifacts': copy.deepcopy(self.archives),
                'signer': {'key_arn': self.policy['key_arn'], 'fingerprint': FINGERPRINT,
                           'first_machine_tag': self.policy['first_machine_tag'],
                           'transition_sha256': self.trust['transition_sha256'], 'policy_commit': P,
                           'signer_workflow_sha': H}}

    def reserve(self, tuple_value=None):
        """The reservation: refs/tags/reserve/v<V> -> annotated tag -> S, message T."""
        self.refs['tags/reserve/v' + self.v] = ('tag', RESERVE_TAG)
        self.tags[RESERVE_TAG] = dict(sha=RESERVE_TAG, tag='reserve/v' + self.v,
                                      message_bytes=canon(tuple_value or self.expected_tuple()),
                                      object_type='commit', object_sha=S, sha256=digest(94))

    # -- founder cards ---------------------------------------------------------------------------

    def card(self, receipt_sha256, kind):
        return {'kind': kind, 'sha256': receipt_sha256,
                'reference': 'dynamodb://' + ACCOUNT + '/us-east-2/rdy-test-release-control/APPROVAL%23'
                             + receipt_sha256 + '/RECORD'}

    def add_card(self, kind, via='archive', scope=None, receipt_kind=None, schema=2):
        """Adds a founder card to E. `archive`: an accepted envelope in the archive. `first_use`: a
        live approval run whose receipt the eligibility step rebuilds. `schema` 2 is the successor
        record (a stable identity and an audit section); 1 is the frozen v1 record."""
        request = {'schema': 1, 'decision': 'D-PZKAP', 'policy_commit': P, 'kind': receipt_kind or kind,
                   'scope': scope or self.scope_for(receipt_kind or kind)}
        request_bytes = canon(request)
        run_id, job_id, artifact_id = self.next_run, self.next_run + 100, self.next_run + 200
        self.next_run += 1
        if via == 'first_use':
            self.run(run_id, 1, APPROVAL_WF, 'workflow_dispatch', 31, actor=FOUNDER, head=HEAD, branch='main')
            self.job(run_id, 1, job_id, 'accept-founder-approval', head=HEAD)
            self.artifacts[artifact_id] = self.artifact(artifact_id, 'approval-request-' + str(run_id) + '-1', run_id,
                                                        digest(15 + run_id), 512, head=HEAD)
            self.members[(artifact_id, 'approval-request.json')] = request_bytes
            self.reviews[run_id] = [{'state': 'approved', 'comment': 'APPROVE sha256:' + sha(request_bytes),
                                     'user_id': FOUNDER, 'user_type': 'User',
                                     'environments': ((41, 'release-exception-approval'),)}]
            art = self.artifacts[artifact_id]
            ep_bytes = self.files[('release-keys/evidence-policy.json', P)]
            facts = {'artifact': (art['sha256'], 700), 'implementation_source': (digest(21), 701),
                     'job': (digest(22), 702), 'protected_policy': (sha(ep_bytes), len(ep_bytes)),
                     'repository': (self.repository_sha, 703), 'request_archive': (art['digest'], art['size']),
                     'review_history': (digest(23), 705),
                     'run_attempt': (self.runs[(run_id, 1)]['sha256'], 706),
                     'workflow_source': (sha(self.approval_wf), len(self.approval_wf))}
            self.responses[run_id] = [{'kind': k, 'sha256': facts[k][0], 'bytes': facts[k][1]} for k in SOURCE_KINDS]
            head_sha, zip_sha = HEAD, art['digest']
        else:
            head_sha, zip_sha = '4' * 40, digest(15)
            self.responses[run_id] = [{'kind': k, 'sha256': digest(20 + i), 'bytes': 100 + i}
                                      for i, k in enumerate(SOURCE_KINDS)]
        facts = {
            'repository_id': REPO_ID, 'head_repository_id': REPO_ID, 'workflow_id': 31,
            'workflow_path': APPROVAL_WF, 'implementation_sha': H, 'workflow_file_sha256': sha(self.approval_wf),
            'run_id': run_id, 'run_attempt': 1, 'run_head_sha': head_sha, 'ref': 'refs/heads/main',
            'event': 'workflow_dispatch', 'status': 'completed', 'conclusion': 'success', 'actor_id': FOUNDER,
            'triggering_actor_id': FOUNDER,
            'job': {'id': job_id, 'name': 'accept-founder-approval', 'run_id': run_id, 'status': 'completed',
                    'conclusion': 'success'},
            'artifact': {'id': artifact_id, 'name': 'approval-request-' + str(run_id) + '-1', 'sha256': zip_sha,
                         'workflow_run_id': run_id, 'repository_id': REPO_ID, 'head_repository_id': REPO_ID,
                         'head_sha': head_sha, 'expired': False, 'request_sha256': sha(request_bytes)},
            'review': {'state': 'approved', 'comment': 'APPROVE sha256:' + sha(request_bytes), 'user_id': FOUNDER,
                       'user_type': 'User', 'environment_id': 41, 'environment_name': 'release-exception-approval'}}
        if schema == 1:
            projection_bytes = canon(dict(facts, schema=1, source_responses=self.responses[run_id]))
            binding = ('provider_evidence_sha256', sha(projection_bytes))
        else:
            identity = dict(facts, schema=2, decision='D-PZKAP', kind=request['kind'], policy_commit=P,
                            scope=copy.deepcopy(request['scope']))
            projection_bytes = canon({'schema': 2, 'identity': identity,
                                      'audit': {'source_responses': self.responses[run_id]}})
            binding = ('stable_identity_sha256', sha(canon(identity)))
        receipt = {'schema': schema, 'decision': 'D-PZKAP', 'kind': request['kind'], 'policy_commit': P,
                   'workflow_scope_digest': sha(request_bytes), 'scope': copy.deepcopy(request['scope']),
                   'issuer': {'repository_id': REPO_ID, 'workflow_id': 31, 'workflow_path': APPROVAL_WF,
                              'implementation_sha': H, 'run_id': run_id, 'run_attempt': 1, 'run_head_sha': head_sha,
                              'ref': 'refs/heads/main', 'actor_id': FOUNDER, 'triggering_actor_id': FOUNDER,
                              'job_id': job_id, 'job_name': 'accept-founder-approval', 'artifact_id': artifact_id,
                              'artifact_sha256': zip_sha, 'reviewer_id': FOUNDER, 'environment_id': 41,
                              'environment_name': 'release-exception-approval',
                              'review_comment_sha256': sha(('APPROVE sha256:' + sha(request_bytes)).encode()),
                              binding[0]: binding[1]}}
        receipt_bytes = canon(receipt)
        receipt_sha = sha(receipt_bytes)
        if via == 'first_use':
            self.approval_runs.append({'receipt_sha256': receipt_sha, 'run_id': run_id, 'artifact_id': artifact_id})
        else:
            self.archive[receipt_sha] = self.envelope(request_bytes, receipt_bytes, projection_bytes)
        self.e['approval_basis']['mode'] = 'founder_card'
        self.e['approval_basis']['cards'].append(self.card(receipt_sha, kind))
        self.seal()
        return receipt_sha

    def envelope(self, request_bytes, receipt_bytes, projection_bytes):
        """The archive record the signer writes at first acceptance, in the receipt's schema."""
        acceptance = {'schema': 1, 'receipt_sha256': sha(receipt_bytes), 'request_sha256': sha(request_bytes),
                      'provider_projection_sha256': sha(projection_bytes), 'signer_workflow_sha': H,
                      'run_id': 7001, 'run_attempt': 1, 'job_id': 7101, 'policy_commit': P,
                      'policy_sha256': sha(self.files[('release-keys/policy.json', P)]),
                      'authority_sha256': LINEAGE['authority_sha256'], 'table_id': LINEAGE['table_id'],
                      'genesis_sha256': LINEAGE['genesis_sha256'], 'accepted_at': '2026-11-05T10:00:00Z'}
        return canon({'schema': json.loads(receipt_bytes)['schema'], 'request_body': request_bytes.decode(),
                      'receipt_body': receipt_bytes.decode(), 'provider_projection_body': projection_bytes.decode(),
                      'acceptance': acceptance})


def stable_identity(projection_bytes):
    """The successor approval identity a schema 2 receipt binds: the SHA-256 of the canonical identity."""
    return sha(canon(json.loads(projection_bytes)['identity']))


def accept(world, receipt_sha256, **changes):
    """Changes the acceptance lineage of an archived envelope; its bodies and their joins stay valid."""
    envelope = json.loads(world.archive[receipt_sha256])
    envelope['acceptance'].update(changes)
    world.archive[receipt_sha256] = canon(envelope)


def first_machine_world():
    """Release 1: nothing after the historical range is published; V is the cutover version."""
    return World(version='0.42.0', published=('0.17.0', '0.35.0', '0.41.0'), first_tag='v0.42.0')


def key_change_world():
    """A later key: v0.42.0 shipped under the first machine key; the policy cuts over at v0.43.0."""
    w = World(version='0.43.0', published=('0.17.0', '0.41.0', '0.42.0'), first_tag='v0.43.0')
    w.policy['key_arn'] = NEW_KEY_ARN
    w.publish_policy(P)
    w.publish_policy(HEAD)
    w.e['approval_basis']['scope'] = w.full_scope()
    w.e['approval_basis']['scope']['key_arn'] = NEW_KEY_ARN
    w.seal()
    return w


class Base(unittest.TestCase):
    def refuses(self, world, message, recovery=False, request=None):
        call = ELIGIBILITY.authenticate_recovery if recovery else ELIGIBILITY.qualify_new
        with self.assertRaises(Refused) as caught:
            call(request or world.request(), world.facts())
        self.assertEqual(str(caught.exception), message)
        self.assertIsInstance(caught.exception, ControllerError)
        self.assertEqual(world.mutations, [])
        return caught.exception

    def qualifies(self, world, recovery=False, request=None):
        call = ELIGIBILITY.authenticate_recovery if recovery else ELIGIBILITY.qualify_new
        result = call(request or world.request(), world.facts())
        self.assertEqual(world.mutations, [])
        return result


class Eligibility(Base):
    def test_rsig5_all_eligibility_facts_are_required(self):
        w = World()
        q = self.qualifies(w)
        # T is rebuilt exactly from authenticated facts, and every T field is in the result.
        self.assertEqual(q.mode, 'new')
        self.assertEqual(q.tuple_bytes, canon(w.expected_tuple()))
        self.assertEqual(q.tuple_sha256, sha(q.tuple_bytes))
        self.assertEqual(q.tuple_value(), w.expected_tuple())
        rebuilt = {'schema': 1, 'repository': REPO, 'version': q.version, 'commit': q.commit,
                   'release_evidence_sha256': q.evidence_sha256, 'manifest_sha256': q.manifest_sha256,
                   'artifacts': [{'name': n, 'sha256': d, 'size': s} for n, d, s in q.artifacts],
                   'signer': {'key_arn': q.key_arn, 'fingerprint': q.fingerprint,
                              'first_machine_tag': q.first_machine_tag, 'transition_sha256': q.transition_sha256,
                              'policy_commit': q.policy_commit, 'signer_workflow_sha': q.signer_workflow_sha}}
        self.assertEqual(EVIDENCE.tuple_bytes(rebuilt), q.tuple_bytes)
        self.assertEqual((q.evidence_sha256, q.evidence_bytes), (w.evidence_sha, w.seal()))
        self.assertEqual(q.dispatcher, (DISPATCH_RUN, 1, APP, APP))
        self.assertEqual(q.evidence_run, (EVID_RUN, 1, EVID_ART))
        self.assertEqual(q.candidate_run, (CAND_RUN, 1, CAND_ART, digest(5)))
        self.assertEqual((q.approval_mode, q.cards, q.reservation, q.publication), ('standing_preview', (), None, None))
        # Permitted ignored fast-suite tests are recorded apart from the install runs.
        self.assertEqual(q.fast_gate_ignored, ((MAC, 3), (LINUX, 3)))
        self.assertEqual(q.install, ((MAC, 8802, digest(30)), (LINUX, 8803, digest(31))))
        # Immutable: no field can change.
        with self.assertRaises(dataclasses.FrozenInstanceError):
            q.commit = OTHER
        # It read authority from main, never wrote, and asked the trust tool for a new signing.
        self.assertIn(('release_trust', '0.43.0', P, True), w.calls)
        self.assertIn(('open_code_scanning_alerts', 'refs/heads/main'), w.calls)

        # Every required fact is read; missing or unavailable facts refuse.
        missing = (
            (lambda w: w.runs.pop((EVID_RUN, 1)), 'eligibility incomplete: GitHub run attempt: not found'),
            (lambda w: w.runs.pop((CAND_RUN, 1)), 'eligibility incomplete: GitHub run attempt: not found'),
            (lambda w: w.runs.pop((SEC_RUN, 1)), 'eligibility incomplete: GitHub run attempt: not found'),
            (lambda w: w.runs.pop((DISPATCH_RUN, 1)), 'eligibility incomplete: GitHub run attempt: not found'),
            (lambda w: w.artifacts.pop(CAND_ART), 'eligibility incomplete: GitHub artifact: not found'),
            (lambda w: w.artifacts.pop(EVID_ART), 'eligibility incomplete: GitHub artifact: not found'),
            (lambda w: w.members.pop((EVID_ART, 'release-record.json')),
             'eligibility incomplete: the artifact does not hold release-record.json'),
            (lambda w: w.members.pop((CAND_ART, 'candidate.json')),
             'eligibility incomplete: the artifact does not hold candidate.json'),
            (lambda w: w.refs.pop('tags/candidate/v0.43.0'), 'eligibility incomplete: GitHub ref: not found'),
            (lambda w: w.refs.pop('heads/main'), 'eligibility incomplete: GitHub ref: not found'),
            (lambda w: w.files.pop(('release/notes.md', S)), 'eligibility incomplete: GitHub contents: not found'),
            (lambda w: w.files.pop(('release-keys/policy.json', P)),
             'eligibility incomplete: GitHub contents: not found'),
            (lambda w: w.files.pop(('release-keys/evidence-policy.json', HEAD)),
             'eligibility incomplete: GitHub contents: not found'),
            (lambda w: w.jobs[(EVID_RUN, 1)].pop(1),
             'eligibility incomplete: the aarch64-apple-darwin install job is not a job of run 888001 attempt 1'),
            (lambda w: w.jobs[(SEC_RUN, 1)].pop(0),
             'eligibility incomplete: the CodeQL (actions) job is not a job of run 880002 attempt 1'),
            (lambda w: w.jobs[(CAND_RUN, 1)].pop(3),
             'eligibility incomplete: the candidate run has no single job Candidate manifest'),
            (lambda w: w.errors.update(release_trust=ControllerError('release trust: no key for v0.43.0')),
             'eligibility incomplete: release trust: no key for v0.43.0'),
            (lambda w: w.errors.update(releases=GITHUB.Uncertain('releases', 'outcome unknown (timeout)')),
             'eligibility incomplete: GitHub releases: outcome unknown (timeout)'),
            (lambda w: w.errors.update(attempt_jobs=GITHUB.Unauthorized('jobs', 'not authorized (HTTP 403)', 403)),
             'eligibility incomplete: GitHub jobs: not authorized (HTTP 403)'),
            (lambda w: w.runs[(EVID_RUN, 1)].update(conclusion='cancelled'),
             'eligibility incomplete: the evidence run did not conclude success'),
            (lambda w: w.find_job(7702).update(conclusion='failure'),
             'eligibility incomplete: a job of the candidate run did not conclude success'),
        )
        for change, message in missing:
            w = World()
            change(w)
            with self.subTest(message=message):
                self.refuses(w, message)
        # E must hold every field; a missing record refuses before anything else is trusted.
        for key in ('source', 'candidate', 'targets', 'security', 'audit', 'tutorials', 'approval_basis'):
            w = World()
            del w.e[key]
            w.seal()
            with self.subTest(field=key):
                self.refuses(w, "eligibility incomplete: missing field '" + key + "'")
        # Bounds refuse as a limit, never as dropped evidence.
        w = World()
        w.members[(EVID_ART, 'release-record.json')] = b' ' * (1024 * 1024 + 1)
        self.refuses(w, 'input limit exceeded: release-record.json is larger than 1048576 bytes')
        w = World()
        w.commits = {HEAD: (P,), P: ('e' * 40,)}
        sha_ = 'e' * 40
        for n in range(1100):
            nxt = format(n, '040x')
            w.commits[sha_] = (nxt,)
            sha_ = nxt
        self.refuses(w, "input limit exceeded: main's first-parent chain is longer than 1000 commits before the "
                        'commit it needs')
        # The request is closed and typed.
        w = World()
        value = {'version': '0.43.0', 'candidate_run_id': CAND_RUN, 'candidate_run_attempt': 1,
                 'candidate_artifact_id': CAND_ART, 'evidence_run_id': EVID_RUN, 'evidence_run_attempt': 1,
                 'evidence_artifact_id': EVID_ART, 'evidnce_sha256': w.evidence_sha, 'dispatch_run_id': DISPATCH_RUN,
                 'dispatch_run_attempt': 1, 'approvals': []}
        with self.assertRaises(Refused) as caught:
            ELIGIBILITY.parse_request(value)
        self.assertEqual(str(caught.exception),
                         "evidence mismatch: unknown field 'evidnce_sha256'; did you mean 'evidence_sha256'?")
        for changes, message in (({'evidence_run_attempt': True},
                                  'evidence mismatch: the request evidence run attempt is not a positive integer'),
                                 ({'version': '0.43.1'}, 'evidence mismatch: the request version is not a 0.N.0 preview'),
                                 ({'evidence_sha256': 'A' * 64},
                                  'evidence mismatch: the request evidence digest is not a 64-hex SHA-256')):
            with self.assertRaises(Refused) as caught:
                w.request(**changes)
            self.assertEqual(str(caught.exception), message)

    def test_rsig5_forged_or_wrong_commit_receipts_refuse(self):
        cases = []

        def case(message, change, reseal=True):
            cases.append((message, change, reseal))

        # E names another commit, and the run and source agree with it: the candidate ref still names S.
        case('evidence mismatch: the release record does not name v0.43.0 at the release commit',
             lambda w: w.e.update(commit=OTHER) or w.e['approval_basis']['scope'].update(commit=OTHER))
        # The evidence run, its artifact or the requested digest are from somewhere else.
        case('evidence mismatch: the evidence run did not run on the release commit',
             lambda w: w.runs[(EVID_RUN, 1)].update(head_sha=OTHER))
        case('eligibility incomplete: GitHub artifact: the artifact is not from run 888001',
             lambda w: w.artifacts[EVID_ART].update(run_id=EVID_RUN + 1))
        case('evidence mismatch: the release record artifact is not from the release commit',
             lambda w: w.artifacts[EVID_ART].update(head_sha=OTHER))
        case('evidence mismatch: the release record does not match its requested digest',
             lambda w: setattr(w, 'evidence_sha', digest(99)), reseal=False)
        # The candidate receipts: artifact identity, candidate.json and its counts.
        case('evidence mismatch: the candidate artifact is not the one the release record names',
             lambda w: w.artifacts[CAND_ART].update(digest=digest(98)))
        case('evidence mismatch: the candidate artifact is not from the release commit',
             lambda w: w.artifacts[CAND_ART].update(head_sha=OTHER))
        case('evidence mismatch: candidate.json does not name this candidate run at the release commit',
             lambda w: w.members.update({(CAND_ART, 'candidate.json'): canon(dict(w.candidate, commit=OTHER))}))
        forged = copy.deepcopy(World().candidate)
        forged['builds'][0]['fast_gate']['passed'] = 901
        case('evidence mismatch: the aarch64-apple-darwin fast gate counts are not its build receipt counts',
             lambda w: w.members.update({(CAND_ART, 'candidate.json'): canon(forged)}))
        case('evidence mismatch: the requested candidate is not the candidate of the release record',
             lambda w: w.e['candidate'].update(run_id=CAND_RUN + 1))
        case('evidence mismatch: the candidate run did not run on the release commit',
             lambda w: w.runs[(CAND_RUN, 1)].update(head_sha=OTHER))
        case('not authorized: the candidate run is not the release candidate workflow',
             lambda w: w.runs[(CAND_RUN, 1)].update(path='.github/workflows/other.yml'))
        # E's jobs: another job, a URL that is not the job, or a job on another commit.
        case('evidence mismatch: the aarch64-apple-darwin build job is not its native candidate job',
             lambda w: w.e['targets'][0]['build'].update(job_id=7701, job_url=ACTIONS + str(CAND_RUN) + '/job/7701'))
        case('evidence mismatch: the x86_64-unknown-linux-gnu install job URL is not its job',
             lambda w: w.find_job(8803).update(html_url=ACTIONS + str(EVID_RUN) + '/job/1'))
        case('evidence mismatch: the aarch64-apple-darwin install job did not run on the release commit',
             lambda w: w.find_job(8802).update(head_sha=OTHER))
        case('evidence mismatch: the aarch64-apple-darwin install job is not from the candidate or evidence run',
             lambda w: w.e['targets'][0]['install'].update(job=dict(w.e['security']['jobs'][0]['job'])))
        # The source at S: Cargo and main's first-parent chain.
        case('evidence mismatch: the Cargo version at the release commit is not 0.43.0',
             lambda w: w.put(S, 'Cargo.toml', b'[workspace.package]\nversion = "0.44.0"\n'))
        case("evidence mismatch: the release commit is not on main's first-parent chain",
             lambda w: w.commits.update({P: (H, S)}))
        # The candidate ref moved to another commit on main: the runs on S no longer match it.
        case('evidence mismatch: the release run did not run on the release commit',
             lambda w: w.refs.update({'tags/candidate/v0.43.0': ('commit', H)}))
        case('evidence mismatch: the candidate ref does not resolve directly to a commit',
             lambda w: w.refs.update({'tags/candidate/v0.43.0': ('tag', RESERVE_TAG)}))
        # The pinned implementation of the evidence workflow, and the policy digests.
        case('not authorized: the evidence workflow at the release commit is not its pinned implementation',
             lambda w: w.put(S, EVIDENCE_WF, b'name: release-evidence\nrun: anything\n'))
        case('evidence mismatch: the trust records do not match their policy digest',
             lambda w: w.put(P, 'release-keys/records.txt', b'transition-v0.50.0.json\n'))
        case('evidence mismatch: the policy commit does not descend from the signer commit',
             lambda w: w.compares.clear())
        case('evidence mismatch: the release key of v0.43.0 is not the policy key',
             lambda w: w.trust.update(fingerprint='SHA256:' + 'B' * 43))
        case('evidence mismatch: the approval scope transition sha256 does not match the authenticated release facts',
             lambda w: w.trust.update(transition_sha256=digest(77)))
        for message, change, reseal in cases:
            w = World()
            change(w)
            if reseal:
                w.seal()
            with self.subTest(message=message):
                self.refuses(w, message)

    def test_rsig5_notes_and_tutorials_bind_exact_digests(self):
        cases = (
            ('evidence mismatch: release/notes.md does not match its digest',
             lambda w: w.put(S, 'release/notes.md', w.notes + b'changed\n')),
            ('evidence mismatch: release/AGENTS.md and release/CLAUDE.md differ',
             lambda w: w.put(S, 'release/CLAUDE.md', w.guide + b'x')),
            ('evidence mismatch: the release guide does not match its digest',
             lambda w: [w.put(S, 'release/CLAUDE.md', b'other guide\n'), w.put(S, 'release/AGENTS.md', b'other guide\n')]),
            ('evidence mismatch: release/source.json does not match the release record',
             lambda w: w.put(S, 'release/source.json', canon(dict(w.source, design_landing='e' * 64)))),
            ('evidence mismatch: the release guide does not match its digest',
             lambda w: w.e['source'].update(guide_sha256=digest(97))),
            ('evidence mismatch: release/notes.md does not match its digest',
             lambda w: w.e['source'].update(notes_sha256=digest(96))),
            ('evidence mismatch: the x86_64-unknown-linux-gnu tutorial did not use its candidate archive',
             lambda w: w.e['tutorials'][1].update(archive_sha256=digest(95))),
            ('evidence mismatch: the aarch64-apple-darwin tutorial did not use the candidate manifest',
             lambda w: w.e['tutorials'][0].update(manifest_sha256=digest(94))),
            ('evidence mismatch: the tutorials need exactly one entry per target',
             lambda w: w.e['tutorials'].pop()),
            ('evidence mismatch: the aarch64-apple-darwin tutorial did not pass every step',
             lambda w: w.e['tutorials'][0]['steps'][1].update(result='fail')),
            ('evidence mismatch: the aarch64-apple-darwin tutorial is not a fresh Sonnet/medium session',
             lambda w: w.e['tutorials'][0].update(effort='high')),
        )
        for message, change in cases:
            w = World()
            change(w)
            w.seal()
            with self.subTest(message=message):
                self.refuses(w, message)
        # E can be consistent with itself (tutorials, targets and scope all bound to one manifest) and
        # still refuse: its archives must be the authenticated candidate's archives.
        w = World()
        fake = [dict(f, sha256=digest(50 + i)) for i, f in enumerate(w.archives)]
        fake_manifest = manifest_sha(fake)
        w.e['candidate'].update(artifacts=copy.deepcopy(fake), manifest_sha256=fake_manifest)
        for i, entry in enumerate(w.e['targets']):
            entry['archive'] = copy.deepcopy(fake[i])
        for i, entry in enumerate(w.e['tutorials']):
            entry.update(archive_sha256=fake[i]['sha256'], manifest_sha256=fake_manifest)
        w.e['approval_basis']['scope'].update(artifacts=copy.deepcopy(fake), manifest_sha256=fake_manifest)
        w.seal()
        self.refuses(w, 'evidence mismatch: the release record archives are not the authenticated candidate archives')
        # The qualified record binds the exact tutorial-bound manifest and archives.
        w = World()
        q = self.qualifies(w)
        self.assertEqual(q.manifest_sha256, w.manifest)
        self.assertEqual(q.artifacts, tuple((f['name'], f['sha256'], f['size']) for f in w.archives))

    def test_rsig5_security_and_audit_cannot_be_skipped(self):
        cases = (
            ('eligibility incomplete: the CodeQL (rust) job did not conclude success',
             lambda w: w.find_job(9902).update(conclusion='skipped')),
            ('eligibility incomplete: the Core CI job did not conclude success',
             lambda w: w.find_job(9903).update(conclusion='cancelled')),
            ('evidence mismatch: the Core CI job is stale: it did not run on the release commit',
             lambda w: w.runs[(CI_RUN, 1)].update(head_sha=OTHER)),
            ('evidence mismatch: the Core security job did not run on the release commit',
             lambda w: w.find_job(9904).update(head_sha=OTHER)),
            ('evidence mismatch: the CodeQL (actions) job is not from its workflow on main',
             lambda w: w.runs[(SEC_RUN, 1)].update(event='pull_request')),
            ('evidence mismatch: the Core CI job is not from its workflow on main',
             lambda w: w.runs[(CI_RUN, 1)].update(path='.github/workflows/security.yml')),
            ('evidence mismatch: the CodeQL (rust) job is another job',
             lambda w: w.find_job(9902).update(name='CodeQL (actions)')),
            ('eligibility incomplete: 1 open high or critical code scanning alert',
             lambda w: w.alerts.append({'number': 2, 'state': 'open', 'tool': 'CodeQL',
                                        'security_severity': 'high'})),
            ('eligibility incomplete: 2 open high or critical code scanning alerts',
             lambda w: w.alerts.extend([{'number': n, 'state': 'open', 'tool': 'CodeQL',
                                         'security_severity': 'critical'} for n in (3, 4)])),
            ('eligibility incomplete: GitHub code scanning alerts: not authorized (HTTP 403)',
             lambda w: w.errors.update(open_code_scanning_alerts=GITHUB.Unauthorized(
                 'code scanning alerts', 'not authorized (HTTP 403)', 403))),
            ('eligibility incomplete: GitHub code scanning alerts: outcome unknown (HTTP 502)',
             lambda w: w.errors.update(open_code_scanning_alerts=GITHUB.Uncertain(
                 'code scanning alerts', 'outcome unknown (HTTP 502)')))
        )
        for message, change in cases:
            w = World()
            change(w)
            with self.subTest(message=message):
                self.refuses(w, message)
        # E cannot drop, skip or waive the security record or the audit.
        sealed = (
            ("eligibility incomplete: missing field 'audit'", lambda w: w.e.pop('audit')),
            ("eligibility incomplete: missing field 'reference'", lambda w: w.e['audit'].pop('reference')),
            ('evidence mismatch: the security jobs are not exactly the six required jobs',
             lambda w: w.e['security']['jobs'].pop(2)),
            ('evidence mismatch: the open high or critical CodeQL alert count is not 0',
             lambda w: w.e['security'].update(codeql_open_high_critical=1)),
            ('evidence mismatch: the advisory fetch result is not clean',
             lambda w: w.e['security']['advisory_fetch'].update(result='waived')),
            ('evidence mismatch: the x86_64-unknown-linux-gnu install skip count is not 0',
             lambda w: w.e['targets'][1]['install'].update(skipped=1)),
            ('evidence mismatch: the aarch64-apple-darwin fast gate prerequisite skip count is not 0',
             lambda w: w.e['targets'][0]['fast_gate'].update(prerequisite_skipped=2)),
        )
        for message, change in sealed:
            w = World()
            change(w)
            w.seal()
            with self.subTest(message=message):
                self.refuses(w, message)
        # A low-severity alert does not block; the security jobs are in the result's evidence.
        w = World()
        self.qualifies(w)

    def test_rsig5_unauthorized_dispatcher_with_valid_evidence_refuses(self):
        cases = (
            # The release dispatch.
            ('not authorized: the release run was dispatched by an actor who is not a coordinator',
             lambda w: w.runs[(DISPATCH_RUN, 1)].update(actor_id=ATTACKER, triggering_actor_id=ATTACKER)),
            ('not authorized: the release run is not the release workflow',
             lambda w: w.runs[(DISPATCH_RUN, 1)].update(path='.github/workflows/other.yml')),
            ('not authorized: the release run is not a workflow_dispatch on candidate/v0.43.0',
             lambda w: w.runs[(DISPATCH_RUN, 1)].update(event='push')),
            ('not authorized: the release run is not a workflow_dispatch on candidate/v0.43.0',
             lambda w: w.runs[(DISPATCH_RUN, 1)].update(head_branch='main')),
            ('evidence mismatch: the release run did not run on the release commit',
             lambda w: w.runs[(DISPATCH_RUN, 1)].update(head_sha=OTHER)),
            # The evidence run: the coordinator, the pinned workflow, the event and the ref.
            ('not authorized: the evidence run was dispatched by an actor who is not a coordinator',
             lambda w: w.runs[(EVID_RUN, 1)].update(actor_id=ATTACKER, triggering_actor_id=ATTACKER)),
            ('not authorized: the evidence run is not the pinned evidence workflow',
             lambda w: w.runs[(EVID_RUN, 1)].update(workflow_id=82)),
            ('not authorized: the evidence run is not the pinned evidence workflow',
             lambda w: w.runs[(EVID_RUN, 1)].update(path='.github/workflows/forged.yml')),
            ('not authorized: the evidence run is not a workflow_dispatch on candidate/v0.43.0',
             lambda w: w.runs[(EVID_RUN, 1)].update(event='push')),
            ('not authorized: the evidence run is not a workflow_dispatch on candidate/v0.43.0',
             lambda w: w.runs[(EVID_RUN, 1)].update(head_branch='main')),
        )
        for message, change in cases:
            w = World()
            change(w)
            with self.subTest(message=message):
                self.refuses(w, message)
        # The candidate tree cannot extend the allowlist: S's own evidence policy is never read.
        w = World()
        widened = copy.deepcopy(w.ep)
        widened['coordinators'].append({'actor_id': ATTACKER, 'app_id': None, 'installation_id': None})
        w.put(S, 'release-keys/evidence-policy.json', canon(widened))
        w.runs[(DISPATCH_RUN, 1)].update(actor_id=ATTACKER, triggering_actor_id=ATTACKER)
        self.refuses(w, 'not authorized: the release run was dispatched by an actor who is not a coordinator')
        self.assertNotIn(('contents', 'release-keys/evidence-policy.json', S), w.calls)
        # Nor can E's approval policy commit point at a policy that is no longer current.
        w = World()
        w.ep['coordinators'].append({'actor_id': ATTACKER, 'app_id': None, 'installation_id': None})
        w.publish_policy(P)
        w.runs[(DISPATCH_RUN, 1)].update(actor_id=ATTACKER, triggering_actor_id=ATTACKER)
        self.refuses(w, 'not authorized: the release run was dispatched by an actor who is not a coordinator')
        w.runs[(DISPATCH_RUN, 1)].update(actor_id=APP, triggering_actor_id=APP)
        self.refuses(w, 'evidence mismatch: the release record policy commit is not the current policy')
        w = World()
        w.ep['coordinators'].append({'actor_id': ATTACKER, 'app_id': None, 'installation_id': None})
        w.publish_policy(P)
        w.publish_policy(OTHER)
        w.commits.update({HEAD: (OTHER,), OTHER: (P,)})
        w.e['approval_basis']['policy_commit'] = OTHER
        w.seal()
        w.compares[(H, OTHER)] = 'ahead'
        w.runs[(EVID_RUN, 1)].update(actor_id=ATTACKER, triggering_actor_id=ATTACKER)
        self.refuses(w, 'not authorized: the evidence run was dispatched by an actor who is not a coordinator')
        # With valid authority, the same facts qualify.
        self.qualifies(World())

    def test_rsig5_self_asserted_approval_refuses(self):
        # Release 1 cannot ride the standing preview approval.
        w = first_machine_world()
        self.refuses(w, 'not authorized: a standing preview cannot cover a release_1 release')
        # A card that only E asserts: no accepted receipt and no approval run.
        w = first_machine_world()
        receipt = w.add_card('release_1')
        w.archive.clear()
        w.reserve()
        self.refuses(w, 'not authorized: the release_1 card has no accepted founder receipt', recovery=True)
        # The archived receipt must come from the enrolled archive, through protected lineage.
        for change, message in (
                (lambda w, r: w.lineage.update(table_id='fedcba98-7654-4321-8fed-cba987654321'),
                 'not authorized: the archived release_1 approval is not from the enrolled archive'),
                (lambda w, r: w.lineage.update(genesis_sha256=digest(66)),
                 'not authorized: the archived release_1 approval is not from the enrolled archive'),
                (lambda w, r: accept(w, r, policy_sha256=digest(65)),
                 'not authorized: the archived release_1 approval policy is not the protected policy'),
                (lambda w, r: accept(w, r, signer_workflow_sha=OTHER),
                 'not authorized: the archived release_1 approval was not accepted by its policy signer'),
                (lambda w, r: accept(w, r, policy_commit=OTHER),
                 "evidence mismatch: the archived release_1 approval policy commit is not on main's first-parent chain"),
                (lambda w, r: w.archive.update({r: b'{}\n'}),
                 "eligibility incomplete: missing field 'acceptance'"),
                (lambda w, r: w.ep['approval_channel'].update(enabled=False) or w.publish_policy(P) or
                 w.publish_policy(HEAD),
                 'not authorized: the founder approval channel is disabled'),
                (lambda w, r: w.ep['approval_channel'].update(revoked_receipts=[r]) or w.publish_policy(P) or
                 w.publish_policy(HEAD),
                 'not authorized: the release_1 card receipt is revoked')):
            w = first_machine_world()
            receipt = w.add_card('release_1')
            w.reserve()
            change(w, receipt)
            with self.subTest(message=message):
                self.refuses(w, message, recovery=True)
        # A card reference outside the pinned archive.
        w = first_machine_world()
        receipt = w.add_card('release_1')
        w.e['approval_basis']['cards'][0]['reference'] = w.card(receipt, 'release_1')['reference'].replace(
            'rdy-test-release-control', 'attacker-table')
        w.seal()
        self.refuses(w, 'not authorized: the release_1 card is not in the pinned approval archive')

        # First use: the live GitHub join must prove the founder approved exactly this request.
        def first_use(change):
            w = first_machine_world()
            w.add_card('release_1', via='first_use')
            change(w, w.approval_runs[0]['run_id'], w.approval_runs[0]['artifact_id'])
            return w

        for change, message in (
                (lambda w, r, a: w.runs[(r, 1)].update(actor_id=PERSON, triggering_actor_id=PERSON),
                 'not authorized: the release_1 approval run was not dispatched by the founder'),
                (lambda w, r, a: w.runs[(r, 1)].update(triggering_actor_id=APP),
                 'not authorized: the release_1 approval run was not dispatched by the founder'),
                (lambda w, r, a: w.runs[(r, 1)].update(path=EVIDENCE_WF),
                 'not authorized: the release_1 approval run is not the pinned founder approval workflow'),
                (lambda w, r, a: w.runs[(r, 1)].update(head_branch='candidate/v0.42.0'),
                 'not authorized: the release_1 approval run is not the pinned founder approval workflow'),
                (lambda w, r, a: w.put(HEAD, APPROVAL_WF, b'name: release-approval\nrun: forged\n'),
                 'not authorized: the release_1 approval workflow is not its pinned bytes'),
                (lambda w, r, a: w.reviews[r][0].update(user_id=PERSON),
                 'not authorized: the release_1 review is not the founder approval in the pinned environment'),
                (lambda w, r, a: w.reviews[r][0].update(environments=((42, 'release'),)),
                 'not authorized: the release_1 review is not the founder approval in the pinned environment'),
                (lambda w, r, a: w.reviews[r][0].update(state='rejected'),
                 'not authorized: the release_1 review is not the founder approval in the pinned environment'),
                (lambda w, r, a: w.reviews[r][0].update(comment='APPROVE sha256:' + digest(64)),
                 'not authorized: the founder review does not approve the release_1 request'),
                (lambda w, r, a: w.reviews[r].append(dict(w.reviews[r][0])),
                 'not authorized: the release_1 approval run needs exactly one founder review'),
                (lambda w, r, a: w.reviews[r].clear(),
                 'not authorized: the release_1 approval run needs exactly one founder review'),
                (lambda w, r, a: w.artifacts[a].update(name='approval-request-' + str(r) + '-2'),
                 'evidence mismatch: the release_1 approval artifact is not the request of its run'),
                (lambda w, r, a: w.runs.update({(r, 2): w.runs.pop((r, 1))}),
                 'eligibility incomplete: GitHub run attempt: not found'),
                (lambda w, r, a: w.responses[r][3].update(sha256=digest(63)),
                 'evidence mismatch: the release_1 approval source responses do not match the authenticated facts')):
            w = first_use(change)
            with self.subTest(message=message):
                self.refuses(w, message)
        # The coordinator cannot approve itself: a request in an artifact the coordinator made,
        # carried by a run the coordinator dispatched, is not a founder receipt.
        w = first_machine_world()
        w.add_card('release_1', via='first_use')
        run_id = w.approval_runs[0]['run_id']
        w.runs[(run_id, 1)].update(actor_id=APP, triggering_actor_id=APP, workflow_id=81, path=EVIDENCE_WF,
                                   head_branch='candidate/v0.42.0')
        self.refuses(w, 'not authorized: the release_1 approval run is not the pinned founder approval workflow')
        # A requested approval run that E's cards do not name.
        w = first_machine_world()
        w.add_card('release_1')
        self.refuses(w, 'evidence mismatch: a requested approval run names no card of the release record',
                     request=w.request(approvals=[{'receipt_sha256': digest(61), 'run_id': 6100, 'artifact_id': 6200}]))
        # Valid founder receipts qualify: an archived one, and a first use with its bytes for the archive.
        w = first_machine_world()
        receipt = w.add_card('release_1')
        w.reserve()
        q = self.qualifies(w, recovery=True)
        self.assertEqual([(c.kind, c.receipt_sha256, c.source) for c in q.cards], [('release_1', receipt, 'archive')])
        self.assertEqual(q.cards[0].envelope_bytes, w.archive[receipt])
        w = first_machine_world()
        receipt = w.add_card('release_1', via='first_use')
        q = self.qualifies(w)
        card = q.cards[0]
        self.assertEqual((card.kind, card.receipt_sha256, card.source, card.envelope_bytes),
                         ('release_1', receipt, 'first_use', None))
        self.assertEqual(sha(card.receipt_bytes), receipt)
        self.assertEqual(EVIDENCE.parse_approval_receipt(card.receipt_bytes).value['issuer']['stable_identity_sha256'],
                         stable_identity(card.projection_bytes))
        self.assertEqual(card.request_bytes, w.members[(w.approval_runs[0]['artifact_id'], 'approval-request.json')])
        # Before credentials the archive is never read: this proof says nothing about it.
        self.assertNotIn('approval_record', [c[0] for c in w.calls])
        self.assertIsInstance(q, ELIGIBILITY.Preliminary)
        self.assertNotIsInstance(q, ELIGIBILITY.Qualified)
        # At the signer phase, fresh provider proof is used only after the archive authenticates that no
        # accepted record exists.
        w.reserve()
        q = self.qualifies(w, recovery=True)
        self.assertIsInstance(q, ELIGIBILITY.Qualified)
        self.assertEqual((q.cards[0].source, q.cards[0].receipt_bytes), ('first_use', card.receipt_bytes))
        self.assertIn(('approval_record', receipt), w.calls)
        self.assertEqual(w.archive, {})

    def test_rsig5_missing_or_wrong_scope_key_and_platform_cards_refuse(self):
        # A key change needs its card; a standing preview cannot cover it.
        w = key_change_world()
        self.refuses(w, 'not authorized: a standing preview cannot cover a key_change release')
        w = key_change_world()
        w.add_card('new_platform')
        self.refuses(w, 'not authorized: the release needs a key_change card')
        # The key card must cover exactly this key, transition, cutover and target set.
        for key, value in (('key_arn', KEY_ARN), ('fingerprint', 'SHA256:' + 'C' * 43),
                           ('transition_sha256', digest(60)), ('first_machine_tag', 'v0.44.0'),
                           ('targets', [MAC])):
            w = key_change_world()
            scope = w.scope_for('key_change')
            scope['key_arn'] = NEW_KEY_ARN
            scope[key] = value
            w.add_card('key_change', scope=scope)
            w.reserve()
            with self.subTest(key=key):
                self.refuses(w, 'not authorized: the key_change card does not cover the release '
                                + key.replace('_', ' '), recovery=True)
        w = key_change_world()
        scope = w.scope_for('key_change')
        scope['key_arn'] = NEW_KEY_ARN
        receipt = w.add_card('key_change', scope=scope)
        q = self.qualifies(w)
        self.assertEqual([(c.kind, c.receipt_sha256) for c in q.cards], [('key_change', receipt)])
        self.assertEqual(q.key_arn, NEW_KEY_ARN)
        # A card whose receipt is another kind.
        w = key_change_world()
        w.add_card('key_change', receipt_kind='new_platform')
        w.reserve()
        self.refuses(w, 'evidence mismatch: the key_change card names a new_platform receipt', recovery=True)
        # Release 1 needs its own card; a key card does not stand in for it, and its scope is exact.
        w = first_machine_world()
        w.add_card('key_change')
        self.refuses(w, 'not authorized: the release needs a release_1 card')
        w = first_machine_world()
        w.add_card('release_1', scope=dict(w.full_scope(), version='0.44.0'))
        w.reserve()
        self.refuses(w, 'not authorized: the release_1 card does not cover the release version', recovery=True)
        w = first_machine_world()
        w.add_card('release_1', scope=dict(w.full_scope(), manifest_sha256=manifest_sha([archive('0.42.0', MAC)]),
                                           artifacts=[archive('0.42.0', MAC)]))
        w.reserve()
        self.refuses(w, 'not authorized: the release_1 card does not cover the release manifest sha256', recovery=True)
        # A platform card must name the exact complete target set.
        w = World()
        w.add_card('new_platform', scope={'repository': REPO, 'targets': [LINUX]})
        w.reserve()
        self.refuses(w, 'not authorized: the new_platform card does not cover the release targets', recovery=True)
        # A new target set needs a card and a qualified adapter: no card makes it signable here.
        for with_card in (False, True):
            w = World()
            w.policy['targets'] = [MAC, 'x86_64-pc-windows-msvc', LINUX]
            w.publish_policy(P)
            w.publish_policy(HEAD)
            if with_card:
                w.add_card('new_platform', scope={'repository': REPO, 'targets': w.policy['targets']})
            with self.subTest(card=with_card):
                self.refuses(w, 'not authorized: the policy targets need a new_platform card and a qualified adapter')
        # A revocation card never permits signing.
        w = World()
        w.add_card('revocation')
        self.refuses(w, 'not authorized: a revocation card never permits signing')
        # The standing preview after the key's first release needs no card.
        self.qualifies(World())

    def test_rsig5_rerun_checks_original_and_triggering_actor(self):
        def rerun(actor, trigger):
            w = World()
            w.runs[(EVID_RUN, 2)] = dict(w.runs[(EVID_RUN, 1)], run_attempt=2, actor_id=actor,
                                         triggering_actor_id=trigger)
            w.jobs[(EVID_RUN, 2)] = [dict(j, run_attempt=2) for j in w.jobs[(EVID_RUN, 1)]]
            for entry in w.e['targets']:
                entry['install']['job'].update(run_attempt=2)
            w.seal()
            return w, w.request(evidence_run_attempt=2)

        w, request = rerun(PERSON, ATTACKER)
        self.refuses(w, 'not authorized: the evidence run was triggered by an actor who is not a coordinator',
                     request=request)
        w, request = rerun(ATTACKER, PERSON)
        self.refuses(w, 'not authorized: the evidence run was dispatched by an actor who is not a coordinator',
                     request=request)
        w, request = rerun(PERSON, APP)
        self.assertEqual(self.qualifies(w, request=request).evidence_run, (EVID_RUN, 2, EVID_ART))
        # The request names one exact attempt; another attempt is another run.
        w = World()
        self.refuses(w, 'eligibility incomplete: GitHub run attempt: not found',
                     request=w.request(evidence_run_attempt=2))
        # A rerun of the release dispatch: both of its actors are checked again.
        for actor, trigger, message in (
                (APP, ATTACKER, 'not authorized: the release run was triggered by an actor who is not a coordinator'),
                (ATTACKER, APP, 'not authorized: the release run was dispatched by an actor who is not a coordinator')):
            w = World()
            w.runs[(DISPATCH_RUN, 2)] = dict(w.runs[(DISPATCH_RUN, 1)], run_attempt=2, actor_id=actor,
                                             triggering_actor_id=trigger)
            self.refuses(w, message, request=w.request(dispatch_run_attempt=2))
        # Recovery keeps the original E and its actors, and checks the current dispatcher against
        # the current policy: it never inherits the first run's permission.
        w = World()
        w.reserve()
        w.runs[(DISPATCH_RUN, 2)] = dict(w.runs[(DISPATCH_RUN, 1)], run_attempt=2, triggering_actor_id=ATTACKER)
        self.refuses(w, 'not authorized: the release run was triggered by an actor who is not a coordinator',
                     recovery=True, request=w.request(dispatch_run_attempt=2))
        w = World()
        w.reserve()
        current = copy.deepcopy(w.ep)
        w.ep['coordinators'] = [c for c in w.ep['coordinators'] if c['actor_id'] != APP]
        w.publish_policy(HEAD)
        w.ep = current
        self.refuses(w, 'not authorized: the release run was dispatched by an actor who is not a coordinator',
                     recovery=True)
        # The original evidence run is checked against the policy it was sealed under.
        w = World()
        w.reserve()
        w.runs[(EVID_RUN, 1)].update(triggering_actor_id=ATTACKER)
        self.refuses(w, 'not authorized: the evidence run was triggered by an actor who is not a coordinator',
                     recovery=True)
        w = World()
        w.reserve()
        q = self.qualifies(w, recovery=True, request=None)
        self.assertEqual((q.mode, q.dispatcher, q.evidence_run), ('recovery', (DISPATCH_RUN, 1, APP, APP),
                                                                  (EVID_RUN, 1, EVID_ART)))

    def test_rsig5_new_version_rule_does_not_reject_authenticated_recovery(self):
        # v0.43.0 is reserved, and v0.44.0 has published since.
        w = World()
        w.reserve()
        w.releases.append({'release_id': 300, 'tag_name': 'v0.44.0', 'target_commitish': OTHER, 'draft': False,
                           'prerelease': True, 'assets': ()})
        self.refuses(w, 'reservation conflict: v0.43.0 is already reserved; recover its reservation')
        q = self.qualifies(w, recovery=True)
        self.assertEqual(q.mode, 'recovery')
        self.assertEqual(q.tuple_bytes, w.tags[RESERVE_TAG]['message_bytes'])
        self.assertEqual(q.reservation, ELIGIBILITY.Reservation(digest(91), RESERVE_TAG))
        self.assertEqual(q.publication, ELIGIBILITY.Publication('unpublished', None))
        # Recovery asks the trust tool for archival selection, and does not re-run live-only checks.
        self.assertIn(('release_trust', '0.43.0', P, False), w.calls)
        self.assertNotIn(('open_code_scanning_alerts', 'refs/heads/main'), w.calls)
        # A lost publication acknowledgement: the matching release is read-only success.
        w = World()
        w.reserve()
        w.releases.append({'release_id': 301, 'tag_name': 'v0.43.0', 'target_commitish': S, 'draft': False,
                           'prerelease': True, 'assets': (1, 2)})
        w.refs['tags/v0.43.0'] = ('commit', S)
        q = self.qualifies(w, recovery=True)
        self.assertEqual(q.publication, ELIGIBILITY.Publication('published', 301))
        self.assertEqual(w.mutations, [])
        # An unfinished draft is reported; a conflicting release refuses.
        w = World()
        w.reserve()
        w.releases.append({'release_id': 302, 'tag_name': 'v0.43.0', 'target_commitish': S, 'draft': True,
                           'prerelease': True, 'assets': ()})
        self.assertEqual(self.qualifies(w, recovery=True).publication, ELIGIBILITY.Publication('draft', 302))
        for change, message in (
                (lambda w: (w.releases.append({'release_id': 303, 'tag_name': 'v0.43.0', 'target_commitish': OTHER,
                                               'draft': False, 'prerelease': True, 'assets': ()}),
                            w.refs.update({'tags/v0.43.0': ('commit', OTHER)})),
                 'reservation conflict: the release of v0.43.0 does not target the reserved commit'),
                (lambda w: (w.releases.append({'release_id': 304, 'tag_name': 'v0.43.0', 'target_commitish': S,
                                               'draft': False, 'prerelease': True, 'assets': ()}),
                            w.refs.update({'tags/v0.43.0': ('commit', OTHER)})),
                 'reservation conflict: the published v0.43.0 is not the reserved commit'),
                (lambda w: w.refs.update({'tags/v0.43.0': ('commit', S)}),
                 'reservation conflict: the tag v0.43.0 exists without its release')):
            w = World()
            w.reserve()
            change(w)
            with self.subTest(message=message):
                self.refuses(w, message, recovery=True)
        # Recovery authenticates the exact reservation identity first.
        w = World()
        self.refuses(w, 'eligibility incomplete: v0.43.0 has no reservation to recover', recovery=True)
        w = World()
        w.refs['tags/reserve/v0.43.0'] = ('commit', S)
        self.refuses(w, 'reservation conflict: the reservation of v0.43.0 is not an annotated tag', recovery=True)
        w = World()
        w.reserve()
        w.tags[RESERVE_TAG]['tag'] = 'reserve/v0.44.0'
        self.refuses(w, 'reservation conflict: the reservation tag object is not reserve/v0.43.0 on a commit',
                     recovery=True)
        w = World()
        w.reserve()
        w.tags[RESERVE_TAG]['message_bytes'] = b'not a tuple\n'
        self.refuses(w, 'reservation conflict: the reservation message is not a tuple: evidence is not JSON',
                     recovery=True)
        # A reservation of other evidence, or a tuple that differs from the authenticated facts.
        w = World()
        w.reserve(dict(World().expected_tuple(), release_evidence_sha256=digest(59)))
        self.refuses(w, 'evidence mismatch: the requested release record is not the reserved one', recovery=True)
        w = World()
        t = w.expected_tuple()
        t['signer']['transition_sha256'] = digest(58)
        w.reserve(t)
        self.refuses(w, 'evidence mismatch: the reservation tuple is not the tuple of the authenticated facts',
                     recovery=True)
        w = World()
        w.reserve(dict(w.expected_tuple(), commit=H))
        self.refuses(w, 'reservation conflict: the reservation tuple does not name its tag', recovery=True)
        # The new-version rule still holds for a new reservation.
        for change, message in (
                (lambda w: w.releases.append({'release_id': 305, 'tag_name': 'v0.44.0', 'target_commitish': OTHER,
                                              'draft': False, 'prerelease': True, 'assets': ()}),
                 'reservation conflict: v0.43.0 is not newer than every published version'),
                (lambda w: w.refs.update({'tags/v0.43.0': ('commit', S)}),
                 'reservation conflict: the tag v0.43.0 already exists'),
                (lambda w: w.releases.append({'release_id': 306, 'tag_name': 'v0.43.0', 'target_commitish': S,
                                              'draft': True, 'prerelease': True, 'assets': ()}),
                 'reservation conflict: a release of v0.43.0 already exists'),
                (lambda w: w.releases.append({'release_id': 307, 'tag_name': 'v1.0.0', 'target_commitish': S,
                                              'draft': False, 'prerelease': False, 'assets': ()}),
                 'evidence mismatch: a published release tag is not a v0.N.0 preview')):
            w = World()
            change(w)
            with self.subTest(message=message):
                self.refuses(w, message)


def restate(world, run_id, artifact_id, **changes):
    """Changes the founder's approval request consistently: its bytes, and the review that approves them."""
    key = (artifact_id, 'approval-request.json')
    request = json.loads(world.members[key])
    request.update(changes)
    world.members[key] = canon(request)
    world.reviews[run_id][0]['comment'] = 'APPROVE sha256:' + sha(world.members[key])


def respond(world, run_id, **digests):
    """Records new source-response digests for an approval run. An API response also changes size;
    a file or ZIP entry keeps the size of its bytes."""
    for entry in world.responses[run_id]:
        if entry['kind'] in digests:
            entry['sha256'] = digests[entry['kind']]
            if entry['kind'] not in ('protected_policy', 'request_archive', 'workflow_source'):
                entry['bytes'] += 1


def current_channel(world, **changes):
    """Changes the approval channel in the current policy on main only; E's policy commit keeps its own."""
    sealed = copy.deepcopy(world.ep)
    world.ep['approval_channel'].update(changes)
    world.publish_policy(HEAD)
    world.ep = sealed


class FirstUseIdentity(Base):
    def first_use(self):
        w = first_machine_world()
        receipt = w.add_card('release_1', via='first_use')
        return w, receipt, w.approval_runs[0]['run_id'], w.approval_runs[0]['artifact_id']

    def test_rsig5_first_use_receipt_survives_provider_metadata_changes(self):
        w, receipt, r, a = self.first_use()
        first = self.qualifies(w).cards[0]
        self.assertEqual(first.receipt_sha256, receipt)
        # GitHub re-renders its responses (pushes, counters, timestamps): every response digest
        # changes, consistently in the live facts and in the recorded source responses.
        w.repository_sha, w.runs[(r, 1)]['sha256'], w.artifacts[a]['sha256'] = digest(95), digest(96), digest(97)
        respond(w, r, repository=digest(95), run_attempt=digest(96), artifact=digest(97), job=digest(98),
                review_history=digest(99), implementation_source=digest(100))
        again = self.qualifies(w).cards[0]
        self.assertEqual((again.receipt_sha256, again.receipt_bytes, again.request_bytes),
                         (receipt, first.receipt_bytes, first.request_bytes))
        # The projection still records the responses it saw, in its audit section; they are not its identity.
        self.assertEqual(json.loads(again.projection_bytes)['audit']['source_responses'], w.responses[r])
        self.assertNotEqual(again.projection_bytes, first.projection_bytes)
        self.assertEqual(json.loads(again.projection_bytes)['identity'], json.loads(first.projection_bytes)['identity'])
        self.assertEqual(stable_identity(again.projection_bytes), stable_identity(first.projection_bytes))
        # The recorded responses date from the approval; later live responses differ from them.
        w.repository_sha, w.runs[(r, 1)]['sha256'], w.artifacts[a]['sha256'] = digest(101), digest(102), digest(103)
        self.assertEqual(self.qualifies(w).cards[0].receipt_bytes, first.receipt_bytes)
        # After the reservation, recovery authenticates the same receipt that E froze.
        w.reserve()
        recovered = self.qualifies(w, recovery=True)
        self.assertEqual(recovered.mode, 'recovery')
        self.assertEqual([(c.kind, c.receipt_sha256, c.source) for c in recovered.cards],
                         [('release_1', receipt, 'first_use')])
        self.assertEqual(recovered.cards[0].receipt_bytes, first.receipt_bytes)
        w.repository_sha, w.runs[(r, 1)]['sha256'] = digest(104), digest(105)
        respond(w, r, repository=digest(104), run_attempt=digest(105))
        self.assertEqual(self.qualifies(w, recovery=True).cards[0].receipt_bytes, first.receipt_bytes)

    def test_rsig5_first_use_identity_and_current_authority_refuse(self):
        changes = (
            # The approver is current authority: it refuses with its own reason.
            (lambda w, r, a: w.reviews[r][0].update(user_id=PERSON),
             'not authorized: the release_1 review is not the founder approval in the pinned environment'),
            # Kind, scope and artifact digest are the approval's identity: E's card no longer names it.
            (lambda w, r, a: restate(w, r, a, kind='key_change', scope=w.scope_for('key_change')),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            (lambda w, r, a: restate(w, r, a, scope=dict(w.full_scope(), design_landing=digest(77))),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            (lambda w, r, a: (w.artifacts[a].update(digest=digest(78)), respond(w, r, request_archive=digest(78))),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            # So are the policy commit, the job and the run head, each changed consistently.
            (lambda w, r, a: restate(w, r, a, policy_commit=S),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            (lambda w, r, a: w.jobs[(r, 1)][0].update(job_id=9999),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            (lambda w, r, a: (w.runs[(r, 1)].update(head_sha=P), w.artifacts[a].update(head_sha=P),
                              w.put(P, APPROVAL_WF, w.approval_wf)),
             'not authorized: the release_1 card does not name the founder-approved receipt'),
            # A changed source, attempt or comment, and competing reviews, refuse with their own reasons.
            (lambda w, r, a: w.put(HEAD, APPROVAL_WF, b'name: release-approval\nrun: forged\n'),
             'not authorized: the release_1 approval workflow is not its pinned bytes'),
            (lambda w, r, a: w.runs.update({(r, 2): dict(w.runs.pop((r, 1)), run_attempt=2)}),
             'eligibility incomplete: GitHub run attempt: not found'),
            (lambda w, r, a: w.reviews[r][0].update(comment='APPROVE sha256:' + digest(64)),
             'not authorized: the founder review does not approve the release_1 request'),
            (lambda w, r, a: w.reviews[r].append(dict(w.reviews[r][0], user_id=PERSON)),
             'not authorized: the release_1 approval run needs exactly one founder review'),
        )
        for recovery in (False, True):
            for change, message in changes:
                w, receipt, r, a = self.first_use()
                if recovery:
                    w.reserve()
                change(w, r, a)
                with self.subTest(recovery=recovery, message=message):
                    self.refuses(w, message, recovery=recovery)
        # Recovery checks current authority from the current policy on main, apart from the identity
        # that E froze: a revoked receipt, a disabled channel or another founder refuses.
        for via in ('first_use', 'archive'):
            for channel, message in (
                    ({'revoked_receipts': None}, 'not authorized: the release_1 card receipt is revoked'),
                    ({'enabled': False}, 'not authorized: the founder approval channel is disabled'),
                    ({'founder_user_id': 4343}, 'not authorized: ' + {
                        'first_use': 'the release_1 approval run was not dispatched by the founder',
                        'archive': 'the archived release_1 approval is not the founder approval'}[via])):
                w = first_machine_world()
                receipt = w.add_card('release_1', via=via)
                w.reserve()
                self.qualifies(w, recovery=True)
                if 'revoked_receipts' in channel:
                    channel = {'revoked_receipts': [receipt]}
                current_channel(w, **channel)
                with self.subTest(via=via, message=message):
                    self.refuses(w, message, recovery=True)

    def test_rsig5_first_acceptance_is_kept_and_reused_from_the_archive(self):
        # Before the first durable archive, fresh provider proof is required: unavailable or expired
        # proof refuses.
        for change, message in (
                (lambda w, r, a: w.artifacts[a].update(expired=True),
                 'eligibility incomplete: GitHub artifact: the artifact expired'),
                (lambda w, r, a: w.errors.update(approval_responses=ControllerError('approval responses unavailable')),
                 'eligibility incomplete: approval responses unavailable'),
                (lambda w, r, a: w.reviews[r].clear(),
                 'not authorized: the release_1 approval run needs exactly one founder review')):
            w, receipt, r, a = self.first_use()
            change(w, r, a)
            with self.subTest(message=message):
                self.refuses(w, message)
        # An unreadable archive is neither absence nor a way back to fresh proof.
        w, receipt, r, a = self.first_use()
        w.reserve()
        w.errors['approval_record'] = ControllerError('the archive is unavailable')
        self.refuses(w, 'eligibility incomplete: the archive is unavailable', recovery=True)
        # The first acceptance: the signer archives the exact qualified bytes, after the reservation.
        w, receipt, r, a = self.first_use()
        e_bytes = w.members[(EVID_ART, 'release-record.json')]
        first = self.qualifies(w)
        card = first.cards[0]
        w.reserve()
        record = w.envelope(card.request_bytes, card.receipt_bytes, card.projection_bytes)
        w.archive[receipt] = record
        # Later the provider evidence changes or goes: responses re-render, the artifact expires and the
        # review history is gone. Recovery reuses the accepted record and observes nothing fresh.
        respond(w, r, repository=digest(95), run_attempt=digest(96), artifact=digest(97), job=digest(98))
        w.artifacts[a]['expired'] = True
        w.reviews[r] = []
        w.calls = []
        recovered = self.qualifies(w, recovery=True)
        again = recovered.cards[0]
        self.assertEqual((again.source, again.receipt_sha256, again.envelope_bytes), ('archive', receipt, record))
        self.assertEqual((again.request_bytes, again.receipt_bytes, again.projection_bytes),
                         (card.request_bytes, card.receipt_bytes, card.projection_bytes))
        self.assertEqual(json.loads(again.projection_bytes)['audit'], json.loads(card.projection_bytes)['audit'])
        self.assertNotEqual(json.loads(again.projection_bytes)['audit']['source_responses'], w.responses[r])
        self.assertEqual((recovered.evidence_bytes, recovered.evidence_sha256), (e_bytes, first.evidence_sha256))
        self.assertEqual(recovered.tuple_bytes, first.tuple_bytes)
        self.assertEqual(recovered.tuple_bytes, w.tags[RESERVE_TAG]['message_bytes'])
        self.assertEqual(w.archive, {receipt: record})
        for name in ('run_approvals', 'approval_responses'):
            self.assertNotIn(name, [c[0] for c in w.calls])
        self.assertNotIn(('artifact', a, r), w.calls)
        self.assertNotIn(('artifact_member', a, 'approval-request.json'), w.calls)
        self.assertNotIn(('run_attempt', r, 1), w.calls)
        # The same holds without the approval run in the request; a request that names another run for
        # the accepted receipt refuses.
        self.assertEqual(self.qualifies(w, recovery=True, request=w.request(approvals=[])).cards[0].envelope_bytes,
                         record)
        other = [dict(w.approval_runs[0], run_id=r + 50)]
        self.refuses(w, 'evidence mismatch: the requested release_1 approval run is not the accepted one',
                     recovery=True, request=w.request(approvals=other))
        # Current authority is still checked apart from the accepted identity.
        current_channel(w, enabled=False)
        self.refuses(w, 'not authorized: the founder approval channel is disabled', recovery=True)

    def test_rsig5_frozen_v1_approvals_keep_full_projection_semantics(self):
        # A frozen v1 archive record still authenticates: its receipt binds the whole projection.
        w = first_machine_world()
        receipt = w.add_card('release_1', schema=1)
        w.reserve()
        card = self.qualifies(w, recovery=True).cards[0]
        self.assertEqual((card.source, card.envelope_bytes), ('archive', w.archive[receipt]))
        issuer = EVIDENCE.parse_approval_receipt(card.receipt_bytes).value['issuer']
        self.assertEqual(issuer['provider_evidence_sha256'], sha(card.projection_bytes))
        # A new first use writes schema 2 only: a card that names a v1 receipt refuses, in both phases,
        # before it can take a reservation that nothing can then authorize.
        w = first_machine_world()
        w.add_card('release_1', via='first_use', schema=1)
        self.refuses(w, 'not authorized: the release_1 card does not name the founder-approved receipt')
        w.reserve()
        self.refuses(w, 'not authorized: the release_1 card does not name the founder-approved receipt', recovery=True)


if __name__ == '__main__':
    unittest.main()
