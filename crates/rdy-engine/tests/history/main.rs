//! History tests: load chains, dumps, imports, replay, upgrades and holders, grouped into one test
//! binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

mod carried_warning;
mod compact;
mod compare_states;
mod content_order;
mod dump_after;
mod dump_stop;
mod export_walk_known;
mod fork_at;
mod fork_origins;
mod formats;
mod history_facts;
mod holder;
mod holder_load;
mod import_atomic;
mod load_chain;
mod load_chain_scan;
mod markdown_import_files;
mod markdown_import_parse;
mod port_bundle;
mod port_prepare;
mod reconstructed;
mod replay;
mod replay_api;
mod upgrade_open;
mod versions;
