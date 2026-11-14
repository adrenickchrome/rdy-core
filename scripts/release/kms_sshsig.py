#!/usr/bin/env python3
"""Sign a candidate's SHA256SUMS with rdy's release key in AWS KMS, as an OpenSSH SSHSIG.

The candidate workflow's sign job runs this after the build jobs. The AWS CLI gets its session from
the job's OIDC token (AWS_ROLE_ARN and AWS_WEB_IDENTITY_TOKEN_FILE). KMS signs the SSHSIG preimage
(Ed25519, RAW); this script wraps the 64-byte signature in the OpenSSH envelope. The job then checks
the result with `ssh-keygen -Y verify` against the release key's allowed-signers line.

usage: kms_sshsig.py <SHA256SUMS> <SHA256SUMS.sig>
"""
import base64
import hashlib
import json
import os
import struct
import subprocess
import sys
import textwrap

KEY_ARN = 'arn:aws:kms:us-east-2:682024900233:key/f3f465c1-a1b7-4bc0-8d00-e08b9d55923e'
REGION = 'us-east-2'
NAMESPACE = b'rdy-release-v1'
# The release key's public half, as in the release notes' allowed-signers line.
PUBLIC_KEY = 'AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8'
ALLOWED_SIGNERS = 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 ' + PUBLIC_KEY


def field(data):
    return struct.pack('>I', len(data)) + data


def preimage(data):
    """The bytes OpenSSH signs for `ssh-keygen -Y sign -n rdy-release-v1` with sha512."""
    return b'SSHSIG' + field(NAMESPACE) + field(b'') + field(b'sha512') + field(hashlib.sha512(data).digest())


def armor(raw_signature):
    """The SSHSIG envelope around a 64-byte Ed25519 signature, in OpenSSH's armor."""
    if len(raw_signature) != 64:
        raise SystemExit(f'kms_sshsig: KMS returned {len(raw_signature)} signature bytes, not 64')
    public = base64.b64decode(PUBLIC_KEY)
    signature = field(b'ssh-ed25519') + field(raw_signature)
    envelope = (b'SSHSIG' + struct.pack('>I', 1) + field(public) + field(NAMESPACE) + field(b'')
                + field(b'sha512') + field(signature))
    lines = textwrap.wrap(base64.b64encode(envelope).decode('ascii'), 76)  # as the releases since 0.46.0
    return ('-----BEGIN SSH SIGNATURE-----\n' + '\n'.join(lines) + '\n-----END SSH SIGNATURE-----\n').encode('ascii')


def kms_sign(message):
    command = ['aws', '--region', REGION, '--no-cli-pager', '--output', 'json', 'kms', 'sign',
               '--key-id', KEY_ARN, '--message-type', 'RAW', '--signing-algorithm', 'ED25519_SHA_512',
               '--message', 'fileb:///dev/stdin']
    done = subprocess.run(command, input=message, capture_output=True, timeout=120, check=False)
    if done.returncode != 0:
        raise SystemExit('kms_sshsig: aws kms sign failed: ' + done.stderr.decode('utf-8', 'replace').strip())
    return base64.b64decode(json.loads(done.stdout)['Signature'])


def main(argv):
    if len(argv) != 3:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    with open(argv[1], 'rb') as sums:
        data = sums.read()
    signature = armor(kms_sign(preimage(data)))
    os.makedirs(os.path.dirname(os.path.abspath(argv[2])), exist_ok=True)
    with open(argv[2], 'wb') as out:
        out.write(signature)
    print(f'kms_sshsig: signed {len(data)} bytes of {argv[1]}; wrote {argv[2]}')


if __name__ == '__main__':
    main(sys.argv)
