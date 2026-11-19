//! Redaction tests: approvals, cards, carriers, projections, reads and reports, grouped into one
//! test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

mod redaction_approval;
mod redaction_cards;
mod redaction_carriers;
mod redaction_projection;
mod redaction_reads;
mod redaction_report;
mod snapshot_delta_redaction;
