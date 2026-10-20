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
use crate::carried_warning;
use crate::dump_error;
use crate::dump_stop::{DumpStop, MAX_CHAIN_FILES};
use crate::error::{E_BAD_INPUT, E_IO, RdyError};
use crate::load::{
    LoadSummary, LoadedChainFile, Made, ORIGIN, StoppedAt, check_target, create_store, damaged,
    holder_files, incomplete, next_step, open_dump, place, read_error, target_error,
};
use rdy_store::StoreError;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, TRAILER_LEN, read_envelope};
use rdy_store::dump_chain::DumpKind;
use rdy_store::dump_scan::{
    ChainLink, ChainSource, Envelope, FileRole, METADATA_MISMATCH, ProjectFile, ScanError,
    ScanPlan, StopFailure, StopMatch, scan,
};
use rdy_store::holder::StoreMode;
use rdy_store::ingest::Ingest;
use rdy_store::redaction_carried::check_bounds;
use rdy_store::segment::Location;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The refusal of an input whose bytes differ from what the pre-pass checked.
const CHANGED: &str = "dump changed during load";
/// The length of a dump record's frame header.
const RECORD_HEAD: u64 = 37;
/// The byte offset of a dump's header record, after its prelude.
const HEADER_AT: u64 = 12;
/// The record kind of a project file.
const FILE_RECORD: u8 = 0xF1;

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
    Chain(Box<ScanPlan>),
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
    // inputs whose verified trailers count no batch hold no log: the released refusal, not the
    // metadata check that such a log cannot pass (every store holds its format batch)
    let empty = src.envelopes().iter().all(|e| e.trailer.batches == 0);
    let plan = scan(&mut src, stop.map(stop_match)).map_err(|e| match e {
        ScanError::Input {
            source: StoreError::Dump { ref what, .. },
            ..
        } if empty && what == METADATA_MISMATCH => incomplete("it holds no batches".to_string()),
        e => scan_error(files, e),
    })?;
    // a holder carries no project file, in any input (D-BPYN8)
    if plan.mode() == StoreMode::Holder && plan.files().iter().any(|f| !f.files.is_empty()) {
        return Err(holder_files());
    }
    Ok(ChainPlan {
        files: files.to_vec(),
        inner: Inner::Chain(Box::new(plan)),
    })
}

/// Loads the verified chain `plan` into `project` (`crate::load::load_chain`), in one
/// transaction:
///
/// 1. Read the project files of the input that owns them, at the places the pre-pass recorded
///    and with the hashes it read, then check the target (`check_target`): nothing is created
///    before both pass.
/// 2. Create `.rdy/store.loading` (`create_store`), then read each input that contributes
///    retained batches, in argument order, through its end with the dump reader: each payload
///    is hashed, the batches after the stop included. Its retained batches go to the one ingest
///    at their source places, each frame as the plan decides (`Ingest::batch_with_plan`): a
///    payload a removal at or before the stop removed is written as its header and zeros only.
///    Each input's trailer must be the one the pre-pass read; an input wholly after the stop has
///    its envelope read again and compared. A difference is `dump changed during load`.
/// 3. The retained batches must end at the selected end, and every frame the plan names must
///    be one the ingest met. Then the carried batch (`Ingest::append_carried`, once), the one
///    upgrade, the ingest's final check, and the load's steps 5 to 7 (`place`).
///
/// Any failure takes back what the load made (`Made::undo`), and no store is in place before
/// every input passed.
pub(crate) fn load_verified(plan: &ChainPlan, project: &Path) -> Result<LoadSummary, RdyError> {
    let Some(scan) = plan.scan() else {
        return Err(RdyError::new(
            crate::error::E_INTERNAL,
            "a chain load was given no verified chain",
            "report this as a bug",
        ));
    };
    let paths = plan.files();
    let owner = scan.owner();
    let files = owner_files(&paths[owner], &scan.files()[owner].files)?;
    let absent = check_target(project, &files)?;
    let mut made = Made::default();
    let out = (|| {
        let (mut ingest, loading) = create_store(project, &mut made)?;
        let (batches, frames) = ingest_inputs(plan, scan, &mut ingest, &loading)?;
        let finished = ingest_tail(plan, scan, ingest, &loading)?;
        debug_assert_eq!((finished.batches, finished.frames), (batches, frames));
        let records = carried_records(scan)?;
        let holder = scan.mode() == StoreMode::Holder;
        let warnings = carried_warning::for_load(scan.mode(), &records)?;
        let placed = place(project, &files, &absent, &mut made, None)?;
        let first = &scan.envelopes()[0];
        Ok(LoadSummary {
            project: project.display().to_string(),
            dump_format: first.format,
            store_format: first.header.store_format,
            batches,
            frames,
            files_written: placed.written,
            files_kept: placed.kept,
            landings: placed.fsck.landings,
            tags: placed.tags,
            views: placed.views,
            fsck: placed.fsck,
            next: next_step(holder),
            chain: chain_rows(paths, scan.envelopes()),
            stopped_at: scan.explicit_stop().then(|| StoppedAt {
                position: scan.end(),
                landing: scan.landing(),
                tag: scan.tag().map(str::to_string),
            }),
            warnings,
        })
    })();
    if out.is_err() {
        made.undo();
    }
    out
}

