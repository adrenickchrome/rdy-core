#!/bin/sh
# Standalone installer: the Python implementation is embedded, never fetched.
set -eu
if ! command -v python3 >/dev/null 2>&1; then
  printf '%s\n' 'rdy installer requires Python 3.9 or later. Install Python or use the manual archive instructions at https://github.com/adrenickchrome/rdy-core/releases.' >&2
  exit 2
fi
exec python3 - "$@" <<'RDY_INSTALLER_PYTHON'
import argparse
import contextlib
import gzip
import hashlib
import json
import os
import platform
import re
import shutil
import ssl
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

if os.name != "posix":
    sys.exit("rdy installer: Windows is not yet supported; use a qualified Linux or macOS host")
import fcntl
import resource

REPOSITORY = "adrenickchrome/rdy-core"
API = "https://api.github.com/repos/" + REPOSITORY
DOWNLOAD = "https://github.com/" + REPOSITORY + "/releases/download/"
SIGNER = 'rdy-release namespaces="rdy-release-v1" sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIDiiQ0vO3dvG2AtbJYDwsshJl75kvbbnRDOCUdsjfrDRAAAAD3NzaDpyZHktcmVsZWFzZQ=='
TARGETS = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
HOSTS = frozenset(("github.com", "api.github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"))
ARCHIVE_MAX = 269484032
BINARY_MAX = 268435456
EXPANDED_MAX = BINARY_MAX + 1048576
MANIFEST_MAX = 1024
SIGNATURE_MAX = 16384
METADATA_MAX = 2097152


class Refusal(Exception):
    pass


def version_value(value):
    if not isinstance(value, str) or not re.fullmatch(r"0\.[1-9][0-9]{0,9}\.0", value):
        raise Refusal("expected an exact preview version such as 0.17.0")
    return value


def archive_name(version, target):
    return "rdy-" + version_value(version) + "-preview-" + target + ".tar.gz"


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            h.update(block)
    return h.hexdigest()


def platform_target(system=None, machine=None):
    system = platform.system() if system is None else system
    machine = platform.machine() if machine is None else machine
    if system == "Darwin":
        if machine in ("arm64", "aarch64"):
            return TARGETS[0]
        if machine == "x86_64":
            try:
                translated = subprocess.run(["/usr/sbin/sysctl", "-n", "sysctl.proc_translated"], capture_output=True, timeout=5, check=False).stdout.strip()
            except (OSError, subprocess.TimeoutExpired):
                translated = b""
            if translated == b"1":
                raise Refusal("this shell runs under Rosetta; run the installer from a native Apple Silicon shell")
        raise Refusal("this release supports Apple Silicon macOS only; Intel macOS has no qualified archive")
    if system == "Linux" and machine in ("x86_64", "amd64"):
        try:
            libc = os.confstr("CS_GNU_LIBC_VERSION")
        except (ValueError, OSError):
            libc = None
        if libc and re.fullmatch(r"glibc [0-9]+\.[0-9]+", libc):
            return TARGETS[1]
        raise Refusal("this release needs GNU libc Linux; musl/Alpine is not qualified")
    raise Refusal("unsupported platform: initial archives are Apple Silicon macOS and x86_64 GNU/Linux; Windows is not yet supported")


def trusted_url(url):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.hostname not in HOSTS or parsed.port not in (None, 443)
            or parsed.username is not None or parsed.password is not None or parsed.fragment):
        raise Refusal("release download left the allowed HTTPS GitHub hosts")
    return url


