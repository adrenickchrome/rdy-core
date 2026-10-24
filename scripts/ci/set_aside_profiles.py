#!/usr/bin/env python3
"""Set aside coverage profiles whose header is corrupt, before the coverage reports merge them.

A killed instrumented child can leave a .profraw with a corrupt header, and one such file makes
the whole merge fail. Each nonempty profile is checked with `llvm-profdata show`, one at a time.
Only the one known header diagnostic (exact bytes, exit status 1) moves the profile out of the
tree; every other failure is forwarded unchanged and fails this script. Zero-byte profiles stay
in place for the merge, which accepts them. This script reads no profile bytes itself, merges
nothing, runs no report and removes nothing: rejected profiles are kept under RUNNER_TEMP.
"""
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

CHUNK = 65536


class ProfileCheckFailed(Exception):
    """A check failed and its diagnostics are already forwarded; `status` is the exit status."""

    def __init__(self, status):
        super().__init__(status)
        self.status = status


def profiles(directory):
    """Yield every .profraw below `directory` in name order, one directory read at a time."""
    for name in sorted(os.listdir(directory)):
        path = directory / name
        # lstat propagates every stat error and never follows a symlink, so a directory link is
        # not entered (os.path.isdir would turn a stat error into False and skip the entry).
        if stat.S_ISDIR(os.lstat(path).st_mode):
            yield from profiles(path)
        elif name.endswith('.profraw'):
            yield path


def forward(stderr):
    stderr.seek(0)
    while True:
        chunk = stderr.read(CHUNK)
        if not chunk:
            break
        sys.stderr.buffer.write(chunk)
    sys.stderr.buffer.flush()


def has_corrupt_header(profile, status, stderr):
    expected = ('error: %s: invalid instrumentation profile data (file header is corrupt)\n'
                % profile)
    expected = os.fsencode(expected)
    stderr.seek(0)
    return status == 1 and stderr.read(len(expected) + 1) == expected


def set_aside_profiles(profile_root, profdata, work_root):
    """Check every nonempty profile; return how many were checked."""
    checked = 0
    rejected = None
    for profile in profiles(profile_root):
        if os.stat(profile).st_size == 0:
            continue
        checked += 1
        with tempfile.TemporaryFile() as stderr:
            status = subprocess.run([str(profdata), 'show', str(profile)], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=stderr).returncode
            if status == 0:
                continue
            if not has_corrupt_header(profile, status, stderr):
                forward(stderr)
                raise ProfileCheckFailed(status if status > 0 else 1)
        if rejected is None:
            rejected = Path(tempfile.mkdtemp(prefix='rdy-profraw.', dir=work_root)) / 'rejected'
        destination = rejected / profile.relative_to(profile_root)
        if os.path.lexists(destination):
            raise FileExistsError(17, 'File exists', str(destination))
        destination.parent.mkdir(parents=True, exist_ok=True)
        os.rename(profile, destination)
        sys.stdout.write('coverage: set aside corrupt-header profile: %s -> %s\n'
                         % (json.dumps(str(profile), ensure_ascii=True),
                            json.dumps(str(destination), ensure_ascii=True)))
        sys.stdout.flush()
    return checked


def rustc(*args):
    """Run rustc; return its stdout, or None after printing its stderr when it failed."""
    done = subprocess.run(['rustc', *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          encoding='utf-8', errors='surrogateescape')
    if done.returncode != 0:
        sys.stderr.write(done.stderr)
        return None
    return done.stdout


def main():
    if len(sys.argv) > 1:
        sys.stderr.write('coverage: expected no arguments\n')
        return 1
    try:
        if 'RUNNER_TEMP' not in os.environ:
            sys.stderr.write('coverage: RUNNER_TEMP is not set\n')
            return 1
        sysroot = rustc('--print', 'sysroot')
        if sysroot is None:
            return 1
        version = rustc('-vV')
        if version is None:
            return 1
        hosts = [line[len('host: '):] for line in version.splitlines() if line.startswith('host: ')]
        if not hosts:
            sys.stderr.write('coverage: rustc did not report a host\n')
            return 1
        profdata = Path(sysroot.rstrip('\n')) / 'lib' / 'rustlib' / hosts[0] / 'bin' / 'llvm-profdata'
        set_aside_profiles(Path('target/llvm-cov-target'), profdata, Path(os.environ['RUNNER_TEMP']))
    except ProfileCheckFailed as failed:
        return failed.status
    except OSError as error:
        sys.stderr.write('%s\n' % error)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
