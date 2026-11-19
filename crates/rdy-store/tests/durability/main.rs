//! Durability tests: crashes, reopen, locks, compaction and upgrades, grouped into one test binary
//! (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[path = "../support/carried.rs"]
mod carried_fixture;
#[allow(dead_code)]
#[path = "../support/files.rs"]
mod files;
#[path = "../support/redaction.rs"]
mod redaction_fixture;

mod compact;
mod coord;
mod crash;
mod follower_guard;
mod follower_ingest;
mod import_seal;
mod punch;
mod read_only;
mod signing_atomic;
mod snapshot_delta_upgrade;
mod start_lock;
mod store;
mod transition;
mod upgrade;
mod windows_durability;
