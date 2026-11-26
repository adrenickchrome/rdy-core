//! Sync tests: fetch, replay, streams, foreign facts and snapshot deltas, grouped into one test
//! binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;

mod decoded_batch;
mod follower_guard;
mod foreign_deviation;
mod foreign_facts;
mod foreign_instructions;
mod foreign_refs;
mod foreign_signals;
mod foreign_state;
mod links;
mod snapshot_delta_checks;
mod snapshot_delta_reads;
mod snapshot_delta_upgrade;
mod stream_check;
mod stream_errors;
mod stream_export;
mod stream_take;
mod sync_fetch;
mod sync_replay;
