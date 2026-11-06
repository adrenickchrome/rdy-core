#!/usr/bin/env python3
"""Bounded GitHub REST transport for the release controller.

It returns GitHub-owned facts with their exact identity: repository, run, run attempt, job,
artifact, ref, tag object, commit and file. Each fact carries the SHA-256 of the response bytes it
came from. Nothing here decides eligibility or moves a state; later steps do.

Outcomes are separate refusals, never mixed up:
  Absent             an authenticated 404: the repository was read with these credentials first
  Unauthorized       401, 403, or 404 before the repository identity was authenticated
  Uncertain          a timeout, transport failure, 5xx or rate limit after three read attempts
  StatusError        any other HTTP status
  Malformed          a body over its bound, not JSON, a wrong type or a wrong identity; a page
                     over 100 rows, more than 100 pages, or lost, repeated or extra rows
  ReconcileRequired  a mutation whose outcome is not known (timeout, transport failure, 409,
                     422, 429, 5xx, or a success that differs from the request)

Reads are tried at most three times on uncertainty. A mutation is sent exactly once and never
retried: an unknown outcome needs a read of the exact ref and object before any further write.
A successful mutation returns what GitHub created, for exact readback; it is not a boolean.

The transport is injected. `UrllibTransport` is the real one; tests use fakes only.
"""
import base64
from dataclasses import dataclass
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import candidate as cand  # noqa: E402

ControllerError = cand.ControllerError
REPOSITORY = cand.REPOSITORY
REPOSITORY_ID = 1403751388
API = 'https://api.github.com'

MIB = cand.MIB
BODY_MAX = 4 * MIB            # any API body
CONTENTS_MAX = 1 * MIB        # one file read through the contents API
PAGE_ROWS = 100
PAGES_MAX = 100
JOBS_MAX = 64                 # jobs of one run attempt
REQUEST_DEADLINE = 60         # seconds for one network request
READ_ATTEMPTS = 3

COMMIT_RE = cand.COMMIT_RE
DIGEST_RE = re.compile(r'sha256:([0-9a-f]{64})')
RESERVE_TAG_RE = re.compile(r'reserve/v0\.[1-9][0-9]{0,8}\.0')
RESERVE_REF_RE = re.compile(r'refs/tags/reserve/v0\.[1-9][0-9]{0,8}\.0')
REF_NAME_RE = re.compile(r'(?:tags|heads)/[A-Za-z0-9][A-Za-z0-9._/-]{0,200}')
PATH_RE = re.compile(r'[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+){0,15}')
NEXT_RE = re.compile(r'<[^>]*>;\s*rel="next"')


# ---------------------------------------------------------------------------------------------
# Outcomes
# ---------------------------------------------------------------------------------------------

class GitHubError(ControllerError):
    """A GitHub refusal: `<operation>: <reason>`."""

    def __init__(self, operation, reason, status=None):
        ControllerError.__init__(self, 'GitHub ' + operation + ': ' + reason)
        self.operation = operation
        self.status = status


class Absent(GitHubError):
    """An authenticated 404."""


class Unauthorized(GitHubError):
    """401, 403, or a 404 that authentication does not explain."""


class Uncertain(GitHubError):
    """The read's outcome is unknown after every attempt."""


class StatusError(GitHubError):
    """Another HTTP status."""


class Malformed(GitHubError):
    """A response that breaks its bound, type or identity."""


class ArtifactExpired(Malformed):
    """An artifact that GitHub reports as expired. It is not approval, and it is not a malformed fact:
    recovery can tell the two apart."""


class ReconcileRequired(GitHubError):
    """A mutation whose outcome is unknown."""

    def __init__(self, operation, reason, status=None):
        GitHubError.__init__(self, operation, 'outcome unknown (' + reason
                             + '); read the exact ref and object before any further write', status)
        self.reason = reason


class Timeout(Exception):
    """Raised by a transport when its deadline passes."""


class TransportFailure(Exception):
    """Raised by a transport when the connection fails."""


@dataclass(frozen=True)
class Response(object):
    status: int
    headers: dict
    body: bytes


# ---------------------------------------------------------------------------------------------
# Facts
# ---------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class RepositoryFact:
    repository_id: int
    full_name: str
    sha256: str


@dataclass(frozen=True)
class RunFact:
    run_id: int
    run_attempt: int
    workflow_id: int
    path: str
    head_sha: str
    head_branch: object
    event: str
    status: str
    conclusion: object
    actor_id: int
    triggering_actor_id: int
    repository_id: int
    head_repository_id: int
    html_url: str
    sha256: str


@dataclass(frozen=True)
class JobFact:
    job_id: int
    run_id: int
    run_attempt: int
    name: str
    status: str
    conclusion: object
    html_url: str
    head_sha: str


