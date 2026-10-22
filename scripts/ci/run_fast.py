#!/usr/bin/env python3
"""Run the existing fast gate without changing lockfile or performance authority."""
from contextlib import contextmanager
import hashlib
import json
import math
import os
import platform
import re
import secrets
from pathlib import Path
import subprocess
import sys
import stat

ROOT = Path(__file__).resolve().parents[2]


def tracked_snapshot(root):
    names = subprocess.check_output(['git', '-c', 'core.fsmonitor=false', '-c',
                                     'core.hooksPath=/dev/null', 'ls-files', '-z'], cwd=root).split(b'\0')
    result = {}
    for raw in names:
        if not raw:
            continue
        path = root / os.fsdecode(raw)
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            data = os.fsencode(os.readlink(path))
        elif stat.S_ISREG(metadata.st_mode):
            data = path.read_bytes()
        else:
            raise ValueError('tracked source is not a regular file or symlink')
        result[os.fsdecode(raw)] = (stat.S_IMODE(metadata.st_mode), hashlib.sha256(data).hexdigest())
    if not result:
        raise ValueError('empty tracked source')
    return result



# Reports are evidence, never a replacement for cargo qa's result. Raw QA details
# can contain source, failure text and private paths, so only these fields leave
# the runner. Do not upload target/qa/report.json or report.md directly.
GATES = (
    'cargo build --release -p rdy-cli', 'cargo fmt --all --check',
    'cargo clippy -D warnings', 'cargo test --workspace --exclude rdy-qa',
    'rdy-qa test layout', 'unix seams',
)
AREAS = frozenset(('brief cli code daemon engine gate invariants io mcp model '
                   'proposal rebase release render render_quality schema store swarm '
                   'tensions views wc lib').split())
COUNTS = ('passed', 'failed', 'ignored', 'skipped')
MAX_RAW_REPORT = 2 * 1024 * 1024
MAX_SUMMARY = 64 * 1024


def number(value):
    if type(value) is not int or not 0 <= value <= 100000000:
        raise ValueError('invalid count')
    return value


def boolean(value):
    if type(value) is not bool:
        raise ValueError('invalid outcome')
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate report key')
        result[key] = value
    return result


def sanitized_qa(raw, source_commit):
    """Copy only numeric/boolean outcomes and fixed names from the actual QA schema."""
    if len(raw) > MAX_RAW_REPORT:
        raise ValueError('oversized report')
    data = json.loads(raw, object_pairs_hook=unique_object)
    revision = data['revision']
    if not isinstance(revision, str) or not re.fullmatch(r'[a-f0-9]{7,40}', revision):
        raise ValueError('invalid revision')
    if source_commit is None or not source_commit.startswith(revision):
        raise ValueError('report source mismatch')
    if data['mode'] != 'standard':
        raise ValueError('unexpected gate mode')
    duration = data['duration_secs']
    if type(duration) not in (int, float) or not math.isfinite(duration) or not 0 <= duration <= 604800:
        raise ValueError('invalid duration')
    gates, areas = data['gates'], data['areas']
    if not isinstance(gates, list) or len(gates) != len(GATES):
        raise ValueError('invalid stages')
    if not isinstance(areas, list) or len(areas) > len(AREAS):
        raise ValueError('invalid areas')
    result = {'revision': revision, 'dirty': boolean(data['dirty']),
              'mode': 'standard', 'duration_secs': duration,
              'pass': boolean(data['pass']), 'stages': [], 'areas': [],
              'performance': 'not_requested_skip_perf', 'deep_gate': 'not_requested'}
    for gate, expected in zip(gates, GATES):
        if gate['name'] != expected:
            raise ValueError('unexpected stage')
        row = {'name': expected, 'pass': boolean(gate['pass'])}
        if expected == GATES[3]:
            # The first clause is structured by rdy-qa; any following test names,
            # panic text or compiler output is deliberately discarded.
            detail = gate['detail']
            match = re.match(r'^(\d+) passed, (\d+) failed, (\d+) ignored, (\d+) skipped(?=$|[;\n (])', detail)
            if not match:
                raise ValueError('missing workspace counts')
            row['counts'] = {key: number(int(value)) for key, value in zip(COUNTS, match.groups())}
        result['stages'].append(row)
    seen = set()
    for area in areas:
        name = area['name']
        if name not in AREAS or name in seen:
            raise ValueError('unexpected area')
        seen.add(name)
        result['areas'].append({'name': name, 'ok': boolean(area['ok']),
                                'counts': {key: number(area[key]) for key in COUNTS}})
    if result['pass'] and (seen != AREAS or any(not row['pass'] for row in result['stages'])
                           or result['stages'][3]['counts']['failed'] != 0
                           or any(not row['ok'] or row['counts']['failed'] for row in result['areas'])):
        raise ValueError('inconsistent successful report')
    result['area_totals'] = {key: sum(row['counts'][key] for row in result['areas']) for key in COUNTS}
    # Keep the workspace and QA area scopes separate rather than hiding their
    # different ignored/skipped coverage in a single pass/fail number.
    return result


