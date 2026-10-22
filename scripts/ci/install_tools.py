#!/usr/bin/env python3
"""Download checksum-pinned official CI binaries; extract only the named regular file."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import sys
import tarfile
import urllib.request

MAX_ARCHIVE = 64 * 1024 * 1024
MAX_BINARY = 128 * 1024 * 1024
ROOT = Path(__file__).resolve().parent


def extract_binary(payload, expected_digest, binary):
    if len(payload) > MAX_ARCHIVE or hashlib.sha256(payload).hexdigest() != expected_digest:
        raise ValueError('tool archive digest/size mismatch')
    with tarfile.open(fileobj=io.BytesIO(payload), mode='r:*') as archive:
        matches = []
        for member in archive:
            path = PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or member.issym() or member.islnk():
                raise ValueError('unsafe tool archive')
            if path.name == binary:
                if not member.isfile() or not 0 < member.size <= MAX_BINARY:
                    raise ValueError('invalid tool executable')
                matches.append(member)
        if len(matches) != 1:
            raise ValueError('ambiguous tool executable')
        stream = archive.extractfile(matches[0])
        data = stream.read(MAX_BINARY + 1)
        if len(data) != matches[0].size:
            raise ValueError('truncated tool executable')
        return data


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('tools', nargs='+')
    args = parser.parse_args()
    manifest = json.loads((ROOT / 'tools.json').read_text())
    key = platform.system() + '-' + platform.machine()
    args.out.mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in args.tools:
        item = manifest['tools'][name]['platforms'][key]
        request = urllib.request.Request(item['url'], headers={'User-Agent': 'rdy-core-ci'})
        with urllib.request.urlopen(request, timeout=60) as response:
            if not response.geturl().startswith('https://'):
                raise ValueError('non-HTTPS download')
            payload = response.read(MAX_ARCHIVE + 1)
        data = extract_binary(payload, item['sha256'], item['binary'])
        fd = os.open(args.out / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o500)
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        print(name + ' ' + manifest['tools'][name]['version'] + ': verified')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, tarfile.TarError):
        raise SystemExit('CI tool installation refused; no unverified executable was run')
