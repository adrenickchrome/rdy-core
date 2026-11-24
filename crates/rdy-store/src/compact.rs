//! The log's byte report and compact (D-4YRQ4: "Compaction never moves a frame, never changes a
//! byte that rdy reads and never changes a hash."). One walk of the frame headers counts every
//! physical frame; the removed payloads are then punched, best effort. No frame is appended.
//!
//! Compact also runs the span map's mapping pass (D-1ER84, `crate::dedup`): it scans the sealed
//! log for exact repeated byte runs ([`crate::dedup_candidates`]), appends their rows and one
//! `Through` row, syncs the span map once, publishes the rows under the map's write lock, then
//! punches every durable mapped range. A store opened by a write runs the same pass once at
//! least [`crate::dedup::THRESHOLD_BYTES`] unmapped sealed bytes follow the last `Through`.
use crate::dedup::{self, MapEvent, Mapping, SpanMap, StopReason};
use crate::dedup_candidates::{self as cand, Class, FrameRef, Run};
use crate::error::StoreError;
use crate::segment::{Location, MAX_PUNCH_SPANS, punch_diagnostics};
use crate::store::batch_open;
use crate::{Store, codec, kinds, pio};
use rdy_model::Hash;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;

/// The most segments one report counts.
pub const MAX_REPORT_SEGMENTS: u64 = 65_536;
/// The most frame headers one report counts.
pub const MAX_REPORT_FRAMES: u64 = 100_000_000;
/// The most removed spans one report counts.
pub const MAX_REPORT_SPANS: u64 = MAX_PUNCH_SPANS;

/// The bytes of a frame header: length, kind and hash.
const HEADER: u64 = 37;

/// The note of a final allocation census that failed.
const CENSUS_FAILED: &str =
    "disk allocation could not be read after punching; on_disk_bytes is the count before this call";

/// A report's refusal: the store's own error, or a closed limit of the count.
#[derive(Debug, thiserror::Error)]
pub enum CompactError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Limit(String),
}

impl From<io::Error> for CompactError {
    fn from(e: io::Error) -> Self {
        CompactError::Store(e.into())
    }
}

/// One logical kind's row of a byte report. `bytes` holds its frames' headers and payloads, and
/// `removed_bytes` the part of them that is removed payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KindBytes {
    pub kind: u8,
    pub name: &'static str,
    pub bytes: u64,
    pub removed_bytes: u64,
}

/// The log's bytes: every sealed frame and marker, with its header, and the removed payloads among
/// them. A row exists only for a kind the log holds, by numeric kind.
///
/// `deduplicated_bytes` is the target payload bytes of the durable span map rows whose frames
/// no removal names: a logical count of mapped bytes, not the allocation they freed. `notes`
/// holds the span map's stopped-mapping and full-map notes; it is left out of JSON when empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogBytes {
    pub log_bytes: u64,
    pub removed_bytes: u64,
    pub deduplicated_bytes: u64,
    pub by_kind: Vec<KindBytes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// What [`Store::compact`] found and did. `on_disk_bytes` is the segment files' allocation after
/// the call, and `punched_bytes` the allocation that the call freed.
///
/// `deduplicated_bytes` counts as [`LogBytes`] does; a dry run includes the candidates this
/// call would add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompactReport {
    pub log_bytes: u64,
    pub removed_bytes: u64,
    pub deduplicated_bytes: u64,
    pub punched_bytes: u64,
    pub on_disk_bytes: u64,
    pub by_kind: Vec<KindBytes>,
    pub notes: Vec<String>,
}

/// Counts a census starts from, in place of a log too large to build (test hook).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CensusSeed {
    pub segments: u64,
    pub frames: u64,
    pub spans: u64,
    pub bytes: u64,
}

/// A test backend of one segment's allocation; `None` reads the file.
#[doc(hidden)]
pub type AllocationBackend = Box<dyn FnMut(u32) -> Option<io::Result<u64>>>;

thread_local! {
    static CENSUS_SEED: RefCell<CensusSeed> = RefCell::new(CensusSeed::default());
    static ALLOCATION: RefCell<Option<AllocationBackend>> = const { RefCell::new(None) };
}

/// Test hook: every report on this thread starts from `seed`. None removes it.
#[doc(hidden)]
pub fn on_census(seed: Option<CensusSeed>) {
    CENSUS_SEED.with(|s| *s.borrow_mut() = seed.unwrap_or_default());
}

