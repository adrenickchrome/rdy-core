"""The native install check of one E target: jobs `install-mac` and `install-linux` of
`.github/workflows/release-evidence.yml`, each on one fresh standard GitHub-hosted VM.

  run      authenticates the dispatch again (read-only), creates the new job-owned root
           `$RUNNER_TEMP/rdy-release-rehearsal/<run_id>-<attempt>/<target>` (0700, no symlink, no escape),
           downloads the exact candidate artifact by id and service digest, copies the checker built from H at
           its fixed build path into `tools/`, measures and records its provenance, checks the architecture,
           verifies the measured digest immediately before invocation, and runs H's unchanged `rehearse.py`
           with the producer's disposable test signer and a minimal environment (no token, AWS, OIDC or
           release-key variable). The sanitized receipt `out/rehearsal-<target>.json` is checked before the
           step succeeds; only that file is uploaded.
  cleanup  the job's last step, always: removes every task-owned path. Uncertain cleanup exits 1.

No path or command comes from dispatch input: run id and attempt come from the runner, the target is one of
two constants, and every path is derived from them. Environment filtering is not an OS sandbox; the fresh VM's
destruction is the outer containment, and unlinking files is not secure erasure.

Run as `/usr/bin/python3 -I -B <P>/scripts/release_producers/install.py --trusted <checkout of H> <command>`.
A refusal prints `release producer: <reason>` and exits 1.
"""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import signal
import stat
import subprocess
import sys
import tempfile

if __name__ == '__main__':
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from release_producers import common
    _ARGV = common.trust(sys.argv)
else:
    from release_producers import common

from release_producers import evidence  # noqa: E402

cand, ev, gh, elig = common._release()
rehearse = ev.rehearse
ControllerError = cand.ControllerError

H = '3dac36753d9512773f91a2650c79e2805941ecbd'
RUST = '1.99.0'
RECIPE = ('cargo +1.99.0 build --locked --offline --release -p rdy-qa --bin qa-release-install '
          '--target <fixed-native-target>\n')
RECIPE_SHA256 = 'a5abc9b8e8689be5a7853ee48280f5017295f4d0e2380af89783829d2b909057'
LOCK_SHA256 = '126dfc4360e2630c9d9ccaec4118e78d7653891779306fbe8d46e182830415fc'
CHECKER = 'qa-release-install'
CHECKER_MAX = 256 * cand.MIB
# The runner and machine each native target must report: no emulation, no other architecture.
ARCHITECTURE = {'aarch64-apple-darwin': ('ARM64', 'arm64'), 'x86_64-unknown-linux-gnu': ('X64', 'x86_64')}
PYTHON = '/usr/bin/python3'
SIGNER = Path(__file__).resolve().parent / 'rehearsal_test_signer.py'
PASSED_ENV = rehearse.PASSED_ENV           # PATH, LANG, LC_ALL, TZ: everything else stays out of H
REHEARSAL_DEADLINE = 50 * 60               # seconds; the job's own timeout is 60 minutes
STOP_GRACE = 120                           # seconds for H's cleanup after a forwarded termination
JOB_DIRS = ('input', 'tools', 'work', 'out')
OUTPUT_MAX = 64 * cand.KIB


def _decimal(value, what):
    text = str(value) if type(value) is int else value
    if not (isinstance(text, str) and text.isascii() and evidence.DECIMAL_RE.fullmatch(text)):
        raise ControllerError('the ' + what + ' is not a positive decimal')
    return int(text)


def _temp(runner_temp):
    path = Path(runner_temp) if isinstance(runner_temp, str) else None
    if path is None or not path.is_absolute() or path.is_symlink() or not path.is_dir():
        raise ControllerError('the runner temporary directory is not a real absolute directory')
    return path


def _scope(runner_temp, run_id, run_attempt, target, top):
    temp = _temp(runner_temp)
    if target not in evidence.NATIVE:
        raise ControllerError('the target is not a native evidence target')
    return temp, temp / top / (str(_decimal(run_id, 'run id')) + '-' + str(_decimal(run_attempt, 'run attempt'))) \
        / target


