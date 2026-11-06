#!/usr/bin/env python3
"""Generate and check the public agent guide bundle that rdy compiles in.

The input is a Markdown export of the guide at one frozen landing, plus the JSON projection of its
metadata table. Both are explicit files from the writer's export receipt; nothing here reads a
project. This script wraps the exported bodies in JSON and rewrites no prose: it removes only the
exporter's front matter and anchors. `check-binary` then runs the packaged binary and compares what it
delivers with the bundle: a comparison of source files alone would not prove what the binary holds.

A delivered reply is the bundle plus one generated line on the final row of each ruleset that owns
tools. `check-binary` derives those lines from the verified bundle and the checked-in tool table, and
compares whole rows: it strips nothing before the comparison.

Subcommands (each failure is one `embedded guide: ...` or `kungfu catalogue: ...` line on stderr and
exit 1; invalid arguments exit 2):
  bundle  write release/agent-guide.json, its digest, and the export receipt
  check   check a bundle and its digest; with a receipt, repeat `bundle` and require equal bytes
  check-binary  check a bundle and its digest, then the tool table, then compare a packaged rdy's rows
                and menu with them
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading

FIX = 're-export the guide and its digest from one frozen landing'
BUNDLE_PATH = 'release/agent-guide.json'
SIDECAR_PATH = 'release/agent-guide.sha256'

BUNDLE_MAX = 1048576
BODIES_MAX = 1048576
PROJECTION_MAX = 65536
RECEIPT_MAX = 65536
PATH_MAX = 1024
SIDECAR_BYTES = 65
DEPTH_MAX = 8
NAME_MAX = 64
TITLE_MAX = 256
WHEN_MAX = 1024
ROW_ID_MAX = 96
ROW_TEXT_MAX = 262144
REPLY_MAX = 16777216  # the reply limit of rdy_instructions
BINARY_DEADLINE = 60  # seconds for the packaged binary to answer
BINARY_DIFFERS = 'packaged binary differs from %s' % BUNDLE_PATH
TABLE_MAX = 32768
TABLE_ROWS_MAX = 256
TOOL_MAX = 96

# The rulesets, in delivery order. The bootstrap alone is required. A changed layout needs a plan
# change here and in crates/rdy-engine/build/guide.rs, never a more permissive parser.
NAMES = ('bootstrap', 'write', 'after-write', 'approvals', 'as-built', 'read', 'publish', 'review',
         'diagrams', 'parallel', 'project-ops', 'troubleshoot')
BOOTSTRAP_ROWS = ('rdy:bootstrap:before', 'rdy:bootstrap:approval', 'rdy:bootstrap:after')
ROWS_MAX = 14
APPROVAL_TEXT = "Never approve, tag or answer on your own judgment. Relay the person's exact words."
# The rulesets that own tools, and the tools no ruleset owns. The rules of the table are those of
# crates/rdy-engine/build/kungfu.rs.
OWNERS = ('bootstrap', 'write', 'after-write', 'approvals', 'read', 'publish', 'review', 'parallel', 'project-ops')
EXEMPT = ('rdy_instructions', 'rdy_session')
# The exported folders and index pages, and the page that holds the metadata table. None is a ruleset.
KNOWN_PATHS = ('index.md', 'agents/index.md', 'agents/agent-guide/index.md', 'agents/agent-guide/about.md')
PUBLIC_PROFILE = 'agent-guide'
DEVELOPMENT_PROFILE = 'agent-guide-dev'
DEVELOPMENT = '-dev'  # the ending of an edition whose bundle keeps the as-built marks

VERSION_RE = re.compile(r'[0-9]+\.[0-9]+\.[0-9]+')
EDITION_RE = re.compile(r'[0-9]+\.[0-9]+\.[0-9]+(?:-dev)?')
SHA256_RE = re.compile(r'[0-9a-f]{64}')
NAME_RE = re.compile(r'[a-z0-9]+(?:-[a-z0-9]+)*')
ROW_ID_RE = re.compile(r'[!-~]+')
BLOCK_RE = re.compile(r'p-[A-Z0-9]+')
ANCHOR = r'<a id="[^"]*"></a>'
ANCHOR_LINE_RE = re.compile(r'(?: {4})*' + ANCHOR)
LIST_ANCHOR_RE = re.compile(r'((?: {4})*(?:-   |[0-9]+\. +))' + ANCHOR)
LINE_PREFIX_RE = re.compile(r'(?:> ?| {4}|-   |[0-9]+\. +)*')
FENCE_RE = re.compile(r'(`{3,})[^`]*')
HTML_RE = re.compile(r'<[A-Za-z/!?]')
TOOL_RE = re.compile(r'[a-z0-9_]+')
TABLE_RULESET_RE = re.compile(r'[!-~]*')


class GuideError(Exception):
    """One refusal. Its text is the whole diagnostic, with the fix."""

    def __init__(self, message):
        super().__init__('embedded guide: %s; %s' % (message, FIX))


class CatalogueError(Exception):
    """One refusal of the tool table or the values derived with it. It never prints a value."""

    def __init__(self, reason):
        super().__init__('kungfu catalogue: %s' % reason)


def invalid(field, reason):
    return GuideError('invalid %s: %s' % (field, reason))


def exceeds(field, limit):
    return GuideError('%s exceeds %s' % (field, limit))


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def edit_distance(a, b):
    row = list(range(len(b) + 1))
    for i, ca in enumerate(a):
        prev, row[0] = row[0], i + 1
        for j, cb in enumerate(b):
            cur = row[j + 1]
            row[j + 1] = min(prev + (ca != cb), row[j] + 1, cur + 1)
            prev = cur
    return row[len(b)]


def nearest(word, known):
    """The did-you-mean rule of rdy-model's did_you_mean::nearest, over a closed list."""
    close = [(edit_distance(word, k), i, k) for i, k in enumerate(known)]
    close = [c for c in close if c[0] <= 2]
    if close:
        return min(close)[2]
    for k in known:
        short, long = (k, word) if len(k) <= len(word) else (word, k)
        if len(short) >= 3 and long.startswith(short):
            return k
    return None


