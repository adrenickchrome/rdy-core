//! Core engine tests: writes, reads, queries, folders, paths, events, code checks and Windows text
//! paths, grouped into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[path = "../../../rdy-daemon/build_support.rs"]
mod build_support;
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;

mod check_code;
mod code_index;
mod engine;
mod entry_points;
mod errors;
mod events;
mod folders;
mod foundation;
mod hooks;
mod io;
mod module_compile;
mod paths;
mod query;
mod read_only;
mod read_state;
mod scope_share;
mod scopes;
mod source_exists;
mod windows_native_fixtures;
mod windows_text_paths;
mod write;
mod wx27_no_console_window;
