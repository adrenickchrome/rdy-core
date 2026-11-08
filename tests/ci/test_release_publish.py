"""The release caller, the private draft, the genuine-check receipts and promotion, with fake remote services.

No test reaches GitHub, AWS or a key. Every write goes to `FakeReleases` or the eligibility `World`. Archive
bytes here are fixed filler, so their digests are real digests of filler. The one real signature is the public
v0.35.0 vector already in the repository: it proves the verifier's command shape on real bytes, and nothing
else. A "signature" anywhere else is opaque bytes that only the fake verifier knows.
"""
import copy
import dataclasses
import io
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_release_controller as ctl  # noqa: E402
import test_release_sign as sgn  # noqa: E402

base = ctl.base
CONTROLLER, ELIGIBILITY, GITHUB, CANDIDATE = ctl.CONTROLLER, base.ELIGIBILITY, base.GITHUB, base.CANDIDATE
sys.modules['controller'] = CONTROLLER
sys.modules['sign'] = sgn.SIGN
SIGN = sgn.SIGN
PUBLISH = base.load('publish')
Refused, ControllerError = CONTROLLER.Refused, base.ControllerError
ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / '.github/workflows/release.yml'
WORKFLOW = WORKFLOW_PATH.read_text() if WORKFLOW_PATH.exists() else 'jobs:\n'
SIGN_WORKFLOW = (ROOT / '.github/workflows/release-sign.yml').read_text()
VECTOR = ROOT / 'crates/rdy-qa/fixtures/release-keys/receiver-v1/real-v0.35.0'

sha, canon, digest = base.sha, base.canon, base.digest
S, P, H, REPO = base.S, base.P, base.H, base.REPO
MAC, LINUX = CANDIDATE.TARGETS
DISPATCH_RUN = base.DISPATCH_RUN
SIGNATURE = ctl.SIGNATURE
RUNS_ON = {MAC: 'macos-26', LINUX: 'ubuntu-26.04'}
ARCHIVE_BYTES = {MAC: b'filler archive for the mac target\n', LINUX: b'filler archive for the linux target\n'}
CRATES = {'imbl-7.0.2.crate': b'filler crate one\n', 'imbl-sized-chunks-0.2.0.crate': b'filler crate two\n'}
LOCK = ('version = 4\n\n[[package]]\nname = "imbl"\nversion = "7.0.2"\nsource = "registry+x"\nchecksum = "'
        + sha(CRATES['imbl-7.0.2.crate']) + '"\n\n[[package]]\nname = "imbl-sized-chunks"\nversion = "0.2.0"\n'
        'source = "registry+x"\nchecksum = "' + sha(CRATES['imbl-sized-chunks-0.2.0.crate'])
        + '"\n\n[[package]]\nname = "rdy-core"\nversion = "0.43.0"\n').encode()
SIG_ART, RECEIPT_ART, ASSETS_ART = 9301, 9302, 9303
GENUINE_ART = {MAC: 9304, LINUX: 9305}
GENUINE_JOB = {MAC: 9406, LINUX: 9407}
ELIGIBILITY_JOB_ID, DRAFT_JOB_ID, TAG_JOB_ID = 9401, 9402, 9403


def real_archive(version, target):
    data = ARCHIVE_BYTES[target]
    return {'name': 'rdy-' + version + '-preview-' + target + '.tar.gz', 'sha256': sha(data), 'size': len(data)}


def release_world(world=None):
    """One release whose archives, crates and lock file are real bytes."""
    with mock.patch.object(base, 'archive', real_archive):
        w = world() if world else base.World()
    sources = [{'name': n, 'sha256': sha(d), 'size': len(d)} for n, d in sorted(CRATES.items())]
    for build in w.candidate['builds']:
        build['sources'] = copy.deepcopy(sources)
    w.members[(base.CAND_ART, 'candidate.json')] = canon(w.candidate)
    w.put(S, 'Cargo.lock', LOCK)
    w.seal()
    return w


def archive_files(w):
    return {f['name']: ARCHIVE_BYTES[MAC if MAC in f['name'] else LINUX] for f in w.archives}


def manifest_of(w):
    return CANDIDATE.manifest_bytes(w.archives)


class Git(ctl.FakeGit):
    """The fact source, with the artifact ZIPs and the run lists that the release job names."""

    def __init__(self, world):
        ctl.FakeGit.__init__(self, world)
        self.paths = {}
        self.runs_of_release = []

    def artifact_path(self, fact):
        return self.paths[fact.artifact_id]

    def release_runs(self, version):
        return list(self.runs_of_release)

    def run_artifacts(self, run_id):
        return [GITHUB.ArtifactFact(**a) for a in self.w.artifacts.values() if a['run_id'] == run_id]


class FakeReleases(object):
    """GitHub's release API over the World. A write is recorded. Nothing can delete a draft or an asset."""

    def __init__(self, stage):
        self.stage = stage
        self.w = stage.rig.w
        self.rows = []
        self.blobs = {}
        self.mutations = []
        self.faults = {}
        self.next_id = 7000

    def __getattr__(self, name):
        return getattr(self.stage.rig.git, name)

    def _fact(self, row):
        return PUBLISH.DraftFact(row['id'], row['tag'], row['target'], row['name'], row['draft'], row['prerelease'],
                                 row['body'], tuple(PUBLISH.AssetFact(**a) for a in row['assets']))

    def full_releases(self):
        return [self._fact(r) for r in self.rows]

    def releases(self):
        rows = [GITHUB.ReleaseFact(**r) for r in self.w.releases]
        for row in self.rows:
            if row['draft']:
                rows.append(GITHUB.ReleaseFact(row['id'], row['tag'], row['target'], True, row['prerelease'], ()))
        return rows

    def release_full(self, release_id):
        for row in self.rows:
            if row['id'] == release_id:
                return self._fact(row)
        raise base.absent('release')

    def public_release(self, tag):
        for row in self.rows:
            if row['tag'] == tag and not row['draft']:
                return self._fact(row)
        raise base.absent('release')

    def _fault(self, name, applied):
        fault = self.faults.get(name)
        if fault is None or fault[0] != applied:
            return
        if fault[1] > 0:
            self.faults[name] = (applied, fault[1] - 1)
            raise GITHUB.ReconcileRequired(name, 'timeout')

    def create_draft(self, tag, target, name, body):
        self.mutations.append('create_draft')
        self._fault('create_draft', False)
        self.next_id += 1
        row = dict(id=self.next_id, tag=tag, target=target, name=name, draft=True, prerelease=True, body=body, assets=[])
        self.rows.append(row)
        self._fault('create_draft', True)
        return self._fact(row)

    def add_asset(self, row, name, data, state='uploaded'):
        self.next_id += 1
        asset = dict(asset_id=self.next_id, name=name, size=len(data), state=state, digest='sha256:' + sha(data))
        row['assets'].append(asset)
        self.blobs[self.next_id] = data
        return asset

    def upload_asset(self, release_id, name, path):
        self.mutations.append('upload ' + name)
        self._fault('upload ' + name, False)
        row = [r for r in self.rows if r['id'] == release_id][0]
        if any(a['name'] == name for a in row['assets']):
            raise GITHUB.ReconcileRequired('upload', 'HTTP 422', 422)
        asset = self.add_asset(row, name, Path(path).read_bytes())
        self._fault('upload ' + name, True)
        return PUBLISH.AssetFact(**asset)

    def download_asset(self, asset, out, limit):
        data = self.blobs[asset.asset_id]
        Path(out).write_bytes(data)
        return sha(data), len(data)

    def public_asset(self, tag, name, out, limit):
        row = [r for r in self.rows if r['tag'] == tag and not r['draft']][0]
        found = [a for a in row['assets'] if a['name'] == name]
        if not found:
            raise GITHUB.Malformed('public asset', 'HTTP 404')
        data = self.blobs[found[0]['asset_id']]
        Path(out).write_bytes(data)
        return sha(data), len(data)

    def create_final_tag(self, version, commit):
        self.mutations.append('create_final_tag')
        self._fault('create_final_tag', False)
        self.w.refs['tags/v' + version] = ('commit', commit)
        self._fault('create_final_tag', True)
        return GITHUB.RefFact('refs/tags/v' + version, 'commit', commit, digest(91))

    def publish_draft(self, release_id):
        self.mutations.append('publish_draft')
        self._fault('publish_draft', False)
        row = [r for r in self.rows if r['id'] == release_id][0]
        row['draft'] = False
        tag = row['tag']
        self.w.refs.setdefault('tags/' + tag, ('commit', row['target']))
        self.w.releases.append(dict(release_id=row['id'], tag_name=tag, target_commitish=row['target'], draft=False,
                                    prerelease=True, assets=()))
        self._fault('publish_draft', True)
        return self._fact(row)

    def draft_row(self):
        return [r for r in self.rows if r['tag'] == 'v' + self.w.v][0]
    def signs(self):
        return self.stage.rig.kms.signs


class Stage(object):
    """One release, one run, advanced job by job. Every step reads its input as the workflow gives it."""

    def __init__(self, world=None):
        self.w = world or release_world()
        self.rig = ctl.Rig(self.w)
        self.git = Git(self.w)
        self.rig.git = self.git
        self.releases = FakeReleases(self)
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.count = 0
        self.w.job(DISPATCH_RUN, 1, ELIGIBILITY_JOB_ID, PUBLISH.ELIGIBILITY_JOB)
        self.checker_calls = []
        self.install_report = None
        self.put_candidate_zip()

    def close(self):
        self.tmp.cleanup()

    def work(self):
        self.count += 1
        path = self.root / ('work-' + str(self.count))
        path.mkdir()
        return path

    def put_candidate_zip(self):
        path = self.root / 'candidate.zip'
        with zipfile.ZipFile(path, 'w') as z:
            for name, data in archive_files(self.w).items():
                z.writestr(name, data)
            z.writestr('SHA256SUMS', manifest_of(self.w))
            z.writestr('candidate.json', canon(self.w.candidate))
        self.git.paths[base.CAND_ART] = path

    def request(self, attempt=1):
        return self.w.request(dispatch_run_attempt=attempt)

    def transfers(self):
        return PUBLISH.Transfers(signature_id=SIG_ART, signature_digest=digest(21), draft_receipt_id=RECEIPT_ART,
                                 genuine=dict(GENUINE_ART))

    def fetch_crate(self, name):
        return CRATES[name]

    # -- jobs, in order ---------------------------------------------------------------------------

    def sign(self):
        """The reserve and signer jobs, through the controller, and their transfer."""
        self.rig.reserve_version()
        ctrl = self.rig.session()
        signature = self.rig.sign(ctrl)
        self.authorization = ctrl.authorization()
        self.transfer_signature(signature, self.authorization)
        return signature

    def transfer_signature(self, signature, authorization, attempt=1, art=SIG_ART):
        self.w.artifacts[art] = self.w.artifact(art, 'rdy-signature-a' + str(attempt), DISPATCH_RUN, digest(21), 512)
        self.w.members[(art, 'SHA256SUMS.sig')] = signature
        self.w.members[(art, 'authorization.json')] = authorization

    def draft(self, attempt=1):
        """The draft job: it gathers, syncs, reads back and transfers both artifacts."""
        result = PUBLISH.run_draft(self.git, self.releases, self.rig.enrollment, self.request(attempt), self.transfers(),
                                   DISPATCH_RUN, attempt, H, self.rig.verifier, self.fetch_crate, self.work())
        self.w.job(DISPATCH_RUN, attempt, DRAFT_JOB_ID + 1000 * (attempt - 1), PUBLISH.DRAFT_JOB)
        self.w.artifacts[RECEIPT_ART] = self.w.artifact(RECEIPT_ART, 'draft-receipt-a' + str(attempt), DISPATCH_RUN,
                                                        digest(22), 512)
        self.w.members[(RECEIPT_ART, 'draft-receipt.json')] = result.receipt
        self.w.artifacts[ASSETS_ART] = self.w.artifact(ASSETS_ART, 'draft-assets-a' + str(attempt), DISPATCH_RUN,
                                                       digest(23), 4096)
        path = self.root / ('assets-' + str(attempt) + '.zip')
        with zipfile.ZipFile(path, 'w') as z:
            for entry in sorted(result.folder.iterdir()):
                z.write(entry, entry.name)
        self.git.paths[ASSETS_ART] = path
        self.draft_result = result
        return result

    def checker(self, target):
        def run(argv, boundary):
            self.checker_calls.append(argv)
            download = Path(argv[argv.index('--download') + 1])
            self.seen_download = sorted(p.name for p in download.iterdir())
            return self.install_report or self.report(target)
        return run

    def report(self, target, **changes):
        value = {'binary': 'rdy', 'checks': [{'name': n, 'passed': True} for n in CANDIDATE.CHECK_NAMES],
                 'commit': S, 'os': 'test', 'skipped': 0, 'target': target, 'version': self.w.v}
        value.update(changes)
        return canon(value)

    def genuine(self, target, attempt=1):
        data = PUBLISH.genuine_check(self.git, self.rig.enrollment, self.request(attempt), self.transfers(), target, ASSETS_ART,
                                     DISPATCH_RUN, attempt, self.rig.verifier, self.checker(target), self.work(),
                                     {'PATH': '/usr/bin'})
        self.w.job(DISPATCH_RUN, attempt, GENUINE_JOB[target] + 1000 * (attempt - 1), PUBLISH.GENUINE_JOBS[target])
        art = GENUINE_ART[target]
        self.w.artifacts[art] = self.w.artifact(art, PUBLISH.GENUINE_ARTIFACT + target + '-a' + str(attempt), DISPATCH_RUN,
                                                digest(24), 512)
        self.w.members[(art, PUBLISH.GENUINE_MEMBER)] = data
        return data

    def both_genuine(self, attempt=1):
        self.genuine(MAC, attempt)
        self.genuine(LINUX, attempt)

    def final_tag(self, attempt=1):
        evidence = PUBLISH.run_final_tag(self.releases, self.rig.enrollment, self.request(attempt), self.transfers(),
                                         DISPATCH_RUN, attempt, H, self.rig.verifier, self.work())
        self.w.job(DISPATCH_RUN, attempt, TAG_JOB_ID + 1000 * (attempt - 1), PUBLISH.TAG_JOB)
        return evidence

    def promote(self, attempt=1):
        return PUBLISH.promote(self.git, self.releases, self.rig.enrollment, self.request(attempt), self.transfers(),
                               DISPATCH_RUN, attempt, H, self.rig.verifier, self.work())

    def ready(self):
        """Everything before the final tag."""
        self.sign()
        self.draft()
        self.both_genuine()
        return self

    def later_attempt(self, attempt=2):
        """A later attempt's eligibility and signer jobs: the signer recovers the retained bytes and transfers them."""
        self.w.runs.setdefault((DISPATCH_RUN, attempt), dict(self.w.runs[(DISPATCH_RUN, 1)], run_attempt=attempt))
        self.w.job(DISPATCH_RUN, attempt, ELIGIBILITY_JOB_ID + 1000 * (attempt - 1), PUBLISH.ELIGIBILITY_JOB)
        result = PUBLISH.reconcile(self.request(attempt), self.git, self.releases, self.rig.verifier, H, self.work())
        signer = self.rig.session(self.rig.rerun(attempt=attempt))
        retained = signer.recover(self.rig.tuple_bytes())
        self.transfer_signature(retained.signature, signer.authorization(), attempt=attempt)
        return result

    def public_check(self, receipt, target=LINUX, **changes):
        arguments = dict(checker=self.checker(target), verify=self.rig.verifier, work=self.work(),
                         env={'PATH': '/usr/bin'})
        arguments.update(changes)
        return PUBLISH.check_published(self.releases, receipt, target, **arguments)


def job_text(name):
    match = re.search(r'^  ' + re.escape(name) + r':\n((?:(?:    .*)?\n)+)', WORKFLOW, re.M)
    if match is None:
        raise AssertionError('no job ' + name)
    return match.group(1)


def job_ids():
    jobs = WORKFLOW[WORKFLOW.index('\njobs:\n'):]
    return re.findall(r'^  ([A-Za-z0-9_-]+):\n', jobs, re.M)


def permissions_of(name):
    match = re.search(r'^    permissions:\n((?:      .*\n)+)', job_text(name), re.M)
    if match is None:
        return {}
    return dict(re.findall(r'^      ([a-z-]+): (read|write)$', match.group(1), re.M))


def needs_of(name):
    match = re.search(r'^    needs: \[(.*)\]$', job_text(name), re.M)
    if match:
        return [n.strip() for n in match.group(1).split(',')]
    match = re.search(r'^    needs: (.*)$', job_text(name), re.M)
    return [match.group(1).strip()] if match else []


def reach(name):
    found = set()
    for need in needs_of(name):
        found.add(need)
        found |= reach(need)
    return found


class Base(unittest.TestCase):
    def stage(self, world=None):
        stage = Stage(world)
        self.addCleanup(stage.close)
        return stage

    def refused(self, text, call, *args, **kwargs):
        with self.assertRaises(ControllerError) as caught:
            call(*args, **kwargs)
        self.assertEqual(str(caught.exception), text)

    def no_publication(self, stage):
        self.assertNotIn('publish_draft', stage.releases.mutations)


