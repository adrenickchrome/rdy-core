//! Maintenance calls (D-WWAK7, D-69AY3): format facts, dump inspection and load, as the engine
//! gives them. They open no store, so they need no handle; `StoreHandle::dump`,
//! `StoreHandle::dump_after` and `StoreHandle::fsck` are the calls that read an open store. The
//! chain types of a dump summary (`ChainEnd`, `BatchMarker`, `DumpKind`, `Location`) are here,
//! so a consumer can build every field of one through this crate alone. A holder dumps and loads
//! with no project file (D-BPYN8). There is no upgrade call: load reads every released dump
//! format and `fsck` checks the result.

use rdy_engine::RdyError;
pub use rdy_engine::formats::{DumpFormatFacts, DumpPair, FormatFacts};
pub use rdy_engine::{
    BatchMarker, ChainEnd, DumpKind, DumpSummary, FsckReport, LoadSummary, Location,
};
use std::path::Path;

/// The formats this rdy writes and reads.
pub fn formats() -> FormatFacts {
    rdy_engine::formats::formats()
}

/// The formats of the dump `file`, from its prelude and header only.
pub fn inspect_dump(file: &Path) -> Result<DumpFormatFacts, RdyError> {
    rdy_engine::formats::inspect_dump(file)
}

/// Loads the dump `file` into `dir`, a directory with no store, as `rdy load` does. A holder
/// dump becomes a holder: no config or pack file is written, and no canon is made. A refused or
/// failed load leaves `dir` as it was.
pub fn load(file: &Path, dir: &Path) -> Result<LoadSummary, RdyError> {
    rdy_engine::load::load(file, dir)
}
