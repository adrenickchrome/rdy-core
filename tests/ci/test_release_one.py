"""Release 1 candidate contracts (relc1): three native builds, the frozen sample, the plain names and
the Windows build. No network, GitHub write, key or signature."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_release_candidate as base  # noqa: E402

CANDIDATE = base.CANDIDATE
ControllerError = base.ControllerError
V, S, RUN_ID, ATTEMPT = base.V, base.S, base.RUN_ID, base.ATTEMPT
MAC, WIN, LINUX = CANDIDATE.RELEASE_TARGETS
SAMPLE = b'a bounded synthetic stand-in for the frozen sample dump\n'
sha = base.sha


def name_of(target):
    return 'rdy-' + V + '-' + target + '.tar.gz'


def plain_archive(target, binary=b'\x7fELF rdy release binary /rdy/crates/rdy-cli/src/main.rs\n'):
    stem = 'rdy-' + V + '-' + target
    exe = 'rdy.exe' if target == WIN else 'rdy'
    return base.tar_gz([(stem, None, 'dir'), (stem + '/' + exe, binary, 'file'),
                        (stem + '/LICENSE-MIT', b'MIT\n', 'file'),
                        (stem + '/LICENSE-APACHE', b'Apache\n', 'file'),
                        (stem + '/THIRD-PARTY-NOTICES.txt', b'notices\n', 'file')])


def receipt_of(target, data, **change):
    receipt = base.build_receipt(target, data)
    receipt['archive']['name'] = name_of(target)
    receipt.update(change)
    return receipt


class Release(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.archives = {t: plain_archive(t) for t in CANDIDATE.RELEASE_TARGETS}
        self.source = self.tmp / 'source'
        self.write_sample()

    def write_sample(self, data=SAMPLE, freeze=None):
        folder = self.source / 'samples' / 'tally'
        folder.mkdir(parents=True, exist_ok=True)
        (folder / 'tally.rdydump').write_bytes(data)
        record = {'sha256': sha(SAMPLE), 'size': len(SAMPLE)} if freeze is None else freeze
        (folder / 'freeze.json').write_bytes(base.plain_json(record))

    def build_input(self, target, receipt=None, extra=(), name=None, artifact_id=None):
        folder = self.tmp / ('in-' + target + '-' + str(len(list(self.tmp.iterdir()))))
        folder.mkdir()
        archive = self.archives[target]
        receipt = receipt_of(target, archive) if receipt is None else receipt
        members = [(name or name_of(target), archive),
                   ('build-' + target + '.json', base.plain_json(receipt))] + list(extra)
        digest = base.artifact_zip(folder / 'artifact.zip', members)
        return (folder, artifact_id or 100 + CANDIDATE.RELEASE_TARGETS.index(target), digest)

    def inputs(self):
        return {t: self.build_input(t) for t in CANDIDATE.RELEASE_TARGETS}

    def assemble(self, inputs=None, out=None, source=None):
        out = out or self.tmp / 'candidate'
        return CANDIDATE.assemble(inputs or self.inputs(), version=V, commit=S, run_id=RUN_ID,
                                  run_attempt=ATTEMPT, out=out, source=source or self.source), out

    def refused(self, reason, **kw):
        out = self.tmp / ('out-' + str(len(list(self.tmp.iterdir()))))
        with self.assertRaises(ControllerError) as caught:
            self.assemble(out=out, **kw)
        self.assertEqual(str(caught.exception), reason)
        self.assertFalse(out.exists(), 'a refused candidate leaves no output')


class relc1_Candidate(Release):
    def test_relc1_candidate_assembles_three_builds_and_frozen_sample(self):
        record, out = self.assemble()
        names = [name_of(t) for t in (MAC, WIN, LINUX)] + ['tally.rdydump']
        manifest = ''.join(sha(self.archives[t]) + '  ' + name_of(t) + '\n' for t in (MAC, WIN, LINUX))
        manifest += sha(SAMPLE) + '  tally.rdydump\n'
        self.assertEqual((out / 'SHA256SUMS').read_bytes(), manifest.encode())
        self.assertEqual(sorted(p.name for p in out.iterdir()), sorted(names + ['SHA256SUMS', 'candidate.json']))
        self.assertEqual((out / 'tally.rdydump').read_bytes(), SAMPLE)
        for target in (MAC, WIN, LINUX):
            self.assertEqual((out / name_of(target)).read_bytes(), self.archives[target])
        self.assertEqual(record['schema'], 2)
        self.assertEqual([a['name'] for a in record['artifacts']], names)
        self.assertEqual(record['manifest_sha256'], sha(manifest.encode()))
        self.assertEqual(record['sample'], {'name': 'tally.rdydump', 'source': 'samples/tally/tally.rdydump',
                                            'commit': S, 'sha256': sha(SAMPLE), 'size': len(SAMPLE),
                                            'freeze_sha256': sha(base.plain_json({'sha256': sha(SAMPLE),
                                                                                 'size': len(SAMPLE)}))})
        self.assertEqual([b['target'] for b in record['builds']], [MAC, WIN, LINUX])
        self.assertEqual((out / 'candidate.json').read_bytes(), CANDIDATE.canonical(record))
        self.assertEqual(CANDIDATE.load_release_candidate((out / 'candidate.json').read_bytes()), record)
        self.assertLess(len(manifest), CANDIDATE.MANIFEST_MAX)

    def test_relc1_candidate_without_a_sample_folder_has_no_sample_entry(self):
        shutil.rmtree(self.source / 'samples')
        record, out = self.assemble()
        names = [name_of(t) for t in (MAC, WIN, LINUX)]
        manifest = ''.join(sha(self.archives[t]) + '  ' + name_of(t) + '\n' for t in (MAC, WIN, LINUX))
        self.assertEqual((out / 'SHA256SUMS').read_bytes(), manifest.encode())
        self.assertEqual(sorted(p.name for p in out.iterdir()), sorted(names + ['SHA256SUMS', 'candidate.json']))
        self.assertEqual(record['schema'], 2)
        self.assertNotIn('sample', record)
        self.assertEqual([a['name'] for a in record['artifacts']], names)
        self.assertEqual(CANDIDATE.load_release_candidate((out / 'candidate.json').read_bytes()), record)

    def test_relc1_candidate_bounds_are_one_checked_calculation(self):
        self.assertEqual(CANDIDATE.ARCHIVE_MAX, 536870912)
        self.assertEqual(CANDIDATE.SAMPLE_MAX, 16777216)
        self.assertEqual(CANDIDATE.RECORD_MAX, 65536)
        self.assertEqual(CANDIDATE.MANIFEST_MAX, 1024)
        self.assertEqual(CANDIDATE.ZIP_MAX, 1628569600)
        self.assertEqual(CANDIDATE.ZIP_MAX, 3 * 536870912 + 16777216 + 2 * 65536 + 1048576)

    def test_relc1_candidate_refuses_a_missing_or_wrong_sample(self):
        (self.source / 'samples/tally/tally.rdydump').unlink()
        self.refused('the frozen sample samples/tally/tally.rdydump is missing at the commit')
        self.write_sample()
        (self.source / 'samples/tally/freeze.json').unlink()
        self.refused('the sample freeze record samples/tally/freeze.json is missing at the commit')
        self.write_sample(data=SAMPLE + b'x')
        self.refused('the sample does not match its freeze record')
        self.write_sample(freeze={'sha256': sha(SAMPLE), 'size': 1})
        self.refused('the sample does not match its freeze record')
        self.write_sample(freeze={'sha256': 'A' * 64, 'size': len(SAMPLE)})
        self.refused('the sample freeze record is malformed')
        self.write_sample(freeze={'size': len(SAMPLE)})
        self.refused('the sample freeze record is malformed')
        (self.source / 'samples/tally/freeze.json').write_bytes(b'{"sha256": 1.5}')
        self.refused('evidence holds a float')
        # a link in place of the sample is never followed
        self.write_sample()
        real = self.tmp / 'real'
        real.write_bytes(SAMPLE)
        (self.source / 'samples/tally/tally.rdydump').unlink()
        (self.source / 'samples/tally/tally.rdydump').symlink_to(real)
        self.refused('the frozen sample samples/tally/tally.rdydump is missing at the commit')

    def test_relc1_candidate_refuses_an_oversized_sample_before_reading_it(self):
        path = self.source / 'samples/tally/tally.rdydump'
        with open(str(path), 'wb') as handle:
            handle.truncate(CANDIDATE.SAMPLE_MAX + 1)
        self.refused('the frozen sample is larger than 16777216 bytes')
        with open(str(path), 'wb') as handle:
            handle.truncate(CANDIDATE.SAMPLE_MAX)
        digest = hashlib.sha256(b'\0' * CANDIDATE.SAMPLE_MAX).hexdigest()
        self.write_sample(data=path.read_bytes(), freeze={'sha256': digest, 'size': CANDIDATE.SAMPLE_MAX})
        record, out = self.assemble()
        self.assertEqual(record['sample']['size'], CANDIDATE.SAMPLE_MAX)

    def test_relc1_candidate_needs_exactly_the_three_builds(self):
        good = self.inputs()
        names = 'aarch64-apple-darwin, x86_64-pc-windows-msvc and x86_64-unknown-linux-gnu'
        reason = 'the candidate needs exactly the ' + names + ' builds'
        for drop in (MAC, WIN, LINUX):
            self.refused(reason, inputs={t: v for t, v in good.items() if t != drop})
        self.refused(reason, inputs=dict(good, **{'x86_64-apple-darwin': good[MAC]}))
        # the preview archive name is not the Release 1 name
        self.refused('the ' + WIN + ' artifact is not exactly its archive and build receipt',
                     inputs=dict(good, **{WIN: self.build_input(WIN, name=base.CANDIDATE.archive_name(V, LINUX))}))
        self.refused('the ' + WIN + ' artifact is not exactly its archive and build receipt',
                     inputs=dict(good, **{WIN: self.build_input(WIN, name='rdy-' + V + '-preview-' + WIN + '.tar.gz')}))
        self.refused('two builds share one artifact id',
                     inputs=dict(good, **{WIN: self.build_input(WIN, artifact_id=100)}))
        self.refused('the ' + WIN + ' archive does not match its build receipt',
                     inputs=dict(good, **{WIN: self.build_input(WIN, receipt=receipt_of(
                         WIN, self.archives[WIN], archive={'name': name_of(WIN), 'sha256': 'd' * 64,
                                                           'size': len(self.archives[WIN])}))}))
        self.refused('the ' + WIN + ' build receipt does not match commit',
                     inputs=dict(good, **{WIN: self.build_input(WIN, receipt=receipt_of(
                         WIN, self.archives[WIN], commit='b' * 40))}))
        self.refused('the ' + WIN + ' fast gate did not pass with every prerequisite',
                     inputs=dict(good, **{WIN: self.build_input(WIN, receipt=receipt_of(
                         WIN, self.archives[WIN], fast_gate=dict(base.FAST_GATE, prerequisite_skipped=1)))}))

    def test_relc1_candidate_refuses_a_second_copy_of_an_output(self):
        self.assemble()
        with self.assertRaises(ControllerError) as caught:
            self.assemble(out=self.tmp / 'candidate')
        self.assertEqual(str(caught.exception), 'the output directory already exists')

    def test_relc1_historical_two_target_assembly_still_works(self):
        record = CANDIDATE.assemble({MAC: base.Fixture.build_input(self.hist(), MAC),
                                     LINUX: base.Fixture.build_input(self.hist(), LINUX)},
                                    version=V, commit=S, run_id=RUN_ID, run_attempt=ATTEMPT,
                                    out=self.tmp / 'historical')
        self.assertEqual(record['schema'], 1)
        self.assertEqual(len(record['artifacts']), 2)

    def hist(self):
        class Shim:
            pass
        shim = Shim()
        shim.tmp = self.tmp
        shim.archives = {MAC: base.release_archive(MAC), LINUX: base.release_archive(LINUX)}
        return shim


class relc1_Windows(Release):
    def test_relc1_windows_remaps_are_windows_paths(self):
        flags = CANDIDATE.remap_flags(home='C:\\Users\\runneradmin', cargo_home='C:\\Users\\runneradmin\\.cargo',
                                      rustup_home='C:\\Users\\runneradmin\\.rustup', source='D:\\a\\_temp\\w\\src',
                                      target_dir='D:\\a\\_temp\\w\\target', windows=True)
        self.assertEqual(flags, ['--remap-path-prefix=C:\\Users\\runneradmin=/build',
                                 '--remap-path-prefix=C:\\Users\\runner=/build',
                                 '--remap-path-prefix=C:\\Users\\runneradmin\\.cargo=/cargo',
                                 '--remap-path-prefix=C:\\Users\\runneradmin\\.rustup=/rustup',
                                 '--remap-path-prefix=D:\\a\\_temp\\w\\src=/rdy',
                                 '--remap-path-prefix=D:\\a\\_temp\\w\\target=/target'])
        for bad in ('C:Users', 'relative\\path', '/posix/path', 'C:\\a b', 'C:\\a=b', 'C:\\'):
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.remap_flags(home=bad, cargo_home='C:\\c', rustup_home='C:\\r', source='C:\\s',
                                      target_dir='C:\\t', windows=True)
            self.assertEqual(str(caught.exception), 'a build path is not a plain absolute path')
        # a POSIX path is not a Windows build path, and a Windows path is not a POSIX one
        with self.assertRaises(ControllerError):
            CANDIDATE.remap_flags(home='C:\\Users\\x', cargo_home='/c', rustup_home='/r', source='/s',
                                  target_dir='/t')

    def test_relc1_windows_archive_is_scanned_in_utf8_and_utf16(self):
        path = self.tmp / name_of(WIN)
        path.write_bytes(self.archives[WIN])
        self.assertEqual(CANDIDATE.inspect_archive(path, V, WIN, extra=['D:\\a\\_temp\\w']), 0)
        filler = b'\0' * (CANDIDATE.SCAN_CHUNK - 3)
        leaks = [b'panic at C:\\Users\\runneradmin\\.cargo\\registry\\x.rs',
                 b'C:/Users/runner/work/rdy/src/main.rs',
                 'C:\\Users\\nick\\Repo'.encode('utf-16-le'),
                 'C:/Users/nick/Repo'.encode('utf-16-le'),
                 b'built in D:\\a\\_temp\\w\\src\\crates',
                 'D:\\a\\_temp\\w\\target'.encode('utf-16-le'),
                 filler + b'C:\\Users\\split']
        for leak in leaks:
            path.write_bytes(plain_archive(WIN, leak))
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.inspect_archive(path, V, WIN, extra=['D:\\a\\_temp\\w'])
            self.assertEqual(str(caught.exception), 'the ' + WIN + ' binary holds 1 host path', leak)
            self.assertNotIn('runner', str(caught.exception))
        # the archive holds rdy.exe under the plain stem, and nothing else is the binary
        stem = 'rdy-' + V + '-' + WIN
        for members in ([(stem + '/rdy', b'a', 'file')],
                        [(stem + '/rdy.exe', b'a', 'file'), (stem + '/rdy.exe', b'b', 'file')],
                        [('rdy-' + V + '-preview-' + WIN + '/rdy.exe', b'a', 'file')],
                        [(stem + '/rdy.exe', '/etc/passwd', 'link')]):
            path.write_bytes(base.tar_gz(members))
            with self.assertRaises(ControllerError) as caught:
                CANDIDATE.inspect_archive(path, V, WIN, extra=[])
            self.assertEqual(str(caught.exception), 'the ' + WIN + ' archive is not one packaged release')


if __name__ == '__main__':
    unittest.main()