def read_bounded(path, limit, field):
    """The bytes of `path`, refused by their size before a buffer for them exists."""
    try:
        with open(path, 'rb') as f:
            if os.fstat(f.fileno()).st_size > limit:
                raise exceeds(field, '%d bytes' % limit)
            data = f.read(limit + 1)
    except FileNotFoundError:
        raise GuideError('missing %s' % field) from None
    except OSError as e:
        raise GuideError('missing %s: %s' % (field, e.strerror)) from None
    if len(data) > limit:
        raise exceeds(field, '%d bytes' % limit)
    return data


def check_depth(text):
    """Refuses nesting past the bound before a recursive parser sees it."""
    depth = 0
    in_string = False
    escaped = False
    for c in text:
        if in_string:
            if escaped:
                escaped = False
            elif c == '\\':
                escaped = True
            elif c == '"':
                in_string = False
        elif c == '"':
            in_string = True
        elif c in '[{':
            depth += 1
            if depth > DEPTH_MAX:
                raise exceeds('depth', DEPTH_MAX)
        elif c in ']}':
            depth -= 1


class DuplicateKey(Exception):
    pass


def no_duplicates(pairs):
    out = {}
    for key, value in pairs:
        if key in out:
            raise DuplicateKey()
        out[key] = value
    return out


def refuse_constant(_):
    raise ValueError('a non-finite number')


def strict_json(data, field):
    """One JSON document: valid UTF-8, bounded depth, no duplicate key, no non-finite number."""
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError:
        raise invalid(field, 'invalid UTF-8') from None
    check_depth(text)
    try:
        return json.loads(text, object_pairs_hook=no_duplicates, parse_constant=refuse_constant)
    except DuplicateKey:
        raise invalid(field, 'duplicate key') from None
    except ValueError:
        raise invalid(field, 'invalid JSON') from None


def closed(field, value, keys):
    """`value` as an object with no key outside `keys`. A missing key reads as a wrong type later."""
    if not isinstance(value, dict):
        raise invalid(field, 'wrong type')
    for key in sorted(value):
        if key not in keys:
            near = nearest(key, keys)
            raise invalid(field, 'unknown key %s%s' % (key, '; did you mean %s?' % near if near else ''))
    return value


def text_of(field, value):
    if not isinstance(value, str):
        raise invalid(field, 'wrong type')
    return value


def bool_of(field, value):
    if not isinstance(value, bool):
        raise invalid(field, 'wrong type')
    return value


