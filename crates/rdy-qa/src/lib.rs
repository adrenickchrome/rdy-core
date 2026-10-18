//! rdy's QA gate: fixtures, the invariant checker, an MCP client with deadlines, the perf
//! workload and the gate report. The tests live in `tests/`, one test binary per area,
//! `tests/<area>/main.rs`.
pub mod daemon;
pub mod edit_loop;
pub mod every_kind;
pub mod fixtures;
pub mod git_repo;
pub mod golden;
pub mod http;
pub mod inbox;
pub mod intake;
pub mod invariants;
pub mod landing_audit;
pub mod mcp;
pub mod packs;
pub mod perf;
pub mod platform;
pub mod reindex_check;
pub mod report;
pub mod seams;
pub mod signing;
pub mod site;
pub mod skip;
pub mod swarm;
pub mod tensions;
pub mod wc;
pub mod wc_check;

/// The libtest name of the test fn `$f` in the calling file (see `fixtures::libtest_name`):
/// pass it to `--exact` when a test re-runs its own binary.
#[macro_export]
macro_rules! test_name {
    ($f:expr) => {
        $crate::fixtures::libtest_name(module_path!(), $f)
    };
}
