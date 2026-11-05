#!/usr/bin/env python3
"""The release caller's jobs: reconcile, reserve, the private draft, the genuine checks and promotion.

Each subcommand is one job of .github/workflows/release.yml, in this order:

  eligibility     Reconciles first. An exactly matching published release is a terminal, read-only
                  observation. Otherwise it authenticates a new version or an existing reservation.
  reserve         The only job that writes the reservation. It holds no alert read: it needs the eligibility
                  job's success in this exact run attempt.
  draft           Copies the verified signature and the reserved bytes into the one private draft. It uploads
                  a missing asset once, accepts an existing asset only by its exact bytes, then downloads every
                  asset anew. It never removes or replaces anything.
  genuine         Runs the genuine checker over the draft's exact bytes on one native target.
  final-tag       Checks everything again, then creates v<V> at S once. An existing tag at S is a recorded retry:
                  no write, and its evidence says so.
  promote         Checks everything again through the publisher App's view, then flips that same draft id.
  check-published Reads every public asset with no credentials and checks exact bytes. A failure here is an
                  incident: nothing replaces a published byte.

The scripts run from the trusted checkout at the caller's literal signer revision, never from candidate data.
Failures are one `release controller: <reason>` line on stderr and exit 1; invalid arguments exit 2.
"""
import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import shutil
import sys
import urllib.parse
import urllib.request
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402
import controller as ctl  # noqa: E402
import eligibility as elig  # noqa: E402
import github as gh  # noqa: E402
import rehearse  # noqa: E402
import sign  # noqa: E402

ControllerError = cand.ControllerError
Refused = ctl.Refused
INCOMPLETE, MISMATCH, UNAUTHORIZED, CONFLICT = ctl.INCOMPLETE, ctl.MISMATCH, ctl.UNAUTHORIZED, ctl.CONFLICT

# A caller that is not activated holds this literal. It resolves to no workflow, so GitHub starts no job, and
# every subcommand here refuses it. The pin commit replaces every occurrence with one reviewed full SHA.
UNPINNED = '0' * 40

ELIGIBILITY_JOB = 'Release eligibility'
DRAFT_JOB = 'Prepare private draft'
TAG_JOB = 'Create final tag'
PROMOTE_JOB = 'Promote verified draft'
GENUINE_JOBS = {target: 'Genuine check (' + target + ')' for target in cand.TARGETS}

SIGNATURE_ARTIFACT = 'rdy-signature-a'
SIGNATURE_MEMBER = 'SHA256SUMS.sig'
AUTHORIZATION_MEMBER = 'authorization.json'
DRAFT_RECEIPT_ARTIFACT = 'draft-receipt-a'
DRAFT_RECEIPT_MEMBER = 'draft-receipt.json'
DRAFT_RECEIPT_RE = re.compile(r'draft-receipt-a[1-9][0-9]*')
DRAFT_ASSETS_ARTIFACT = 'draft-assets-a'
GENUINE_ARTIFACT = 'genuine-'
GENUINE_MEMBER = 'genuine-receipt.json'
FINAL_TAG_ARTIFACT = 'final-tag-evidence-a'
FINAL_TAG_MEMBER = 'final-tag-evidence.json'

GUIDES = ('AGENTS.md', 'CLAUDE.md')
MANIFEST = 'SHA256SUMS'
SIGNATURE = 'SHA256SUMS.sig'
SIGNATURE_MAX = ctl.SIGNATURE_MAX
ASSET_MAX = cand.ARCHIVE_MAX
CRATE_MAX = 64 * cand.MIB
CRATE_COUNT = 2
ASSET_NAME_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,127}')
CRATE_HOST = 'https://static.crates.io/crates/'
UPLOADS = 'https://uploads.github.com'
PUBLIC_DOWNLOADS = 'https://github.com/' + cand.REPOSITORY + '/releases/download/'
REDIRECTS_MAX = 3

DRAFT_RECEIPT_KEYS = ('schema', 'version', 'commit', 'tuple_sha256', 'release_id', 'notes_sha256', 'assets')
ASSET_RECEIPT_KEYS = ('asset_id', 'name', 'sha256', 'size')
GENUINE_KEYS = ('schema', 'version', 'commit', 'target', 'tuple_sha256', 'manifest_sha256', 'signature_sha256',
                'release_id', 'draft_receipt_sha256', 'report_sha256', 'checks', 'skipped')
PROMOTION_KEYS = ('schema', 'version', 'commit', 'tuple_sha256', 'release_id', 'notes_sha256', 'signer_line', 'assets')

# What the founder's readback must say before the protected jobs act. It holds names and states, never a
# credential. Contents:write also permits pushes and ref changes, so this states the rules that bound it.
TAG_RULE = {'update': 'denied', 'delete': 'denied', 'publisher_bypass': False}
SETTINGS_READBACK = {
    'schema': 1,
    'repository_id': str(gh.REPOSITORY_ID),
    'publisher': {'installed_repositories': [cand.REPOSITORY], 'permissions': {'contents': 'write', 'metadata': 'read'},
                  'signing_grant': False},
    'environment': 'release',
    'rulesets': {'main': {'force_push': 'denied', 'delete': 'denied', 'publisher_bypass': False},
                 'tags': {'v*': TAG_RULE, 'candidate/*': TAG_RULE, 'reserve/*': TAG_RULE}},
}


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _canon(value):
    return cand.canonical(value)


def _lifted(call, *args):
    try:
        return call(*args)
    except elig.Refused as error:
        raise ctl.lift(error) from None


def check_pin(signer_sha):
    """The caller's literal, before anything else happens."""
    if not (isinstance(signer_sha, str) and cand.COMMIT_RE.fullmatch(signer_sha)) or signer_sha == UNPINNED:
        raise Refused(UNAUTHORIZED, 'the release caller has no literal signer revision')
    return signer_sha


def _check_release_pin(release, signer_sha):
    if release.signer_workflow_sha != signer_sha:
        raise Refused(UNAUTHORIZED, 'the policy names another signer revision')


def check_settings_readback(text):
    """The sanitized readback that activation requires. Anything else, or none, stops the job."""
    try:
        value = cand.loads(text.encode('utf-8'))
    except ControllerError:
        value = None
    if value != SETTINGS_READBACK:
        raise Refused(UNAUTHORIZED, 'the publisher settings readback does not match the qualified settings')


def check_anonymous_env(environ):
    """A public check sees what anyone sees. It refuses to start beside a credential."""
    if rehearse.forbidden(environ) or 'RDY_PUBLISHER_TOKEN' in environ:
        raise ControllerError('the public checks must run without credentials')


# ---------------------------------------------------------------------------------------------
# Transports and the release API
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class AssetFact(object):
    asset_id: int
    name: str
    size: int
    state: str
    digest: object


@dataclass(frozen=True)
class DraftFact(object):
    release_id: int
    tag_name: str
    target_commitish: str
    name: object
    draft: bool
    prerelease: bool
    body: str
    assets: tuple


class Transport(gh.UrllibTransport):
    """The token's reads and writes. Asset bytes come through the API's own redirect, with no token past it."""

    def fetch_asset(self, path, sink, limit):
        request = urllib.request.Request(self.api + path, method='GET')
        request.add_header('Accept', 'application/octet-stream')
        request.add_header('X-GitHub-Api-Version', '2022-11-28')
        request.add_header('Authorization', 'Bearer ' + self._token)
        request.add_header('User-Agent', 'rdy-release-controller')
        buffer = gh._Buffer()
        status, headers = self._open(request, buffer, limit)
        if status == 302:
            return self.download(headers.get('location', ''), sink, limit)
        sink.write(buffer.data())
        return status

    def upload(self, url, path, limit):
        size = os.path.getsize(str(path))
        if size > limit or not url.startswith(UPLOADS + '/'):
            raise gh.TransportFailure()
        with open(str(path), 'rb') as handle:
            request = urllib.request.Request(url, data=handle, method='POST')
            request.add_header('Accept', 'application/vnd.github+json')
            request.add_header('X-GitHub-Api-Version', '2022-11-28')
            request.add_header('Authorization', 'Bearer ' + self._token)
            request.add_header('User-Agent', 'rdy-release-controller')
            request.add_header('Content-Type', 'application/octet-stream')
            request.add_header('Content-Length', str(size))
            buffer = gh._Buffer()
            status, headers = self._open(request, buffer, gh.BODY_MAX)
        return gh.Response(status, headers, buffer.data())


