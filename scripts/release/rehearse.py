#!/usr/bin/env python3
"""Rehearse one candidate target with an isolated test signer, then destroy every test state.

The founder's runner setup supplies the test signer: one executable with two verbs.
  <signer> sign     reads exactly the candidate's SHA256SUMS bytes on stdin and prints one JSON
                    object {"signature": <armored SSHSIG>, "signer_line": <allowed-signers line>}.
                    It may keep its test key under $RDY_REHEARSAL_STATE.
  <signer> destroy  erases its test key and root; exit 0 means they are gone.
The signer gets nothing else: no candidate path, no archive, no credential. Its environment is a
fixed minimal set, and a rehearsal refuses to start where AWS, OIDC or release-key settings are in
reach, so the machine KMS key is never used.

A rehearsal downloads nothing itself. It takes the assembled candidate artifact ZIP, checks its
artifact digest and the expected manifest digest, rehashes every archive, and copies one target's
exact bytes into a private boundary directory. There the test signature and the test root meet
the genuine native checker (qa-release-install). An optional session command (a fresh-agent
tutorial, run by the founder's setup) runs inside the same boundary. On every exit (success,
refusal, cancellation) the signer destroys its state and the boundary is removed. Only the
sanitized receipt leaves: no signature, no signer line, no test root.

Subcommands (each failure is one `release controller: <reason>` line on stderr and exit 1;
invalid arguments exit 2):
  run        rehearse one target and write its sanitized receipt
  tutorials  check the fresh-agent tutorial receipts against the candidate and print them sealed
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402

ControllerError = cand.ControllerError
TARGETS = cand.TARGETS
CHECK_NAMES = cand.CHECK_NAMES
SIGNER_ANSWER_MAX = 16 * cand.KIB
STATEMENT_MAX = 4096           # one SSHSIG or one public signer line
CHECK_DEADLINE = 600           # seconds for the native checker, which keeps its own bounds as built
TUTORIAL_STEPS_MAX = 256
# Environment that never reaches a rehearsal: KMS and OIDC settings, release keys and tokens.
FORBIDDEN_PREFIXES = ('AWS_', 'ACTIONS_ID_TOKEN_REQUEST_')
FORBIDDEN_NAMES = ('RDY_RELEASE_KEY_ARN', 'RDY_RELEASE_ROLE_ARN', 'GITHUB_TOKEN', 'GH_TOKEN')
# The only parent variables a test signer, checker or session sees.
PASSED_ENV = ('PATH', 'LANG', 'LC_ALL', 'TZ')
SIGNATURE_BEGIN = '-----BEGIN SSH SIGNATURE-----\n'
SIGNATURE_END = '-----END SSH SIGNATURE-----\n'
SIGNER_LINE_RE = re.compile(r'rdy-release namespaces="rdy-release-v1" '
                            r'(?:ssh-ed25519|sk-ssh-ed25519@openssh\.com) [A-Za-z0-9+/]+={0,2}')
CANCELLED = 'the rehearsal was cancelled; its test state was destroyed'
CLEANUP_FAILED = 'rehearsal cleanup failed; test state may remain'


class Cancelled(Exception):
    """A SIGTERM or SIGHUP during a rehearsal."""


def forbidden(env):
    return any(k.startswith(FORBIDDEN_PREFIXES) or k in FORBIDDEN_NAMES for k in env)


def child_env(env, boundary):
    out = {k: env[k] for k in PASSED_ENV if k in env}
    out.update({'HOME': str(boundary / 'state'), 'TMPDIR': str(boundary / 'tmp'),
                'RDY_REHEARSAL_STATE': str(boundary / 'state')})
    return out


def open_candidate(path, digest, manifest_sha256):
    """The candidate artifact ZIP, checked against its artifact digest, with exactly its two archives,
    SHA256SUMS and candidate.json, and a manifest equal to `manifest_sha256` over those archives.
    Returns (ZipFile, candidate record, manifest bytes)."""
    if not (isinstance(manifest_sha256, str) and cand.SHA256_RE.fullmatch(manifest_sha256)):
        raise ControllerError('the expected manifest digest is not 64 hex')
    _, _, archive = cand.open_artifact(Path(path), digest, None, 'candidate', '',
                                       2 * cand.ARCHIVE_MAX + 2 * cand.MIB)
    try:
        not_exact = ControllerError('the candidate artifact is not exactly its archives, SHA256SUMS and candidate.json')
        names = [i.filename for i in archive.infolist()]
        if 'candidate.json' not in names:
            raise not_exact
        record = cand.load_candidate(cand.read_member(archive, 'candidate.json', cand.RECORD_MAX))
        limits = {f['name']: cand.ARCHIVE_MAX for f in record['artifacts']}
        limits.update({'SHA256SUMS': cand.MANIFEST_MAX, 'candidate.json': cand.RECORD_MAX})
        if not cand.exact_members(archive, limits):
            raise not_exact
        manifest = cand.read_member(archive, 'SHA256SUMS', cand.MANIFEST_MAX)
        if (cand.hashlib.sha256(manifest).hexdigest() != manifest_sha256
                or record['manifest_sha256'] != manifest_sha256):
            raise ControllerError('the candidate manifest does not match the expected manifest digest')
        if manifest != cand.manifest_bytes(record['artifacts']):
            raise ControllerError('the candidate archives do not match their manifest')
    except BaseException:
        archive.close()
        raise
    return archive, record, manifest


def hash_member(archive, name, limit):
    digest, size = cand.hashlib.sha256(), 0
    with archive.open(name) as stream:
        while True:
            chunk = stream.read(cand.MIB)
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                raise ControllerError(name + ' is larger than ' + str(limit) + ' bytes')
            digest.update(chunk)
    return digest.hexdigest(), size


def check_signer_answer(raw):
    answer = cand.closed(cand.loads(raw, limit=SIGNER_ANSWER_MAX), ('signature', 'signer_line'),
                         'the test signer answer')
    signature, line = answer['signature'], answer['signer_line']
    if not (isinstance(signature, str) and isinstance(line, str)
            and len(signature.encode()) <= STATEMENT_MAX and len(line.encode()) <= STATEMENT_MAX
            and signature.startswith(SIGNATURE_BEGIN) and signature.endswith(SIGNATURE_END)
            and SIGNER_LINE_RE.fullmatch(line)):
        raise ControllerError('the test signer answer is malformed')
    return signature, line


def check_install_report(raw, target, version, commit):
    where = 'the ' + target + ' install report'
    report = cand.closed(cand.loads(raw, limit=cand.OUTPUT_MAX),
                         ('binary', 'checks', 'commit', 'os', 'skipped', 'target', 'version'), where)
    for key, want in (('version', version), ('commit', commit), ('target', target)):
        if report[key] != want:
            raise ControllerError(where + ' does not match ' + key)
    checks = report['checks']
    want = [{'name': n, 'passed': True} for n in CHECK_NAMES]
    if checks != want or any(type(c.get('passed')) is not bool for c in checks) \
            or not (cand.is_int(report['skipped']) and report['skipped'] == 0):
        raise ControllerError('the ' + target + ' install check did not pass all six checks')
    return {'report_sha256': cand.hashlib.sha256(raw).hexdigest(), 'checks': want, 'skipped': 0}


def run_checker(checker, argv, boundary, env):
    if callable(checker):
        return checker(argv, boundary)
    status, out, _ = cand.run_bounded(argv, cap=cand.OUTPUT_MAX, deadline=CHECK_DEADLINE, env=env, cwd=boundary)
    if status != 0:
        target = argv[argv.index('--target') + 1]
        raise ControllerError('the ' + target + ' install check failed')
    return out


def run_session(session, boundary, env):
    child = subprocess.Popen([str(a) for a in session], stdin=subprocess.DEVNULL, cwd=str(boundary), env=env)
    try:
        status = child.wait()
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
    if status != 0:
        raise ControllerError('the rehearsal session failed')


def remove_tree(path):
    def retry(function, name, _info):
        try:
            os.chmod(os.path.dirname(name), stat.S_IRWXU)
            os.chmod(name, stat.S_IRWXU)
            function(name)
        except OSError:
            pass
    if sys.version_info >= (3, 12):
        shutil.rmtree(str(path), onexc=retry)
    else:
        shutil.rmtree(str(path), onerror=retry)
    return not os.path.lexists(str(path))


def cleanup(boundary, signer, env):
    """Destroys the signer's state (once it was started) and removes the boundary. Returns whether
    both happened. Termination signals wait until it is done."""
    main = threading.current_thread() is threading.main_thread()
    held = {}
    if main:
        for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            held[number] = signal.signal(number, signal.SIG_IGN)
    try:
        ok = True
        if signer is not None:
            try:
                status, _, _ = cand.run_bounded(list(signer) + ['destroy'], cap=SIGNER_ANSWER_MAX, env=env,
                                                cwd=boundary)
                ok = status == 0
            except ControllerError:
                ok = False
        return remove_tree(boundary) and ok
    finally:
        for number, handler in held.items():
            signal.signal(number, handler)


def rehearse(artifact, artifact_digest, manifest_sha256, target, signer, workdir, out, checker, session=None,
             env=None):
    """Rehearses `target` of the candidate artifact ZIP `artifact` with the isolated test signer
    `signer` (an argv list) and the native checker (`checker`: a program path, or a callable taking
    (argv, boundary) and returning its stdout). Writes the sanitized receipt to the new file `out`
    only after the test state is destroyed, and returns it."""
    env = dict(os.environ if env is None else env)
    out = Path(out)
    if os.path.lexists(str(out)):
        raise ControllerError('the receipt file already exists')
    if forbidden(env):
        raise ControllerError('the rehearsal environment holds release credentials or KMS settings')
    if target not in TARGETS:
        raise ControllerError('the target is not a release target')
    workdir = Path(workdir)
    if workdir.is_symlink() or not workdir.is_dir():
        raise ControllerError('the rehearsal work directory does not exist')
    boundary = Path(tempfile.mkdtemp(prefix='rdy-rehearsal-', dir=str(workdir)))
    started = []
    environment = child_env(env, boundary)
    error, receipt = None, None
    try:
        try:
            receipt = rehearse_in(boundary, artifact, artifact_digest, manifest_sha256, target, signer, checker,
                                  session, environment, started)
        except (KeyboardInterrupt, Cancelled):
            error = ControllerError(CANCELLED)
        except ControllerError as refusal:
            error = refusal
        except OSError:
            error = ControllerError('a file operation failed')
    finally:
        if not cleanup(boundary, signer if started else None, environment):
            raise ControllerError(CLEANUP_FAILED)
    if error is not None:
        raise error
    cand.write_new(out, cand.canonical(receipt))
    return receipt


def rehearse_in(boundary, artifact, artifact_digest, manifest_sha256, target, signer, checker, session, env,
                started):
    for name in ('download', 'state', 'trust', 'prefix', 'tmp'):
        (boundary / name).mkdir(mode=0o700)
    download = boundary / 'download'
    archive, record, manifest = open_candidate(artifact, artifact_digest, manifest_sha256)
    version, commit = record['version'], record['commit']
    name = cand.archive_name(version, target)
    with archive:
        for entry in record['artifacts']:
            if entry['name'] == name:
                actual = cand.extract_member(archive, name, download / name, cand.ARCHIVE_MAX, 0o600)
            else:
                actual = hash_member(archive, entry['name'], cand.ARCHIVE_MAX)
            if actual != (entry['sha256'], entry['size']):
                raise ControllerError('the candidate archives do not match their manifest')
    cand.write_new(download / 'SHA256SUMS', manifest, 0o600)

    started.append(True)
    status, answer, _ = cand.run_bounded(list(signer) + ['sign'], cap=SIGNER_ANSWER_MAX, stdin=manifest, env=env,
                                         cwd=boundary / 'state')
    if status != 0:
        raise ControllerError('the test signer failed')
    signature, line = check_signer_answer(answer)
    cand.write_new(download / 'SHA256SUMS.sig', signature.encode(), 0o600)
    cand.write_new(boundary / 'trust' / 'allowed_signers', (line + '\n').encode(), 0o600)

    argv = [str(checker), '--download', str(download), '--signer-line', line, '--version', version,
            '--commit', commit, '--target', target, '--unpack', str(boundary / 'unpack'),
            '--prefix', str(boundary / 'prefix' / ('rdy-' + version))]
    install = check_install_report(run_checker(checker, argv, boundary, env), target, version, commit)
    ran = None
    if session:
        session_env = dict(env, RDY_REHEARSAL_DOWNLOAD=str(download),
                           RDY_REHEARSAL_ALLOWED_SIGNERS=str(boundary / 'trust' / 'allowed_signers'))
        run_session(session, boundary, session_env)
        ran = 'passed'
    entry = [f for f in record['artifacts'] if f['name'] == name][0]
    return {'schema': 1, 'version': version, 'commit': commit, 'target': target,
            'manifest_sha256': manifest_sha256, 'archive': dict(entry), 'install': install, 'session': ran,
            'cleanup': 'destroyed'}


TUTORIAL_KEYS = ('target', 'agent', 'model', 'effort', 'session', 'tutorial_revision', 'steps', 'archive_sha256',
                 'manifest_sha256', 'result')


def check_tutorials(entries, record):
    """E's `tutorials`: one passed fresh Sonnet/medium session per release target, each on that
    target's candidate archive and the candidate manifest. Returns them sorted by target."""
    per_target = ControllerError('the tutorials need exactly one entry per target')
    if not (isinstance(entries, list) and 0 < len(entries) <= 16):
        raise per_target
    for entry in entries:
        cand.closed(entry, TUTORIAL_KEYS, 'a tutorial')
        for key in ('target', 'agent', 'model', 'effort', 'session', 'tutorial_revision', 'archive_sha256',
                    'manifest_sha256', 'result'):
            cand.text(entry[key], key)
    if sorted(e['target'] for e in entries) != sorted(TARGETS):
        raise per_target
    if len({e['session'] for e in entries}) != len(entries):
        raise ControllerError('each tutorial needs its own fresh session')
    archives = {f['name']: f['sha256'] for f in record['artifacts']}
    sealed = []
    for entry in sorted(entries, key=lambda e: e['target']):
        target = entry['target']
        steps = entry['steps']
        if not (isinstance(steps, list) and 0 < len(steps) <= TUTORIAL_STEPS_MAX):
            raise ControllerError('the ' + target + ' tutorial steps are not 1 to 256 entries')
        for step in steps:
            cand.closed(step, ('id', 'result'), 'a tutorial step')
            cand.text(step['id'], 'step id')
            if step['result'] not in ('pass', 'fail') or not isinstance(step['result'], str):
                raise ControllerError('the ' + target + ' tutorial step result is not pass or fail')
        if len({s['id'] for s in steps}) != len(steps):
            raise ControllerError('the ' + target + ' tutorial repeats a step id')
        if entry['effort'] != 'medium' or 'sonnet' not in entry['model'].lower():
            raise ControllerError('the ' + target + ' tutorial is not a fresh Sonnet/medium session')
        if entry['archive_sha256'] != archives.get(cand.archive_name(record['version'], target)):
            raise ControllerError('the ' + target + ' tutorial did not use its candidate archive')
        if entry['manifest_sha256'] != record['manifest_sha256']:
            raise ControllerError('the ' + target + ' tutorial did not use the candidate manifest')
        if entry['result'] != 'pass' or any(s['result'] != 'pass' for s in steps):
            raise ControllerError('the ' + target + ' tutorial did not pass every step')
        sealed.append(dict(entry, steps=[dict(s) for s in steps]))
    return sealed