class Redirects(urllib.request.HTTPRedirectHandler):
    max_redirections = 5
    max_repeats = 2

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        trusted_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(url, destination, limit):
    trusted_url(url)
    opener = urllib.request.build_opener(Redirects(), urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    accept = "application/vnd.github+json" if urllib.parse.urlsplit(url).hostname == "api.github.com" else "application/octet-stream"
    request = urllib.request.Request(url, headers={"User-Agent": "rdy-core-installer/1", "Accept": accept})
    start = time.monotonic()
    try:
        with opener.open(request, timeout=30) as response, open(destination, "xb") as out:
            trusted_url(response.url)
            if response.status != 200:
                raise Refusal("release download did not return HTTP 200")
            length = response.headers.get("Content-Length")
            if length is not None and (not length.isdigit() or int(length) > limit):
                raise Refusal("release download exceeds its size limit")
            count = 0
            while True:
                block = response.read(min(65536, limit + 1 - count))
                if not block:
                    break
                count += len(block)
                if count > limit or time.monotonic() - start > 180:
                    raise Refusal("release download exceeded its size or time limit")
                out.write(block)
            if count == 0 or (length is not None and count != int(length)):
                raise Refusal("release download was empty or incomplete")
            out.flush()
            os.fsync(out.fileno())
    except (urllib.error.URLError, OSError, ValueError) as exc:
        raise Refusal("release download failed; check network access and the exact release version") from exc


def read_regular(path, limit):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size < 1 or info.st_size > limit:
                raise Refusal("release input is not a bounded regular file")
            value = stream.read(limit + 1)
            if len(value) != info.st_size:
                raise Refusal("release input changed while being read")
            return value
    except OSError as exc:
        raise Refusal("release input is missing, unreadable or a symlink") from exc


def copy_regular(source, destination, limit):
    # Do not allocate the archive in memory or follow a local supplied symlink.
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as src, open(destination, "xb") as dst:
        info = os.fstat(src.fileno())
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= limit:
            raise Refusal("offline release input is not a bounded regular file")
        count = 0
        while True:
            block = src.read(65536)
            if not block:
                break
            count += len(block)
            if count > limit:
                raise Refusal("offline release input exceeds its size limit")
            dst.write(block)
        if count != info.st_size:
            raise Refusal("offline release input changed while being read")
        dst.flush()
        os.fsync(dst.fileno())


def metadata(url, scratch):
    file = scratch / "release.json"
    download(url, file, METADATA_MAX)
    try:
        return json.loads(read_regular(file, METADATA_MAX))
    except (ValueError, UnicodeError) as exc:
        raise Refusal("GitHub returned invalid release metadata") from exc


def resolve_release(requested, channel, scratch):
    if requested:
        version = version_value(requested)
        release = metadata(API + "/releases/tags/v" + version, scratch)
    else:
        if channel != "preview":
            raise Refusal("choose --version 0.N.0 or --channel preview")
        rows = metadata(API + "/releases?per_page=100", scratch)
        if not isinstance(rows, list):
            raise Refusal("invalid preview release list")
        candidates = [row for row in rows if isinstance(row, dict)
                      and row.get("prerelease") is True and row.get("draft") is False
                      and isinstance(row.get("tag_name"), str)
                      and re.fullmatch(r"v0\.[1-9][0-9]{0,9}\.0", row["tag_name"])]
        if not candidates:
            raise Refusal("no supported preview release is available; use an explicit version")
        release = max(candidates, key=lambda row: int(row["tag_name"].split(".")[1]))
        version = version_value(release["tag_name"][1:])
    if (not isinstance(release, dict) or release.get("tag_name") != "v" + version
            or release.get("draft") is not False or release.get("prerelease") is not True
            or type(release.get("id")) is not int):
        raise Refusal("release identity does not match the requested published preview")
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise Refusal("release asset list is missing")
    names = [a.get("name") for a in assets if isinstance(a, dict)]
    for name in ["SHA256SUMS", "SHA256SUMS.sig"] + [archive_name(version, target) for target in TARGETS]:
        if names.count(name) != 1:
            raise Refusal("release is incomplete or contains duplicate required assets")
    return version, release["id"]


def checksums(manifest, version):
    try:
        lines = manifest.decode("ascii").splitlines()
    except UnicodeError as exc:
        raise Refusal("checksum manifest is not ASCII") from exc
    expected = {archive_name(version, target) for target in TARGETS}
    result = {}
    if len(lines) != 2 or not manifest.endswith(b"\n") or b"\r" in manifest:
        raise Refusal("checksum manifest must contain exactly the two preview archives")
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] not in expected or match[2] in result:
            raise Refusal("checksum manifest contains an invalid, duplicate or unexpected entry")
        result[match[2]] = match[1]
    if set(result) != expected:
        raise Refusal("checksum manifest is incomplete")
    return result


