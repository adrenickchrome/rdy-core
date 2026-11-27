//! Format tests: encodings, kinds, segments, the index, object refs and file identity, grouped into
//! one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[path = "../support/carried.rs"]
mod carried_fixture;
#[path = "../support/redaction.rs"]
mod redaction_fixture;

mod blob_state;
mod chain;
mod closure;
mod closure_metadata;
mod display_path;
mod encoding;
mod fileid;
mod foreign_encoding;
mod format;
mod index;
mod kinds;
mod meta;
mod object_refs;
mod ported_encoding;
mod private_write;
mod reader;
mod reflects_encoding;
mod residue_encoding;
mod segment;
mod semantic;
mod windows_dir;