@dataclass(frozen=True)
class ArtifactFact:
    artifact_id: int
    name: str
    size: int
    digest: str
    expired: bool
    run_id: int
    repository_id: int
    head_repository_id: int
    head_sha: str
    sha256: str


@dataclass(frozen=True)
class DownloadFact:
    artifact_id: int
    run_id: int
    path: str
    sha256: str
    size: int


@dataclass(frozen=True)
class RefFact:
    ref: str
    object_type: str
    object_sha: str
    sha256: str


@dataclass(frozen=True)
class TagFact:
    sha: str
    tag: str
    message_bytes: bytes
    object_type: str
    object_sha: str
    sha256: str


@dataclass(frozen=True)
class CommitFact:
    sha: str
    tree_sha: str
    parents: tuple
    sha256: str


@dataclass(frozen=True)
class FileFact:
    path: str
    ref: str
    blob_sha: str
    data: bytes
    sha256: str


@dataclass(frozen=True)
class ReleaseFact:
    release_id: int
    tag_name: str
    target_commitish: str
    draft: bool
    prerelease: bool
    assets: tuple


@dataclass(frozen=True)
class CompareFact:
    base: str
    head: str
    status: str
    ahead_by: int
    behind_by: int
    merge_base_sha: str
    sha256: str


@dataclass(frozen=True)
class ApprovalFact:
    state: str
    comment: str
    user_id: int
    user_type: str
    environments: tuple


@dataclass(frozen=True)
class AlertFact:
    number: int
    state: str
    tool: str
    security_severity: object


@dataclass(frozen=True)
class WorkflowFact:
    workflow_id: int
    path: str
    state: str
    sha256: str


# ---------------------------------------------------------------------------------------------
# Field checks
# ---------------------------------------------------------------------------------------------

def need_id(value, what):
    if not cand.is_int(value, 1):
        raise ControllerError('the ' + what + ' is not a positive integer')
    return value


def need_commit(value, what):
    if not (isinstance(value, str) and COMMIT_RE.fullmatch(value)):
        raise ControllerError('the ' + what + ' is not a 40-hex Git SHA')
    return value


def field(value, key, kind, operation, what=None, nullable=False):
    """`value[key]` of exactly `kind` (int never matches a boolean)."""
    what = what or key.replace('_', ' ')
    item = value.get(key) if isinstance(value, dict) else None
    if item is None and nullable and isinstance(value, dict) and key in value:
        return None
    if kind is int:
        ok = cand.is_int(item, 1)
    elif kind == 'sha':
        ok = isinstance(item, str) and COMMIT_RE.fullmatch(item) is not None
    elif kind is bool:
        ok = type(item) is bool
    else:
        ok = isinstance(item, kind)
    if not ok:
        raise Malformed(operation, 'the ' + what + ' is malformed')
    return item


def nested_id(value, key, operation, what):
    inner = value.get(key) if isinstance(value, dict) else None
    return field(inner, 'id', int, operation, what)


def is_rate_limited(response):
    if response.status == 429:
        return True
    return response.status == 403 and (response.headers.get('x-ratelimit-remaining') == '0'
                                       or 'retry-after' in response.headers)


def quote(segment):
    return urllib.parse.quote(segment, safe='')


# ---------------------------------------------------------------------------------------------
# The client
# ---------------------------------------------------------------------------------------------