def list_of(field, value):
    if not isinstance(value, list):
        raise invalid(field, 'wrong type')
    return value


def bounded_text(field, value, limit):
    text = text_of(field, value)
    if len(text.encode('utf-8', 'surrogatepass')) > limit:
        raise exceeds(field, '%d bytes' % limit)
    if not text.strip():
        raise invalid(field, 'empty text')
    return text


def checked_name(field, value, index, seen):
    name = text_of(field, value)
    if len(name.encode('utf-8', 'surrogatepass')) > NAME_MAX:
        raise exceeds(field, '%d bytes' % NAME_MAX)
    if not NAME_RE.fullmatch(name):
        raise invalid(field, 'invalid name')
    if name in seen:
        raise invalid(field, 'duplicate ruleset')
    seen.add(name)
    if NAMES[index] != name:
        raise invalid(field, 'unexpected order')
    return name


def has_mark(text, first_row):
    """A mark paragraph, a marked note, a marked lede in a body's first row, or the mark of the
    approval paragraph, which ends the slice before it: what the development profile adds."""
    if text.endswith('\n\nPlanned ') or any(line in ('**Planned**', '> **Planned**') for line in text.split('\n')):
        return True
    _, sep, rest = text.partition('\n\n')
    return first_row and bool(sep) and rest.startswith('Planned ')


def checked_path(field, value, seen):
    path = text_of(field, value)
    if len(path.encode('utf-8', 'surrogatepass')) > PATH_MAX:
        raise exceeds(field, '%d bytes' % PATH_MAX)
    parts = path.split('/')
    if (not path or '\\' in path or ':' in path or '\0' in path
            or any(part in ('', '.', '..') for part in parts)):
        raise invalid(field, 'path escapes export')
    folded = path.casefold()
    if folded in seen or path in KNOWN_PATHS:
        raise invalid(field, 'duplicate ruleset')
    seen.add(folded)
    return path


def read_projection(path):
    value = strict_json(read_bounded(path, PROJECTION_MAX, 'projection'), 'projection')
    value = closed('projection', value, ('schema', 'rulesets'))
    if type(value.get('schema')) is not int or value['schema'] != 1:
        raise invalid('projection.schema', 'unsupported schema')
    entries = list_of('projection.rulesets', value.get('rulesets'))
    if len(entries) > len(NAMES):
        raise exceeds('rulesets', len(NAMES))
    if len(entries) < len(NAMES):
        raise invalid('rulesets', 'unexpected order')
    seen_names = set()
    seen_paths = set()
    out = []
    for i, entry in enumerate(entries):
        field = 'rulesets[%d]' % i
        entry = closed(field, entry, ('name', 'title', 'when', 'required', 'path', 'approval'))
        name = checked_name(field + '.name', entry.get('name'), i, seen_names)
        title = bounded_text(field + '.title', entry.get('title'), TITLE_MAX)
        when = bounded_text(field + '.when', entry.get('when'), WHEN_MAX)
        required = bool_of(field + '.required', entry.get('required'))
        if required != (i == 0):
            raise invalid(field + '.required', 'invalid required set')
        path = checked_path(field + '.path', entry.get('path'), seen_paths)
        approval = entry.get('approval')
        if i == 0:
            approval = closed(field + '.approval', approval, ('block', 'text'))
            block = text_of(field + '.approval.block', approval.get('block'))
            if not BLOCK_RE.fullmatch(block):
                raise invalid(field + '.approval.block', 'invalid provenance')
            if text_of(field + '.approval.text', approval.get('text')) != APPROVAL_TEXT:
                raise invalid(field + '.approval.text', 'invalid approval lock')
        elif approval is not None:
            raise invalid(field + '.approval', 'invalid approval lock')
        out.append({'name': name, 'title': title, 'when': when, 'required': required, 'path': path})
    return out


