#!/usr/bin/env python3
"""Installs the one pinned AWS CLI v2 for the protected signer, in its own work directory.

The order is fixed: download at most the pinned size, check the official package digest, check every
member's name, type and size, extract with a running bound, and only then execute the bundled installer.
Nothing executes before the digest and member checks pass. There is no system install, and no fallback to
a CLI that is already on the machine: the only result is the binary this run installed and checked.
Python and the installer run with no credential in their environment.
"""
import argparse
from dataclasses import dataclass
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import stat
import subprocess
import sys
import time
import urllib.request
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402

ControllerError = cand.ControllerError
OFFICIAL_PREFIX = 'https://awscli.amazonaws.com/'
TOOL = 'aws-cli'
DOWNLOAD_DEADLINE = 600     # seconds for the whole package
READ_DEADLINE = 60          # seconds for one network read
INSTALL_DEADLINE = 300      # seconds for the bundled installer
CHUNK = 1024 * 1024
TOOL_KEYS = ('version', 'max_archive_bytes', 'max_expanded_bytes', 'max_entries', 'platforms')
PLATFORM_KEYS = ('url', 'sha256', 'size')
VERSION_RE = re.compile(r'[0-9]+\.[0-9]+\.[0-9]+')
MODE_MASK = 0o755


@dataclass(frozen=True)
class Tool(object):
    version: str
    url: str
    sha256: str
    size: int
    max_archive: int
    max_expanded: int
    max_entries: int


def load_tool(path, key):
    """The closed pin for one platform. A platform with no pin refuses: nothing else stands in for it."""
    manifest = cand.loads(Path(path).read_bytes())
    if not isinstance(manifest, dict) or manifest.get('schema') != 1 or not isinstance(manifest.get('tools'), dict):
        raise ControllerError('the tools manifest is malformed')
    value = cand.closed(manifest['tools'].get(TOOL), TOOL_KEYS, 'the AWS CLI pin')
    item = value['platforms'].get(key) if isinstance(value['platforms'], dict) else None
    if item is None:
        raise ControllerError('the AWS CLI has no pin for this platform')
    cand.closed(item, PLATFORM_KEYS, 'the AWS CLI pin')
    if not (isinstance(value['version'], str) and VERSION_RE.fullmatch(value['version'])
            and isinstance(item['sha256'], str) and cand.SHA256_RE.fullmatch(item['sha256'])
            and all(cand.is_int(n, 1) for n in (item['size'], value['max_archive_bytes'], value['max_expanded_bytes'],
                                                value['max_entries']))):
        raise ControllerError('the AWS CLI pin is malformed')
    return Tool(value['version'], item['url'], item['sha256'], item['size'], value['max_archive_bytes'],
                value['max_expanded_bytes'], value['max_entries'])


def _no_redirect():
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None
    return urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect).open


def download(tool, out, opener=None):
    """The official package to the new file `out`, at most its pinned size. A failure leaves no file."""
    if not tool.url.startswith(OFFICIAL_PREFIX):
        raise ControllerError('the AWS CLI pin is not an official package')
    request = urllib.request.Request(tool.url, headers={'User-Agent': 'rdy-core-release-signer'})
    limit = min(tool.size, tool.max_archive)
    out = Path(out)
    try:
        fd = os.open(str(out), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        raise ControllerError('the AWS CLI package file already exists')
    try:
        started = time.monotonic()
        with os.fdopen(fd, 'wb') as handle, (opener or _no_redirect())(request, timeout=READ_DEADLINE) as stream:
            total = 0
            while True:
                chunk = stream.read(CHUNK)
                if not chunk:
                    break
                total += len(chunk)
                if total > limit:
                    raise ControllerError('the AWS CLI package is larger than its pin')
                if time.monotonic() - started > DOWNLOAD_DEADLINE:
                    raise ControllerError('the AWS CLI download passed its deadline')
                handle.write(chunk)
    except BaseException as error:
        out.unlink()
        if isinstance(error, (OSError, http.client.HTTPException)):
            raise ControllerError('the AWS CLI download failed')
        raise
    return out


def verify_archive(tool, archive):
    """The size and the official package digest, streamed, before the ZIP is even opened."""
    try:
        size = os.lstat(str(archive))
    except OSError:
        raise ControllerError('the AWS CLI package is unreadable')
    if not stat.S_ISREG(size.st_mode) or size.st_size > tool.max_archive or size.st_size != tool.size:
        raise ControllerError('the AWS CLI package is not its pinned size')
    digest = hashlib.sha256()
    with open(str(archive), 'rb') as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b''):
            digest.update(chunk)
    if digest.hexdigest() != tool.sha256:
        raise ControllerError('the AWS CLI package does not match its pinned digest')