/// Test hook: every allocation census on this thread asks `backend` for each segment. None removes
/// it.
#[doc(hidden)]
pub fn on_allocation(backend: Option<AllocationBackend>) {
    ALLOCATION.with(|b| *b.borrow_mut() = backend);
}

/// The report's name of logical kind `kind`, from the closed table. None for a code no kind has.
fn kind_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        kinds::BLOCK => "block",
        kinds::CHANGE => "change",
        kinds::PROPOSAL => "proposal",
        kinds::LANDING => "landing",
        kinds::SNAPSHOT => "snapshot",
        kinds::BLOB_CHUNK => "blob_chunk",
        kinds::BLOB_MANIFEST => "blob_manifest",
        kinds::REF_UPDATE => "ref_update",
        kinds::TAG => "tag",
        kinds::ANSWER => "answer",
        kinds::SIGNATURE => "signature",
        kinds::VIEW_OP => "view_op",
        kinds::BATCH_COMMIT => "batch_commit",
        kinds::INTENT => "intent",
        kinds::ALT_SET => "alt_set",
        kinds::REVIEW => "review",
        kinds::TENSION => "tension",
        kinds::TRANSITION => "transition",
        kinds::REDACTION => "redaction",
        kinds::REDACTION_RESIDUE => "redaction_residue",
        kinds::REDACTION_APPLY => "redaction_apply",
        kinds::REDACTION_CARRIED => "redaction_carried",
        kinds::PLAN => "plan",
        kinds::ASSESSMENT => "assessment",
        kinds::DISPUTE => "dispute",
        kinds::DISPUTE_RESOLUTION => "dispute_resolution",
        kinds::CHECK_REQUEST => "check_request",
        kinds::FOREIGN => "foreign",
        kinds::SNAPSHOT_DELTA => "snapshot_delta",
        _ => return None,
    })
}

fn over(limit: u64, unit: &str) -> CompactError {
    CompactError::Limit(format!("the log byte report exceeds {limit} {unit}"))
}

fn add(a: u64, b: u64) -> Result<u64, CompactError> {
    a.checked_add(b).ok_or_else(|| {
        CompactError::Limit(format!("the log byte count exceeds {} bytes", u64::MAX))
    })
}

/// The first bytes of a marked frame's payload: its version, which [`codec::split_stored`] reads.
/// A payload too short to hold them is what the split refuses.
fn version_prefix(file: &std::fs::File, at: u64, len: u32) -> io::Result<([u8; 2], usize)> {
    let mut prefix = [0u8; 2];
    let n = (len as usize).min(prefix.len());
    pio::read_exact_at(file, &mut prefix[..n], at)?;
    Ok((prefix, n))
}