def job_root(runner_temp, run_id, run_attempt, target):
    """The fixed job root of one native rehearsal (not created)."""
    return _scope(runner_temp, run_id, run_attempt, target, 'rdy-release-rehearsal')[1]


def build_root(runner_temp, run_id, run_attempt, target):
    """The fixed, task-specific folder of the checker build (its CARGO_TARGET_DIR is `<root>/target`)."""
    return _scope(runner_temp, run_id, run_attempt, target, 'rdy-release-checker')[1]


def build_path(runner_temp, run_id, run_attempt, target):
    """Where the recipe leaves the native checker; the caller cannot choose this path."""
    return build_root(runner_temp, run_id, run_attempt, target) / 'target' / target / 'release' / CHECKER


def _private_dir(path):
    """Creates `path` (0700), or accepts an existing real, private, own directory."""
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    info = os.lstat(str(path))
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise ControllerError('a job path is not a real directory of this job')
    os.chmod(str(path), 0o700)


def make_job_root(runner_temp, run_id, run_attempt, target):
    """Creates the new job root and its four private folders. Any symlink or existing root refuses."""
    temp = _temp(runner_temp)
    root = job_root(runner_temp, run_id, run_attempt, target)
    _private_dir(root.parent.parent)
    _private_dir(root.parent)
    if os.path.lexists(str(root)):
        raise ControllerError('the job root already exists')
    root.mkdir(mode=0o700)
    if os.path.realpath(str(root)) != os.path.join(os.path.realpath(str(temp)), str(root.relative_to(temp))):
        raise ControllerError('the job root escapes the runner temporary directory')
    for name in JOB_DIRS:
        (root / name).mkdir(mode=0o700)
    return root


def _regular(path, what):
    info = os.lstat(str(path))
    if not stat.S_ISREG(info.st_mode):
        raise ControllerError('the ' + what + ' is not a regular file')
    return info


