#!/usr/bin/env python3
"""Build and assemble one immutable rdy candidate.

The candidate is the unsigned bytes of one release: one native archive per release target, the
frozen sample dump of the commit and a SHA256SUMS manifest over exactly those files (Release 1:
the macOS, Windows and Linux archives, then tally.rdydump). A preview candidate (macOS and Linux
archives, no sample) assembles through `assemble`, which the retained signer tools still read. It is built from `git archive S` in a new
workspace, packaged with the existing native tool, inspected for host paths, and run once to compare
its built-in rows with the guide bundle at the commit. Every upload is
addressed by run, run attempt, artifact id, artifact digest and target, and is downloaded and
rehashed before assembly. Nothing here signs, holds a key or writes to GitHub.

Subcommands (each failure is one `release controller: <reason>` line on stderr and exit 1;
invalid arguments exit 2):
  check-ref    the candidate ref, main ancestry, Cargo version and target policy at S
  gate-counts  the native fast gate's exact test counts from its report
  build        build, package and inspect one target in an isolated workspace
  assemble     verify the downloaded build artifacts and write SHA256SUMS and candidate.json
               (`--source` adds the frozen sample and makes a Release 1 candidate)
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import threading
import zipfile

REPOSITORY = 'adrenickchrome/rdy-core'
# The preview target set, which the retained signer tools still read.
TARGETS = ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu')
WINDOWS_TARGET = 'x86_64-pc-windows-msvc'
# The Release 1 target set, in manifest order (the sorted archive names).
RELEASE_TARGETS = ('aarch64-apple-darwin', WINDOWS_TARGET, 'x86_64-unknown-linux-gnu')
# The frozen sample dump: its name in the manifest, and its place and freeze record at S.
SAMPLE_NAME = 'tally.rdydump'
SAMPLE_SOURCE = 'samples/tally/tally.rdydump'
SAMPLE_FREEZE = 'samples/tally/freeze.json'
GUIDE_TOOL = Path(__file__).resolve().parent / 'guide.py'
KUNGFU_TABLE = 'crates/rdy-engine/kungfu-tools.json'
# The packaged checker's six checks, in order (`CHECK_NAMES` in rdy-qa's release_install.rs).
CHECK_NAMES = ('signature', 'checksums', 'install', 'version', 'daemon', 'mcp')

KIB = 1024
MIB = 1024 * KIB
RECORD_MAX = 64 * KIB          # a receipt, policy or tuple JSON
ARCHIVE_MAX = 512 * MIB        # one release archive
MANIFEST_MAX = 1024            # the adapter's manifest limit
SAMPLE_MAX = 16 * MIB          # the sample dump, checked before it is read
ZIP_OVERHEAD = 1 * MIB         # ZIP headers of the assembled candidate artifact
OUTPUT_MAX = 4 * MIB           # a child's output
REPORT_MAX = 16 * MIB          # the native fast gate's report.json
ASSET_MAX = 32                 # files in one artifact
HELPER_DEADLINE = 120          # seconds for one helper child
SCAN_CHUNK = 1 * MIB           # host-path scan window
SCAN_HITS_MAX = 1000
# The assembled candidate artifact: three archives, the sample, two small records (SHA256SUMS and
# candidate.json) and the ZIP overhead. 1,628,569,600 bytes.
ZIP_MAX = len(RELEASE_TARGETS) * ARCHIVE_MAX + SAMPLE_MAX + 2 * RECORD_MAX + ZIP_OVERHEAD

VERSION_RE = re.compile(r'0\.[1-9][0-9]*\.0')
CANDIDATE_REF_RE = re.compile(r'refs/tags/candidate/v(0\.[1-9][0-9]*\.0)')
COMMIT_RE = re.compile(r'[0-9a-f]{40}')
SHA256_RE = re.compile(r'[0-9a-f]{64}')
DIGEST_RE = re.compile(r'(?:sha256:)?([0-9a-f]{64})')
SOURCE_NAME_RE = re.compile(r'[A-Za-z0-9_][A-Za-z0-9_.+-]{0,127}\.crate')
# Home prefixes of every hosted runner and every local build machine.
HOST_PATTERNS = (b'/Users/', b'/home/')
HOSTED_HOMES = ('/Users/runner', '/home/runner')
# The Windows hosted runner's homes, and the prefixes that mark any Windows user path.
WINDOWS_HOSTED_HOMES = ('C:\\Users\\runneradmin', 'C:\\Users\\runner')
WINDOWS_HOST_PREFIXES = ('C:\\Users\\', 'C:/Users/', '\\Users\\')


class ControllerError(Exception):
    """A refusal; its text follows `release controller: ` on stderr."""


# ---------------------------------------------------------------------------------------------
# Closed JSON
# ---------------------------------------------------------------------------------------------

def canonical(value):
    """Sorted keys, compact separators, UTF-8 and one final LF. No float."""
    _no_float(value)
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False) + '\n').encode('utf-8')


def _no_float(value):
    if isinstance(value, float):
        raise ControllerError('evidence holds a float')
    if isinstance(value, dict):
        for item in value.values():
            _no_float(item)
    elif isinstance(value, list):
        for item in value:
            _no_float(item)


def _pairs(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ControllerError("duplicate field '" + key + "'")
        seen[key] = value
    return seen


def _float(_text):
    raise ControllerError('evidence holds a float')


def _constant(_text):
    raise ControllerError('evidence holds a non-finite number')


def loads(data, limit=RECORD_MAX, floats=False):
    """Parses JSON bytes of at most `limit` bytes; refuses duplicate keys, and floats unless allowed."""
    if len(data) > limit:
        raise ControllerError('evidence is larger than ' + str(limit) + ' bytes')
    try:
        decoded = data.decode('utf-8')
    except UnicodeDecodeError:
        raise ControllerError('evidence is not UTF-8')
    try:
        if floats:
            return json.loads(decoded, object_pairs_hook=_pairs, parse_constant=_constant)
        return json.loads(decoded, object_pairs_hook=_pairs, parse_float=_float, parse_constant=_constant)
    except ValueError:
        raise ControllerError('evidence is not JSON')


def closed(value, keys, where):
    """Checks that `value` is an object with exactly `keys`."""
    if not isinstance(value, dict):
        raise ControllerError(where + ' is not an object')
    for key in sorted(value):
        if key not in keys:
            message = "unknown field '" + key + "'"
            near = difflib.get_close_matches(key, sorted(keys), n=1, cutoff=0.6)
            if near:
                message += "; did you mean '" + near[0] + "'?"
            raise ControllerError(message)
    for key in sorted(keys):
        if key not in value:
            raise ControllerError("missing field '" + key + "'")
    return value


def is_int(value, minimum=0):
    """A JSON integer (never a boolean) of at least `minimum`."""
    return type(value) is int and value >= minimum


def text(value, name, limit=256):
    """A non-empty string of at most `limit` UTF-8 bytes."""
    if not isinstance(value, str):
        raise ControllerError('the ' + name + ' is not a string')
    if not value:
        raise ControllerError('the ' + name + ' is empty')
    if len(value.encode('utf-8')) > limit:
        raise ControllerError('the ' + name + ' is longer than ' + str(limit) + ' bytes')
    return value


def file_entry(value, where):
    """A `{name, sha256, size}` file entry (T's artifact shape)."""
    closed(value, ('name', 'sha256', 'size'), where)
    if not (isinstance(value['name'], str) and isinstance(value['sha256'], str)
            and SHA256_RE.fullmatch(value['sha256']) and is_int(value['size'], 1)):
        raise ControllerError(where + ' is malformed')
    return value


def archive_name(version, target):
    """The preview archive's file name. Release 1 uses `release_archive_name`."""
    return 'rdy-' + version + '-preview-' + target + '.tar.gz'


def archive_stem(version, target):
    """The Release 1 archive's folder (`archive_stem` in rdy-qa's release_install.rs)."""
    return 'rdy-' + version + '-' + target


def release_archive_name(version, target):
    """The Release 1 archive's file name (`archive_name` in rdy-qa's release_install.rs)."""
    return archive_stem(version, target) + '.tar.gz'


def binary_name(target):
    """The executable's name in the archive of `target`."""
    return 'rdy.exe' if target == WINDOWS_TARGET else 'rdy'


def release_files(version):
    """The four manifest names of Release 1, in manifest order."""
    return [release_archive_name(version, t) for t in RELEASE_TARGETS] + [SAMPLE_NAME]


def manifest_bytes(files):
    """SHA256SUMS over `files` ({name, sha256}), sorted by file name."""
    data = ''.join(f['sha256'] + '  ' + f['name'] + '\n' for f in sorted(files, key=lambda f: f['name'])).encode()
    if len(data) > MANIFEST_MAX:
        raise ControllerError('the manifest is larger than ' + str(MANIFEST_MAX) + ' bytes')
    return data


def sha256_file(path, limit):
    """The SHA-256 and size of a regular file of at most `limit` bytes, streamed."""
    digest, size = hashlib.sha256(), 0
    with open(str(path), 'rb') as handle:
        while True:
            chunk = handle.read(MIB)
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                raise ControllerError(Path(path).name + ' is larger than ' + str(limit) + ' bytes')
            digest.update(chunk)
    return digest.hexdigest(), size


def write_new(path, data, mode=0o644):
    """Writes `data` to a new file; never replaces one."""
    try:
        fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    except FileExistsError:
        raise ControllerError('the file ' + Path(path).name + ' already exists')
    with os.fdopen(fd, 'wb') as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


# ---------------------------------------------------------------------------------------------
# Bounded children
# ---------------------------------------------------------------------------------------------

def run_bounded(argv, cap=OUTPUT_MAX, deadline=HELPER_DEADLINE, stdin=None, env=None, cwd=None):
    """Runs `argv`; returns (status, stdout, stderr), each stream at most `cap` bytes.

    A child that writes more, or outlives `deadline`, is killed and refused. An exception in the
    caller (a cancellation) kills the child too."""
    try:
        child = subprocess.Popen([str(a) for a in argv], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, env=env, cwd=None if cwd is None else str(cwd))
    except OSError:
        raise ControllerError('a helper could not start')
    out, err, over = [], [], []

    def read(stream, sink):
        total = 0
        while True:
            chunk = stream.read(64 * KIB)
            if not chunk:
                return
            total += len(chunk)
            if total > cap:
                over.append(True)
                child.kill()
                return
            sink.append(chunk)

    readers = [threading.Thread(target=read, args=(child.stdout, out)),
               threading.Thread(target=read, args=(child.stderr, err))]
    try:
        for reader in readers:
            reader.daemon = True
            reader.start()
        try:
            if stdin is not None:
                child.stdin.write(stdin)
            child.stdin.close()
        except BrokenPipeError:
            pass
        try:
            status = child.wait(timeout=deadline)
        except subprocess.TimeoutExpired:
            raise ControllerError('a helper passed its ' + str(deadline) + '-second deadline')
        for reader in readers:
            reader.join()
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        for stream in (child.stdout, child.stderr):
            stream.close()
    if over:
        raise ControllerError('a helper wrote more than ' + str(cap) + ' bytes')
    return status, b''.join(out), b''.join(err)


def git(repo, *args, cap=OUTPUT_MAX):
    """(status, stdout) of one git command in `repo`."""
    status, out, _ = run_bounded(['git', '-C', repo] + list(args), cap=cap)
    return status, out


# ---------------------------------------------------------------------------------------------
# check-ref
# ---------------------------------------------------------------------------------------------

def cargo_version(toml):
    """The one `[workspace.package]` version of a Cargo.toml text, or None."""
    section, found = None, None
    for line in toml.splitlines():
        stripped = line.strip()
        if stripped.startswith('['):
            section = stripped
            continue
        if section == '[workspace.package]':
            match = re.fullmatch(r'version\s*=\s*"([^"]*)"', stripped)
            if match:
                if found is not None:
                    return None
                found = match.group(1)
    return found


def check_ref(repo, ref, sha, main_ref):
    """Checks that `ref` is `refs/tags/candidate/v<V>` and resolves directly to commit `sha`, that the
    commit is on `main_ref`'s first-parent chain with Cargo version V and the release target
    policy, and that V is neither reserved nor released. Returns the candidate facts."""
    match = CANDIDATE_REF_RE.fullmatch(ref) if isinstance(ref, str) else None
    if not match:
        raise ControllerError('the ref is not refs/tags/candidate/v<V>')
    version = match.group(1)
    if not (isinstance(sha, str) and COMMIT_RE.fullmatch(sha)):
        raise ControllerError('the commit is not a full 40-hex SHA')
    status, out = git(repo, 'show-ref', '--verify', ref)
    if status != 0 or out.decode('utf-8', 'replace').splitlines() != [sha + ' ' + ref]:
        raise ControllerError('the candidate ref does not resolve directly to the commit')
    status, out = git(repo, 'cat-file', '-t', sha)
    if status != 0 or out != b'commit\n':
        raise ControllerError('the candidate ref does not resolve directly to the commit')
    status, out = git(repo, 'rev-list', '--first-parent', main_ref, '--')
    if status != 0:
        raise ControllerError('main is not available')
    if sha not in out.decode('ascii', 'replace').split('\n'):
        raise ControllerError("the commit is not on main's first-parent chain")
    status, out = git(repo, 'show', sha + ':Cargo.toml', cap=RECORD_MAX)
    found = cargo_version(out.decode('utf-8', 'replace')) if status == 0 else None
    if found is None or not VERSION_RE.fullmatch(found):
        raise ControllerError('the Cargo version at the commit is unreadable')
    if found != version:
        raise ControllerError('Cargo version ' + found + ' at the commit is not ' + version)
    status, _ = git(repo, 'cat-file', '-e', sha + ':release-keys/policy.json')
    if status == 0:
        status, out = git(repo, 'show', sha + ':release-keys/policy.json', cap=RECORD_MAX)
        policy = loads(out) if status == 0 else None
        if not (isinstance(policy, dict) and policy.get('targets') in (list(TARGETS), list(RELEASE_TARGETS))):
            raise ControllerError('the target policy at the commit is not exactly ' + ' and '.join(TARGETS)
                                  + ' or ' + ', '.join(RELEASE_TARGETS))
    for taken in ('refs/tags/reserve/v' + version, 'refs/tags/v' + version):
        status, _ = git(repo, 'show-ref', '--verify', '--quiet', taken)
        if status == 0:
            raise ControllerError('v' + version + ' is already reserved or released; a retry reuses its '
                                  'reserved candidate and never rebuilds')
    return {'ref': ref, 'version': version, 'commit': sha, 'targets': list(RELEASE_TARGETS)}


# ---------------------------------------------------------------------------------------------
# gate-counts
# ---------------------------------------------------------------------------------------------

COUNT_RE = re.compile(r'([0-9]+) passed, ([0-9]+) failed, ([0-9]+) ignored, ([0-9]+) skipped')


def fast_gate_counts(report, commit):
    """The exact counts of a native `cargo qa --skip-perf --require-all` report.json on `commit`:
    every test gate and every area summed. Refuses a failed, dirty, other-commit or empty run, and
    any failed or prerequisite-skipped test."""
    value = loads(report, limit=REPORT_MAX, floats=True)
    malformed = ControllerError('the native fast gate report is malformed')
    if not isinstance(value, dict):
        raise malformed
    if value.get('dirty') is not False:
        raise ControllerError('the native fast gate ran on a changed tree')
    revision = value.get('revision')
    if not (isinstance(revision, str) and re.fullmatch(r'[0-9a-f]{7,40}', revision) and commit.startswith(revision)):
        raise ControllerError('the native fast gate ran on another commit')
    if value.get('pass') is not True:
        raise ControllerError('the native fast gate did not pass')
    gates, areas = value.get('gates'), value.get('areas')
    if not (isinstance(gates, list) and isinstance(areas, list)):
        raise malformed
    totals = [0, 0, 0, 0]
    for gate in gates:
        detail = gate.get('detail') if isinstance(gate, dict) else None
        if not isinstance(detail, str):
            raise malformed
        first = detail.split('\n', 1)[0]
        if not first[:1].isdigit():
            continue
        match = COUNT_RE.fullmatch(first.split(' (', 1)[0])
        if not match:
            raise malformed
        totals = [t + int(g) for t, g in zip(totals, match.groups())]
    for area in areas:
        keys = ('passed', 'failed', 'ignored', 'skipped')
        if not (isinstance(area, dict) and all(is_int(area.get(k)) for k in keys)):
            raise malformed
        totals = [t + area[k] for t, k in zip(totals, keys)]
    passed, failed, ignored, skipped = totals
    if failed or skipped:
        raise ControllerError('the native fast gate did not pass with every prerequisite')
    if passed == 0:
        raise ControllerError('the native fast gate ran no test')
    return {'passed': passed, 'failed': 0, 'ignored': ignored, 'prerequisite_skipped': 0}


def check_fast_gate(value, target):
    """E's `fast_gate` counts: passed above 0, failed 0, prerequisite_skipped 0."""
    closed(value, ('passed', 'failed', 'ignored', 'prerequisite_skipped'), 'the ' + target + ' fast gate')
    if not (is_int(value['passed'], 1) and is_int(value['failed']) and value['failed'] == 0
            and is_int(value['ignored']) and is_int(value['prerequisite_skipped'])
            and value['prerequisite_skipped'] == 0):
        raise ControllerError('the ' + target + ' fast gate did not pass with every prerequisite')
    return value


# ---------------------------------------------------------------------------------------------
# build
# ---------------------------------------------------------------------------------------------

WINDOWS_PATH_RE = re.compile(r'[A-Za-z]:[\\/][^\s=\x00-\x1f]+')


def remap_flags(home, cargo_home, rustup_home, source, target_dir, windows=False):
    """rustc path remaps for a release build. The hosted runner homes come first; rustc applies the
    last matching remap, so each more specific prefix follows the home it lies in. A Windows build
    takes Windows paths (a drive and a backslash or slash, then named components) and the hosted
    Windows homes."""
    hosted = WINDOWS_HOSTED_HOMES if windows else HOSTED_HOMES
    pairs = [(hosted[0], '/build'), (hosted[1], '/build'), (home, '/build'),
             (cargo_home, '/cargo'), (rustup_home, '/rustup'), (source, '/rdy'), (target_dir, '/target')]
    flags = []
    for prefix, to in pairs:
        if windows:
            plain = isinstance(prefix, str) and WINDOWS_PATH_RE.fullmatch(prefix) is not None
        else:
            plain = (isinstance(prefix, str) and prefix.startswith('/') and len(prefix) > 1
                     and not re.search(r'[\s=\x00-\x1f]', prefix))
        if not plain:
            raise ControllerError('a build path is not a plain absolute path')
        flag = '--remap-path-prefix=' + prefix.rstrip('\\/') + '=' + to
        if flag not in flags:
            flags.append(flag)
    return flags


def host_patterns(target, extra=()):
    """The byte patterns that mark a host path in a binary: every Unix home prefix and each `extra`
    path; for the Windows target also the Windows user prefixes, and each pattern with its slashes
    both ways, in UTF-8 and in UTF-16 (little endian), which is how a Windows binary holds a path."""
    texts = [p.decode() for p in HOST_PATTERNS] + [p for p in extra if p]
    if target == WINDOWS_TARGET:
        texts += list(WINDOWS_HOST_PREFIXES)
    spelled = []
    for text in texts:
        forms = [text]
        if target == WINDOWS_TARGET:
            forms += [text.replace('\\', '/'), text.replace('/', '\\')]
        for form in forms:
            for encoded in ((form.encode(), form.encode('utf-16-le')) if target == WINDOWS_TARGET
                            else (form.encode(),)):
                if encoded not in spelled:
                    spelled.append(encoded)
    return spelled


def inspect_archive(path, version, target, extra=()):
    """Checks that `path` is one packaged release (every member under its plain stem, no link, one
    regular `rdy`, or `rdy.exe` for Windows), then scans that binary for host paths: any `/Users/` or
    `/home/` and each prefix in `extra`, and for Windows the user prefixes, each in UTF-8 and UTF-16.
    Returns 0, or refuses with the number of host paths found."""
    stem = archive_stem(version, target)
    bad = ControllerError('the ' + target + ' archive is not one packaged release')
    patterns = host_patterns(target, extra)
    longest = max(len(p) for p in patterns)
    hits = set()
    try:
        with tarfile.open(str(path), mode='r:gz') as archive:
            binary = None
            for member in archive:
                parts = member.name.split('/')
                if (member.name.startswith('/') or '..' in parts or parts[0] != stem
                        or not (member.isfile() or member.isdir())):
                    raise bad
                if member.name != stem + '/' + binary_name(target):
                    continue
                if binary is not None or not member.isfile() or not 0 < member.size <= ARCHIVE_MAX:
                    raise bad
                binary = member
                stream = archive.extractfile(member)
                tail, base = b'', 0
                while len(hits) < SCAN_HITS_MAX:
                    chunk = stream.read(SCAN_CHUNK)
                    if not chunk:
                        break
                    window, start = tail + chunk, base - len(tail)
                    for pattern in patterns:
                        at = window.find(pattern)
                        while at >= 0:
                            hits.add((start + at, start + at + len(pattern)))
                            at = window.find(pattern, at + 1)
                    base += len(chunk)
                    tail = window[-(longest - 1):]
            if binary is None:
                raise bad
    except (tarfile.TarError, OSError, EOFError):
        raise bad
    merged = []
    for first, last in sorted(hits):
        # one path found by two patterns (a drive prefix and its tail) is one host path
        if merged and first < merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], last)
        else:
            merged.append([first, last])
    if merged:
        count = len(merged)
        raise ControllerError('the ' + target + ' binary holds ' + str(count) + ' host path'
                              + ('' if count == 1 else 's'))
    return 0


