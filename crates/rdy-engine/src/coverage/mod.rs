//! Coverage (FW-CV1, FW-CH1): six dimensions, scored per block and computed from the current
//! state, never stored (D-NMGD2). Each failed check is a coverage issue that names its block,
//! its dimension and the fix (D-XQVM1).
mod carry;
mod change;
mod confidence;
mod kinds;
mod model;
mod onboarding;
mod score;
mod state;
mod structural;

pub(crate) use carry::carry;
pub use carry::{carried_entries, carry_for_perf};
pub use change::{CoverageChange, PageChange, coverage_change};
pub(crate) use confidence::confidence_in;
pub use confidence::*;
pub use kinds::{KindCount, kinds_builds};
pub(crate) use model::sort_issue_refs;
pub use model::{
    BlockEntry, CoverageIssue, CoverageIssues, Dimension, Dismissal, IssueCode, Structural,
    sort_issues,
};
pub use onboarding::*;
pub(crate) use score::page_blocks;
pub use score::{AreaScore, CoverageReport, PAGES_CAP, PageScore, Rollup, report_builds, rollup};
pub(crate) use state::CoverageMemo;
pub use state::{
    CoverageSummary, coverage_error_text, issues_rebuilt_for_tests, local_entries, rollup_walks,
    state_layer_builds, suspect_walks,
};
pub(crate) use structural::lands_unapplied;
pub use structural::{governed_scans, rebuilt_for_tests, structural_builds, structural_for_tests};