class AnonymousTransport(gh.UrllibTransport):
    """What anyone reads: no token, no proxy from the environment, no authentication handler and only GET."""

    def __init__(self, api=gh.API, deadline=gh.REQUEST_DEADLINE):
        self.api = api
        self.deadline = deadline
        self._token = None
        self._opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), gh._NoRedirect)

    def request(self, method, path, body=None, limit=None):
        if method != 'GET' or body is not None:
            raise ControllerError('the anonymous reader only reads')
        limit = gh.BODY_MAX if limit is None else limit
        request = urllib.request.Request(self.api + path, method='GET')
        request.add_header('Accept', 'application/vnd.github+json')
        request.add_header('X-GitHub-Api-Version', '2022-11-28')
        request.add_header('User-Agent', 'rdy-release-controller')
        buffer = gh._Buffer()
        status, headers = self._open(request, buffer, limit)
        return gh.Response(status, headers, buffer.data())

    def public_get(self, url, sink, limit):
        """A public URL with at most REDIRECTS_MAX HTTPS redirects. Returns the final status."""
        for _ in range(REDIRECTS_MAX + 1):
            if not url.startswith('https://'):
                raise gh.TransportFailure()
            request = urllib.request.Request(url, method='GET')
            request.add_header('User-Agent', 'rdy-release-controller')
            buffer = gh._Buffer()
            status, headers = self._open(request, buffer, limit)
            if status not in (301, 302, 303, 307, 308):
                sink.write(buffer.data())
                return status
            url = headers.get('location', '')
        raise gh.TransportFailure()


class Releases(sign.LiveFacts):
    """`sign.LiveFacts` plus the release API this path names. The token's permissions bound what succeeds."""

    @staticmethod
    def _asset(operation, value):
        size = value.get('size') if isinstance(value, dict) else None
        if not cand.is_int(size, 0):
            raise gh.Malformed(operation, 'the asset size is malformed')
        return AssetFact(gh.field(value, 'id', int, operation, 'asset id'),
                         gh.field(value, 'name', str, operation, 'asset name'), size,
                         gh.field(value, 'state', str, operation, 'asset state'),
                         gh.field(value, 'digest', str, operation, 'asset digest', nullable=True)
                         if 'digest' in value else None)

    def _full(self, operation, value):
        assets = value.get('assets') if isinstance(value, dict) else None
        if not (isinstance(assets, list) and all(isinstance(a, dict) for a in assets)):
            raise gh.Malformed(operation, 'the release assets are malformed')
        body = gh.field(value, 'body', str, operation, 'release body', nullable=True)
        return DraftFact(gh.field(value, 'id', int, operation, 'release id'),
                         gh.field(value, 'tag_name', str, operation, 'tag name'),
                         gh.field(value, 'target_commitish', str, operation, 'release target'),
                         gh.field(value, 'name', str, operation, 'release name', nullable=True),
                         gh.field(value, 'draft', bool, operation, 'draft flag'),
                         gh.field(value, 'prerelease', bool, operation, 'prerelease flag'),
                         '' if body is None else body,
                         tuple(self._asset(operation, a) for a in assets))

    def full_releases(self):
        operation = 'releases'
        return [self._full(operation, row) for row in self._pages(operation, self.root + '/releases')]

    def release_full(self, release_id):
        operation = 'release'
        gh.need_id(release_id, 'release id')
        value, _ = self._object(operation, self.root + '/releases/' + str(release_id))
        fact = self._full(operation, value)
        if fact.release_id != release_id:
            raise gh.Malformed(operation, 'the answer is not release ' + str(release_id))
        return fact

    def public_release(self, tag):
        operation = 'release'
        value, _ = self._object(operation, self.root + '/releases/tags/' + urllib.parse.quote(tag, safe=''))
        fact = self._full(operation, value)
        if fact.tag_name != tag:
            raise gh.Malformed(operation, 'the answer is not release ' + tag)
        return fact

    def create_draft(self, tag, target, name, body):
        """The one draft. Its answer must be the exact request."""
        operation = 'create draft'
        if not (isinstance(tag, str) and elig.TAG_RE.fullmatch(tag)):
            raise ControllerError('the release tag is not v<0.N.0>')
        gh.need_commit(target, 'release target')
        value, _ = self._mutate(operation, self.root + '/releases',
                                {'tag_name': tag, 'target_commitish': target, 'name': name, 'body': body, 'draft': True,
                                 'prerelease': True})
        try:
            fact = self._full(operation, value)
        except gh.Malformed:
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')
        if (fact.tag_name, fact.target_commitish, fact.draft, fact.prerelease, fact.body) != (tag, target, True, True, body):
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')
        return fact

    def upload_asset(self, release_id, name, path):
        """One POST of one file. GitHub refuses a duplicate name; a refusal or timeout reads back, never clobbers."""
        operation = 'upload asset'
        if not (isinstance(name, str) and ASSET_NAME_RE.fullmatch(name)):
            raise ControllerError('the asset name is not allowed')
        self.repository()
        url = UPLOADS + self.root + '/releases/' + str(gh.need_id(release_id, 'release id')) + '/assets?name=' + name
        try:
            response = self.transport.upload(url, path, ASSET_MAX)
        except gh.Timeout:
            raise gh.ReconcileRequired(operation, 'timeout')
        except gh.TransportFailure:
            raise gh.ReconcileRequired(operation, 'transport failure')
        if response.status != 201:
            raise gh.ReconcileRequired(operation, 'HTTP ' + str(response.status), response.status)
        try:
            return self._asset(operation, self._json(operation, response))
        except gh.Malformed:
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')

    def _save(self, operation, out, limit, fetch):
        """Runs `fetch(sink)` into the new file `out`. A failure leaves no file."""
        out = Path(out)
        digest = hashlib.sha256()
        cand.write_new(out, b'', 0o600)
        try:
            with open(str(out), 'wb') as handle:
                sink = gh._HashingSink(handle, digest, limit)
                try:
                    status = fetch(sink)
                except gh.Timeout:
                    raise gh.Uncertain(operation, 'outcome unknown (timeout)')
                except gh.TransportFailure:
                    raise gh.Uncertain(operation, 'outcome unknown (transport failure)')
            if status != 200:
                raise gh.StatusError(operation, 'HTTP ' + str(status), status)
            if sink.size > limit:
                raise gh.Malformed(operation, 'larger than ' + str(limit) + ' bytes')
        except BaseException:
            out.unlink()
            raise
        return digest.hexdigest(), sink.size

    def download_asset(self, asset, out, limit):
        """The current bytes of one draft asset, through the authenticated API."""
        self.repository()
        path = self.root + '/releases/assets/' + str(gh.need_id(asset.asset_id, 'asset id'))
        return self._save('asset download', out, limit, lambda sink: self.transport.fetch_asset(path, sink, limit))

    def public_asset(self, tag, name, out, limit):
        """The public bytes of one asset, as anyone downloads them."""
        if not (isinstance(name, str) and ASSET_NAME_RE.fullmatch(name)):
            raise ControllerError('the asset name is not allowed')
        url = PUBLIC_DOWNLOADS + urllib.parse.quote(tag, safe='') + '/' + name
        return self._save('public asset', out, limit, lambda sink: self.transport.public_get(url, sink, limit))

    def create_final_tag(self, version, commit):
        """`refs/tags/v<V>` at a commit, once."""
        operation = 'create final tag'
        tag = 'v' + str(version)
        if not elig.TAG_RE.fullmatch(tag):
            raise ControllerError('the release tag is not v<0.N.0>')
        gh.need_commit(commit, 'release commit')
        ref = 'refs/tags/' + tag
        value, digest = self._mutate(operation, self.root + '/git/refs', {'ref': ref, 'sha': commit})
        try:
            fact = self._ref(operation, value, digest, ref)
        except gh.Malformed:
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')
        if (fact.object_type, fact.object_sha) != ('commit', commit):
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')
        return fact

    def publish_draft(self, release_id):
        """The promotion: the same draft id, with only its draft flag changed."""
        operation = 'publish draft'
        gh.need_id(release_id, 'release id')
        self.repository()
        path = self.root + '/releases/' + str(release_id)
        try:
            response = self.transport.request('PATCH', path, b'{"draft":false}', gh.BODY_MAX)
        except gh.Timeout:
            raise gh.ReconcileRequired(operation, 'timeout')
        except gh.TransportFailure:
            raise gh.ReconcileRequired(operation, 'transport failure')
        if response.status in (409, 422, 429) or response.status >= 500:
            raise gh.ReconcileRequired(operation, 'HTTP ' + str(response.status), response.status)
        if response.status != 200:
            self._refuse(operation, response.status, read=False)
        try:
            fact = self._full(operation, self._json(operation, response))
        except gh.Malformed:
            raise gh.ReconcileRequired(operation, 'the answer is not JSON')
        if fact.release_id != release_id or fact.draft:
            raise gh.ReconcileRequired(operation, 'the answer differs from the request')
        return fact