impl Store {
    /// Counts the log by one walk of its frame headers. Every physical frame counts, duplicates and
    /// markers included. A frame whose raw kind or encoding this rdy does not read, a removed span
    /// that does not read as zeros, or one whose header names something else, is refused with the
    /// store's own error. An open held batch is refused too.
    pub fn log_bytes(&self) -> Result<LogBytes, CompactError> {
        if self.open_batch().is_some() {
            return Err(batch_open("first").into());
        }
        let seed = CENSUS_SEED.with(|s| *s.borrow());
        let log = self.segment_log();
        let segments = add(log.readers().len() as u64, seed.segments)?;
        if segments > MAX_REPORT_SEGMENTS {
            return Err(over(MAX_REPORT_SEGMENTS, "segments"));
        }
        let holes = &log.authorization().holes;
        let spans = add(holes.len() as u64, seed.spans)?;
        if spans > MAX_REPORT_SPANS {
            return Err(over(MAX_REPORT_SPANS, "removed spans"));
        }

        // (bytes, removed bytes) by logical kind: the counters are fixed, whatever the log holds
        let mut rows = [(0u64, 0u64); 256];
        let mut frames = seed.frames;
        let mut total = seed.bytes;
        let mut removed = 0u64;
        let mut next_hole = holes.iter().peekable();
        log.walk_headers::<CompactError>(|file, loc, len, raw, hash| {
            frames = add(frames, 1)?;
            if frames > MAX_REPORT_FRAMES {
                return Err(over(MAX_REPORT_FRAMES, "frames"));
            }
            let kind = codec::logical_kind(raw);
            let payload_at = loc.offset + HEADER;
            let row = &mut rows[usize::from(kind)];
            let bytes = HEADER + u64::from(len);
            match next_hole.peek().copied() {
                // both walks ascend, so a hole before this frame is a place no frame starts at
                Some((place, _)) if *place < loc => {
                    return Err(StoreError::corrupt_at(*place).into());
                }
                Some((place, hole)) if *place == loc => {
                    next_hole.next();
                    if (len, kind, hash) != (hole.len, hole.kind, hole.hash)
                        || !crate::segment::payload_is_zero(file, payload_at, u64::from(len))?
                    {
                        return Err(StoreError::corrupt_at(loc).into());
                    }
                    row.1 = add(row.1, u64::from(len))?;
                    removed = add(removed, u64::from(len))?;
                }
                _ => {
                    // the decoder of the store decides whether this kind and encoding are valid
                    let (mut prefix, n) = if raw & codec::VERSIONED == 0 {
                        ([0; 2], 0)
                    } else {
                        version_prefix(file, payload_at, len)?
                    };
                    // the prefix is read through the span map, as every logical read is
                    if n > 0 {
                        let map = log.spans().read().unwrap_or_else(|e| e.into_inner());
                        if map.is_target(&loc) {
                            map.overlay_window(
                                &crate::segment::LogFiles(log.readers()),
                                loc,
                                raw,
                                hash,
                                u64::from(len),
                                0,
                                &mut prefix[..n],
                            )?;
                        }
                    }
                    codec::split_stored(raw, &prefix[..n])?;
                }
            }
            if kind_name(kind).is_none() {
                return Err(codec::unknown_encoding(raw, &[]).into());
            }
            row.0 = add(row.0, bytes)?;
            total = add(total, bytes)?;
            Ok(())
        })?;
        if let Some((place, _)) = next_hole.next() {
            return Err(StoreError::corrupt_at(*place).into());
        }

        let by_kind = (0..=u8::MAX)
            .filter_map(|kind| {
                let (bytes, removed_bytes) = rows[usize::from(kind)];
                (bytes > 0).then_some(())?;
                Some(KindBytes {
                    kind,
                    name: kind_name(kind)?,
                    bytes,
                    removed_bytes,
                })
            })
            .collect();
        let map = log.spans().read().unwrap_or_else(|e| e.into_inner());
        Ok(LogBytes {
            log_bytes: total,
            removed_bytes: removed,
            deduplicated_bytes: map.mapped_bytes(|p| holes.contains_key(p)),
            by_kind,
            notes: map.notes(),
        })
    }

    /// The bytes the file system has allocated to the segment files.
    fn allocated(&self) -> Result<u64, CompactError> {
        let mut total = 0;
        for (seg, file) in (0u32..).zip(self.segment_log().readers()) {
            let injected =
                ALLOCATION.with(|b| b.borrow_mut().as_mut().and_then(|backend| backend(seg)));
            let bytes = match injected {
                Some(answer) => answer?,
                None => pio::allocated_bytes(file)?,
            };
            total = add(total, bytes)?;
        }
        Ok(total)
    }

    /// Validates and counts the log, runs the span map's mapping pass (D-1ER84) when the store
    /// lists `dedup_spans`, then tries to punch every removed payload, in ascending place order,
    /// and every durably mapped range. With `dry_run` it only reports, the candidates this call
    /// would map included: no row, sync, punch, reindex or publish. It takes no approval and
    /// appends no frame. A punch outcome is a note, never an error, and so is a full span map
    /// and a final allocation census that fails; everything before the first punch is an
    /// ordinary error.
    pub fn compact(&self, dry_run: bool) -> Result<CompactReport, CompactError> {
        self.ensure_writable()?;
        let LogBytes {
            log_bytes,
            removed_bytes,
            deduplicated_bytes,
            by_kind,
            notes: _,
        } = self.log_bytes()?;
        let before = self.allocated()?;
        let mut report = CompactReport {
            log_bytes,
            removed_bytes,
            deduplicated_bytes,
            punched_bytes: 0,
            on_disk_bytes: before,
            by_kind,
            notes: vec![],
        };
        let pass = if self.maps_spans() {
            Some(self.prepare_pass(ORIGIN_PLACE)?)
        } else {
            None
        };
        if dry_run {
            if let Some(p) = &pass {
                report.deduplicated_bytes +=
                    p.maps.iter().map(|m| u64::from(m.target.len)).sum::<u64>();
                report.notes = self.map_notes(p.full);
            }
            return Ok(report);
        }
        if let Some(p) = pass {
            self.commit_pass(p)?;
        }
        let summary = self.segment_log().punch_removed(MAX_PUNCH_SPANS);
        let mapped = self.punch_mapped();
        report.notes = punch_diagnostics(&summary);
        let map_full = {
            let map = self
                .segment_log()
                .spans()
                .read()
                .unwrap_or_else(|e| e.into_inner());
            report.deduplicated_bytes =
                map.mapped_bytes(|p| self.segment_log().authorization().holes.contains_key(p));
            map.is_full()
        };
        report.notes.extend(self.map_notes(map_full));
        report.notes.extend(mapped_diagnostics(&mapped));
        match self.allocated() {
            Ok(after) => {
                report.on_disk_bytes = after;
                report.punched_bytes = before.saturating_sub(after);
            }
            Err(_) => report.notes.push(CENSUS_FAILED.to_string()),
        }
        Ok(report)
    }