def install_checker(runner_temp, run_id, run_attempt, target, root):
    """Copies only the built native checker into `tools/` and returns its measured SHA-256."""
    built = build_path(runner_temp, run_id, run_attempt, target)
    try:
        _regular(built, 'built checker')
    except OSError:
        raise ControllerError('the checker was not built at its fixed path')
    out = Path(root) / 'tools' / CHECKER
    digest, size = hashlib.sha256(), 0
    fd = os.open(str(out), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
    with os.fdopen(fd, 'wb') as handle, open(str(built), 'rb') as source:
        while True:
            chunk = source.read(cand.MIB)
            if not chunk:
                break
            size += len(chunk)
            if size > CHECKER_MAX:
                raise ControllerError('the checker is larger than ' + str(CHECKER_MAX) + ' bytes')
            digest.update(chunk)
            handle.write(chunk)
    os.chmod(str(out), 0o700)
    return digest.hexdigest()


def verify_checker(root, checker_sha256):
    """The pinned digest, checked immediately before the checker can run."""
    path = Path(root) / 'tools' / CHECKER
    try:
        _regular(path, 'checker')
        actual, _ = cand.sha256_file(path, CHECKER_MAX)
    except OSError:
        raise ControllerError('the checker is missing')
    if actual != checker_sha256:
        raise ControllerError('the checker is not its measured digest')
    return path


def _observe(argv):
    """A fixed read-only command's combined output (rustc, git, ssh -V)."""
    status, out, err = cand.run_bounded(argv, cap=OUTPUT_MAX, deadline=60)
    if status != 0:
        raise ControllerError('a provenance command failed')
    return (out + err).decode('utf-8', 'replace')


def provenance(trusted, target, env, checker_sha256, runner=_observe, machine=None, lock_sha256=LOCK_SHA256):
    """The checker's sanitized companion evidence: source H, lockfile, recipe, compiler, target, image,
    architecture, OpenSSH and the build run. Every identity it can check against a pin is checked here."""
    trusted = Path(trusted)
    runner_arch, uname = ARCHITECTURE[target]
    if env.get('RUNNER_ARCH') != runner_arch:
        raise ControllerError('the runner architecture is not ' + runner_arch)
    machine = platform.machine() if machine is None else machine
    if machine != uname:
        raise ControllerError('the machine is not ' + uname)
    if runner(['git', '-C', str(trusted), 'rev-parse', 'HEAD']).strip() != H:
        raise ControllerError('the trusted checkout is not H')
    lock, _ = cand.sha256_file(trusted / 'Cargo.lock', 4 * cand.MIB)
    if lock != lock_sha256:
        raise ControllerError("the trusted Cargo.lock is not H's lockfile")
    if hashlib.sha256(RECIPE.encode('utf-8')).hexdigest() != RECIPE_SHA256:
        raise ControllerError('the build recipe is not the reviewed recipe')
    fields = {}
    for line in runner(['rustc', '+' + RUST, '-vV']).splitlines():
        key, _, value = line.partition(': ')
        fields[key] = value
    if fields.get('release') != RUST:
        raise ControllerError('the compiler is not Rust ' + RUST)
    if fields.get('host') != target:
        raise ControllerError('the compiler host is not ' + target)
    return {'checker_sha256': checker_sha256, 'source': H, 'lock_sha256': lock, 'recipe_sha256': RECIPE_SHA256,
            'rust': RUST, 'rust_host': fields['host'], 'target': target, 'image_os': env.get('ImageOS'),
            'image_version': env.get('ImageVersion'), 'runner_arch': env.get('RUNNER_ARCH'), 'machine': machine,
            'openssh': runner(['/usr/bin/ssh', '-V']).strip()[:200],
            'run_id': _decimal(env.get('GITHUB_RUN_ID'), 'run id'),
            'run_attempt': _decimal(env.get('GITHUB_RUN_ATTEMPT'), 'run attempt'), 'job': env.get('GITHUB_JOB')}


def rehearsal_env(environ):
    """H's environment: only PATH, LANG, LC_ALL and TZ. Credentials are absent, not empty."""
    return {k: environ[k] for k in PASSED_ENV if k in environ}


def rehearse_argv(trusted, root, target, artifact_digest, manifest_sha256, python=PYTHON, signer=SIGNER):
    """H's fixed `rehearse.py run` arguments; every path is derived, none comes from input."""
    root = Path(root)
    return [str(python), '-I', '-B', str(Path(trusted) / 'scripts' / 'release' / 'rehearse.py'), 'run',
            '--artifact', str(root / 'input' / 'candidate.zip'), '--artifact-digest', artifact_digest,
            '--manifest-sha256', manifest_sha256, '--target', target, '--signer', str(signer),
            '--checker', str(root / 'tools' / CHECKER), '--workdir', str(root / 'work'),
            '--out', str(root / 'out' / evidence.receipt_member(target))]


def _run_h(argv, env, deadline=REHEARSAL_DEADLINE):
    """Runs H. A termination of this step is forwarded to H, whose finally path destroys the test state;
    it is never killed first. Returns H's exit status, or None when its cleanup is uncertain."""
    child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, env=env)
    held = {}

    def forward(number, _frame):
        if child.poll() is None:
            child.send_signal(signal.SIGTERM)

    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        held[number] = signal.signal(number, forward)
    try:
        try:
            return child.wait(timeout=deadline)
        except subprocess.TimeoutExpired:
            child.send_signal(signal.SIGTERM)
            try:
                child.wait(timeout=STOP_GRACE)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            return None
    finally:
        for number, handler in held.items():
            signal.signal(number, handler)