def publisher_client(environ, work, verifier):
    """The publisher App's client. There is no other token for a promotion, and no fallback."""
    token = environ.get('RDY_PUBLISHER_TOKEN', '')
    if not token:
        raise ControllerError('the promotion has no publisher App token')
    return Releases(Transport(token), work, verifier)


class DraftView(object):
    """The facts as the publisher sees them: a read-only workflow token cannot list a draft."""

    def __init__(self, facts, publisher):
        self._facts = facts
        self._publisher = publisher

    def __getattr__(self, name):
        return getattr(self._facts, name)

    def releases(self):
        return self._publisher.releases()


# ---------------------------------------------------------------------------------------------
# The expected asset set
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Want(object):
    """An asset's exact digest and size."""
    sha256: object
    size: object


def _locked_checksums(data):
    """<name>-<version>.crate -> checksum, from Cargo.lock's own [[package]] entries."""
    found = {}
    for block in data.decode('utf-8', 'replace').split('[[package]]')[1:]:
        fields = dict(re.findall(r'^(name|version|checksum) = "([^"\n]*)"$', block, re.M))
        if {'name', 'version', 'checksum'} <= set(fields):
            found[fields['name'] + '-' + fields['version'] + '.crate'] = fields['checksum']
    return found


def _source_crates(facts, release):
    """Both builds' identical source crates, from the authenticated candidate record, against Cargo.lock at S."""
    run_id, _, artifact_id, _ = release.candidate_run
    artifact = facts.artifact(artifact_id, run_id)
    record = cand.load_candidate(facts.artifact_member(artifact, elig.CANDIDATE_MEMBER, cand.RECORD_MAX))
    lists = [b['sources'] for b in record['builds']]
    if not (lists and all(entry == lists[0] for entry in lists) and len(lists[0]) == CRATE_COUNT):
        raise Refused(MISMATCH, 'the release does not name exactly two source crates')
    locked = _locked_checksums(facts.contents('Cargo.lock', release.commit, gh.CONTENTS_MAX).data)
    crates = {}
    for entry in lists[0]:
        name = entry['name']
        if not (cand.SOURCE_NAME_RE.fullmatch(name) and cand.is_int(entry['size'], 1) and entry['size'] <= CRATE_MAX
                and isinstance(entry['sha256'], str) and cand.SHA256_RE.fullmatch(entry['sha256'])):
            raise Refused(MISMATCH, 'the release does not name exactly two source crates')
        if locked.get(name) != entry['sha256']:
            raise Refused(MISMATCH, 'the source crate ' + name + ' is not in Cargo.lock at the release commit')
        crates[name] = Want(entry['sha256'], entry['size'])
    return crates


def wanted_assets(facts, release, signature=None):
    """The exact public asset set of this release: name -> Want, and the bytes of the small committed files.
    The signature's Want is its exact bytes when they are known, else None: its truth is the SSHSIG check."""
    manifest = cand.manifest_bytes(release.tuple_value()['artifacts'])
    if _sha(manifest) != release.manifest_sha256:
        raise Refused(MISMATCH, 'the reserved manifest is not the reserved archives')
    wants = {name: Want(digest, size) for name, digest, size in release.artifacts}
    wants[MANIFEST] = Want(release.manifest_sha256, len(manifest))
    wants[SIGNATURE] = Want(None, None) if signature is None else Want(_sha(signature), len(signature))
    files = {MANIFEST: manifest}
    for name in GUIDES:
        files[name] = facts.contents('release/' + name, release.commit, elig.SOURCE_FILE_MAX).data
    if release.first_machine_tag == 'v' + release.version:
        stem = 'transition-' + release.first_machine_tag + '.json'
        files[stem] = facts.contents('release-keys/' + stem, release.policy_commit, sign.KEY_FILE_MAX).data
        files[stem + '.sig'] = facts.contents('release-keys/' + stem + '.sig', release.policy_commit, sign.KEY_FILE_MAX).data
        if _sha(files[stem]) != release.transition_sha256:
            raise Refused(MISMATCH, 'the published transition is not the reserved transition')
    for name, data in files.items():
        wants[name] = Want(_sha(data), len(data))
    wants.update(_source_crates(facts, release))
    return wants, files


def _notes(facts, release):
    data = facts.contents('release/notes.md', release.commit, elig.SOURCE_FILE_MAX).data
    try:
        return data.decode('utf-8')
    except UnicodeDecodeError:
        raise Refused(MISMATCH, 'release/notes.md is not UTF-8')


# ---------------------------------------------------------------------------------------------
# Reconcile and reserve
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Reconciled(object):
    """`state` is `ready` or `already_published_matching`, and the second one is terminal."""
    state: str
    mode: object
    terminal: object


class PublishedVerifier(object):
    """The exact-byte proof behind a terminal observation: public bytes, read with no credentials, against the
    original tuple and the version's signer. Missing proof raises, which is an incomplete reconciliation."""

    def __init__(self, public, facts, verifier, work):
        self.public = public
        self.facts = facts
        self.verifier = verifier
        self.work = Path(work)

    def _read(self, tag, name, limit):
        folder = self.work / ('public-' + _sha((tag + name).encode())[:16])
        shutil.rmtree(str(folder), ignore_errors=True)
        folder.mkdir(mode=0o700, parents=True)
        path = folder / 'bytes'
        try:
            digest, size = self.public.public_asset(tag, name, path, limit)
            data = path.read_bytes() if limit <= SIGNATURE_MAX else None
        finally:
            shutil.rmtree(str(folder), ignore_errors=True)
        return digest, size, data

    def verify_published(self, reconciliation):
        version = reconciliation.version
        tag = 'v' + version
        wants, files = wanted_assets(self.facts, reconciliation)
        release = self.public.public_release(tag)
        names = [a.name for a in release.assets]
        complete = sorted(names) == sorted(wants)
        try:
            _, _, signature = self._read(tag, SIGNATURE, SIGNATURE_MAX)
        except ControllerError:
            raise ControllerError('the published signature is missing')
        try:
            published_manifest, _, _ = self._read(tag, MANIFEST, cand.MANIFEST_MAX)
        except ControllerError:
            raise ControllerError('the published manifest is missing')
        archives = []
        for name, _, _ in reconciliation.artifacts:
            try:
                digest, size, _ = self._read(tag, name, ASSET_MAX)
            except ControllerError:
                raise ControllerError('the published archive ' + name + ' is missing')
            archives.append((name, digest, size))
        checked = {SIGNATURE, MANIFEST} | {a[0] for a in archives}
        for name in names:
            if complete and name not in checked:
                digest, size, _ = self._read(tag, name, ASSET_MAX)
                complete = (digest, size) == (wants[name].sha256, wants[name].size)
        trust = self.facts.release_trust(version, reconciliation.policy_commit, False)
        good = self.verifier.verify(signature, files[MANIFEST], trust['signer_line'], ctl.PRINCIPAL, ctl.NAMESPACE) is True
        return ctl.PublishedBytes(
            release_id=release.release_id, tag_name=release.tag_name, commit=_final_tag_commit(self.public, version),
            draft=release.draft, manifest_sha256=published_manifest, archives=tuple(archives),
            notes_sha256=_sha(release.body.encode('utf-8')), signature_sha256=_sha(signature),
            fingerprint=trust['fingerprint'], principal=ctl.PRINCIPAL, namespace=ctl.NAMESPACE, signature_verified=good,
            assets_complete=complete)