    /// The span map's notes: the stopped-mapping note this handle keeps, then the full-map note
    /// when `full`.
    fn map_notes(&self, full: bool) -> Vec<String> {
        let map = self
            .segment_log()
            .spans()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let mut out = vec![];
        if let Some(why) = map.stopped() {
            out.push(why.note());
        }
        if full || map.is_full() {
            out.push(dedup::FULL_NOTE.to_string());
        }
        out
    }

    /// Whether this store maps spans: a writable log of a store that lists `dedup_spans`.
    pub(crate) fn maps_spans(&self) -> bool {
        !self.segment_log().is_read_only()
            && self.has_feature_now(crate::format::FEATURE_DEDUP_SPANS)
            && self.open_batch().is_none()
    }

    /// The unmapped sealed log bytes: from the last durable `Through` (or the log's start) to
    /// the end of the log, across segments.
    pub(crate) fn unmapped_bytes(&self) -> u64 {
        let log = self.segment_log();
        let from = log
            .spans()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .through()
            .unwrap_or(ORIGIN_PLACE);
        let end = log.end();
        if end <= from {
            return 0;
        }
        let mut total = 0u64;
        for seg in from.segment..=end.segment {
            let start = if seg == from.segment { from.offset } else { 0 };
            let stop = if seg == end.segment {
                end.offset
            } else {
                log.segment_len(seg).unwrap_or(0)
            };
            total = total.saturating_add(stop.saturating_sub(start));
        }
        total
    }

