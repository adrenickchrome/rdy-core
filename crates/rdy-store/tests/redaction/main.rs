//! Redaction tests: carried, imported, landed and recovered redactions and their records, grouped
//! into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[path = "../support/carried.rs"]
mod carried_fixture;
#[path = "../support/redaction.rs"]
mod redaction_fixture;

mod redaction_carried;
mod redaction_import;
mod redaction_land;
mod redaction_records;
mod redaction_recovery;