def check_packaged_guide(path, version, target, source, work, env):
    """Runs the `rdy` of the inspected archive `path` and compares its built-in rows and menu with
    the guide bundle and tool table of the source archive. Refuses with the checker's own line."""
    folder = Path(work) / 'guide-check'
    folder.mkdir(mode=0o700)
    binary = folder / binary_name(target)
    bad = ControllerError('the ' + target + ' archive is not one packaged release')
    try:
        with tarfile.open(str(path), mode='r:gz') as archive:
            stream = archive.extractfile(archive_stem(version, target) + '/' + binary_name(target))
            fd = os.open(str(binary), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
            with os.fdopen(fd, 'wb') as handle:
                shutil.copyfileobj(stream, handle, MIB)
    except (tarfile.TarError, OSError, EOFError, KeyError, AttributeError):
        raise bad
    release = Path(source) / 'release'
    status, _, said = run_bounded([sys.executable, '-I', '-B', GUIDE_TOOL, 'check-binary', '--binary', binary,
                                   '--bundle', release / 'agent-guide.json', '--sidecar', release / 'agent-guide.sha256',
                                   '--table', Path(source) / KUNGFU_TABLE],
                                  env=env)
    shutil.rmtree(str(folder), ignore_errors=True)
    if status != 0:
        lines = said.decode('utf-8', 'replace').splitlines()
        raise ControllerError(lines[0] if lines else 'the guide check of the ' + target + ' binary failed')


def build_receipt(version, commit, target, run_id, run_attempt, archive_name, archive_bytes=None,
                  fast_gate=None, sources=(), archive_sha256=None, archive_size=None):
    """The closed build receipt of one target. It keeps S, the commit the archive was built from."""
    if archive_bytes is not None:
        archive_sha256, archive_size = hashlib.sha256(archive_bytes).hexdigest(), len(archive_bytes)
    return {'schema': 1, 'repository': REPOSITORY, 'version': version, 'commit': commit, 'target': target,
            'run_id': run_id, 'run_attempt': run_attempt,
            'archive': {'name': archive_name, 'sha256': archive_sha256, 'size': archive_size},
            'fast_gate': fast_gate, 'host_paths': 0, 'sources': list(sources)}


def check_arguments(version, commit, target, run_id, run_attempt):
    """The grammar of a build's identity."""
    if not (isinstance(version, str) and VERSION_RE.fullmatch(version)):
        raise ControllerError('the version is not a 0.<minor>.0 preview')
    if not (isinstance(commit, str) and COMMIT_RE.fullmatch(commit)):
        raise ControllerError('the commit is not a full 40-hex SHA')
    if target is not None and target not in RELEASE_TARGETS:
        raise ControllerError('the target is not a release target')
    if not (is_int(run_id, 1) and is_int(run_attempt, 1)):
        raise ControllerError('the run id and attempt are not positive integers')


def build_step(argv, cwd, env):
    """Runs one long build step with its output in the job log; the job's timeout bounds it."""
    try:
        status = subprocess.call([str(a) for a in argv], cwd=str(cwd), env=env)
    except OSError:
        status = -1
    if status != 0:
        raise ControllerError(' '.join(str(a) for a in argv[:2]) + ' failed')


def native_join(target, base, *names):
    """`base` and `names` joined with the separator of `target`'s system."""
    separator = '\\' if target == WINDOWS_TARGET else '/'
    return separator.join([base.rstrip('\\/')] + list(names))


def tar_program(target):
    """The system's own tar: on Windows the one in System32, never one that PATH finds first."""
    if target == WINDOWS_TARGET and os.name == 'nt':
        return os.path.join(os.environ.get('SystemRoot', 'C:\\Windows'), 'System32', 'tar.exe')
    return 'tar'


def build(repo, commit, version, target, work, out, fast_gate_path, run_id, run_attempt, env=None):
    """Builds `target` from `git archive <commit>` in the new workspace `work`, stages the MPL source
    crates with qa-release-sources (which checks release/notes.md first), packages with
    qa-release-package, inspects the packaged binary, and writes the archive and its build receipt
    to the new directory `out`. The Windows target builds natively: its helpers are `.exe` files, its
    homes are Windows paths, and the packaged binary is scanned for Windows paths too."""
    env = dict(os.environ if env is None else env)
    check_arguments(version, commit, target, run_id, run_attempt)
    windows = target == WINDOWS_TARGET
    fast_gate = check_fast_gate(loads(Path(fast_gate_path).read_bytes()[:RECORD_MAX + 1]), target)
    work, out = Path(work), Path(out)
    if not (work.is_absolute() and out.is_absolute()):
        raise ControllerError('a build path is not a plain absolute path')
    if out.exists() or work.exists():
        raise ControllerError('the output directory already exists')
    work.mkdir(mode=0o700, parents=True)
    source, target_dir = work / 'src', work / 'target'
    source.mkdir(mode=0o700)
    status, _, _ = run_bounded(['git', '-C', repo, 'archive', '--format=tar', '-o', work / 'source.tar', commit])
    if status != 0:
        raise ControllerError('git archive of the commit failed')
    status, _, _ = run_bounded([tar_program(target), '-x', '-f', work / 'source.tar', '-C', source])
    if status != 0:
        raise ControllerError('the source archive did not extract')
    (work / 'source.tar').unlink()
    notes = source / 'release' / 'notes.md'
    if notes.is_symlink() or not notes.is_file():
        raise ControllerError('release/notes.md is missing at the commit')

    home = env.get('USERPROFILE', '') if windows else env.get('HOME', '')
    cargo_home = env.get('CARGO_HOME') or native_join(target, home, '.cargo')
    rustup_home = env.get('RUSTUP_HOME') or native_join(target, home, '.rustup')
    flags = remap_flags(home, cargo_home, rustup_home, str(source), str(target_dir), windows=windows)
    env.pop('RUSTFLAGS', None)
    env.update({'CARGO_ENCODED_RUSTFLAGS': '\x1f'.join(flags), 'CARGO_TARGET_DIR': str(target_dir),
                'CARGO_INCREMENTAL': '0'})
    build_step(['cargo', 'fetch', '--locked'], source, env)
    env['CARGO_NET_OFFLINE'] = 'true'
    build_step(['cargo', 'build', '--release', '--locked', '-p', 'rdy-cli', '-p', 'rdy-qa'], source, env)

    release = target_dir / 'release'
    exe = binary_name(target)
    helper = (lambda name: name + '.exe') if windows else (lambda name: name)
    status, said, _ = run_bounded([release / exe, '--version'], env=env)
    if status != 0 or said != ('rdy ' + version + '\n').encode():
        raise ControllerError('the built rdy does not print rdy ' + version)
    status, _, _ = run_bounded([release / helper('qa-release-sources'), '--version', version, '--out',
                                work / 'sources', '--check-notes', notes], env=env)
    if status != 0:
        raise ControllerError('qa-release-sources refused the release sources or notes')
    entries = sorted((work / 'sources').iterdir())
    if not 0 < len(entries) <= ASSET_MAX:
        raise ControllerError('qa-release-sources staged no source crate')
    sources = []
    for entry in entries:
        if entry.is_symlink() or not entry.is_file() or not SOURCE_NAME_RE.fullmatch(entry.name):
            raise ControllerError('qa-release-sources staged an unexpected file')
        digest, size = sha256_file(entry, ARCHIVE_MAX)
        sources.append({'name': entry.name, 'sha256': digest, 'size': size})
    name = release_archive_name(version, target)
    status, said, _ = run_bounded([release / helper('qa-release-package'), '--binary', release / exe,
                                   '--version', version, '--target', target, '--out', work / 'package'], env=env)
    if status != 0 or said != (str(work / 'package' / name) + '\n').encode():
        raise ControllerError('qa-release-package did not package the ' + target + ' archive')
    packaged = work / 'package' / name
    inspect_archive(packaged, version, target, extra=sorted({home, str(work), cargo_home, rustup_home}))
    check_packaged_guide(packaged, version, target, source, work, env)
    digest, size = sha256_file(packaged, ARCHIVE_MAX)
    out.mkdir(mode=0o700, parents=True)
    shutil.copyfile(str(packaged), str(out / name))
    if sha256_file(out / name, ARCHIVE_MAX) != (digest, size):
        raise ControllerError('the ' + target + ' archive changed while it was copied')
    receipt = build_receipt(version, commit, target, run_id, run_attempt, name, fast_gate=fast_gate,
                            sources=sources, archive_sha256=digest, archive_size=size)
    write_new(out / ('build-' + target + '.json'), canonical(receipt))
    return receipt


# ---------------------------------------------------------------------------------------------
# assemble
# ---------------------------------------------------------------------------------------------

def artifact_name(target, run_attempt):
    """The Actions artifact name of one target's build in one run attempt."""
    return 'rdy-candidate-' + target + '-a' + str(run_attempt)


def candidate_artifact_name(run_attempt):
    """The Actions artifact name of the assembled candidate in one run attempt."""
    return 'rdy-candidate-a' + str(run_attempt)


def digest_value(digest, where):
    """The 64 hex of an artifact-service digest, `sha256:<hex>` or bare hex."""
    match = DIGEST_RE.fullmatch(digest) if isinstance(digest, str) else None
    if not match:
        raise ControllerError('the ' + where + ' artifact digest is not sha256:<64 hex>')
    return match.group(1)


def downloaded_zip(folder, where):
    """The one regular artifact file in a download directory."""
    folder = Path(folder)
    entries = sorted(folder.iterdir()) if folder.is_dir() and not folder.is_symlink() else []
    if len(entries) != 1 or entries[0].is_symlink() or not entries[0].is_file():
        raise ControllerError('the ' + where + ' download is not exactly one artifact file')
    return entries[0]


def open_artifact(path, digest, members, where, what, limit):
    """Checks an Actions artifact ZIP against its artifact-service digest, then that it holds exactly
    the regular files `members` (name -> size limit; None checks no member yet). Returns (artifact
    digest, size, ZipFile)."""
    expected = digest_value(digest, where)
    actual, size = sha256_file(path, limit)
    if actual != expected:
        raise ControllerError('the ' + where + ' artifact does not match its artifact digest')
    try:
        archive = zipfile.ZipFile(str(path))
        archive.infolist()
    except (zipfile.BadZipFile, OSError):
        raise ControllerError('the ' + where + ' artifact is not a ZIP file')
    if members is not None and not exact_members(archive, members):
        archive.close()
        raise ControllerError('the ' + where + ' artifact is not exactly ' + what)
    return 'sha256:' + actual, size, archive


def exact_members(archive, members):
    """Whether a ZIP holds exactly the regular files `members` (name -> size limit), each once."""
    infos = archive.infolist()
    names = [i.filename for i in infos]
    regular = all(not i.is_dir() and (i.external_attr >> 16) & 0o170000 in (0, 0o100000) for i in infos)
    return (sorted(names) == sorted(members) and len(set(names)) == len(names) and regular
            and all(i.file_size <= members[i.filename] for i in infos))


def read_member(archive, name, limit):
    """One ZIP member's bytes, at most `limit`."""
    with archive.open(name) as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ControllerError(name + ' is larger than ' + str(limit) + ' bytes')
    return data


def extract_member(archive, name, dest, limit, mode=0o644):
    """Streams one ZIP member to a new file; returns its SHA-256 and size."""
    digest, size = hashlib.sha256(), 0
    fd = os.open(str(dest), os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(fd, 'wb') as handle, archive.open(name) as stream:
        while True:
            chunk = stream.read(MIB)
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                raise ControllerError(name + ' is larger than ' + str(limit) + ' bytes')
            digest.update(chunk)
            handle.write(chunk)
        handle.flush()
        os.fsync(handle.fileno())
    return digest.hexdigest(), size


def check_build_receipt(receipt, target, version, commit, run_id, run_attempt, name_of=None):
    """A downloaded build receipt names exactly this run, attempt, commit, version and target."""
    keys = ('schema', 'repository', 'version', 'commit', 'target', 'run_id', 'run_attempt', 'archive',
            'fast_gate', 'host_paths', 'sources')
    closed(receipt, keys, 'the ' + target + ' build receipt')
    want = {'schema': 1, 'repository': REPOSITORY, 'version': version, 'commit': commit, 'target': target,
            'run_id': run_id, 'run_attempt': run_attempt, 'host_paths': 0}
    for key in ('schema', 'repository', 'version', 'commit', 'target', 'run_id', 'run_attempt', 'host_paths'):
        if receipt[key] != want[key] or type(receipt[key]) is not type(want[key]):
            raise ControllerError('the ' + target + ' build receipt does not match ' + key)
    file_entry(receipt['archive'], 'the ' + target + ' build receipt archive')
    if receipt['archive']['name'] != (name_of or archive_name)(version, target):
        raise ControllerError('the ' + target + ' archive does not match its build receipt')
    check_fast_gate(receipt['fast_gate'], target)
    sources = receipt['sources']
    malformed = ControllerError('the ' + target + ' build receipt sources are malformed')
    if not (isinstance(sources, list) and 0 < len(sources) <= ASSET_MAX):
        raise malformed
    for entry in sources:
        file_entry(entry, 'the ' + target + ' build receipt source')
        if not SOURCE_NAME_RE.fullmatch(entry['name']):
            raise malformed
    if [e['name'] for e in sources] != sorted({e['name'] for e in sources}):
        raise malformed
    return receipt


def targets_text(targets):
    """`a and b`, or `a, b and c`."""
    return ', '.join(targets[:-1]) + ' and ' + targets[-1] if len(targets) > 2 else ' and '.join(targets)


def read_sample(source, commit):
    """The frozen sample dump of the source tree `source` at `commit` and its record. The sample is
    a regular file (never a link) of at most SAMPLE_MAX bytes, checked before it is read, and it
    matches the SHA-256 and size of its freeze record. Returns (bytes, sample record)."""
    root = Path(source)
    missing = ControllerError('the frozen sample ' + SAMPLE_SOURCE + ' is missing at the commit')
    path = root / SAMPLE_SOURCE
    try:
        info = os.lstat(str(path))
    except OSError:
        raise missing
    if not stat.S_ISREG(info.st_mode):
        raise missing
    if info.st_size > SAMPLE_MAX:
        raise ControllerError('the frozen sample is larger than ' + str(SAMPLE_MAX) + ' bytes')
    freeze_path = root / SAMPLE_FREEZE
    try:
        freeze_info = os.lstat(str(freeze_path))
    except OSError:
        freeze_info = None
    if freeze_info is None or not stat.S_ISREG(freeze_info.st_mode):
        raise ControllerError('the sample freeze record ' + SAMPLE_FREEZE + ' is missing at the commit')
    if freeze_info.st_size > RECORD_MAX:
        raise ControllerError('the sample freeze record is larger than ' + str(RECORD_MAX) + ' bytes')
    freeze_raw = freeze_path.read_bytes()
    freeze = loads(freeze_raw)
    if not (isinstance(freeze, dict) and isinstance(freeze.get('sha256'), str)
            and SHA256_RE.fullmatch(freeze['sha256']) and is_int(freeze.get('size'), 1)):
        raise ControllerError('the sample freeze record is malformed')
    with open(str(path), 'rb') as handle:
        data = handle.read(SAMPLE_MAX + 1)
    if len(data) > SAMPLE_MAX:
        raise ControllerError('the frozen sample is larger than ' + str(SAMPLE_MAX) + ' bytes')
    digest = hashlib.sha256(data).hexdigest()
    if (digest, len(data)) != (freeze['sha256'], freeze['size']):
        raise ControllerError('the sample does not match its freeze record')
    return data, {'name': SAMPLE_NAME, 'source': SAMPLE_SOURCE, 'commit': commit, 'sha256': digest,
                  'size': len(data), 'freeze_sha256': hashlib.sha256(freeze_raw).hexdigest()}


def assemble(inputs, version, commit, run_id, run_attempt, out, source=None):
    """Verifies each target's downloaded build artifact (`inputs`: target -> (download directory,
    artifact id, artifact digest)) and writes the archives, SHA256SUMS over exactly the files, and
    candidate.json to the new directory `out`. Returns the candidate record.

    With `source` (the checkout at `commit`) this is a Release 1 candidate: the macOS, Windows and
    Linux archives with their plain names, the frozen sample from `source`, a four-line manifest and
    a schema 2 record. Without it, a preview candidate: two archives, no sample, schema 1."""
    check_arguments(version, commit, None, run_id, run_attempt)
    release = source is not None
    targets = RELEASE_TARGETS if release else TARGETS
    if sorted(inputs) != sorted(targets):
        raise ControllerError('the candidate needs exactly the ' + targets_text(targets) + ' builds')
    for target in targets:
        if not is_int(inputs[target][1], 1):
            raise ControllerError('the ' + target + ' artifact id is not a positive integer')
        digest_value(inputs[target][2], target)
    if len({inputs[t][1] for t in targets}) != len(targets):
        raise ControllerError('two builds share one artifact id')
    sample = read_sample(source, commit) if release else None
    out = Path(out)
    if out.exists() or out.is_symlink():
        raise ControllerError('the output directory already exists')
    out.mkdir(mode=0o700, parents=True)
    try:
        return assemble_into(inputs, version, commit, run_id, run_attempt, out, targets, sample)
    except BaseException:
        shutil.rmtree(str(out), ignore_errors=True)
        raise


def assemble_into(inputs, version, commit, run_id, run_attempt, out, targets, sample):
    builds, files = [], []
    name_for = release_archive_name if sample is not None else archive_name
    for target in targets:
        folder, ident, digest = inputs[target]
        name, receipt_name = name_for(version, target), 'build-' + target + '.json'
        path = downloaded_zip(folder, target)
        artifact_digest, artifact_size, archive = open_artifact(
            path, digest, {name: ARCHIVE_MAX, receipt_name: RECORD_MAX}, target, 'its archive and build receipt',
            ARCHIVE_MAX + RECORD_MAX + MIB)
        with archive:
            raw = read_member(archive, receipt_name, RECORD_MAX)
            receipt = check_build_receipt(loads(raw), target, version, commit, run_id, run_attempt, name_for)
            actual = extract_member(archive, name, out / name, ARCHIVE_MAX)
        if actual != (receipt['archive']['sha256'], receipt['archive']['size']):
            raise ControllerError('the ' + target + ' archive does not match its build receipt')
        files.append({'name': name, 'sha256': actual[0], 'size': actual[1]})
        builds.append({'target': target,
                       'artifact': {'id': ident, 'name': artifact_name(target, run_attempt),
                                    'digest': artifact_digest, 'size': artifact_size},
                       'receipt_sha256': hashlib.sha256(raw).hexdigest(),
                       'fast_gate': receipt['fast_gate'], 'sources': receipt['sources']})
    record = {'schema': 1, 'repository': REPOSITORY, 'ref': 'refs/tags/candidate/v' + version, 'version': version,
              'commit': commit, 'run_id': run_id, 'run_attempt': run_attempt}
    if sample is not None:
        data, facts = sample
        write_new(out / SAMPLE_NAME, data)
        files.append({'name': SAMPLE_NAME, 'sha256': facts['sha256'], 'size': facts['size']})
        record['schema'] = 2
        record['sample'] = facts
    files.sort(key=lambda f: f['name'])
    manifest = manifest_bytes(files)
    write_new(out / 'SHA256SUMS', manifest)
    record.update({'manifest_sha256': hashlib.sha256(manifest).hexdigest(), 'artifacts': files, 'builds': builds})
    write_new(out / 'candidate.json', canonical(record))
    return record


CANDIDATE_KEYS = ('schema', 'repository', 'ref', 'version', 'commit', 'run_id', 'run_attempt', 'manifest_sha256',
                  'artifacts', 'builds')
BUILD_KEYS = ('target', 'artifact', 'receipt_sha256', 'fast_gate', 'sources')


def load_candidate(raw):
    """Parses and checks a candidate.json record."""
    record = closed(loads(raw), CANDIDATE_KEYS, 'the candidate record')
    version = record['version']
    check_arguments(version, record['commit'], None, record['run_id'], record['run_attempt'])
    malformed = ControllerError('the candidate record is malformed')
    if record['schema'] != 1 or record['repository'] != REPOSITORY or record['ref'] != 'refs/tags/candidate/v' + version:
        raise malformed
    if not (isinstance(record['manifest_sha256'], str) and SHA256_RE.fullmatch(record['manifest_sha256'])):
        raise malformed
    files = record['artifacts']
    if not (isinstance(files, list) and [file_entry(f, 'a candidate artifact')['name'] for f in files]
            == [archive_name(version, t) for t in TARGETS]):
        raise malformed
    if hashlib.sha256(manifest_bytes(files)).hexdigest() != record['manifest_sha256']:
        raise malformed
    builds = record['builds']
    if not (isinstance(builds, list) and [closed(b, BUILD_KEYS, 'a candidate build')['target'] for b in builds]
            == list(TARGETS)):
        raise malformed
    for entry in builds:
        check_fast_gate(entry['fast_gate'], entry['target'])
    return record


RELEASE_CANDIDATE_KEYS = CANDIDATE_KEYS + ('sample',)
SAMPLE_KEYS = ('name', 'source', 'commit', 'sha256', 'size', 'freeze_sha256')


def load_release_candidate(raw):
    """Parses and checks a schema 2 (Release 1) candidate.json record."""
    record = closed(loads(raw), RELEASE_CANDIDATE_KEYS, 'the candidate record')
    version = record['version']
    check_arguments(version, record['commit'], None, record['run_id'], record['run_attempt'])
    malformed = ControllerError('the candidate record is malformed')
    if record['schema'] != 2 or record['repository'] != REPOSITORY or record['ref'] != 'refs/tags/candidate/v' + version:
        raise malformed
    if not (isinstance(record['manifest_sha256'], str) and SHA256_RE.fullmatch(record['manifest_sha256'])):
        raise malformed
    files = record['artifacts']
    if not (isinstance(files, list) and [file_entry(f, 'a candidate artifact')['name'] for f in files]
            == release_files(version)):
        raise malformed
    if hashlib.sha256(manifest_bytes(files)).hexdigest() != record['manifest_sha256']:
        raise malformed
    sample = closed(record['sample'], SAMPLE_KEYS, 'the candidate sample')
    if (sample['name'], sample['source'], sample['commit']) != (SAMPLE_NAME, SAMPLE_SOURCE, record['commit']):
        raise malformed
    if (sample['sha256'], sample['size']) != (files[-1]['sha256'], files[-1]['size']) or not (
            isinstance(sample['freeze_sha256'], str) and SHA256_RE.fullmatch(sample['freeze_sha256'])):
        raise malformed
    builds = record['builds']
    if not (isinstance(builds, list) and [closed(b, BUILD_KEYS, 'a candidate build')['target'] for b in builds]
            == list(RELEASE_TARGETS)):
        raise malformed
    for entry in builds:
        check_fast_gate(entry['fast_gate'], entry['target'])
    return record


# ---------------------------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------------------------

def parse_input(value):
    """TARGET=DIR,ID,DIGEST."""
    match = re.fullmatch(r'([a-z0-9_-]+)=([^,]+),([0-9]{1,19}),([^,]+)', value)
    if not match:
        raise argparse.ArgumentTypeError('expected TARGET=DIR,ID,DIGEST')
    return match.group(1), (Path(match.group(2)), int(match.group(3)), match.group(4))


def main(argv=None):
    parser = argparse.ArgumentParser(prog='candidate.py', description='Build and assemble one rdy candidate.')
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    p = sub.add_parser('check-ref')
    p.add_argument('--repo', type=Path, required=True)
    p.add_argument('--ref', required=True)
    p.add_argument('--sha', required=True)
    p.add_argument('--main', required=True)
    p.add_argument('--github-output', type=Path)
    p = sub.add_parser('gate-counts')
    p.add_argument('--report', type=Path, required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--out', type=Path, required=True)
    p = sub.add_parser('build')
    for name in ('--repo', '--work', '--out', '--fast-gate'):
        p.add_argument(name, type=Path, required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--target', required=True, choices=RELEASE_TARGETS)
    p.add_argument('--run-id', type=int, required=True)
    p.add_argument('--run-attempt', type=int, required=True)
    p = sub.add_parser('assemble')
    p.add_argument('--version', required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--run-id', type=int, required=True)
    p.add_argument('--run-attempt', type=int, required=True)
    p.add_argument('--input', type=parse_input, action='append', required=True)
    p.add_argument('--source', type=Path)
    p.add_argument('--out', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'check-ref':
            facts = check_ref(args.repo, args.ref, args.sha, args.main)
            if args.github_output:
                with open(str(args.github_output), 'a') as output:
                    output.write('version=' + facts['version'] + '\ncommit=' + facts['commit'] + '\n')
            sys.stdout.buffer.write(canonical(facts))
        elif args.command == 'gate-counts':
            with open(str(args.report), 'rb') as report:
                counts = fast_gate_counts(report.read(REPORT_MAX + 1), args.commit)
            write_new(args.out, canonical(counts))
            sys.stdout.buffer.write(canonical(counts))
        elif args.command == 'build':
            receipt = build(args.repo, args.commit, args.version, args.target, args.work, args.out, args.fast_gate,
                            args.run_id, args.run_attempt)
            sys.stdout.buffer.write(canonical(receipt))
        else:
            inputs = dict(args.input)
            if len(inputs) != len(args.input):
                raise ControllerError('the candidate needs exactly the ' + targets_text(
                    RELEASE_TARGETS if args.source else TARGETS) + ' builds')
            record = assemble(inputs, args.version, args.commit, args.run_id, args.run_attempt, args.out,
                              source=args.source)
            sys.stdout.buffer.write(canonical(record))
    except ControllerError as error:
        sys.stderr.write('release controller: ' + str(error) + '\n')
        return 1
    except OSError:
        sys.stderr.write('release controller: a file operation failed\n')
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
