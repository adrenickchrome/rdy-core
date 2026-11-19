//! Publication tests: building, diffing, inspecting, releasing and taking a publication, and its
//! render inputs and site settings, grouped into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

mod as_built;
mod outside_doc;
mod publication;
mod publication_asbuilt;
mod publication_baseline;
mod publication_diff;
mod publication_identity;
mod publication_implicit;
mod publication_inspection;
mod publication_manifest;
mod publication_outside_doc;
mod publication_release;
mod publication_report;
mod publication_review;
mod publication_source;
mod publication_take;
mod render_inputs;
mod site_base_path;
mod site_target;