def reconcile(request, facts, public, verifier, signer_sha, work):
    """The eligibility job. Published and exact is terminal, with zero Sign and zero mutation; a historical
    release needs no current pin or current eligibility. Anything unfinished needs both."""
    check_pin(signer_sha)
    try:
        facts.ref('tags/reserve/v' + request.version)
    except gh.Absent:
        release = _lifted(elig.qualify_new, request, facts)
        _check_release_pin(release, signer_sha)
        return Reconciled('ready', 'new', None)
    reconciliation = _lifted(elig.authenticate_published, request, facts)
    if reconciliation.publication.state == 'published':
        observer = ctl.Controller(request, facts, None, None, None, None, None, None,
                                  published_verifier=PublishedVerifier(public, facts, verifier, work))
        return Reconciled('already_published_matching', None, observer.recover(reconciliation.tuple_bytes))
    release = _lifted(elig.preliminary_recovery, request, facts)
    _check_release_pin(release, signer_sha)
    return Reconciled('ready', 'recovery', None)


def _named_jobs(facts, run_id, attempt, name):
    # A matrix job is `<name> (<value>)`: it never counts as the one job of that name.
    return [j for j in facts.attempt_jobs(run_id, attempt) if j.name == name or j.name.startswith(name + ' (')]


def _job_passed(facts, run_id, attempt, name, commit):
    jobs = [j for j in facts.attempt_jobs(run_id, attempt) if j.name == name]
    if not (len(jobs) == 1 and (jobs[0].status, jobs[0].conclusion) == ('completed', 'success')
            and jobs[0].head_sha == commit):
        raise Refused(INCOMPLETE, 'the ' + name + ' job did not pass')
    return jobs[0]


def inherited_alerts(request, facts, run_id, attempt):
    """The reserve job has contents:write and actions:read only. The alert read belongs to the eligibility job,
    which ran on this commit in this exact attempt. Its success is an earlier observation for the reservation
    bookkeeping of this request, not a fresh read: the signer reads the alerts again before a new Sign."""
    if (request.dispatch_run_id, request.dispatch_run_attempt) != (run_id, attempt):
        raise Refused(INCOMPLETE, 'the request is not this run attempt')
    try:
        run = facts.run_attempt(run_id, attempt)
        job = _job_passed(facts, run_id, attempt, ELIGIBILITY_JOB, run.head_sha)
    except Refused:
        raise
    except ControllerError:
        raise Refused(INCOMPLETE, 'the eligibility job is unavailable')
    return elig.InheritedAlerts(request, run.head_sha, run.path, run_id, attempt, job.job_id)


def reserve_tuple(request, facts, run_id, attempt, signer_sha):
    """The reserve job. It writes only the annotated tag and its ref, through `create_reservation`, and returns
    the canonical receipt for the signer job."""
    check_pin(signer_sha)
    running = _named_jobs(facts, run_id, attempt, ctl.RESERVE_JOB)
    if len(running) != 1 or running[0].name != ctl.RESERVE_JOB or running[0].status != 'in_progress':
        raise ControllerError('this run attempt has no single running reserve job')
    holder = ctl.Holder(run_id, attempt, running[0].job_id)
    try:
        facts.ref('tags/reserve/v' + request.version)
    except gh.Absent:
        preliminary = _lifted(elig.qualify_new, request, facts, inherited_alerts(request, facts, run_id, attempt))
    else:
        preliminary = _lifted(elig.preliminary_recovery, request, facts)
    _check_release_pin(preliminary, signer_sha)
    return ctl.create_reservation(facts, preliminary, holder)


# ---------------------------------------------------------------------------------------------
# Transfers between jobs
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Transfers(object):
    """The artifact ids that earlier jobs of this attempt reported. Each is authenticated again on use."""
    signature_id: int
    signature_digest: str
    draft_receipt_id: object = None
    genuine: object = None


def _signature_transfer(facts, release, transfers, run_id, attempt, verifier, signer_line):
    """The signer job's real bytes and proof, authenticated as this attempt's artifact, then verified offline."""
    try:
        artifact = facts.artifact(transfers.signature_id, run_id)
    except ControllerError:
        raise Refused(INCOMPLETE, 'the signature artifact is unavailable')
    if artifact.name != SIGNATURE_ARTIFACT + str(attempt) or artifact.head_sha != release.commit:
        raise Refused(MISMATCH, 'the signature artifact is not from this run attempt')
    if artifact.digest != transfers.signature_digest:
        raise Refused(MISMATCH, 'the signature artifact is not the one the signer job uploaded')
    try:
        signature = facts.artifact_member(artifact, SIGNATURE_MEMBER, SIGNATURE_MAX)
    except ControllerError:
        raise Refused(INCOMPLETE, 'the transferred signature is unavailable')
    try:
        authorization = facts.artifact_member(artifact, AUTHORIZATION_MEMBER, cand.RECORD_MAX)
    except ControllerError:
        raise Refused(MISMATCH, 'the signer authorization is unavailable')
    try:
        value = cand.closed(cand.loads(authorization), ctl.AUTHORIZATION_KEYS, 'the signer authorization')
    except ControllerError:
        raise Refused(MISMATCH, 'the signer authorization is malformed')
    if (value['version'], value['tuple_sha256']) != (release.version, release.tuple_sha256):
        raise Refused(MISMATCH, 'the signer authorization is not this tuple')
    if value['signature_sha256'] != _sha(signature):
        raise Refused(MISMATCH, 'the transferred signature is not the authorized signature')
    manifest = cand.manifest_bytes(release.tuple_value()['artifacts'])
    if verifier.verify(signature, manifest, signer_line, ctl.PRINCIPAL, ctl.NAMESPACE) is not True:
        raise Refused(MISMATCH, 'the transferred signature does not verify')
    return signature, authorization


def _member(facts, artifact_id, run_id, attempt, prefix, member, what, release):
    try:
        artifact = facts.artifact(artifact_id, run_id)
    except ControllerError:
        raise Refused(INCOMPLETE, 'the ' + what + ' artifact is unavailable')
    if artifact.name != prefix + str(attempt) or artifact.head_sha != release.commit:
        raise Refused(MISMATCH, 'the ' + what + ' artifact is not from this run attempt')
    try:
        return facts.artifact_member(artifact, member, cand.RECORD_MAX)
    except ControllerError:
        raise Refused(INCOMPLETE, 'the ' + what + ' artifact is unavailable')