def walk_export(export, ruleset_paths):
    """Every file of the export as {relative path: absolute path}, refusing any path the export
    must not hold. It stops at the first unknown name, so a hostile tree costs nothing."""
    expected = set(ruleset_paths) | set(KNOWN_PATHS)
    folders = {''}
    for path in expected:
        parts = path.split('/')
        folders.update('/'.join(parts[:i]) for i in range(1, len(parts)))
    found = {}
    stack = ['']
    while stack:
        rel = stack.pop()
        try:
            entries = os.scandir(os.path.join(export, rel))
        except OSError as e:
            raise GuideError('missing export: %s' % e.strerror) from None
        with entries:
            for entry in entries:
                child = rel + '/' + entry.name if rel else entry.name
                if entry.is_symlink():
                    raise invalid(child, 'path escapes export')
                if entry.is_dir(follow_symlinks=False) and child in folders:
                    stack.append(child)
                elif entry.is_file(follow_symlinks=False) and child in expected:
                    found[child] = entry.path
                else:
                    near = nearest(child, sorted(expected))
                    raise invalid('export', 'unknown key %s%s' % (child, '; did you mean %s?' % near if near else ''))
    return found


def strip_front_matter(data, field, title):
    """The body after the exporter's front matter, whose defined end is `---`, then a blank line."""
    if not data.startswith(b'---\n'):
        raise invalid(field, 'invalid provenance')
    end = data.find(b'\n---\n\n')
    if end < 0:
        raise invalid(field, 'invalid provenance')
    lines = data[len(b'---\n'):end].split(b'\n')
    if not lines[0].startswith(b'title: ') or any(not line.startswith(b'rdy_id: ') for line in lines[1:]):
        raise invalid(field, 'invalid provenance')
    try:
        named = json.loads(lines[0][len(b'title: '):].decode('utf-8'))
    except ValueError:
        raise invalid(field, 'invalid provenance') from None
    if named != title:
        raise invalid(field, 'invalid provenance')
    return data[end + len(b'\n---\n\n'):]


def strip_anchors(text, field):
    """`text` without the exporter's anchors: every other byte stays, and any other HTML line
    refuses. An anchor has a line of its own, or stands right after a list marker, where the item's
    text follows on the next line. Code fences are text, so nothing in one counts."""
    lines = text.split('\n')
    out = []
    fence = None
    i = 0
    while i < len(lines):
        line = lines[i]
        content = line[LINE_PREFIX_RE.match(line).end():]
        if fence is not None:
            if content == fence:
                fence = None
            out.append(line)
            i += 1
            continue
        opening = FENCE_RE.fullmatch(content)
        if opening:
            fence = opening.group(1)
            out.append(line)
            i += 1
            continue
        if ANCHOR_LINE_RE.fullmatch(line):
            i += 1
            continue
        item = LIST_ANCHOR_RE.fullmatch(line)
        if item:
            marker = item.group(1)
            pad = ' ' * len(marker)
            # The exporter leaves a blank line after an anchor before a table, a code block or a note.
            following = i + 2 if i + 1 < len(lines) and lines[i + 1] == '' else i + 1
            if following >= len(lines) or not lines[following].startswith(pad):
                raise invalid('%s:%d' % (field, i + 1), 'invalid provenance')
            lines[following] = marker + lines[following][len(pad):]
            i = following
            continue
        if HTML_RE.match(content):
            raise invalid('%s:%d' % (field, i + 1), 'invalid provenance')
        out.append(line)
        i += 1
    return '\n'.join(out)


def split_bootstrap(body, field, development):
    """The three bootstrap slices. The separators stay in the slices beside the paragraph."""
    if body.count(APPROVAL_TEXT) != 1:
        raise invalid(field, 'ambiguous approval paragraph')
    before, _, after = body.partition(APPROVAL_TEXT)
    if not (after == '' or after == '\n' or after.startswith('\n\n')):
        raise invalid(field, 'ambiguous approval paragraph')
    if before.endswith('\n\nPlanned '):
        if not development:
            raise invalid(field, 'invalid provenance')
    elif not before.endswith('\n\n'):
        raise invalid(field, 'ambiguous approval paragraph')
    return before, APPROVAL_TEXT, after


