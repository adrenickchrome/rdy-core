#!/usr/bin/env python3
"""Narrow release control plane. Candidate payloads are data, never control code."""
import argparse
import base64
import datetime as dt
import hashlib
import gzip
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile

REPO = 'adrenickchrome/rdy-core'
REPO_ID = 1403751388
TARGETS = ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu')
SIGNER = 'rdy-release namespaces="rdy-release-v1" sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIDiiQ0vO3dvG2AtbJYDwsshJl75kvbbnRDOCUdsjfrDRAAAAD3NzaDpyZHktcmVsZWFzZQ=='
ROOT = Path(__file__).resolve().parents[2]
MAX_FILE = 100 * 1024 * 1024
MAX_PACKET = 240 * 1024 * 1024
REQUIRED = {'ci.yml': 'Core CI', 'platform.yml': 'Core platforms', 'security.yml': 'Core security'}


class Refusal(Exception):
    pass


def require(condition, message):
    if not condition:
        raise Refusal(message)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read_json(path):
    require(Path(path).stat().st_size <= 4 * 1024 * 1024, 'JSON exceeds limit')
    def pairs(items):
        out = {}
        for key, value in items:
            require(key not in out, 'duplicate JSON key')
            out[key] = value
        return out
    return json.loads(Path(path).read_text(), object_pairs_hook=pairs)


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def now():
    return dt.datetime.now(dt.timezone.utc)


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout.strip()


def version(value):
    require(re.fullmatch(r'0\.[1-9][0-9]{0,9}\.0', value) is not None, 'invalid preview version')
    return value


def hex40(value):
    require(re.fullmatch('[a-f0-9]{40}', value or '') is not None, 'invalid source identity')
    return value


def integer(value):
    require(re.fullmatch('[1-9][0-9]{0,19}', str(value)) is not None, 'invalid run identity')
    return int(value)


def archive_name(ver, target):
    version(ver)
    require(target in TARGETS, 'unsupported archive target')
    return f'rdy-{ver}-preview-{target}.tar.gz'


def output(key, value):
    require(re.fullmatch('[a-z_]+', key) is not None, 'unsafe output key')
    require('\n' not in str(value) and '\r' not in str(value), 'unsafe output value')
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
            stream.write(f'{key}={value}\n')
    else:
        print(f'{key}={value}')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise Refusal('unexpected authenticated API redirect')


