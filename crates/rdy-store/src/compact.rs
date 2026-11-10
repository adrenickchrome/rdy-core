//! The log's byte report and compact (D-4YRQ4: "Compaction never moves a frame, never changes a
//! byte that rdy reads and never changes a hash."). One walk of the frame headers counts every
//! physical frame; the removed payloads are then punched, best effort. No frame is appended.
use crate::error::StoreError;
use crate::segment::{MAX_PUNCH_SPANS, punch_diagnostics};
use crate::store::batch_open;
use crate::{Store, codec, kinds, pio};
use serde::Serialize;
use std::cell::RefCell;
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LogBytes {
    pub log_bytes: u64,
    pub removed_bytes: u64,
    pub by_kind: Vec<KindBytes>,
}

/// What [`Store::compact`] found and did. `on_disk_bytes` is the segment files' allocation after
/// the call, and `punched_bytes` the allocation that the call freed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompactReport {
    pub log_bytes: u64,
    pub removed_bytes: u64,
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
                    let (prefix, n) = if raw & codec::VERSIONED == 0 {
                        ([0; 2], 0)
                    } else {
                        version_prefix(file, payload_at, len)?
                    };
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
        Ok(LogBytes {
            log_bytes: total,
            removed_bytes: removed,
            by_kind,
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

    /// Validates and counts the log, then tries to punch every removed payload, in ascending place
    /// order. With `dry_run` it only reports: no punch, sync, reindex or publish. It takes no
    /// approval and appends no frame. A punch outcome is a note, never an error, and so is a
    /// final allocation census that fails; everything before the first punch is an ordinary
    /// error.
    pub fn compact(&self, dry_run: bool) -> Result<CompactReport, CompactError> {
        self.ensure_writable()?;
        let LogBytes {
            log_bytes,
            removed_bytes,
            by_kind,
        } = self.log_bytes()?;
        let before = self.allocated()?;
        let mut report = CompactReport {
            log_bytes,
            removed_bytes,
            punched_bytes: 0,
            on_disk_bytes: before,
            by_kind,
            notes: vec![],
        };
        if dry_run {
            return Ok(report);
        }
        let summary = self.segment_log().punch_removed(MAX_PUNCH_SPANS);
        report.notes = punch_diagnostics(&summary);
        match self.allocated() {
            Ok(after) => {
                report.on_disk_bytes = after;
                report.punched_bytes = before.saturating_sub(after);
            }
            Err(_) => report.notes.push(CENSUS_FAILED.to_string()),
        }
        Ok(report)
    }
}
