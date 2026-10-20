//! The pre-pass of `rdy load` over a dump chain (D-2FMRR: "Before it writes, the load reads the
//! applies of every file"; D-QF2HQ; D-69AY3). [`inspect`] checks the input count, then the
//! envelopes and links, the framing of every file, the removal proofs, the stop and the files
//! after it (`rdy_store::dump_scan`), and returns the verified plan of the load. No target
//! exists while it runs: it creates no directory, no store, no spool and no file.
//!
//! The order of its refusals (O11): the input count, then each file's envelope and link in
//! argument order, then each file's framing, then the removal proofs in physical order, then the
//! stop, then the full read of the files after the stop. The stop's spelling is the caller's
//! ([`DumpStop::parse`]), before any path is opened. A single file of dump format 1 or 2 with no
//! stop is the released single-file load's; with a stop it is refused.
use crate::dump_error;
use crate::dump_stop::{DumpStop, MAX_CHAIN_FILES};
use crate::error::{E_BAD_INPUT, E_IO, RdyError};
use crate::load::{holder_files, open_dump, read_error};
use rdy_store::StoreError;
use rdy_store::dump_scan::{
    ChainLink, ChainSource, FileRole, ScanError, ScanPlan, StopFailure, StopMatch, scan,
};
use rdy_store::holder::StoreMode;
use rdy_store::segment::Location;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The most files of one chain: a full dump and [`MAX_CHAIN_FILES`] increments.
const MAX_FILES: usize = MAX_CHAIN_FILES + 1;

/// The verified plan of a chain load: the inputs, and what the pre-pass checked and decided for
/// them. Its facts are read-only; it is no token a later load can reuse with other files.
pub(crate) struct ChainPlan {
    files: Vec<PathBuf>,
    inner: Inner,
}

enum Inner {
    /// A single file of dump format 1 or 2 with no stop: the released single-file load reads it.
    Legacy { format: u32 },
    /// A chain of dump format 3.
    Chain(ScanPlan),
}

impl ChainPlan {
    /// The inputs, in argument order.
    pub(crate) fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The dump format of a single released-format input, which the single-file load reads.
    pub(crate) fn legacy(&self) -> Option<u32> {
        match &self.inner {
            Inner::Legacy { format } => Some(*format),
            Inner::Chain(_) => None,
        }
    }

    /// The scanner's verified plan of a chain of dump format 3.
    pub(crate) fn scan(&self) -> Option<&ScanPlan> {
        match &self.inner {
            Inner::Chain(p) => Some(p),
            Inner::Legacy { .. } => None,
        }
    }

    /// The input whose project files the load writes: the last one that contributes a retained
    /// batch.
    pub(crate) fn owner(&self) -> Option<&Path> {
        self.scan().map(|p| self.files[p.owner()].as_path())
    }
}

/// The refusal of a load of no files.
fn no_files() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "A dump chain needs a full dump.",
        "Pass a full dump file first.",
    )
}

/// Refuses no files, and more than a full dump and [`MAX_CHAIN_FILES`] increments, before any
/// path is opened.
fn check_count(files: &[PathBuf]) -> Result<(), RdyError> {
    if files.is_empty() {
        return Err(no_files());
    }
    if files.len() > MAX_FILES {
        return Err(dump_error::chain_too_long());
    }
    Ok(())
}

/// Checks the chain `files` with `stop` before any target write (module docs) and returns the
/// verified plan.
pub(crate) fn inspect(files: &[PathBuf], stop: Option<&DumpStop>) -> Result<ChainPlan, RdyError> {
    check_count(files)?;
    let mut src = ChainSource::open(files).map_err(|e| scan_error(files, e))?;
    if let Some(format) = src.legacy()
        && stop.is_none()
    {
        return Ok(ChainPlan {
            files: files.to_vec(),
            inner: Inner::Legacy { format },
        });
    }
    let plan = scan(&mut src, stop.map(stop_match)).map_err(|e| scan_error(files, e))?;
    // a holder carries no project file, in any input (D-BPYN8)
    if plan.mode() == StoreMode::Holder && plan.files().iter().any(|f| !f.files.is_empty()) {
        return Err(holder_files());
    }
    Ok(ChainPlan {
        files: files.to_vec(),
        inner: Inner::Chain(plan),
    })
}

/// The scanner's form of a parsed stop.
fn stop_match(stop: &DumpStop) -> StopMatch {
    match stop {
        DumpStop::Landing(prefix) => StopMatch::LandingPrefix(prefix.clone()),
        DumpStop::Tag(name) => StopMatch::TagName(name.clone()),
        DumpStop::Position { segment, offset } => StopMatch::Position(Location {
            segment: *segment,
            offset: *offset,
        }),
    }
}

