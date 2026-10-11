//! Coverage (FW-CV1, FW-CH1): six dimensions, scored per block and computed from the current
//! state, never stored (D-NMGD2). Each failed check is a coverage issue that names its block,
//! its dimension and the fix (D-XQVM1).
mod confidence;
mod graph;
mod kinds;
mod model;
mod score;
mod state;
mod structural;

pub use confidence::*;
pub use graph::*;
pub use kinds::KindCount;
pub use model::{
    BlockEntry, CoverageIssue, CoverageIssues, Dimension, Dismissal, IssueCode, Structural,
    sort_issues,
};
pub use score::{AreaScore, CoverageReport, PAGES_CAP, PageScore, Rollup, rollup};
pub(crate) use state::CoverageMemo;
pub use structural::{rebuilt_for_tests, structural_builds};