def assemble(export, projection, landing, version, profile):
    """The canonical bundle bytes and the input digests."""
    if profile not in (PUBLIC_PROFILE, DEVELOPMENT_PROFILE):
        raise invalid('profile', 'invalid provenance')
    if not SHA256_RE.fullmatch(landing):
        raise invalid('design_landing', 'invalid provenance')
    if not VERSION_RE.fullmatch(version):
        raise invalid('edition', 'invalid provenance')
    development = profile == DEVELOPMENT_PROFILE
    metadata = read_bounded(projection, PROJECTION_MAX, 'projection')
    entries = read_projection(projection)
    paths = [e['path'] for e in entries]
    found = walk_export(export, paths)
    for path in paths:
        if path not in found:
            raise invalid(path, 'missing document')
    total = sum(os.lstat(found[path]).st_size for path in paths)
    if total > BODIES_MAX:
        raise exceeds('bodies', '%d bytes' % BODIES_MAX)
    budget = BODIES_MAX
    files = {}
    for rel in sorted(found):
        if rel in paths:
            continue
        files[rel] = sha256(read_bounded(found[rel], BODIES_MAX, rel))
    rulesets = []
    for i, entry in enumerate(entries):
        path = entry['path']
        data = read_bounded(found[path], budget, 'bodies')
        budget -= len(data)
        files[path] = sha256(data)
        stripped = strip_front_matter(data, path, entry['title'])
        try:
            text = stripped.decode('utf-8')
        except UnicodeDecodeError:
            raise invalid(path, 'invalid UTF-8') from None
        body = strip_anchors(text, path)
        if i == 0:
            texts = split_bootstrap(body, path, development)
            ids = BOOTSTRAP_ROWS
        else:
            texts = (body,)
            ids = ('rdy:%s:body' % entry['name'],)
        rows = [{'id': row_id, 'text': row_text, 'locked': row_id == BOOTSTRAP_ROWS[1]}
                for row_id, row_text in zip(ids, texts)]
        rulesets.append({'name': entry['name'], 'title': entry['title'], 'when': entry['when'],
                         'required': entry['required'], 'rows': rows})
    value = {'schema': 1, 'edition': version + (DEVELOPMENT if development else ''),
             'design_landing': landing, 'rulesets': rulesets}
    check_shape(value)
    raw = canonical(value)
    if len(raw) > BUNDLE_MAX:
        raise exceeds(BUNDLE_PATH, '%d bytes' % BUNDLE_MAX)
    return raw, sha256(metadata), files


def canonical(value):
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))
    try:
        return text.encode('utf-8') + b'\n'
    except UnicodeEncodeError:  # a lone surrogate from a JSON escape
        raise invalid(BUNDLE_PATH, 'invalid UTF-8') from None


def check_shape(value):
    """The closed shape of a bundle: the rules of crates/rdy-engine/build/guide.rs."""
    value = closed('bundle', value, ('schema', 'edition', 'design_landing', 'rulesets'))
    if type(value.get('schema')) is not int or value['schema'] != 1:
        raise invalid('schema', 'unsupported schema')
    edition = text_of('edition', value.get('edition'))
    if not EDITION_RE.fullmatch(edition):
        raise invalid('edition', 'invalid provenance')
    if not SHA256_RE.fullmatch(text_of('design_landing', value.get('design_landing'))):
        raise invalid('design_landing', 'invalid provenance')
    sets = list_of('rulesets', value.get('rulesets'))
    if len(sets) > len(NAMES):
        raise exceeds('rulesets', len(NAMES))
    total = sum(len(s['rows']) for s in sets if isinstance(s, dict) and isinstance(s.get('rows'), list))
    if total > ROWS_MAX:
        raise exceeds('rows', ROWS_MAX)
    development = edition.endswith(DEVELOPMENT)
    seen_sets = set()
    seen_rows = set()
    for i, ruleset in enumerate(sets):
        field = 'rulesets[%d]' % i
        ruleset = closed(field, ruleset, ('name', 'title', 'when', 'required', 'rows'))
        name = checked_name(field + '.name', ruleset.get('name'), i, seen_sets)
        bounded_text(field + '.title', ruleset.get('title'), TITLE_MAX)
        bounded_text(field + '.when', ruleset.get('when'), WHEN_MAX)
        if bool_of(field + '.required', ruleset.get('required')) != (i == 0):
            raise invalid(field + '.required', 'invalid required set')
        rows = list_of(field + '.rows', ruleset.get('rows'))
        expected = BOOTSTRAP_ROWS if i == 0 else ('rdy:%s:body' % name,)
        for j, row in enumerate(rows):
            row_field = '%s.rows[%d]' % (field, j)
            row = closed(row_field, row, ('id', 'text', 'locked'))
            row_id = text_of(row_field + '.id', row.get('id'))
            if len(row_id.encode('utf-8', 'surrogatepass')) > ROW_ID_MAX:
                raise exceeds(row_field + '.id', '%d bytes' % ROW_ID_MAX)
            if not ROW_ID_RE.fullmatch(row_id):
                raise invalid(row_field + '.id', 'invalid id')
            if row_id in seen_rows:
                raise invalid(row_field + '.id', 'duplicate row')
            seen_rows.add(row_id)
            if j >= len(expected) or expected[j] != row_id:
                raise invalid(row_field + '.id', 'unexpected order')
            text = bounded_text(row_field + '.text', row.get('text'), ROW_TEXT_MAX)
            is_approval = i == 0 and j == 1
            if bool_of(row_field + '.locked', row.get('locked')) != is_approval:
                raise invalid(row_field + '.locked', 'invalid approval lock')
            if is_approval and text != APPROVAL_TEXT:
                raise invalid(row_field + '.text', 'invalid approval lock')
            if not development and has_mark(text, j == 0):
                raise invalid(row_field + '.text', 'invalid provenance')
        if len(rows) != len(expected):
            raise invalid(field + '.rows', 'unexpected order')
    if len(sets) < len(NAMES):
        raise invalid('rulesets', 'unexpected order')