def _parse_draft_receipt(raw):
    try:
        value = cand.closed(cand.loads(raw), DRAFT_RECEIPT_KEYS, 'the draft receipt')
        for entry in value['assets']:
            cand.closed(entry, ASSET_RECEIPT_KEYS, 'a draft receipt asset')
    except (ControllerError, TypeError):
        raise Refused(MISMATCH, 'the draft receipt is malformed')
    return value


def _draft_receipt(facts, release, transfers, run_id, attempt):
    _job_passed(facts, run_id, attempt, DRAFT_JOB, release.commit)
    raw = _member(facts, transfers.draft_receipt_id, run_id, attempt, DRAFT_RECEIPT_ARTIFACT, DRAFT_RECEIPT_MEMBER,
                  'draft receipt', release)
    value = _parse_draft_receipt(raw)
    if (value['version'], value['commit'], value['tuple_sha256']) != (release.version, release.commit, release.tuple_sha256):
        raise Refused(MISMATCH, 'the draft receipt is not this release')
    return value, raw


def _prior_receipts(facts, version):
    """Every readable draft receipt of every run of this version's caller. An expired artifact says nothing. An
    unreadable one stops the release."""
    found = []
    for run_id, _ in facts.release_runs(version):
        for artifact in facts.run_artifacts(run_id):
            if artifact.expired or not DRAFT_RECEIPT_RE.fullmatch(artifact.name):
                continue
            found.append(_parse_draft_receipt(facts.artifact_member(artifact, DRAFT_RECEIPT_MEMBER, cand.RECORD_MAX)))
    return found


def _check_prior_receipts(facts, version, current):
    """The draft and its bytes never change, so each prior receipt must equal this one."""
    if any(prior != current for prior in _prior_receipts(facts, version)):
        raise Refused(CONFLICT, 'the draft changed after a prior receipt')


# ---------------------------------------------------------------------------------------------
# The draft
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Plan(object):
    version: str
    commit: str
    tuple_sha256: str
    notes: str
    wants: dict
    folder: Path


@dataclass(frozen=True)
class DraftResult(object):
    draft: DraftFact
    receipt: bytes
    folder: Path


def _gather(facts, request, release, signature, fetch_crate, work):
    """Every asset's bytes in one private folder, each checked before it can reach the draft."""
    wants, files = wanted_assets(facts, release, signature)
    candidate = Path(work) / 'candidate'
    sign.prepare_candidate(facts, request, release, candidate)
    folder = Path(work) / 'plan'
    folder.mkdir(mode=0o700)
    for name, _, _ in release.artifacts:
        shutil.move(str(candidate / name), str(folder / name))
    cand.write_new(folder / SIGNATURE, signature, 0o600)
    for name, data in files.items():
        cand.write_new(folder / name, data, 0o600)
    for name, want in wants.items():
        path = folder / name
        if not path.exists():
            data = fetch_crate(name)
            if (_sha(data), len(data)) != (want.sha256, want.size):
                raise Refused(MISMATCH, 'the source crate ' + name + ' is not its reserved bytes')
            cand.write_new(path, data, 0o600)
    return Plan(release.version, release.commit, release.tuple_sha256, _notes(facts, release), wants, folder)


def _find_draft(api, version):
    tag = 'v' + version
    mine = [r for r in api.full_releases() if r.tag_name == tag]
    if len(mine) > 1:
        raise Refused(CONFLICT, 'more than one release names ' + tag)
    return mine[0] if mine else None


def _check_identity(fact, version, commit, notes):
    """The exact release record. It is checked before any asset can change."""
    if fact.target_commitish != commit:
        raise Refused(CONFLICT, 'the release of v' + version + ' does not target the reserved commit')
    if (fact.tag_name, fact.name, fact.draft, fact.prerelease, fact.body) != ('v' + version, 'v' + version, True, True, notes):
        raise Refused(CONFLICT, 'the draft of v' + version + ' is not the exact draft')


def _check_names(assets, wants):
    seen = set()
    for asset in assets:
        if asset.name not in wants:
            raise Refused(CONFLICT, 'the draft holds the unexpected asset ' + asset.name)
        if asset.name in seen:
            raise Refused(CONFLICT, 'the draft holds ' + asset.name + ' twice')
        seen.add(asset.name)
        if asset.state != 'uploaded':
            raise Refused(CONFLICT, 'the draft asset ' + asset.name + ' is incomplete')
    return seen


def _check_asset_bytes(api, asset, want, out):
    """This asset's current bytes, downloaded anew. A size or digest difference is a clobbered asset."""
    not_exact = Refused(CONFLICT, 'the draft asset ' + asset.name + ' is not its exact bytes')
    if asset.size != want.size:
        raise not_exact
    if api.download_asset(asset, out, ASSET_MAX) != (want.sha256, want.size):
        raise not_exact


def _read_back(api, release_id, version, commit, notes, wants, folder):
    """The complete draft again, with its exact identity and every asset's fresh bytes in `folder`."""
    fact = api.release_full(release_id)
    _check_identity(fact, version, commit, notes)
    seen = _check_names(fact.assets, wants)
    for name in sorted(set(wants) - seen):
        raise Refused(CONFLICT, 'the draft lacks ' + name)
    folder.mkdir(mode=0o700)
    for asset in fact.assets:
        _check_asset_bytes(api, asset, wants[asset.name], folder / asset.name)
    return fact


def _receipt_bytes(fact, plan):
    assets = [{'asset_id': a.asset_id, 'name': a.name, 'sha256': plan.wants[a.name].sha256, 'size': a.size}
              for a in sorted(fact.assets, key=lambda a: a.name)]
    return _canon({'schema': 1, 'version': plan.version, 'commit': plan.commit, 'tuple_sha256': plan.tuple_sha256,
                   'release_id': fact.release_id, 'notes_sha256': _sha(plan.notes.encode('utf-8')), 'assets': assets})


def prepare_draft(api, plan, work, receipted=False):
    """The one private draft. An existing draft keeps its id. It uploads a missing asset once, accepts an existing
    asset only by its exact bytes and repairs nothing. Once a receipt or the final tag exists, the draft was complete:
    nothing here writes, and a missing draft or asset is a changed draft."""
    tag = 'v' + plan.version
    fact = _find_draft(api, plan.version)
    tagged = _final_tag_commit(api, plan.version) is not None
    if fact is None:
        if tagged:
            raise Refused(CONFLICT, 'the final tag ' + tag + ' exists without its draft')
        if receipted:
            raise Refused(CONFLICT, 'the draft changed after a prior receipt')
        try:
            fact = api.create_draft(tag, plan.commit, tag, plan.notes)
        except gh.ReconcileRequired:
            fact = _find_draft(api, plan.version)
            if fact is None:
                raise Refused(CONFLICT, 'the draft outcome is unresolved; reconcile again before any creation') from None
    _check_identity(fact, plan.version, plan.commit, plan.notes)
    present = _check_names(fact.assets, plan.wants)
    check = Path(work) / 'existing'
    check.mkdir(mode=0o700)
    for asset in fact.assets:
        _check_asset_bytes(api, asset, plan.wants[asset.name], check / asset.name)
    for name in sorted(set(plan.wants) - present):
        if tagged or receipted:
            raise Refused(CONFLICT, 'the draft lacks ' + name)
        try:
            api.upload_asset(fact.release_id, name, plan.folder / name)
        except gh.ReconcileRequired:
            current = api.release_full(fact.release_id)
            if name not in _check_names(current.assets, plan.wants):
                raise Refused(CONFLICT, 'the upload of ' + name + ' is unresolved; reconcile again before any upload') from None
    folder = Path(work) / 'fresh'
    fact = _read_back(api, fact.release_id, plan.version, plan.commit, plan.notes, plan.wants, folder)
    return DraftResult(fact, _receipt_bytes(fact, plan), folder)