    /// The threshold pass of an ordinary write (D-1ER84): once the write's own batch is
    /// durable, a store with at least [`dedup::THRESHOLD_BYTES`] unmapped sealed bytes maps
    /// them in one pass. Its failure never reaches the write: it stops threshold passes on this
    /// handle and keeps the note for the next compact. A pass that finds the map full stops
    /// threshold passes on this handle until reopen.
    pub(crate) fn threshold_pass(&self) {
        if !self.maps_spans() {
            return;
        }
        let allowed = self
            .segment_log()
            .spans()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .threshold_allowed();
        if !allowed || self.unmapped_bytes() < dedup::THRESHOLD_BYTES {
            return;
        }
        let _ = dedup::event(MapEvent::Threshold);
        let from = self
            .segment_log()
            .spans()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .through()
            .unwrap_or(ORIGIN_PLACE);
        let result = self.prepare_pass(from).and_then(|p| {
            let full = p.full;
            self.commit_pass(p)?;
            Ok(full)
        });
        let mut map = self
            .segment_log()
            .spans()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(true) => map.disable_threshold(),
            Ok(false) => {}
            Err(e) => map.stop(StopReason::of(&e)),
        }
        drop(map);
        let _ = self.punch_mapped();
    }

    /// The span-mapping step's pass (`crate::upgrade::DEDUP_SPANS_STEP`): when the durable map
    /// does not yet reach `source_end`, one pass from its checked progress (or the log start)
    /// through the sealed end, then the punches of mapped ranges. A retry after a crash resumes
    /// from the progress the map holds; a map that already reaches `source_end` does nothing,
    /// so a second open rescans nothing. Its failure is an ordinary error.
    pub(crate) fn upgrade_map_pass(&self, source_end: Location) -> Result<(), StoreError> {
        if self.segment_log().is_read_only() {
            return Ok(());
        }
        let through = self
            .segment_log()
            .spans()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .through();
        if through.is_some_and(|t| t >= source_end) {
            return Ok(());
        }
        let pass = self.prepare_pass(through.unwrap_or(ORIGIN_PLACE))?;
        self.commit_pass(pass)?;
        let _ = self.punch_mapped();
        Ok(())
    }

    /// The candidates of one pass over the sealed log from `from` to its end, against the
    /// durable map: every new mapping, verified byte for byte, aligned, resolved to retained
    /// source bytes and admitted within the map's bounds.
    fn prepare_pass(&self, from: Location) -> Result<Pass, StoreError> {
        let _ = dedup::event(MapEvent::Scan);
        let log = self.segment_log();
        let end = log.end();
        let holes = &log.authorization().holes;
        // the target frames, from the headers alone; a SNAPSHOT keeps its predecessor
        let mut targets: Vec<(Location, u8, Hash, Option<FrameFacts>)> = vec![];
        let mut last_snapshot = None;
        let fits = |len: u32| len as usize <= cand::MAX_PAYLOAD_BYTES;
        log.walk_headers::<StoreError>(|_, loc, len, raw, hash| {
            if loc >= end {
                return Ok(());
            }
            let kind = codec::logical_kind(raw);
            if holes.contains_key(&loc) {
                if kind == kinds::SNAPSHOT {
                    last_snapshot = None;
                }
                return Ok(());
            }
            if !fits(len) {
                // no payload buffer over the bound: the frame is neither a target nor a source
                if kind == kinds::SNAPSHOT {
                    last_snapshot = None;
                }
                return Ok(());
            }
            let target = match kind {
                kinds::CHANGE | kinds::PLAN => Some(None),
                kinds::SNAPSHOT => Some(last_snapshot),
                _ => None,
            };
            if kind == kinds::SNAPSHOT {
                last_snapshot = Some((loc, raw, hash));
            }
            if let Some(pred) = target
                && loc >= from
            {
                targets.try_reserve(1).map_err(|_| StoreError::DedupLimit)?;
                targets.push((loc, raw, hash, pred));
            }
            Ok(())
        })?;

        let durable = log.spans().read().unwrap_or_else(|e| e.into_inner());
        let mut pass = Pass {
            end,
            ..Pass::default()
        };
        let mut alignments: BTreeMap<u32, Option<u64>> = BTreeMap::new();
        // a source frame is read only when its header's length is within the payload bound
        let files = crate::segment::LogFiles(log.readers());
        let source_fits = |at: Location| {
            dedup::SegmentFiles::header(&files, at)
                .ok()
                .flatten()
                .is_some_and(|(len, _, _)| fits(len))
        };
        for (loc, raw, hash, pred) in targets {
            if pass.full {
                break;
            }
            if durable.is_target(&loc) {
                continue;
            }
            let (_, _, payload) = log.read_hashed(loc)?;
            let target = FrameRef {
                place: loc,
                raw_kind: raw,
                hash,
                payload: &payload,
            };
            let runs: Vec<Run> = match codec::logical_kind(raw) {
                kinds::CHANGE => cand::change_runs(&target, |version| {
                    let Some(at) = self.index().typed(kinds::BLOCK, version)? else {
                        return Ok::<_, StoreError>(None);
                    };
                    if holes.contains_key(&at) || at >= loc || !source_fits(at) {
                        return Ok(None);
                    }
                    let (r, h, p) = log.read_hashed(at)?;
                    Ok(Some((at, r, h, p)))
                })?,
                kinds::PLAN => match cand::plan_of(raw, &payload).and_then(|p| p.prev) {
                    Some(prev) => match self.index().typed(kinds::PLAN, &prev)? {
                        Some(at) if !holes.contains_key(&at) && at < loc && source_fits(at) => {
                            let (r, h, p) = log.read_hashed(at)?;
                            let source = FrameRef {
                                place: at,
                                raw_kind: r,
                                hash: h,
                                payload: &p,
                            };
                            cand::plan_runs(&target, &source)
                        }
                        _ => vec![],
                    },
                    None => vec![],
                },
                kinds::SNAPSHOT => match pred {
                    Some((at, _, _)) => {
                        let (r, h, p) = log.read_hashed(at)?;
                        let source = FrameRef {
                            place: at,
                            raw_kind: r,
                            hash: h,
                            payload: &p,
                        };
                        cand::snapshot_runs(&target, &source)
                    }
                    None => vec![],
                },
                _ => vec![],
            };
            if runs.is_empty() {
                continue;
            }
            let a = match alignments.get(&loc.segment) {
                Some(a) => *a,
                None => {
                    let file = log
                        .readers()
                        .get(loc.segment as usize)
                        .ok_or_else(|| StoreError::corrupt_at(loc))?;
                    let a = pio::dedup_alignment(file)?;
                    alignments.insert(loc.segment, a);
                    a
                }
            };
            for run in runs {
                let class = run.class.index();
                pass.table.candidate_bytes[class] += u64::from(run.target.len);
                let Some(a) = a else {
                    pass.table.unsupported_bytes += u64::from(run.target.len);
                    continue;
                };
                for piece in resolve(&run, &durable, &pass.maps) {
                    let Some(m) = cand::align(&piece, a) else {
                        pass.table.trimmed_bytes += u64::from(piece.target.len);
                        continue;
                    };
                    pass.table.trimmed_bytes += u64::from(piece.target.len - m.target.len);
                    let mapping = Mapping {
                        target: m.target,
                        source: m.source,
                    };
                    if !durable.admits(&pass.maps, &mapping) {
                        continue;
                    }
                    // room for this row and the pass's final Through, or the map is full
                    let rows = pass.maps.len() as u64 + 2;
                    let bytes = rows * (dedup::MAX_ROW_BYTES as u64 + 36);
                    if !durable.has_room(rows, bytes) {
                        pass.full = true;
                        break;
                    }
                    pass.table.mapped_bytes[class] += u64::from(m.target.len);
                    pass.maps
                        .try_reserve(1)
                        .map_err(|_| StoreError::DedupLimit)?;
                    pass.maps.push(mapping);
                }
                if pass.full {
                    break;
                }
            }
        }
        if !pass.full && !durable.has_room(1, dedup::MAX_ROW_BYTES as u64 + 36) {
            pass.full = true;
        }
        LAST_TABLE.with(|t| *t.borrow_mut() = pass.table);
        Ok(pass)
    }

    /// Makes one pass durable: its Map rows and one `Through` (when the sealed end advanced or
    /// a row was found; a full map adds none), one sync of the span map, then publication under
    /// the map's write lock. A full map is recorded on the handle.
    fn commit_pass(&self, pass: Pass) -> Result<(), StoreError> {
        let log = self.segment_log();
        let spans = log.spans();
        let staged = {
            let map = spans.read().unwrap_or_else(|e| e.into_inner());
            let advanced = map.through().is_none_or(|t| pass.end > t);
            let through = (!pass.full && (advanced || !pass.maps.is_empty())).then_some(pass.end);
            if pass.maps.is_empty() && through.is_none() {
                None
            } else {
                Some(map.append(self.dir(), &pass.maps, through)?)
            }
        };
        let mut map = spans.write().unwrap_or_else(|e| e.into_inner());
        if let Some(staged) = staged {
            map.publish(staged)?;
        }
        if pass.full {
            map.set_full();
        }
        Ok(())
    }

    /// Punches every durably mapped target range whose frame no removal names, under the span
    /// map's write lock: a reader that started first finishes first, and a later one fills the
    /// hole from the map. Each range's target and source headers are read again first. A
    /// punch's outcome is counted, never an error; no segment is synced after it.
    fn punch_mapped(&self) -> crate::segment::PunchSummary {
        let log = self.segment_log();
        let mut summary = crate::segment::PunchSummary::default();
        if log.is_read_only() {
            return summary;
        }
        let holes = &log.authorization().holes;
        let map = log.spans().write().unwrap_or_else(|e| e.into_inner());
        let files = crate::segment::LogFiles(log.readers());
        let mut open: Option<(u32, Option<File>)> = None;
        for m in map.mappings() {
            if holes.contains_key(&m.target.place) {
                continue;
            }
            if summary.attempted >= MAX_PUNCH_SPANS {
                summary.not_tried += 1;
                continue;
            }
            summary.attempted += 1;
            let seg = m.target.place.segment;
            if open.as_ref().is_none_or(|(s, _)| *s != seg) {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(crate::segment::seg_path(log.dir(), seg))
                    .ok();
                open = Some((seg, file));
            }
            let Some(file) = open.as_ref().and_then(|(_, f)| f.as_ref()) else {
                summary.failed += 1;
                continue;
            };
            let t = &m.target;
            let s = &m.source;
            let checked = crate::dedup::SegmentFiles::header(&files, t.place)
                .ok()
                .flatten()
                == Some((t.payload_len, t.raw_kind, t.hash))
                && crate::dedup::SegmentFiles::header(&files, s.place)
                    .ok()
                    .flatten()
                    == Some((s.payload_len, s.raw_kind, s.hash));
            if !checked || dedup::event(MapEvent::Punch(t.place)).is_err() {
                summary.failed += 1;
                continue;
            }
            match pio::punch_hole(file, t.physical_start(), u64::from(t.len)) {
                Ok(pio::Punch::Supported) => {}
                Ok(pio::Punch::Unsupported) => summary.unsupported += 1,
                Err(_) => summary.failed += 1,
            }
        }
        summary
    }
}