def write_atomic(path, data):
    path = Path(path)
    fd, temp = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + '.')
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
        os.replace(temp, str(path))
    except BaseException:
        try:
            os.unlink(temp)
        except OSError:
            pass
        raise


def bundle(export, projection, landing, version, profile, out, sidecar, receipt):
    """Writes the bundle, its digest and the receipt that binds every input. Returns the digest."""
    raw, metadata_sha256, files = assemble(export, projection, landing, version, profile)
    digest = sha256(raw)
    write_atomic(out, raw)
    write_atomic(sidecar, digest.encode('ascii') + b'\n')
    write_atomic(receipt, canonical({
        'schema': 1, 'design_landing': landing, 'profile': profile, 'version': version,
        'metadata_sha256': metadata_sha256, 'files': files, 'bundle_sha256': digest,
    }))
    return digest


def check(out, sidecar, receipt, export, projection):
    """Checks the bundle against its digest and closed shape. With a receipt, also checks each
    input's digest, repeats the transformation and requires byte equality."""
    raw = read_bounded(out, BUNDLE_MAX, BUNDLE_PATH)
    named = read_bounded(sidecar, SIDECAR_BYTES, SIDECAR_PATH)
    if len(named) != SIDECAR_BYTES or not named.endswith(b'\n') or not SHA256_RE.fullmatch(named[:-1].decode('latin-1')):
        raise invalid(SIDECAR_PATH, 'invalid provenance')
    digest = sha256(raw)
    if digest.encode('ascii') != named[:-1]:
        raise GuideError('%s does not match %s' % (BUNDLE_PATH, SIDECAR_PATH))
    value = strict_json(raw, BUNDLE_PATH)
    check_shape(value)
    if canonical(value) != raw:
        raise invalid(BUNDLE_PATH, 'invalid JSON')
    if receipt is None:
        return digest
    bound = strict_json(read_bounded(receipt, RECEIPT_MAX, 'receipt'), 'receipt')
    bound = closed('receipt', bound, ('schema', 'design_landing', 'profile', 'version', 'metadata_sha256',
                                      'files', 'bundle_sha256'))
    if type(bound.get('schema')) is not int or bound['schema'] != 1:
        raise invalid('receipt.schema', 'unsupported schema')
    landing = text_of('receipt.design_landing', bound.get('design_landing'))
    version = text_of('receipt.version', bound.get('version'))
    profile = text_of('receipt.profile', bound.get('profile'))
    remade, metadata_sha256, files = assemble(export, projection, landing, version, profile)
    if (bound.get('metadata_sha256') != metadata_sha256 or bound.get('files') != files
            or bound.get('bundle_sha256') != digest or remade != raw):
        raise invalid('receipt', 'invalid provenance')
    return digest


def table_row(row):
    """The tool and ruleset of one row of a parsed table, in the order the build checks its fields."""
    if not isinstance(row, dict) or set(row) != {'tool', 'ruleset', 'response'}:
        raise CatalogueError('invalid table')
    tool, ruleset, response = row['tool'], row['ruleset'], row['response']
    if not isinstance(tool, str) or not isinstance(response, str) or not (ruleset is None or isinstance(ruleset, str)):
        raise CatalogueError('invalid table')
    if len(tool) > TOOL_MAX or not TOOL_RE.fullmatch(tool):
        raise CatalogueError('invalid table')
    return tool, ruleset, response


