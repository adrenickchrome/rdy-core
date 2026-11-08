#!/usr/bin/env python3
"""Download checksum-pinned official CI binaries; extract only the named regular file."""
import argparse
import hashlib
import http.client
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import sys
import tarfile
import time
import urllib.error
import urllib.request

MAX_ARCHIVE = 64 * 1024 * 1024
MAX_BINARY = 128 * 1024 * 1024
ROOT = Path(__file__).resolve().parent
# Seconds to wait before each new download attempt. Only a network failure is tried again; a digest,
# size or archive failure is refused at once.
RETRY_DELAYS = (5, 20, 60)
TRANSIENT_HTTP = {408, 429, 500, 502, 503, 504}


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


def describe(error):
    """A short name for a download failure: the HTTP status, or the class of the network error."""
    if isinstance(error, urllib.error.HTTPError):
        return 'HTTP ' + str(error.code)
    if isinstance(error, urllib.error.URLError) and isinstance(error.reason, BaseException):
        return type(error.reason).__name__
    return type(error).__name__


def is_transient(error):
    if isinstance(error, urllib.error.HTTPError):
        return error.code in TRANSIENT_HTTP
    return isinstance(error, (OSError, http.client.HTTPException))


def download(url, urlopen=urllib.request.urlopen, sleep=time.sleep):
    """The response body, at most MAX_ARCHIVE + 1 bytes. It is not trusted until extract_binary checks it."""
    for delay in RETRY_DELAYS + (None,):
        try:
            request = urllib.request.Request(url, headers={'User-Agent': 'rdy-core-ci'})
            with urlopen(request, timeout=60) as response:
                if not response.geturl().startswith('https://'):
                    raise ValueError('non-HTTPS download')
                return response.read(MAX_ARCHIVE + 1)
        except (OSError, http.client.HTTPException) as error:
            if delay is None or not is_transient(error):
                raise
            print('download failed (' + describe(error) + '); trying again in ' + str(delay) + ' s',
                  file=sys.stderr, flush=True)
            sleep(delay)


def install(manifest, key, out, tools):
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in tools:
        try:
            item = manifest['tools'][name]['platforms'][key]
            try:
                payload = download(item['url'])
            except (OSError, http.client.HTTPException) as error:
                raise ValueError('download failed (' + describe(error) + ')') from error
            data = extract_binary(payload, item['sha256'], item['binary'])
            fd = os.open(out / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o500)
            with os.fdopen(fd, 'wb') as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
        except (OSError, ValueError, KeyError, tarfile.TarError) as error:
            reason = str(error) if isinstance(error, ValueError) else type(error).__name__
            raise SystemExit('CI tool installation refused (' + name + ': ' + reason + ')'
                             '; no unverified executable was run') from None
        print(name + ' ' + manifest['tools'][name]['version'] + ': verified', flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('tools', nargs='+')
    args = parser.parse_args()
    try:
        manifest = json.loads((ROOT / 'tools.json').read_text())
    except (OSError, ValueError):
        raise SystemExit('CI tool installation refused (tools.json unreadable); no unverified executable was run')
    install(manifest, platform.system() + '-' + platform.machine(), args.out, args.tools)


if __name__ == '__main__':
    main()