class GitHub(object):
    """Reads and the two reservation writes for one repository, through an injected transport.

    A transport has `request(method, path, body=None, limit=None) -> Response` (it may return at
    most `limit + 1` body bytes and raises `Timeout` or `TransportFailure`) and
    `download(url, sink, limit) -> status`, an unauthenticated GET that writes at most `limit + 1`
    bytes to `sink`."""

    def __init__(self, transport, pause=None):
        self.transport = transport
        # Waits before read attempt 2 and 3; tests inject a no-op.
        self.pause = pause or (lambda attempt: time.sleep(2 * attempt))
        self.root = '/repos/' + REPOSITORY
        self._repository = None

    # -- reads ------------------------------------------------------------------------------

    def _send(self, operation, path, limit):
        """One read with up to three attempts; returns the 200 response."""
        reason = None
        for attempt in range(READ_ATTEMPTS):
            if attempt:
                self.pause(attempt)
            try:
                response = self.transport.request('GET', path, None, limit)
            except Timeout:
                reason = 'timeout'
                continue
            except TransportFailure:
                reason = 'transport failure'
                continue
            if len(response.body) > limit:
                raise Malformed(operation, 'the response is larger than ' + str(limit) + ' bytes')
            if response.status == 200:
                return response
            if is_rate_limited(response):
                reason = 'rate limited'
                continue
            if response.status >= 500:
                reason = 'HTTP ' + str(response.status)
                continue
            self._refuse(operation, response.status, read=True)
        raise Uncertain(operation, 'outcome unknown (' + reason + ')')

    def _refuse(self, operation, status, read):
        if status == 404 and read and self._repository is not None:
            raise Absent(operation, 'not found', status)
        if status in (401, 403, 404):
            raise Unauthorized(operation, 'not authorized (HTTP ' + str(status) + ')', status)
        raise StatusError(operation, 'HTTP ' + str(status), status)

    @staticmethod
    def _json(operation, response):
        try:
            return cand.loads(response.body, len(response.body), floats=True)
        except (ControllerError, RecursionError):
            raise Malformed(operation, 'the response is not JSON')

    def _get(self, operation, path, limit=BODY_MAX):
        response = self._send(operation, path, limit)
        return self._json(operation, response), hashlib.sha256(response.body).hexdigest()

    def _object(self, operation, path, limit=BODY_MAX):
        self.repository()
        value, digest = self._get(operation, path, limit)
        if not isinstance(value, dict):
            raise Malformed(operation, 'the answer is malformed')
        return value, digest

    def repository(self):
        """The repository identity. Every other call authenticates it first."""
        if self._repository is None:
            operation = 'repository'
            value, digest = self._get(operation, self.root)
            if not (isinstance(value, dict) and value.get('full_name') == REPOSITORY
                    and type(value.get('id')) is int and value['id'] == REPOSITORY_ID):
                raise Malformed(operation, 'the repository is not ' + REPOSITORY + ' (' + str(REPOSITORY_ID) + ')')
            self._repository = RepositoryFact(REPOSITORY_ID, REPOSITORY, digest)
        return self._repository

    def _pages(self, operation, path, key=None, rows_max=PAGE_ROWS * PAGES_MAX):
        """Every row of a paginated list. With `key`, the list sits under it beside `total_count`."""
        self.repository()
        rows, seen, total = [], set(), None
        joiner = '&' if '?' in path else '?'
        for number in range(1, PAGES_MAX + 1):
            response = self._send(operation, path + joiner + 'per_page=' + str(PAGE_ROWS) + '&page=' + str(number),
                                  BODY_MAX)
            value = self._json(operation, response)
            if key is None:
                page = value
            else:
                page = value.get(key) if isinstance(value, dict) else None
                count = value.get('total_count') if isinstance(value, dict) else None
                if not cand.is_int(count, 0):
                    raise Malformed(operation, 'the total count is malformed')
                if total is None:
                    total = count
                    if total > rows_max:
                        raise Malformed(operation, 'more than ' + str(rows_max) + ' rows')
                elif count != total:
                    raise Malformed(operation, 'the total changed between pages')
            if not isinstance(page, list) or not all(isinstance(row, dict) for row in page):
                raise Malformed(operation, 'the answer is malformed')
            if len(page) > PAGE_ROWS:
                raise Malformed(operation, 'a page holds more than ' + str(PAGE_ROWS) + ' rows')
            for row in page:
                ident = row.get('id', row.get('number'))
                if not cand.is_int(ident, 1):
                    raise Malformed(operation, 'a row has no id')
                if ident in seen:
                    raise Malformed(operation, 'the pages returned a row twice')
                seen.add(ident)
            rows.extend(page)
            if len(rows) > rows_max:
                raise Malformed(operation, 'more than ' + str(rows_max) + ' rows')
            has_next = NEXT_RE.search(response.headers.get('link', '')) is not None
            if key is not None:
                if len(rows) > total:
                    raise Malformed(operation, 'the pages returned extra rows')
                if len(rows) == total:
                    return rows
                if not page:
                    raise Malformed(operation, 'the pages lost rows')
            else:
                if has_next and len(page) < PAGE_ROWS:
                    raise Malformed(operation, 'a short page has a next page')
                if not has_next:
                    return rows
        raise Malformed(operation, 'more than ' + str(PAGES_MAX) + ' pages')

    def workflow(self, workflow_id):
        operation = 'workflow'
        need_id(workflow_id, 'workflow id')
        value, digest = self._object(operation, self.root + '/actions/workflows/' + str(workflow_id))
        if value.get('id') != workflow_id or type(value.get('id')) is not int:
            raise Malformed(operation, 'the answer is not workflow ' + str(workflow_id))
        return WorkflowFact(workflow_id, field(value, 'path', str, operation), field(value, 'state', str, operation),
                            digest)

    def run_attempt(self, run_id, attempt):
        """One exact run attempt: never the latest attempt, never a run found by name."""
        operation = 'run attempt'
        need_id(run_id, 'run id')
        need_id(attempt, 'run attempt')
        value, digest = self._object(operation, self.root + '/actions/runs/' + str(run_id) + '/attempts/'
                                     + str(attempt))
        if not (value.get('id') == run_id and value.get('run_attempt') == attempt
                and type(value.get('id')) is int and type(value.get('run_attempt')) is int):
            raise Malformed(operation, 'the answer is not run ' + str(run_id) + ' attempt ' + str(attempt))
        repository_id = nested_id(value, 'repository', operation, 'repository id')
        if repository_id != REPOSITORY_ID:
            raise Malformed(operation, 'the run is not in rdy-core')
        head_repository_id = nested_id(value, 'head_repository', operation, 'head repository id')
        if head_repository_id != REPOSITORY_ID:
            raise Malformed(operation, 'the run head is a fork')
        return RunFact(
            run_id=run_id, run_attempt=attempt,
            workflow_id=field(value, 'workflow_id', int, operation, 'workflow id'),
            path=field(value, 'path', str, operation),
            head_sha=field(value, 'head_sha', 'sha', operation, 'head SHA'),
            head_branch=field(value, 'head_branch', str, operation, 'head branch', nullable=True),
            event=field(value, 'event', str, operation),
            status=field(value, 'status', str, operation),
            conclusion=field(value, 'conclusion', str, operation, nullable=True),
            actor_id=nested_id(value, 'actor', operation, 'actor id'),
            triggering_actor_id=nested_id(value, 'triggering_actor', operation, 'triggering actor id'),
            repository_id=repository_id, head_repository_id=head_repository_id,
            html_url=field(value, 'html_url', str, operation, 'run URL'), sha256=digest)

    def _job(self, operation, value):
        return JobFact(
            job_id=field(value, 'id', int, operation, 'job id'),
            run_id=field(value, 'run_id', int, operation, 'job run id'),
            run_attempt=field(value, 'run_attempt', int, operation, 'job run attempt'),
            name=field(value, 'name', str, operation, 'job name'),
            status=field(value, 'status', str, operation, 'job status'),
            conclusion=field(value, 'conclusion', str, operation, 'job conclusion', nullable=True),
            html_url=field(value, 'html_url', str, operation, 'job URL'),
            head_sha=field(value, 'head_sha', 'sha', operation, 'job head SHA'))

    def attempt_jobs(self, run_id, attempt):
        """Every job of one exact run attempt (at most 64)."""
        operation = 'jobs'
        need_id(run_id, 'run id')
        need_id(attempt, 'run attempt')
        rows = self._pages(operation, self.root + '/actions/runs/' + str(run_id) + '/attempts/' + str(attempt)
                           + '/jobs', 'jobs', JOBS_MAX)
        jobs = [self._job(operation, row) for row in rows]
        for entry in jobs:
            if entry.run_id != run_id or entry.run_attempt != attempt:
                raise Malformed(operation, 'a job is not from run ' + str(run_id) + ' attempt ' + str(attempt))
        return jobs

    def job(self, job_id):
        operation = 'job'
        need_id(job_id, 'job id')
        value, _ = self._object(operation, self.root + '/actions/jobs/' + str(job_id))
        if value.get('id') != job_id or type(value.get('id')) is not int:
            raise Malformed(operation, 'the answer is not job ' + str(job_id))
        return self._job(operation, value)

    def _artifact(self, operation, value, digest):
        run = value.get('workflow_run')
        if not isinstance(run, dict):
            raise Malformed(operation, 'the artifact run is malformed')
        raw = value.get('digest')
        match = isinstance(raw, str) and DIGEST_RE.fullmatch(raw)
        if not match:
            raise Malformed(operation, 'the artifact has no sha256 digest')
        size = value.get('size_in_bytes')
        if not cand.is_int(size, 0):
            raise Malformed(operation, 'the artifact size is malformed')
        return ArtifactFact(
            artifact_id=field(value, 'id', int, operation, 'artifact id'),
            name=field(value, 'name', str, operation, 'artifact name'), size=size, digest=match.group(1),
            expired=field(value, 'expired', bool, operation, 'expired flag'),
            run_id=field(run, 'id', int, operation, 'artifact run id'),
            repository_id=field(run, 'repository_id', int, operation, 'artifact repository id'),
            head_repository_id=field(run, 'head_repository_id', int, operation, 'artifact head repository id'),
            head_sha=field(run, 'head_sha', 'sha', operation, 'artifact head SHA'), sha256=digest)

    def artifact(self, artifact_id, run_id):
        """One artifact by numeric id, which must come from `run_id` in rdy-core and be unexpired."""
        operation = 'artifact'
        need_id(artifact_id, 'artifact id')
        need_id(run_id, 'run id')
        value, digest = self._object(operation, self.root + '/actions/artifacts/' + str(artifact_id))
        if value.get('id') != artifact_id or type(value.get('id')) is not int:
            raise Malformed(operation, 'the answer is not artifact ' + str(artifact_id))
        fact = self._artifact(operation, value, digest)
        if fact.repository_id != REPOSITORY_ID or fact.head_repository_id != REPOSITORY_ID:
            raise Malformed(operation, 'the artifact is not in rdy-core')
        if fact.run_id != run_id:
            raise Malformed(operation, 'the artifact is not from run ' + str(run_id))
        if fact.expired:
            raise ArtifactExpired(operation, 'the artifact expired')
        return fact

    def run_artifacts(self, run_id):
        """Every artifact of one run, by its exact facts."""
        operation = 'artifacts'
        need_id(run_id, 'run id')
        rows = self._pages(operation, self.root + '/actions/runs/' + str(run_id) + '/artifacts', 'artifacts')
        facts = [self._artifact(operation, row, None) for row in rows]
        for fact in facts:
            if fact.run_id != run_id:
                raise Malformed(operation, 'an artifact is not from run ' + str(run_id))
        return facts

    def download_artifact(self, fact, out, limit):
        """Downloads an artifact ZIP to the new file `out`, at most `limit` bytes, and checks it
        against the artifact-service digest. A failure leaves no file."""
        operation = 'artifact download'
        if not isinstance(fact, ArtifactFact):
            raise ControllerError('the artifact is not an authenticated artifact fact')
        too_large = Malformed(operation, 'larger than ' + str(limit) + ' bytes')
        if fact.size > limit:
            raise too_large
        location = self._redirect(operation, self.root + '/actions/artifacts/' + str(fact.artifact_id) + '/zip')
        out = Path(out)
        cand.write_new(out, b'')
        try:
            digest, size = hashlib.sha256(), 0
            with open(str(out), 'wb') as handle:
                sink = _HashingSink(handle, digest, limit)
                try:
                    status = self.transport.download(location, sink, limit)
                except Timeout:
                    raise Uncertain(operation, 'outcome unknown (timeout)')
                except TransportFailure:
                    raise Uncertain(operation, 'outcome unknown (transport failure)')
                size = sink.size
                handle.flush()
                os.fsync(handle.fileno())
            if sink.size > limit:
                raise too_large
            if status != 200:
                raise StatusError(operation, 'HTTP ' + str(status), status)
            if digest.hexdigest() != fact.digest or size != fact.size:
                raise Malformed(operation, 'the bytes do not match the artifact digest')
        except BaseException:
            out.unlink()
            raise
        return DownloadFact(fact.artifact_id, fact.run_id, str(out), digest.hexdigest(), size)

    def _redirect(self, operation, path):
        self.repository()
        reason = None
        for attempt in range(READ_ATTEMPTS):
            if attempt:
                self.pause(attempt)
            try:
                response = self.transport.request('GET', path, None, BODY_MAX)
            except Timeout:
                reason = 'timeout'
                continue
            except TransportFailure:
                reason = 'transport failure'
                continue
            if response.status == 302:
                location = response.headers.get('location', '')
                if not (isinstance(location, str) and location.startswith('https://')):
                    raise Malformed(operation, 'the redirect is malformed')
                return location
            if is_rate_limited(response):
                reason = 'rate limited'
                continue
            if response.status >= 500:
                reason = 'HTTP ' + str(response.status)
                continue
            self._refuse(operation, response.status, read=True)
        raise Uncertain(operation, 'outcome unknown (' + reason + ')')

    def ref(self, name):
        """One exact ref (`tags/...` or `heads/...`); never a prefix match."""
        operation = 'ref'
        if not (isinstance(name, str) and REF_NAME_RE.fullmatch(name) and '..' not in name):
            raise ControllerError('the ref name is malformed')
        value, digest = self._object(operation, self.root + '/git/ref/' + '/'.join(quote(p) for p in name.split('/')))
        return self._ref(operation, value, digest, 'refs/' + name)

    @staticmethod
    def _ref(operation, value, digest, expected):
        if value.get('ref') != expected:
            raise Malformed(operation, 'the answer is not ' + expected)
        target = value.get('object')
        kind = target.get('type') if isinstance(target, dict) else None
        if kind not in ('tag', 'commit') or not isinstance(kind, str):
            raise Malformed(operation, 'the ref object is malformed')
        return RefFact(expected, kind, field(target, 'sha', 'sha', operation, 'ref object SHA'), digest)

    def tag(self, sha):
        """One annotated tag object, with its exact message bytes and direct target."""
        operation = 'tag'
        need_commit(sha, 'tag object SHA')
        value, digest = self._object(operation, self.root + '/git/tags/' + sha)
        return self._tag(operation, value, digest, sha)

    @staticmethod
    def _tag(operation, value, digest, sha=None):
        got = field(value, 'sha', 'sha', operation, 'tag object SHA')
        if sha is not None and got != sha:
            raise Malformed(operation, 'the answer is not object ' + sha)
        target = value.get('object')
        kind = target.get('type') if isinstance(target, dict) else None
        if not isinstance(kind, str):
            raise Malformed(operation, 'the tag target is malformed')
        message = field(value, 'message', str, operation, 'tag message')
        try:
            message_bytes = message.encode('utf-8')
        except UnicodeEncodeError:
            raise Malformed(operation, 'the tag message is malformed')
        return TagFact(got, field(value, 'tag', str, operation, 'tag name'), message_bytes, kind,
                       field(target, 'sha', 'sha', operation, 'tag target SHA'), digest)

    def commit(self, sha):
        operation = 'commit'
        need_commit(sha, 'commit')
        value, digest = self._object(operation, self.root + '/git/commits/' + sha)
        if value.get('sha') != sha:
            raise Malformed(operation, 'the answer is not commit ' + sha)
        parents = value.get('parents')
        if not (isinstance(parents, list) and len(parents) <= 16 and all(isinstance(p, dict) for p in parents)):
            raise Malformed(operation, 'the parents are malformed')
        tree = value.get('tree')
        return CommitFact(sha, field(tree if isinstance(tree, dict) else None, 'sha', 'sha', operation, 'tree SHA'),
                          tuple(field(p, 'sha', 'sha', operation, 'parent SHA') for p in parents), digest)

    def contents(self, path, ref_sha, limit=CONTENTS_MAX):
        """One file's exact bytes at commit `ref_sha`, checked against its Git blob id."""
        operation = 'contents'
        if not (isinstance(path, str) and PATH_RE.fullmatch(path) and '..' not in path.split('/')):
            raise ControllerError('the repository path is not allowlisted')
        need_commit(ref_sha, 'commit')
        limit = min(limit, CONTENTS_MAX)
        value, _ = self._object(operation, self.root + '/contents/' + path + '?ref=' + ref_sha)
        if value.get('type') != 'file' or value.get('path') != path or value.get('encoding') != 'base64':
            raise Malformed(operation, 'the answer is not the file ' + path)
        size = value.get('size')
        if not cand.is_int(size, 0):
            raise Malformed(operation, 'the file size is malformed')
        if size > limit:
            raise Malformed(operation, 'the file is larger than ' + str(limit) + ' bytes')
        content = field(value, 'content', str, operation, 'file content')
        if len(content) > 2 * limit + 4096:
            raise Malformed(operation, 'the file is larger than ' + str(limit) + ' bytes')
        try:
            data = base64.b64decode(''.join(content.split()), validate=True)
        except (ValueError, TypeError):
            raise Malformed(operation, 'the file content is malformed')
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        if len(data) != size or blob != value.get('sha'):
            raise Malformed(operation, 'the bytes do not match their Git blob id')
        return FileFact(path, ref_sha, blob, data, hashlib.sha256(data).hexdigest())

    def compare(self, base, head):
        """Ancestry of two exact commits (`status` is ahead, behind, identical or diverged)."""
        operation = 'compare'
        need_commit(base, 'base commit')
        need_commit(head, 'head commit')
        value, digest = self._object(operation, self.root + '/compare/' + base + '...' + head + '?per_page=1')
        status = value.get('status')
        if status not in ('ahead', 'behind', 'identical', 'diverged') or not isinstance(status, str):
            raise Malformed(operation, 'the status is malformed')
        ahead, behind = value.get('ahead_by'), value.get('behind_by')
        if not (cand.is_int(ahead, 0) and cand.is_int(behind, 0)):
            raise Malformed(operation, 'the distance is malformed')
        merge_base = value.get('merge_base_commit')
        return CompareFact(base, head, status, ahead, behind,
                           field(merge_base if isinstance(merge_base, dict) else None, 'sha', 'sha', operation,
                                 'merge base SHA'), digest)

    def _release(self, operation, value):
        assets = value.get('assets')
        if not (isinstance(assets, list) and all(isinstance(a, dict) for a in assets)):
            raise Malformed(operation, 'the release assets are malformed')
        return ReleaseFact(field(value, 'id', int, operation, 'release id'),
                           field(value, 'tag_name', str, operation, 'tag name'),
                           field(value, 'target_commitish', str, operation, 'release target'),
                           field(value, 'draft', bool, operation, 'draft flag'),
                           field(value, 'prerelease', bool, operation, 'prerelease flag'),
                           tuple(field(a, 'id', int, operation, 'asset id') for a in assets))

    def releases(self):
        """Every release, drafts included when the credentials can see them."""
        operation = 'releases'
        return [self._release(operation, row) for row in self._pages(operation, self.root + '/releases')]

    def release_by_tag(self, tag_name):
        operation = 'release'
        if not (isinstance(tag_name, str) and re.fullmatch(r'v0\.[1-9][0-9]{0,8}\.0', tag_name)):
            raise ControllerError('the release tag is not v<0.N.0>')
        value, _ = self._object(operation, self.root + '/releases/tags/' + tag_name)
        fact = self._release(operation, value)
        if fact.tag_name != tag_name:
            raise Malformed(operation, 'the answer is not release ' + tag_name)
        return fact

    def open_code_scanning_alerts(self, ref):
        """Every open code scanning alert on `ref` (for example `refs/heads/main`)."""
        operation = 'code scanning alerts'
        if not (isinstance(ref, str) and ref.startswith('refs/') and REF_NAME_RE.fullmatch(ref[5:])):
            raise ControllerError('the ref name is malformed')
        rows = self._pages(operation, self.root + '/code-scanning/alerts?state=open&ref=' + quote(ref))
        alerts = []
        for row in rows:
            rule, tool = row.get('rule'), row.get('tool')
            if not (isinstance(rule, dict) and isinstance(tool, dict)):
                raise Malformed(operation, 'an alert is malformed')
            severity = rule.get('security_severity_level')
            if severity is not None and not isinstance(severity, str):
                raise Malformed(operation, 'an alert severity is malformed')
            alerts.append(AlertFact(field(row, 'number', int, operation, 'alert number'),
                                    field(row, 'state', str, operation, 'alert state'),
                                    field(tool, 'name', str, operation, 'alert tool'), severity))
        return alerts

    def run_approvals(self, run_id):
        """The environment review history of one run (at most 100 rows)."""
        operation = 'run approvals'
        need_id(run_id, 'run id')
        self.repository()
        value, _ = self._get(operation, self.root + '/actions/runs/' + str(run_id) + '/approvals')
        if not (isinstance(value, list) and all(isinstance(row, dict) for row in value)):
            raise Malformed(operation, 'the answer is malformed')
        if len(value) > PAGE_ROWS:
            raise Malformed(operation, 'more than ' + str(PAGE_ROWS) + ' rows')
        facts = []
        for row in value:
            user, environments = row.get('user'), row.get('environments')
            if not (isinstance(user, dict) and isinstance(environments, list)
                    and all(isinstance(e, dict) for e in environments)):
                raise Malformed(operation, 'a review is malformed')
            comment = row.get('comment')
            if not isinstance(comment, str):
                raise Malformed(operation, 'a review comment is malformed')
            facts.append(ApprovalFact(field(row, 'state', str, operation, 'review state'), comment,
                                      field(user, 'id', int, operation, 'reviewer id'),
                                      field(user, 'type', str, operation, 'reviewer type'),
                                      tuple((field(e, 'id', int, operation, 'environment id'),
                                             field(e, 'name', str, operation, 'environment name'))
                                            for e in environments)))
        return facts

    # -- mutations: sent once, never retried --------------------------------------------------

    def _mutate(self, operation, path, payload):
        self.repository()
        body = json.dumps(payload, sort_keys=True, separators=(',', ':')).encode('utf-8')
        try:
            response = self.transport.request('POST', path, body, BODY_MAX)
        except Timeout:
            raise ReconcileRequired(operation, 'timeout')
        except TransportFailure:
            raise ReconcileRequired(operation, 'transport failure')
        if response.status in (409, 422, 429) or response.status >= 500:
            raise ReconcileRequired(operation, 'HTTP ' + str(response.status), response.status)
        if response.status != 201:
            self._refuse(operation, response.status, read=False)
        if len(response.body) > BODY_MAX:
            raise ReconcileRequired(operation, 'the answer is larger than ' + str(BODY_MAX) + ' bytes')
        try:
            value = self._json(operation, response)
        except Malformed:
            raise ReconcileRequired(operation, 'the answer is not JSON')
        if not isinstance(value, dict):
            raise ReconcileRequired(operation, 'the answer differs from the request')
        return value, hashlib.sha256(response.body).hexdigest()

    def create_tag_object(self, tag, message, commit_sha):
        """Creates the annotated tag object `reserve/v<V>` at a commit. This alone reserves nothing."""
        operation = 'create tag object'
        if not (isinstance(tag, str) and RESERVE_TAG_RE.fullmatch(tag)):
            raise ControllerError('the tag is not reserve/v<0.N.0>')
        if not isinstance(message, str) or len(message.encode('utf-8')) > 64 * cand.KIB:
            raise ControllerError('the tag message is not a string of at most 65536 bytes')
        need_commit(commit_sha, 'commit')
        value, digest = self._mutate(operation, self.root + '/git/tags',
                                     {'tag': tag, 'message': message, 'object': commit_sha, 'type': 'commit'})
        try:
            fact = self._tag(operation, value, digest)
        except Malformed:
            raise ReconcileRequired(operation, 'the answer differs from the request')
        if (fact.tag, fact.message_bytes, fact.object_type, fact.object_sha) != (
                tag, message.encode('utf-8'), 'commit', commit_sha):
            raise ReconcileRequired(operation, 'the answer differs from the request')
        return fact

    def create_ref(self, ref, object_sha):
        """Creates `refs/tags/reserve/v<V>` once. Its creation is the reservation commit point."""
        operation = 'create ref'
        if not (isinstance(ref, str) and RESERVE_REF_RE.fullmatch(ref)):
            raise ControllerError('the ref is not refs/tags/reserve/v<0.N.0>')
        need_commit(object_sha, 'tag object SHA')
        value, digest = self._mutate(operation, self.root + '/git/refs', {'ref': ref, 'sha': object_sha})
        try:
            fact = self._ref(operation, value, digest, ref)
        except Malformed:
            raise ReconcileRequired(operation, 'the answer differs from the request')
        if (fact.object_type, fact.object_sha) != ('tag', object_sha):
            raise ReconcileRequired(operation, 'the answer differs from the request')
        return fact