def read_table(path):
    """The checked tool table as {owner name: its tools}, by the rules of the build's validator."""
    try:
        with open(path, 'rb') as f:
            if os.fstat(f.fileno()).st_size > TABLE_MAX:
                raise CatalogueError('table exceeds %d bytes' % TABLE_MAX)
            data = f.read(TABLE_MAX + 1)
    except OSError:
        raise CatalogueError('invalid table') from None
    if len(data) > TABLE_MAX:
        raise CatalogueError('table exceeds %d bytes' % TABLE_MAX)
    try:
        rows = json.loads(data.decode('utf-8'), object_pairs_hook=no_duplicates, parse_constant=refuse_constant)
    except (ValueError, DuplicateKey, RecursionError):
        raise CatalogueError('invalid table') from None
    if not isinstance(rows, list):
        raise CatalogueError('invalid table')
    if len(rows) > TABLE_ROWS_MAX:
        raise CatalogueError('table exceeds %d rows' % TABLE_ROWS_MAX)
    owners = {name: [] for name in OWNERS}
    exempt = 0
    seen = set()
    previous = None
    for row in rows:
        tool, ruleset, response = table_row(row)
        if tool in seen:
            raise CatalogueError('duplicate tool %s' % tool)
        if previous is not None and tool < previous:
            raise CatalogueError('invalid table')
        if ruleset is None:
            if tool not in EXEMPT:
                raise CatalogueError('invalid table')
            exempt += 1
        else:
            if not TABLE_RULESET_RE.fullmatch(ruleset):
                raise CatalogueError('invalid table')
            if not ruleset.startswith('rdy:') or ruleset[len('rdy:'):] not in owners:
                raise CatalogueError('unknown ruleset %s' % ruleset)
            owners[ruleset[len('rdy:'):]].append(tool)
        if response == 'refuse':
            raise CatalogueError('non-warn response %s' % tool)
        if response != 'warn':
            raise CatalogueError('invalid table')
        seen.add(tool)
        previous = tool
    if exempt != len(EXEMPT) or not all(owners.values()):
        raise CatalogueError('invalid table')
    return owners


def kungfu_value(texts):
    """`kf-` and the first eight digits of the SHA-256 of `texts`, joined with no separator."""
    return 'kf-' + sha256(''.join(texts).encode('utf-8'))[:8]


def kungfu_line(value, tools):
    """The line added to an owner's final row. It ends with LF, and its tools are in ascending order."""
    return 'On every call to a tool in this ruleset, pass kungfu: "%s". Tools: %s.\n' % (value, ', '.join(sorted(tools)))


