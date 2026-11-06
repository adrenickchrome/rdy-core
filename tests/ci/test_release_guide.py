"""The public guide bundle: exported bytes in, deterministic bounded JSON out, no prose changed."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/release/guide.py'


def load():
    spec = importlib.util.spec_from_file_location('guide', SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules['guide'] = module
    spec.loader.exec_module(module)
    return module


GUIDE = load()
FIX = 're-export the guide and its digest from one frozen landing'
L = 'a' * 64
V = '0.43.0'
FOLDER = 'agents/agent-guide/'
APPROVAL = "Never approve, tag or answer on your own judgment. Relay the person's exact words."
NAMES = ('bootstrap', 'write', 'after-write', 'approvals', 'as-built', 'read', 'publish', 'review',
         'diagrams', 'parallel', 'project-ops', 'troubleshoot')
KNOWN = ('index.md', 'agents/index.md', FOLDER + 'index.md', FOLDER + 'about.md')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def front(title, doc):
    return ('---\ntitle: %s\nrdy_id: %s\n---\n\n' % (json.dumps(title), doc)).encode()


def bootstrap(mark=''):
    return (front('Start with rdy', 'd-1') + (
        '# Start with rdy\n\n'
        '<a id="rdy-holds"></a>\n'
        'rdy holds what a person intends.\n\n'
        '<a id="never-approve"></a>\n'
        + mark + APPROVAL + '\n\n'
        '<a id="for-every-change"></a>\n'
        'For every change:\n\n'
        '1.  `rdy_session`: your own actor.\n').encode())


def body_of(name):
    return front(name.title(), 'd-' + name) + (
        '# %s\n\n'
        '<a id="plain"></a>\n'
        'Caf\u00e9 \u2014 \u65e5\u672c\u8a9e, a "quote", a back\\\\slash and \u2028 a separator.\n'
        'A CRLF line\r\nand the next line.\n\n'
        '-   <a id="list-item"></a>\n'
        '    First item.\n'
        '-   Second item.\n\n'
        '1.  <a id="ordered-item"></a>\n'
        '    One.\n\n'
        '    <a id="second-block"></a>\n'
        '    Second block of the item.\n\n'
        '    -   <a id="nested-item"></a>\n'
        '        Nested.\n\n'
        '-   <a id="list-table"></a>\n\n'
        '    | c |\n\n'
        '<a id="table-block"></a>\n\n'
        '| a | b |\n'
        '| --- | --- |\n\n'
        '```text\n'
        '<a id="kept-in-code"></a>\n'
        '<html>\n'
        '```\n\n'
        'Last line, then a final newline.\n' % name).encode()


def delivered(name):
    """What the row of `name` must hold: front matter and anchors gone, no other byte changed."""
    return (
        '# %s\n\n'
        'Caf\u00e9 \u2014 \u65e5\u672c\u8a9e, a "quote", a back\\\\slash and \u2028 a separator.\n'
        'A CRLF line\r\nand the next line.\n\n'
        '-   First item.\n'
        '-   Second item.\n\n'
        '1.  One.\n\n'
        '    Second block of the item.\n\n'
        '    -   Nested.\n\n'
        '-   | c |\n\n'
        '\n'
        '| a | b |\n'
        '| --- | --- |\n\n'
        '```text\n'
        '<a id="kept-in-code"></a>\n'
        '<html>\n'
        '```\n\n'
        'Last line, then a final newline.\n' % name)


def projection():
    rows = []
    for name in NAMES:
        title = 'Start with rdy' if name == 'bootstrap' else name.title()
        rows.append({
            'name': name, 'title': title, 'when': 'when ' + name, 'required': name == 'bootstrap',
            'path': FOLDER + name + '.md',
            'approval': {'block': 'p-TPWCZ', 'text': APPROVAL} if name == 'bootstrap' else None,
        })
    return {'schema': 1, 'rulesets': rows}


class World:
    def __init__(self, test):
        self.dir = Path(tempfile.mkdtemp(prefix='rdyguide'))
        test.addCleanup(shutil.rmtree, self.dir, True)
        self.test = test
        self.export = self.dir / 'export'
        self.projection = self.dir / 'projection.json'
        self.out = self.dir / 'agent-guide.json'
        self.sidecar = self.dir / 'agent-guide.sha256'
        self.receipt = self.dir / 'receipt.json'
        self.put('index.md', front('Root', 'd-r') + b'# Root\n')
        self.put('agents/index.md', front('Agents', 'd-a') + b'# Agents\n')
        self.put(FOLDER + 'index.md', front('Agent guide', 'd-f') + b'# Agent guide\n')
        self.put(FOLDER + 'about.md', front('About', 'd-b') + b'# About\n')
        self.put(FOLDER + 'bootstrap.md', bootstrap())
        for name in NAMES[1:]:
            self.put(FOLDER + name + '.md', body_of(name))
        self.write_projection(projection())

    def put(self, rel, data):
        path = self.export / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def write_projection(self, value):
        self.projection.write_bytes(json.dumps(value).encode())

    def bundle(self, profile='agent-guide', landing=L, version=V):
        return GUIDE.bundle(self.export, self.projection, landing, version, profile,
                            self.out, self.sidecar, self.receipt)

    def refused(self, needle, profile='agent-guide'):
        with self.test.assertRaises(GUIDE.GuideError) as caught:
            self.bundle(profile)
        self.test.assertIn(needle, str(caught.exception))
        self.test.assertTrue(str(caught.exception).startswith('embedded guide: '), str(caught.exception))
        self.test.assertTrue(str(caught.exception).endswith(FIX), str(caught.exception))
        self.test.assertFalse(self.out.exists(), 'a refused bundle writes nothing')

    def rulesets(self):
        return json.loads(self.out.read_bytes())['rulesets']


class Agi1Bundle(unittest.TestCase):
    def test_agi1_bundle_preserves_public_bodies(self):
        w = World(self)
        w.bundle()
        raw = w.out.read_bytes()
        value = json.loads(raw)
        self.assertEqual(sorted(value), ['design_landing', 'edition', 'rulesets', 'schema'])
        self.assertEqual((value['schema'], value['edition'], value['design_landing']), (1, V, L))
        sets = value['rulesets']
        self.assertEqual([s['name'] for s in sets], list(NAMES))
        self.assertEqual([s['required'] for s in sets], [True] + [False] * 11)
        self.assertEqual(sets[0]['title'], 'Start with rdy')
        self.assertEqual(sets[1]['when'], 'when write')
        # Every optional body round-trips: only the front matter and the anchors are gone.
        for s in sets[1:]:
            self.assertEqual(s['rows'], [{'id': 'rdy:%s:body' % s['name'],
                                          'text': delivered(s['name']), 'locked': False}])
        # The bootstrap splits at the approval paragraph; its slices rebuild the body exactly.
        rows = sets[0]['rows']
        self.assertEqual([r['id'] for r in rows],
                         ['rdy:bootstrap:before', 'rdy:bootstrap:approval', 'rdy:bootstrap:after'])
        self.assertEqual([r['locked'] for r in rows], [False, True, False])
        self.assertEqual(rows[1]['text'], APPROVAL)
        self.assertEqual(''.join(r['text'] for r in rows),
                         '# Start with rdy\n\nrdy holds what a person intends.\n\n' + APPROVAL
                         + '\n\nFor every change:\n\n1.  `rdy_session`: your own actor.\n')
        # Non-ASCII text stays literal UTF-8, and no row or ruleset stores a reason.
        self.assertIn('Caf\u00e9'.encode(), raw)
        self.assertNotIn(b'reason', raw)

    def test_agi1_bundle_refuses_paths_duplicates_and_html(self):
        w = World(self)
        for bad in ('../outside.md', '/abs/x.md', 'a/../../x.md', 'C:\\x.md', 'a\\b.md', ''):
            p = projection()
            p['rulesets'][3]['path'] = bad
            w.write_projection(p)
            w.refused('invalid rulesets[3].path: path escapes export')
        p = projection()
        p['rulesets'][3]['path'] = p['rulesets'][2]['path']
        w.write_projection(p)
        w.refused('invalid rulesets[3].path: duplicate ruleset')
        p = projection()
        p['rulesets'][3]['name'] = 'write'
        w.write_projection(p)
        w.refused('invalid rulesets[3].name: duplicate ruleset')
        p = projection()
        p['rulesets'][3]['path'] = FOLDER + 'about.md'
        w.write_projection(p)
        w.refused('invalid rulesets[3].path: duplicate ruleset')
        w.write_projection(projection())

        (w.export / FOLDER / 'review.md').unlink()
        w.refused('invalid %sreview.md: missing document' % FOLDER)
        w.put(FOLDER + 'review.md', body_of('review'))

        w.put('extra.md', b'x')
        w.refused('invalid export: unknown key extra.md')
        (w.export / 'extra.md').unlink()

        (w.export / '.rdy').mkdir()
        w.refused('invalid export: unknown key .rdy')
        (w.export / '.rdy').rmdir()

        if hasattr(os, 'symlink'):
            target = w.export / FOLDER / 'review.md'
            target.unlink()
            try:
                os.symlink(w.dir / 'projection.json', target)
            except OSError:
                pass
            else:
                w.refused('path escapes export')
                target.unlink()
            w.put(FOLDER + 'review.md', body_of('review'))

        for line in (b'<div>raw</div>\n', b'<a id="x"></a> tail\n', b'    <b>x</b>\n', b'> <a id="q"></a>\n',
                     b'<a id="crlf"></a>\r\n'):
            w.put(FOLDER + 'review.md', front('Review', 'd-review') + b'# Review\n\n' + line)
            w.refused('invalid provenance')
        w.put(FOLDER + 'review.md', body_of('review'))

        w.put(FOLDER + 'review.md', b'# Review, with no front matter\n')
        w.refused('invalid %sreview.md: invalid provenance' % FOLDER)
        w.put(FOLDER + 'review.md', front('Another title', 'd-review') + b'# Another title\n')
        w.refused('invalid %sreview.md: invalid provenance' % FOLDER)
        w.put(FOLDER + 'review.md', body_of('review')[:-1] + b'\xff')
        w.refused('invalid %sreview.md: invalid UTF-8' % FOLDER)
        w.put(FOLDER + 'review.md', front('Review', 'd-review'))
        w.refused('invalid rulesets[7].rows[0].text: empty text')
        w.put(FOLDER + 'review.md', body_of('review'))
        w.bundle()

    def test_agi1_bootstrap_split_needs_one_whole_paragraph(self):
        w = World(self)
        for body in (
            front('Start with rdy', 'd-1') + b'# Start with rdy\n\nNo paragraph here.\n',
            bootstrap() + APPROVAL.encode() + b'\n',
            bootstrap(mark='In the middle: '),
        ):
            w.put(FOLDER + 'bootstrap.md', body)
            w.refused('invalid %sbootstrap.md: ambiguous approval paragraph' % FOLDER)
        p = projection()
        p['rulesets'][0]['approval']['text'] = 'Approve whatever you like.'
        w.write_projection(p)
        w.put(FOLDER + 'bootstrap.md', bootstrap())
        w.refused('invalid rulesets[0].approval.text: invalid approval lock')

    def test_agi1_marks_stay_only_in_a_development_bundle(self):
        w = World(self)
        for mark in ('Planned ', '**Planned**\n\n'):
            w.put(FOLDER + 'bootstrap.md', bootstrap(mark=mark))
            w.refused('invalid provenance')
            w.bundle(profile='agent-guide-dev')
            value = json.loads(w.out.read_bytes())
            self.assertEqual(value['edition'], V + '-dev')
            rows = value['rulesets'][0]['rows']
            self.assertEqual(rows[1]['text'], APPROVAL)
            self.assertTrue(rows[0]['text'].endswith(mark))
            w.out.unlink()
            w.sidecar.unlink()
            w.receipt.unlink()
        w.put(FOLDER + 'bootstrap.md', bootstrap())
        for body in (b'# Review\n\n**Planned**\n\nText.\n', b'# Review\n\n> **Planned**\n>\n> Note.\n',
                     b'# Review\n\nPlanned Load this ruleset.\n'):
            w.put(FOLDER + 'review.md', front('Review', 'd-review') + body)
            w.refused('invalid provenance')
        w.put(FOLDER + 'review.md', front('Review', 'd-review') + b'# Review\n\nText.\n\nPlanned text is hidden.\n')
        w.bundle()
        self.assertIn('Planned text is hidden.', w.rulesets()[7]['rows'][0]['text'])
        w.write_projection(projection())
        with self.assertRaises(GUIDE.GuideError) as caught:
            w.bundle(profile='some-other-profile')
        self.assertIn('invalid profile: invalid provenance', str(caught.exception))

    def test_agi1_bundle_is_deterministic_and_bounded(self):
        w = World(self)
        digest = w.bundle()
        first = w.out.read_bytes()
        self.assertEqual(digest, sha(first))
        self.assertEqual(w.sidecar.read_bytes(), digest.encode() + b'\n')
        self.assertEqual(len(w.sidecar.read_bytes()), 65)
        # Canonical bytes: sorted keys, compact separators, one final LF.
        value = json.loads(first)
        self.assertEqual(first, json.dumps(value, ensure_ascii=False, sort_keys=True,
                                           separators=(',', ':')).encode() + b'\n')
        again = World(self)
        self.assertEqual(again.bundle(), digest)
        self.assertEqual(again.out.read_bytes(), first)

        # The receipt binds every input.
        receipt = json.loads(w.receipt.read_bytes())
        self.assertEqual(sorted(receipt), ['bundle_sha256', 'design_landing', 'files', 'metadata_sha256',
                                           'profile', 'schema', 'version'])
        self.assertEqual(receipt['bundle_sha256'], digest)
        self.assertEqual(receipt['metadata_sha256'], sha(w.projection.read_bytes()))
        self.assertEqual(sorted(receipt['files']), sorted(list(KNOWN) + [FOLDER + n + '.md' for n in NAMES]))
        self.assertEqual(receipt['files'][FOLDER + 'write.md'], sha(body_of('write')))
        GUIDE.check(w.out, w.sidecar, w.receipt, w.export, w.projection)

        def check_refused(needle):
            with self.assertRaises(GUIDE.GuideError) as caught:
                GUIDE.check(w.out, w.sidecar, w.receipt, w.export, w.projection)
            self.assertIn(needle, str(caught.exception))

        # A changed input, receipt, bundle or sidecar refuses.
        w.put(FOLDER + 'write.md', body_of('write') + b'x')
        check_refused('invalid receipt: invalid provenance')
        w.put(FOLDER + 'write.md', body_of('write'))
        good_receipt = w.receipt.read_bytes()
        w.receipt.write_bytes(json.dumps(dict(receipt, surprise=1)).encode())
        check_refused('invalid receipt: unknown key surprise')
        w.receipt.write_bytes(json.dumps(dict(receipt, version='0.44.0')).encode())
        check_refused('invalid receipt: invalid provenance')
        w.receipt.write_bytes(good_receipt)
        w.sidecar.write_bytes(b'0' * 64 + b'\n')
        check_refused('release/agent-guide.json does not match release/agent-guide.sha256')
        w.sidecar.write_bytes(digest.encode() + b'\n')
        changed = first.replace(b'Caf', b'Kaf', 1)
        w.out.write_bytes(changed)
        w.sidecar.write_bytes(sha(changed).encode() + b'\n')
        check_refused('invalid receipt: invalid provenance')
        w.out.write_bytes(first)
        w.sidecar.write_bytes(digest.encode() + b'\n')
        GUIDE.check(w.out, w.sidecar, None, None, None)

        # Bounds: the projection, a path, the bodies, a title, a when, a landing and a version.
        def fresh():
            w.write_projection(projection())
            if w.out.exists():
                w.out.unlink()

        fresh()
        w.projection.write_bytes(b' ' * (65536 + 1))
        w.refused('projection exceeds 65536 bytes')
        fresh()
        w.projection.write_bytes(b'[' * 9 + b']' * 9)
        w.refused('depth exceeds 8')
        fresh()
        w.projection.write_bytes(b'[' * 60000)
        w.refused('depth exceeds 8')
        fresh()
        p = projection()
        p['rulesets'][2]['path'] = 'a/' * 600 + 'x.md'
        w.write_projection(p)
        w.refused('rulesets[2].path exceeds 1024 bytes')
        fresh()
        p = projection()
        p['rulesets'][2]['title'] = 't' * 257
        w.write_projection(p)
        w.refused('rulesets[2].title exceeds 256 bytes')
        fresh()
        p = projection()
        p['rulesets'][2]['when'] = 'w' * 1025
        w.write_projection(p)
        w.refused('rulesets[2].when exceeds 1024 bytes')
        fresh()
        w.put(FOLDER + 'write.md', front('Write', 'd-write') + b'# W\n\n' + b'x' * 300000 + b'\n')
        w.refused('rulesets[1].rows[0].text exceeds 262144 bytes')
        w.put(FOLDER + 'write.md', body_of('write'))
        big = front('Review', 'd-review') + b'# R\n\n' + b'x' * (1048576 + 1) + b'\n'
        w.put(FOLDER + 'review.md', big)
        w.refused('bodies exceeds 1048576 bytes')
        w.put(FOLDER + 'review.md', body_of('review'))
        for landing, version in ((L[:12], V), ('A' * 64, V), (L, 'v1'), (L, V + '-dev')):
            with self.assertRaises(GUIDE.GuideError) as caught:
                w.bundle(landing=landing, version=version)
            self.assertIn('invalid provenance', str(caught.exception))

        # Closed shapes: an unknown key names the nearest known one.
        fresh()
        p = projection()
        p['rulesets'][2]['titel'] = p['rulesets'][2].pop('title')
        w.write_projection(p)
        w.refused('invalid rulesets[2]: unknown key titel; did you mean title?')
        fresh()
        p = projection()
        p['surprise'] = 1
        w.write_projection(p)
        w.refused('invalid projection: unknown key surprise')
        fresh()
        w.projection.write_bytes(b'{"schema":1,"schema":1,"rulesets":[]}')
        w.refused('invalid projection: duplicate key')
        fresh()
        w.projection.write_bytes(b'{"schema":NaN,"rulesets":[]}')
        w.refused('invalid projection: invalid JSON')
        fresh()
        w.projection.write_bytes(b'\xff')
        w.refused('invalid projection: invalid UTF-8')
        fresh()
        p = projection()
        p['rulesets'][2]['title'] = '\ud800'
        w.write_projection(p)
        w.put(FOLDER + 'after-write.md', front('After-Write', 'd-w') + b'# A\n')
        with self.assertRaises(GUIDE.GuideError):
            w.bundle()
        w.put(FOLDER + 'after-write.md', body_of('after-write'))
        fresh()
        p = projection()
        p['schema'] = 2
        w.write_projection(p)
        w.refused('invalid projection.schema: unsupported schema')
        fresh()
        p = projection()
        p['rulesets'][2], p['rulesets'][3] = p['rulesets'][3], p['rulesets'][2]
        w.write_projection(p)
        w.refused('invalid rulesets[2].name: unexpected order')
        fresh()
        p = projection()
        p['rulesets'][2]['required'] = True
        w.write_projection(p)
        w.refused('invalid rulesets[2].required: invalid required set')
        fresh()
        p = projection()
        p['rulesets'][2]['required'] = 'no'
        w.write_projection(p)
        w.refused('invalid rulesets[2].required: wrong type')
        fresh()

        # `check` applies the same shape rules to the bundle itself.
        w.bundle()
        original = json.loads(w.out.read_bytes())
        def check_shape(value, needle):
            raw = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode() + b'\n'
            w.out.write_bytes(raw)
            w.sidecar.write_bytes(sha(raw).encode() + b'\n')
            with self.assertRaises(GUIDE.GuideError) as caught:
                GUIDE.check(w.out, w.sidecar, None, None, None)
            self.assertIn(needle, str(caught.exception))
        value = json.loads(json.dumps(original))
        value['rulesets'][1]['rows'][0]['reason'] = []
        check_shape(value, 'invalid rulesets[1].rows[0]: unknown key reason')
        value = json.loads(json.dumps(original))
        value['rulesets'][0]['rows'][1]['locked'] = False
        check_shape(value, 'invalid rulesets[0].rows[1].locked: invalid approval lock')
        value = json.loads(json.dumps(original))
        value['rulesets'][1]['rows'][0]['text'] = ' \n'
        check_shape(value, 'invalid rulesets[1].rows[0].text: empty text')
        value = json.loads(json.dumps(original))
        value['rulesets'][2]['rows'][0]['id'] = 'rdy:write:body'
        check_shape(value, 'invalid rulesets[2].rows[0].id: duplicate row')

    def test_agi1_cli_reports_one_line_and_exits_nonzero(self):
        w = World(self)
        w.bundle()
        ok = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), 'check', '--bundle', str(w.out),
                             '--sidecar', str(w.sidecar)], capture_output=True, text=True)
        self.assertEqual((ok.returncode, ok.stderr), (0, ''), ok.stderr)
        w.sidecar.write_bytes(b'0' * 64 + b'\n')
        bad = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), 'check', '--bundle', str(w.out),
                              '--sidecar', str(w.sidecar)], capture_output=True, text=True)
        self.assertEqual(bad.returncode, 1)
        self.assertEqual(bad.stderr, 'embedded guide: release/agent-guide.json does not match '
                                     'release/agent-guide.sha256; ' + FIX + '\n')
        w.out.unlink()
        w.sidecar.unlink()
        made = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), 'bundle', '--export', str(w.export),
                               '--projection', str(w.projection), '--landing', L, '--version', V,
                               '--profile', 'agent-guide', '--out', str(w.out), '--sidecar', str(w.sidecar),
                               '--receipt', str(w.receipt)], capture_output=True, text=True)
        self.assertEqual((made.returncode, made.stderr), (0, ''), made.stderr)
        GUIDE.check(w.out, w.sidecar, w.receipt, w.export, w.projection)


def reply_of(value):
    """The reply a binary built from bundle `value` gives in an empty home: every field from the
    bundle, computed here and not by the code under test."""
    rows, menu = [], []
    for ruleset in value['rulesets']:
        handle = 'rdy:' + ruleset['name']
        rows.extend({'id': row['id'], 'ruleset': handle, 'text': row['text'], 'layer': 'rdy',
                     'locked': row['locked'], 'reason': []} for row in ruleset['rows'])
        size = sum(len(row['text'].encode()) for row in ruleset['rows'])
        menu.append({'id': handle, 'name': ruleset['title'], 'when': ruleset['when'], 'layer': 'rdy',
                     'required': ruleset['required'], 'tokens': (size + 3) // 4, 'hints': []})
    return {'instructions': rows, 'menu': menu, 'hint': 'Run rdy setup to create your own rules project.'}


def binary_source(reply, before=''):
    """A real executable that answers `instructions --full --json`, as the packaged rdy does."""
    return ('#!/usr/bin/env python3\n'
            'import sys\n'
            + before +
            'if sys.argv[1:] != ["instructions", "--full", "--json"]:\n'
            '    sys.exit(64)\n'
            'sys.stdout.write(%r)\n' % (reply if isinstance(reply, str) else json.dumps(reply),))


def write_executable(path, source):
    path.write_text(source)
    path.chmod(0o755)
    return path


def valid_bundle(version=V, landing=L):
    """Canonical bundle bytes with the closed layout and no export behind them."""
    sets = []
    for name in NAMES:
        if name == 'bootstrap':
            rows = [{'id': 'rdy:bootstrap:before', 'text': '# Start with rdy\n\n', 'locked': False},
                    {'id': 'rdy:bootstrap:approval', 'text': APPROVAL, 'locked': True},
                    {'id': 'rdy:bootstrap:after', 'text': '\n\nCafé is five bytes.\n', 'locked': False}]
        else:
            rows = [{'id': 'rdy:%s:body' % name, 'text': '# %s\n\nA body of %s.\n' % (name.title(), name), 'locked': False}]
        sets.append({'name': name, 'title': 'Start with rdy' if name == 'bootstrap' else name.title(),
                     'when': 'when ' + name, 'required': name == 'bootstrap', 'rows': rows})
    value = {'schema': 1, 'edition': version, 'design_landing': landing, 'rulesets': sets}
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode() + b'\n'


class Agi6Binary(unittest.TestCase):
    DIFFERS = 'embedded guide: packaged binary differs from release/agent-guide.json; ' + FIX + '\n'

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix='rdyguide'))
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.bundle = self.dir / 'agent-guide.json'
        self.sidecar = self.dir / 'agent-guide.sha256'
        self.put_bundle(valid_bundle())
        self.binary = self.dir / 'rdy'

    def put_bundle(self, raw):
        self.bundle.write_bytes(raw)
        self.sidecar.write_bytes(sha(raw).encode() + b'\n')
        self.value = json.loads(raw)

    def run_check(self, env=None):
        return subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), 'check-binary', '--binary', str(self.binary),
                               '--bundle', str(self.bundle), '--sidecar', str(self.sidecar)],
                              capture_output=True, text=True, env=env)

    def differs(self, reply, before=''):
        write_executable(self.binary, binary_source(reply, before))
        out = self.run_check()
        self.assertEqual((out.returncode, out.stderr, out.stdout), (1, self.DIFFERS, ''))

    def test_agi6_packaged_binary_matches_frozen_export(self):
        good = reply_of(self.value)
        write_executable(self.binary, binary_source(good))
        out = self.run_check()
        self.assertEqual((out.returncode, out.stderr), (0, ''), out.stderr)
        self.assertEqual(out.stdout, sha(self.bundle.read_bytes()) + '\n')
        # Rows and menu arrive in JSON object order the binary chose: only the values count.
        write_executable(self.binary, binary_source(dict(reversed(list(good.items())))))
        self.assertEqual(self.run_check().returncode, 0)
        # A relative path names the binary from the caller's folder, not the empty one it runs in.
        out = subprocess.run([sys.executable, '-I', '-B', str(SCRIPT), 'check-binary', '--binary', self.binary.name,
                              '--bundle', self.bundle.name, '--sidecar', self.sidecar.name],
                             capture_output=True, text=True, cwd=self.dir)
        self.assertEqual((out.returncode, out.stderr), (0, ''), out.stderr)

        def changed(apply):
            reply = json.loads(json.dumps(good))
            apply(reply)
            return reply

        rows = lambda r: r['instructions']  # noqa: E731
        menu = lambda r: r['menu']  # noqa: E731
        for label, apply in (
                ('body text', lambda r: rows(r)[3].update(text=rows(r)[3]['text'] + 'x')),
                ('body byte, same token count', lambda r: rows(r)[3].update(text=rows(r)[3]['text'].replace('A body', 'B body'))),
                ('bootstrap slice', lambda r: rows(r)[2].update(text=rows(r)[2]['text'].replace('\n\n', '\n', 1))),
                ('row id', lambda r: rows(r)[3].update(id='rdy:write:other')),
                ('row ruleset', lambda r: rows(r)[3].update(ruleset='rdy:after-write')),
                ('row layer', lambda r: rows(r)[3].update(layer='project')),
                ('lock lost', lambda r: rows(r)[1].update(locked=False)),
                ('lock added', lambda r: rows(r)[3].update(locked=True)),
                ('lock is a number', lambda r: rows(r)[1].update(locked=1)),
                ('tokens are a float', lambda r: menu(r)[1].update(tokens=float(menu(r)[1]['tokens']))),
                ('reason added', lambda r: rows(r)[3].update(reason=[{'id': 'D-AAAAA', 'statement': 'x'}])),
                ('row key added', lambda r: rows(r)[3].update(extra=1)),
                ('row key lost', lambda r: rows(r)[3].pop('reason')),
                ('missing row', lambda r: rows(r).pop(5)),
                ('extra row', lambda r: rows(r).append(dict(rows(r)[5], id='rdy:more:body'))),
                ('stored row in an empty home', lambda r: rows(r).append(dict(rows(r)[5], id='p-AAAAA', layer='person'))),
                ('row order', lambda r: rows(r).insert(3, rows(r).pop(4))),
                ('no rows', lambda r: r.update(instructions=[])),
                ('rows are not a list', lambda r: r.update(instructions={})),
                ('menu title', lambda r: menu(r)[1].update(name='Other')),
                ('menu when', lambda r: menu(r)[1].update(when='never')),
                ('menu required', lambda r: menu(r)[1].update(required=True)),
                ('menu required lost', lambda r: menu(r)[0].update(required=False)),
                ('menu tokens', lambda r: menu(r)[1].update(tokens=menu(r)[1]['tokens'] + 1)),
                ('menu hints', lambda r: menu(r)[1].update(hints=['x'])),
                ('menu layer', lambda r: menu(r)[1].update(layer='project')),
                ('menu id', lambda r: menu(r)[1].update(id='rdy:other')),
                ('missing menu entry', lambda r: menu(r).pop(1)),
                ('extra menu entry', lambda r: menu(r).append(dict(menu(r)[1], id='rdy:more'))),
                ('menu order', lambda r: menu(r).insert(1, menu(r).pop(2))),
                ('no menu', lambda r: r.pop('menu'))):
            with self.subTest(label):
                self.differs(changed(apply))

        # A binary that fails, says nothing usable or writes without end is not the guide either.
        for label, reply, before in (
                ('exits nonzero', good, 'sys.exit(3)\n'),
                ('not JSON', 'not json', ''),
                ('duplicate key', '{"instructions":[],"instructions":[],"menu":[]}', ''),
                ('trailing text', json.dumps(good) + 'x', ''),
                ('nested too deep', '[' * 100, ''),
                ('not an object', '[]', ''),
                ('no such form', good, 'sys.argv[1:] = []\n'),
                ('endless output', good, 'sys.stdout.write("x" * 17000000)\nsys.stdout.flush()\n')):
            with self.subTest(label):
                self.differs(reply, before)

    def test_agi6_check_binary_refuses_a_bad_bundle_before_it_runs_anything(self):
        marker = self.dir / 'ran'
        write_executable(self.binary, binary_source(reply_of(self.value),
                                                    'open(%r, "w").close()\n' % str(marker)))
        raw = self.bundle.read_bytes()
        self.bundle.write_bytes(raw.replace(b'A body', b'B body', 1))
        out = self.run_check()
        self.assertEqual((out.returncode, out.stderr), (1, 'embedded guide: release/agent-guide.json does not match '
                                                           'release/agent-guide.sha256; ' + FIX + '\n'))
        self.put_bundle(raw)
        self.sidecar.write_bytes(b'0' * 64 + b'\n')
        self.assertEqual(self.run_check().returncode, 1)
        self.put_bundle(raw.replace(b'"locked":true', b'"locked":false'))
        out = self.run_check()
        self.assertEqual((out.returncode, out.stderr), (1, 'embedded guide: invalid rulesets[0].rows[1].locked: '
                                                           'invalid approval lock; ' + FIX + '\n'))
        self.sidecar.unlink()
        self.assertIn('missing release/agent-guide.sha256', self.run_check().stderr)
        self.assertFalse(marker.exists(), 'a bad bundle runs no binary')
        self.put_bundle(raw)
        self.binary.unlink()
        out = self.run_check()
        self.assertEqual((out.returncode, out.stderr), (1, 'embedded guide: missing packaged binary; ' + FIX + '\n'))

    def test_agi6_check_binary_runs_in_a_home_of_its_own(self):
        seen = self.dir / 'seen.json'
        probe = ('import json, os\n'
                 'home = os.environ.get("HOME", "")\n'
                 'open(os.path.join(home, "left-behind"), "w").close()\n'
                 'json.dump({"env": dict(os.environ), "cwd": os.getcwd(), "cwd_files": os.listdir("."),\n'
                 '           "home_files": os.listdir(home)}, open(%r, "w"))\n' % str(seen))
        write_executable(self.binary, binary_source(reply_of(self.value), probe))
        parent = dict(os.environ, HOME=str(self.dir), USERPROFILE=str(self.dir), XDG_CONFIG_HOME=str(self.dir),
                      APPDATA=str(self.dir), HOMEDRIVE='Z:', HOMEPATH='\\real', RDY_ACTOR='human:real',
                      RDY_RUN='real', RDY_NO_DAEMON='1')
        out = self.run_check(env=parent)
        self.assertEqual((out.returncode, out.stderr), (0, ''), out.stderr)
        got = json.loads(seen.read_text())
        env = got['env']
        home = Path(env['HOME'])
        self.assertEqual(env['USERPROFILE'], env['HOME'])
        for name in ('XDG_CONFIG_HOME', 'APPDATA'):
            self.assertTrue(Path(env[name]).is_relative_to(home), name)
        self.assertNotIn(str(self.dir), env['HOME'])
        self.assertIn('left-behind', got['home_files'])
        self.assertNotIn(self.bundle.name, got['home_files'], 'a home of its own')
        self.assertEqual(got['cwd_files'], [], 'no project in the working directory')
        self.assertEqual(Path(got['cwd']).parent.resolve(), home.parent.resolve(), 'a folder beside the home')
        self.assertEqual([k for k in env if k.startswith('RDY_')], [])
        self.assertNotIn('HOMEDRIVE', env)
        self.assertNotIn('HOMEPATH', env)
        self.assertFalse(home.parent.exists(), 'the home is removed afterward')

    def test_agi6_check_binary_stops_a_hung_binary(self):
        write_executable(self.binary, binary_source(reply_of(self.value), 'import time\ntime.sleep(600)\n'))
        with self.assertRaises(GUIDE.GuideError) as caught:
            GUIDE.check_binary(self.binary, self.bundle, self.sidecar, deadline=0.1)
        self.assertEqual(str(caught.exception), self.DIFFERS[:-1])


if __name__ == '__main__':
    unittest.main()