class Caller(Base):
    def test_rsig7_caller_uses_candidate_ref_and_literal_signer_sha(self):
        # The one literal: the uses line, the callee input, every checkout and every script argument.
        self.assertIn('on:\n  workflow_dispatch:\n', WORKFLOW)
        for word in ('\n  push:', '\n  schedule:', '\n  workflow_call:', '\n  pull_request'):
            self.assertNotIn(word, WORKFLOW)
        uses = re.findall(r'uses: adrenickchrome/rdy-core/\.github/workflows/release-sign\.yml@(\S+)', WORKFLOW)
        self.assertEqual(len(uses), 1, 'one reusable call: the callee checks that the run references it once')
        literal = uses[0]
        self.assertRegex(literal, r'\A[0-9a-f]{40}\Z')
        self.assertEqual(job_text('sign').count('signer_sha: ' + literal + '\n'), 1)
        refs = re.findall(r'^\s+ref: (.*)$', WORKFLOW, re.M)
        self.assertTrue(refs)
        self.assertEqual(set(refs), {literal})
        arguments = re.findall(r'--signer-sha (\S+)', WORKFLOW)
        self.assertTrue(arguments)
        self.assertEqual(set(arguments), {literal})
        self.assertNotIn('@main', WORKFLOW)
        self.assertNotIn('uses: ./', WORKFLOW)
        self.assertNotIn('secrets: inherit', WORKFLOW)
        self.assertEqual(re.findall(r'^\s+repository: (.*)$', WORKFLOW, re.M), [REPO] * len(refs))
        self.assertEqual(WORKFLOW.count('persist-credentials: false'), len(refs))
        for line in re.findall(r'uses: (\S+)', WORKFLOW):
            self.assertRegex(line, r'@[0-9a-f]{40}\Z', 'every action and the callee are literal commits')

        # The lane covers the whole caller; the callee never locks it again.
        self.assertIn('concurrency:\n  group: rdy-core-release-authority\n  cancel-in-progress: false\n', WORKFLOW)
        self.assertNotIn('concurrency', SIGN_WORKFLOW.split('jobs:', 1)[1])
        self.assertNotIn('rdy-core-release-authority\n', SIGN_WORKFLOW.split('jobs:', 1)[1])

        # All permissions are at job scope; only the reusable call can pass OIDC.
        self.assertIn('\npermissions: {}\n', WORKFLOW)
        self.assertNotIn('always()', WORKFLOW)
        self.assertNotIn('continue-on-error', WORKFLOW)
        self.assertEqual(WORKFLOW.count('id-token: write'), 1)
        self.assertEqual(permissions_of('sign'), {'actions': 'read', 'contents': 'read', 'security-events': 'read',
                                                  'id-token': 'write'})
        self.assertEqual(permissions_of('eligibility'), {'actions': 'read', 'contents': 'read', 'security-events': 'read'})
        for name in job_ids():
            if name != 'sign':
                self.assertNotIn('uses: adrenickchrome/', job_text(name))
                self.assertIn('    timeout-minutes:', job_text(name))
        self.assertEqual(re.search(r'^    name: (.*)$', job_text('sign'), re.M).group(1), 'Sign')
        self.assertEqual(sorted(re.findall(r'^      (mode|signer_sha|request|reserve_artifact_id):', job_text('sign'), re.M)),
                         ['mode', 'request', 'reserve_artifact_id', 'signer_sha'])

        # The reservation job's frozen identity.
        self.assertEqual(CONTROLLER.RESERVE_JOB_KEY, 'reserve')
        reserve = job_text('reserve')
        self.assertEqual(re.search(r'^    name: (.*)$', reserve, re.M).group(1), 'Reserve release tuple')
        self.assertEqual(permissions_of('reserve'), {'actions': 'read', 'contents': 'write'})
        for word in ('matrix', 'strategy', 'secrets.', 'id-token', 'environment:', 'create-github-app-token'):
            self.assertNotIn(word, reserve)
        self.assertIn('name: reserve-receipt-${{ github.run_attempt }}\n', reserve)
        self.assertEqual(WORKFLOW_PATH.relative_to(ROOT).as_posix(), CONTROLLER.RESERVE_WORKFLOW)

        # A skeleton caller cannot do anything: the literal does not resolve, and the scripts refuse it first.
        if literal == PUBLISH.UNPINNED:
            self.assertEqual(set(uses), {PUBLISH.UNPINNED})
        for bad in (PUBLISH.UNPINNED, 'main', '2' * 39, 'A' * 40):
            with self.subTest(bad=bad):
                self.refused('not authorized: the release caller has no literal signer revision', PUBLISH.check_pin, bad)
        stage = self.stage()
        calls = len(stage.w.calls)
        self.refused('not authorized: the release caller has no literal signer revision', PUBLISH.reconcile,
                     stage.request(), stage.git, stage.releases, stage.rig.verifier, PUBLISH.UNPINNED, stage.work())
        self.assertEqual(len(stage.w.calls), calls, 'it refuses before any read')
        # The literal must be the policy's signer: the same refusal at every stage that acts.
        self.refused('not authorized: the policy names another signer revision', PUBLISH.reconcile, stage.request(), stage.git,
                     stage.releases, stage.rig.verifier, '4' * 40, stage.work())

        # Eligibility reads the dispatch run: it runs on the candidate ref, never main.
        stage = self.stage()
        stage.w.runs[(DISPATCH_RUN, 1)]['head_branch'] = 'main'
        with self.assertRaises(ControllerError) as caught:
            PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertIn('not authorized', str(caught.exception))

        # The dispatch inputs become one closed request. A bad input refuses without echoing its body.
        environ = {'RDY_INPUT_VERSION': '0.43.0', 'RDY_INPUT_CANDIDATE_RUN_ID': '5', 'RDY_INPUT_CANDIDATE_RUN_ATTEMPT': '1',
                   'RDY_INPUT_CANDIDATE_ARTIFACT_ID': '6', 'RDY_INPUT_EVIDENCE_RUN_ID': '7',
                   'RDY_INPUT_EVIDENCE_RUN_ATTEMPT': '1', 'RDY_INPUT_EVIDENCE_ARTIFACT_ID': '8',
                   'RDY_INPUT_EVIDENCE_SHA256': digest(1), 'RDY_INPUT_APPROVALS': '[]', 'GITHUB_RUN_ID': '9',
                   'GITHUB_RUN_ATTEMPT': '2'}
        request = PUBLISH.request_from_inputs(environ)
        self.assertEqual((request.version, request.dispatch_run_id, request.dispatch_run_attempt, request.approvals),
                         ('0.43.0', 9, 2, ()))
        self.refused('the input candidate_run_id is not a positive integer', PUBLISH.request_from_inputs,
                     dict(environ, RDY_INPUT_CANDIDATE_RUN_ID='5; curl hostile'))

    def test_rsig7_reserve_job_writes_only_the_reservation(self):
        stage = self.stage()
        stage.w.find_job(ctl.RESERVE_JOB_ID).update(status='in_progress', conclusion=None)
        # The eligibility job holds the alert read. The reserve job has no security-events permission, so it
        # never asks: it needs that job's success in this exact attempt.
        stage.git.open_code_scanning_alerts = mock.Mock(side_effect=AssertionError('the reserve job has no alert read'))
        data = PUBLISH.reserve_tuple(stage.request(), stage.git, DISPATCH_RUN, 1, H)
        receipt = json.loads(data)
        self.assertIs(receipt['created'], True)
        self.assertEqual(receipt['job_id'], str(ctl.RESERVE_JOB_ID))
        self.assertEqual(stage.git.created, ['tag object', 'ref'])

        # A later attempt reads the reservation and never writes it.
        again = json.loads(PUBLISH.reserve_tuple(stage.request(), stage.git, DISPATCH_RUN, 1, H))
        self.assertIs(again['created'], False)
        self.assertEqual(stage.git.created, ['tag object', 'ref'])

        # No passing eligibility job, no new reservation.
        stage = self.stage()
        stage.w.find_job(ctl.RESERVE_JOB_ID).update(status='in_progress', conclusion=None)
        stage.w.find_job(ELIGIBILITY_JOB_ID).update(conclusion='failure')
        with self.assertRaises(ControllerError) as caught:
            PUBLISH.reserve_tuple(stage.request(), stage.git, DISPATCH_RUN, 1, H)
        self.assertIn('the Release eligibility job did not pass', str(caught.exception))
        self.assertEqual(stage.git.created, [])

        # A matrix or a spoofed second job has no single identity.
        stage = self.stage()
        stage.w.find_job(ctl.RESERVE_JOB_ID).update(status='in_progress', conclusion=None)
        stage.w.job(DISPATCH_RUN, 1, 9988, CONTROLLER.RESERVE_JOB)
        stage.w.find_job(9988).update(status='in_progress', conclusion=None)
        self.refused('this run attempt has no single running reserve job', PUBLISH.reserve_tuple, stage.request(), stage.git,
                     DISPATCH_RUN, 1, H)
        self.assertEqual(stage.git.created, [])


class Draft(Base):
    def test_agi6_first_embedded_release_has_no_guide_assets(self):
        # The exact set has neither legacy guide nor a replacement: only archives, manifest, signature, crates.
        stage = self.stage()
        stage.sign()
        result = stage.draft()
        expected = sorted(list(archive_files(stage.w)) + ['SHA256SUMS', 'SHA256SUMS.sig'] + list(CRATES))
        row = stage.releases.draft_row()
        self.assertEqual(sorted(a['name'] for a in row['assets']), expected)
        self.assertEqual(sorted(p.name for p in result.folder.iterdir()), expected)
        self.assertEqual([a['name'] for a in json.loads(result.receipt)['assets']], expected)
        uploaded = [m for m in stage.releases.mutations if m.startswith('upload ')]
        forbidden = ('AGENTS.md', 'CLAUDE.md', 'agent-guide.json', 'agent-guide.sha256')
        for name in forbidden:
            self.assertNotIn('upload ' + name, uploaded)
        # The old assets at S are not read; S no longer holds them, and the set does not depend on it.
        read = [c[1] for c in stage.w.calls if c[0] == 'contents']
        self.assertNotIn('release/AGENTS.md', read)
        self.assertNotIn('release/CLAUDE.md', read)
        # Each of the four names is an unexpected asset of the draft, the old two and the new two.
        for name in forbidden:
            fresh = self.stage()
            fresh.sign()
            fresh.draft()
            fresh.releases.add_asset(fresh.releases.draft_row(), name, b'# Agent guide\n')
            before = list(fresh.releases.mutations)
            with self.subTest(name):
                with self.assertRaises(ControllerError) as caught:
                    fresh.draft()
                self.assertEqual(str(caught.exception), 'reservation conflict: the draft holds the unexpected asset ' + name)
                self.assertEqual(fresh.releases.mutations, before, 'a refusal writes nothing')
        # S may still hold the old files: the exact set does not read them or any other guide file.
        w = release_world()
        w.put(S, 'release/AGENTS.md', b'# Agent guide\n')
        w.put(S, 'release/CLAUDE.md', b'# Agent guide\n')
        release = mock.Mock(version='0.43.0', commit=S, first_machine_tag='v0.42.0', policy_commit=P,
                            transition_sha256=digest(7), manifest_sha256=w.manifest,
                            artifacts=tuple((f['name'], f['sha256'], f['size']) for f in w.archives),
                            candidate_run=(base.CAND_RUN, 1, base.CAND_ART, digest(5)))
        release.tuple_value.return_value = {'artifacts': w.archives}
        wants, files = PUBLISH.wanted_assets(Git(w), release, SIGNATURE)
        self.assertEqual(sorted(wants), expected)
        self.assertEqual(sorted(files), ['SHA256SUMS'])

    def test_rsig7_draft_target_body_and_asset_set_are_exact(self):
        stage = self.stage()
        stage.sign()
        result = stage.draft()
        row = stage.releases.draft_row()
        self.assertEqual((row['tag'], row['target'], row['draft'], row['prerelease'], row['name']),
                         ('v0.43.0', S, True, True, 'v0.43.0'))
        self.assertEqual(row['body'], stage.w.notes.decode())
        names = sorted(archive_files(stage.w)) + ['SHA256SUMS', 'SHA256SUMS.sig'] + sorted(CRATES)
        self.assertEqual(sorted(a['name'] for a in row['assets']), sorted(names))
        self.assertEqual(stage.releases.mutations.count('create_draft'), 1)
        self.assertEqual(sorted(p.name for p in result.folder.iterdir()), sorted(names))
        receipt = json.loads(result.receipt)
        self.assertEqual((receipt['release_id'], receipt['commit'], receipt['version'], receipt['tuple_sha256']),
                         (row['id'], S, '0.43.0', stage.rig.tuple_sha()))
        self.assertEqual(receipt['notes_sha256'], sha(stage.w.notes))
        self.assertEqual([a['name'] for a in receipt['assets']], sorted(names))
        for entry in receipt['assets']:
            self.assertEqual(stage.releases.blobs[entry['asset_id']], (result.folder / entry['name']).read_bytes())

        # Each change of an existing draft refuses. The job never deletes, replaces or edits anything.
        def change(field, value):
            def apply(s):
                s.releases.draft_row()[field] = value
            return apply

        def add_extra(name):
            def apply(s):
                s.releases.add_asset(s.releases.draft_row(), name, b'x')
            return apply

        def clobber(s):
            row = s.releases.draft_row()
            asset = [a for a in row['assets'] if a['name'] == 'SHA256SUMS'][0]
            s.releases.blobs[asset['asset_id']] = b'y' * asset['size']

        def resize(s):
            row = s.releases.draft_row()
            asset = [a for a in row['assets'] if a['name'] == 'imbl-sized-chunks-0.2.0.crate'][0]
            asset['size'] += 1

        def twice(s):
            s.releases.add_asset(s.releases.draft_row(), 'imbl-sized-chunks-0.2.0.crate', CRATES['imbl-sized-chunks-0.2.0.crate'])

        def incomplete(s):
            row = s.releases.draft_row()
            [a for a in row['assets'] if a['name'] == 'imbl-7.0.2.crate'][0]['state'] = 'starter'

        def second(s):
            s.releases.next_id += 1
            s.releases.rows.append(dict(s.releases.draft_row(), id=s.releases.next_id, assets=[]))

        for apply, text in (
                (change('target', '9' * 40), 'reservation conflict: the release of v0.43.0 does not target the reserved commit'),
                (change('body', 'other notes'), 'reservation conflict: the draft of v0.43.0 is not the exact draft'),
                (change('prerelease', False), 'reservation conflict: the draft of v0.43.0 is not the exact draft'),
                (change('name', 'other'), 'reservation conflict: the draft of v0.43.0 is not the exact draft'),
                (add_extra('extra.txt'), 'reservation conflict: the draft holds the unexpected asset extra.txt'),
                (add_extra('test-root.pub'), 'reservation conflict: the draft holds the unexpected asset test-root.pub'),
                (add_extra('allowed_signers'), 'reservation conflict: the draft holds the unexpected asset allowed_signers'),
                (clobber, 'reservation conflict: the draft asset SHA256SUMS is not its exact bytes'),
                (resize, 'reservation conflict: the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes'),
                (twice, 'reservation conflict: the draft holds imbl-sized-chunks-0.2.0.crate twice'),
                (incomplete, 'reservation conflict: the draft asset imbl-7.0.2.crate is incomplete'),
                (second, 'reservation conflict: more than one release names v0.43.0')):
            fresh = self.stage()
            fresh.sign()
            fresh.draft()
            apply(fresh)
            before = list(fresh.releases.mutations)
            with self.subTest(text=text):
                with self.assertRaises(ControllerError) as caught:
                    fresh.draft()
                self.assertEqual(str(caught.exception), text)
                self.assertEqual(fresh.releases.mutations, before, 'a refusal writes nothing')

        # A missing asset at promotion is a changed draft, not something to upload there.
        fresh = self.stage()
        fresh.ready()
        row = fresh.releases.draft_row()
        row['assets'] = [a for a in row['assets'] if a['name'] != 'imbl-7.0.2.crate']
        self.refused('reservation conflict: the draft lacks imbl-7.0.2.crate', fresh.final_tag)
        self.assertEqual(fresh.releases.mutations.count('upload imbl-7.0.2.crate'), 1)

        # The signature artifact supplies only verified bytes.
        for change_signature, text in (
                (lambda s: s.w.members.update({(SIG_ART, 'SHA256SUMS.sig'): b'other bytes'}),
                 'evidence mismatch: the transferred signature is not the authorized signature'),
                (lambda s: s.rig.verifier.__setattr__('good', set()),
                 'evidence mismatch: the transferred signature does not verify'),
                (lambda s: s.w.artifacts[SIG_ART].update(name='rdy-signature-a7'),
                 'evidence mismatch: the signature artifact is not from this run attempt'),
                (lambda s: s.w.artifacts[SIG_ART].update(head_sha='9' * 40),
                 'evidence mismatch: the signature artifact is not from this run attempt'),
                (lambda s: s.w.artifacts[SIG_ART].update(digest=digest(77)),
                 'evidence mismatch: the signature artifact is not the one the signer job uploaded')):
            fresh = self.stage()
            fresh.sign()
            change_signature(fresh)
            with self.subTest(text=text):
                self.refused(text, fresh.draft)
                self.assertEqual(fresh.releases.mutations, [])

        # Crates: exact digest, size and lock entry, from the authenticated candidate record.
        fresh = self.stage()
        fresh.sign()
        fresh.fetch_crate = lambda name: b'different bytes'
        self.refused('evidence mismatch: the source crate imbl-7.0.2.crate is not its reserved bytes', fresh.draft)
        self.assertEqual(fresh.releases.mutations, [])
        fresh = self.stage()
        fresh.sign()
        fresh.w.put(S, 'Cargo.lock', LOCK.replace(sha(CRATES['imbl-7.0.2.crate']).encode(), digest(5).encode()))
        self.refused('evidence mismatch: the source crate imbl-7.0.2.crate is not in Cargo.lock at the release commit',
                     fresh.draft)
        fresh = self.stage()
        fresh.sign()
        one = fresh.w.candidate
        for build in one['builds']:
            build['sources'] = build['sources'][:1]
        fresh.w.members[(base.CAND_ART, 'candidate.json')] = canon(one)
        fresh.put_candidate_zip()
        self.refused('evidence mismatch: the release does not name exactly two source crates', fresh.draft)

        # The first machine release adds exactly the transition pair; the JSON matches the trust digest.
        def first():
            return base.first_machine_world()
        transition = b'{"scheme":"filler"}\n'
        w = release_world(first)
        w.put(P, 'release-keys/transition-v0.42.0.json', transition)
        w.put(P, 'release-keys/transition-v0.42.0.json.sig', b'filler endorsement\n')
        release = mock.Mock(version='0.42.0', commit=S, first_machine_tag='v0.42.0', policy_commit=P,
                            transition_sha256=sha(transition), manifest_sha256=w.manifest,
                            artifacts=tuple((f['name'], f['sha256'], f['size']) for f in w.archives),
                            candidate_run=(base.CAND_RUN, 1, base.CAND_ART, digest(5)))
        release.tuple_value.return_value = {'artifacts': w.archives}
        wants, files = PUBLISH.wanted_assets(Git(w), release, SIGNATURE)
        self.assertEqual(sorted(wants), sorted(list(archive_files(w)) + [
            'SHA256SUMS', 'SHA256SUMS.sig', 'transition-v0.42.0.json',
            'transition-v0.42.0.json.sig'] + list(CRATES)))
        self.assertEqual(files['transition-v0.42.0.json'], transition)
        release.transition_sha256 = digest(1)
        self.refused('evidence mismatch: the published transition is not the reserved transition',
                     PUBLISH.wanted_assets, Git(w), release, SIGNATURE)

    def test_rsig7_draft_job_has_no_signing_or_publisher_authority(self):
        for name in ('draft', 'final-tag'):
            job = job_text(name)
            self.assertEqual(permissions_of(name), {'actions': 'read', 'contents': 'write'})
            self.assertNotIn('create-github-app-token', job)
            self.assertNotIn('secrets.', job)
            self.assertNotIn('id-token', job)
        self.assertIn('sign', needs_of('draft'))
        self.assertNotIn('environment:', job_text('draft'))
        self.assertNotIn('cargo ', job_text('draft'))
        # The signer has no release write: it stays in its own job, separate from the draft.
        self.assertNotIn('create_draft', (ROOT / 'scripts/release/sign.py').read_text())
        for word in ('contents: write', 'RDY_RELEASE_PUBLISHER', 'secrets:'):
            self.assertNotIn(word, SIGN_WORKFLOW)
        # A publication attempt from a read-only reader has no method to call.
        stage = self.stage()
        stage.sign()
        stage.draft()
        self.assertEqual(stage.releases.mutations.count('publish_draft'), 0)
        self.assertFalse(hasattr(stage.git, 'publish_draft'))


