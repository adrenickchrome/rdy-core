"""kms_sshsig: the CI sign step gives the same SHA256SUMS.sig bytes as the released 0.51.0 signature."""
import base64
import os
import pathlib
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/release/kms_sshsig.py'
FIXTURES = ROOT / 'tests/ci/fixtures'
sys.path.insert(0, str(SCRIPT.parent))
import kms_sshsig  # noqa: E402


def raw_signature_of(armored):
    """The 64-byte Ed25519 signature inside an OpenSSH SSHSIG armor."""
    body = b''.join(line for line in armored.splitlines() if not line.startswith(b'-----'))
    blob = base64.b64decode(body)
    fields, at = [], 10  # 'SSHSIG' + uint32 version
    while at < len(blob):
        (size,) = struct.unpack('>I', blob[at:at + 4])
        fields.append(blob[at + 4:at + 4 + size])
        at += 4 + size
    signature = fields[-1]
    (size,) = struct.unpack('>I', signature[:4])
    return signature[8 + size:]


class KmsSshsig(unittest.TestCase):
    def setUp(self):
        self.sums = (FIXTURES / 'v0.51.0-SHA256SUMS').read_bytes()
        self.released = (FIXTURES / 'v0.51.0-SHA256SUMS.sig').read_bytes()
        self.raw = raw_signature_of(self.released)
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def fake_aws(self, signature_b64, exit_code=0):
        """An `aws` on PATH that records its arguments and stdin and prints one KMS Sign answer."""
        fake = self.tmp / 'bin' / 'aws'
        fake.parent.mkdir()
        fake.write_text(
            '#!/bin/sh\n'
            f'printf "%s\\n" "$@" > "{self.tmp}/args"\n'
            f'cat > "{self.tmp}/stdin"\n'
            f'echo \'{{"KeyId": "{kms_sshsig.KEY_ARN}", "Signature": "{signature_b64}", '
            '"SigningAlgorithm": "ED25519_SHA_512"}\'\n'
            f'exit {exit_code}\n')
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        return {**os.environ, 'PATH': f'{fake.parent}:{os.environ["PATH"]}'}

    def run_script(self, env):
        out = self.tmp / 'SHA256SUMS.sig'
        sums = self.tmp / 'SHA256SUMS'
        sums.write_bytes(self.sums)
        done = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), str(sums), str(out)],
                              capture_output=True, env=env)
        return done, out

    def test_output_is_byte_equal_to_the_released_signature(self):
        done, out = self.run_script(self.fake_aws(base64.b64encode(self.raw).decode()))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(out.read_bytes(), self.released)

    def test_kms_gets_the_sshsig_preimage_with_ed25519_raw_on_the_release_key(self):
        self.run_script(self.fake_aws(base64.b64encode(self.raw).decode()))
        self.assertEqual((self.tmp / 'stdin').read_bytes(), kms_sshsig.preimage(self.sums))
        args = (self.tmp / 'args').read_text().split('\n')
        for pair in (['--key-id', kms_sshsig.KEY_ARN], ['--message-type', 'RAW'],
                     ['--signing-algorithm', 'ED25519_SHA_512'], ['--region', 'us-east-2']):
            at = args.index(pair[0])
            self.assertEqual(args[at + 1], pair[1])

    @unittest.skipUnless(shutil.which('ssh-keygen'), 'ssh-keygen is not installed')
    def test_ssh_keygen_verifies_the_output_with_the_allowed_signers_line(self):
        done, out = self.run_script(self.fake_aws(base64.b64encode(self.raw).decode()))
        self.assertEqual(done.returncode, 0, done.stderr)
        signers = self.tmp / 'allowed_signers'
        signers.write_text(kms_sshsig.ALLOWED_SIGNERS + '\n')
        check = subprocess.run(['ssh-keygen', '-Y', 'verify', '-f', str(signers), '-I', 'rdy-release',
                                '-n', 'rdy-release-v1', '-s', str(out)], input=self.sums, capture_output=True)
        self.assertEqual(check.returncode, 0, check.stderr)

    def test_the_allowed_signers_line_is_the_one_in_the_release_notes(self):
        self.assertIn(kms_sshsig.ALLOWED_SIGNERS, (ROOT / 'release/notes.md').read_text())

    def test_a_failed_kms_call_writes_nothing(self):
        done, out = self.run_script(self.fake_aws('', exit_code=254))
        self.assertNotEqual(done.returncode, 0)
        self.assertIn(b'aws kms sign failed', done.stderr)
        self.assertFalse(out.exists())

    def test_a_signature_of_the_wrong_length_writes_nothing(self):
        done, out = self.run_script(self.fake_aws(base64.b64encode(self.raw[:63]).decode()))
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()