def on_signal(_number, _frame):
    raise Cancelled()


def main(argv=None):
    parser = argparse.ArgumentParser(prog='rehearse.py', description='Rehearse a candidate with an isolated test signer.')
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    p = sub.add_parser('run')
    p.add_argument('--artifact', type=Path, required=True)
    p.add_argument('--artifact-digest', required=True)
    p.add_argument('--manifest-sha256', required=True)
    p.add_argument('--target', required=True, choices=TARGETS)
    p.add_argument('--signer', type=Path, required=True)
    p.add_argument('--checker', type=Path, required=True)
    p.add_argument('--workdir', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('session', nargs='*')
    p = sub.add_parser('tutorials')
    p.add_argument('--candidate', type=Path, required=True)
    p.add_argument('--receipts', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'run':
            for number in (signal.SIGTERM, signal.SIGHUP):
                signal.signal(number, on_signal)
            receipt = rehearse(args.artifact, args.artifact_digest, args.manifest_sha256, args.target,
                               [str(args.signer)], args.workdir, args.out, args.checker, args.session or None)
            sys.stdout.buffer.write(cand.canonical(receipt))
        else:
            with open(str(args.candidate), 'rb') as handle:
                record = cand.load_candidate(handle.read(cand.RECORD_MAX + 1))
            with open(str(args.receipts), 'rb') as handle:
                entries = cand.loads(handle.read(cand.RECORD_MAX + 1))
            sys.stdout.buffer.write(cand.canonical(check_tutorials(entries, record)))
    except ControllerError as error:
        sys.stderr.write('release controller: ' + str(error) + '\n')
        return 1
    except OSError:
        sys.stderr.write('release controller: a file operation failed\n')
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
