//! Sync tests: sync state, takes, streams, foreign transport, holders and snapshot deltas, grouped
//! into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[path = "../support/carried.rs"]
mod carried_fixture;
#[path = "../support/redaction.rs"]
mod redaction_fixture;

mod carried_records;
mod checked_reuse;
mod decoded_batch;
mod dump_chain;
mod foreign_compatibility;
mod foreign_frozen;
mod foreign_fsck;
mod foreign_log;
mod foreign_removal;
mod foreign_removal_transport;
mod foreign_transport;
mod foreign_verdicts;
mod fork_origins;
mod holder;
mod hub_canon;
mod hub_ports;
mod ingest_chain;
mod link_delivery_stream;
mod port_merge;
mod reflects_stream;
mod snapshot_delta;
mod snapshot_delta_land;
mod stream_source;
mod sync_history;
mod sync_landed;
mod sync_state;
mod sync_stream;
mod take_batch;
