#!/usr/bin/env python3
"""Shadow test selection for pull requests.

Reads the changed paths of a diff and the workspace crate graph (`cargo metadata`), and works out
which crates and which rdy-qa areas a diff could affect: the crates the diff touches and every crate
that depends on them. Some paths select everything: the shared model, the store format, the gate,
the workflows, dependency files, and any path this script does not map.

With `--report` it also reads the full suite's `report.json` (it never runs a test) and lists the
failures the selection would have missed.

Nothing here decides a pull request. The output is a file for people to read, and the full suite
stays the gate.
"""
import argparse
import json
import os
import subprocess
import sys

CRATES_DIR = 'crates/'
QA_CRATE = 'rdy-qa'
QA_TESTS = 'crates/rdy-qa/tests/'
QA_SUPPORT = 'crates/rdy-qa/tests/support/'

# The rdy-qa area (a directory under crates/rdy-qa/tests) -> the crates its tests exercise. An area
# runs when one of these crates is affected. An area missing from this table always runs: the
# script does not know what it covers.
AREA_CRATES = {
    'model': ['rdy-model'],
    'schema': ['rdy-schema'],
    'store': ['rdy-store'],
    'engine': ['rdy-engine'],
    'engine_read': ['rdy-engine'],
    'brief': ['rdy-engine'],
    'checks': ['rdy-engine'],
    'proposal': ['rdy-engine'],
    'tensions': ['rdy-engine'],
    'views': ['rdy-engine'],
    'rebase': ['rdy-engine'],
    'publication': ['rdy-engine'],
    'render': ['rdy-render'],
    'render_quality': ['rdy-render'],
    'mcp': ['rdy-mcp'],
    'daemon': ['rdy-daemon'],
    'cli': ['rdy-cli'],
}

# Whole-tree reasons, by exact path or prefix. First match wins.
EVERYTHING_EXACT = {
    'Cargo.toml': 'dependency or workspace file',
    'Cargo.lock': 'dependency or workspace file',
    'rust-toolchain.toml': 'toolchain file',
    'deny.toml': 'dependency policy',
    'clippy.toml': 'lint config',
    'crates/rdy-qa/qa-baseline.json': 'gate baseline',
}
EVERYTHING_PREFIX = (
    ('.github/workflows/', 'workflow'),
    ('.cargo/', 'cargo config'),
    ('crates/rdy-model/', 'shared model'),
    ('crates/rdy-store/src/format', 'store format'),
    ('crates/rdy-qa/src/', 'gate'),
    (QA_SUPPORT, 'gate support'),
    ('crates/rdy-qa/fixtures/', 'gate fixtures'),
)


def load_graph(metadata):
    """{crate name: {'dir': 'crates/x/', 'deps': {workspace crates it depends on}}}."""
    members = set(metadata['workspace_members'])
    pkgs = [p for p in metadata['packages'] if p['id'] in members]
    names = {p['name'] for p in pkgs}
    root = metadata['workspace_root'].rstrip('/') + '/'
    graph = {}
    for p in pkgs:
        d = os.path.dirname(p['manifest_path'])
        d = (d + '/')
        if d.startswith(root):
            d = d[len(root):]
        graph[p['name']] = {
            'dir': d,
            'deps': {dep['name'] for dep in p['dependencies'] if dep['name'] in names},
        }
    return graph


def dependents(graph, seeds):
    """The seeds and every crate that depends on one of them, directly or not."""
    out = set(seeds)
    grew = True
    while grew:
        grew = False
        for name, info in graph.items():
            if name not in out and info['deps'] & out:
                out.add(name)
                grew = True
    return out


def everything_reason(path):
    if path in EVERYTHING_EXACT:
        return EVERYTHING_EXACT[path]
    if path.endswith('Cargo.toml') or path.endswith('Cargo.lock'):
        return 'dependency or workspace file'
    for prefix, why in EVERYTHING_PREFIX:
        if path.startswith(prefix):
            return why
    return None