class Genuine(Base):
    def test_rsig7_both_native_genuine_checks_precede_publish(self):
        # Both native jobs, on the qualified native runners, come before the tag and the promotion.
        for target, name in ((MAC, 'genuine-mac'), (LINUX, 'genuine-linux')):
            job = job_text(name)
            self.assertIn('runs-on: ' + RUNS_ON[target] + '\n', job)
            self.assertEqual(permissions_of(name), {'actions': 'read', 'contents': 'read'})
            self.assertIn('    name: ' + PUBLISH.GENUINE_JOBS[target] + '\n', job)
            self.assertIn(name, needs_of('final-tag'))
            self.assertIn(name, needs_of('promote'))
            self.assertIn('draft', needs_of(name))
            self.assertIn('--target ' + target, job)
        self.assertIn('final-tag', needs_of('promote'))
        self.assertEqual(set(job_ids()) - reach('promote') - {'promote'}, {'published-mac', 'published-linux'})
        self.assertLessEqual({'sign', 'draft', 'genuine-mac', 'genuine-linux', 'final-tag', 'eligibility', 'reserve'},
                             reach('promote'))

        # The checker takes the exact bytes of the draft, with the version-selected key. Nothing rebuilds.
        stage = self.stage()
        stage.sign()
        stage.draft()
        data = stage.genuine(MAC)
        argv = stage.checker_calls[-1]
        for flag, want in (('--version', '0.43.0'), ('--commit', S), ('--target', MAC),
                           ('--signer-line', stage.w.trust['signer_line'])):
            self.assertEqual(argv[argv.index(flag) + 1], want)
        self.assertEqual(stage.seen_download, sorted([real_archive('0.43.0', MAC)['name'], 'SHA256SUMS', 'SHA256SUMS.sig']))
        receipt = json.loads(data)
        self.assertEqual((receipt['version'], receipt['commit'], receipt['target'], receipt['skipped']),
                         ('0.43.0', S, MAC, 0))
        self.assertEqual(receipt['checks'], [{'name': n, 'passed': True} for n in CANDIDATE.CHECK_NAMES])
        self.assertEqual(receipt['tuple_sha256'], stage.rig.tuple_sha())
        self.assertEqual(receipt['manifest_sha256'], stage.w.manifest)
        self.assertEqual(receipt['signature_sha256'], sha(SIGNATURE))
        self.assertEqual(receipt['release_id'], stage.releases.draft_row()['id'])
        self.assertEqual(receipt['draft_receipt_sha256'], sha(stage.draft_result.receipt))

        # A bad report refuses in the job itself.
        for report in (stage.report(MAC, skipped=1), stage.report(MAC, commit='9' * 40), stage.report(MAC, version='0.44.0'),
                       stage.report(MAC, checks=[{'name': n, 'passed': n != 'mcp'} for n in CANDIDATE.CHECK_NAMES]),
                       stage.report(MAC, checks=[{'name': 'signature', 'passed': True}])):
            stage.install_report = report
            with self.assertRaises(ControllerError):
                stage.genuine(MAC)
        stage.install_report = None

        # A changed archive in the transfer never reaches the checker.
        calls = len(stage.checker_calls)
        with zipfile.ZipFile(stage.git.paths[ASSETS_ART], 'r') as z:
            members = {n: z.read(n) for n in z.namelist()}
        for change, text in (
                ({real_archive('0.43.0', MAC)['name']: b'tampered'},
                 'evidence mismatch: the draft assets artifact is not the draft receipt'),
                ({'allowed_signers': b'test root'}, 'evidence mismatch: the draft assets artifact is not the draft receipt')):
            path = stage.root / 'changed.zip'
            with zipfile.ZipFile(path, 'w') as z:
                for name, value in dict(members, **change).items():
                    z.writestr(name, value)
            stage.git.paths[ASSETS_ART] = path
            with self.subTest(text=text):
                self.refused(text, stage.genuine, MAC)
        self.assertEqual(len(stage.checker_calls), calls)

        # Promotion needs both receipts, from both jobs of this exact attempt.
        def drop_job(target):
            def apply(s):
                s.w.jobs[(DISPATCH_RUN, 1)] = [j for j in s.w.jobs[(DISPATCH_RUN, 1)] if j['name'] != PUBLISH.GENUINE_JOBS[target]]
            return apply

        def fail_job(target):
            def apply(s):
                s.w.find_job(GENUINE_JOB[target]).update(conclusion='failure')
            return apply

        def skip_job(target):
            def apply(s):
                s.w.find_job(GENUINE_JOB[target]).update(conclusion='skipped')
            return apply

        def edit(who, **changes):
            def apply(s):
                key = (GENUINE_ART[who], PUBLISH.GENUINE_MEMBER)
                s.w.members[key] = canon(dict(json.loads(s.w.members[key]), **changes))
            return apply

        def wrong_artifact(s):
            s.w.artifacts[GENUINE_ART[LINUX]]['name'] = PUBLISH.GENUINE_ARTIFACT + MAC + '-a1'

        def missing(s):
            del s.w.artifacts[GENUINE_ART[MAC]]

        def old_attempt(s):
            s.w.artifacts[GENUINE_ART[MAC]]['name'] = PUBLISH.GENUINE_ARTIFACT + MAC + '-a9'
        name = PUBLISH.GENUINE_JOBS
        for apply, text in (
                (drop_job(MAC), 'eligibility incomplete: the ' + name[MAC] + ' job did not pass'),
                (drop_job(LINUX), 'eligibility incomplete: the ' + name[LINUX] + ' job did not pass'),
                (fail_job(LINUX), 'eligibility incomplete: the ' + name[LINUX] + ' job did not pass'),
                (skip_job(MAC), 'eligibility incomplete: the ' + name[MAC] + ' job did not pass'),
                (edit(MAC, skipped=1), 'evidence mismatch: the ' + MAC + ' genuine receipt is not all six checks'),
                (edit(LINUX, checks=[]), 'evidence mismatch: the ' + LINUX + ' genuine receipt is not all six checks'),
                (edit(MAC, commit='9' * 40), 'evidence mismatch: the ' + MAC + ' genuine receipt is not this release'),
                (edit(LINUX, version='0.44.0'), 'evidence mismatch: the ' + LINUX + ' genuine receipt is not this release'),
                (edit(MAC, tuple_sha256=digest(1)), 'evidence mismatch: the ' + MAC + ' genuine receipt is not this release'),
                (edit(LINUX, manifest_sha256=digest(1)), 'evidence mismatch: the ' + LINUX + ' genuine receipt is not this release'),
                (edit(MAC, signature_sha256=digest(1)), 'evidence mismatch: the ' + MAC + ' genuine receipt is not this release'),
                (edit(MAC, release_id=1), 'evidence mismatch: the ' + MAC + ' genuine receipt is not this release'),
                (edit(LINUX, draft_receipt_sha256=digest(1)), 'evidence mismatch: the ' + LINUX + ' genuine receipt is not this release'),
                (edit(MAC, target=LINUX), 'evidence mismatch: the ' + MAC + ' genuine receipt is not this release'),
                (wrong_artifact, 'evidence mismatch: the ' + LINUX + ' genuine receipt artifact is not from this run attempt'),
                (old_attempt, 'evidence mismatch: the ' + MAC + ' genuine receipt artifact is not from this run attempt'),
                (missing, 'eligibility incomplete: the ' + MAC + ' genuine receipt artifact is unavailable')):
            fresh = self.stage()
            fresh.ready()
            apply(fresh)
            with self.subTest(text=text):
                self.refused(text, fresh.final_tag)
                self.assertNotIn('create_final_tag', fresh.releases.mutations)
            # The same refusal holds if the tag already exists: the promotion checks it all again.
            fresh = self.stage()
            fresh.ready()
            fresh.final_tag()
            apply(fresh)
            with self.subTest(text=text, at='promote'):
                self.refused(text, fresh.promote)
                self.no_publication(fresh)

        # The final tag is created once, at S, only after the checks; a promotion needs it.
        stage = self.stage()
        stage.ready()
        self.assertNotIn('tags/v0.43.0', stage.w.refs)
        self.refused('reservation conflict: the final tag v0.43.0 is missing', stage.promote)
        self.no_publication(stage)
        stage.final_tag()
        self.assertEqual(stage.w.refs['tags/v0.43.0'], ('commit', S))
        receipt = json.loads(stage.promote())
        self.assertEqual(stage.releases.mutations.count('publish_draft'), 1)
        self.assertFalse(stage.releases.draft_row()['draft'])
        self.assertEqual(receipt['release_id'], stage.releases.draft_row()['id'])
        self.assertEqual(receipt['tuple_sha256'], stage.rig.tuple_sha())
        self.assertEqual(receipt['signer_line'], stage.w.trust['signer_line'])
        # The receipt stays outside E: E's bytes did not change.
        self.assertEqual(sha(stage.w.members[(base.EVID_ART, 'release-record.json')]), stage.w.evidence_sha)

        # A tag somewhere else stops; a lost create reads back; it never repeats.
        stage = self.stage()
        stage.ready()
        stage.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)
        with self.assertRaises(ControllerError):
            stage.final_tag()
        self.assertNotIn('create_final_tag', stage.releases.mutations)
        stage = self.stage()
        stage.ready()
        stage.releases.faults['create_final_tag'] = (True, 1)
        stage.final_tag()
        self.assertEqual(stage.releases.mutations.count('create_final_tag'), 1)
        stage = self.stage()
        stage.ready()
        stage.releases.faults['create_final_tag'] = (False, 1)
        self.refused('reservation conflict: the final tag outcome is unresolved; reconcile again before any creation',
                     stage.final_tag)
        self.assertEqual(stage.releases.mutations.count('create_final_tag'), 1)


class Drift(Base):
    def test_rsig7_drift_or_revocation_before_publish_refuses(self):
        def revoke(s):
            good = s.git.release_trust

            def trust(version, policy_commit, signing):
                if signing:
                    raise ControllerError('the key for v0.43.0 is revoked for new signing')
                return good(version, policy_commit, signing)
            s.git.release_trust = trust

        def trust_unavailable(s):
            s.git.release_trust = mock.Mock(side_effect=ControllerError('offline'))

        def admission(state):
            def apply(s):
                s.rig.install(state=state, holder=None if state == 'CLOSED' else {
                    'run_id': '1', 'run_attempt': '1', 'job_id': '1', 'grant_id': '00000000-0000-4000-8000-000000000009'})
            return apply

        def other_signer(s):
            s.rig.install(signer_workflow_sha='4' * 40)

        def rollback(s):
            s.rig.install(policy_commit=H)

        def asset(name, data):
            def apply(s):
                row = s.releases.draft_row()
                entry = [a for a in row['assets'] if a['name'] == name][0]
                s.releases.blobs[entry['asset_id']] = data
                entry['size'] = len(data)
            return apply

        def extra(s):
            s.releases.add_asset(s.releases.draft_row(), 'unexpected.bin', b'z')

        def replaced_asset(s):
            row = s.releases.draft_row()
            entry = [a for a in row['assets'] if a['name'] == 'SHA256SUMS.sig'][0]
            entry['asset_id'] = 99123
            s.releases.blobs[99123] = SIGNATURE

        def new_draft_id(s):
            row = s.releases.draft_row()
            old = row['id']
            row['id'] = old + 500

        def edit_notes(s):
            s.releases.draft_row()['body'] = 'changed notes'

        def edit_target(s):
            s.releases.draft_row()['target'] = '9' * 40

        def un_prerelease(s):
            s.releases.draft_row()['prerelease'] = False

        def moved_reservation(s):
            value = copy.deepcopy(s.w.expected_tuple())
            value['manifest_sha256'] = digest(1)
            s.w.tags[s.w.refs['tags/reserve/v0.43.0'][1]]['message_bytes'] = canon(value)

        def changed_e(s):
            s.w.members[(base.EVID_ART, 'release-record.json')] = canon(dict(s.w.e, audit={'reference': 'x', 'sha256': digest(1)}))

        def moved_tag(s):
            s.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)

        def moved_candidate(s):
            s.w.refs['tags/candidate/v0.43.0'] = ('commit', '9' * 40)

        def no_authorization(s):
            del s.w.members[(SIG_ART, 'authorization.json')]

        def other_tuple(s):
            key = (SIG_ART, 'authorization.json')
            s.w.members[key] = canon(dict(json.loads(s.w.members[key]), tuple_sha256=digest(1)))

        def changed_receipt(s):
            s.w.members[(RECEIPT_ART, 'draft-receipt.json')] = canon(dict(json.loads(s.w.members[(RECEIPT_ART, 'draft-receipt.json')]),
                                                                         notes_sha256=digest(3)))

        for apply, code in (
                (revoke, 'signer revoked: the key for v0.43.0 is revoked for new signing'),
                (trust_unavailable, None),
                (admission('CLOSED'), 'authority unavailable: admission is CLOSED'),
                (admission('DRAINING'), None),
                (other_signer, 'authority unavailable: the admission record names another signer'),
                (rollback, None),
                (asset('SHA256SUMS', b'other'), 'reservation conflict: the draft asset SHA256SUMS is not its exact bytes'),
                (asset('imbl-sized-chunks-0.2.0.crate', b'other'), 'reservation conflict: the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes'),
                (asset('imbl-7.0.2.crate', b'other'), 'reservation conflict: the draft asset imbl-7.0.2.crate is not its exact bytes'),
                (extra, 'reservation conflict: the draft holds the unexpected asset unexpected.bin'),
                (replaced_asset, 'reservation conflict: the draft changed after its receipt'),
                (new_draft_id, 'reservation conflict: the draft changed after its receipt'),
                (edit_notes, 'reservation conflict: the draft of v0.43.0 is not the exact draft'),
                (edit_target, None),
                (un_prerelease, 'reservation conflict: the draft of v0.43.0 is not the exact draft'),
                (moved_reservation, None),
                (changed_e, None),
                (moved_tag, None),
                (moved_candidate, None),
                (no_authorization, 'evidence mismatch: the signer authorization is unavailable'),
                (other_tuple, 'evidence mismatch: the signer authorization is not this tuple'),
                (changed_receipt, None)):
            for when in ('final_tag', 'promote'):
                fresh = self.stage()
                fresh.ready()
                if when == 'promote':
                    fresh.final_tag()
                elif apply is moved_tag:
                    continue
                apply(fresh)
                with self.subTest(change=apply.__name__, when=when):
                    with self.assertRaises(ControllerError) as caught:
                        getattr(fresh, when)()
                    if code is not None:
                        self.assertEqual(str(caught.exception), code)
                    self.no_publication(fresh)

        # A policy revocation of a founder receipt or the disabled channel does not touch a standing preview,
        # but a settled revocation of the signer does, even though the signature is already retained.
        fresh = self.stage()
        fresh.ready()
        fresh.final_tag()
        revoke(fresh)
        self.refused('signer revoked: the key for v0.43.0 is revoked for new signing', fresh.promote)

        # A prior attempt's receipt: anything different stops the release.
        stage = self.stage()
        stage.sign()
        stage.draft()
        prior = 9500
        stage.w.artifacts[prior] = stage.w.artifact(prior, 'draft-receipt-a1', DISPATCH_RUN - 1, digest(26), 512)
        value = json.loads(stage.draft_result.receipt)
        stage.w.members[(prior, 'draft-receipt.json')] = canon(dict(value, release_id=value['release_id'] + 1))
        stage.git.runs_of_release = [(DISPATCH_RUN - 1, 1)]
        self.refused('reservation conflict: the draft changed after a prior receipt', stage.draft)
        # An identical prior receipt is fine.
        stage.w.members[(prior, 'draft-receipt.json')] = canon(value)
        stage.draft()
        # An expired prior receipt says nothing; an unreadable one stops.
        stage.w.artifacts[prior]['expired'] = True
        stage.draft()
        stage.w.artifacts[prior]['expired'] = False
        del stage.w.members[(prior, 'draft-receipt.json')]
        with self.assertRaises(ControllerError):
            stage.draft()


