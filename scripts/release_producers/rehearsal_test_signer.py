#!/usr/bin/python3 -IB
"""The disposable test signer of one native E rehearsal. H's `rehearse.py` runs it as one argv element
plus a verb, inside its private boundary:

  sign     cwd `<boundary>/state`; stdin is H's bounded, checked SHA256SUMS (at most 1024 bytes). Creates one
           new Ed25519 test key under `$RDY_REHEARSAL_STATE`, signs exactly the stdin bytes under namespace
           `rdy-release-v1`, and prints only `{"signature": <SSHSIG armor>, "signer_line": <principal line>}`
           on the private pipe to H.
  destroy  cwd `<boundary>`; deletes this helper's private and public test key, checks that both are gone and
           that the state holds nothing else, and exits 1 when cleanup is uncertain.

H gives the helper only PATH, LANG, LC_ALL, TZ, HOME, TMPDIR and RDY_REHEARSAL_STATE (the same folder as HOME,
new, empty, mode 0700). The helper takes no other argument: no key, path or command comes from its caller. It
refuses an existing state (no repeated signing), a key agent, and any key it did not create. It makes no network
request, runs no candidate code, and never prints its key, its signature (except to H) or its root. OpenSSH is
the platform's fixed `/usr/bin/ssh-keygen`; the native job records its version.

The shebang names the fixed system interpreter with isolated flags in one argument, because Linux passes the
whole shebang argument as one word. The code runs on Python 3.9 (`/usr/bin/python3` on macOS).
"""
import json
import os
import re
import stat
import subprocess
import sys

SSH_KEYGEN = '/usr/bin/ssh-keygen'
PRINCIPAL = 'rdy-release'
NAMESPACE = 'rdy-release-v1'
KEY = 'rehearsal_ed25519'
PUBLIC = KEY + '.pub'
OWNED = (KEY, PUBLIC)
MANIFEST_MAX = 1024        # H's manifest bound (candidate.MANIFEST_MAX)
ANSWER_MAX = 16 * 1024     # H's answer bound (rehearse.SIGNER_ANSWER_MAX)
DEADLINE = 60              # seconds for one ssh-keygen call; H's own deadline is 120
MANIFEST_RE = re.compile(rb'(?:[0-9a-f]{64}  [A-Za-z0-9][A-Za-z0-9._+-]{0,255}\n){1,32}')
PUBLIC_RE = re.compile(r'ssh-ed25519 ([A-Za-z0-9+/]+={0,2})(?: [^\n]*)?\n?')
BEGIN = '-----BEGIN SSH SIGNATURE-----\n'
END = '-----END SSH SIGNATURE-----\n'


class Refused(Exception):
    """A refusal: one short line on stderr, never key material."""


def state():
    """The helper's state folder: H's new private `RDY_REHEARSAL_STATE`, which is also HOME."""
    path = os.environ.get('RDY_REHEARSAL_STATE', '')
    if not path or not os.path.isabs(path) or os.environ.get('HOME') != path:
        raise Refused('the rehearsal state is not the isolated HOME')
    try:
        info = os.lstat(path)
    except OSError:
        raise Refused('the rehearsal state does not exist')
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise Refused('the rehearsal state is not a private directory')
    return path


def run(argv, stdin=b''):
    """`ssh-keygen` with a fixed argument list and the isolated environment; returns its stdout."""
    env = {k: os.environ[k] for k in ('PATH', 'LANG', 'LC_ALL', 'TZ', 'HOME', 'TMPDIR') if k in os.environ}
    try:
        done = subprocess.run([SSH_KEYGEN] + argv, input=stdin, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              env=env, timeout=DEADLINE)
    except (OSError, subprocess.TimeoutExpired):
        raise Refused('ssh-keygen did not run')
    if done.returncode != 0 or len(done.stdout) > ANSWER_MAX:
        raise Refused('ssh-keygen failed')
    return done.stdout


def sign(folder):
    if 'SSH_AUTH_SOCK' in os.environ:
        raise Refused('a key agent is in reach')
    if os.listdir(folder):
        raise Refused('the rehearsal state is not new')
    manifest = sys.stdin.buffer.read(MANIFEST_MAX + 1)
    if len(manifest) > MANIFEST_MAX or not MANIFEST_RE.fullmatch(manifest):
        raise Refused('the manifest is not a bounded SHA256SUMS')
    key = os.path.join(folder, KEY)
    try:
        run(['-q', '-t', 'ed25519', '-N', '', '-C', 'rdy-release-rehearsal', '-f', key])
        signature = run(['-Y', 'sign', '-f', key, '-n', NAMESPACE], manifest).decode('ascii', 'replace')
        with open(os.path.join(folder, PUBLIC), 'r') as handle:
            match = PUBLIC_RE.fullmatch(handle.read(4096))
        if not (match and signature.startswith(BEGIN) and signature.endswith(END)):
            raise Refused('the test signature is malformed')
    except BaseException:
        destroy(folder)  # H runs destroy too; a failed sign leaves no key behind either way
        raise
    line = PRINCIPAL + ' namespaces="' + NAMESPACE + '" ssh-ed25519 ' + match.group(1)
    answer = json.dumps({'signature': signature, 'signer_line': line}, sort_keys=True, separators=(',', ':'))
    sys.stdout.write(answer + '\n')


def destroy(folder):
    """Deletes the helper's key pair and confirms that the state holds nothing. No secure erasure is claimed."""
    for name in OWNED:
        path = os.path.join(folder, name)
        if os.path.lexists(path):
            os.unlink(path)
    if any(os.path.lexists(os.path.join(folder, name)) for name in OWNED) or os.listdir(folder):
        raise Refused('test state remains')


def main(argv):
    if len(argv) != 2 or argv[1] not in ('sign', 'destroy'):
        sys.stderr.write('rehearsal test signer: usage: rehearsal_test_signer.py sign|destroy\n')
        return 2
    try:
        folder = state()
        if argv[1] == 'sign':
            sign(folder)
        else:
            destroy(folder)
    except Refused as error:
        sys.stderr.write('rehearsal test signer: ' + str(error) + '\n')
        return 1
    except OSError:
        sys.stderr.write('rehearsal test signer: a file operation failed\n')
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