def run_draft(facts, api, request, transfers, run_id, attempt, signer_sha, verifier, fetch_crate, work):
    """The draft job. Real-signature verification comes first: an unverified signature reaches no draft."""
    check_pin(signer_sha)
    release = _lifted(elig.preliminary_recovery, request, facts)
    _check_release_pin(release, signer_sha)
    trust = ctl._signing_trust(facts, release, release.policy_commit)
    signature, _ = _signature_transfer(facts, release, transfers, run_id, attempt, verifier, trust['signer_line'])
    plan = _gather(facts, request, release, signature, fetch_crate, work)
    result = prepare_draft(api, plan, work, bool(_prior_receipts(facts, release.version)))
    _check_prior_receipts(facts, release.version, _parse_draft_receipt(result.receipt))
    return result


# ---------------------------------------------------------------------------------------------
# The genuine checks
# ---------------------------------------------------------------------------------------------

def _boundary(work):
    boundary = Path(work) / 'boundary'
    boundary.mkdir(mode=0o700)
    for name in ('download', 'home', 'tmp'):
        (boundary / name).mkdir(mode=0o700)
    return boundary


def _run_install(checker, boundary, signer_line, version, commit, target, env):
    """The genuine checker over `boundary/download`, in a named environment. All six checks true, skipped 0."""
    argv = [str(checker), '--download', str(boundary / 'download'), '--signer-line', signer_line, '--version', version,
            '--commit', commit, '--target', target, '--unpack', str(boundary / 'unpack'), '--prefix',
            str(boundary / 'home' / ('rdy-' + version))]
    child = {k: env[k] for k in rehearse.PASSED_ENV if k in env}
    child.update({'HOME': str(boundary / 'home'), 'TMPDIR': str(boundary / 'tmp')})
    return rehearse.check_install_report(rehearse.run_checker(checker, argv, boundary, child), target, version, commit)


def genuine_check(facts, request, target, assets_id, receipt_id, run_id, attempt, checker, work, env):
    """One native target: the draft's exact bytes, from this attempt's transfer, through the genuine checker
    with the version-selected key. S and V must match. Nothing is rebuilt."""
    release = _lifted(elig.preliminary_recovery, request, facts)
    receipt, raw = _draft_receipt(facts, release, Transfers(0, '', draft_receipt_id=receipt_id), run_id, attempt)
    trust = ctl._signing_trust(facts, release, release.policy_commit)
    entries = {e['name']: e for e in receipt['assets']}
    archive_name = cand.archive_name(release.version, target)
    wrong = Refused(MISMATCH, 'the draft assets artifact is not the draft receipt')
    try:
        artifact = facts.artifact(assets_id, run_id)
    except ControllerError:
        raise Refused(INCOMPLETE, 'the draft assets artifact is unavailable')
    if artifact.name != DRAFT_ASSETS_ARTIFACT + str(attempt) or artifact.head_sha != release.commit:
        raise wrong
    boundary = _boundary(work)
    download = boundary / 'download'
    try:
        with zipfile.ZipFile(str(facts.artifact_path(artifact))) as archive:
            if not cand.exact_members(archive, {name: ASSET_MAX for name in entries}):
                raise wrong
            for name in (archive_name, MANIFEST, SIGNATURE):
                got = cand.extract_member(archive, name, download / name, ASSET_MAX, 0o600) if name in entries else None
                if name not in entries or got != (entries[name]['sha256'], entries[name]['size']):
                    raise wrong
    except (zipfile.BadZipFile, OSError):
        raise wrong
    signature = (download / SIGNATURE).read_bytes()
    install = _run_install(checker, boundary, trust['signer_line'], release.version, release.commit, target, env)
    return _canon({'schema': 1, 'version': release.version, 'commit': release.commit, 'target': target,
                   'tuple_sha256': release.tuple_sha256, 'manifest_sha256': release.manifest_sha256,
                   'signature_sha256': _sha(signature), 'release_id': receipt['release_id'],
                   'draft_receipt_sha256': _sha(raw), 'report_sha256': install['report_sha256'],
                   'checks': install['checks'], 'skipped': install['skipped']})


def _check_genuine(facts, release, target, artifact_id, run_id, attempt, receipt, raw, signature):
    _job_passed(facts, run_id, attempt, GENUINE_JOBS[target], release.commit)
    data = _member(facts, artifact_id, run_id, attempt, GENUINE_ARTIFACT + target + '-a', GENUINE_MEMBER,
                   target + ' genuine receipt', release)
    try:
        value = cand.closed(cand.loads(data), GENUINE_KEYS, 'the genuine receipt')
    except ControllerError:
        raise Refused(MISMATCH, 'the ' + target + ' genuine receipt is malformed')
    if value['checks'] != [{'name': n, 'passed': True} for n in cand.CHECK_NAMES] or type(value['skipped']) is not int \
            or value['skipped'] != 0:
        raise Refused(MISMATCH, 'the ' + target + ' genuine receipt is not all six checks')
    if (value['schema'], value['version'], value['commit'], value['target'], value['tuple_sha256'],
            value['manifest_sha256'], value['signature_sha256'], value['release_id'], value['draft_receipt_sha256']) != (
                1, release.version, release.commit, target, release.tuple_sha256, release.manifest_sha256,
                _sha(signature), receipt['release_id'], _sha(raw)):
        raise Refused(MISMATCH, 'the ' + target + ' genuine receipt is not this release')


# ---------------------------------------------------------------------------------------------
# Final tag and promotion
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Ready(object):
    release: object
    trust: dict
    receipt: dict
    fact: DraftFact


def _qualify(reader, api, enrollment, request, transfers, run_id, attempt, signer_sha, verifier, work):
    """Every check before a ref or the release changes, read again now: current revocations, policy and admission;
    the reservation, E and T; the verified signature; both genuine receipts; the draft and every asset's bytes."""
    check_pin(signer_sha)
    release = _lifted(elig.preliminary_recovery, request, reader)
    _check_release_pin(release, signer_sha)
    trust = ctl._signing_trust(reader, release, release.policy_commit)
    signature, authorization = _signature_transfer(reader, release, transfers, run_id, attempt, verifier, trust['signer_line'])
    receipt, raw = _draft_receipt(reader, release, transfers, run_id, attempt)
    for target in cand.TARGETS:
        _check_genuine(reader, release, target, transfers.genuine[target], run_id, attempt, receipt, raw, signature)
    _lifted(ctl.check_promotion, reader, enrollment, request, run_id, attempt, authorization)
    _check_prior_receipts(reader, release.version, receipt)
    wants, _ = wanted_assets(reader, release, signature)
    notes = _notes(reader, release)
    fact = _find_draft(api, release.version)
    if fact is None or fact.release_id != receipt['release_id'] or _sha(notes.encode('utf-8')) != receipt['notes_sha256']:
        raise Refused(CONFLICT, 'the draft changed after its receipt')
    fact = _read_back(api, fact.release_id, release.version, release.commit, notes, wants, Path(work) / 'fresh')
    current = [{'asset_id': a.asset_id, 'name': a.name} for a in sorted(fact.assets, key=lambda a: a.name)]
    if current != [{'asset_id': e['asset_id'], 'name': e['name']} for e in receipt['assets']]:
        raise Refused(CONFLICT, 'the draft changed after its receipt')
    return Ready(release, trust, receipt, fact)


def _final_tag_commit(reader, version):
    try:
        ref = reader.ref('tags/v' + version)
    except gh.Absent:
        return None
    if ref.object_type == 'commit':
        return ref.object_sha
    tag = reader.tag(ref.object_sha)
    return tag.object_sha if tag.object_type == 'commit' else None