class Retry(Base):
    def lost_ack(self):
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        stage.releases.faults['publish_draft'] = (True, 1)
        return stage

    def test_rsig7_retry_reuses_draft_signature_and_tuple(self):
        # A run that stopped after two uploads: the next attempt reuses them and uploads each missing asset once.
        stage = self.stage()
        stage.sign()
        signature_bytes = stage.w.members[(SIG_ART, 'SHA256SUMS.sig')]
        stage.releases.faults['upload imbl-7.0.2.crate'] = (False, 1)
        self.refused('reservation conflict: the upload of imbl-7.0.2.crate is unresolved; reconcile again before any upload',
                     stage.draft)
        first = list(stage.releases.mutations)
        self.assertEqual(first.count('create_draft'), 1)
        draft_id = stage.releases.draft_row()['id']
        kept = {a['name']: a['asset_id'] for a in stage.releases.draft_row()['assets']}
        self.assertEqual(sorted(kept), ['SHA256SUMS', 'SHA256SUMS.sig'])
        self.assertNotIn('imbl-7.0.2.crate', kept)
        stage.rig.w.runs.setdefault((DISPATCH_RUN, 2), dict(stage.w.runs[(DISPATCH_RUN, 1)], run_attempt=2))
        # The signer's second attempt recovers the retained signature with no second Sign. Its bytes are the same.
        signs = stage.rig.kms.signs
        self.assertEqual(signs, 1)
        stage.transfer_signature(signature_bytes, stage.authorization, attempt=2)
        stage.w.job(DISPATCH_RUN, 2, 9410, PUBLISH.ELIGIBILITY_JOB)
        stage.draft(attempt=2)
        second = stage.releases.mutations[len(first):]
        self.assertEqual(second, ['upload ' + n for n in sorted(set(stage_names(stage)) - set(kept))])
        self.assertEqual(stage.releases.draft_row()['id'], draft_id, 'the same draft')
        for name, asset_id in kept.items():
            self.assertEqual([a['asset_id'] for a in stage.releases.draft_row()['assets'] if a['name'] == name], [asset_id])
        self.assertEqual(stage.rig.kms.signs, 1)

        # A create whose acknowledgement was lost reads back; no second draft.
        stage = self.stage()
        stage.sign()
        stage.releases.faults['create_draft'] = (True, 1)
        stage.draft()
        self.assertEqual(stage.releases.mutations.count('create_draft'), 1)
        self.assertEqual(len(stage.releases.rows), 1)
        # An upload whose acknowledgement was lost reads back; never a second upload.
        stage = self.stage()
        stage.sign()
        stage.releases.faults['upload imbl-sized-chunks-0.2.0.crate'] = (True, 1)
        stage.draft()
        self.assertEqual(stage.releases.mutations.count('upload imbl-sized-chunks-0.2.0.crate'), 1)
        # A create that did not apply: a later attempt must reconcile. Nothing else happens now.
        stage = self.stage()
        stage.sign()
        stage.releases.faults['create_draft'] = (False, 1)
        self.refused('reservation conflict: the draft outcome is unresolved; reconcile again before any creation', stage.draft)
        self.assertEqual(stage.releases.mutations, ['create_draft'])

        # The lost publication acknowledgement inside the job: read, never send it again.
        stage = self.lost_ack()
        receipt = json.loads(stage.promote())
        self.assertEqual(stage.releases.mutations.count('publish_draft'), 1)
        self.assertEqual(receipt['release_id'], stage.releases.draft_row()['id'])
        # A publication that did not apply: it refuses and leaves the draft for a later reconcile.
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        stage.releases.faults['publish_draft'] = (False, 1)
        self.refused('reservation conflict: the publication outcome is unresolved; reconcile again before any publication',
                     stage.promote)
        self.assertEqual(stage.releases.mutations.count('publish_draft'), 1)

        # A retry dispatch after the lost acknowledgement, and after a later version published: read-only success.
        for later in (False, True):
            stage = self.lost_ack()
            stage.promote()
            if later:
                stage.w.releases.append(dict(release_id=900, tag_name='v0.44.0', target_commitish='7' * 40, draft=False,
                                             prerelease=True, assets=()))
            before = list(stage.releases.mutations)
            signs = stage.rig.kms.signs
            created = list(stage.git.created)
            stage.git.artifact_path = mock.Mock(side_effect=AssertionError('no artifact is needed'))
            result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
            with self.subTest(later=later):
                self.assertEqual(result.state, 'already_published_matching')
                self.assertIsInstance(result.terminal, CONTROLLER.AlreadyPublishedMatching)
                self.assertEqual(result.terminal.tuple_sha256, stage.rig.tuple_sha())
                self.assertEqual(result.terminal.signature_sha256, sha(SIGNATURE))
                self.assertEqual(stage.releases.mutations, before, 'zero release mutation')
                self.assertEqual(stage.rig.kms.signs, signs, 'zero Sign')
                self.assertEqual(stage.git.created, created)

        # The observation stands even with the channel disabled, and it reports the status apart.
        stage = self.lost_ack()
        stage.promote()
        stage.w.ep['approval_channel']['enabled'] = False
        stage.w.publish_policy(ctl.HEAD)
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual(result.state, 'already_published_matching')
        self.assertIs(result.terminal.channel_enabled, False)
        self.assertIs(result.terminal.signer_revoked, False)
        # A later revocation of the signer is reported apart: history, not a claim of current trust.
        stage = self.lost_ack()
        stage.promote()
        good = stage.git.release_trust

        def revoked(version, policy_commit, signing):
            if signing:
                raise ControllerError('the key for v0.43.0 is revoked for new signing')
            return good(version, policy_commit, signing)
        stage.git.release_trust = revoked
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual(result.state, 'already_published_matching')
        self.assertIs(result.terminal.signer_revoked, True)

        # An old published version, under an older signer, needs no pin match: it is not eligibility.
        stage = self.lost_ack()
        stage.promote()
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, '4' * 40, stage.work())
        self.assertEqual(result.state, 'already_published_matching')

        # Exact bytes, or no observation. Each of these stays read-only and never opens a signing path.
        def blob(name, data):
            def apply(s):
                row = s.releases.draft_row()
                entry = [a for a in row['assets'] if a['name'] == name][0]
                s.releases.blobs[entry['asset_id']] = data
            return apply

        def drop(name):
            def apply(s):
                row = s.releases.draft_row()
                row['assets'] = [a for a in row['assets'] if a['name'] != name]
            return apply

        def extra(s):
            s.releases.add_asset(s.releases.draft_row(), 'extra.txt', b'x')

        def bad_sig(s):
            s.rig.verifier.good = set()

        def notes(s):
            s.releases.draft_row()['body'] = 'edited'
        archive = real_archive('0.43.0', MAC)['name']
        for apply, text in (
                (blob(archive, b'different'), 'reservation conflict: the published archives differ from the reserved tuple'),
                (blob('SHA256SUMS', b'different'), 'reservation conflict: the published manifest differs from the reserved tuple'),
                (blob('imbl-sized-chunks-0.2.0.crate', b'different'), 'reservation conflict: the published asset set is not the expected set'),
                (blob('imbl-7.0.2.crate', b'different'), 'reservation conflict: the published asset set is not the expected set'),
                (extra, 'reservation conflict: the published asset set is not the expected set'),
                (drop('imbl-7.0.2.crate'), 'reservation conflict: the published asset set is not the expected set'),
                (drop('SHA256SUMS.sig'), 'eligibility incomplete: the published signature is missing'),
                (drop(archive), 'eligibility incomplete: the published archive ' + archive + ' is missing'),
                (bad_sig, 'reservation conflict: the published signature does not verify'),
                (notes, 'reservation conflict: the published notes differ from the release record')):
            stage = self.lost_ack()
            stage.promote()
            apply(stage)
            before = list(stage.releases.mutations)
            signs = stage.rig.kms.signs
            with self.subTest(text=text):
                self.refused(text, PUBLISH.reconcile, stage.request(), stage.git, stage.releases, stage.rig.verifier, H,
                             stage.work())
                self.assertEqual(stage.releases.mutations, before)
                self.assertEqual(stage.rig.kms.signs, signs)

        # The workflow stops at the terminal observation: nothing later can run.
        for name in ('reserve', 'draft', 'genuine-mac', 'genuine-linux', 'final-tag', 'promote', 'published-mac', 'published-linux'):
            self.assertIn('eligibility', reach(name))
        for name in ('reserve', 'draft'):
            self.assertIn("needs.eligibility.outputs.state == 'ready'", job_text(name))
        self.assertIn("needs.eligibility.outputs.state == 'ready'", job_text('sign'))

        # An unfinished older version stays eligible for recovery only, under current authority.
        stage = self.stage()
        stage.sign()
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual((result.state, result.mode), ('ready', 'recovery'))
        stage = self.stage()
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual((result.state, result.mode), ('ready', 'new'))
        # A later version shipped and this one has no reservation: nothing new.
        stage = self.stage()
        stage.w.releases.append(dict(release_id=900, tag_name='v0.44.0', target_commitish='7' * 40, draft=False,
                                     prerelease=True, assets=()))
        with self.assertRaises(ControllerError):
            PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())