def identity(root):
    def git_object(kind):
        try:
            value = subprocess.check_output(['git', 'rev-parse', kind], cwd=root,
                                            stderr=subprocess.DEVNULL).decode().strip()
            return value if re.fullmatch(r'[a-f0-9]{40}', value) else None
        except (OSError, subprocess.SubprocessError, UnicodeError):
            return None
    commit = git_object('HEAD')
    system, machine = platform.system(), platform.machine()
    result = {'commit': commit, 'tree': git_object('HEAD^{tree}'),
              'os': system if system in ('Linux', 'Darwin') else 'other',
              'architecture': machine if machine in ('x86_64', 'arm64', 'aarch64') else 'other'}
    for key, env, pattern in [('github_sha', 'GITHUB_SHA', r'[a-f0-9]{40}'),
                              ('run_id', 'GITHUB_RUN_ID', r'[0-9]{1,20}'),
                              ('run_attempt', 'GITHUB_RUN_ATTEMPT', r'[0-9]{1,5}')]:
        value = os.environ.get(env, '')
        result[key] = value if re.fullmatch(pattern, value) else None
    return result


@contextmanager
def report_directory(root):
    """Use directory descriptors so test-created symlinks cannot redirect writes."""
    descriptors = []
    try:
        current = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptors.append(current)
        for name in ('target', 'qa'):
            try:
                os.mkdir(name, dir_fd=current)
            except FileExistsError:
                pass
            current = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
            descriptors.append(current)
        yield current
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def clear_reports(root):
    with report_directory(root) as directory:
        for name in ('report.json', 'ci-fast-report.json'):
            try:
                os.unlink(name, dir_fd=directory)
            except FileNotFoundError:
                pass


def emit_report(root, summary):
    """Never include exception text, source snippets or raw report fields."""
    try:
        encoded = json.dumps(summary, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n'
        if len(encoded.encode()) > MAX_SUMMARY:
            raise ValueError('oversized summary')
        with report_directory(root) as directory:
            temporary = 'ci-fast-report-' + secrets.token_hex(16) + '.tmp'
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                 0o600, dir_fd=directory)
            try:
                with os.fdopen(descriptor, 'w') as handle:
                    handle.write(encoded)
                os.replace(temporary, 'ci-fast-report.json', src_dir_fd=directory,
                           dst_dir_fd=directory)
            finally:
                try:
                    os.unlink(temporary, dir_fd=directory)
                except FileNotFoundError:
                    pass
        print('CI_FAST_REPORT ' + encoded, end='', flush=True)
        return True
    except (OSError, ValueError, TypeError):
        print('CI_FAST_REPORT unavailable: sanitized report capture failed', file=sys.stderr)
        return False