/// The engine's error of a scanner refusal: path errors name the input, link errors are the
/// chain rows of `E_DUMP_CHAIN`, stop errors the rows of `E_LOAD_STOP`.
fn scan_error(files: &[PathBuf], e: ScanError) -> RdyError {
    match e {
        ScanError::Input { file, source } => {
            let Some(path) = files.get(file) else {
                return source.into();
            };
            match source {
                StoreError::Io(e) if e.kind() == ErrorKind::NotFound => match open_dump(path) {
                    Err(e) => e,
                    Ok(_) => read_error(path, StoreError::Io(e)),
                },
                source => read_error(path, source),
            }
        }
        ScanError::Link { file, link } => {
            let path = files[file].as_path();
            match link {
                ChainLink::FirstIsIncrement => dump_error::first_is_increment(path),
                ChainLink::LaterIsFull => dump_error::later_is_full(path),
                ChainLink::Legacy { format } => dump_error::load_legacy_file(path, format),
                ChainLink::LegacyStop { format } => dump_error::stop_legacy_file(path, format),
                ChainLink::Repo { file, chain } => {
                    dump_error::load_repo_mismatch(path, &file, &chain)
                }
                ChainLink::StoreFormat { file, chain } => {
                    dump_error::load_store_mismatch(path, file, chain)
                }
                ChainLink::Position { start, prev_end } => dump_error::load_position_mismatch(
                    path,
                    start.segment,
                    start.offset,
                    prev_end.segment,
                    prev_end.offset,
                ),
                ChainLink::Marker => dump_error::load_marker_mismatch(path),
                ChainLink::Digest { named, actual } => {
                    dump_error::load_digest_mismatch(path, &named, &actual)
                }
            }
        }
        ScanError::Stop(f) => match f {
            StopFailure::MissingLanding(p) => dump_error::stop_no_landing(&p),
            StopFailure::MissingTag(n) => dump_error::stop_no_tag(&n),
            StopFailure::AmbiguousLanding(p) => dump_error::stop_ambiguous_landing(&p),
            StopFailure::NotBatchEnd(at) => dump_error::stop_not_batch_end(at.segment, at.offset),
            // distinct tag objects of one name: the existing refusal of damaged tags
            StopFailure::DuplicateTag(name) => RdyError::new(
                E_IO,
                format!("two tags denote version {name}"),
                "a tag ref is damaged or was written outside rdy tag: restore the store from a backup",
            ),
        },
    }
}

/// What a pre-pass decided, for tests: the plan's facts with no payload and no reusable plan.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectFacts {
    /// The dump format of a single released-format input (the single-file load's).
    pub legacy: Option<u32>,
    /// The selected end, and whether a stop selected it.
    pub end: Option<Location>,
    pub explicit_stop: bool,
    /// Whether the chain is a holder's.
    pub holder: bool,
    /// The input that owns the project files, and their paths.
    pub owner: Option<usize>,
    pub owner_files: Vec<String>,
    /// Each input's part: `retained`, `stop` or `after`.
    pub roles: Vec<&'static str>,
    /// The prepared carried records and the residues they bring.
    pub carried: usize,
    pub residues: usize,
}

/// Test hook: the pre-pass of a load of `files` stopped at `stop` (its text, parsed after the
/// input count, before any path is opened), as [`InspectFacts`].
#[doc(hidden)]
pub fn inspect_facts(files: &[PathBuf], stop: Option<&str>) -> Result<InspectFacts, RdyError> {
    check_count(files)?;
    let stop = stop.map(DumpStop::parse).transpose()?;
    let plan = inspect(files, stop.as_ref())?;
    let Some(s) = plan.scan() else {
        return Ok(InspectFacts {
            legacy: plan.legacy(),
            end: None,
            explicit_stop: false,
            holder: false,
            owner: None,
            owner_files: vec![],
            roles: vec![],
            carried: 0,
            residues: 0,
        });
    };
    debug_assert_eq!(plan.owner(), Some(plan.files()[s.owner()].as_path()));
    Ok(InspectFacts {
        legacy: None,
        end: Some(s.end()),
        explicit_stop: s.explicit_stop(),
        holder: s.mode() == StoreMode::Holder,
        owner: Some(s.owner()),
        owner_files: s.files()[s.owner()]
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect(),
        roles: s
            .files()
            .iter()
            .map(|f| match f.role {
                FileRole::Retained => "retained",
                FileRole::Stop => "stop",
                FileRole::After => "after",
            })
            .collect(),
        carried: s.carried().len(),
        residues: s.residues().len(),
    })
}