def expected_reply(value, table):
    """The rows and menu a binary built from bundle `value` and tool `table` delivers with --full and
    no other rule. Each value hashes source rows only; the line is added to a copy of the final row."""
    values = {}
    for ruleset in value['rulesets']:
        if ruleset['name'] in table:
            named = kungfu_value(row['text'] for row in ruleset['rows'])
            for owner, other in values.items():
                if other == named:
                    raise CatalogueError('value collision rdy:%s rdy:%s' % (owner, ruleset['name']))
            values[ruleset['name']] = named
    rows = []
    menu = []
    for ruleset in value['rulesets']:
        name = ruleset['name']
        handle = 'rdy:' + name
        size = 0
        final = len(ruleset['rows']) - 1
        for index, row in enumerate(ruleset['rows']):
            text = row['text']
            if name in table and index == final:
                text += ('' if text.endswith('\n') else '\n') + kungfu_line(values[name], table[name])
            rows.append({'id': row['id'], 'ruleset': handle, 'text': text, 'layer': 'rdy',
                         'locked': row['locked'], 'reason': []})
            size += len(text.encode('utf-8'))
        # estimate_tokens in crates/rdy-engine/src/instructions.rs: UTF-8 bytes / 4, rounded up.
        menu.append({'id': handle, 'name': ruleset['title'], 'when': ruleset['when'], 'layer': 'rdy',
                     'required': ruleset['required'], 'tokens': (size + 3) // 4, 'hints': []})
    return rows, menu


def binary_output(binary, root, deadline):
    """The packaged binary's stdout for `instructions --full --json`, or None when it fails. It runs
    in an empty folder with an empty home below `root`, with no variable of the caller's but PATH: no
    person layer, user config or project can reach the reply."""
    home = os.path.join(root, 'home')
    work = os.path.join(root, 'work')
    os.mkdir(home)
    os.mkdir(work)
    env = {'HOME': home, 'USERPROFILE': home, 'XDG_CONFIG_HOME': os.path.join(home, '.config'),
           'APPDATA': os.path.join(home, 'AppData')}
    if 'PATH' in os.environ:
        env['PATH'] = os.environ['PATH']
    try:
        child = subprocess.Popen([os.path.abspath(binary), 'instructions', '--full', '--json'], cwd=work, env=env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    except FileNotFoundError:
        raise GuideError('missing packaged binary') from None
    except OSError:
        return None
    timer = threading.Timer(deadline, child.kill)
    timer.start()
    try:
        data = child.stdout.read(REPLY_MAX + 1)
        if len(data) > REPLY_MAX:
            child.kill()
        status = child.wait()
    finally:
        timer.cancel()
        if child.poll() is None:
            child.kill()
            child.wait()
        child.stdout.close()
    return None if status != 0 or len(data) > REPLY_MAX else data


def check_binary(binary, bundle_path, sidecar, table_path, deadline=BINARY_DEADLINE):
    """Checks the bundle, then the tool table and the values, then runs the packaged binary and
    compares every delivered built-in row and menu field with them, order included. A row of any other
    source is an extra row: the home is empty."""
    digest = check(bundle_path, sidecar, None, None, None)
    value = strict_json(read_bounded(bundle_path, BUNDLE_MAX, BUNDLE_PATH), BUNDLE_PATH)
    rows, menu = expected_reply(value, read_table(table_path))
    root = tempfile.mkdtemp(prefix='rdyguide')
    try:
        data = binary_output(binary, root, deadline)
    finally:
        shutil.rmtree(root, ignore_errors=True)
    if data is None:
        raise GuideError(BINARY_DIFFERS)
    try:
        reply = strict_json(data, 'reply')
    except GuideError:
        raise GuideError(BINARY_DIFFERS) from None
    # Compared as JSON text, where 1 is not true and 1.0 is not 1.
    if not isinstance(reply, dict) or (json.dumps([reply.get('instructions'), reply.get('menu')], sort_keys=True)
                                       != json.dumps([rows, menu], sort_keys=True)):
        raise GuideError(BINARY_DIFFERS)
    return digest


def main(argv=None):
    parser = argparse.ArgumentParser(description='The public agent guide bundle.')
    commands = parser.add_subparsers(dest='command', required=True)
    make = commands.add_parser('bundle')
    make.add_argument('--export', required=True)
    make.add_argument('--projection', required=True)
    make.add_argument('--landing', required=True)
    make.add_argument('--version', required=True)
    make.add_argument('--profile', required=True)
    make.add_argument('--out', required=True)
    make.add_argument('--sidecar', required=True)
    make.add_argument('--receipt', required=True)
    verify = commands.add_parser('check')
    verify.add_argument('--bundle', required=True)
    verify.add_argument('--sidecar', required=True)
    verify.add_argument('--receipt')
    verify.add_argument('--export')
    verify.add_argument('--projection')
    packaged = commands.add_parser('check-binary')
    packaged.add_argument('--binary', required=True)
    packaged.add_argument('--bundle', required=True)
    packaged.add_argument('--sidecar', required=True)
    packaged.add_argument('--table', required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'bundle':
            digest = bundle(args.export, args.projection, args.landing, args.version, args.profile,
                            args.out, args.sidecar, args.receipt)
        elif args.command == 'check-binary':
            digest = check_binary(args.binary, args.bundle, args.sidecar, args.table)
        else:
            if args.receipt and not (args.export and args.projection):
                parser.error('--receipt needs --export and --projection')
            digest = check(args.bundle, args.sidecar, args.receipt, args.export, args.projection)
    except (GuideError, CatalogueError) as e:
        print(e, file=sys.stderr)
        return 1
    print(digest)
    return 0


if __name__ == '__main__':
    sys.exit(main())