/// The rows of the inputs `paths`, from the envelopes the pre-pass read.
fn chain_rows(paths: &[PathBuf], envelopes: &[Envelope]) -> Vec<LoadedChainFile> {
    paths
        .iter()
        .zip(envelopes)
        .map(|(p, e)| LoadedChainFile {
            file: p.display().to_string(),
            kind: e.header.chain.map_or(DumpKind::Full, |c| c.kind),
            base: e.header.chain.map_or(ORIGIN, |c| c.base),
            end: e.header.log_end,
        })
        .collect()
}

/// The prepared carried records with the hashes of their frames, in the plan's order: one
/// warning each (D-QF2HQ).
fn carried_records(
    scan: &ScanPlan,
) -> Result<Vec<(rdy_model::Hash, rdy_model::RedactionCarried)>, RdyError> {
    let mut out = Vec::new();
    out.try_reserve_exact(scan.carried().len())
        .map_err(|_| allocation())?;
    for r in scan.carried() {
        let bytes = check_bounds(r)?;
        out.push((rdy_model::Hash::of(&bytes), r.clone()));
    }
    Ok(out)
}

fn allocation() -> RdyError {
    RdyError::new(
        crate::error::E_LIMIT,
        "record allocation failed",
        "Free memory, then run the command again.",
    )
}

/// The refusal of input `file`, whose bytes at `at` are not those the pre-pass checked.
fn changed(file: &Path, at: u64) -> RdyError {
    damaged(file, at, CHANGED)
}

/// The byte offset of the trailer record of the input of dump format 3 whose envelope is `e`.
fn trailer_at(e: &Envelope) -> u64 {
    e.len.saturating_sub(RECORD_HEAD + TRAILER_LEN as u64)
}