def run_rehearsal(trusted, root, target, artifact_digest, manifest_sha256, checker_sha256, environ, version,
                  commit, archive, python=PYTHON, signer=SIGNER):
    """Verifies the checker, runs H once, and returns the install record of its sanitized receipt."""
    root = Path(root)
    out = root / 'out' / evidence.receipt_member(target)
    if os.path.lexists(str(out)):
        raise ControllerError('the receipt file already exists')
    verify_checker(root, checker_sha256)
    status = _run_h(rehearse_argv(trusted, root, target, artifact_digest, manifest_sha256, python, signer),
                    rehearsal_env(environ))
    if status is None:
        raise ControllerError('the ' + target + ' rehearsal passed its deadline; its cleanup is uncertain')
    if os.listdir(str(root / 'work')):
        raise ControllerError('the ' + target + ' rehearsal left test state; its cleanup is uncertain')
    if status != 0:
        raise ControllerError('the ' + target + ' rehearsal failed')
    try:
        _regular(out, 'receipt')
        with open(str(out), 'rb') as handle:
            data = handle.read(ev.RECORD_MAX + 1)
    except OSError:
        raise ControllerError('the ' + target + ' rehearsal wrote no receipt')
    return evidence.parse_rehearsal_receipt(data, version, commit, target, manifest_sha256, archive)


def fetch_candidate(auth, root):
    """The exact authenticated candidate artifact, downloaded read-only by id and checked against its
    service digest and byte bound."""
    out = Path(root) / 'input' / 'candidate.zip'
    auth.facts.call('download_artifact', auth.artifact, out, evidence.CANDIDATE_ZIP_MAX)
    os.chmod(str(out), 0o600)
    actual, size = cand.sha256_file(out, evidence.CANDIDATE_ZIP_MAX)
    if (actual, size) != (auth.artifact.digest, auth.artifact.size):
        raise ControllerError('the candidate download does not match its service digest')
    return out


def cleanup(runner_temp, run_id, run_attempt, target):
    """Removes the job root and the checker build. Returns True; uncertain cleanup refuses. A symlink is
    removed as a link, never followed."""
    certain = True
    for path in (job_root(runner_temp, run_id, run_attempt, target),
                 build_root(runner_temp, run_id, run_attempt, target)):
        if path.is_symlink():
            path.unlink()
            certain = False
        elif os.path.lexists(str(path)) and not rehearse.remove_tree(path):
            certain = False
    if not certain:
        raise ControllerError('cleanup of the job root is uncertain')
    return True


# ---------------------------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------------------------

def parser():
    top = argparse.ArgumentParser(prog='install.py')
    commands = top.add_subparsers(dest='command', required=True)
    command = commands.add_parser('run')
    command.add_argument('--target', choices=sorted(evidence.NATIVE), required=True)
    command.add_argument('--event', required=True)
    command = commands.add_parser('cleanup')
    command.add_argument('--target', choices=sorted(evidence.NATIVE), required=True)
    return top


def run(argv, trusted):
    args = parser().parse_args(argv[1:])
    environ = dict(os.environ)
    runner_temp = environ.get('RUNNER_TEMP', '')
    run_id, run_attempt = common.run_identity()
    os.umask(0o077)
    if args.command == 'cleanup':
        cleanup(runner_temp, run_id, run_attempt, args.target)
        sys.stdout.write('cleanup complete\n')
        return 0
    root = make_job_root(runner_temp, run_id, run_attempt, args.target)
    work = tempfile.mkdtemp(prefix='api-', dir=str(root / 'input'))
    try:
        facts = common.github(work)
        auth = evidence.authenticate(facts, run_id, run_attempt, evidence.event_inputs(args.event))
        fetch_candidate(auth, root)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    digest = install_checker(runner_temp, run_id, run_attempt, args.target, root)
    record = provenance(trusted, args.target, environ, digest)
    sys.stdout.write('checker provenance: ' + cand.canonical(record).decode('utf-8'))
    archive = [f for f in auth.record['artifacts'] if f['name'] == cand.archive_name(auth.version, args.target)][0]
    install = run_rehearsal(trusted, root, args.target, auth.artifact.digest, auth.record['manifest_sha256'], digest,
                            environ, auth.version, auth.commit, archive)
    sys.stdout.write(args.target + ' install report sha256: ' + install['report_sha256'] + '\n')
    return 0


if __name__ == '__main__':
    sys.exit(common.main_guard(lambda: run(_ARGV, Path(sys.argv[2]).resolve())))
