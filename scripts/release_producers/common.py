"""Shared pieces of the two release producers: the trusted parser path, closed control inputs and the
dispatch bound.

The producers reuse the release parsers and read-only GitHub facts of the reviewed signer revision H,
unchanged, from a separate checkout. Nothing here writes to GitHub, AWS or a key. A refusal is the release
scripts' ControllerError.

Runtime code stays compatible with Python 3.9 (`/usr/bin/python3` on macOS) and imports no YAML parser.
"""
import os
from pathlib import Path
import sys

KIB = 1024
MIB = 1024 * KIB

# The complete dispatch request and the attestation transport (plan: inputs without a new producer).
DISPATCH_MAX = 61440            # the serialized UTF-8 dispatch request, ref and inputs included
BASE64_MAX = 57344              # canonical padded standard base64 of the attestation gzip
GZIP_MAX = 43008                # the decoded gzip bytes
DOCUMENT_MAX = 1048576          # the streamed decompressed attestation
APPROVAL_RUNS_MAX = 16          # approval run locators named by one E dispatch
APPROVAL_RUNS_BYTES_MAX = 65536
ID_DIGITS_MAX = 20              # a decimal GitHub id in a dispatch input
EVENT_MAX = 1 * MIB             # the workflow event file
OUTPUT_MAX = 64 * KIB           # one record a producer writes

MAIN_REF = 'refs/heads/main'
TRUSTED_MARKER = ('scripts', 'release', 'eligibility.py')


# ---------------------------------------------------------------------------------------------
# The trusted parsers (a checkout of H, never S)
# ---------------------------------------------------------------------------------------------

def trust(argv):
    """Puts the trusted release parsers on `sys.path` for a helper run as a script, and returns the
    remaining arguments. The first two arguments must be `--trusted <checkout of H>`: a literal path in
    the workflow, never a dispatch input."""
    if len(argv) < 3 or argv[1] != '--trusted':
        sys.stderr.write('release producer: the first argument is not --trusted <checkout>\n')
        raise SystemExit(2)
    root = Path(argv[2])
    if root.is_symlink() or not root.is_dir() or not root.joinpath(*TRUSTED_MARKER).is_file():
        sys.stderr.write('release producer: the trusted checkout has no release parsers\n')
        raise SystemExit(2)
    sys.path.insert(0, str(root.resolve() / 'scripts' / 'release'))
    return [argv[0]] + argv[3:]


def _release():
    """The trusted release modules, as eligibility itself imported them: one module set, one error class."""
    import eligibility as elig
    return elig.cand, elig.ev, elig.gh, elig


def controller_error():
    return _release()[0].ControllerError


def refuse(message):
    """A refusal with the release scripts' own error class."""
    return controller_error()(message)


# ---------------------------------------------------------------------------------------------
# Control inputs
# ---------------------------------------------------------------------------------------------

def load_event(data):
    """The workflow event file: closed JSON (no duplicate key, float or non-finite number; UTF-8)."""
    cand = _release()[0]
    value = cand.loads(data, EVENT_MAX)
    if not isinstance(value, dict):
        raise refuse('the event is not an object')
    if not isinstance(value.get('inputs'), dict):
        raise refuse('the event has no inputs object')
    return value


def read_event(path):
    """The event file at `path`, read with its bound before parsing."""
    try:
        with open(str(path), 'rb') as handle:
            data = handle.read(EVENT_MAX + 1)
    except OSError:
        raise refuse('the event file cannot be read')
    return load_event(data)


def utf8(value, name):
    """The UTF-8 bytes of an input string; a lone surrogate refuses."""
    if not isinstance(value, str):
        raise refuse(name + ' is not a string')
    try:
        return value.encode('utf-8')
    except UnicodeEncodeError:
        raise refuse(name + ' is not UTF-8')


def dispatch_request(ref, inputs):
    """The serialized dispatch request whose size GitHub bounds: canonical `{inputs, ref}`."""
    cand = _release()[0]
    return cand.canonical({'inputs': inputs, 'ref': ref})


def check_dispatch(ref, inputs):
    """The size of the complete dispatch request, refused above the bound before any submission."""
    size = len(dispatch_request(ref, inputs))
    if size > DISPATCH_MAX:
        raise refuse('the dispatch request is larger than ' + str(DISPATCH_MAX) + ' bytes')
    return size


# ---------------------------------------------------------------------------------------------
# Read-only GitHub facts
# ---------------------------------------------------------------------------------------------

READS = ('repository', 'run_attempt', 'attempt_jobs', 'job', 'artifact', 'run_artifacts', 'artifact_member',
         'run_approvals', 'approval_responses', 'contents', 'ref', 'commit', 'tag', 'compare', 'releases',
         'release_by_tag', 'open_code_scanning_alerts', 'workflow')


class ReadOnly(object):
    """A fact source that exposes only named reads. No mutation, archive, download-to-path or trust-tool
    call passes through it: a producer can never reach `create_*` or `release_trust`."""

    def __init__(self, facts, reads=READS):
        self._facts = facts
        self._reads = tuple(reads)

    def __getattr__(self, name):
        if name.startswith('_') or name not in self._reads:
            raise AttributeError(name)
        return getattr(self._facts, name)


def token():
    """The job's read-only token from the step environment. It is never printed."""
    value = os.environ.get('GITHUB_TOKEN', '')
    if not value:
        raise refuse('the step has no GITHUB_TOKEN')
    return value


def github(work):
    """The live read-only fact source: H's `sign.LiveFacts` (a `github.GitHub`) over the real transport.
    Its trust verifier is never called; the ReadOnly wrapper refuses `release_trust`."""
    _, _, gh, _ = _release()
    import sign
    return ReadOnly(sign.LiveFacts(gh.UrllibTransport(token()), work, os.devnull))


def run_identity():
    """This run's id and attempt, from the runner's environment (never from dispatch input)."""
    cand = _release()[0]
    values = []
    for name in ('GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT'):
        text = os.environ.get(name, '')
        if not (text.isdigit() and text[0] != '0' and len(text) <= ID_DIGITS_MAX and text.isascii()):
            raise refuse(name + ' is not a positive decimal')
        values.append(int(text))
    if not all(cand.is_int(v, 1) for v in values):
        raise refuse('the run identity is malformed')
    return values[0], values[1]


def write_record(folder, name, data):
    """Writes `data` as the one new file `name` in the new private folder `folder`."""
    cand = _release()[0]
    if len(data) > OUTPUT_MAX:
        raise refuse(name + ' is larger than ' + str(OUTPUT_MAX) + ' bytes')
    folder = Path(folder)
    try:
        folder.mkdir(mode=0o700)
    except FileExistsError:
        if folder.is_symlink() or not folder.is_dir():
            raise refuse('the output folder is not a directory')
    except OSError:
        raise refuse('the output folder cannot be created')
    path = folder / name
    cand.write_new(path, data)
    return path


def main_guard(run):
    """Runs a helper's command: a refusal prints `release producer: <reason>` (never input bytes) and exits 1."""
    try:
        code = run()
    except Exception as error:  # noqa: BLE001 - only the release error class is reported by text
        names = [k.__name__ for k in type(error).__mro__]
        if 'ControllerError' not in names:
            sys.stderr.write('release producer: internal error (' + type(error).__name__ + ')\n')
            return 1
        sys.stderr.write('release producer: ' + str(error).replace('\n', ' ')[:512] + '\n')
        return 1
    return code or 0