class TagWindow(Base):
    def test_rsig7_tag_without_a_visible_release_resumes_a_retry(self):
        # A read-only token cannot list a draft. After the final tag and before the promotion lands, a new run reads
        # the tag at the reserved commit with no release. That is an unfinished release, not a conflict.
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        before = list(stage.releases.mutations)
        signs = stage.rig.kms.signs
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual((result.state, result.mode, result.terminal), ('ready', 'recovery', None))
        self.assertEqual(stage.releases.mutations, before)
        self.assertEqual(stage.rig.kms.signs, signs)
        # A second tag job in the same state changes nothing either.
        stage.final_tag()
        self.assertEqual(stage.releases.mutations.count('create_final_tag'), 1)
        # A tag somewhere else still stops the retry, before any credentials, Sign or write.
        stage.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)
        self.refused('reservation conflict: the tag v0.43.0 exists without its release', PUBLISH.reconcile, stage.request(),
                     stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual(stage.releases.mutations, before)
        self.assertEqual(stage.rig.kms.signs, signs)


class FinalTagRetry(Base):
    """A retry between the final tag and the publication. Every remote write goes to the fakes."""

    def interrupted(self):
        """A run that created the final tag and stopped before its promotion landed."""
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        return stage

    def snapshot(self, stage):
        return (list(stage.releases.mutations), stage.rig.kms.signs, list(stage.git.created),
                copy.deepcopy(stage.releases.rows), stage.w.refs.get('tags/v0.43.0'))

    def test_rsig11_new_reservation_with_existing_tag_still_refuses(self):
        text = 'reservation conflict: the tag v0.43.0 already exists'
        stage = self.stage()
        stage.w.refs['tags/v0.43.0'] = ('commit', S)
        self.refused(text, PUBLISH.reconcile, stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        stage.w.find_job(ctl.RESERVE_JOB_ID).update(status='in_progress', conclusion=None)
        self.refused(text, PUBLISH.reserve_tuple, stage.request(), stage.git, DISPATCH_RUN, 1, H)
        self.assertEqual(stage.git.created, [], 'no reservation object or ref')
        self.assertNotIn('tags/reserve/v0.43.0', stage.w.refs)

    def test_rsig11_retry_after_final_tag_makes_no_second_sign_and_promotes_the_same_draft(self):
        stage = self.interrupted()
        draft_id = stage.releases.draft_row()['id']
        assets = copy.deepcopy(stage.releases.draft_row()['assets'])
        tag = stage.w.refs['tags/v0.43.0']
        created = list(stage.git.created)
        self.assertEqual(stage.rig.kms.signs, 1)

        # Attempt 2: every job runs again and reads everything again.
        result = stage.later_attempt(2)
        self.assertEqual((result.state, result.mode), ('ready', 'recovery'))
        stage.w.job(DISPATCH_RUN, 2, 9499, CONTROLLER.RESERVE_JOB)
        stage.w.find_job(9499).update(status='in_progress', conclusion=None)
        receipt = json.loads(PUBLISH.reserve_tuple(stage.request(2), stage.git, DISPATCH_RUN, 2, H))
        self.assertIs(receipt['created'], False)
        stage.draft(attempt=2)
        stage.both_genuine(attempt=2)
        evidence = json.loads(stage.final_tag(attempt=2))
        self.assertEqual(evidence['result'], 'recorded_retry')
        published = json.loads(stage.promote(attempt=2))

        # One Sign, one draft, one tag, one flip, and the same draft id and assets.
        self.assertEqual(stage.rig.kms.signs, 1)
        self.assertEqual(stage.git.created, created)
        mutations = stage.releases.mutations
        self.assertEqual((mutations.count('create_draft'), mutations.count('create_final_tag'),
                          mutations.count('publish_draft')), (1, 1, 1))
        self.assertEqual(mutations[-1], 'publish_draft')
        self.assertEqual(len(stage.releases.rows), 1)
        self.assertEqual(published['release_id'], draft_id)
        self.assertEqual(evidence['release_id'], draft_id)
        self.assertFalse(stage.releases.draft_row()['draft'])
        self.assertEqual(stage.releases.draft_row()['assets'], assets)
        self.assertEqual(stage.w.refs['tags/v0.43.0'], tag)
        # The retried release is then the ordinary terminal observation.
        result = PUBLISH.reconcile(stage.request(2), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual(result.state, 'already_published_matching')
        self.assertEqual(result.terminal.release_id, draft_id)

    def test_rsig11_final_tag_job_accepts_only_the_exact_tag_with_no_write_and_records_it(self):
        stage = self.stage()
        stage.ready()
        created = json.loads(stage.final_tag())
        self.assertEqual(created, {'schema': 1, 'version': '0.43.0', 'commit': S, 'tuple_sha256': stage.rig.tuple_sha(),
                                   'release_id': stage.releases.draft_row()['id'], 'run_id': DISPATCH_RUN, 'run_attempt': 1,
                                   'result': 'created'})
        before = self.snapshot(stage)
        # The exact tag: no write, and the evidence says so.
        retry = json.loads(stage.final_tag())
        self.assertEqual(retry, dict(created, result='recorded_retry'))
        self.assertEqual(self.snapshot(stage), before)
        # An annotated tag at S reads through to S, and it is recorded the same way.
        name = format(0xfeed, '040x')
        stage.w.tags[name] = dict(sha=name, tag='v0.43.0', object_type='commit', object_sha=S, message_bytes=b'',
                                  sha256=digest(93))
        stage.w.refs['tags/v0.43.0'] = ('tag', name)
        self.assertEqual(json.loads(stage.final_tag())['result'], 'recorded_retry')
        self.assertEqual(stage.releases.mutations, before[0])

        # A tag at another commit refuses, and nothing moves it.
        stage.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)
        self.refused('reservation conflict: the tag v0.43.0 is not the reserved commit', stage.final_tag)
        self.assertEqual(stage.w.refs['tags/v0.43.0'], ('commit', '9' * 40))
        self.assertEqual(stage.releases.mutations, before[0])
        # The job's own read comes after its checks. A tag that moved during them is refused by that read.
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        before = list(stage.releases.mutations)
        real = PUBLISH._qualify

        def moved_during_the_checks(*args):
            ready = real(*args)
            stage.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)
            return ready
        with mock.patch.object(PUBLISH, '_qualify', moved_during_the_checks):
            self.refused('reservation conflict: the final tag v0.43.0 exists before this job created it', stage.final_tag)
        self.assertEqual(stage.releases.mutations, before)

        # All its checks come first: with the exact tag present, each failure is that check's own refusal.
        def drop_genuine(s):
            del s.w.members[(GENUINE_ART[MAC], PUBLISH.GENUINE_MEMBER)]

        def bad_signature(s):
            s.rig.verifier.good = set()

        def asset_bytes(s):
            row = s.releases.draft_row()
            entry = [a for a in row['assets'] if a['name'] == 'imbl-sized-chunks-0.2.0.crate'][0]
            s.releases.blobs[entry['asset_id']] = b'o' * entry['size']

        def closed(s):
            s.rig.install(state='CLOSED')
        for apply, text in (
                (drop_genuine, 'eligibility incomplete: the ' + MAC + ' genuine receipt artifact is unavailable'),
                (bad_signature, 'evidence mismatch: the transferred signature does not verify'),
                (asset_bytes, 'reservation conflict: the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes'),
                (closed, 'authority unavailable: admission is CLOSED')):
            fresh = self.interrupted()
            apply(fresh)
            before = self.snapshot(fresh)
            with self.subTest(apply=apply.__name__):
                self.refused(text, fresh.final_tag)
                self.assertEqual(self.snapshot(fresh), before)

        # The evidence reaches a durable per-attempt artifact, with no new permission. A job with no record fails.
        job = job_text('final-tag')
        self.assertEqual(permissions_of('final-tag'), {'actions': 'read', 'contents': 'write'})
        self.assertIn('--out "$RUNNER_TEMP/final-tag-evidence"', job)
        self.assertIn('name: ' + PUBLISH.FINAL_TAG_ARTIFACT + '${{ github.run_attempt }}\n', job)
        self.assertIn('path: ${{ runner.temp }}/final-tag-evidence/' + PUBLISH.FINAL_TAG_MEMBER + '\n', job)
        self.assertIn('if-no-files-found: error\n', job)
        stage = self.interrupted()
        out = stage.work() / 'evidence'
        with mock.patch.object(PUBLISH, '_start', lambda *_: (stage.releases, stage.work(), stage.rig.verifier)), \
                mock.patch.object(PUBLISH, '_enrollment', lambda *_: stage.rig.enrollment), \
                mock.patch.object(PUBLISH, '_parsed_request', lambda _: stage.request()), \
                mock.patch.object(PUBLISH, '_transfers', lambda _: stage.transfers()), \
                mock.patch.object(PUBLISH.sign, 'SshVerifier', lambda: stage.rig.verifier):
            args = mock.Mock(signer_sha=H, out=out)
            PUBLISH.cmd_final_tag(args, {'RDY_RELEASE_SETTINGS_READBACK': json.dumps(PUBLISH.SETTINGS_READBACK),
                                         'GITHUB_RUN_ID': str(DISPATCH_RUN), 'GITHUB_RUN_ATTEMPT': '1'})
        self.assertEqual(json.loads((out / PUBLISH.FINAL_TAG_MEMBER).read_bytes())['result'], 'recorded_retry')

    def test_rsig11_missing_or_changed_draft_refuses_and_creates_nothing(self):
        def missing(s):
            s.releases.rows.clear()

        def missing_asset(s):
            row = s.releases.draft_row()
            row['assets'] = [a for a in row['assets'] if a['name'] != 'imbl-7.0.2.crate']

        def extra_asset(s):
            s.releases.add_asset(s.releases.draft_row(), 'unexpected.bin', b'z')

        def asset_bytes(s):
            row = s.releases.draft_row()
            entry = [a for a in row['assets'] if a['name'] == 'imbl-sized-chunks-0.2.0.crate'][0]
            s.releases.blobs[entry['asset_id']] = b'o' * entry['size']

        def notes(s):
            s.releases.draft_row()['body'] = 'changed notes'

        def target(s):
            s.releases.draft_row()['target'] = '9' * 40

        def second(s):
            s.releases.rows.append(dict(s.releases.draft_row(), id=9999))

        def replaced(s):
            # The same bytes under another draft id: only the receipts can tell.
            s.releases.draft_row()['id'] += 500
        tag = 'the final tag v0.43.0 exists without its draft'
        changed = 'the draft changed after its receipt'
        for apply, draft, final in (
                (missing, tag, changed),
                (missing_asset, 'the draft lacks imbl-7.0.2.crate', 'the draft lacks imbl-7.0.2.crate'),
                (extra_asset, 'the draft holds the unexpected asset unexpected.bin',
                 'the draft holds the unexpected asset unexpected.bin'),
                (asset_bytes, 'the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes',
                 'the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes'),
                (notes, 'the draft of v0.43.0 is not the exact draft', 'the draft of v0.43.0 is not the exact draft'),
                (target, 'the release of v0.43.0 does not target the reserved commit', None),
                (second, 'more than one release names v0.43.0', None),
                (replaced, 'the draft changed after a prior receipt', changed)):
            fresh = self.interrupted()
            fresh.git.runs_of_release = [(DISPATCH_RUN, 1)]
            fresh.later_attempt(2)
            fresh.draft(attempt=2)
            fresh.both_genuine(attempt=2)
            apply(fresh)
            before = self.snapshot(fresh)
            # The draft job, which could write, is the one that must not.
            with self.subTest(apply=apply.__name__, job='draft'):
                self.refused('reservation conflict: ' + draft, fresh.draft, attempt=2)
                self.assertEqual(self.snapshot(fresh), before)
            # The later jobs refuse the same state, against this attempt's receipts.
            for name in ('final_tag', 'promote'):
                with self.subTest(apply=apply.__name__, job=name):
                    with self.assertRaises(ControllerError) as caught:
                        getattr(fresh, name)(attempt=2)
                    if final is not None:
                        self.assertEqual(str(caught.exception), 'reservation conflict: ' + final)
                    self.assertEqual(self.snapshot(fresh), before)

    def test_rsig11_revocation_still_stops_the_retry(self):
        def revoke(s):
            good = s.git.release_trust

            def trust(version, policy_commit, signing):
                if signing:
                    raise ControllerError('the key for v0.43.0 is revoked for new signing')
                return good(version, policy_commit, signing)
            s.git.release_trust = trust

        def closed(s):
            s.rig.install(state='CLOSED')
        for apply, text in ((revoke, 'signer revoked: the key for v0.43.0 is revoked for new signing'),
                            (closed, 'authority unavailable: admission is CLOSED')):
            stage = self.interrupted()
            stage.later_attempt(2)
            stage.draft(attempt=2)
            stage.both_genuine(attempt=2)
            apply(stage)
            before = self.snapshot(stage)
            for job in (stage.final_tag, stage.promote):
                with self.subTest(apply=apply.__name__, job=job.__name__):
                    self.refused(text, job, attempt=2)
                    self.assertEqual(self.snapshot(stage), before)
            # The signer of a later attempt stops too, with no Sign and no release write.
            stage.w.runs.setdefault((DISPATCH_RUN, 3), dict(stage.w.runs[(DISPATCH_RUN, 1)], run_attempt=3))
            with self.subTest(apply=apply.__name__, job='signer'):
                self.refused(text, lambda: stage.rig.session(stage.rig.rerun(attempt=3)))
                self.assertEqual(self.snapshot(stage), before)

    def test_rsig11_no_permission_timeout_or_ambiguous_visibility_is_no_proof_of_absence(self):
        failures = (('permission', GITHUB.Unauthorized('read', 'not authorized (HTTP 403)', 403)),
                    ('timeout', GITHUB.Uncertain('read', 'outcome unknown (timeout)')),
                    ('ambiguous', GITHUB.Malformed('read', 'the answer is malformed')))

        def reconciled(stage):
            return PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())

        def tag_read_fails(stage, error):
            real = stage.git.ref

            def ref(name):
                if name == 'tags/v0.43.0':
                    raise error
                return real(name)
            stage.git.ref = ref
        for name, error in failures:
            # The reader: a failed read of the releases or of the tag reports no state at all.
            for broken in ('releases', 'tag'):
                stage = self.interrupted()
                before = self.snapshot(stage)
                if broken == 'releases':
                    stage.git.releases = mock.Mock(side_effect=error)
                else:
                    tag_read_fails(stage, error)
                with self.subTest(name=name, broken=broken, job='eligibility'):
                    with self.assertRaises(ControllerError) as caught:
                        reconciled(stage)
                    self.assertIn(str(error), str(caught.exception))
                    self.assertEqual(self.snapshot(stage), before)
            # The writer: a failed list of the drafts creates nothing and flips nothing.
            stage = self.interrupted()
            stage.later_attempt(2)
            stage.draft(attempt=2)
            stage.both_genuine(attempt=2)
            before = self.snapshot(stage)
            stage.releases.full_releases = mock.Mock(side_effect=error)
            for job in (stage.draft, stage.final_tag, stage.promote):
                with self.subTest(name=name, job=job.__name__):
                    with self.assertRaises(ControllerError) as caught:
                        job(attempt=2)
                    self.assertIn(str(error), str(caught.exception))
                    self.assertEqual(self.snapshot(stage), before)
        # A token that lists nothing without saying why: a success with no draft is what a read-only token sees. It
        # is not absence either. The draft is still there, and no writer acts on that answer.
        stage = self.interrupted()
        stage.later_attempt(2)
        stage.draft(attempt=2)
        stage.both_genuine(attempt=2)
        before = self.snapshot(stage)
        stage.releases.full_releases = lambda: []
        self.refused('reservation conflict: the final tag v0.43.0 exists without its draft', stage.draft, attempt=2)
        for job in (stage.final_tag, stage.promote):
            self.refused('reservation conflict: the draft changed after its receipt', job, attempt=2)
        self.assertEqual(self.snapshot(stage), before)

        # The wire: only an authenticated 404 is absence. A 403, a 5xx and a timeout never read as no tag or no release.
        class Wire(object):
            def __init__(self, answer):
                self.answer = answer

            def request(self, method, path, body=None, limit=None):
                if path == '/repos/' + CANDIDATE.REPOSITORY:
                    return GITHUB.Response(200, {}, canon({'full_name': CANDIDATE.REPOSITORY, 'id': GITHUB.REPOSITORY_ID}))
                if isinstance(self.answer, Exception):
                    raise self.answer
                return GITHUB.Response(self.answer, {}, b'{}')

        def client(answer):
            return PUBLISH.Releases(Wire(answer), self.stage().work(), 'unused', pause=lambda attempt: None)
        self.assertIsNone(PUBLISH._final_tag_commit(client(404), '0.43.0'))
        for answer in (401, 403, 500, GITHUB.Timeout(), GITHUB.TransportFailure()):
            with self.subTest(answer=answer):
                with self.assertRaises(ControllerError) as caught:
                    PUBLISH._final_tag_commit(client(answer), '0.43.0')
                self.assertNotIsInstance(caught.exception, GITHUB.Absent)
                with self.assertRaises(ControllerError) as caught:
                    client(answer).full_releases()
                self.assertNotIsInstance(caught.exception, GITHUB.Absent)

    def test_rsig11_later_writer_keeps_the_hidden_draft_or_refuses_a_duplicate_or_mismatch(self):
        stage = self.interrupted()
        row = copy.deepcopy(stage.releases.draft_row())
        # The read-only reader sees the tag alone. The writer's token sees the draft that was hidden from it.
        self.assertEqual(ELIGIBILITY.preliminary_recovery(stage.request(), stage.git).publication,
                         ELIGIBILITY.Publication('final_tag', None))
        self.assertEqual(ELIGIBILITY.preliminary_recovery(stage.request(), stage.releases).publication,
                         ELIGIBILITY.Publication('draft', row['id']))
        stage.later_attempt(2)
        before = self.snapshot(stage)
        result = stage.draft(attempt=2)
        self.assertEqual(self.snapshot(stage), before, 'the writer reconciles with no write')
        self.assertEqual(stage.releases.draft_row(), row)
        receipt = json.loads(result.receipt)
        self.assertEqual((receipt['release_id'], receipt['commit']), (row['id'], row['target']))
        self.assertEqual(receipt['notes_sha256'], sha(row['body'].encode('utf-8')))
        self.assertEqual([a['asset_id'] for a in receipt['assets']],
                         [a['asset_id'] for a in sorted(row['assets'], key=lambda a: a['name'])])

        def duplicate(s):
            s.releases.rows.append(dict(s.releases.draft_row(), id=9999))

        def other_target(s):
            s.releases.draft_row()['target'] = '9' * 40

        def other_notes(s):
            s.releases.draft_row()['body'] = 'changed notes'

        def other_id(s):
            s.releases.draft_row()['id'] += 500

        def other_asset(s):
            row = s.releases.draft_row()
            entry = [a for a in row['assets'] if a['name'] == 'imbl-sized-chunks-0.2.0.crate'][0]
            s.releases.blobs[entry['asset_id']] = b'o' * entry['size']
        for change, text in ((duplicate, 'more than one release names v0.43.0'),
                             (other_target, 'the release of v0.43.0 does not target the reserved commit'),
                             (other_notes, 'the draft of v0.43.0 is not the exact draft'),
                             (other_id, 'the draft changed after a prior receipt'),
                             (other_asset, 'the draft asset imbl-sized-chunks-0.2.0.crate is not its exact bytes')):
            fresh = self.interrupted()
            fresh.git.runs_of_release = [(DISPATCH_RUN, 1)]
            fresh.later_attempt(2)
            change(fresh)
            before = self.snapshot(fresh)
            with self.subTest(change=change.__name__):
                self.refused('reservation conflict: ' + text, fresh.draft, attempt=2)
                self.assertEqual(self.snapshot(fresh), before)

    def test_rsig11_a_final_tag_gives_no_publication_authority(self):
        def stops(stage, job, *args, **kwargs):
            before = self.snapshot(stage)
            with self.assertRaises(ControllerError):
                job(*args, **kwargs)
            self.assertEqual(self.snapshot(stage), before)
            self.no_publication(stage)
        # A tag at S and the signature, with no draft: nothing to promote, and no job makes one.
        stage = self.stage()
        stage.sign()
        stage.w.refs['tags/v0.43.0'] = ('commit', S)
        stops(stage, stage.draft)
        self.assertEqual(stage.releases.rows, [])
        # The tag and the exact draft, with no genuine checks in this attempt.
        stage = self.interrupted()
        stage.later_attempt(2)
        stage.draft(attempt=2)
        stops(stage, stage.final_tag, attempt=2)
        stops(stage, stage.promote, attempt=2)
        # The tag and the exact draft, with only the earlier attempt's receipts: they name another attempt.
        stage = self.interrupted()
        stage.later_attempt(2)
        stops(stage, stage.final_tag, attempt=2)
        stops(stage, stage.promote, attempt=2)
        # The tag, the exact draft and every receipt, when current authority is gone.
        stage = self.interrupted()
        stage.rig.install(state='CLOSED')
        stops(stage, stage.final_tag)
        stops(stage, stage.promote)

    def published_and_gone(self):
        """A release that a run published and a later run read as published, and that has since gone."""
        stage = self.interrupted()
        stage.git.runs_of_release = [(DISPATCH_RUN, 1)]
        stage.promote()
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual(result.state, 'already_published_matching')
        stage.releases.rows.clear()
        stage.w.releases[:] = [r for r in stage.w.releases if r['tag_name'] != 'v0.43.0']
        return stage

    def evidence(self, stage):
        return copy.deepcopy(stage.w.artifacts), dict(stage.w.members), dict(stage.rig.table.items)

    def test_rsig11_published_release_that_has_gone_creates_no_replacement_and_keeps_the_evidence(self):
        def gone(stage):
            pass

        def gone_with_its_tag(stage):
            # The rules deny this. The prior receipt alone must still stop a replacement.
            del stage.w.refs['tags/v0.43.0']
        for variant, draft_text in ((gone, 'the final tag v0.43.0 exists without its draft'),
                                    (gone_with_its_tag, 'the draft changed after a prior receipt')):
            stage = self.published_and_gone()
            variant(stage)
            stage.later_attempt(2)
            before, kept = self.snapshot(stage), self.evidence(stage)
            with self.subTest(variant=variant.__name__, job='draft'):
                self.refused('reservation conflict: ' + draft_text, stage.draft, attempt=2)
            for job in (stage.final_tag, stage.promote):
                with self.subTest(variant=variant.__name__, job=job.__name__):
                    with self.assertRaises(ControllerError):
                        job(attempt=2)
            self.assertEqual(self.snapshot(stage), before)
            self.assertEqual(self.evidence(stage), kept)
            self.assertEqual(stage.releases.mutations.count('create_draft'), 1)
            self.assertEqual(stage.rig.kms.signs, 1)
        # The same bytes in a new draft are no recovery: only the prior receipt can tell, by the draft's id.
        stage = self.published_and_gone()
        stage.later_attempt(2)
        stage.releases.create_draft('v0.43.0', S, 'v0.43.0', stage.draft_result.draft.body)
        for entry in stage.draft_result.draft.assets:
            stage.releases.add_asset(stage.releases.draft_row(), entry.name, (stage.draft_result.folder / entry.name).read_bytes())
        before = self.snapshot(stage)
        self.refused('reservation conflict: the draft changed after a prior receipt', stage.draft, attempt=2)
        self.assertEqual(self.snapshot(stage), before)

    def test_rsig11_a_receipted_draft_is_never_created_or_filled_again(self):
        # No tag yet. A receipt says the draft was complete, so a missing draft or asset is a changed draft.
        def removed(s):
            s.releases.rows.clear()

        def lost_an_asset(s):
            row = s.releases.draft_row()
            row['assets'] = [a for a in row['assets'] if a['name'] != 'imbl-7.0.2.crate']
        for change, text in ((removed, 'the draft changed after a prior receipt'), (lost_an_asset, 'the draft lacks imbl-7.0.2.crate')):
            stage = self.stage()
            stage.sign()
            stage.draft()
            stage.git.runs_of_release = [(DISPATCH_RUN, 1)]
            change(stage)
            before = list(stage.releases.mutations)
            with self.subTest(change=change.__name__):
                self.refused('reservation conflict: ' + text, stage.draft)
                self.assertEqual(stage.releases.mutations, before)


class FinalTagSigner(sgn.Base):
    """The real signer order and adapter over the journal, with a final tag at the reserved commit."""

    def tagged(self):
        self.rig.w.refs['tags/v0.43.0'] = ('commit', S)

    def test_rsig11_signer_after_final_tag_reuses_the_retained_signature(self):
        rig = self.rig
        rig.reserve_version()
        first = sgn.Inner(rig)
        rig.run(rig.controller(), first)
        self.assertEqual(first.signs, 1)
        self.tagged()
        second = sgn.Inner(rig)
        outcome = rig.run(rig.later(2, 'unknown'), second)
        self.assertEqual(outcome.result, 'already verified')
        self.assertEqual(outcome.signature, sgn.ARMORED)
        self.assertEqual(second.calls, ['get-public-key'])

    def test_rsig11_an_attempt_with_no_retained_bytes_stays_unknown(self):
        rig = self.rig
        rig.reserve_version()
        with self.assertRaises(sgn.Death):
            rig.run(rig.controller(), sgn.Inner(rig, die_at='after dispatch'))
        self.tagged()
        later = sgn.Inner(rig)
        with self.assertRaises(ControllerError) as caught:
            rig.run(rig.later(2, 'unknown'), later)
        self.assertEqual(caught.exception.code, CONTROLLER.UNKNOWN)
        self.assertEqual(later.signs, 0)

    def test_rsig11_a_final_tag_gives_no_first_sign(self):
        # A tag at the reserved commit before any attempt did not come from this path. It cannot start signing: the
        # state gives no new authority. The refusal keeps its old text and leaves the reservation as it was.
        rig = self.rig
        rig.reserve_version()
        self.tagged()
        inner = sgn.Inner(rig)
        with self.assertRaises(ControllerError) as caught:
            rig.run(rig.controller(), inner)
        self.assertEqual(str(caught.exception), 'reservation conflict: the tag v0.43.0 exists without its release')
        self.assertEqual(inner.signs, 0)
        self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
        self.assertEqual(rig.row()['phase'], 'RESERVED')

    def test_rsig11_a_visible_draft_beside_the_tag_gives_no_first_sign(self):
        # The signer's token cannot list a draft. One that could still reads the tag: the state is not what forbids it.
        rig = self.rig
        rig.reserve_version()
        self.tagged()
        rig.w.releases.append({'release_id': 302, 'tag_name': 'v0.43.0', 'target_commitish': S, 'draft': True,
                               'prerelease': True, 'assets': ()})
        inner = sgn.Inner(rig)
        with self.assertRaises(ControllerError) as caught:
            rig.run(rig.controller(), inner)
        self.assertEqual(str(caught.exception), 'reservation conflict: the tag v0.43.0 exists without its release')
        self.assertEqual(inner.signs, 0)
        self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)

    def test_rsig11_unknown_attempt_history_refuses_with_a_final_tag(self):
        # No attempt record, a tag at S and history that cannot prove the earlier run stopped. The answer is unknown,
        # not a conflict that a later run could read as proof that nothing was sent.
        rig = self.rig
        rig.reserve_version()
        rig.session()
        self.tagged()
        for attempt, change in (
                (2, lambda: setattr(rig.history, 'error', ControllerError('the history read failed'))),
                (3, lambda: setattr(rig.history, 'answer', CONTROLLER.History(complete=False, invocations=()))),
                (4, lambda: None)):
            ctrl = rig.later(attempt, 'unknown')
            change()
            inner = sgn.Inner(rig)
            with self.subTest(attempt=attempt):
                with self.assertRaises(ControllerError) as caught:
                    rig.run(ctrl, inner)
                self.assertEqual(caught.exception.code, CONTROLLER.UNKNOWN)
                self.assertEqual(inner.signs, 0)
                self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
                self.assertEqual(rig.row()['phase'], 'RESERVED')
            rig.history.error = None

    def test_rsig11_a_failed_tag_read_gives_no_first_sign(self):
        rig = self.rig
        rig.reserve_version()
        real = rig.git.ref
        errors = (GITHUB.Unauthorized('ref', 'not authorized (HTTP 403)', 403), GITHUB.Uncertain('ref', 'outcome unknown (timeout)'))
        for attempt, error in enumerate(errors, 2):
            def ref(name, error=error):
                if name == 'tags/v0.43.0':
                    raise error
                return real(name)
            ctrl = rig.later(attempt)
            rig.git.ref = ref
            inner = sgn.Inner(rig)
            with self.subTest(error=str(error)):
                with self.assertRaises(ControllerError):
                    rig.run(ctrl, inner)
                self.assertEqual(inner.signs, 0)
                self.assertNotIn((rig.tuple_pk(), 'ATTEMPT'), rig.table.items)
                # The read right before the attempt says the same, not "no tag".
                with self.assertRaises(ControllerError) as caught:
                    CONTROLLER._final_tag_present(rig.git, '0.43.0')
                self.assertEqual(str(caught.exception), 'signing outcome unknown: the final tag read is unavailable')
            rig.git.ref = real
        self.assertIs(CONTROLLER._final_tag_present(rig.git, '0.43.0'), False)
        self.tagged()
        self.assertIs(CONTROLLER._final_tag_present(rig.git, '0.43.0'), True)