def api(path, method='GET', data=None, absent=False, upload=None):
    require(path.startswith('/') and not path.startswith('//'), 'invalid API path')
    host = 'https://uploads.github.com' if upload is not None else 'https://api.github.com'
    headers = {'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'rdy-core-release'}
    if os.environ.get('GH_TOKEN'):
        headers['Authorization'] = 'Bearer ' + os.environ['GH_TOKEN']
    body = None
    if upload is not None:
        require(Path(upload).stat().st_size <= MAX_FILE, 'asset exceeds limit')
        body = Path(upload).read_bytes()
        headers['Content-Type'] = 'application/octet-stream'
    elif data is not None:
        body = json.dumps(data).encode()
        headers['Content-Type'] = 'application/json'
    request = urllib.request.Request(host + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=90) as response:
            blob = response.read(8 * 1024 * 1024 + 1)
            require(len(blob) <= 8 * 1024 * 1024, 'API response exceeds limit')
            return json.loads(blob) if blob else None
    except urllib.error.HTTPError as exc:
        if absent and exc.code == 404:
            return None
        raise Refusal(f'GitHub API refused {method} request (HTTP {exc.code}); reconcile before retry') from None


def identity():
    require(os.environ.get('GITHUB_REPOSITORY') == REPO, 'wrong repository')
    require(os.environ.get('GITHUB_REPOSITORY_ID') == str(REPO_ID), 'wrong repository ID')
    require(os.environ.get('GITHUB_REF') == 'refs/heads/main', 'release controls must run from main')
    require(os.environ.get('GITHUB_EVENT_NAME') == 'workflow_dispatch', 'release requires explicit dispatch')
    commit = hex40(os.environ.get('GITHUB_SHA'))
    require(run(['git', 'rev-parse', 'HEAD']) == commit, 'checkout differs from control revision')
    require(api(f'/repos/{REPO}')['id'] == REPO_ID, 'native repository ID mismatch')
    return commit


def source_unchanged():
    require(not run(['git', 'status', '--porcelain', '--untracked-files=no']), 'tracked source or lockfile changed')


def required_checks(commit):
    """Workflow path, exact main push SHA, aggregate job; names alone are insufficient."""
    accepted = {}
    for workflow, name in REQUIRED.items():
        result = api(f'/repos/{REPO}/actions/workflows/{workflow}/runs?event=push&head_sha={commit}&per_page=100')
        runs = [r for r in result['workflow_runs'] if r['head_sha'] == commit and r['head_branch'] == 'main' and r['event'] == 'push' and r['path'] == '.github/workflows/' + workflow]
        require(runs, 'missing exact-source required workflow: ' + workflow)
        latest = max(runs, key=lambda r: r['id'])
        require(latest['status'] == 'completed' and latest['conclusion'] == 'success', 'required workflow not successful: ' + workflow)
        jobs = api(f'/repos/{REPO}/actions/runs/{latest["id"]}/attempts/{latest["run_attempt"]}/jobs?per_page=100')
        require(jobs['total_count'] <= 100, 'unexpected job pagination')
        match = [j for j in jobs['jobs'] if j['name'] == name]
        require(len(match) == 1 and match[0]['status'] == 'completed' and match[0]['conclusion'] == 'success', 'missing successful required aggregate: ' + name)
        accepted[workflow] = {'run_id': latest['id'], 'attempt': latest['run_attempt'], 'aggregate_job_id': match[0]['id']}
    return accepted


def start(ver, destination):
    commit = identity()
    version(ver)
    require(tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version'] == ver, 'source version mismatch')
    require('const PREVIEW: bool = true;' in (ROOT / 'crates/rdy-cli/src/main.rs').read_text(), 'source is not a preview')
    require(api(f'/repos/{REPO}/git/ref/tags/v{ver}', absent=True) is None, 'release tag already exists')
    require(api(f'/repos/{REPO}/releases/tags/v{ver}', absent=True) is None, 'release version already exists')
    checks = required_checks(commit)
    source_unchanged()
    encoded = os.environ.get('FACTORY_QUALIFICATION_BASE64', '')
    require(0 < len(encoded) <= 32768, 'factory qualification input missing or oversized')
    external = json.loads(base64.b64decode(encoded, validate=True))
    validate_factory(external, commit)
    write_json(destination, {'source_sha': commit, 'version': ver, 'checks': checks, 'factory_qualification': external,
                             'external_evidence_policy': 'Attributed factory receipts; release owner must inspect underlying evidence. Hosted fast QA does not rerun deep/perf.'})


def validate_factory(external, commit):
    require(isinstance(external, dict) and set(external) == {'schema', 'source_sha', 'deep', 'performance', 'agent_guide_sha256'}, 'factory qualification fields mismatch')
    require(external['schema'] == 1 and external['source_sha'] == commit, 'factory qualification source mismatch')
    require(external['agent_guide_sha256'] == sha(ROOT / 'scripts/release/agent-guide.md'), 'factory has not qualified this end-user guide')
    for name in ('deep', 'performance'):
        gate = external[name]
        require(isinstance(gate, dict) and set(gate) == {'result', 'command', 'evidence_sha256', 'evidence_uri', 'machine_class'}, 'factory gate fields mismatch')
        require(gate['result'] == 'pass', 'required external gate missing or nonpassing')
        require(isinstance(gate['command'], str) and 0 < len(gate['command']) < 512 and not any(c in gate['command'] for c in '\r\n\x00'), 'invalid external command record')
        require(re.fullmatch('[a-f0-9]{64}', gate['evidence_sha256'] or ''), 'missing external evidence digest')
        uri = urllib.parse.urlsplit(gate['evidence_uri'])
        require(uri.scheme == 'https' and uri.hostname and not uri.username and not uri.password and not uri.query and not uri.fragment and len(gate['evidence_uri']) < 1024, 'external evidence URI must be stable credential-free HTTPS')
        require(isinstance(gate['machine_class'], str) and 0 < len(gate['machine_class']) <= 200 and not any(c in gate['machine_class'] for c in '\r\n\x00'), 'missing external machine class')
        if name == 'performance':
            require(gate['machine_class'] in read_json(ROOT / 'crates/rdy-qa/qa-baseline.json')['classes'], 'performance machine class has no accepted baseline')


class BoundedArchiveReader:
    """Bound decompressed bytes before tarfile can consume PAX/long-name metadata."""
    def __init__(self, stream, limit):
        self.stream, self.remaining = stream, limit

    def read(self, size=-1):
        requested = self.remaining + 1 if size < 0 else min(size, self.remaining + 1)
        data = self.stream.read(requested)
        self.remaining -= len(data)
        require(self.remaining >= 0, 'decompressed archive exceeds limit')
        return data


def inspect_archive(path, ver, target):
    require(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= MAX_FILE, 'invalid archive file')
    stem = archive_name(ver, target)[:-7]
    expected = {stem + '/rdy', stem + '/LICENSE-MIT', stem + '/LICENSE-APACHE', stem + '/PREVIEW.txt'}
    seen, size = set(), 0
    with gzip.open(path, 'rb') as compressed, tarfile.open(fileobj=BoundedArchiveReader(compressed, MAX_FILE + 1024 * 1024), mode='r|') as archive:
        for item in archive:
            name = item.name.rstrip('/')
            require(name not in seen, 'duplicate archive member')
            seen.add(name)
            require(not item.pax_headers or all(k in ('mtime', 'atime', 'ctime') for k in item.pax_headers), 'unexpected archive metadata')
            if name == stem:
                require(item.isdir(), 'invalid archive root')
            else:
                require(name in expected and item.isfile() and not item.issym() and not item.islnk(), 'unsafe archive member')
                require(item.mode & 0o7000 == 0 and 0 < item.size <= MAX_FILE, 'invalid archive member mode or size')
                size += item.size
                require(size <= MAX_FILE, 'expanded archive exceeds limit')
                if name.endswith('/PREVIEW.txt'):
                    require(item.size <= 4096, 'oversize preview marker')
                    require(b'preview' in archive.extractfile(item).read().lower(), 'missing preview marker')
    require(seen in (expected, expected | {stem}), 'archive members incomplete')


def build_record(ver, target, directory):
    require(target in TARGETS, 'unsupported native build')
    commit = hex40(os.environ.get('GITHUB_SHA'))
    dest = Path(directory)
    path = dest / archive_name(ver, target)
    inspect_archive(path, ver, target)
    binary = ROOT / 'target/release/rdy'
    blob = binary.read_bytes()
    for value in (str(ROOT), str(Path.home())):
        require(value.encode() not in blob, 'private build path embedded in binary')
    binary_kind = run(['file', str(binary)]).replace(str(binary), 'rdy')
    required_kind = 'arm64' if target == TARGETS[0] else 'x86-64'
    require(required_kind in binary_kind, 'binary architecture mismatch')
    if target == TARGETS[1]:
        dynamic = run(['readelf', '-d', str(binary)])
        symbols = run(['readelf', '--version-info', str(binary)])
        glibc = sorted(set(re.findall(r'GLIBC_[0-9.]+', symbols)), key=lambda v: tuple(map(int, v[6:].split('.'))))
        platform = {'kernel': run(['uname', '-sr']), 'glibc_required_symbols': glibc, 'dynamic_libraries': re.findall(r'Shared library: \[([^]]+)\]', dynamic)}
    else:
        platform = {'os': run(['sw_vers']), 'libraries': run(['otool', '-L', str(binary)]).replace(str(binary), 'rdy'), 'load_commands': run(['otool', '-l', str(binary)]).replace(str(binary), 'rdy')}
    # Resolved Cargo graph, including build dependencies; not a runtime-only claim.
    metadata = json.loads(run(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1']))
    packages = {p['id']: p for p in metadata['packages']}
    refs = {pid: hashlib.sha256(pid.replace(str(ROOT), '/src').encode()).hexdigest() for pid in packages}
    components = []
    for pid, p in sorted(packages.items()):
        c = {'type': 'library', 'bom-ref': refs[pid], 'name': p['name'], 'version': p['version']}
        if p.get('license'):
            c['licenses'] = [{'expression': p['license']}]
        if (p.get('source') or '').startswith('registry+'):
            c['purl'] = f'pkg:cargo/{p["name"]}@{p["version"]}'
        components.append(c)
    sbom = {'bomFormat': 'CycloneDX', 'specVersion': '1.6', 'version': 1, 'metadata': {'component': {'type': 'application', 'name': 'rdy', 'version': ver}, 'properties': [{'name': 'rdy:scope', 'value': 'resolved Cargo graph including build dependencies'}, {'name': 'rdy:Cargo.lock.sha256', 'value': sha(ROOT / 'Cargo.lock')}]}, 'components': components, 'dependencies': [{'ref': refs[n['id']], 'dependsOn': sorted(refs[d] for d in n['dependencies'])} for n in metadata['resolve']['nodes']]}
    write_json(dest / f'sbom-{target}.cdx.json', sbom)
    source_unchanged()
    write_json(dest / f'build-{target}.json', {'schema': 1, 'source_sha': commit, 'control_sha': commit, 'version': ver, 'target': target, 'archive': path.name, 'sha256': sha(path), 'bytes': path.stat().st_size, 'binary_kind': binary_kind, 'platform': platform, 'rustc': run(['rustc', '-Vv']), 'cargo_lock_sha256': sha(ROOT / 'Cargo.lock'), 'gate': 'fast --skip-perf --require-all passed; ignored and performance tests remain separate', 'run_id': integer(os.environ['GITHUB_RUN_ID']), 'run_attempt': integer(os.environ['GITHUB_RUN_ATTEMPT'])})


def packet_files(ver):
    return {archive_name(ver, t) for t in TARGETS} | {f'build-{t}.json' for t in TARGETS} | {f'sbom-{t}.cdx.json' for t in TARGETS} | {'SHA256SUMS', 'install.sh', 'INSTALL.md', 'AGENTS.md', 'CLAUDE.md', 'admission.json'}


def regular_files(folder, allowed):
    entries = list(folder.iterdir())
    require({p.name for p in entries} == allowed, 'packet asset set mismatch')
    require(all(p.is_file() and not p.is_symlink() and 0 < p.stat().st_size <= MAX_FILE for p in entries), 'unsafe or oversized packet file')
    require(sum(p.stat().st_size for p in entries) <= MAX_PACKET, 'packet exceeds limit')


def assemble(ver, directory):
    folder = Path(directory)
    commit = identity()
    for t in TARGETS:
        record = read_json(folder / f'build-{t}.json')
        require(record['source_sha'] == record['control_sha'] == commit and record['target'] == t and record['version'] == ver, 'build identity mismatch')
        require(record['run_id'] == integer(os.environ['GITHUB_RUN_ID']) and record['run_attempt'] == integer(os.environ['GITHUB_RUN_ATTEMPT']), 'build run mismatch')
        require(record['sha256'] == sha(folder / archive_name(ver, t)), 'build archive digest mismatch')
        inspect_archive(folder / archive_name(ver, t), ver, t)
    for src, name in [('scripts/install.sh', 'install.sh'), ('docs/install.md', 'INSTALL.md'), ('scripts/release/agent-guide.md', 'AGENTS.md'), ('scripts/release/agent-guide.md', 'CLAUDE.md')]:
        shutil.copyfile(ROOT / src, folder / name)
    (folder / 'SHA256SUMS').write_text(''.join(f'{sha(folder / archive_name(ver, t))}  {archive_name(ver, t)}\n' for t in TARGETS))
    regular_files(folder, packet_files(ver))
    admission = read_json(folder / 'admission.json')
    require(admission['source_sha'] == commit and admission['version'] == ver, 'admission identity mismatch')
    artifacts = api(f'/repos/{REPO}/actions/runs/{integer(os.environ["GITHUB_RUN_ID"])}/artifacts?per_page=100')
    require(artifacts['total_count'] <= 100, 'unexpected artifact pagination')
    native = [a for a in artifacts['artifacts'] if a['name'] in {'native-' + t for t in TARGETS}]
    require(len(native) == 2 and len({a['name'] for a in native}) == 2 and all(not a['expired'] and re.fullmatch('sha256:[a-f0-9]{64}', a.get('digest', '')) for a in native), 'native artifact identities missing')
    record = {'schema': 1, 'repository': REPO, 'repository_id': REPO_ID, 'source_sha': commit, 'control_sha': commit, 'version': ver, 'run_id': integer(os.environ['GITHUB_RUN_ID']), 'run_attempt': integer(os.environ['GITHUB_RUN_ATTEMPT']), 'created_at': now().isoformat(), 'expires_at': (now() + dt.timedelta(days=7)).isoformat(), 'native_artifacts': [{k: a[k] for k in ('id', 'name', 'digest')} for a in native], 'files': {p.name: {'sha256': sha(p), 'bytes': p.stat().st_size} for p in sorted(folder.iterdir())}}
    write_json(folder / 'candidate.json', record)
    output('manifest_sha', sha(folder / 'SHA256SUMS'))
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as stream:
            stream.write(f'## Candidate awaiting offline signature\nVersion: `{ver}`; source/control: `{commit}`.\n\nRun `{record["run_id"]}`, attempt `{record["run_attempt"]}`. SHA256SUMS SHA-256: `{sha(folder / "SHA256SUMS")}`.\n\nDownload the exact `candidate` artifact and review `candidate.json`, native records and checks. Follow `docs/releasing.md`; do not upload a private key. Artifacts expire in seven days.\n')


def candidate_run(run_id):
    commit = identity()
    item = api(f'/repos/{REPO}/actions/runs/{integer(run_id)}')
    require(item['repository']['id'] == REPO_ID and item['head_repository']['id'] == REPO_ID, 'candidate repository mismatch')
    require(item['head_sha'] == commit and item['head_branch'] == 'main' and item['event'] == 'workflow_dispatch' and item['path'] == '.github/workflows/release-candidate.yml', 'candidate workflow/source mismatch')
    require(item['status'] == 'completed' and item['conclusion'] == 'success', 'candidate not successful')
    require(item['run_attempt'] == 1, 'rerun candidate forbidden; create a fresh dispatch')
    artifacts = api(f'/repos/{REPO}/actions/runs/{integer(run_id)}/artifacts?per_page=100')
    require(artifacts['total_count'] <= 100, 'artifact pagination refused')
    selected = [a for a in artifacts['artifacts'] if a['name'] == 'candidate']
    require(len(selected) == 1 and not selected[0]['expired'] and re.fullmatch('sha256:[a-f0-9]{64}', selected[0].get('digest', '')), 'candidate artifact missing, expired or ambiguous')
    provenance = [a for a in artifacts['artifacts'] if a['name'] == 'provenance']
    require(len(provenance) == 1 and not provenance[0]['expired'] and re.fullmatch('sha256:[a-f0-9]{64}', provenance[0].get('digest', '')), 'provenance artifact missing or expired')
    output('artifact_id', selected[0]['id'])
    output('provenance_id', provenance[0]['id'])
    return item


def verify_packet(directory, run_id, manifest_sha, signed=False):
    folder = Path(directory)
    require((folder / 'candidate.json').is_file() and not (folder / 'candidate.json').is_symlink(), 'invalid candidate record')
    record = read_json(folder / 'candidate.json')
    ver = version(record['version'])
    commit = hex40(os.environ.get('GITHUB_SHA'))
    require(record['schema'] == 1 and record['repository'] == REPO and record['repository_id'] == REPO_ID, 'candidate repository/schema mismatch')
    require(record['source_sha'] == record['control_sha'] == commit, 'candidate source/control mismatch')
    require(record['run_id'] == integer(run_id) and record['run_attempt'] == 1, 'candidate replay/attempt mismatch')
    require(dt.datetime.fromisoformat(record['created_at']) <= now() + dt.timedelta(seconds=30) and now() < dt.datetime.fromisoformat(record['expires_at']) <= dt.datetime.fromisoformat(record['created_at']) + dt.timedelta(days=7, seconds=1), 'candidate expired or invalid time bound')
    allowed = packet_files(ver)
    qualification = {'release-qualification.json'} if signed and (folder / 'release-qualification.json').exists() else set()
    regular_files(folder, allowed | {'candidate.json', 'provenance.sigstore.json'} | ({'SHA256SUMS.sig'} if signed else set()) | qualification)
    require(set(record['files']) == allowed, 'candidate manifest set mismatch')
    for name, info in record['files'].items():
        path = folder / name
        require(info == {'sha256': sha(path), 'bytes': path.stat().st_size}, 'candidate asset digest/size mismatch: ' + name)
    require(re.fullmatch('[a-f0-9]{64}', manifest_sha or '') and sha(folder / 'SHA256SUMS') == manifest_sha, 'signing manifest identity mismatch')
    canonical = ''.join(f'{sha(folder / archive_name(ver, t))}  {archive_name(ver, t)}\n' for t in TARGETS)
    require((folder / 'SHA256SUMS').read_text() == canonical, 'noncanonical two-archive checksums')
    for src, name in [('scripts/install.sh', 'install.sh'), ('docs/install.md', 'INSTALL.md'), ('scripts/release/agent-guide.md', 'AGENTS.md'), ('scripts/release/agent-guide.md', 'CLAUDE.md')]:
        require(sha(ROOT / src) == sha(folder / name), 'installer/guide control source mismatch')
    for target in TARGETS:
        info = read_json(folder / f'build-{target}.json')
        require(info['source_sha'] == info['control_sha'] == commit and info['target'] == target and info['version'] == ver and info['run_id'] == integer(run_id) and info['run_attempt'] == 1, 'native build binding mismatch')
        require(info['sha256'] == sha(folder / archive_name(ver, target)), 'native digest mismatch')
        inspect_archive(folder / archive_name(ver, target), ver, target)
    admission = read_json(folder / 'admission.json')
    require(admission['source_sha'] == commit and admission['version'] == ver, 'admission source mismatch')
    validate_factory(admission['factory_qualification'], commit)
    verify_provenance(folder, commit)
    if signed:
        verify_signature(folder)
    if qualification:
        validate_qualification(read_json(folder / 'release-qualification.json'), record, manifest_sha)
    return record


def verify_provenance(folder, commit):
    # Verify GitHub certificate identity/source, not just user-controlled predicate fields.
    bundle = folder / 'provenance.sigstore.json'
    require(bundle.stat().st_size <= 4 * 1024 * 1024, 'provenance bundle exceeds limit')
    for name in ('candidate.json', 'SHA256SUMS', 'install.sh'):
        run(['gh', 'attestation', 'verify', str(folder / name), '--bundle', str(bundle),
             '--repo', REPO, '--signer-workflow', REPO + '/.github/workflows/release-candidate.yml',
             '--source-ref', 'refs/heads/main', '--source-digest', commit, '--signer-digest', commit,
             '--deny-self-hosted-runners', '--format', 'json'])
    # candidate.json binds every archive, build record, SBOM, admission and guide digest.


def verify_signature(folder):
    signature = folder / 'SHA256SUMS.sig'
    require(signature.is_file() and not signature.is_symlink() and 0 < signature.stat().st_size <= 4096, 'invalid public signature file')
    with tempfile.TemporaryDirectory() as temp:
        allowed = Path(temp) / 'allowed-signers'
        allowed.write_text(SIGNER + '\n')
        with (folder / 'SHA256SUMS').open() as manifest:
            result = subprocess.run(['ssh-keygen', '-Y', 'verify', '-f', str(allowed), '-I', 'rdy-release', '-n', 'rdy-release-v1', '-s', str(signature)], stdin=manifest, capture_output=True, text=True)
        require(result.returncode == 0, 'founder release signature refused')


def seal(directory, run_id, manifest_sha, encoded):
    require(len(encoded) <= 5500 and re.fullmatch('[A-Za-z0-9+/]*={0,2}', encoded or '') is not None, 'invalid signature input encoding')
    record = verify_packet(directory, run_id, manifest_sha)
    raw = base64.b64decode(encoded, validate=True)
    require(0 < len(raw) <= 4096 and raw.startswith(b'-----BEGIN SSH SIGNATURE-----\n') and raw.endswith(b'-----END SSH SIGNATURE-----\n'), 'invalid SSHSIG armor')
    sig = Path(directory) / 'SHA256SUMS.sig'
    with sig.open('xb') as stream:
        stream.write(raw)
    verify_signature(Path(directory))
    output('version', record['version'])


def acceptance(directory, run_id, manifest_sha, target):
    folder = Path(directory).resolve()
    record = verify_packet(folder, run_id, manifest_sha, signed=True)
    require(target in TARGETS, 'invalid native acceptance target')
    with tempfile.TemporaryDirectory(prefix='rdy-signed-install-') as temp:
        tmp = Path(temp)
        download = tmp / 'download'
        download.mkdir(mode=0o700)
        for name in (archive_name(record['version'], target), 'SHA256SUMS', 'SHA256SUMS.sig'):
            shutil.copyfile(folder / name, download / name)
        # New prefixes/disposable projects: the existing checker runs CLI, daemon and MCP.
        report = run(['cargo', 'run', '--locked', '--offline', '--release', '-p', 'rdy-qa', '--bin', 'qa-release-install', '--', '--download', str(download), '--signer-line', SIGNER, '--version', record['version'], '--commit', record['source_sha'], '--target', target, '--unpack', str(tmp / 'unpack'), '--prefix', str(tmp / ('rdy-' + record['version']))])
        json.loads(report)
        managed = tmp / 'managed'
        run(['sh', str(ROOT / 'scripts/install.sh'), '--version', record['version'], '--offline-dir', str(download), '--prefix', str(managed)])
        require(run([str(managed / 'bin/rdy'), '--version']) == 'rdy ' + record['version'] + ' (preview)', 'managed binary version mismatch')
        write_json(Path(os.environ.get('RUNNER_TEMP', tempfile.gettempdir())) / f'acceptance-{target}.json', {'candidate_run_id': integer(run_id), 'source_sha': record['source_sha'], 'target': target, 'manifest_sha256': manifest_sha, 'result': 'passed', 'existing_checker_report': json.loads(report), 'managed_installer': 'passed'})


def validate_qualification(data, candidate, manifest_sha):
    require(set(data) == {'schema', 'source_sha', 'candidate_run_id', 'manifest_sha256', 'finalization_run_id', 'finalization_attempt', 'native_results'}, 'release qualification fields mismatch')
    require(data['schema'] == 1 and data['source_sha'] == candidate['source_sha'] and data['candidate_run_id'] == candidate['run_id'] and data['manifest_sha256'] == manifest_sha, 'release qualification source mismatch')
    integer(data['finalization_run_id'])
    require(data['finalization_attempt'] == 1 and data['native_results'].keys() == set(TARGETS), 'release qualification target/attempt mismatch')
    for target, result in data['native_results'].items():
        require(set(result) == {'result', 'receipt_sha256'}, 'native qualification fields mismatch')
        require(result['result'] == 'passed' and re.fullmatch('[a-f0-9]{64}', result['receipt_sha256']), 'native qualification missing/nonpassing')


def validate_acceptance(report, target, candidate, manifest_sha):
    require(report['candidate_run_id'] == candidate['run_id'] and report['source_sha'] == candidate['source_sha'] and report['manifest_sha256'] == manifest_sha and report['target'] == target and report['result'] == 'passed' and report['managed_installer'] == 'passed', 'signed native acceptance report mismatch')


class StopRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def receipt_bytes(endpoint, expected_size, digest):
    """Bounded API download; bearer token never follows a storage redirect."""
    require(endpoint.startswith(f'/repos/{REPO}/') and not any(c in endpoint for c in ('?', '#')), 'invalid receipt endpoint')
    require(isinstance(expected_size, int) and 0 < expected_size <= 4 * 1024 * 1024 and re.fullmatch('sha256:[a-f0-9]{64}', digest or ''), 'invalid receipt size/digest')
    url = 'https://api.github.com' + endpoint
    headers = {'Accept': 'application/octet-stream', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'rdy-core-release'}
    if os.environ.get('GH_TOKEN'):
        headers['Authorization'] = 'Bearer ' + os.environ['GH_TOKEN']
    for _ in range(4):
        try:
            request = urllib.request.Request(url, headers=headers)
            with urllib.request.build_opener(StopRedirect()).open(request, timeout=60) as response:
                blob = response.read(expected_size + 1)
                require(len(blob) == expected_size and 'sha256:' + hashlib.sha256(blob).hexdigest() == digest, 'receipt download size/digest mismatch')
                return blob
        except urllib.error.HTTPError as exc:
            if exc.code not in (301, 302, 303, 307, 308):
                raise Refusal(f'receipt download refused (HTTP {exc.code})') from None
            url = exc.headers.get('Location', '')
            parsed = urllib.parse.urlsplit(url)
            host = parsed.hostname or ''
            require(parsed.scheme == 'https' and not parsed.username and not parsed.password and parsed.port in (None, 443) and (host == 'release-assets.githubusercontent.com' or host.endswith('.actions.githubusercontent.com') or host.endswith('.blob.core.windows.net')), 'unsafe receipt download redirect')
            headers = {'User-Agent': 'rdy-core-release'}
    raise Refusal('receipt download redirect limit')


def verify_prior_qualification(data, candidate, manifest_sha):
    """A failed publication can still have two successful native acceptance jobs."""
    validate_qualification(data, candidate, manifest_sha)
    run_id = integer(data['finalization_run_id'])
    item = api(f'/repos/{REPO}/actions/runs/{run_id}')
    require(item['repository']['id'] == REPO_ID and item['head_repository']['id'] == REPO_ID and item['head_sha'] == candidate['source_sha'] and item['head_branch'] == 'main' and item['event'] == 'workflow_dispatch' and item['path'] == '.github/workflows/release-publish.yml', 'prior qualification workflow/source mismatch')
    require(item['status'] == 'completed' and item['run_attempt'] == 1, 'prior qualification run incomplete or rerun')
    jobs = api(f'/repos/{REPO}/actions/runs/{run_id}/attempts/1/jobs?per_page=100')
    require(jobs['total_count'] <= 100, 'prior qualification job pagination')
    artifacts = api(f'/repos/{REPO}/actions/runs/{run_id}/artifacts?per_page=100')
    require(artifacts['total_count'] <= 100, 'prior qualification artifact pagination')
    for target in TARGETS:
        selected = [j for j in jobs['jobs'] if j['name'] == f'Signed native install ({target})']
        require(len(selected) == 1 and selected[0]['status'] == 'completed' and selected[0]['conclusion'] == 'success', 'prior native acceptance job missing/nonpassing')
        matches = [a for a in artifacts['artifacts'] if a['name'] == 'signed-install-' + target]
        require(len(matches) == 1 and not matches[0]['expired'], 'prior native receipt missing/expired')
        artifact = matches[0]
        blob = receipt_bytes(f'/repos/{REPO}/actions/artifacts/{integer(artifact["id"])}/zip', artifact['size_in_bytes'], artifact.get('digest'))
        with zipfile.ZipFile(io.BytesIO(blob)) as archive:
            members = archive.infolist()
            name = f'acceptance-{target}.json'
            require(len(members) == 1 and members[0].filename == name and not members[0].is_dir() and not members[0].flag_bits & 1 and 0 < members[0].file_size <= 4 * 1024 * 1024, 'invalid prior receipt archive')
            raw = archive.read(members[0])
        require(hashlib.sha256(raw).hexdigest() == data['native_results'][target]['receipt_sha256'], 'prior native receipt digest mismatch')
        with tempfile.TemporaryDirectory(prefix='rdy-prior-receipt-') as temp:
            path = Path(temp) / name
            path.write_bytes(raw)
            validate_acceptance(read_json(path), target, candidate, manifest_sha)


def qualify_release(directory, receipt_directory, candidate, manifest_sha, release=None):
    folder = Path(receipt_directory)
    regular_files(folder, {f'acceptance-{t}.json' for t in TARGETS})
    reports = {}
    for target in TARGETS:
        path = folder / f'acceptance-{target}.json'
        report = read_json(path)
        validate_acceptance(report, target, candidate, manifest_sha)
        reports[target] = {'result': 'passed', 'receipt_sha256': sha(path)}
    data = {'schema': 1, 'source_sha': candidate['source_sha'], 'candidate_run_id': candidate['run_id'], 'manifest_sha256': manifest_sha, 'finalization_run_id': integer(os.environ['GITHUB_RUN_ID']), 'finalization_attempt': integer(os.environ['GITHUB_RUN_ATTEMPT']), 'native_results': reports}
    validate_qualification(data, candidate, manifest_sha)
    path = Path(directory) / 'release-qualification.json'
    prior = [a for a in (release or {}).get('assets', []) if a['name'] == path.name]
    require(len(prior) <= 1, 'duplicate prior release qualification')
    if prior:
        asset = prior[0]
        require(asset.get('state') == 'uploaded', 'prior qualification upload incomplete')
        raw = receipt_bytes(f'/repos/{REPO}/releases/assets/{integer(asset["id"])}', asset['size'], asset.get('digest'))
        with tempfile.TemporaryDirectory(prefix='rdy-prior-qualification-') as temp:
            saved = Path(temp) / path.name
            saved.write_bytes(raw)
            verify_prior_qualification(read_json(saved), candidate, manifest_sha)
        if path.exists():
            require(path.read_bytes() == raw, 'local qualification differs from durable original; no overwrite')
        else:
            with path.open('xb') as stream:
                stream.write(raw)
        return
    if path.exists():
        require(read_json(path) == data, 'existing qualification record differs; no overwrite')
    else:
        write_json(path, data)


def release_assets_match(release, folder, exact=True):
    wanted = {p.name: p for p in folder.iterdir()}
    assets = release.get('assets', [])
    require(len({a['name'] for a in assets}) == len(assets), 'duplicate release assets')
    require(set(a['name'] for a in assets) <= set(wanted), 'foreign release asset')
    if exact:
        require(set(a['name'] for a in assets) == set(wanted), 'incomplete release asset set')
    for asset in assets:
        p = wanted[asset['name']]
        require(asset.get('state') == 'uploaded' and asset.get('size') == p.stat().st_size and asset.get('digest') == 'sha256:' + sha(p), 'remote release asset digest mismatch')


def publish(directory, run_id, manifest_sha, receipt_directory=None):
    """Only this command mutates GitHub; no candidate binary/build-script execution."""
    commit = identity()
    require(os.environ.get('CORE_RELEASE_CONTROLS_READY') == 'true', 'reviewed release environment is not activated')
    require(os.environ.get('GITHUB_RUN_ATTEMPT') == '1', 'rerun publisher refused; reconcile and dispatch explicitly')
    candidate_run(run_id)
    folder = Path(directory)
    record = verify_packet(folder, run_id, manifest_sha, signed=True)
    required_checks(commit)
    require(receipt_directory is not None, 'native acceptance receipts required')
    tag = 'v' + record['version']
    marker = f'<!-- rdy-candidate:{run_id}:{manifest_sha}:{commit} -->'
    release = api(f'/repos/{REPO}/releases/tags/{tag}', absent=True)
    # Drafts are not reliably returned by the tag route; resolve exact named drafts too.
    if release is None:
        listed = api(f'/repos/{REPO}/releases?per_page=100')
        require(len(listed) < 100, 'release reconciliation requires explicit pagination review')
        matches = [r for r in listed if r['tag_name'] == tag]
        require(len(matches) <= 1, 'ambiguous existing release')
        release = matches[0] if matches else None
    tag_ref = api(f'/repos/{REPO}/git/ref/tags/{tag}', absent=True)
    if tag_ref:
        require(tag_ref['object']['type'] == 'commit' and tag_ref['object']['sha'] == commit and release is not None, 'foreign/moving release tag')
    if release:
        require(marker in (release.get('body') or '') and release['target_commitish'] == commit and release['prerelease'], 'existing release identity mismatch; no overwrite')
    qualify_release(folder, receipt_directory, record, manifest_sha, release)
    if release:
        release_assets_match(release, folder, exact=not release['draft'])
        if not release['draft']:
            require(release.get('immutable') is True, 'existing published release is not immutable; no mutation attempted')
            output('release_id', release['id'])
            return
    else:
        body = f'{marker}\n\nPreview {record["version"]}. Source and release controls: `{commit}`.\n\nSigned archives support Linux x86-64 GNU and macOS ARM only. See INSTALL.md and candidate.json for native evidence and observed ABI. This preview does not promise Release 1 compatibility. Windows is not distributed.\n\nSHA256SUMS is signed by the offline founder key. Installer/guide/SBOM/provenance are separately bound evidence, not additional SSHSIG manifest entries.\n'
        release = api(f'/repos/{REPO}/releases', 'POST', {'tag_name': tag, 'target_commitish': commit, 'name': f'rdy {record["version"]} preview', 'body': body, 'draft': True, 'prerelease': True, 'make_latest': 'false'})
    present = {a['name'] for a in release['assets']}
    for path in sorted(folder.iterdir()):
        if path.name not in present:
            api(f'/repos/{REPO}/releases/{release["id"]}/assets?name={urllib.parse.quote(path.name)}', 'POST', upload=path)
    release = api(f'/repos/{REPO}/releases/{release["id"]}')
    release_assets_match(release, folder)
    require(release['draft'], 'release unexpectedly published during draft assembly')
    release = api(f'/repos/{REPO}/releases/{release["id"]}', 'PATCH', {'draft': False, 'prerelease': True, 'make_latest': 'false'})
    require(release.get('immutable') is True, 'published release immutability absent; report incident, never delete/recreate')
    release_assets_match(release, folder)
    output('release_id', release['id'])


class PublicRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        parsed = urllib.parse.urlsplit(newurl)
        require(parsed.scheme == 'https' and parsed.hostname in ('github.com', 'release-assets.githubusercontent.com') and not parsed.username and not parsed.password, 'unsafe public download redirect')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def public_acceptance(ver, directory, run_id, manifest_sha, target):
    version(ver)
    release = api(f'/repos/{REPO}/releases/tags/v{ver}')
    require(not release['draft'] and release['prerelease'] and release.get('immutable') is True and release['target_commitish'] == os.environ.get('GITHUB_SHA'), 'public release identity/immutability mismatch')
    expected = packet_files(ver) | {'candidate.json', 'SHA256SUMS.sig', 'provenance.sigstore.json', 'release-qualification.json'}
    require({a['name'] for a in release['assets']} == expected and len(release['assets']) == len(expected), 'public asset set mismatch')
    folder = Path(directory)
    folder.mkdir(mode=0o700)
    total = 0
    for asset in release['assets']:
        require(0 < asset['size'] <= MAX_FILE and re.fullmatch('sha256:[a-f0-9]{64}', asset.get('digest', '')), 'invalid public asset size/digest')
        total += asset['size']
        require(total <= MAX_PACKET, 'public release exceeds bound')
        url = f'https://github.com/{REPO}/releases/download/v{ver}/{asset["name"]}'
        require(asset['browser_download_url'] == url, 'foreign public download URL')
        request = urllib.request.Request(url, headers={'User-Agent': 'rdy-core-public-install-check'})
        with urllib.request.build_opener(PublicRedirect()).open(request, timeout=60) as response, (folder / asset['name']).open('xb') as stream:
            size = 0
            while True:
                blob = response.read(1024 * 1024)
                if not blob:
                    break
                size += len(blob)
                require(size <= asset['size'], 'public download exceeds expected size')
                stream.write(blob)
        require(size == asset['size'] and 'sha256:' + sha(folder / asset['name']) == asset['digest'], 'public download truncated or changed')
    acceptance(folder, run_id, manifest_sha, target)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('operation', choices=['start', 'record', 'assemble', 'lookup', 'seal', 'verify', 'accept', 'publish', 'public'])
    parser.add_argument('--version')
    parser.add_argument('--directory', default='packet')
    parser.add_argument('--target')
    parser.add_argument('--run-id')
    parser.add_argument('--manifest-sha')
    parser.add_argument('--acceptance-directory')
    args = parser.parse_args()
    if args.operation == 'start':
        require(os.environ.get('GITHUB_RUN_ATTEMPT') == '1', 'candidate rerun forbidden; create a fresh dispatch')
        start(args.version, args.directory)
    elif args.operation == 'record':
        build_record(args.version, args.target, args.directory)
    elif args.operation == 'assemble':
        assemble(args.version, args.directory)
    elif args.operation == 'lookup':
        candidate_run(args.run_id)
    elif args.operation == 'seal':
        seal(args.directory, args.run_id, args.manifest_sha, os.environ.get('PUBLIC_SSHSIG_BASE64', ''))
    elif args.operation == 'verify':
        verify_packet(args.directory, args.run_id, args.manifest_sha, signed=True)
    elif args.operation == 'accept':
        acceptance(args.directory, args.run_id, args.manifest_sha, args.target)
    elif args.operation == 'public':
        public_acceptance(args.version, args.directory, args.run_id, args.manifest_sha, args.target)
    else:
        publish(args.directory, args.run_id, args.manifest_sha, args.acceptance_directory)


if __name__ == '__main__':
    try:
        main()
    except (Refusal, ValueError, KeyError, TypeError, AttributeError, OSError, subprocess.CalledProcessError, tarfile.TarError) as exc:
        # GitHub credentials are never arguments or printed provider bodies.
        print('release refused: ' + str(exc) if isinstance(exc, Refusal) else 'release refused: malformed input or failed command; inspect bounded step evidence', file=sys.stderr)
        sys.exit(1)