def verify_release(scratch, version, target):
    tool = shutil.which("ssh-keygen")
    if tool is None:
        raise Refusal("verified install requires OpenSSH ssh-keygen with -Y verify; the manual archive path remains available")
    manifest = read_regular(scratch / "SHA256SUMS", MANIFEST_MAX)
    read_regular(scratch / "SHA256SUMS.sig", SIGNATURE_MAX)
    signers = scratch / "allowed-signers"
    signers.write_text(SIGNER + "\n", encoding="ascii")
    signers.chmod(0o600)
    try:
        result = subprocess.run([tool, "-Y", "verify", "-f", str(signers), "-I", "rdy-release", "-n", "rdy-release-v1", "-s", str(scratch / "SHA256SUMS.sig")],
                                input=manifest, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15,
                                env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C"})
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Refusal("release signature verification failed or OpenSSH lacks the required support") from exc
    if result.returncode != 0:
        raise Refusal("release signature did not verify with the pinned rdy release key")
    expected = checksums(manifest, version)[archive_name(version, target)]
    if digest(scratch / archive_name(version, target)) != expected:
        raise Refusal("release archive checksum mismatch")
    return expected


class BoundedReader:
    def __init__(self, stream, limit):
        self.stream, self.limit, self.count = stream, limit, 0

    def read(self, size=-1):
        size = min(self.limit + 1 - self.count, 65536 if size < 0 else size)
        value = self.stream.read(size)
        self.count += len(value)
        if self.count > self.limit:
            raise Refusal("expanded archive exceeds its size limit")
        return value


def unpack(archive, stage, version, target):
    stem = archive_name(version, target)[:-7]
    limits = {"rdy": BINARY_MAX, "LICENSE-MIT": 65536, "LICENSE-APACHE": 65536, "PREVIEW.txt": 65536}
    found = set()
    try:
        with open(archive, "rb") as raw, gzip.GzipFile(fileobj=raw) as zipped:
            bounded = BoundedReader(zipped, EXPANDED_MAX)
            with tarfile.open(fileobj=bounded, mode="r|") as tar:
                for member in tar:
                    name = member.name
                    if name in found or member.pax_headers or member.mode & 0o7000:
                        raise Refusal("archive contains duplicate or unsupported metadata")
                    found.add(name)
                    if name == stem and member.isdir():
                        continue
                    relative = name[len(stem) + 1:] if name.startswith(stem + "/") else None
                    if relative not in limits or not member.isreg() or not 0 < member.size <= limits[relative]:
                        raise Refusal("archive contains an unexpected path, type or file size")
                    source = tar.extractfile(member)
                    if source is None:
                        raise Refusal("archive member has no contents")
                    with source, open(stage / relative, "xb") as destination:
                        count = 0
                        while True:
                            block = source.read(65536)
                            if not block:
                                break
                            count += len(block)
                            if count > limits[relative]:
                                raise Refusal("archive member exceeds its size limit")
                            destination.write(block)
                        if count != member.size:
                            raise Refusal("archive member is truncated")
                        destination.flush()
                        os.fsync(destination.fileno())
                    (stage / relative).chmod(0o755 if relative == "rdy" else 0o644)
            # Consume trailer to check gzip CRC and bound hidden trailing payload.
            while bounded.read(65536):
                pass
    except (tarfile.TarError, OSError, EOFError) as exc:
        raise Refusal("release archive is invalid or incomplete") from exc
    required = {stem + "/" + name for name in limits}
    if not required <= found or found - required not in (set(), {stem}):
        raise Refusal("release archive lacks its exact binary/license/preview payload")


def safe_directory(path, create=False):
    path = Path(os.path.abspath(path))
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current = current / part
        try:
            info = current.lstat()
        except FileNotFoundError:
            if not create:
                raise Refusal("installation directory is absent")
            current.mkdir(mode=0o700)
            info = current.lstat()
        if not stat.S_ISDIR(info.st_mode):
            raise Refusal("installation directories must not be symlinks or files")
        if info.st_uid not in (0, os.getuid()):
            raise Refusal("installation directory has an unexpected owner")
        # A sticky system temporary ancestor is safe only because descendants are checked.
        if info.st_mode & 0o022 and not (info.st_uid == 0 and info.st_mode & stat.S_ISVTX):
            raise Refusal("installation directory is writable by another user or group")
    if path.stat().st_uid != os.getuid():
        raise Refusal("installation destination must belong to the current user")
    return path


@contextlib.contextmanager
def install_lock(root):
    path = root / ".install.lock"
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise Refusal("installer lock is not privately owned")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise Refusal("another installer is already running for this prefix") from exc
        yield
    finally:
        os.close(fd)


def stored_version(directory, version, target):
    safe_directory(directory)
    try:
        receipt = json.loads(read_regular(directory / "receipt.json", 4096))
    except (ValueError, UnicodeError) as exc:
        raise Refusal("installed version receipt is invalid") from exc
    if not isinstance(receipt, dict) or set(receipt) != {"format", "version", "target", "archive_sha256", "binary_sha256", "release_id"}:
        raise Refusal("installed version receipt has unexpected fields")
    if receipt["format"] != 1 or receipt["version"] != version or receipt["target"] != target:
        raise Refusal("installed version receipt identity mismatch")
    if not all(isinstance(receipt[k], str) and re.fullmatch(r"[0-9a-f]{64}", receipt[k]) for k in ("archive_sha256", "binary_sha256")):
        raise Refusal("installed version receipt digests are invalid")
    info = (directory / "rdy").lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refusal("installed executable is not safely owned")
    if digest(directory / "rdy") != receipt["binary_sha256"]:
        raise Refusal("installed executable changed since installation")
    return receipt


def check_command(command, versions):
    if not command.exists() and not command.is_symlink():
        return None
    if not command.is_symlink():
        raise Refusal("an unmanaged rdy already exists in this prefix; choose another --prefix")
    target = Path(os.path.abspath(command.parent / os.readlink(command)))
    if target.name != "rdy" or target.parent.parent != versions:
        raise Refusal("existing rdy link points outside this installer; choose another --prefix")
    match = re.fullmatch(r"(0\.[1-9][0-9]{0,9}\.0)--(aarch64-apple-darwin|x86_64-unknown-linux-gnu)", target.parent.name)
    if not match:
        raise Refusal("existing rdy link is not a recognized managed version")
    stored_version(target.parent, match[1], match[2])
    return os.readlink(command)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def activate(command, versions, directory, previous):
    if check_command(command, versions) != previous:
        raise Refusal("rdy command changed during installation; no activation performed")
    temporary = command.parent / (".rdy-link-" + os.urandom(12).hex())
    try:
        os.symlink(os.path.relpath(directory / "rdy", command.parent), temporary)
        os.replace(temporary, command)
        sync_directory(command.parent)
    finally:
        temporary.unlink(missing_ok=True)


def version_probe(binary, version, scratch):
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(scratch), "XDG_CONFIG_HOME": str(scratch / "config"), "LC_ALL": "C"}
    output = scratch / "version-output"
    try:
        with open(output, "xb") as out:
            result = subprocess.run([str(binary), "--version"], cwd=scratch, env=env, stdout=out, stderr=subprocess.DEVNULL, timeout=10,
                                    preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_FSIZE, (4096, 4096)))
        if result.returncode or read_regular(output, 1024) != ("rdy " + version + " (preview)\n").encode():
            raise Refusal("installed binary reports an unexpected version")
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Refusal("installed binary cannot run on this host; previous installation retained") from exc