/// A frame's place, raw kind and hash, from its header.
type FrameFacts = (Location, u8, Hash);

/// The start of the log.
const ORIGIN_PLACE: Location = Location {
    segment: 0,
    offset: 0,
};

/// One mapping pass's candidates, before they are durable.
#[derive(Default)]
struct Pass {
    end: Location,
    maps: Vec<Mapping>,
    table: MapTable,
    full: bool,
}

impl Default for Location {
    fn default() -> Location {
        ORIGIN_PLACE
    }
}

/// The byte table of the last mapping pass on this thread (a test and bench hook): candidate
/// and mapped bytes by class (CHANGE, PLAN, SNAPSHOT), the candidate bytes trimmed away by the
/// alignment or the one-block minimum, and those of files with no supported alignment.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MapTable {
    pub candidate_bytes: [u64; 3],
    pub mapped_bytes: [u64; 3],
    pub trimmed_bytes: u64,
    pub unsupported_bytes: u64,
}

thread_local! {
    static LAST_TABLE: RefCell<MapTable> = RefCell::new(MapTable::default());
}

/// Test hook: the byte table of the last mapping pass on this thread.
#[doc(hidden)]
pub fn last_map_table() -> MapTable {
    LAST_TABLE.with(|t| *t.borrow())
}

/// The class names of [`MapTable`]'s rows, in order.
#[doc(hidden)]
pub const MAP_CLASSES: [Class; 3] = [Class::Change, Class::Plan, Class::Snapshot];

