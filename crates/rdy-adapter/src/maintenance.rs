//! Maintenance calls (D-WWAK7, D-69AY3, D-2FMRR, D-QF2HQ): format facts, dump inspection and
//! load, as the engine gives them. They open no store, so they need no handle; `StoreHandle::dump`,
//! `StoreHandle::dump_after` and `StoreHandle::fsck` are the calls that read an open store. The
//! chain types of a dump summary (`ChainEnd`, `BatchMarker`, `DumpKind`, `Location`) are here,
//! so a consumer can build every field of one through this crate alone; so are the stop of a
//! chain load (`DumpStop`) and the rows and stop of its summary (`LoadedChainFile`,
//! `StoppedAt`), and the format types of the facts (`DumpStore`, the store of a dump, and
//! `StoreFeatures`, a feature manifest). A holder dumps and loads
//! with no project file (D-BPYN8). Load reads every released dump format and `fsck` checks the
//! result.
//!
//! A store upgrade is an explicit call, never a side effect of an open (D-WWAK7): [`inspect_store`]
//! plans it and [`upgrade`] runs it, both on a project or holder root with no handle. The
//! caller closes its handles and releases its captured readers first; a running owner is
//! `E_LOCKED` and an outstanding reader `E_HELD_BY_READER`. Maintenance stops no host worker
//! and chooses no holder generation: the host owns both (hub `operations/upgrades`).

use rdy_engine::RdyError;
pub use rdy_engine::dump_stop::DumpStop;
pub use rdy_engine::formats::{DumpFormatFacts, DumpPair, FormatFacts};
pub use rdy_engine::{
    BatchMarker, ChainEnd, DumpKind, DumpSummary, FsckReport, LoadSummary, LoadedChainFile,
    Location, StoppedAt,
};
pub use rdy_store::format::{DumpStore, FeatureClass, StoreFeature, StoreFeatures};
pub use rdy_store::upgrade::{UpgradePlan, UpgradeReport, UpgradeStep};
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

/// The upgrade plan of the closed store of the project or holder at `root`, as `rdy upgrade
/// --dry-run` lists it. It opens no store, takes no lock and writes nothing.
pub fn inspect_store(root: &Path) -> Result<UpgradePlan, RdyError> {
    rdy_engine::maintenance::plan(root)
}

/// Inspects (`dry_run`) or upgrades the store of the project or holder at `root`, and returns
/// the report core's helper returns. An apply takes the store's lock, runs the one upgrade
/// batch, the reindex and the full check, and only then publishes the `FORMAT` mirror. The
/// caller closes its handles and releases its captured readers first: a running owner is
/// `E_LOCKED` and an outstanding reader `E_HELD_BY_READER`. A dry run creates no handle and
/// writes nothing. Unknown read-only features are never upgraded here (`E_STORE_READ_ONLY`).
pub fn upgrade(root: &Path, dry_run: bool) -> Result<UpgradeReport, RdyError> {
    rdy_engine::maintenance::run(root, dry_run)
}

/// Test hook: [`upgrade`] with the upgrade steps `registry`, through the same planner, runner
/// and full check. No product path reaches it.
#[doc(hidden)]
pub fn upgrade_with_registry_for_tests(
    root: &Path,
    dry_run: bool,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, RdyError> {
    rdy_engine::maintenance::run_with_registry_for_tests(root, dry_run, registry)
}