/// The project files of the input `path` that the pre-pass recorded as `wanted`: each record read
/// at its offset, with its header and payload checked against the recorded length and hash.
fn owner_files(path: &Path, wanted: &[ProjectFile]) -> Result<Vec<(String, Vec<u8>)>, RdyError> {
    let mut out = Vec::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    out.try_reserve_exact(wanted.len())
        .map_err(|_| allocation())?;
    let mut f = BufReader::new(open_dump(path)?);
    let io = |e: std::io::Error| read_error(path, StoreError::Io(e));
    for w in wanted {
        f.seek(SeekFrom::Start(w.offset)).map_err(io)?;
        let mut head = [0u8; RECORD_HEAD as usize];
        f.read_exact(&mut head)
            .map_err(|_| changed(path, w.offset))?;
        let len = u64::from(u32::from_le_bytes(head[0..4].try_into().expect("4 bytes")));
        if len != w.len || head[4] != FILE_RECORD || head[5..] != w.hash.0 {
            return Err(changed(path, w.offset));
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(len as usize)
            .map_err(|_| allocation())?;
        (&mut f).take(len).read_to_end(&mut payload).map_err(io)?;
        if payload.len() as u64 != len || rdy_model::Hash::of(&payload) != w.hash {
            return Err(changed(path, w.offset));
        }
        let name_len = payload
            .get(0..2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            .ok_or_else(|| changed(path, w.offset))?;
        let name = payload
            .get(2..2 + name_len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .filter(|n| *n == w.path)
            .ok_or_else(|| changed(path, w.offset))?;
        let name = name.to_string();
        payload.drain(..2 + name_len);
        out.push((name, payload));
    }
    Ok(out)
}

/// Step 2 of [`load_verified`]: every input in argument order. Returns the retained source
/// batches and data frames.
fn ingest_inputs(
    plan: &ChainPlan,
    scan: &ScanPlan,
    ingest: &mut Ingest,
    loading: &Path,
) -> Result<(u64, u64), RdyError> {
    let paths = plan.files();
    let planned = scan.planned_places();
    let mut cursor = 0usize;
    let mut n = 0u64;
    let mut frames = 0u64;
    for (i, facts) in scan.files().iter().enumerate() {
        let path = paths[i].as_path();
        let env = &scan.envelopes()[i];
        if facts.role == FileRole::After {
            // its body was read whole by the pre-pass: its envelope must be the one it read
            let mut f = open_dump(path)?;
            let (format, header, trailer, len) =
                read_envelope(&mut f).map_err(|e| read_error(path, e))?;
            if format != env.format || header != env.header {
                return Err(changed(path, HEADER_AT));
            }
            if trailer != env.trailer || len != env.len {
                // the trailer of the file as it is now
                return Err(changed(
                    path,
                    len.saturating_sub(RECORD_HEAD + TRAILER_LEN as u64),
                ));
            }
            continue;
        }
        let f = open_dump(path)?;
        let (mut reader, format, header) =
            DumpReader::open(BufReader::new(f)).map_err(|e| read_error(path, e))?;
        if format != env.format || header != env.header {
            return Err(changed(path, HEADER_AT));
        }
        let mut kept = 0u64;
        loop {
            let at = reader.offset();
            match reader.next().map_err(|e| read_error(path, e))? {
                // the owner's files were read before the target was touched; the reader hashes
                // each one again
                Some(DumpItem::File { .. }) => {}
                Some(DumpItem::Batch(b)) if kept < facts.kept_batches => {
                    kept += 1;
                    n = n.checked_add(1).ok_or_else(|| {
                        damaged(path, at, rdy_store::dump_chain::chain_text::OVERFLOW)
                    })?;
                    meet_planned(&planned, &mut cursor, &b)
                        .map_err(|what| damaged(path, at, what))?;
                    ingest.batch_with_plan(n, &b, scan).map_err(|e| match e {
                        StoreError::Dump { what, .. } => damaged(path, at, &what),
                        e => target_error(loading, e),
                    })?;
                    frames += b.frames.len() as u64;
                }
                // a batch after the stop: read and hashed by the reader, then dropped
                Some(DumpItem::Batch(_)) => {}
                None => {
                    if reader.trailer() != Some(&env.trailer) || kept != facts.kept_batches {
                        return Err(changed(path, at));
                    }
                    break;
                }
            }
        }
    }
    if cursor != planned.len() {
        let file = &paths[scan.owner()];
        return Err(damaged(
            file,
            trailer_at(&scan.envelopes()[scan.owner()]),
            "dropped frame does not match its authorized span",
        ));
    }
    Ok((n, frames))
}

/// Notes the places of `b`'s frames that the plan names, in log order: a planned place that no
/// frame starts at is refused.
fn meet_planned(
    planned: &[Location],
    cursor: &mut usize,
    b: &DumpBatch,
) -> Result<(), &'static str> {
    const SPAN: &str = "dropped frame does not match its authorized span";
    let mut place = b.start;
    for f in &b.frames {
        if planned.get(*cursor).is_some_and(|p| *p < place) {
            return Err(SPAN);
        }
        if planned.get(*cursor) == Some(&place) {
            *cursor += 1;
        }
        place.offset = place
            .offset
            .checked_add(RECORD_HEAD)
            .and_then(|o| o.checked_add(u64::from(f.original_len())))
            .ok_or(rdy_store::dump_chain::chain_text::OVERFLOW)?;
    }
    Ok(())
}

/// Step 3 of [`load_verified`]: the end check, the carried batch, the upgrade and the final
/// check of the ingest.
fn ingest_tail(
    plan: &ChainPlan,
    scan: &ScanPlan,
    mut ingest: Ingest,
    loading: &Path,
) -> Result<rdy_store::ingest::IngestReport, RdyError> {
    let owner = scan.owner();
    let file = plan.files()[owner].as_path();
    let at = trailer_at(&scan.envelopes()[owner]);
    let end = ingest.end();
    if end != scan.end() {
        let want = scan.end();
        return Err(incomplete(format!(
            "its header says the log ends at segment {} offset {}, but its batches end at segment {} offset {}",
            want.segment, want.offset, end.segment, end.offset
        )));
    }
    if scan.files().iter().all(|f| f.kept_batches == 0) {
        return Err(incomplete("it holds no batches".to_string()));
    }
    ingest
        .append_carried(scan.residues(), scan.carried())
        .map_err(|e| target_error(loading, e))?;
    ingest
        .upgrade(scan.envelopes()[0].header.store_format)
        .map_err(|e| target_error(loading, e))?;
    ingest.finish().map_err(|e| match e {
        StoreError::Dump { what, .. } => damaged(file, at, &what),
        e => target_error(loading, e),
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