/// The notes of the mapped-span punches `s`, in their fixed order.
fn mapped_diagnostics(s: &crate::segment::PunchSummary) -> Vec<String> {
    let mut lines = vec![];
    if s.unsupported > 0 {
        lines.push(format!(
            "hole punching is not supported for {} mapped spans; they keep their disk space",
            s.unsupported
        ));
    }
    if s.failed > 0 {
        lines.push(format!(
            "hole punching failed for {} mapped spans; they keep their disk space",
            s.failed
        ));
    }
    lines
}

/// The pieces of `run` with each source byte resolved to retained bytes: a part of the source
/// range that a durable or staged mapping covers reads from that mapping's own source instead,
/// so no entry points through another. The pieces are split at those boundaries.
fn resolve(run: &Run, map: &SpanMap, staged: &[Mapping]) -> Vec<Run> {
    let place = run.source.place;
    let mut covering: Vec<Mapping> = map
        .mappings_of(&place)
        .iter()
        .copied()
        .chain(staged.iter().copied().filter(|m| m.target.place == place))
        .collect();
    covering.sort_by_key(|m| m.target.start);
    let (mut at, end) = (
        u64::from(run.source.start),
        u64::from(run.source.start) + u64::from(run.source.len),
    );
    let mut out = vec![];
    let mut piece = |from: u64, to: u64, source: crate::dedup::FrameSpan, src_at: u64| {
        if to <= from {
            return;
        }
        let mut p = *run;
        p.target.start = run.target.start + (from - u64::from(run.source.start)) as u32;
        p.target.len = (to - from) as u32;
        p.source = source;
        p.source.start = src_at as u32;
        p.source.len = (to - from) as u32;
        out.push(p);
    };
    for m in &covering {
        let (ms, me) = (
            u64::from(m.target.start),
            u64::from(m.target.start) + u64::from(m.target.len),
        );
        if me <= at || ms >= end {
            continue;
        }
        piece(at, ms.max(at), run.source, at);
        let (s, e) = (ms.max(at), me.min(end));
        piece(s, e, m.source, u64::from(m.source.start) + (s - ms));
        at = e;
    }
    piece(at, end, run.source, at);
    out
}