def check_members(tool, package):
    """Every member stays under aws/, is a regular file or a directory, and the total fits. Reads names and
    types only. Returns the members in order."""
    infos = package.infolist()
    if not infos or len(infos) > tool.max_entries:
        raise ControllerError('the AWS CLI package does not have an allowed number of entries')
    total, seen = 0, set()
    for info in infos:
        name = info.filename
        path = PurePosixPath(name)
        kind = stat.S_IFMT(info.external_attr >> 16)
        if (path.is_absolute() or '..' in path.parts or '\\' in name or '\0' in name or not path.parts
                or path.parts[0] != 'aws' or name in seen or info.flag_bits & 0x1):
            raise ControllerError('the AWS CLI package holds an unsafe path')
        seen.add(name)
        if kind not in (stat.S_IFREG, stat.S_IFDIR) or (kind == stat.S_IFDIR) != name.endswith('/'):
            raise ControllerError('the AWS CLI package holds a link or a special file')
        total += info.file_size
        if total > tool.max_expanded:
            raise ControllerError('the AWS CLI package expands past its limit')
    return infos


def extract(tool, package, infos, destination):
    """Streams each checked member, with a running total that a lying header cannot pass."""
    destination.mkdir(mode=0o700)
    total = 0
    for info in infos:
        target = destination.joinpath(*PurePosixPath(info.filename).parts)
        mode = stat.S_IMODE(info.external_attr >> 16) & MODE_MASK
        if info.filename.endswith('/'):
            target.mkdir(mode=0o755, parents=True, exist_ok=True)
            continue
        target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        fd = os.open(str(target), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'wb') as handle, package.open(info) as stream:
            while True:
                chunk = stream.read(CHUNK)
                if not chunk:
                    break
                total += len(chunk)
                if total > tool.max_expanded:
                    raise ControllerError('the AWS CLI package expands past its limit')
                handle.write(chunk)
        os.chmod(str(target), mode or 0o644)
    return total


def install(tool, archive, work):
    """Verifies, extracts and installs. Returns the installed binary, inside `work`."""
    verify_archive(tool, archive)
    try:
        package = zipfile.ZipFile(str(archive))
    except (zipfile.BadZipFile, OSError):
        raise ControllerError('the AWS CLI package is not a ZIP file')
    work = Path(work).resolve()
    with package:
        infos = check_members(tool, package)
        unpacked = work / 'unpacked'
        try:
            extract(tool, package, infos, unpacked)
        except (zipfile.BadZipFile, OSError):
            raise ControllerError('the AWS CLI package could not be extracted')
    installer, install_dir, bin_dir = unpacked / 'aws' / 'install', work / 'install', work / 'bin'
    # Every value is named: the job's tokens and any ambient AWS setting stay out of the installer.
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(work), 'LANG': 'C.UTF-8'}
    try:
        done = subprocess.run([str(installer), '--install-dir', str(install_dir), '--bin-dir', str(bin_dir)],
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
                              cwd=str(work), timeout=INSTALL_DEADLINE, check=False)
    except (OSError, subprocess.SubprocessError):
        raise ControllerError('the bundled AWS CLI installer failed')
    if done.returncode != 0:
        raise ControllerError('the bundled AWS CLI installer failed')
    return check_installed(tool, work, bin_dir / 'aws', env)


def check_installed(tool, work, binary, env):
    """The installed binary is under `work` and reports the pinned version. There is nothing to fall back on."""
    missing = ControllerError('the installed AWS CLI is not the pinned version')
    try:
        if work not in binary.resolve().parents:
            raise missing
        status, out, _ = cand.run_bounded([binary, '--version'], env=env, deadline=60)
    except (OSError, ControllerError):
        raise missing
    if status != 0 or not out.startswith(b'aws-cli/' + tool.version.encode() + b' '):
        raise missing
    return binary


def main(argv=None):
    parser = argparse.ArgumentParser(prog='install_aws.py', description='Install the pinned AWS CLI for the signer.')
    parser.add_argument('--tools', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        args.work.mkdir(mode=0o700)
        tool = load_tool(args.tools, platform.system() + '-' + platform.machine())
        archive = download(tool, args.work / 'awscli.zip')
        binary = install(tool, archive, args.work)
        archive.unlink()
    except ControllerError as error:
        sys.stderr.write('release controller: ' + str(error) + '\n')
        return 1
    except OSError:
        sys.stderr.write('release controller: a file operation failed\n')
        return 1
    sys.stdout.write(json.dumps({'aws_cli': tool.version, 'bin': str(binary.parent), 'sha256': tool.sha256},
                                sort_keys=True, separators=(',', ':')) + '\n')
    return 0


if __name__ == '__main__':
    sys.exit(main())
