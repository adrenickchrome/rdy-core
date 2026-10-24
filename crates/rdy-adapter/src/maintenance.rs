//! Maintenance calls (D-WWAK7, D-69AY3, D-2FMRR, D-QF2HQ): format facts, dump inspection and
//! load, as the engine gives them. They open no store, so they need no handle; `StoreHandle::dump`,
//! `StoreHandle::dump_after` and `StoreHandle::fsck` are the calls that read an open store. The
//! chain types of a dump summary (`ChainEnd`, `BatchMarker`, `DumpKind`, `Location`) are here,
//! so a consumer can build every field of one through this crate alone; so are the stop of a
//! chain load (`DumpStop`) and the rows and stop of its summary (`LoadedChainFile`,
//! `StoppedAt`), and the format types of the facts (`DumpStore`, the store of a dump, and
//! `StoreFeatures`, a feature manifest). A holder dumps and loads
//! with no project file (D-BPYN8). There is no upgrade call: load reads every released dump
//! format and `fsck` checks the result.

use rdy_engine::RdyError;
pub use rdy_engine::dump_stop::DumpStop;
pub use rdy_engine::formats::{DumpFormatFacts, DumpPair, FormatFacts};
pub use rdy_engine::{
    BatchMarker, ChainEnd, DumpKind, DumpSummary, FsckReport, LoadSummary, LoadedChainFile,
    Location, StoppedAt,
};
pub use rdy_store::format::{DumpStore, FeatureClass, StoreFeature, StoreFeatures};
use std::path::{Path, PathBuf};

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

/// Loads the dump chain `files` (a full dump, then its increments in chain order) into `dir`, a
/// directory with no store, up to `stop` when one is given, as `rdy load` does. Every file is
/// checked before `dir` is touched; a stop before a removal of the source carries it, and the
/// summary's `warnings` name each carried removal (D-QF2HQ, D-0VG72). A refused or failed load
/// leaves `dir` as it was.
pub fn load_chain(
    files: &[PathBuf],
    dir: &Path,
    stop: Option<&DumpStop>,
) -> Result<LoadSummary, RdyError> {
    rdy_engine::load::load_chain(files, dir, stop)
}