def stage_names(stage):
    return list(archive_files(stage.w)) + ['SHA256SUMS', 'SHA256SUMS.sig'] + list(CRATES)


class FakeResponse(io.BytesIO):
    def __init__(self, status, body=b'', headers=None):
        io.BytesIO.__init__(self, body)
        self.code = status
        self.headers = headers or {}

    def getcode(self):
        return self.code


class FakeOpener(object):
    """Records each Request. It answers from a list; a test reads the headers it was given."""

    def __init__(self, answers):
        self.answers = list(answers)
        self.requests = []

    def open(self, request, timeout=None):
        self.requests.append(request)
        return self.answers.pop(0)


def repository_answer():
    return FakeResponse(200, json.dumps({'id': base.REPO_ID, 'full_name': REPO}).encode())


class Publisher(Base):
    def test_rsig7_publish_uses_only_the_publisher_app(self):
        # The workflow: the one token mint, in the one qualified job, in environment release.
        self.assertEqual(WORKFLOW.count('create-github-app-token'), 1)
        promote = job_text('promote')
        self.assertIn('    environment: release\n', promote)
        self.assertEqual(permissions_of('promote'), {'actions': 'read', 'contents': 'read'})
        self.assertIn('    name: ' + PUBLISH.PROMOTE_JOB + '\n', promote)
        token = re.search(r'uses: actions/create-github-app-token@[0-9a-f]{40}.*\n        with:\n((?:          .*\n)+)', promote)
        self.assertEqual(dict(re.findall(r'^\s+([a-z-]+): (.*)$', token.group(1), re.M)),
                         {'app-id': '${{ vars.RDY_RELEASE_PUBLISHER_APP_ID }}',
                          'private-key': '${{ secrets.RDY_RELEASE_PUBLISHER_APP_PRIVATE_KEY }}',
                          'owner': 'adrenickchrome', 'repositories': 'rdy-core', 'permission-contents': 'write'})
        self.assertEqual(re.findall(r'secrets\.[A-Z_]+', WORKFLOW), ['secrets.RDY_RELEASE_PUBLISHER_APP_PRIVATE_KEY'])
        self.assertEqual(WORKFLOW.count('RDY_PUBLISHER_TOKEN'), 1)
        for name in job_ids():
            if name != 'promote':
                self.assertNotIn('RDY_RELEASE_PUBLISHER', job_text(name))
                self.assertNotIn('steps.token', job_text(name))
        self.assertNotIn('gh release', WORKFLOW)
        self.assertNotIn('--clobber', WORKFLOW)
        # No candidate code runs beside the App key: no cargo, no candidate download, only the trusted scripts.
        for word in ('cargo ', 'candidate', 'rustup', './'):
            self.assertNotIn(word, promote.split('steps:', 1)[1].replace('trusted/', ''))
        self.assertLess(promote.index('ref: '), promote.index('create-github-app-token'))

        # The script: only the publisher token can publish; there is no fallback.
        stage = self.stage()
        self.refused('the promotion has no publisher App token', PUBLISH.publisher_client,
                     {'GITHUB_TOKEN': 'built-in-value', 'GH_TOKEN': 'other'}, stage.work(), 'verifier')
        client = PUBLISH.publisher_client({'GITHUB_TOKEN': 'built-in-value', 'RDY_PUBLISHER_TOKEN': 'app-value'}, stage.work(),
                                          'verifier')
        opener = FakeOpener([repository_answer(), FakeResponse(200, json.dumps(release_row(draft=False)).encode())])
        client.transport._opener = opener
        client.pause = lambda attempt: None
        published = client.publish_draft(4242)
        self.assertFalse(published.draft)
        patch = opener.requests[-1]
        self.assertEqual((patch.get_method(), patch.full_url, patch.data),
                         ('PATCH', 'https://api.github.com/repos/' + REPO + '/releases/4242', b'{"draft":false}'))
        self.assertEqual(patch.get_header('Authorization'), 'Bearer app-value')
        for request in opener.requests:
            self.assertNotIn('built-in-value', str(request.headers))

        # Only a draft flip: the body names nothing else, and an uncertain answer asks for a reconcile.
        for answer in (FakeResponse(422), FakeResponse(409), FakeResponse(500), FakeResponse(429)):
            client.transport._opener = FakeOpener([answer])
            with self.assertRaises(GITHUB.ReconcileRequired):
                client.publish_draft(4242)
        client.transport._opener = FakeOpener([FakeResponse(200, json.dumps(release_row(draft=True)).encode())])
        with self.assertRaises(GITHUB.ReconcileRequired):
            client.publish_draft(4242)
        client.transport._opener = FakeOpener([FakeResponse(403, b'{}')])
        with self.assertRaises(ControllerError):
            client.publish_draft(4242)

        # A signing job has no App, and the promotion has no signing path.
        self.assertNotIn('secrets', SIGN_WORKFLOW.split('jobs:', 1)[1])
        source = (ROOT / 'scripts/release/publish.py').read_text()
        for word in ('before_sign', 'load_adapter', 'kms', 'request_oidc', 'assume_role', 'delete_', "'DELETE'"):
            self.assertNotIn(word, source)

        # The activation readback: exact, sanitized and required at both jobs that change refs or the release.
        good = copy.deepcopy(PUBLISH.SETTINGS_READBACK)
        PUBLISH.check_settings_readback(json.dumps(good))
        for text in ('', 'not json'):
            self.refused('not authorized: the publisher settings readback does not match the qualified settings',
                         PUBLISH.check_settings_readback, text)
        self.assertEqual(good['publisher']['installed_repositories'], [REPO])
        self.assertEqual(good['publisher']['permissions'], {'contents': 'write', 'metadata': 'read'})
        self.assertIs(good['publisher']['signing_grant'], False)
        self.assertEqual(good['environment'], 'release')
        self.assertEqual(good['rulesets']['main'], {'delete': 'denied', 'force_push': 'denied', 'publisher_bypass': False})
        self.assertEqual(sorted(good['rulesets']['tags']), ['candidate/*', 'reserve/*', 'v*'])
        for rule in good['rulesets']['tags'].values():
            self.assertEqual(rule, {'delete': 'denied', 'update': 'denied', 'publisher_bypass': False})

        def mutate(path, value):
            changed = copy.deepcopy(PUBLISH.SETTINGS_READBACK)
            target = changed
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            return changed
        for changed in (mutate(('publisher', 'installed_repositories'), [REPO, 'rdyhq/rdy-web']),
                        mutate(('publisher', 'permissions'), {'contents': 'write', 'metadata': 'read', 'workflows': 'write'}),
                        mutate(('publisher', 'signing_grant'), True),
                        mutate(('environment',), 'other'),
                        mutate(('rulesets', 'main', 'publisher_bypass'), True),
                        mutate(('rulesets', 'main', 'force_push'), 'allowed'),
                        mutate(('rulesets', 'tags', 'reserve/*', 'publisher_bypass'), True),
                        mutate(('rulesets', 'tags', 'v*', 'delete'), 'allowed'),
                        dict(PUBLISH.SETTINGS_READBACK, extra=1)):
            with self.subTest(changed=changed):
                self.refused('not authorized: the publisher settings readback does not match the qualified settings',
                             PUBLISH.check_settings_readback, json.dumps(changed))
        self.assertIn('RDY_RELEASE_SETTINGS_READBACK: ${{ vars.RDY_RELEASE_SETTINGS_READBACK }}', promote)
        self.assertIn('RDY_RELEASE_SETTINGS_READBACK: ${{ vars.RDY_RELEASE_SETTINGS_READBACK }}', job_text('final-tag'))
        self.assertIn('    environment: release\n', job_text('final-tag'))

        # The promotion reads drafts only through the App's view: the built-in read token cannot see them.
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        self.assertEqual([r.tag_name for r in stage.git.releases() if r.tag_name == 'v0.43.0'], [])
        stage.promote()
        self.assertEqual(stage.releases.mutations.count('publish_draft'), 1)


def release_row(draft=True, assets=(), body='notes', tag='v0.43.0', ident=4242):
    return {'id': ident, 'tag_name': tag, 'target_commitish': S, 'name': tag, 'draft': draft, 'prerelease': True,
            'body': body, 'assets': list(assets)}


class Public(Base):
    def published(self):
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        return stage, stage.promote()

    def test_rsig7_published_checks_are_anonymous_and_byte_exact(self):
        # The workflow: no token, no secret, no environment in a public check.
        for target, name in ((MAC, 'published-mac'), (LINUX, 'published-linux')):
            job = job_text(name)
            self.assertIn('runs-on: ' + RUNS_ON[target] + '\n', job)
            self.assertEqual(permissions_of(name), {'contents': 'read'})
            for word in ('GITHUB_TOKEN', 'github.token', 'secrets.', 'RDY_PUBLISHER', 'environment:', 'GH_TOKEN'):
                self.assertNotIn(word, job)
            self.assertIn('promote', needs_of(name))
            self.assertIn('--target ' + target, job)

        # The transport: no Authorization header, ever; no proxy from the environment; GET only.
        with mock.patch.dict('os.environ', {'GITHUB_TOKEN': 'ambient', 'https_proxy': 'http://proxy.invalid:1',
                                            'HTTPS_PROXY': 'http://proxy.invalid:1'}):
            transport = PUBLISH.AnonymousTransport()
        handlers = [type(h).__name__ for h in transport._opener.handlers]
        self.assertNotIn('HTTPBasicAuthHandler', handlers)
        for handler in transport._opener.handlers:
            self.assertEqual(getattr(handler, 'proxies', {}), {})
        opener = FakeOpener([repository_answer(), FakeResponse(200, json.dumps(release_row(draft=False)).encode()),
                             FakeResponse(302, b'', {'location': 'https://objects.example/x'}),
                             FakeResponse(200, b'exact bytes')])
        transport._opener = opener
        client = PUBLISH.Releases(transport, self.stage().work(), 'verifier')
        client.pause = lambda attempt: None
        self.assertEqual(client.public_release('v0.43.0').release_id, 4242)
        folder = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, str(folder))
        self.assertEqual(client.public_asset('v0.43.0', 'SHA256SUMS', folder / 'out', 1024), (sha(b'exact bytes'), 11))
        self.assertEqual((folder / 'out').read_bytes(), b'exact bytes')
        for request in opener.requests:
            self.assertIsNone(request.get_header('Authorization'))
            self.assertEqual(request.get_method(), 'GET')
        self.assertEqual(opener.requests[2].full_url,
                         'https://github.com/' + REPO + '/releases/download/v0.43.0/SHA256SUMS')
        self.assertEqual(opener.requests[3].full_url, 'https://objects.example/x')
        for bad in (lambda: transport.request('POST', '/x', b'{}'), lambda: transport.request('PATCH', '/x', b'{}'),
                    lambda: transport.request('DELETE', '/x')):
            with self.assertRaises(ControllerError):
                bad()
        # A redirect to plain HTTP or a 404 is not a success.
        for answers in ([FakeResponse(302, b'', {'location': 'http://objects.example/x'})], [FakeResponse(404, b'gone')]):
            transport._opener = FakeOpener(answers)
            target = folder / ('bad-' + str(len(answers[0].headers)))
            with self.assertRaises(ControllerError):
                client.public_asset('v0.43.0', 'SHA256SUMS', target, 1024)
            self.assertFalse(target.exists())
        # The environment guard.
        for name in ('GITHUB_TOKEN', 'GH_TOKEN', 'RDY_PUBLISHER_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN', 'AWS_SESSION_TOKEN'):
            self.refused('the public checks must run without credentials', PUBLISH.check_anonymous_env, {name: 'x'})
        PUBLISH.check_anonymous_env({'PATH': '/usr/bin', 'RUNNER_TEMP': '/tmp'})

        # A good release: every asset, by exact bytes, and the native quick start.
        stage, receipt = self.published()
        data = stage.public_check(receipt)
        value = json.loads(data)
        self.assertEqual((value['version'], value['commit'], value['tuple_sha256'], value['release_id'], value['target']),
                         ('0.43.0', S, stage.rig.tuple_sha(), stage.releases.draft_row()['id'], LINUX))
        self.assertEqual(value['notes_sha256'], sha(stage.w.notes))
        self.assertEqual(sorted(value['assets']), sorted(stage_names(stage)))
        self.assertEqual(value['checks'], [{'name': n, 'passed': True} for n in CANDIDATE.CHECK_NAMES])
        argv = stage.checker_calls[-1]
        self.assertEqual(argv[argv.index('--signer-line') + 1], stage.w.trust['signer_line'])
        before = list(stage.releases.mutations)

        # Each difference is an incident: a refusal, and still no write.
        def blob(name, data):
            def apply(s):
                row = s.releases.draft_row()
                entry = [a for a in row['assets'] if a['name'] == name][0]
                s.releases.blobs[entry['asset_id']] = data
            return apply

        def drop(name):
            def apply(s):
                row = s.releases.draft_row()
                row['assets'] = [a for a in row['assets'] if a['name'] != name]
            return apply

        def extra(s):
            s.releases.add_asset(s.releases.draft_row(), 'extra.txt', b'x')

        def body(s):
            s.releases.draft_row()['body'] = 'edited'

        def tag(s):
            s.w.refs['tags/v0.43.0'] = ('commit', '9' * 40)

        def verify(s):
            s.rig.verifier.good = set()

        def un_publish(s):
            s.releases.draft_row()['draft'] = True

        def another(s):
            s.releases.draft_row()['id'] += 1
        incident = 'published v0.43.0 is an incident: '
        for apply, text in (
                (blob('SHA256SUMS', b'other'), 'SHA256SUMS is not its exact bytes'),
                (blob(real_archive('0.43.0', LINUX)['name'], b'other'),
                 real_archive('0.43.0', LINUX)['name'] + ' is not its exact bytes'),
                (blob('imbl-sized-chunks-0.2.0.crate', b'other'), 'imbl-sized-chunks-0.2.0.crate is not its exact bytes'),
                (drop('imbl-7.0.2.crate'), 'the public asset set is not the expected set'),
                (extra, 'the public asset set is not the expected set'),
                (body, 'the public notes are not the release notes'),
                (tag, 'the tag is not at the release commit'),
                (verify, 'the public signature is not Good'),
                (un_publish, 'the public release is unavailable'),
                (another, 'the public release is another release')):
            fresh, data = self.published()
            apply(fresh)
            mutations = list(fresh.releases.mutations)
            with self.subTest(text=text):
                self.refused(incident + text, fresh.public_check, data)
                self.assertEqual(fresh.releases.mutations, mutations)
        fresh, data = self.published()
        fresh.install_report = fresh.report(LINUX, skipped=1)
        with self.assertRaises(ControllerError):
            fresh.public_check(data)
        self.assertEqual(stage.releases.mutations, before)

        # A notification failure is outside this path: the workflow never touches it, and nothing here rolls back.
        self.assertNotIn('core-release-notify', WORKFLOW.split('jobs:', 1)[1])
        self.assertNotIn('dispatches', WORKFLOW)

        # The published-exact verifier takes its signature from public bytes, against the reserved signer line.
        stage, _ = self.published()
        reconciliation = ELIGIBILITY.authenticate_published(stage.request(), stage.git)
        calls = []

        class Verify(object):
            def verify(self, signature, manifest, line, principal, namespace):
                calls.append((signature, manifest, line, principal, namespace))
                return True
        proof = PUBLISH.PublishedVerifier(stage.releases, stage.git, Verify(), stage.work()).verify_published(reconciliation)
        self.assertEqual(calls, [(SIGNATURE, manifest_of(stage.w), stage.w.trust['signer_line'], 'rdy-release', 'rdy-release-v1')])
        self.assertIs(proof.signature_verified, True)
        self.assertEqual((proof.principal, proof.namespace, proof.fingerprint), ('rdy-release', 'rdy-release-v1', ctl.FINGERPRINT))
        self.assertEqual(proof.archives, tuple((f['name'], f['sha256'], f['size']) for f in stage.w.archives))
        self.assertIs(proof.assets_complete, True)
        self.assertEqual(proof.notes_sha256, sha(stage.w.notes))
        self.assertEqual(proof.commit, S)
        self.assertIs(proof.draft, False)

    @unittest.skipUnless(shutil.which('ssh-keygen'), 'ssh-keygen is not installed')
    def test_rsig7_verifier_accepts_the_public_v035_vector_only(self):
        line = (VECTOR / 'allowed-signers').read_text().strip()
        manifest = (VECTOR / 'SHA256SUMS').read_bytes()
        signature = (VECTOR / 'SHA256SUMS.sig').read_bytes()
        verifier = SIGN.SshVerifier()
        self.assertIs(verifier.verify(signature, manifest, line, 'rdy-release', 'rdy-release-v1'), True)
        self.assertIs(verifier.verify(signature, manifest + b'x', line, 'rdy-release', 'rdy-release-v1'), False)
        self.assertIs(verifier.verify(signature, manifest, line, 'rdy-release', 'rdy-release-key-v1'), False)
        self.assertIs(verifier.verify(signature, manifest, line, 'someone-else', 'rdy-release-v1'), False)