def select(paths, graph):
    """What a diff selects: {'everything', 'reasons', 'crates', 'qa_areas'}."""
    all_areas = sorted(set(AREA_CRATES))
    touched, areas, reasons = set(), set(), []

    def wide(path, why):
        reasons.append('%s: %s' % (path, why))

    by_dir = sorted(((i['dir'], n) for n, i in graph.items()), key=lambda t: -len(t[0]))
    for path in sorted(set(paths)):
        why = everything_reason(path)
        if why:
            wide(path, why)
            continue
        if path.startswith(QA_TESTS):
            rest = path[len(QA_TESTS):]
            if '/' in rest:
                areas.add(rest.split('/', 1)[0])
                continue
            wide(path, 'rdy-qa test file outside an area')
            continue
        owner = next((n for d, n in by_dir if path.startswith(d)), None)
        if owner is None:
            wide(path, 'unmapped path')
            continue
        touched.add(owner)

    if reasons:
        return {
            'everything': True,
            'reasons': reasons,
            'crates': sorted(graph),
            'qa_areas': None,
        }
    affected = dependents(graph, touched)
    # An area also runs when one of the crates it exercises is affected, and an area the script
    # does not know always runs when anything in a crate changed.
    selected = set(areas)
    for area in all_areas:
        if set(AREA_CRATES[area]) & affected:
            selected.add(area)
    return {
        'everything': False,
        'reasons': [],
        'crates': sorted(affected),
        'qa_areas': sorted(selected),
    }


def known_qa_areas(root):
    base = os.path.join(root, QA_TESTS)
    if not os.path.isdir(base):
        return []
    return sorted(
        d for d in os.listdir(base)
        if d != 'support' and os.path.isfile(os.path.join(base, d, 'main.rs'))
    )


def unknown_areas_always_run(selection, areas_on_disk):
    """Fold in areas that exist on disk but are not in AREA_CRATES: they always run."""
    if selection['everything']:
        return selection
    extra = [a for a in areas_on_disk if a not in AREA_CRATES]
    if extra and selection['crates']:
        selection['qa_areas'] = sorted(set(selection['qa_areas']) | set(extra))
    return selection


def missed(selection, report):
    """The full suite's failures the selection would have skipped.

    `report` is the qa gate's report.json. An area that failed counts as missed when the
    selection did not run it. A failed gate step with no area (the workspace unit tests) cannot be
    placed in a crate, so it is listed as unattributed and counted as missed unless everything ran.
    """
    out = {'areas': [], 'unattributed': []}
    if report is None:
        return out
    ran = None if selection['everything'] else set(selection['qa_areas'])
    for area in report.get('areas', []):
        if area.get('ok', True) and not area.get('failures'):
            continue
        if ran is None or area['name'] in ran:
            continue
        out['areas'].append({'area': area['name'], 'tests': list(area.get('failures', []))})
    for gate in report.get('gates', []):
        if gate.get('pass', True) or gate.get('state') == 'skipped':
            continue
        if 'workspace' in gate.get('name', '') and ran is not None:
            out['unattributed'].append(gate['name'])
    return out


def changed_paths(base):
    res = subprocess.run(
        ['git', 'diff', '--name-only', '--no-renames', base, 'HEAD'],
        check=True, capture_output=True, text=True,
    )
    return [p for p in res.stdout.splitlines() if p]


def cargo_metadata():
    res = subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--no-deps', '--locked', '--offline'],
        check=True, capture_output=True, text=True,
    )
    return json.loads(res.stdout)


def build_shadow(paths, graph, report, root='.'):
    selection = unknown_areas_always_run(select(paths, graph), known_qa_areas(root))
    full = {'report_found': report is not None}
    if report is not None:
        full['pass'] = bool(report.get('pass'))
        full['failed_areas'] = [
            a['name'] for a in report.get('areas', [])
            if not a.get('ok', True) or a.get('failures')
        ]
    return {
        'schema': 1,
        'mode': 'shadow',
        'changed_paths': len(set(paths)),
        'selection': selection,
        'full_suite': full,
        'missed': missed(selection, report),
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('--base', required=True, help='the revision the pull request is compared with')
    ap.add_argument('--report', help="the full suite's report.json (read only)")
    ap.add_argument('--out', default='selection-shadow.json')
    args = ap.parse_args(argv)
    report = None
    if args.report and os.path.isfile(args.report):
        with open(args.report) as f:
            report = json.load(f)
    shadow = build_shadow(changed_paths(args.base), load_graph(cargo_metadata()), report)
    with open(args.out, 'w') as f:
        json.dump(shadow, f, indent=2, sort_keys=True)
        f.write('\n')
    print('selection-shadow: everything=%s crates=%d missed=%d'
          % (shadow['selection']['everything'], len(shadow['selection']['crates']),
             len(shadow['missed']['areas']) + len(shadow['missed']['unattributed'])))
    return 0


if __name__ == '__main__':
    sys.exit(main())