class _HashingSink(object):
    """Writes and hashes at most `limit + 1` bytes, then counts without writing."""

    def __init__(self, handle, digest, limit):
        self.handle, self.digest, self.limit, self.size = handle, digest, limit, 0

    def write(self, chunk):
        room = self.limit + 1 - self.size
        if room > 0:
            part = chunk[:room]
            self.handle.write(part)
            self.digest.update(part)
        self.size += len(chunk)
        return len(chunk)


# ---------------------------------------------------------------------------------------------
# The real transport
# ---------------------------------------------------------------------------------------------

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


class UrllibTransport(object):
    """HTTPS to api.github.com with a caller-supplied token, a 60-second deadline per request and
    bounded bodies. Redirects are returned, not followed; an artifact download goes to its
    redirect without credentials."""

    CHUNK = 64 * 1024

    def __init__(self, token, api=API, deadline=REQUEST_DEADLINE):
        if not (isinstance(token, str) and token):
            raise ControllerError('the GitHub transport has no credentials')
        self._token = token
        self.api = api
        self.deadline = deadline
        self._opener = urllib.request.build_opener(_NoRedirect)

    def _read(self, stream, sink, limit, started):
        size = 0
        while size <= limit:
            if time.monotonic() - started > self.deadline:
                raise Timeout()
            chunk = stream.read(min(self.CHUNK, limit + 1 - size))
            if not chunk:
                break
            sink.write(chunk)
            size += len(chunk)
        return size

    def _open(self, request, sink, limit):
        started = time.monotonic()
        try:
            try:
                stream = self._opener.open(request, timeout=self.deadline)
            except urllib.error.HTTPError as error:
                stream = error
            with stream:
                status = stream.getcode() if hasattr(stream, 'getcode') else stream.code
                headers = {k.lower(): v for k, v in stream.headers.items()}
                self._read(stream, sink, limit, started)
                return status, headers
        except Timeout:
            raise
        except socket.timeout:
            raise Timeout()
        except (OSError, http.client.HTTPException, AttributeError, ValueError):
            raise TransportFailure()

    def request(self, method, path, body=None, limit=None):
        limit = BODY_MAX if limit is None else limit
        request = urllib.request.Request(self.api + path, data=body, method=method)
        request.add_header('Accept', 'application/vnd.github+json')
        request.add_header('X-GitHub-Api-Version', '2022-11-28')
        request.add_header('Authorization', 'Bearer ' + self._token)
        request.add_header('User-Agent', 'rdy-release-controller')
        if body is not None:
            request.add_header('Content-Type', 'application/json')
        buffer = _Buffer()
        status, headers = self._open(request, buffer, limit)
        return Response(status, headers, buffer.data())

    def download(self, url, sink, limit):
        if not (isinstance(url, str) and url.startswith('https://')):
            raise TransportFailure()
        request = urllib.request.Request(url, method='GET')
        request.add_header('User-Agent', 'rdy-release-controller')
        status, _ = self._open(request, sink, limit)
        return status


class _Buffer(object):
    def __init__(self):
        self.parts = []

    def write(self, chunk):
        self.parts.append(chunk)

    def data(self):
        return b''.join(self.parts)