def main(argv=None):
    parser = argparse.ArgumentParser(description="Install a signed rdy preview for this user. Requires Python3.9+ and OpenSSH signature verification. Projects and daemons are never changed.")
    choice = parser.add_mutually_exclusive_group(required=True)
    choice.add_argument("--version", help="exact preview, for example 0.17.0")
    choice.add_argument("--channel", choices=["preview"], help="resolve a current published preview once")
    choice.add_argument("--activate", metavar="VERSION", help="select a retained managed binary; does not roll back projects")
    choice.add_argument("--uninstall", action="store_true", help="remove the managed command link; retain versions and all project data")
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local", help="user-owned installation prefix (default: ~/.local)")
    parser.add_argument("--offline-dir", type=Path, help="local archive, SHA256SUMS and signature; requires --version and verifies the fixed release key")
    args = parser.parse_args(argv)
    if sys.version_info < (3, 9):
        raise Refusal("Python3.9 or later is required; use the manual archive path on older hosts")
    if os.getuid() == 0:
        raise Refusal("run this user-local installer without sudo or root")
    if args.offline_dir and not args.version:
        raise Refusal("--offline-dir requires an exact --version")
    if args.version:
        version_value(args.version)
    if args.activate:
        version_value(args.activate)
    target = platform_target()
    prefix = safe_directory(args.prefix.expanduser(), create=not (args.uninstall or args.activate))
    root = safe_directory(prefix / "share" / "rdy", create=not (args.uninstall or args.activate))
    versions = safe_directory(root / "versions", create=not (args.uninstall or args.activate))
    bindir = safe_directory(prefix / "bin", create=not (args.uninstall or args.activate))
    command = bindir / "rdy"
    with install_lock(root):
        previous = check_command(command, versions)
        if args.uninstall:
            if previous is not None:
                command.unlink()
                sync_directory(bindir)
            print("Removed managed command link. Retained versions at " + str(versions) + "; projects and configuration are unchanged.")
            return
        if args.activate:
            directory = versions / (args.activate + "--" + target)
            stored_version(directory, args.activate, target)
            activate(command, versions, directory, previous)
            print("Selected rdy " + args.activate + ". Project data was not rolled back.")
            return
        if previous is not None:
            print("Updating only the command. Before using an existing preview project, dump with its old binary, load with the new binary into a new directory, then run fsck. No daemon will be restarted.")
        with tempfile.TemporaryDirectory(prefix=".install-", dir=root) as scratch_name:
            scratch = Path(scratch_name)
            if args.offline_dir:
                version, release_id = args.version, None
            else:
                version, release_id = resolve_release(args.version, args.channel, scratch)
            print("Installing rdy " + version + " for " + target + " into " + str(prefix))
            for name, limit in (("SHA256SUMS", MANIFEST_MAX), ("SHA256SUMS.sig", SIGNATURE_MAX), (archive_name(version, target), ARCHIVE_MAX)):
                if args.offline_dir:
                    copy_regular(args.offline_dir / name, scratch / name, limit)
                else:
                    download(DOWNLOAD + "v" + version + "/" + name, scratch / name, limit)
            archive_digest = verify_release(scratch, version, target)
            directory = versions / (version + "--" + target)
            if directory.exists() or directory.is_symlink():
                receipt = stored_version(directory, version, target)
                if receipt["archive_sha256"] != archive_digest:
                    raise Refusal("published archive changed for an already installed version")
            else:
                stage = scratch / "version"
                stage.mkdir(mode=0o700)
                unpack(scratch / archive_name(version, target), stage, version, target)
                version_probe(stage / "rdy", version, scratch)
                receipt = {"format": 1, "version": version, "target": target, "archive_sha256": archive_digest, "binary_sha256": digest(stage / "rdy"), "release_id": release_id}
                with open(stage / "receipt.json", "x") as record:
                    json.dump(receipt, record, sort_keys=True)
                    record.write("\n")
                    record.flush()
                    os.fsync(record.fileno())
                (stage / "receipt.json").chmod(0o600)
                sync_directory(stage)
                os.rename(stage, directory)
                sync_directory(versions)
            activate(command, versions, directory, previous)
    print("Installed: " + str(command) + " (rdy " + version + ")")
    if str(bindir) not in os.environ.get("PATH", "").split(os.pathsep):
        print("Add this directory to your shell PATH: " + str(bindir))
    if previous is not None:
        print("Previous versions are retained. For preview projects use the old binary to dump, then the new binary to load into a new directory and run fsck. No project or daemon was changed.")


if __name__ == "__main__":
    try:
        main()
    except (Refusal, OSError, ValueError) as error:
        print("rdy installer: " + str(error), file=sys.stderr)
        sys.exit(1)
RDY_INSTALLER_PYTHON
