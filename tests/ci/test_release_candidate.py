"""Candidate build and isolated rehearsal contracts, with no network, GitHub write, key or signature."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/release' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # rehearse.py imports candidate.py: one module, one error class
    spec.loader.exec_module(module)
    return module


CANDIDATE = load('candidate')
REHEARSE = load('rehearse')
ControllerError = CANDIDATE.ControllerError
WORKFLOW = (ROOT / '.github/workflows/release-candidate.yml').read_text()

MAC = 'aarch64-apple-darwin'
LINUX = 'x86_64-unknown-linux-gnu'
V = '0.42.0'
S = 'a' * 40
RUN_ID = 9001
ATTEMPT = 2


def sha(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1',
               GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.invalid',
               GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.invalid')
    out = subprocess.run(['git', '-C', str(repo)] + list(args), check=True, env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return out.stdout.decode().strip()


def commit_version(repo, version, message):
    (repo / 'Cargo.toml').write_text(
        '[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nedition = "2024"\n'
        'version = "' + version + '"\n')
    git(repo, 'add', 'Cargo.toml')
    git(repo, 'commit', '-q', '-m', message)
    return git(repo, 'rev-parse', 'HEAD')


def tar_gz(members):
    """A .tar.gz with (name, bytes or None for a directory, kind) members."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w:gz') as archive:
        for name, data, kind in members:
            info = tarfile.TarInfo(name)
            if kind == 'dir':
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                archive.addfile(info)
            elif kind == 'link':
                info.type = tarfile.SYMTYPE
                info.linkname = data
                archive.addfile(info)
            else:
                info.size = len(data)
                info.mode = 0o755
                archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def release_archive(target, binary=b'\x7fELF rdy release binary /rdy/crates/rdy-cli/src/main.rs\n'):
    stem = 'rdy-' + V + '-preview-' + target
    return tar_gz([(stem, None, 'dir'), (stem + '/rdy', binary, 'file'),
                   (stem + '/LICENSE-MIT', b'MIT\n', 'file'),
                   (stem + '/LICENSE-APACHE', b'Apache\n', 'file'),
                   (stem + '/THIRD-PARTY-NOTICES.txt', b'notices\n', 'file'),
                   (stem + '/PREVIEW.txt', b'preview\n', 'file')])


def gate_report(**change):
    report = {'revision': S[:9], 'dirty': False, 'pass': True, 'mode': 'skip-perf',
              'gates': [{'name': 'fmt', 'pass': True, 'detail': ''},
                        {'name': 'workspace tests', 'pass': True,
                         'detail': '120 passed, 0 failed, 3 ignored, 0 skipped'},
                        {'name': 'lib', 'pass': True, 'detail': '40 passed, 0 failed, 0 ignored, 0 skipped'}],
              'areas': [{'name': 'cli', 'passed': 30, 'failed': 0, 'ignored': 1, 'skipped': 0},
                        {'name': 'release', 'passed': 10, 'failed': 0, 'ignored': 0, 'skipped': 0}]}
    report.update(change)
    return json.dumps(report).encode()


FAST_GATE = {'passed': 200, 'failed': 0, 'ignored': 4, 'prerequisite_skipped': 0}


def build_receipt(for_target, archive_bytes, **change):
    receipt = {'schema': 1, 'repository': 'adrenickchrome/rdy-core', 'version': V, 'commit': S,
               'target': for_target, 'run_id': RUN_ID, 'run_attempt': ATTEMPT,
               'archive': {'name': CANDIDATE.archive_name(V, for_target), 'sha256': sha(archive_bytes),
                           'size': len(archive_bytes)},
               'fast_gate': dict(FAST_GATE), 'host_paths': 0,
               'sources': [{'name': 'imbl-7.0.2.crate', 'sha256': 'b' * 64, 'size': 155700}]}
    receipt.update(change)
    return receipt


def plain_json(value):
    """JSON bytes a hostile or careless writer might send: sorted, but floats allowed."""
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def workflow_jobs(text):
    """The text of each job under the top-level `jobs:` key, by job id."""
    lines = text.splitlines(True)
    start = lines.index('jobs:\n') + 1
    jobs, name = {}, None
    for line in lines[start:]:
        if line[:1] not in (' ', '\n', '#'):
            break
        match = re.match(r'  ([A-Za-z0-9_-]+):\n$', line)
        if match:
            name = match.group(1)
            jobs[name] = ''
        elif name:
            jobs[name] += line
    return jobs


def artifact_zip(path, members):
    with zipfile.ZipFile(path, 'w') as z:
        for name, data in members:
            z.writestr(name, data)
    data = path.read_bytes()
    return 'sha256:' + sha(data)


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.archives = {MAC: release_archive(MAC), LINUX: release_archive(LINUX)}

    def build_input(self, target, receipt=None, extra=(), name=None, artifact_id=None):
        folder = self.tmp / ('in-' + target + '-' + str(len(list(self.tmp.iterdir()))))
        folder.mkdir()
        archive = self.archives[target]
        receipt = build_receipt(target, archive) if receipt is None else receipt
        members = [(name or CANDIDATE.archive_name(V, target), archive),
                   ('build-' + target + '.json', plain_json(receipt))] + list(extra)
        digest = artifact_zip(folder / 'artifact.zip', members)
        return (folder, artifact_id or (100 if target == MAC else 200), digest)

    def assemble(self, inputs, out=None):
        out = out or self.tmp / 'candidate'
        return CANDIDATE.assemble(inputs, version=V, commit=S, run_id=RUN_ID, run_attempt=ATTEMPT,
                                  out=out), out