def run_final_tag(api, enrollment, request, transfers, run_id, attempt, signer_sha, verifier, work):
    """After both genuine checks and every other check: v<V> at S, once. The publisher needs no tag bypass, and
    the tag is never moved. A lost answer reads back; an unresolved one stops. An existing tag at S is a recorded
    retry with no write. Returns the job's evidence."""
    ready = _qualify(api, api, enrollment, request, transfers, run_id, attempt, signer_sha, verifier, work)
    version, commit = ready.release.version, ready.release.commit
    existing = _final_tag_commit(api, version)
    if existing is not None and existing != commit:
        raise Refused(CONFLICT, 'the final tag v' + version + ' exists before this job created it')
    if existing is None:
        try:
            api.create_final_tag(version, commit)
        except gh.ReconcileRequired:
            pass
        if _final_tag_commit(api, version) != commit:
            raise Refused(CONFLICT, 'the final tag outcome is unresolved; reconcile again before any creation')
    return _canon({'schema': 1, 'version': version, 'commit': commit, 'tuple_sha256': ready.release.tuple_sha256,
                   'release_id': ready.fact.release_id, 'run_id': run_id, 'run_attempt': attempt,
                   'result': 'created' if existing is None else 'recorded_retry'})


def _promotion_receipt(ready, fact):
    wants = {e['name']: e for e in ready.receipt['assets']}
    return _canon({'schema': 1, 'version': ready.release.version, 'commit': ready.release.commit,
                   'tuple_sha256': ready.release.tuple_sha256, 'release_id': fact.release_id,
                   'notes_sha256': ready.receipt['notes_sha256'], 'signer_line': ready.trust['signer_line'],
                   'assets': [wants[a.name] for a in sorted(fact.assets, key=lambda a: a.name)]})


def promote(facts, publisher, enrollment, request, transfers, run_id, attempt, signer_sha, verifier, work):
    """That same draft id, promoted by the publisher App, after everything is read again. It sends one flip. A lost
    answer reads back, and a read that does not show it published stops: the next run reconciles. The receipt stays
    outside the immutable E."""
    reader = DraftView(facts, publisher)
    ready = _qualify(reader, publisher, enrollment, request, transfers, run_id, attempt, signer_sha, verifier, work)
    version, commit = ready.release.version, ready.release.commit
    if _final_tag_commit(reader, version) != commit:
        raise Refused(CONFLICT, 'the final tag v' + version + ' is missing')
    try:
        fact = publisher.publish_draft(ready.fact.release_id)
    except gh.ReconcileRequired:
        fact = publisher.release_full(ready.fact.release_id)
        if fact.draft:
            raise Refused(CONFLICT, 'the publication outcome is unresolved; reconcile again before any publication') from None
    if (fact.release_id, fact.tag_name, fact.target_commitish, fact.draft) != (
            ready.fact.release_id, 'v' + version, commit, False) \
            or {a.asset_id for a in fact.assets} != {a.asset_id for a in ready.fact.assets}:
        raise Refused(CONFLICT, 'the published release is not the verified draft')
    return _promotion_receipt(ready, fact)


# ---------------------------------------------------------------------------------------------
# The public checks
# ---------------------------------------------------------------------------------------------

def check_published(public, receipt, target, checker, verify, work, env):
    """Anonymous HTTP 200 and exact bytes for every public asset, the tag at S, the real signature Good, the notes
    digest and the native quick start. A failure is an incident, reported as one: nothing here can write."""
    value = cand.closed(cand.loads(receipt), PROMOTION_KEYS, 'the promotion receipt')
    version, commit = value['version'], value['commit']
    tag = 'v' + version

    def incident(reason):
        return ControllerError('published ' + tag + ' is an incident: ' + reason)
    try:
        fact = public.public_release(tag)
    except ControllerError:
        raise incident('the public release is unavailable')
    if fact.release_id != value['release_id'] or fact.draft:
        raise incident('the public release is another release')
    if _sha(fact.body.encode('utf-8')) != value['notes_sha256']:
        raise incident('the public notes are not the release notes')
    if _final_tag_commit(public, version) != commit:
        raise incident('the tag is not at the release commit')
    entries = {e['name']: e for e in value['assets']}
    if sorted(a.name for a in fact.assets) != sorted(entries):
        raise incident('the public asset set is not the expected set')
    boundary = _boundary(work)
    download, others = boundary / 'download', boundary / 'others'
    others.mkdir(mode=0o700)
    archive = cand.archive_name(version, target)
    for name, entry in sorted(entries.items()):
        folder = download if name in (archive, MANIFEST, SIGNATURE) else others
        try:
            got = public.public_asset(tag, name, folder / name, ASSET_MAX)
        except ControllerError:
            raise incident(name + ' is unavailable')
        if got != (entry['sha256'], entry['size']):
            raise incident(name + ' is not its exact bytes')
    signature, manifest = (download / SIGNATURE).read_bytes(), (download / MANIFEST).read_bytes()
    if verify.verify(signature, manifest, value['signer_line'], ctl.PRINCIPAL, ctl.NAMESPACE) is not True:
        raise incident('the public signature is not Good')
    try:
        install = _run_install(checker, boundary, value['signer_line'], version, commit, target, env)
    except ControllerError as error:
        raise incident(str(error))
    return _canon({'schema': 1, 'version': version, 'commit': commit, 'tuple_sha256': value['tuple_sha256'],
                   'release_id': value['release_id'], 'target': target, 'notes_sha256': value['notes_sha256'],
                   'assets': sorted(entries), 'report_sha256': install['report_sha256'], 'checks': install['checks'],
                   'skipped': install['skipped']})


# ---------------------------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------------------------

def _input_int(environ, key):
    text = environ.get('RDY_INPUT_' + key.upper(), '')
    if not re.fullmatch(r'[1-9][0-9]{0,18}', text):
        raise ControllerError('the input ' + key + ' is not a positive integer')
    return int(text)


def request_from_inputs(environ):
    """The dispatch inputs and this run attempt, as one closed request. No input body reaches a refusal."""
    value = {key: _input_int(environ, key) for key in ('candidate_run_id', 'candidate_run_attempt', 'candidate_artifact_id',
                                                       'evidence_run_id', 'evidence_run_attempt', 'evidence_artifact_id')}
    run_id, attempt = _where(environ)
    value.update(version=environ.get('RDY_INPUT_VERSION', ''), evidence_sha256=environ.get('RDY_INPUT_EVIDENCE_SHA256', ''),
                 approvals=cand.loads(environ.get('RDY_INPUT_APPROVALS', '').encode('utf-8')), dispatch_run_id=run_id,
                 dispatch_run_attempt=attempt)
    return elig.parse_request(value)


def _request_value(request):
    return {'version': request.version, 'candidate_run_id': request.candidate_run_id,
            'candidate_run_attempt': request.candidate_run_attempt, 'candidate_artifact_id': request.candidate_artifact_id,
            'evidence_run_id': request.evidence_run_id, 'evidence_run_attempt': request.evidence_run_attempt,
            'evidence_artifact_id': request.evidence_artifact_id, 'evidence_sha256': request.evidence_sha256,
            'dispatch_run_id': request.dispatch_run_id, 'dispatch_run_attempt': request.dispatch_run_attempt,
            'approvals': [{'receipt_sha256': a.receipt_sha256, 'run_id': a.run_id, 'artifact_id': a.artifact_id}
                          for a in request.approvals]}


def _parsed_request(environ):
    return elig.parse_request(cand.loads(environ.get('RDY_RELEASE_REQUEST', '').encode('utf-8')))


def _where(environ):
    run_id, attempt = environ.get('GITHUB_RUN_ID', ''), environ.get('GITHUB_RUN_ATTEMPT', '')
    if not (run_id.isdigit() and attempt.isdigit()):
        raise ControllerError('the job environment is incomplete')
    return int(run_id), int(attempt)


def _output(path, **values):
    """Single-line values only, so an output cannot add another output."""
    with open(str(path), 'a') as handle:
        for key, value in values.items():
            text = str(value)
            if '\n' in text or '\r' in text:
                raise ControllerError('a job output holds a line break')
            handle.write(key + '=' + text + '\n')


