//! rdy's QA gate: fixtures, the invariant checker, an MCP client with deadlines, the perf
//! workload and the gate report. The tests live in `tests/`, one file per area.
pub mod daemon;
pub mod fixtures;
pub mod golden;
pub mod invariants;
pub mod mcp;
pub mod packs;
pub mod perf;
pub mod report;
pub mod site;