class rsig4_Candidate(Fixture):
    def test_rsig4_candidate_ref_binds_source_and_version(self):
        repo = self.tmp / 'repo'
        repo.mkdir()
        git(repo, 'init', '-q', '-b', 'main')
        commit_version(repo, '0.41.0', 'base')
        good = commit_version(repo, V, 'release')
        git(repo, 'tag', 'candidate/v' + V, good)
        # a side commit reachable only through a merge's second parent
        git(repo, 'checkout', '-q', '-b', 'side')
        side = commit_version(repo, '0.43.0', 'side')
        git(repo, 'checkout', '-q', 'main')
        git(repo, 'merge', '-q', '--no-ff', '-m', 'merge side', 'side')
        git(repo, 'tag', 'candidate/v0.43.0', side)
        git(repo, 'tag', '-a', '-m', 'annotated', 'candidate/v0.44.0', good)
        git(repo, 'tag', 'candidate/v0.45.0', good)
        git(repo, 'tag', 'candidate/v0.0.0', good)

        facts = CANDIDATE.check_ref(repo, 'refs/tags/candidate/v' + V, good, 'refs/heads/main')
        self.assertEqual(facts, {'ref': 'refs/tags/candidate/v' + V, 'version': V, 'commit': good,
                                 'targets': [MAC, LINUX]})

        def refused(ref, sha_, reason):
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.check_ref(repo, ref, sha_, 'refs/heads/main')
            self.assertEqual(str(caught.exception), reason)

        refused('refs/heads/main', good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/v' + V, good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/candidate/v0.42.1', good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/candidate/v1.42.0', good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/candidate/v0.042.0', good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/candidate/v0.0.0', good, 'the ref is not refs/tags/candidate/v<V>')
        refused('refs/tags/candidate/v' + V, good[:12], 'the commit is not a full 40-hex SHA')
        refused('refs/tags/candidate/v' + V, good.upper(), 'the commit is not a full 40-hex SHA')
        refused('refs/tags/candidate/v' + V, side, 'the candidate ref does not resolve directly to the commit')
        refused('refs/tags/candidate/v0.44.0', good, 'the candidate ref does not resolve directly to the commit')
        refused('refs/tags/candidate/v0.43.0', side, "the commit is not on main's first-parent chain")
        refused('refs/tags/candidate/v0.45.0', good, 'Cargo version 0.42.0 at the commit is not 0.45.0')
        refused('refs/tags/candidate/v0.46.0', good, 'the candidate ref does not resolve directly to the commit')

        # a reserved or released version never builds again
        git(repo, 'tag', 'reserve/v' + V, good)
        refused('refs/tags/candidate/v' + V, good,
                'v0.42.0 is already reserved or released; a retry reuses its reserved candidate and never rebuilds')
        git(repo, 'tag', '-d', 'reserve/v' + V)
        git(repo, 'tag', 'v' + V, good)
        refused('refs/tags/candidate/v' + V, good,
                'v0.42.0 is already reserved or released; a retry reuses its reserved candidate and never rebuilds')
        git(repo, 'tag', '-d', 'v' + V)

        # the target policy at S is exactly the two release targets
        (repo / 'release-keys').mkdir()
        (repo / 'release-keys/policy.json').write_text(json.dumps({'targets': [MAC, LINUX, 'x86_64-pc-windows-msvc']}))
        git(repo, 'add', 'release-keys/policy.json')
        git(repo, 'commit', '-q', '-m', 'wider policy')
        commit_version(repo, '0.47.0', 'bump')
        wide = git(repo, 'rev-parse', 'HEAD')
        git(repo, 'tag', 'candidate/v0.47.0', wide)
        refused('refs/tags/candidate/v0.47.0', wide,
                'the target policy at the commit is not exactly aarch64-apple-darwin and x86_64-unknown-linux-gnu')

        # the build receipt keeps S
        archive = self.archives[LINUX]
        receipt = CANDIDATE.build_receipt(version=V, commit=good, target=LINUX, run_id=RUN_ID,
                                          run_attempt=ATTEMPT, archive_name=CANDIDATE.archive_name(V, LINUX),
                                          archive_bytes=archive, fast_gate=dict(FAST_GATE), sources=[])
        self.assertEqual(receipt['commit'], good)
        self.assertEqual(receipt['archive'], {'name': CANDIDATE.archive_name(V, LINUX),
                                              'sha256': sha(archive), 'size': len(archive)})
        self.assertTrue(CANDIDATE.canonical(receipt).endswith(b'}\n'))
        self.assertEqual(CANDIDATE.canonical(receipt), CANDIDATE.canonical(json.loads(CANDIDATE.canonical(receipt))))

    def test_rsig4_manifest_is_exactly_the_packaged_archives(self):
        candidate, out = self.assemble({MAC: self.build_input(MAC), LINUX: self.build_input(LINUX)})
        names = sorted([CANDIDATE.archive_name(V, MAC), CANDIDATE.archive_name(V, LINUX)])
        manifest = ''.join(sha(self.archives[t]) + '  ' + CANDIDATE.archive_name(V, t) + '\n'
                           for t in (MAC, LINUX)).encode()
        self.assertEqual((out / 'SHA256SUMS').read_bytes(), manifest)
        self.assertEqual(sorted(p.name for p in out.iterdir()), sorted(names + ['SHA256SUMS', 'candidate.json']))
        for target in (MAC, LINUX):
            self.assertEqual((out / CANDIDATE.archive_name(V, target)).read_bytes(), self.archives[target])
        self.assertEqual(candidate['manifest_sha256'], sha(manifest))
        self.assertEqual(candidate['artifacts'], [{'name': CANDIDATE.archive_name(V, t), 'sha256': sha(self.archives[t]),
                                                   'size': len(self.archives[t])} for t in (MAC, LINUX)])
        self.assertEqual((out / 'candidate.json').read_bytes(), CANDIDATE.canonical(candidate))
        self.assertEqual([b['target'] for b in candidate['builds']], [MAC, LINUX])
        mac = candidate['builds'][0]
        self.assertEqual(mac['artifact']['id'], 100)
        self.assertEqual(mac['artifact']['name'], 'rdy-candidate-' + MAC + '-a2')
        self.assertTrue(re.fullmatch('sha256:[0-9a-f]{64}', mac['artifact']['digest']))
        self.assertEqual(mac['fast_gate'], FAST_GATE)
        self.assertEqual((candidate['run_id'], candidate['run_attempt'], candidate['commit'], candidate['version']),
                         (RUN_ID, ATTEMPT, S, V))

        def refused(inputs, reason):
            with self.assertRaises(ControllerError) as caught:
                self.assemble(inputs, self.tmp / ('out-' + str(len(list(self.tmp.iterdir())))))
            self.assertEqual(str(caught.exception), reason)

        good_linux = self.build_input(LINUX)
        refused({MAC: self.build_input(MAC)}, 'the candidate needs exactly the aarch64-apple-darwin and '
                'x86_64-unknown-linux-gnu builds')
        refused({MAC: self.build_input(MAC), LINUX: good_linux, 'x86_64-pc-windows-msvc': good_linux},
                'the candidate needs exactly the aarch64-apple-darwin and x86_64-unknown-linux-gnu builds')
        folder, ident, digest = self.build_input(MAC)
        refused({MAC: (folder, ident, 'sha256:' + 'c' * 64), LINUX: good_linux},
                'the aarch64-apple-darwin artifact does not match its artifact digest')
        refused({MAC: (folder, 0, digest), LINUX: good_linux}, 'the aarch64-apple-darwin artifact id is not a positive integer')
        refused({MAC: (folder, ident, digest[7:].upper()), LINUX: good_linux},
                'the aarch64-apple-darwin artifact digest is not sha256:<64 hex>')
        (folder / 'second.zip').write_bytes(b'x')
        refused({MAC: (folder, ident, digest), LINUX: good_linux},
                'the aarch64-apple-darwin download is not exactly one artifact file')
        refused({MAC: self.build_input(MAC, extra=[('SHA256SUMS.sig', b'test signature')]), LINUX: good_linux},
                'the aarch64-apple-darwin artifact is not exactly its archive and build receipt')
        refused({MAC: self.build_input(MAC, name='rdy-' + V + '-' + MAC + '.tar.gz'), LINUX: good_linux},
                'the aarch64-apple-darwin artifact is not exactly its archive and build receipt')
        archive = self.archives[MAC]
        for change, reason in [
                ({'commit': 'b' * 40}, 'the aarch64-apple-darwin build receipt does not match commit'),
                ({'version': '0.43.0'}, 'the aarch64-apple-darwin build receipt does not match version'),
                ({'target': LINUX}, 'the aarch64-apple-darwin build receipt does not match target'),
                ({'run_attempt': 1}, 'the aarch64-apple-darwin build receipt does not match run_attempt'),
                ({'run_id': 1}, 'the aarch64-apple-darwin build receipt does not match run_id'),
                ({'archive': {'name': CANDIDATE.archive_name(V, MAC), 'sha256': 'd' * 64, 'size': len(archive)}},
                 'the aarch64-apple-darwin archive does not match its build receipt'),
                ({'archive': {'name': CANDIDATE.archive_name(V, MAC), 'sha256': sha(archive), 'size': 1}},
                 'the aarch64-apple-darwin archive does not match its build receipt'),
                ({'host_paths': 1}, 'the aarch64-apple-darwin build receipt does not match host_paths'),
                ({'fast_gate': dict(FAST_GATE, prerequisite_skipped=1)},
                 'the aarch64-apple-darwin fast gate did not pass with every prerequisite'),
                ({'fast_gate': dict(FAST_GATE, passed=1.0)}, 'evidence holds a float'),
                ({'archived': True}, "unknown field 'archived'; did you mean 'archive'?"),
                ({'zzz': 1}, "unknown field 'zzz'")]:
            receipt = build_receipt(MAC, archive, **change)
            refused({MAC: self.build_input(MAC, receipt=receipt), LINUX: good_linux}, reason)
        missing = build_receipt(MAC, archive)
        del missing['sources']
        refused({MAC: self.build_input(MAC, receipt=missing), LINUX: good_linux}, "missing field 'sources'")
        duplicate = CANDIDATE.canonical(build_receipt(MAC, archive)).replace(b'{"archive"', b'{"schema":1,"archive"', 1)
        folder = self.tmp / 'dup'
        folder.mkdir()
        digest = artifact_zip(folder / 'a.zip', [(CANDIDATE.archive_name(V, MAC), archive),
                                                 ('build-' + MAC + '.json', duplicate)])
        refused({MAC: (folder, 100, digest), LINUX: good_linux}, "duplicate field 'schema'")
        # the same artifact twice is not two builds
        refused({MAC: self.build_input(MAC, artifact_id=7), LINUX: self.build_input(LINUX, artifact_id=7)},
                'two builds share one artifact id')
        # nothing is left in a refused output
        self.assertFalse(any(p.name.startswith('out-') and any(p.iterdir()) for p in self.tmp.iterdir()
                             if p.is_dir()))
        # an existing output directory is never reused
        with self.assertRaises(ControllerError) as caught:
            self.assemble({MAC: self.build_input(MAC), LINUX: self.build_input(LINUX)}, out)
        self.assertEqual(str(caught.exception), 'the output directory already exists')

    def test_rsig4_native_jobs_are_required_and_report_counts(self):
        counts = CANDIDATE.fast_gate_counts(gate_report(), S)
        self.assertEqual(counts, FAST_GATE)
        for change, reason in [
                ({'pass': False}, 'the native fast gate did not pass'),
                ({'dirty': True}, 'the native fast gate ran on a changed tree'),
                ({'revision': 'b' * 9}, 'the native fast gate ran on another commit'),
                ({'revision': 'unknown'}, 'the native fast gate ran on another commit'),
                ({'areas': [{'name': 'cli', 'passed': 3, 'failed': 1, 'ignored': 0, 'skipped': 0}]},
                 'the native fast gate did not pass with every prerequisite'),
                ({'areas': [{'name': 'cli', 'passed': 3, 'failed': 0, 'ignored': 0, 'skipped': 2}]},
                 'the native fast gate did not pass with every prerequisite'),
                ({'gates': [], 'areas': []}, 'the native fast gate ran no test'),
                ({'areas': [{'name': 'cli', 'passed': -1, 'failed': 0, 'ignored': 0, 'skipped': 0}]},
                 'the native fast gate report is malformed'),
                ({'gates': [{'name': 'workspace tests', 'pass': True, 'detail': '1 passed, 0 failed'}]},
                 'the native fast gate report is malformed')]:
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.fast_gate_counts(gate_report(**change), S)
            self.assertEqual(str(caught.exception), reason)
        with self.assertRaises(ControllerError) as caught:
            CANDIDATE.fast_gate_counts(b'{"pass": true, "pass": true}', S)
        self.assertEqual(str(caught.exception), "duplicate field 'pass'")

        jobs = workflow_jobs(WORKFLOW)
        self.assertEqual(sorted(jobs), ['assemble', 'build-linux', 'build-mac', 'check'])
        self.assertIn('\npermissions: {}\n', WORKFLOW)
        self.assertNotIn('continue-on-error', WORKFLOW)
        self.assertNotIn('id-token', WORKFLOW)
        self.assertNotIn('secrets.', WORKFLOW)
        self.assertNotIn('always()', WORKFLOW)
        self.assertNotIn('actions: write', WORKFLOW)
        self.assertRegex(WORKFLOW, r'push:\n    tags: \["candidate/v\*"\]\n')
        for line in WORKFLOW.splitlines():
            if line.strip().startswith('- uses:') or line.strip().startswith('uses:'):
                self.assertRegex(line, r'uses: [\w./-]+@[0-9a-f]{40} # v')
        for name, runner in (('build-mac', 'macos-26'), ('build-linux', 'ubuntu-26.04')):
            job = jobs[name]
            self.assertIn('    runs-on: ' + runner + '\n', job)
            self.assertIn('    timeout-minutes: 180\n', job)
            self.assertIn('    needs: check\n', job)
            self.assertIn('    permissions:\n      contents: read\n    outputs:', job)
            self.assertIn('cargo qa --skip-perf --require-all', job)
            self.assertIn('candidate.py gate-counts', job)
            self.assertIn('candidate.py check-ref', job)
            self.assertIn('fetch-depth: 0', job)
            self.assertLess(job.index('candidate.py check-ref'), job.index('cargo qa'))
            self.assertIn('candidate.py build', job)
            self.assertIn('--fast-gate', job)
            self.assertIn('persist-credentials: false', job)
            target = MAC if name == 'build-mac' else LINUX
            self.assertIn('--target ' + target, job)
            self.assertIn('name: rdy-candidate-' + target + '-a${{ github.run_attempt }}', job)
            self.assertIn('if-no-files-found: error', job)
            self.assertIn('artifact_id: ${{ steps.upload.outputs.artifact-id }}', job)
            self.assertIn('artifact_digest: ${{ steps.upload.outputs.artifact-digest }}', job)
        assemble = jobs['assemble']
        self.assertIn('    needs: [check, build-mac, build-linux]\n', assemble)
        self.assertIn('    permissions:\n      contents: read\n', assemble)
        self.assertEqual(assemble.count('skip-decompress: true'), 2)
        self.assertEqual(assemble.count('digest-mismatch: error'), 2)
        self.assertIn('artifact-ids: ${{ needs.build-mac.outputs.artifact_id }}', assemble)
        self.assertIn('artifact-ids: ${{ needs.build-linux.outputs.artifact_id }}', assemble)
        self.assertIn('candidate.py assemble', assemble)
        self.assertIn('name: rdy-candidate-a${{ github.run_attempt }}', assemble)
        self.assertIn('    permissions:\n      contents: read\n', jobs['check'])
        self.assertIn('candidate.py check-ref', jobs['check'])
        self.assertIn('fetch-depth: 0', jobs['check'])
        self.assertIn('group: rdy-core-candidate-${{ github.ref }}\n  cancel-in-progress: false\n', WORKFLOW)
        self.assertNotIn('rdy-core-release-authority', WORKFLOW)
        self.assertNotIn('rehearse', WORKFLOW)

    def test_rsig4_host_paths_refuse_the_candidate(self):
        flags = CANDIDATE.remap_flags(home='/Users/runner', cargo_home='/Users/runner/.cargo',
                                      rustup_home='/Users/runner/.rustup', source='/tmp/w/src', target_dir='/tmp/w/target')
        self.assertEqual(flags, ['--remap-path-prefix=/Users/runner=/build', '--remap-path-prefix=/home/runner=/build',
                                 '--remap-path-prefix=/Users/runner/.cargo=/cargo',
                                 '--remap-path-prefix=/Users/runner/.rustup=/rustup',
                                 '--remap-path-prefix=/tmp/w/src=/rdy', '--remap-path-prefix=/tmp/w/target=/target'])
        flags = CANDIDATE.remap_flags(home='/home/builder', cargo_home='/opt/cargo', rustup_home='/opt/rustup',
                                      source='/home/builder/w/src', target_dir='/home/builder/w/target')
        self.assertEqual(flags[:3], ['--remap-path-prefix=/Users/runner=/build', '--remap-path-prefix=/home/runner=/build',
                                     '--remap-path-prefix=/home/builder=/build'])
        with self.assertRaises(ControllerError) as caught:
            CANDIDATE.remap_flags(home='/home/a b', cargo_home='/c', rustup_home='/r', source='/s', target_dir='/t')
        self.assertEqual(str(caught.exception), 'a build path is not a plain absolute path')

        path = self.tmp / CANDIDATE.archive_name(V, LINUX)
        path.write_bytes(self.archives[LINUX])
        self.assertEqual(CANDIDATE.inspect_archive(path, V, LINUX, extra=['/tmp/w']), 0)
        filler = b'\0' * (CANDIDATE.SCAN_CHUNK - 3)
        for binary in [b'panic at /home/runner/work/rdy-core/src/main.rs', b'/Users/runner/.cargo/registry/x',
                       b'/Users/nick/Repo/rdy', b'/home/ubuntu/.rustup/toolchains', filler + b'/home/runner/split',
                       b'built in /tmp/w/src/crates']:
            path.write_bytes(release_archive(LINUX, binary))
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.inspect_archive(path, V, LINUX, extra=['/tmp/w'])
            self.assertEqual(str(caught.exception), 'the x86_64-unknown-linux-gnu binary holds 1 host path')
            self.assertNotIn('runner', str(caught.exception))
        path.write_bytes(release_archive(LINUX, b'/home/a and /Users/b'))
        with self.assertRaises(ControllerError) as caught:
            CANDIDATE.inspect_archive(path, V, LINUX, extra=[])
        self.assertEqual(str(caught.exception), 'the x86_64-unknown-linux-gnu binary holds 2 host paths')
        stem = 'rdy-' + V + '-preview-' + LINUX
        for members in ([(stem + '/LICENSE-MIT', b'x', 'file')],
                        [(stem + '/rdy', b'a', 'file'), (stem + '/rdy', b'b', 'file')],
                        [(stem + '/rdy', '/etc/passwd', 'link')],
                        [(stem + '/rdy', b'a', 'file'), ('../escape', b'x', 'file')],
                        [('rdy-' + V + '-' + LINUX + '/rdy', b'a', 'file')]):
            path.write_bytes(tar_gz(members))
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.inspect_archive(path, V, LINUX, extra=[])
            self.assertEqual(str(caught.exception), 'the x86_64-unknown-linux-gnu archive is not one packaged release')


class FakeSigner:
    """A fake isolated test signer: a script that keeps key state as marker files."""
    SOURCE = r'''#!{exe}
import json, os, sys
log = os.environ.get('FAKE_LOG') or sys.argv[0] + '.log'
state = os.environ['RDY_REHEARSAL_STATE']
held = sys.argv[0] + '.key'
verb = sys.argv[1:]
with open(sys.argv[0] + '.calls', 'a') as calls:
    calls.write(json.dumps({'argv': verb, 'env': sorted(os.environ)}) + '\n')
mode = open(sys.argv[0] + '.mode').read().strip()
if verb == ['sign']:
    manifest = sys.stdin.buffer.read()
    open(sys.argv[0] + '.stdin', 'wb').write(manifest)
    open(os.path.join(state, 'test-key'), 'w').write('test private key marker')
    open(held, 'w').write('held test key marker')
    if mode == 'badjson':
        sys.stdout.write('{"signature": "x", "signer_line": "y", "extra": 1}')
        sys.exit(0)
    sig = '-----BEGIN SSH SIGNATURE-----\nU1NIU0lHdGVzdA==\n-----END SSH SIGNATURE-----\n'
    line = 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeTestRootOnly'
    sys.stdout.write(json.dumps({'signature': sig, 'signer_line': line}))
    sys.exit(0)
if verb == ['destroy']:
    if mode == 'nocleanup':
        sys.exit(1)
    if os.path.exists(held):
        os.remove(held)
    sys.exit(0)
sys.exit(64)
'''

    def __init__(self, folder, mode='ok'):
        self.path = folder / 'test-signer'
        self.path.write_text(self.SOURCE.replace('{exe}', sys.executable))
        self.path.chmod(0o700)
        Path(str(self.path) + '.mode').write_text(mode)

    def calls(self):
        p = Path(str(self.path) + '.calls')
        return [json.loads(line) for line in p.read_text().splitlines()] if p.exists() else []

    def holds_key(self):
        return Path(str(self.path) + '.key').exists()

    def stdin(self):
        return Path(str(self.path) + '.stdin').read_bytes()


def install_report(target, **change):
    report = {'binary': '/x/bin/rdy', 'version': V, 'commit': S, 'target': target, 'os': 'Test 1.0',
              'checks': [{'name': n, 'passed': True} for n in REHEARSE.CHECK_NAMES], 'skipped': 0}
    report.update(change)
    return json.dumps(report).encode()


class rsig4_Rehearsal(Fixture):
    def candidate_zip(self, tamper=None):
        out = self.tmp / 'candidate'
        if out.exists():
            out = self.tmp / ('candidate-' + str(len(list(self.tmp.iterdir()))))
        candidate, out = self.assemble({MAC: self.build_input(MAC), LINUX: self.build_input(LINUX)}, out)
        members = [(p.name, p.read_bytes()) for p in sorted(out.iterdir())]
        if tamper:
            members = tamper(members)
        folder = self.tmp / ('cand-' + str(len(list(self.tmp.iterdir()))))
        folder.mkdir()
        digest = artifact_zip(folder / 'candidate.zip', members)
        return folder / 'candidate.zip', digest, candidate

    def checker(self, seen, report=None, raises=None):
        def run(argv, boundary):
            seen.append({'argv': argv, 'files': {p.name: p.read_bytes() for p in Path(argv[argv.index('--download') + 1]).iterdir()},
                         'mode': Path(argv[argv.index('--download') + 1]).stat().st_mode & 0o777, 'boundary': boundary})
            if raises:
                raise raises
            return report if report is not None else install_report(argv[argv.index('--target') + 1])
        return run

    def test_rsig4_rehearsal_uses_exact_bytes_and_erases_test_state(self):
        zpath, digest, candidate = self.candidate_zip()
        signer = FakeSigner(self.tmp)
        seen = []
        work = self.tmp / 'work'
        work.mkdir(mode=0o700)
        out = self.tmp / 'receipt.json'
        env = {'PATH': os.environ['PATH'], 'HOME': str(self.tmp), 'LANG': 'C.UTF-8'}
        receipt = REHEARSE.rehearse(zpath, digest, candidate['manifest_sha256'], LINUX, signer=[str(signer.path)],
                                    workdir=work, out=out, checker=self.checker(seen), env=env)
        manifest = (self.tmp / 'candidate' / 'SHA256SUMS').read_bytes()
        # the signer saw exactly the fixed manifest and nothing else
        self.assertEqual(signer.stdin(), manifest)
        self.assertEqual([c['argv'] for c in signer.calls()], [['sign'], ['destroy']])
        for call in signer.calls():
            self.assertFalse([k for k in call['env'] if k.startswith(('AWS_', 'ACTIONS_', 'GITHUB_'))], call['env'])
        # the checker ran on the exact candidate bytes with the test signature, inside the boundary
        self.assertEqual(len(seen), 1)
        files = seen[0]['files']
        name = CANDIDATE.archive_name(V, LINUX)
        self.assertEqual(sorted(files), sorted([name, 'SHA256SUMS', 'SHA256SUMS.sig']))
        self.assertEqual(files[name], self.archives[LINUX])
        self.assertEqual(files['SHA256SUMS'], manifest)
        self.assertEqual(seen[0]['mode'], 0o700)
        argv = seen[0]['argv']
        self.assertEqual(argv[argv.index('--signer-line') + 1][:12], 'rdy-release ')
        self.assertEqual(argv[argv.index('--commit') + 1], S)
        self.assertEqual(argv[argv.index('--version') + 1], V)
        self.assertTrue(argv[argv.index('--prefix') + 1].endswith('/rdy-' + V))
        # test state is destroyed: the signer's key, the boundary and every test file
        self.assertFalse(signer.holds_key())
        self.assertEqual(list(work.iterdir()), [])
        # only the sanitized receipt leaves
        data = out.read_bytes()
        self.assertEqual(data, CANDIDATE.canonical(receipt))
        self.assertNotIn(b'SSH SIGNATURE', data)
        self.assertNotIn(b'ssh-ed25519', data)
        self.assertNotIn(b'FakeTestRootOnly', data)
        self.assertEqual(sorted(receipt), ['archive', 'cleanup', 'commit', 'install', 'manifest_sha256', 'schema',
                                           'session', 'target', 'version'])
        self.assertIsNone(receipt['session'])
        self.assertEqual(receipt['archive'], {'name': name, 'sha256': sha(self.archives[LINUX]),
                                              'size': len(self.archives[LINUX])})
        self.assertEqual(receipt['install']['checks'], [{'name': n, 'passed': True} for n in REHEARSE.CHECK_NAMES])
        self.assertEqual(receipt['install']['skipped'], 0)
        self.assertEqual(receipt['install']['report_sha256'], sha(install_report(LINUX)))
        self.assertEqual(receipt['cleanup'], 'destroyed')

        def refused(reason, zp=zpath, dg=digest, manifest_sha=candidate['manifest_sha256'], mode='ok',
                    checker=None, environment=env, signed=False):
            fake = FakeSigner(Path(tempfile.mkdtemp(dir=self.tmp)), mode)
            receipt_path = self.tmp / 'refused.json'
            with self.assertRaises(ControllerError) as caught:
                REHEARSE.rehearse(zp, dg, manifest_sha, LINUX, signer=[str(fake.path)], workdir=work,
                                  out=receipt_path, checker=checker or self.checker([]), env=environment)
            self.assertEqual(str(caught.exception), reason)
            self.assertFalse(receipt_path.exists())
            self.assertEqual(list(work.iterdir()), [])
            self.assertEqual(bool(fake.calls()), signed)
            return fake

        # bytes are checked before the signer runs
        refused('the candidate artifact does not match its artifact digest', dg='sha256:' + 'e' * 64)
        refused('the candidate manifest does not match the expected manifest digest', manifest_sha='f' * 64)

        def swap(members):
            return [(n, self.archives[MAC] if n == name else d) for n, d in members]
        tz, tdigest, _ = self.candidate_zip(swap)
        refused('the candidate archives do not match their manifest', zp=tz, dg=tdigest)
        tz, tdigest, _ = self.candidate_zip(lambda m: m + [('SHA256SUMS.sig', b'old')])
        refused('the candidate artifact is not exactly its archives, SHA256SUMS and candidate.json', zp=tz, dg=tdigest)
        # never with KMS or OIDC in reach
        for key in ('AWS_ACCESS_KEY_ID', 'AWS_ROLE_ARN', 'ACTIONS_ID_TOKEN_REQUEST_URL', 'RDY_RELEASE_KEY_ARN'):
            refused('the rehearsal environment holds release credentials or KMS settings',
                    environment=dict(env, **{key: 'x'}))
        # a bad signer answer, a failed check, a cancellation: all destroy the test state
        fake = refused("unknown field 'extra'", mode='badjson', signed=True)
        self.assertFalse(fake.holds_key())
        self.assertEqual([c['argv'] for c in fake.calls()], [['sign'], ['destroy']])
        fake = refused('the x86_64-unknown-linux-gnu install check did not pass all six checks',
                       checker=self.checker([], report=install_report(LINUX, skipped=1)), signed=True)
        self.assertFalse(fake.holds_key())
        fake = refused('the x86_64-unknown-linux-gnu install check did not pass all six checks',
                       checker=self.checker([], report=install_report(
                           LINUX, checks=[{'name': n, 'passed': n != 'mcp'} for n in REHEARSE.CHECK_NAMES])), signed=True)
        fake = refused('the x86_64-unknown-linux-gnu install report does not match commit',
                       checker=self.checker([], report=install_report(LINUX, commit='b' * 40)), signed=True)
        fake = refused('the rehearsal was cancelled; its test state was destroyed',
                       checker=self.checker([], raises=KeyboardInterrupt()), signed=True)
        self.assertFalse(fake.holds_key())
        fake = refused('the rehearsal was cancelled; its test state was destroyed',
                       checker=self.checker([], raises=REHEARSE.Cancelled()), signed=True)
        self.assertFalse(fake.holds_key())
        # a cleanup failure is never a success, and says test state may remain
        fake = refused('rehearsal cleanup failed; test state may remain', mode='nocleanup', signed=True)
        self.assertTrue(fake.holds_key())
        fake = refused('rehearsal cleanup failed; test state may remain', mode='nocleanup',
                       checker=self.checker([], raises=KeyboardInterrupt()), signed=True)
        # the receipt path is never overwritten
        out2 = self.tmp / 'exists.json'
        out2.write_text('x')
        with self.assertRaises(ControllerError) as caught:
            REHEARSE.rehearse(zpath, digest, candidate['manifest_sha256'], LINUX, signer=[str(signer.path)],
                              workdir=work, out=out2, checker=self.checker([]), env=env)
        self.assertEqual(str(caught.exception), 'the receipt file already exists')

    def test_rsig4_tutorial_receipts_bind_each_target(self):
        candidate, _ = self.assemble({MAC: self.build_input(MAC), LINUX: self.build_input(LINUX)})

        def entry(target, **change):
            value = {'target': target, 'agent': 'claude-code', 'model': 'claude-sonnet-5', 'effort': 'medium',
                     'session': 'session-' + target, 'tutorial_revision': 'c' * 40,
                     'steps': [{'id': 'install', 'result': 'pass'}, {'id': 'first-write', 'result': 'pass'}],
                     'archive_sha256': sha(self.archives.get(target, b'')), 'manifest_sha256': candidate['manifest_sha256'],
                     'result': 'pass'}
            value.update(change)
            return value

        good = [entry(LINUX), entry(MAC)]
        sealed = REHEARSE.check_tutorials(good, candidate)
        self.assertEqual([t['target'] for t in sealed], [MAC, LINUX])
        self.assertEqual(sealed[0], entry(MAC))

        def refused(entries, reason):
            with self.assertRaises(ControllerError) as caught:
                REHEARSE.check_tutorials(entries, candidate)
            self.assertEqual(str(caught.exception), reason)

        refused([entry(MAC)], 'the tutorials need exactly one entry per target')
        refused([entry(MAC), entry(MAC, session='other')], 'the tutorials need exactly one entry per target')
        refused([entry(MAC), entry(LINUX), entry('x86_64-pc-windows-msvc')],
                'the tutorials need exactly one entry per target')
        refused([entry(MAC, archive_sha256=sha(self.archives[LINUX])), entry(LINUX)],
                'the aarch64-apple-darwin tutorial did not use its candidate archive')
        refused([entry(MAC), entry(LINUX, manifest_sha256='d' * 64)],
                'the x86_64-unknown-linux-gnu tutorial did not use the candidate manifest')
        refused([entry(MAC, session='same'), entry(LINUX, session='same')],
                'each tutorial needs its own fresh session')
        refused([entry(MAC, steps=[{'id': 'install', 'result': 'fail'}]), entry(LINUX)],
                'the aarch64-apple-darwin tutorial did not pass every step')
        refused([entry(MAC, result='fail'), entry(LINUX)], 'the aarch64-apple-darwin tutorial did not pass every step')
        refused([entry(MAC, steps=[]), entry(LINUX)], 'the aarch64-apple-darwin tutorial steps are not 1 to 256 entries')
        refused([entry(MAC, steps=[{'id': str(i), 'result': 'pass'} for i in range(257)]), entry(LINUX)],
                'the aarch64-apple-darwin tutorial steps are not 1 to 256 entries')
        refused([entry(MAC, steps=[{'id': 'a', 'result': 'pass'}, {'id': 'a', 'result': 'pass'}]), entry(LINUX)],
                'the aarch64-apple-darwin tutorial repeats a step id')
        refused([entry(MAC, effort='high'), entry(LINUX)], 'the aarch64-apple-darwin tutorial is not a fresh Sonnet/medium session')
        refused([entry(MAC, model='claude-opus-5'), entry(LINUX)],
                'the aarch64-apple-darwin tutorial is not a fresh Sonnet/medium session')
        refused([entry(MAC, tutorial_revision=''), entry(LINUX)], 'the tutorial_revision is empty')
        refused([entry(MAC, sessions='x'), entry(LINUX)], "unknown field 'sessions'; did you mean 'session'?")
        missing = entry(MAC)
        del missing['agent']
        refused([missing, entry(LINUX)], "missing field 'agent'")
        refused([entry(MAC, steps=[{'id': 'a', 'result': True}]), entry(LINUX)],
                'the aarch64-apple-darwin tutorial step result is not pass or fail')
        refused([entry(MAC, session='s' * 257), entry(LINUX)], 'the session is longer than 256 bytes')

        # the CLI reads the receipts file and the candidate record, and prints the sealed list
        receipts = self.tmp / 'tutorials.json'
        receipts.write_bytes(CANDIDATE.canonical(good))
        cand = self.tmp / 'candidate' / 'candidate.json'
        result = subprocess.run([sys.executable, '-B', str(ROOT / 'scripts/release/rehearse.py'), 'tutorials',
                                 '--candidate', str(cand), '--receipts', str(receipts)],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual((result.returncode, result.stderr), (0, b''))
        self.assertEqual(result.stdout, CANDIDATE.canonical(sealed))
        receipts.write_bytes(CANDIDATE.canonical([entry(MAC)]))
        result = subprocess.run([sys.executable, '-B', str(ROOT / 'scripts/release/rehearse.py'), 'tutorials',
                                 '--candidate', str(cand), '--receipts', str(receipts)],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual((result.returncode, result.stdout),
                         (1, b''))
        self.assertEqual(result.stderr, b'release controller: the tutorials need exactly one entry per target\n')
        result = subprocess.run([sys.executable, '-B', str(ROOT / 'scripts/release/rehearse.py'), 'tutorials',
                                 '--candidate', str(cand)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2)


if __name__ == '__main__':
    unittest.main()