def _write(out, name, data):
    out = Path(out)
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    cand.write_new(out / name, data, 0o600)


def _start(args, environ):
    """Trusted checkout, the pin and the verifier built at the same revision. Returns the facts client."""
    check_pin(args.signer_sha)
    sign._check_trusted(args.trusted, args.signer_sha)
    work = Path(args.work)
    work.mkdir(mode=0o700, parents=True)
    verifier = sign._install_verifier(args.verifier, args.verifier_sha256, work)
    return Releases(Transport(environ.get('GITHUB_TOKEN', '')), work, verifier), work, verifier


def _fetch_crate(public):
    def fetch(name):
        crate = name[:-len('.crate')].rsplit('-', 1)[0]
        buffer = gh._Buffer()
        try:
            status = public.public_get(CRATE_HOST + crate + '/' + name, buffer, CRATE_MAX)
        except (gh.Timeout, gh.TransportFailure):
            raise ControllerError('the source crate ' + name + ' is unavailable')
        if status != 200:
            raise ControllerError('the source crate ' + name + ' is unavailable')
        return buffer.data()
    return fetch


def _transfers(environ):
    def number(key):
        text = environ.get(key, '')
        if not text.isdigit():
            raise ControllerError('the transfer ' + key + ' is not a number')
        return int(text)
    genuine = {}
    for target, key in zip(cand.TARGETS, ('RDY_GENUINE_MAC_ARTIFACT_ID', 'RDY_GENUINE_LINUX_ARTIFACT_ID')):
        if key in environ:
            genuine[target] = number(key)
    return Transfers(number('RDY_SIGNATURE_ARTIFACT_ID'), environ.get('RDY_SIGNATURE_ARTIFACT_DIGEST', ''),
                     number('RDY_DRAFT_RECEIPT_ARTIFACT_ID') if 'RDY_DRAFT_RECEIPT_ARTIFACT_ID' in environ else None,
                     genuine)


def _anonymous(work):
    return Releases(AnonymousTransport(), work, 'unused')


def cmd_eligibility(args, environ):
    facts, work, _ = _start(args, environ)
    request = request_from_inputs(environ)
    result = reconcile(request, facts, _anonymous(work), sign.SshVerifier(), args.signer_sha, work)
    _output(args.github_output, state=result.state, request=_canon(_request_value(request)).decode('utf-8').strip())
    if result.terminal is not None:
        terminal = result.terminal
        sys.stdout.write(_canon({'result': result.state, 'version': terminal.version, 'commit': terminal.commit,
                                 'tuple_sha256': terminal.tuple_sha256, 'release_id': terminal.release_id,
                                 'channel_enabled': terminal.channel_enabled,
                                 'revoked_receipts': list(terminal.revoked_receipts),
                                 'signer_status': terminal.signer_status}).decode('utf-8'))


def cmd_reserve(args, environ):
    facts, _, _ = _start(args, environ)
    run_id, attempt = _where(environ)
    _write(args.out, ctl.RESERVE_MEMBER, reserve_tuple(_parsed_request(environ), facts, run_id, attempt, args.signer_sha))


def cmd_draft(args, environ):
    facts, work, _ = _start(args, environ)
    run_id, attempt = _where(environ)
    result = run_draft(facts, facts, _parsed_request(environ), _transfers(environ), run_id, attempt, args.signer_sha,
                       sign.SshVerifier(), _fetch_crate(AnonymousTransport()), work)
    _write(args.out, DRAFT_RECEIPT_MEMBER, result.receipt)
    shutil.move(str(result.folder), str(args.assets))


def cmd_genuine(args, environ):
    check_pin(args.signer_sha)
    sign._check_trusted(args.trusted, args.signer_sha)
    work = Path(args.work)
    work.mkdir(mode=0o700, parents=True)
    run_id, attempt = _where(environ)
    facts = Releases(Transport(environ.get('GITHUB_TOKEN', '')), work, args.verifier)
    assets, receipt = environ.get('RDY_DRAFT_ASSETS_ARTIFACT_ID', ''), environ.get('RDY_DRAFT_RECEIPT_ARTIFACT_ID', '')
    if not (assets.isdigit() and receipt.isdigit()):
        raise ControllerError('the draft transfer ids are not numbers')
    _write(args.out, GENUINE_MEMBER, genuine_check(facts, _parsed_request(environ), args.target, int(assets), int(receipt),
                                                   run_id, attempt, args.checker, work, environ))


def _enrollment(environ, signer_sha):
    return sign.parse_enrollment(environ.get('RDY_RELEASE_ENROLLMENT', ''), signer_sha)[0]


def cmd_final_tag(args, environ):
    check_settings_readback(environ.get('RDY_RELEASE_SETTINGS_READBACK', ''))
    facts, work, _ = _start(args, environ)
    run_id, attempt = _where(environ)
    _write(args.out, FINAL_TAG_MEMBER, run_final_tag(facts, _enrollment(environ, args.signer_sha), _parsed_request(environ),
                                                     _transfers(environ), run_id, attempt, args.signer_sha,
                                                     sign.SshVerifier(), work))


def cmd_promote(args, environ):
    check_settings_readback(environ.get('RDY_RELEASE_SETTINGS_READBACK', ''))
    facts, work, verifier = _start(args, environ)
    run_id, attempt = _where(environ)
    receipt = promote(facts, publisher_client(environ, work, verifier), _enrollment(environ, args.signer_sha),
                      _parsed_request(environ), _transfers(environ), run_id, attempt, args.signer_sha, sign.SshVerifier(),
                      work)
    _output(args.github_output, receipt=receipt.decode('utf-8').strip())


def cmd_check_published(args, environ):
    check_anonymous_env(environ)
    work = Path(args.work)
    work.mkdir(mode=0o700, parents=True)
    receipt = environ.get('RDY_PUBLISH_RECEIPT', '').encode('utf-8')
    _write(args.out, 'published-receipt.json', check_published(_anonymous(work), receipt, args.target, args.checker,
                                                               sign.SshVerifier(), work, environ))


def _common(parser, verifier=True):
    parser.add_argument('--signer-sha', required=True)
    parser.add_argument('--trusted', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    if verifier:
        parser.add_argument('--verifier', type=Path, required=True)
        parser.add_argument('--verifier-sha256', required=True)


def main(argv=None):
    parser = argparse.ArgumentParser(prog='publish.py', description="The release caller's jobs.")
    sub = parser.add_subparsers(dest='command')
    sub.required = True
    commands = {}

    def command(name, call):
        commands[name] = call
        return sub.add_parser(name)
    p = command('eligibility', cmd_eligibility)
    _common(p)
    p.add_argument('--github-output', type=Path, required=True)
    p = command('reserve', cmd_reserve)
    _common(p)
    p.add_argument('--out', type=Path, required=True)
    p = command('draft', cmd_draft)
    _common(p)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--assets', type=Path, required=True)
    p = command('genuine', cmd_genuine)
    _common(p, verifier=False)
    p.add_argument('--verifier', type=Path, required=True)
    p.add_argument('--target', choices=cand.TARGETS, required=True)
    p.add_argument('--checker', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p = command('final-tag', cmd_final_tag)
    _common(p)
    p.add_argument('--out', type=Path, required=True)
    p = command('promote', cmd_promote)
    _common(p)
    p.add_argument('--github-output', type=Path, required=True)
    p = command('check-published', cmd_check_published)
    p.add_argument('--target', choices=cand.TARGETS, required=True)
    p.add_argument('--checker', type=Path, required=True)
    p.add_argument('--work', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        commands[args.command](args, dict(os.environ))
    except ControllerError as error:
        sys.stderr.write('release controller: ' + str(error) + '\n')
        return 1
    except OSError:
        sys.stderr.write('release controller: a file operation failed\n')
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
