"""Shadow test selection: scripts/ci/select_tests.py."""
import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('select_tests', ROOT / 'scripts/ci/select_tests.py')
sel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sel)

# model <- schema <- store <- engine <- {render, mcp} <- daemon <- cli ; diagram <- render
DEPS = {
    'rdy-model': [],
    'rdy-diagram': [],
    'rdy-schema': ['rdy-model'],
    'rdy-store': ['rdy-model', 'rdy-schema'],
    'rdy-engine': ['rdy-store', 'rdy-schema', 'rdy-model'],
    'rdy-render': ['rdy-engine', 'rdy-diagram'],
    'rdy-mcp': ['rdy-engine'],
    'rdy-daemon': ['rdy-mcp', 'rdy-engine', 'rdy-store'],
    'rdy-cli': ['rdy-daemon'],
    'rdy-qa': ['rdy-model', 'rdy-store', 'rdy-engine', 'rdy-render', 'rdy-mcp', 'rdy-daemon'],
}
GRAPH = {n: {'dir': 'crates/%s/' % n, 'deps': set(d)} for n, d in DEPS.items()}


class Selection(unittest.TestCase):
    def test_selection_widens_on_store_format(self):
        for path in ('crates/rdy-store/src/format.rs', 'crates/rdy-store/src/format_v2.rs'):
            got = sel.select([path], GRAPH)
            self.assertTrue(got['everything'], path)
            self.assertEqual(got['crates'], sorted(GRAPH))
            self.assertIn('store format', got['reasons'][0])

    def test_other_store_file_does_not_widen_by_itself(self):
        got = sel.select(['crates/rdy-store/src/index.rs'], GRAPH)
        self.assertFalse(got['everything'])
        self.assertNotIn('rdy-model', got['crates'])

    def test_leaf_edit_selects_only_that_crate_and_dependents(self):
        got = sel.select(['crates/rdy-diagram/src/lib.rs'], GRAPH)
        self.assertFalse(got['everything'])
        self.assertEqual(got['crates'], ['rdy-diagram', 'rdy-qa', 'rdy-render'])
        self.assertEqual(got['qa_areas'], ['render', 'render_quality'])

    def test_unmapped_path_selects_everything(self):
        got = sel.select(['tools/something.sh'], GRAPH)
        self.assertTrue(got['everything'])
        self.assertIn('unmapped path', got['reasons'][0])

    def test_always_wide_paths(self):
        for path in ('Cargo.lock', 'crates/rdy-engine/Cargo.toml', '.github/workflows/ci.yml',
                     'crates/rdy-model/src/lib.rs', 'crates/rdy-qa/src/gate_steps.rs',
                     'crates/rdy-qa/tests/support/mod.rs'):
            self.assertTrue(sel.select([path], GRAPH)['everything'], path)

    def test_qa_area_edit_selects_that_area_only(self):
        got = sel.select(['crates/rdy-qa/tests/cli/main.rs'], GRAPH)
        self.assertFalse(got['everything'])
        self.assertEqual(got['qa_areas'], ['cli'])
        self.assertEqual(got['crates'], [])

    def test_missed_failures_are_the_areas_the_selection_skipped(self):
        selection = sel.select(['crates/rdy-diagram/src/lib.rs'], GRAPH)
        report = {
            'areas': [
                {'name': 'render', 'ok': False, 'failures': ['a']},
                {'name': 'store', 'ok': False, 'failures': ['b', 'c']},
                {'name': 'mcp', 'ok': True, 'failures': []},
            ],
            'gates': [{'name': 'cargo test --workspace --exclude rdy-qa', 'pass': False}],
        }
        got = sel.missed(selection, report)
        self.assertEqual(got['areas'], [{'area': 'store', 'tests': ['b', 'c']}])
        self.assertEqual(got['unattributed'], ['cargo test --workspace --exclude rdy-qa'])

    def test_nothing_is_missed_when_everything_ran_or_no_report(self):
        wide = sel.select(['Cargo.lock'], GRAPH)
        report = {'areas': [{'name': 'store', 'ok': False, 'failures': ['b']}], 'gates': []}
        self.assertEqual(sel.missed(wide, report), {'areas': [], 'unattributed': []})
        self.assertEqual(sel.missed(wide, None), {'areas': [], 'unattributed': []})

    def test_graph_from_metadata(self):
        meta = {
            'workspace_root': '/w',
            'workspace_members': ['a', 'b'],
            'packages': [
                {'id': 'a', 'name': 'a', 'manifest_path': '/w/crates/a/Cargo.toml', 'dependencies': []},
                {'id': 'b', 'name': 'b', 'manifest_path': '/w/crates/b/Cargo.toml',
                 'dependencies': [{'name': 'a'}, {'name': 'serde'}]},
                {'id': 'x', 'name': 'serde', 'manifest_path': '/r/serde/Cargo.toml', 'dependencies': []},
            ],
        }
        g = sel.load_graph(meta)
        self.assertEqual(g['b'], {'dir': 'crates/b/', 'deps': {'a'}})
        self.assertEqual(sel.dependents(g, {'a'}), {'a', 'b'})


if __name__ == '__main__':
    unittest.main()