def capture_qa(root, summary):
    try:
        with report_directory(root) as directory:
            descriptor = os.open('report.json', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
            with os.fdopen(descriptor, 'rb') as handle:
                if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                    raise ValueError('not a regular report')
                raw = handle.read(MAX_RAW_REPORT + 1)
        summary['qa'] = sanitized_qa(raw, summary['source']['commit'])
        summary['report_status'] = 'captured'
    except FileNotFoundError:
        summary['report_status'] = 'missing'
    except (OSError, ValueError, TypeError, KeyError, RecursionError):
        summary['report_status'] = 'invalid'


def main():
    os.chdir(ROOT)
    summary = {'schema_version': 1, 'source': identity(ROOT),
               'invocation': ['cargo', 'qa', '--skip-perf', '--require-all'],
               'stage': 'prerequisites', 'cargo_qa_exit_code': None,
               'wrapper_exit_code': None, 'protected_inputs_unchanged': None,
               'report_status': 'not_run', 'qa': None}
    # Clear both outputs before fetch or compilation can fail. If cleanup fails,
    # do not run the gate with possibly stale evidence still at the upload path.
    try:
        clear_reports(ROOT)
    except OSError:
        print('CI_FAST_REPORT unavailable: cannot clear old report outputs', file=sys.stderr)
        return 1
    emit_report(ROOT, summary)
    try:
        code = run_gate(summary)
    finally:
        if summary['cargo_qa_exit_code'] is not None:
            capture_qa(ROOT, summary)
        if summary['wrapper_exit_code'] == 0 and (summary['report_status'] != 'captured'
                                                or not summary['qa']['pass']):
            summary['wrapper_exit_code'] = 1
        written = emit_report(ROOT, summary)
        # Upload only an output emitted in this execution. In particular, an
        # uncleared old file or a test-created directory is never uploaded.
        if written and os.environ.get('GITHUB_OUTPUT'):
            try:
                descriptor = os.open(os.environ['GITHUB_OUTPUT'],
                                     os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(descriptor, 'w') as output:
                    if not stat.S_ISREG(os.fstat(output.fileno()).st_mode):
                        raise OSError('invalid runner output file')
                    output.write('report_ready=true\n')
            except OSError:
                written = False
    # A failed gate keeps its original code. Only a cargo success can become a
    # reporting-contract failure; unavailable evidence is never a passing report.
    if code == 0:
        return summary['wrapper_exit_code'] if written else 1
    return code


def run_gate(summary):
    if os.name != 'posix' or os.geteuid() == 0:
        raise SystemExit('fast gate requires a non-root Unix runner')
    lock = ROOT / 'Cargo.lock'
    baseline = ROOT / 'crates/rdy-qa/qa-baseline.json'
    before = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in (lock, baseline)}
    source_before = tracked_snapshot(ROOT)
    summary['source']['tracked_inputs_sha256'] = hashlib.sha256(
        json.dumps(source_before, sort_keys=True).encode()).hexdigest()
    version = '1.99.0'
    toolchain = (ROOT / 'rust-toolchain.toml').read_text()
    if 'channel = "' + version + '"' not in toolchain:
        raise SystemExit('review the pinned CI toolchain before changing rust-toolchain.toml')
    summary['stage'] = 'locked_fetch'
    emit_report(ROOT, summary)
    # --locked is necessary before child Cargo invocations are switched offline.
    subprocess.run(['cargo', '+' + version, 'fetch', '--locked'], check=True)
    env = dict(os.environ, CARGO_NET_OFFLINE='true', RUSTUP_TOOLCHAIN=version,
               RDY_QA_REQUIRE_ALL='1')
    summary['stage'] = 'cargo_qa'
    summary['report_status'] = 'incomplete'
    emit_report(ROOT, summary)
    result = subprocess.run(['cargo', 'qa', '--skip-perf', '--require-all'], env=env)
    summary['cargo_qa_exit_code'] = result.returncode
    changed = [str(p.relative_to(ROOT)) for p, digest in before.items()
               if not p.is_file() or hashlib.sha256(p.read_bytes()).hexdigest() != digest]
    if tracked_snapshot(ROOT) != source_before:
        changed.append('tracked source inventory/content/modes')
    summary['protected_inputs_unchanged'] = not changed
    summary['stage'] = 'completed'
    if changed:
        print('gate changed protected inputs: ' + ', '.join(changed), file=sys.stderr)
        summary['wrapper_exit_code'] = 1
        return 1
    summary['wrapper_exit_code'] = result.returncode
    return result.returncode


if __name__ == '__main__':
    sys.exit(main())