class Wire(Base):
    def client(self, answers, token='write-value'):
        client = PUBLISH.Releases(PUBLISH.Transport(token), self.stage().work(), 'verifier')
        client.pause = lambda attempt: None
        client.transport._opener = FakeOpener([repository_answer()] + answers)
        return client

    def test_rsig7_draft_requests_are_exact(self):
        created = release_row()
        client = self.client([FakeResponse(201, json.dumps(created).encode())])
        fact = client.create_draft('v0.43.0', S, 'v0.43.0', 'notes')
        request = client.transport._opener.requests[-1]
        self.assertEqual(request.get_method(), 'POST')
        self.assertEqual(request.full_url, 'https://api.github.com/repos/' + REPO + '/releases')
        self.assertEqual(json.loads(request.data), {'tag_name': 'v0.43.0', 'target_commitish': S, 'name': 'v0.43.0',
                                                    'body': 'notes', 'draft': True, 'prerelease': True})
        self.assertEqual((fact.release_id, fact.draft, fact.target_commitish), (4242, True, S))
        # An answer that is not the exact request needs a reconcile.
        client = self.client([FakeResponse(201, json.dumps(release_row(draft=False)).encode())])
        with self.assertRaises(GITHUB.ReconcileRequired):
            client.create_draft('v0.43.0', S, 'v0.43.0', 'notes')
        client = self.client([])
        for bad in (('main', S), ('v0.43.0', 'main')):
            with self.assertRaises(ControllerError):
                client.create_draft(bad[0], bad[1], 'v', 'notes')

        # An upload: octet-stream, the upload host, once. A 422 reads back; it never clobbers.
        folder = self.stage().work()
        path = folder / 'SHA256SUMS'
        path.write_bytes(b'manifest bytes')
        asset = {'id': 11, 'name': 'SHA256SUMS', 'size': 14, 'state': 'uploaded', 'digest': 'sha256:' + digest(1)}
        client = self.client([FakeResponse(201, json.dumps(asset).encode())])
        fact = client.upload_asset(4242, 'SHA256SUMS', path)
        request = client.transport._opener.requests[-1]
        self.assertEqual(request.full_url,
                         'https://uploads.github.com/repos/' + REPO + '/releases/4242/assets?name=SHA256SUMS')
        self.assertEqual(request.get_header('Content-type'), 'application/octet-stream')
        self.assertEqual(request.get_header('Content-length'), '14')
        self.assertEqual(request.get_method(), 'POST')
        self.assertEqual(request.get_header('Authorization'), 'Bearer write-value')
        self.assertEqual((fact.asset_id, fact.name, fact.size, fact.state), (11, 'SHA256SUMS', 14, 'uploaded'))
        for answer in (FakeResponse(422), FakeResponse(500), FakeResponse(201, b'not json')):
            client = self.client([answer])
            with self.assertRaises(GITHUB.ReconcileRequired):
                client.upload_asset(4242, 'SHA256SUMS', path)
        client = self.client([])
        with self.assertRaises(ControllerError):
            client.upload_asset(4242, '../SHA256SUMS', path)

        # A tag: exactly v<V> at a commit.
        client = self.client([FakeResponse(201, json.dumps({'ref': 'refs/tags/v0.43.0', 'object': {'type': 'commit', 'sha': S}}).encode())])
        fact = client.create_final_tag('0.43.0', S)
        request = client.transport._opener.requests[-1]
        self.assertEqual(json.loads(request.data), {'ref': 'refs/tags/v0.43.0', 'sha': S})
        self.assertEqual((fact.object_type, fact.object_sha), ('commit', S))
        client = self.client([])
        for bad in (('0.43.1', S), ('0.43.0', 'main')):
            with self.assertRaises(ControllerError):
                client.create_final_tag(*bad)

        # An authenticated asset download: octet-stream, through the redirect, with no token past it.
        client = self.client([FakeResponse(302, b'', {'location': 'https://objects.example/a'}), FakeResponse(200, b'bytes')])
        out = folder / 'one'
        result = client.download_asset(PUBLISH.AssetFact(11, 'x', 5, 'uploaded', None), out, 1024)
        self.assertEqual(result, (sha(b'bytes'), 5))
        first, second = client.transport._opener.requests[-2:]
        self.assertEqual(first.full_url, 'https://api.github.com/repos/' + REPO + '/releases/assets/11')
        self.assertEqual(first.get_header('Accept'), 'application/octet-stream')
        self.assertEqual(first.get_header('Authorization'), 'Bearer write-value')
        self.assertIsNone(second.get_header('Authorization'))
        # A body over the limit leaves no file.
        client = self.client([FakeResponse(200, b'x' * 2000)])
        out = folder / 'two'
        with self.assertRaises(ControllerError):
            client.download_asset(PUBLISH.AssetFact(11, 'x', 5, 'uploaded', None), out, 1024)
        self.assertFalse(out.exists())

        # A release read: closed fields of the exact types; duplicate asset names stay visible to the check.
        rows = [release_row(assets=[dict(asset), dict(asset, id=12)])]
        client = self.client([FakeResponse(200, json.dumps(rows).encode())])
        fact = client.full_releases()[0]
        self.assertEqual([a.asset_id for a in fact.assets], [11, 12])
        self.assertEqual(fact.body, 'notes')
        for bad in (dict(rows[0], assets='x'), dict(rows[0], draft='yes'), dict(rows[0], assets=[dict(asset, size='1')]),
                    dict(rows[0], assets=[dict(asset, name=5)])):
            client = self.client([FakeResponse(200, json.dumps([bad]).encode())])
            with self.assertRaises(ControllerError):
                client.full_releases()
        # A null body is an empty body.
        client = self.client([FakeResponse(200, json.dumps([dict(rows[0], body=None)]).encode())])
        self.assertEqual(client.full_releases()[0].body, '')


class Cli(Base):
    def run_cli(self, *args, env=None):
        environ = {'PATH': '/usr/bin:/bin'}
        environ.update(env or {})
        return subprocess.run([sys.executable, '-I', '-B', str(ROOT / 'scripts/release/publish.py')] + list(args),
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environ,
                              check=False)

    def test_rsig7_cli_failures_use_the_controller_prefix(self):
        done = self.run_cli('nonsense')
        self.assertEqual(done.returncode, 2)
        done = self.run_cli('eligibility')
        self.assertEqual(done.returncode, 2)
        with tempfile.TemporaryDirectory() as folder:
            work = str(Path(folder) / 'work')
            done = self.run_cli('check-published', '--target', LINUX, '--checker', '/bin/true', '--work', work,
                                '--out', str(Path(folder) / 'out'), env={'GITHUB_TOKEN': 'secret-value',
                                                                         'RDY_PUBLISH_RECEIPT': '{}'})
        self.assertEqual(done.returncode, 1)
        self.assertEqual(done.stderr, b'release controller: the public checks must run without credentials\n')
        self.assertNotIn(b'secret-value', done.stdout + done.stderr)
        # Every command the workflow names exists, and the workflow's arguments parse.
        for command in re.findall(r'trusted/scripts/release/publish\.py (\S+)', WORKFLOW):
            done = self.run_cli(command, '--help')
            self.assertEqual(done.returncode, 0, command)


class SignerRepairs(unittest.TestCase):
    """The signer repairs, at the reserve and eligibility jobs."""

    def stage(self):
        stage = Stage()
        self.addCleanup(stage.close)
        return stage

    def test_rsig12_c_reserve_keeps_an_inherited_observation(self):
        # The eligibility job's success is an earlier observation of this exact run. It is no alert list.
        stage = self.stage()
        stage.w.find_job(ctl.RESERVE_JOB_ID).update(status='in_progress', conclusion=None)
        stage.git.open_code_scanning_alerts = mock.Mock(side_effect=AssertionError('the reserve job has no alert read'))
        inherited = PUBLISH.inherited_alerts(stage.request(), stage.git, DISPATCH_RUN, 1)
        self.assertIsInstance(inherited, ELIGIBILITY.InheritedAlerts)
        self.assertEqual((inherited.request, inherited.commit, inherited.workflow_path, inherited.run_id,
                          inherited.run_attempt, inherited.job_id),
                         (stage.request(), S, '.github/workflows/release.yml', DISPATCH_RUN, 1, ELIGIBILITY_JOB_ID))
        self.assertFalse(hasattr(inherited, 'alerts'), 'it claims no alert list')
        self.assertFalse(hasattr(PUBLISH, '_AlertsRead'), 'no adapter answers an empty list from job metadata')
        preliminary = ELIGIBILITY.qualify_new(stage.request(), stage.git, inherited)
        self.assertIs(preliminary.alert_observation, inherited)
        stage.git.open_code_scanning_alerts.assert_not_called()
        self.assertIs(json.loads(PUBLISH.reserve_tuple(stage.request(), stage.git, DISPATCH_RUN, 1, H))['created'], True)
        stage.git.open_code_scanning_alerts.assert_not_called()
        # The eligibility job itself made a real read, and says so. Recovery holds no observation.
        stage = self.stage()
        self.assertEqual(ELIGIBILITY.qualify_new(stage.request(), stage.git).alert_observation,
                         ELIGIBILITY.FreshAlertRead('refs/heads/main'))
        self.assertIn(('open_code_scanning_alerts', 'refs/heads/main'), stage.w.calls)
        stage.rig.reserve_version()
        self.assertIsNone(ELIGIBILITY.preliminary_recovery(stage.request(), stage.git).alert_observation)

    def test_rsig12_c_inherited_observation_is_bound_to_this_run(self):
        stage = self.stage()
        good = PUBLISH.inherited_alerts(stage.request(), stage.git, DISPATCH_RUN, 1)
        other_candidate = stage.w.request(candidate_run_attempt=2)
        other_evidence = stage.w.request(evidence_sha256=digest(99))
        message = 'eligibility incomplete: the inherited alert observation is not this exact release run'
        for label, changes in (('commit', {'commit': base.OTHER}), ('workflow', {'workflow_path': '.github/workflows/x.yml'}),
                               ('run', {'run_id': 1}), ('attempt', {'run_attempt': 2}),
                               ('candidate', {'request': other_candidate}), ('evidence', {'request': other_evidence})):
            with self.subTest(label), self.assertRaises(ELIGIBILITY.Refused) as caught:
                ELIGIBILITY.qualify_new(stage.request(), stage.git, dataclasses.replace(good, **changes))
            self.assertEqual(str(caught.exception), message)
        with self.assertRaises(ELIGIBILITY.Refused) as caught:
            ELIGIBILITY.qualify_new(stage.request(), stage.git, [])
        self.assertEqual(str(caught.exception), message)
        self.assertEqual(stage.git.created, [])

    def test_rsig12_c_a_job_name_alone_is_not_the_observation(self):
        did_not_pass = 'eligibility incomplete: the Release eligibility job did not pass'
        cases = (('failed', lambda s: s.w.find_job(ELIGIBILITY_JOB_ID).update(conclusion='failure'), did_not_pass),
                 ('running', lambda s: s.w.find_job(ELIGIBILITY_JOB_ID).update(status='in_progress', conclusion=None),
                  did_not_pass),
                 ('wrong head', lambda s: s.w.find_job(ELIGIBILITY_JOB_ID).update(head_sha=base.OTHER), did_not_pass),
                 ('missing', lambda s: s.w.find_job(ELIGIBILITY_JOB_ID).update(name='Another job'), did_not_pass),
                 ('duplicate', lambda s: s.w.job(DISPATCH_RUN, 1, 9499, PUBLISH.ELIGIBILITY_JOB), did_not_pass),
                 ('unreadable', lambda s: s.w.errors.update(attempt_jobs=GITHUB.Uncertain('jobs', 'outcome unknown')),
                  'eligibility incomplete: the eligibility job is unavailable'))
        for label, change, message in cases:
            stage = self.stage()
            change(stage)
            with self.subTest(label), self.assertRaises(Refused) as caught:
                PUBLISH.inherited_alerts(stage.request(), stage.git, DISPATCH_RUN, 1)
            self.assertEqual(str(caught.exception), message)
        # Another attempt's success is not this attempt's, and a request for another attempt names another run.
        stage = self.stage()
        with self.assertRaises(Refused) as caught:
            PUBLISH.inherited_alerts(stage.request(2), stage.git, DISPATCH_RUN, 1)
        self.assertEqual(str(caught.exception), 'eligibility incomplete: the request is not this run attempt')
        stage.rig.add_job(DISPATCH_RUN, 2, 9498)
        with self.assertRaises(Refused) as caught:
            PUBLISH.inherited_alerts(stage.request(2), stage.git, DISPATCH_RUN, 2)
        self.assertEqual(str(caught.exception), did_not_pass)

    def test_rsig12_b_unknown_status_is_shown_as_unknown(self):
        for revoked, shown in ((None, 'unknown'), (True, 'revoked'), (False, 'not revoked')):
            terminal = CONTROLLER.AlreadyPublishedMatching(
                version='0.43.0', commit=S, tuple_sha256=digest(1), release_id=1, manifest_sha256=digest(2),
                signature_sha256=digest(3), channel_enabled=True, revoked_receipts=(), signer_revoked=revoked)
            result = PUBLISH.Reconciled('already_published_matching', None, terminal)
            out = io.StringIO()
            with mock.patch.object(PUBLISH, '_start', return_value=(None, None, None)), \
                    mock.patch.object(PUBLISH, 'request_from_inputs'), mock.patch.object(PUBLISH, '_anonymous'), \
                    mock.patch.object(PUBLISH, '_request_value', return_value={}), \
                    mock.patch.object(PUBLISH, '_output'), mock.patch.object(PUBLISH, 'reconcile', return_value=result), \
                    mock.patch.object(PUBLISH.sys, 'stdout', out):
                PUBLISH.cmd_eligibility(mock.Mock(), {})
            with self.subTest(shown):
                value = json.loads(out.getvalue())
                self.assertEqual(value['signer_status'], shown)
                self.assertNotIn('signer_revoked', value)


