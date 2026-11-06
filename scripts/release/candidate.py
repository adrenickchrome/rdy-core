#!/usr/bin/env python3
"""Build and assemble one immutable rdy preview candidate.

The candidate is the unsigned bytes of one release: one native archive per release target and a
SHA256SUMS manifest over exactly those archives. It is built from `git archive S` in a new
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
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import threading
import zipfile

REPOSITORY = 'adrenickchrome/rdy-core'
# The release target set at this base. A new platform needs its card and a qualified adapter.
TARGETS = ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu')
GUIDE_TOOL = Path(__file__).resolve().parent / 'guide.py'
# The packaged checker's six checks, in order (`CHECK_NAMES` in rdy-qa's release_install.rs).
CHECK_NAMES = ('signature', 'checksums', 'install', 'version', 'daemon', 'mcp')

KIB = 1024
MIB = 1024 * KIB
RECORD_MAX = 64 * KIB          # a receipt, policy or tuple JSON
ARCHIVE_MAX = 512 * MIB        # one release archive
MANIFEST_MAX = 1024            # the adapter's manifest limit
OUTPUT_MAX = 4 * MIB           # a child's output
REPORT_MAX = 16 * MIB          # the native fast gate's report.json
ASSET_MAX = 32                 # files in one artifact
HELPER_DEADLINE = 120          # seconds for one helper child
SCAN_CHUNK = 1 * MIB           # host-path scan window
SCAN_HITS_MAX = 1000

VERSION_RE = re.compile(r'0\.[1-9][0-9]*\.0')
CANDIDATE_REF_RE = re.compile(r'refs/tags/candidate/v(0\.[1-9][0-9]*\.0)')
COMMIT_RE = re.compile(r'[0-9a-f]{40}')
SHA256_RE = re.compile(r'[0-9a-f]{64}')
DIGEST_RE = re.compile(r'(?:sha256:)?([0-9a-f]{64})')
SOURCE_NAME_RE = re.compile(r'[A-Za-z0-9_][A-Za-z0-9_.+-]{0,127}\.crate')
# Home prefixes of every hosted runner and every local build machine.
HOST_PATTERNS = (b'/Users/', b'/home/')
HOSTED_HOMES = ('/Users/runner', '/home/runner')


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
    """The preview archive's file name (`archive_name` in rdy-qa's release_install.rs)."""
    return 'rdy-' + version + '-preview-' + target + '.tar.gz'


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
        if not (isinstance(policy, dict) and policy.get('targets') == list(TARGETS)):
            raise ControllerError('the target policy at the commit is not exactly ' + ' and '.join(TARGETS))
    for taken in ('refs/tags/reserve/v' + version, 'refs/tags/v' + version):
        status, _ = git(repo, 'show-ref', '--verify', '--quiet', taken)
        if status == 0:
            raise ControllerError('v' + version + ' is already reserved or released; a retry reuses its '
                                  'reserved candidate and never rebuilds')
    return {'ref': ref, 'version': version, 'commit': sha, 'targets': list(TARGETS)}


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

def remap_flags(home, cargo_home, rustup_home, source, target_dir):
    """rustc path remaps for a release build. The hosted runner homes come first; rustc applies the
    last matching remap, so each more specific prefix follows the home it lies in."""
    pairs = [(HOSTED_HOMES[0], '/build'), (HOSTED_HOMES[1], '/build'), (home, '/build'),
             (cargo_home, '/cargo'), (rustup_home, '/rustup'), (source, '/rdy'), (target_dir, '/target')]
    flags = []
    for prefix, to in pairs:
        if not (isinstance(prefix, str) and prefix.startswith('/') and len(prefix) > 1
                and not re.search(r'[\s=\x00-\x1f]', prefix)):
            raise ControllerError('a build path is not a plain absolute path')
        flag = '--remap-path-prefix=' + prefix.rstrip('/') + '=' + to
        if flag not in flags:
            flags.append(flag)
    return flags


def inspect_archive(path, version, target, extra=()):
    """Checks that `path` is one packaged release (every member under its preview stem, no link,
    one regular `rdy`), then scans that binary for host paths: any `/Users/` or `/home/` and each
    prefix in `extra`. Returns 0, or refuses with the number of host paths found."""
    stem = 'rdy-' + version + '-preview-' + target
    bad = ControllerError('the ' + target + ' archive is not one packaged release')
    patterns = list(HOST_PATTERNS) + [p.encode() for p in extra if p]
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
                if member.name != stem + '/rdy':
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
                            hits.add(start + at)
                            at = window.find(pattern, at + 1)
                    base += len(chunk)
                    tail = window[-(longest - 1):]
            if binary is None:
                raise bad
    except (tarfile.TarError, OSError, EOFError):
        raise bad
    if hits:
        count = len(hits)
        raise ControllerError('the ' + target + ' binary holds ' + str(count) + ' host path'
                              + ('' if count == 1 else 's'))
    return 0


def check_packaged_guide(path, version, target, source, work, env):
    """Runs the `rdy` of the inspected archive `path` and compares its built-in rows and menu with
    the guide bundle of the source archive. Refuses with the checker's own line."""
    folder = Path(work) / 'guide-check'
    folder.mkdir(mode=0o700)
    binary = folder / 'rdy'
    bad = ControllerError('the ' + target + ' archive is not one packaged release')
    try:
        with tarfile.open(str(path), mode='r:gz') as archive:
            stream = archive.extractfile('rdy-' + version + '-preview-' + target + '/rdy')
            fd = os.open(str(binary), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
            with os.fdopen(fd, 'wb') as handle:
                shutil.copyfileobj(stream, handle, MIB)
    except (tarfile.TarError, OSError, EOFError, KeyError, AttributeError):
        raise bad
    release = Path(source) / 'release'
    status, _, said = run_bounded([sys.executable, '-I', '-B', GUIDE_TOOL, 'check-binary', '--binary', binary,
                                   '--bundle', release / 'agent-guide.json', '--sidecar', release / 'agent-guide.sha256'],
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
    if target is not None and target not in TARGETS:
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


def build(repo, commit, version, target, work, out, fast_gate_path, run_id, run_attempt, env=None):
    """Builds `target` from `git archive <commit>` in the new workspace `work`, stages the MPL source
    crates with qa-release-sources (which checks release/notes.md first), packages with
    qa-release-package, inspects the packaged binary, and writes the archive and its build receipt
    to the new directory `out`."""
    env = dict(os.environ if env is None else env)
    check_arguments(version, commit, target, run_id, run_attempt)
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
    status, _, _ = run_bounded(['tar', '-x', '-f', work / 'source.tar', '-C', source])
    if status != 0:
        raise ControllerError('the source archive did not extract')
    (work / 'source.tar').unlink()
    notes = source / 'release' / 'notes.md'
    if notes.is_symlink() or not notes.is_file():
        raise ControllerError('release/notes.md is missing at the commit')

    home = env.get('HOME', '')
    cargo_home = env.get('CARGO_HOME') or home + '/.cargo'
    rustup_home = env.get('RUSTUP_HOME') or home + '/.rustup'
    flags = remap_flags(home, cargo_home, rustup_home, str(source), str(target_dir))
    env.pop('RUSTFLAGS', None)
    env.update({'CARGO_ENCODED_RUSTFLAGS': '\x1f'.join(flags), 'CARGO_TARGET_DIR': str(target_dir),
                'CARGO_INCREMENTAL': '0'})
    build_step(['cargo', 'fetch', '--locked'], source, env)
    env['CARGO_NET_OFFLINE'] = 'true'
    build_step(['cargo', 'build', '--release', '--locked', '-p', 'rdy-cli', '-p', 'rdy-qa'], source, env)

    release = target_dir / 'release'
    status, said, _ = run_bounded([release / 'rdy', '--version'], env=env)
    if status != 0 or said != ('rdy ' + version + ' (preview)\n').encode():
        raise ControllerError('the built rdy does not print rdy ' + version + ' (preview)')
    status, _, _ = run_bounded([release / 'qa-release-sources', '--version', version, '--out', work / 'sources',
                                '--check-notes', notes], env=env)
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
    name = archive_name(version, target)
    status, said, _ = run_bounded([release / 'qa-release-package', '--binary', release / 'rdy', '--version', version,
                                   '--target', target, '--out', work / 'package'], env=env)
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


def check_build_receipt(receipt, target, version, commit, run_id, run_attempt):
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
    if receipt['archive']['name'] != archive_name(version, target):
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


def assemble(inputs, version, commit, run_id, run_attempt, out):
    """Verifies each target's downloaded build artifact (`inputs`: target -> (download directory,
    artifact id, artifact digest)) and writes the two archives, SHA256SUMS over exactly those
    archives, and candidate.json to the new directory `out`. Returns the candidate record."""
    check_arguments(version, commit, None, run_id, run_attempt)
    if sorted(inputs) != sorted(TARGETS):
        raise ControllerError('the candidate needs exactly the ' + ' and '.join(TARGETS) + ' builds')
    for target in TARGETS:
        if not is_int(inputs[target][1], 1):
            raise ControllerError('the ' + target + ' artifact id is not a positive integer')
        digest_value(inputs[target][2], target)
    if len({inputs[t][1] for t in TARGETS}) != len(TARGETS):
        raise ControllerError('two builds share one artifact id')
    out = Path(out)
    if out.exists() or out.is_symlink():
        raise ControllerError('the output directory already exists')
    out.mkdir(mode=0o700, parents=True)
    try:
        return assemble_into(inputs, version, commit, run_id, run_attempt, out)
    except BaseException:
        shutil.rmtree(str(out), ignore_errors=True)
        raise


def assemble_into(inputs, version, commit, run_id, run_attempt, out):
    builds, files = [], []
    for target in TARGETS:
        folder, ident, digest = inputs[target]
        name, receipt_name = archive_name(version, target), 'build-' + target + '.json'
        path = downloaded_zip(folder, target)
        artifact_digest, artifact_size, archive = open_artifact(
            path, digest, {name: ARCHIVE_MAX, receipt_name: RECORD_MAX}, target, 'its archive and build receipt',
            ARCHIVE_MAX + RECORD_MAX + MIB)
        with archive:
            raw = read_member(archive, receipt_name, RECORD_MAX)
            receipt = check_build_receipt(loads(raw), target, version, commit, run_id, run_attempt)
            actual = extract_member(archive, name, out / name, ARCHIVE_MAX)
        if actual != (receipt['archive']['sha256'], receipt['archive']['size']):
            raise ControllerError('the ' + target + ' archive does not match its build receipt')
        files.append({'name': name, 'sha256': actual[0], 'size': actual[1]})
        builds.append({'target': target,
                       'artifact': {'id': ident, 'name': artifact_name(target, run_attempt),
                                    'digest': artifact_digest, 'size': artifact_size},
                       'receipt_sha256': hashlib.sha256(raw).hexdigest(),
                       'fast_gate': receipt['fast_gate'], 'sources': receipt['sources']})
    files.sort(key=lambda f: f['name'])
    manifest = manifest_bytes(files)
    write_new(out / 'SHA256SUMS', manifest)
    record = {'schema': 1, 'repository': REPOSITORY, 'ref': 'refs/tags/candidate/v' + version, 'version': version,
              'commit': commit, 'run_id': run_id, 'run_attempt': run_attempt,
              'manifest_sha256': hashlib.sha256(manifest).hexdigest(), 'artifacts': files, 'builds': builds}
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
    parser = argparse.ArgumentParser(prog='candidate.py', description='Build and assemble one rdy preview candidate.')
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
    p.add_argument('--target', required=True, choices=TARGETS)
    p.add_argument('--run-id', type=int, required=True)
    p.add_argument('--run-attempt', type=int, required=True)
    p = sub.add_parser('assemble')
    p.add_argument('--version', required=True)
    p.add_argument('--commit', required=True)
    p.add_argument('--run-id', type=int, required=True)
    p.add_argument('--run-attempt', type=int, required=True)
    p.add_argument('--input', type=parse_input, action='append', required=True)
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
                raise ControllerError('the candidate needs exactly the ' + ' and '.join(TARGETS) + ' builds')
            record = assemble(inputs, args.version, args.commit, args.run_id, args.run_attempt, args.out)
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
