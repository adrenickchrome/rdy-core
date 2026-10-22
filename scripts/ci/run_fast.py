#!/usr/bin/env python3
"""Run the existing fast gate without changing lockfile or performance authority."""
import hashlib
import os
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


def main():
    os.chdir(ROOT)
    if os.name != 'posix' or os.geteuid() == 0:
        raise SystemExit('fast gate requires a non-root Unix runner')
    lock = ROOT / 'Cargo.lock'
    baseline = ROOT / 'crates/rdy-qa/qa-baseline.json'
    before = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in (lock, baseline)}
    source_before = tracked_snapshot(ROOT)
    version = '1.99.0'
    toolchain = (ROOT / 'rust-toolchain.toml').read_text()
    if 'channel = \"' + version + '\"' not in toolchain:
        raise SystemExit('review the pinned CI toolchain before changing rust-toolchain.toml')
    # --locked is necessary before child Cargo invocations are switched offline.
    subprocess.run(['cargo', '+' + version, 'fetch', '--locked'], check=True)
    env = dict(os.environ, CARGO_NET_OFFLINE='true', RUSTUP_TOOLCHAIN=version,
               RDY_QA_REQUIRE_ALL='1')
    result = subprocess.run(['cargo', 'qa', '--skip-perf', '--require-all'], env=env)
    changed = [str(p.relative_to(ROOT)) for p, digest in before.items()
               if not p.is_file() or hashlib.sha256(p.read_bytes()).hexdigest() != digest]
    if tracked_snapshot(ROOT) != source_before:
        changed.append('tracked source inventory/content/modes')
    if changed:
        print('gate changed protected inputs: ' + ', '.join(changed), file=sys.stderr)
        return 1
    return result.returncode


if __name__ == '__main__':
    sys.exit(main())