class CurrentAuthority(Base):
    """A remaining act of an unfinished release needs current authority. Historical policy proves the original
    signature and bytes; a protected-main snapshot, read again now, authorizes the act. Neither stands in for the other."""

    GRANT = '00000000-0000-4000-8000-000000000009'

    @staticmethod
    def closed(s):
        s.rig.install(state='CLOSED')

    @classmethod
    def draining(cls, run_id=DISPATCH_RUN, run_attempt=1):
        def apply(s):
            s.rig.install(state='DRAINING', holder={'run_id': str(run_id), 'run_attempt': str(run_attempt), 'job_id': '1',
                                                    'grant_id': cls.GRANT})
        return apply

    @staticmethod
    def other_h(s):
        s.rig.install(signer_workflow_sha='4' * 40)

    @staticmethod
    def other_running_h(s):
        s.rig.enrollment = dataclasses.replace(s.rig.enrollment, signer_workflow_sha='4' * 40)

    @staticmethod
    def other_key(s):
        arn = s.w.policy['key_arn']
        s.rig.install(key_arn=arn[:-1] + ('0' if arn[-1] != '0' else '1'))

    @staticmethod
    def other_fingerprint(s):
        s.rig.install(fingerprint='SHA256:' + 'B' * 43)

    @staticmethod
    def revoked_signer(s):
        good = s.git.release_trust

        def trust(version, policy_commit, signing):
            if signing:
                raise ControllerError('the key for v0.43.0 is revoked for new signing')
            return good(version, policy_commit, signing)
        s.git.release_trust = trust

    @staticmethod
    def admission_gone(s):
        s.w.files.pop((ctl.ADMISSION, s.rig.head()), None)

    @staticmethod
    def admission_malformed(s):
        s.w.put(s.rig.head(), ctl.ADMISSION, b'{}\n')

    @staticmethod
    def trust_unavailable(s):
        s.git.release_trust = mock.Mock(side_effect=ControllerError('offline'))

    def refusals(self):
        return (('closed', self.closed, 'authority unavailable: admission is CLOSED'),
                ('another holder run', self.draining(run_id=DISPATCH_RUN + 1),
                 'authority unavailable: admission is DRAINING for another holder'),
                ('another holder attempt', self.draining(run_attempt=2),
                 'authority unavailable: admission is DRAINING for another holder'),
                ('changed H', self.other_h, 'authority unavailable: the admission record names another signer'),
                ('changed running H', self.other_running_h, 'authority unavailable: the admission record names another signer'),
                ('changed key', self.other_key, 'authority unavailable: the admission record names another key'),
                ('changed fingerprint', self.other_fingerprint, 'authority unavailable: the admission record names another key'),
                ('revoked signer', self.revoked_signer, 'signer revoked: the key for v0.43.0 is revoked for new signing'),
                ('admission unavailable', self.admission_gone, 'authority unavailable: the admission record is unavailable'),
                ('admission malformed', self.admission_malformed, 'authority unavailable: the admission record is malformed'),
                ('trust unavailable', self.trust_unavailable, 'eligibility incomplete: offline'))

    def later_changes(self):
        """What can change while a job runs. The running H is fixed for the life of the process."""
        return [case for case in self.refusals() if case[1] is not self.other_running_h]

    def signed(self):
        stage = self.stage()
        stage.sign()
        return stage

    def drafted(self):
        stage = self.signed()
        stage.draft()
        return stage

    @staticmethod
    def after_gather_read(stage, change):
        """`change` lands after the job's entry reads and before its first write."""
        real = stage.fetch_crate
        done = []

        def fetch(name):
            if not done:
                done.append(change(stage))
            return real(name)
        stage.fetch_crate = fetch

    @staticmethod
    def after_transfer_read(stage, change):
        """`change` lands after the genuine job's entry reads and before its checker."""
        real = stage.git.artifact_path

        def path(fact):
            change(stage)
            return real(fact)
        stage.git.artifact_path = path

    def no_draft_effect(self, stage):
        self.assertEqual(stage.releases.mutations, [])
        self.assertEqual(stage.releases.rows, [])
        self.assertEqual(stage.rig.kms.signs, 1, 'no second Sign')

    def no_genuine_effect(self, stage, target=MAC):
        self.assertEqual(stage.checker_calls, [])
        self.assertNotIn(GENUINE_ART[target], stage.w.artifacts)

    def card_stage(self):
        """The first machine-signed preview under an archived key card, signed, with the card in its authorization."""
        world = release_world(base.first_machine_world)
        transition = b'{"scheme":"filler"}\n'
        world.put(P, 'release-keys/transition-v0.42.0.json', transition)
        world.put(P, 'release-keys/transition-v0.42.0.json.sig', b'filler endorsement\n')
        world.trust['transition_sha256'] = sha(transition)
        world.e['approval_basis']['scope'] = world.full_scope()
        world.seal()
        stage = self.stage(world)
        receipt = stage.w.add_card('key_change')
        stage.rig.seed_archive(receipt)
        stage.rig.install()
        stage.sign()
        return stage, receipt

    @staticmethod
    def revoke_card(receipt):
        def apply(s):
            ctl.Repairs().land(s.rig, ctl.Repairs.channel(revoked_receipts=[receipt]))
        return apply

    # -- the draft ------------------------------------------------------------------------------

    def test_rsig13_closed_admission_refuses_before_any_draft_write(self):
        stage = self.signed()
        self.closed(stage)
        self.refused('authority unavailable: admission is CLOSED', stage.draft)
        self.no_draft_effect(stage)

    def test_rsig13_each_refusal_stops_the_draft_at_entry_and_before_its_first_write(self):
        for label, change, text in self.refusals():
            stage = self.signed()
            change(stage)
            with self.subTest(label, when='entry'):
                self.refused(text, stage.draft)
                self.no_draft_effect(stage)
        for label, change, text in self.later_changes():
            stage = self.signed()
            self.after_gather_read(stage, change)
            with self.subTest(label, when='before the first write'):
                self.refused(text, stage.draft)
                self.no_draft_effect(stage)

    def test_rsig13_a_change_between_uploads_refuses_the_next_upload(self):
        for label, change, text in self.later_changes():
            stage = self.signed()
            real = stage.releases.upload_asset

            def upload(*args, stage=stage, change=change, real=real):
                answer = real(*args)
                change(stage)
                return answer
            stage.releases.upload_asset = upload
            with self.subTest(label):
                self.refused(text, stage.draft)
                self.assertEqual(stage.releases.mutations.count('create_draft'), 1)
                self.assertEqual(len([m for m in stage.releases.mutations if m.startswith('upload ')]), 1)

    def test_rsig13_a_change_after_draft_creation_refuses_the_first_upload(self):
        stage = self.signed()
        real = stage.releases.create_draft

        def create(*args):
            answer = real(*args)
            self.closed(stage)
            return answer
        stage.releases.create_draft = create
        self.refused('authority unavailable: admission is CLOSED', stage.draft)
        self.assertEqual(stage.releases.mutations, ['create_draft'])

    def test_rsig13_a_closure_after_a_reconciliation_read_refuses_the_next_write(self):
        # The upload's answer is lost. The read that reconciles it finds the asset; admission closes after that read.
        stage = self.signed()
        stage.releases.faults['upload SHA256SUMS'] = (True, 1)
        real = stage.releases.release_full

        def read(release_id):
            answer = real(release_id)
            self.closed(stage)
            return answer
        stage.releases.release_full = read
        self.refused('authority unavailable: admission is CLOSED', stage.draft)
        uploads = [m for m in stage.releases.mutations if m.startswith('upload ')]
        self.assertEqual(uploads, ['upload SHA256SUMS'])
        # The draft's creation: its answer is lost, its read finds it, and admission closes after that read.
        stage = self.signed()
        stage.releases.faults['create_draft'] = (True, 1)
        real_rows = stage.releases.full_releases
        calls = []

        def rows():
            answer = real_rows()
            calls.append(1)
            if len(calls) == 2:
                self.closed(stage)
            return answer
        stage.releases.full_releases = rows
        self.refused('authority unavailable: admission is CLOSED', stage.draft)
        self.assertEqual(stage.releases.mutations, ['create_draft'])

    def test_rsig13_a_previous_job_success_authorizes_no_later_write(self):
        # An unfinished draft from an earlier attempt, then a closure: the retry writes nothing.
        stage = self.signed()
        stage.releases.faults['upload SHA256SUMS'] = (False, 1)
        with self.assertRaises(ControllerError):
            stage.draft()
        before = list(stage.releases.mutations)
        self.closed(stage)
        self.refused('authority unavailable: admission is CLOSED', stage.draft)
        self.assertEqual(stage.releases.mutations, before)

    def test_rsig13_a_complete_draft_under_a_refusal_gets_no_receipt(self):
        # Nothing is left to write, and the reads may run. Their output is not a usable draft receipt.
        for label, change, text in self.refusals():
            stage = self.drafted()
            kept = stage.draft_result
            before = list(stage.releases.mutations)
            change(stage)
            with self.subTest(label):
                self.refused(text, stage.draft)
                self.assertEqual(stage.releases.mutations, before)
                self.assertIs(stage.draft_result, kept)
        # A closure during the last read-back also stops the receipt.
        stage = self.signed()
        real = stage.releases.download_asset

        def download(asset, out, limit, stage=stage, real=real):
            answer = real(asset, out, limit)
            self.closed(stage)
            return answer
        stage.releases.download_asset = download
        self.refused('authority unavailable: admission is CLOSED', stage.draft)

    def test_rsig13_active_and_the_named_holder_proceed_with_the_original_bytes(self):
        outcomes = []
        for label, change in (('active', lambda s: None), ('named holder', self.draining())):
            stage = self.signed()
            reservation = stage.w.refs['tags/reserve/v0.43.0']
            tuple_bytes = stage.w.tags[reservation[1]]['message_bytes']
            record = stage.w.members[(base.EVID_ART, 'release-record.json')]
            authorization = stage.w.members[(SIG_ART, 'authorization.json')]
            change(stage)
            with self.subTest(label):
                result = stage.draft()
                stage.both_genuine()
                self.assertEqual(len(stage.checker_calls), 2)
                self.assertEqual((result.folder / 'SHA256SUMS.sig').read_bytes(), SIGNATURE)
                self.assertEqual((result.folder / 'SHA256SUMS').read_bytes(), manifest_of(stage.w))
                self.assertEqual(stage.w.refs['tags/reserve/v0.43.0'], reservation)
                self.assertEqual(stage.w.tags[reservation[1]]['message_bytes'], tuple_bytes)
                self.assertEqual(stage.w.members[(base.EVID_ART, 'release-record.json')], record)
                self.assertEqual(stage.w.members[(SIG_ART, 'authorization.json')], authorization)
                self.assertEqual(stage.rig.kms.signs, 1)
                stage.final_tag()
                stage.promote()
                self.assertEqual(stage.releases.mutations[-2:], ['create_final_tag', 'publish_draft'])
            outcomes.append((result.receipt, stage.w.members[(GENUINE_ART[MAC], PUBLISH.GENUINE_MEMBER)]))
        self.assertEqual(outcomes[0], outcomes[1], 'the same bytes and the same evidence under both states')

    def test_rsig13_a_revoked_approval_refuses_the_draft_and_the_genuine_check(self):
        text = 'not authorized: the key_change card receipt is revoked'
        stage, receipt = self.card_stage()
        self.after_gather_read(stage, self.revoke_card(receipt))
        self.refused(text, stage.draft)
        self.no_draft_effect(stage)
        stage, receipt = self.card_stage()
        stage.draft()
        done = []

        def once(s):
            if not done:
                done.append(self.revoke_card(receipt)(s))
        self.after_transfer_read(stage, once)
        self.refused(text, stage.genuine, MAC)
        self.no_genuine_effect(stage)

    def test_rsig13_the_accepted_approval_bytes_survive_the_checks(self):
        for label, change in (('active', lambda s: None), ('named holder', self.draining())):
            stage, receipt = self.card_stage()
            key = ('APPROVAL#' + receipt, 'RECORD')
            accepted = stage.rig.table.items[key]
            authorization = stage.w.members[(SIG_ART, 'authorization.json')]
            change(stage)
            with self.subTest(label):
                stage.draft()
                stage.both_genuine()
                self.assertEqual(len(stage.checker_calls), 2)
                self.assertEqual(stage.rig.table.items[key], accepted)
                self.assertEqual(stage.w.members[(SIG_ART, 'authorization.json')], authorization)
                self.assertEqual(stage.rig.kms.signs, 1)

    def test_rsig13_the_authorization_must_be_this_tuple_and_its_cards(self):
        def tampered(**changes):
            def apply(s):
                key = (SIG_ART, 'authorization.json')
                s.w.members[key] = canon(dict(json.loads(s.w.members[key]), **changes))
            return apply
        stage, _ = self.card_stage()
        tampered(cards=[])(stage)
        self.refused('evidence mismatch: the signer authorization does not cover these cards', stage.draft)
        self.no_draft_effect(stage)
        stage, _ = self.card_stage()
        stage.draft()
        tampered(cards=[])(stage)
        self.refused('evidence mismatch: the signer authorization does not cover these cards', stage.genuine, MAC)
        self.no_genuine_effect(stage)

    def test_rsig13_historical_trust_is_no_fallback(self):
        # The historical signature check passes on every read here. Only current evidence is missing.
        for label, change in (('admission', self.admission_gone), ('malformed', self.admission_malformed)):
            stage = self.signed()
            change(stage)
            trust = stage.git.release_trust(stage.w.v, P, True)
            self.assertEqual(trust['version'], stage.w.v)
            with self.subTest(label, job='draft'):
                with self.assertRaises(ControllerError) as caught:
                    stage.draft()
                self.assertTrue(str(caught.exception).startswith('authority unavailable: '), str(caught.exception))
                self.no_draft_effect(stage)
            stage = self.drafted()
            change(stage)
            with self.subTest(label, job='genuine'):
                with self.assertRaises(ControllerError) as caught:
                    stage.genuine(MAC)
                self.assertTrue(str(caught.exception).startswith('authority unavailable: '), str(caught.exception))
                self.no_genuine_effect(stage)
        # A main branch that moves during the read is no snapshot.
        stage = self.signed()
        real = stage.git.contents

        def contents(path, ref_sha, limit=GITHUB.CONTENTS_MAX):
            answer = real(path, ref_sha, limit)
            if path == CONTROLLER.ADMISSION_PATH:
                ctl.Repairs.advance(stage.rig)
            return answer
        stage.git.contents = contents
        self.refused('authority unavailable: main moved during the admission read', stage.draft)
        self.no_draft_effect(stage)

    # -- the genuine check ----------------------------------------------------------------------

    def test_rsig13_genuine_check_under_each_refusal_runs_no_checker_and_emits_no_receipt(self):
        for label, change, text in self.refusals():
            stage = self.drafted()
            change(stage)
            with self.subTest(label, when='entry'):
                self.refused(text, stage.genuine, MAC)
                self.no_genuine_effect(stage)
        for label, change, text in self.later_changes():
            stage = self.drafted()
            self.after_transfer_read(stage, change)
            with self.subTest(label, when='before the checker'):
                self.refused(text, stage.genuine, MAC)
                self.no_genuine_effect(stage)

    def test_rsig13_a_change_during_the_checker_emits_no_receipt(self):
        for label, change, text in self.later_changes():
            stage = self.drafted()
            real = stage.checker

            def checker(target, stage=stage, change=change, real=real):
                run = real(target)

                def closing(argv, boundary):
                    answer = run(argv, boundary)
                    change(stage)
                    return answer
                return closing
            stage.checker = checker
            with self.subTest(label):
                self.refused(text, stage.genuine, MAC)
                self.assertEqual(len(stage.checker_calls), 1)
                self.assertNotIn(GENUINE_ART[MAC], stage.w.artifacts)

    def test_rsig13_the_genuine_check_keeps_its_historical_signature_proof(self):
        # Current authority does not replace the proof: a signature that does not verify stops the check.
        stage = self.drafted()
        stage.rig.verifier.verify = lambda *_: False
        self.refused('evidence mismatch: the transferred signature does not verify', stage.genuine, MAC)
        self.no_genuine_effect(stage)
        # And a genuine receipt from before the closure is not accepted after it.
        stage = self.drafted()
        stage.both_genuine()
        self.closed(stage)
        self.refused('authority unavailable: admission is CLOSED', stage.final_tag)
        self.assertNotIn('create_final_tag', stage.releases.mutations)

    # -- the terminal observation ---------------------------------------------------------------

    def test_rsig13_a_published_observation_still_works_and_gives_no_authority(self):
        stage = self.stage()
        stage.ready()
        stage.final_tag()
        stage.git.runs_of_release = [(DISPATCH_RUN, 1)]
        stage.promote()
        self.closed(stage)
        self.revoked_signer(stage)
        result = PUBLISH.reconcile(stage.request(), stage.git, stage.releases, stage.rig.verifier, H, stage.work())
        self.assertEqual((result.state, result.mode), ('already_published_matching', None))
        self.assertEqual((result.terminal.version, result.terminal.commit), ('0.43.0', S))
        self.assertIs(result.terminal.signer_revoked, True, 'the current status is reported, never acted on')
        calls = len(stage.checker_calls)
        before = (list(stage.releases.mutations), copy.deepcopy(stage.releases.rows), copy.deepcopy(stage.w.refs))
        for job, args in ((stage.draft, ()), (stage.genuine, (MAC,)), (stage.final_tag, ()), (stage.promote, ())):
            with self.subTest(job=job.__name__):
                with self.assertRaises(ControllerError):
                    job(*args)
                self.assertEqual(len(stage.checker_calls), calls)
                self.assertEqual((list(stage.releases.mutations), stage.releases.rows, stage.w.refs), before)
        # Nothing repairs a release that has gone, even with the observation in hand.
        stage.releases.rows.clear()
        stage.w.releases[:] = [r for r in stage.w.releases if r['tag_name'] != 'v0.43.0']
        with self.assertRaises(ControllerError):
            stage.draft()
        self.assertEqual(stage.releases.mutations, before[0])
        # Each later job, and the published checks behind them, runs only for an unfinished release.
        for name in ('enrollment', 'reserve', 'draft', 'genuine-mac', 'genuine-linux', 'final-tag', 'promote'):
            self.assertIn("    if: needs.eligibility.outputs.state == 'ready'\n", job_text(name), name)
        self.assertEqual(needs_of('published-mac'), ['promote'])
        self.assertEqual(needs_of('published-linux'), ['promote'])

    # -- the wiring -----------------------------------------------------------------------------

    def test_rsig13_the_jobs_take_only_existing_authenticated_data(self):
        # The enrollment stays in environment release. One job with no token permission and no code passes it on.
        job = job_text('enrollment')
        self.assertIn('    environment: release\n', job)
        self.assertIn('    permissions: {}\n', job)
        self.assertIn('      value: ${{ vars.RDY_RELEASE_ENROLLMENT }}\n', job)
        for word in ('secrets.', 'github.token', 'GITHUB_TOKEN', 'id-token', 'uses:', 'python', 'trusted'):
            self.assertNotIn(word, job)
        for name in ('draft', 'genuine-mac', 'genuine-linux'):
            job = job_text(name)
            with self.subTest(name):
                self.assertIn('enrollment', needs_of(name))
                self.assertIn('sign', needs_of(name))
                self.assertIn('RDY_RELEASE_ENROLLMENT: ${{ needs.enrollment.outputs.value }}\n', job)
                self.assertIn('RDY_SIGNATURE_ARTIFACT_ID: ${{ needs.sign.outputs.signature_artifact_id }}\n', job)
                self.assertIn('RDY_SIGNATURE_ARTIFACT_DIGEST: ${{ needs.sign.outputs.signature_artifact_digest }}\n', job)
                for word in ('environment:', 'id-token', 'secrets.', 'vars.', 'aws', 'AWS', 'create-github-app-token', 'sts'):
                    self.assertNotIn(word, job)
        # The genuine jobs keep exactly their read permissions, and the candidate's process never sees the enrollment.
        for name in ('genuine-mac', 'genuine-linux'):
            self.assertEqual(permissions_of(name), {'actions': 'read', 'contents': 'read'})
        self.assertEqual(permissions_of('draft'), {'actions': 'read', 'contents': 'write'})
        self.assertNotIn('RDY_RELEASE_ENROLLMENT', PUBLISH.rehearse.PASSED_ENV)
        self.assertEqual(WORKFLOW.count('id-token: write'), 1)
        self.assertEqual(sorted(n for n in job_ids() if 'vars.RDY_RELEASE_ENROLLMENT' in job_text(n)),
                         ['enrollment', 'final-tag', 'promote'])

    def test_rsig13_no_enrollment_stops_the_jobs_before_any_effect(self):
        stage = self.signed()
        with mock.patch.object(PUBLISH, '_start', lambda *_: (stage.releases, stage.work(), stage.rig.verifier)), \
                mock.patch.object(PUBLISH, '_parsed_request', lambda _: stage.request()), \
                mock.patch.object(PUBLISH, '_transfers', lambda _: stage.transfers()), \
                mock.patch.object(PUBLISH.sign, 'SshVerifier', lambda: stage.rig.verifier):
            args = mock.Mock(signer_sha=H, out=stage.root / 'out', assets=stage.root / 'assets')
            with self.assertRaises(ControllerError):
                PUBLISH.cmd_draft(args, {'GITHUB_RUN_ID': str(DISPATCH_RUN), 'GITHUB_RUN_ATTEMPT': '1'})
        self.no_draft_effect(stage)
        self.assertFalse((stage.root / 'out').exists())
        stage = self.drafted()
        out = stage.root / 'genuine'
        with mock.patch.object(PUBLISH, 'Releases', lambda *_: stage.git), \
                mock.patch.object(PUBLISH, 'Transport', lambda *_: None), \
                mock.patch.object(PUBLISH.sign, '_check_trusted', lambda *_: None), \
                mock.patch.object(PUBLISH, '_parsed_request', lambda _: stage.request()), \
                mock.patch.object(PUBLISH, '_transfers', lambda _: stage.transfers()), \
                mock.patch.object(PUBLISH.sign, 'SshVerifier', lambda: stage.rig.verifier):
            args = mock.Mock(signer_sha=H, out=out, work=stage.work() / 'genuine', target=MAC, checker=stage.checker(MAC))
            environ = {'GITHUB_RUN_ID': str(DISPATCH_RUN), 'GITHUB_RUN_ATTEMPT': '1',
                       'RDY_DRAFT_ASSETS_ARTIFACT_ID': str(ASSETS_ART), 'RDY_DRAFT_RECEIPT_ARTIFACT_ID': str(RECEIPT_ART)}
            with self.assertRaises(ControllerError):
                PUBLISH.cmd_genuine(args, environ)
            self.no_genuine_effect(stage)
            self.assertFalse(out.exists())
            # With the enrollment, the job's command runs and writes its receipt.
            with mock.patch.object(PUBLISH, '_enrollment', lambda *_: stage.rig.enrollment):
                args.work = stage.work() / 'genuine'
                PUBLISH.cmd_genuine(args, environ)
        self.assertEqual(json.loads((out / PUBLISH.GENUINE_MEMBER).read_bytes())['target'], MAC)


if __name__ == '__main__':
    unittest.main()
