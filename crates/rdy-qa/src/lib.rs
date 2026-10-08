//! rdy's QA gate: fixtures, the invariant checker, an MCP client with deadlines, the perf
//! workload and the gate report. The tests live in `tests/`, one file per area.
pub mod daemon;
pub mod edit_loop;
pub mod fixtures;
pub mod git_repo;
pub mod golden;
pub mod invariants;
pub mod mcp;
pub mod packs;
pub mod perf;
pub mod report;
pub mod site;
pub mod wc;

/// The libtest name of the test fn `$f` in the calling file (see `fixtures::libtest_name`):
/// pass it to `--exact` when a test re-runs its own binary.
#[macro_export]
macro_rules! test_name {
    ($f:expr) => {
        $crate::fixtures::libtest_name(module_path!(), $f)
    };
}
