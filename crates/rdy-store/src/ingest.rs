//! The log ingest of a load (plan Dump and load, D-WWAK7, D-D2NPN): it writes a dump's sealed
//! batches into a new store directory, each at the place the source log held it. So the new log
//! is the source log byte for byte, and every location inside a payload stays true: a ref's salt
//! (`RefUpdate.at`), an inbox cursor (`InboxCursor.at`) and a marker's start
//! (`BatchCommit.start`). The ingest writes no index: the first open of the store builds it from
//! the log.
//!
//! The new log never rolls by itself. It rolls only where the source rolled: before a batch that
//! starts at offset 0 of the next segment. So a held batch that passed the soft segment limit in
//! the source passes it here too.
//!
//! A dump of an older store format gets one more batch after its own: `upgrade` appends it, and
//! it sets `meta/store_format` to this rdy's store format (FW-UP1, in its smallest form). Until
//! M3a a store format change only adds kinds and refs (D-F0904), so no frame is re-encoded.
//! A log of a store format before 12 has no repo id: the upgrade batch gives it one (RULING F2).
//!
//! A converter for another store format must remap every location inside a payload
//! (`RefUpdate.at`, `InboxCursor.at`) if it changes the layout.
//!
//! A dropped frame (dump format 2, D-3M81W) is written as the source's erased frame is: its
//! original header, then zeros of its original length, at its original place. Its authorization
//! comes later in the stream than the frame, so the ingest keeps each dropped record and checks
//! them all in `finish`, against the complete log and before `FORMAT` is written: the store's
//! own authorization bootstrap must authorize the frame's exact span, by the apply and with the
//! residue the record names. A dangling or forged drop refuses the load, and no store is made.
//!
//! The zeros a dropped frame claims are bounded before they are written (ruling task-9, from
//! tb-A4DAV): one frame claims at most `MAX_BLOB_BYTES` (the reader refuses more, and `batch`
//! refuses more before its batch writes a byte), and the frames whose authorization has not been
//! validated claim at most `MAX_UNAUTHORIZED_BYTES` in all. A frame's bytes are released only
//! when the log's sealed batches validly authorize its exact span, as `finish` checks it: the
//! same validation of every apply, the same span, apply and residue. An apply's hash alone
//! releases nothing. The validation runs when a record would pass the bound, so a stream within
//! the bound pays nothing for it; the record that still passes the bound after it is refused
//! before it is written. The ingest's own bookkeeping grows fallibly: a refused allocation is
//! `record allocation failed`.
use crate::dump::{
    DROPPED_LEN, DumpBatch, Frame, drop_text, dropped_payload, parse_dropped, try_batch_digest,
};
use crate::error::StoreError;
use crate::format::{self, Format};
use crate::fsutil::sync_dir;
use crate::kinds::{self, RefUpdate};
use crate::meta::{REF_REPO_ID, encode_repo_id};
use crate::redaction_io;
use crate::segment::{Location, SegmentLog};
use crate::store::{MAX_BLOB_BYTES, encode};
use rdy_model::{Hash, RepoId};
use std::path::{Path, PathBuf};

/// The directory of the log inside a store directory.
const SEGMENTS_DIR: &str = "segments";

/// The most dropped frames one ingest holds for its final check (inclusive): the bound on
/// scanned frames. One more is refused with `record allocation failed`.
pub const MAX_PENDING_DROPS: usize = 1_000_000;

/// The most zero bytes the ingest writes for dropped frames whose authorization it has not
/// validated (inclusive): 1 GiB. One byte more is refused with `drop_text::BUDGET`.
pub const MAX_UNAUTHORIZED_BYTES: u64 = 1 << 30;

/// A dropped frame waiting for the final check: its 102 container bytes, its original location,
/// and whether a validated authorization of its exact span released its bytes from the bound.
struct PendingDrop {
    record: [u8; DROPPED_LEN],
    loc: Location,
    released: bool,
}

/// A new store directory that takes a dump's batches in log order.
pub struct Ingest {
    dir: PathBuf,
    log: SegmentLog,
    batches: u64,
    frames: u64,
    /// Whether an ingested frame set `meta/repo_id`.
    has_repo_id: bool,
    /// The dropped frames so far, in log order, at most `MAX_PENDING_DROPS`.
    pending: Vec<PendingDrop>,
    /// The zero bytes written for dropped frames not yet released, at most
    /// `MAX_UNAUTHORIZED_BYTES`.
    unauthorized: u64,
}

/// What an ingest wrote: the dump's batches and their data frames (an upgrade batch is not
/// counted), and the end of the new log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngestReport {
    pub batches: u64,
    pub frames: u64,
    pub end: Location,
}

fn dump_error(what: String) -> StoreError {
    StoreError::Dump { at: 0, what }
}

fn refused_allocation() -> StoreError {
    dump_error(drop_text::ALLOCATION.into())
}

impl Ingest {
    /// Creates the store directory `dir` with an empty log; `finish` writes its `FORMAT`. `dir`
    /// must not exist: an existing `dir` is `StoreError::Io` of kind `AlreadyExists`, and
    /// nothing is written into it.
    pub fn create(dir: &Path) -> Result<Ingest, StoreError> {
        std::fs::create_dir(dir)?;
        let parent = match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        sync_dir(parent)?;
        let segments = dir.join(SEGMENTS_DIR);
        std::fs::create_dir(&segments)?;
        sync_dir(dir)?;
        // the log never rolls by itself: `batch` rolls where the source rolled
        let log = SegmentLog::open(&segments, u64::MAX)?;
        Ok(Ingest {
            dir: dir.to_path_buf(),
            log,
            batches: 0,
            frames: 0,
            has_repo_id: false,
            pending: Vec::new(),
            unauthorized: 0,
        })
    }

    /// Appends batch `n` of the dump at its source place, then its marker, and notes whether the
    /// batch sets `meta/repo_id`. When the batch starts at offset 0 of the next segment and the
    /// current segment holds frames, the log rolls once first. The log's end must then equal the
    /// batch's start, so a batch at any other place is refused before the log rolls or takes a
    /// frame. A dropped frame is written as its original header and zeros of its original length,
    /// with its original hash, and kept for `finish` to check; more than `MAX_PENDING_DROPS` of
    /// them is `record allocation failed`. A dropped frame that claims more than
    /// `MAX_BLOB_BYTES` is refused before the batch writes anything. When a dropped frame's claim
    /// would take the bytes not yet released past `MAX_UNAUTHORIZED_BYTES`, the sealed batches'
    /// authorization is validated first (`release_authorized`); if it still would, the frame is
    /// refused before it is written (`drop_text::BUDGET`). The marker's digest must equal the
    /// batch's digest.
    /// Each failure is `StoreError::Dump`. Nothing is synced until `finish`.
    pub fn batch(&mut self, n: u64, b: &DumpBatch) -> Result<(), StoreError> {
        for f in &b.frames {
            if let Frame::Dropped { len, .. } = f
                && u64::from(*len) > MAX_BLOB_BYTES
            {
                return Err(dump_error(drop_text::claim(*len)));
            }
        }
        let end = self.log.end();
        if b.start.segment == end.segment.wrapping_add(1) && b.start.offset == 0 && end.offset > 0 {
            self.log.roll()?;
        }
        let end = self.log.end();
        if end != b.start {
            return Err(dump_error(format!(
                "batch {n} starts at segment {} offset {}, and the log ends at segment {} offset {}",
                b.start.segment, b.start.offset, end.segment, end.offset
            )));
        }
        if b.frames.is_empty() {
            return Err(dump_error(format!("batch {n} has no frames")));
        }
        let mut hashes: Vec<Hash> = Vec::new();
        hashes
            .try_reserve_exact(b.frames.len())
            .map_err(|_| refused_allocation())?;
        let mut sets_repo_id = false;
        for f in &b.frames {
            match f {
                Frame::Present { kind, payload, .. } => {
                    let (_, hash) = self.log.append_unrolled(*kind, payload)?;
                    hashes.push(hash);
                    sets_repo_id |= *kind == kinds::REF_UPDATE
                        && postcard::from_bytes::<RefUpdate>(payload)
                            .is_ok_and(|u| u.name == REF_REPO_ID);
                }
                Frame::Dropped {
                    kind,
                    hash,
                    len,
                    apply,
                    residue,
                } => {
                    let claim = |held: u64| {
                        held.checked_add(u64::from(*len))
                            .filter(|u| *u <= MAX_UNAUTHORIZED_BYTES)
                    };
                    if claim(self.unauthorized).is_none() {
                        self.release_authorized()?;
                    }
                    let unauthorized = claim(self.unauthorized)
                        .ok_or_else(|| dump_error(drop_text::BUDGET.into()))?;
                    if self.pending.len() >= MAX_PENDING_DROPS
                        || self.pending.try_reserve(1).is_err()
                    {
                        return Err(refused_allocation());
                    }
                    let loc = self.log.append_dropped(*kind, hash, *len)?;
                    self.pending.push(PendingDrop {
                        record: dropped_payload(*kind, hash, *len, apply, residue.as_ref()),
                        loc,
                        released: false,
                    });
                    self.unauthorized = unauthorized;
                    hashes.push(*hash);
                }
            }
        }
        let digest = try_batch_digest(hashes.iter()).ok_or_else(refused_allocation)?;
        if digest != b.digest {
            return Err(dump_error(format!("batch {n} does not match its digest")));
        }
        self.log
            .append_marker_digest(b.start, hashes.len() as u32, digest)?;
        self.has_repo_id |= sets_repo_id;
        self.batches += 1;
        self.frames += hashes.len() as u64;
        Ok(())
    }

    /// Releases from the bound the bytes of every dropped frame not yet released whose exact span
    /// the log's sealed batches validly authorize (`redaction_io::bootstrap_sealed`): an
    /// authorized span at its location with its original kind, hash and length, by the apply and
    /// with the residue its record names, which `check_drops` then accepts. When any apply of
    /// the sealed batches fails its validation, nothing is released (`finish` refuses such a log
    /// anyway). An I/O error stays one.
    fn release_authorized(&mut self) -> Result<(), StoreError> {
        let auth = match redaction_io::bootstrap_sealed(self.log.readers()) {
            Ok(auth) => auth,
            Err(StoreError::Io(e)) => return Err(StoreError::Io(e)),
            Err(_) => return Ok(()),
        };
        if !auth.is_active() {
            return Ok(());
        }
        for p in self.pending.iter_mut().filter(|p| !p.released) {
            let Ok(Frame::Dropped {
                kind,
                hash,
                len,
                apply,
                residue,
            }) = parse_dropped(&p.record)
            else {
                continue;
            };
            let (Some(hole), Some(span)) = (auth.holes.get(&p.loc), auth.spans.get(&p.loc)) else {
                continue;
            };
            if (hole.kind, hole.hash, hole.len) == (kind, hash, len)
                && span.apply == apply
                && span.residue == residue
            {
                p.released = true;
                self.unauthorized -= u64::from(len);
            }
        }
        Ok(())
    }

    /// Checks every dropped frame against the complete log, before the store is finished: the
    /// store's authorization bootstrap (`redaction_io::bootstrap`) must hold its location as an
    /// authorized span with its original kind, hash and length, from the first valid apply that
    /// authorizes it, which the record names, and with the residue the record names. Refusals
    /// (`StoreError::Dump`): a span the log's applies place elsewhere or with another header is
    /// `dropped frame does not match its authorized span`; no valid apply for it, or another
    /// apply than the record names, is `dropped frame has no valid authorization`; another
    /// residue is `dropped frame has an invalid residue`. An I/O error stays one.
    fn check_drops(&self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let refuse = |what: &str| Err(dump_error(what.into()));
        let auth = match redaction_io::bootstrap(self.log.readers()) {
            Ok(auth) => auth,
            Err(StoreError::Io(e)) => return Err(StoreError::Io(e)),
            Err(StoreError::RedactionCorrupt { segment, offset }) => {
                let at = Location { segment, offset };
                return refuse(if self.pending.iter().any(|p| p.loc == at) {
                    drop_text::SPAN
                } else {
                    drop_text::NO_AUTHORIZATION
                });
            }
            Err(_) => return refuse(drop_text::NO_AUTHORIZATION),
        };
        for p in &self.pending {
            let Frame::Dropped {
                kind,
                hash,
                len,
                apply,
                residue,
            } = parse_dropped(&p.record).map_err(|what| dump_error(what.into()))?
            else {
                return refuse(drop_text::NO_AUTHORIZATION);
            };
            let (Some(hole), Some(span)) = (auth.holes.get(&p.loc), auth.spans.get(&p.loc)) else {
                return refuse(drop_text::NO_AUTHORIZATION);
            };
            if (hole.kind, hole.hash, hole.len) != (kind, hash, len) {
                return refuse(drop_text::SPAN);
            }
            if span.apply != apply {
                return refuse(drop_text::NO_AUTHORIZATION);
            }
            if span.residue != residue {
                return refuse(drop_text::INVALID_RESIDUE);
            }
        }
        Ok(())
    }

    /// The end of the log so far.
    pub fn end(&self) -> Location {
        self.log.end()
    }

    /// The logged upgrade step (FW-UP1, D-WWAK7, D-F0904), in its smallest form. `from` is the
    /// store format of the dump's log. The cases (closed):
    /// - `from` is this rdy's store format: nothing is appended (such a log holds its id);
    /// - `from` is older: one batch is appended after the dump's batches. It holds the
    ///   `REF_UPDATE` that sets `meta/store_format` to this rdy's store format, salted with the
    ///   log end as every ref write is; then, when no ingested frame set `meta/repo_id`, the
    ///   `REF_UPDATE` of `meta/repo_id` with a new repo id, salted with the log end plus one
    ///   (RULING F2); then its marker. Every earlier byte is kept;
    /// - `from` is newer: refused with `StoreError::Dump`, and nothing is appended.
    ///
    /// Returns whether it appended the batch. Call it once, after the last `batch`.
    pub fn upgrade(&mut self, from: u32) -> Result<bool, StoreError> {
        let current = Format::CURRENT.store_format;
        if from > current {
            return Err(dump_error(format!(
                "store format {from} is newer than this rdy's store format {current}"
            )));
        }
        if from == current {
            return Ok(false);
        }
        let start = self.log.end();
        let mut refs = vec![(format::REF_STORE_FORMAT, format::encode_u32(current))];
        if !self.has_repo_id {
            refs.push((REF_REPO_ID, encode_repo_id(&RepoId::new())));
        }
        // the i-th ref of the batch is salted with the log end plus i, as the store salts
        let frames: Vec<(u8, Vec<u8>)> = refs
            .into_iter()
            .enumerate()
            .map(|(i, (name, value))| {
                let at = Location {
                    segment: start.segment,
                    offset: start.offset + i as u64,
                };
                let update = RefUpdate {
                    name: name.into(),
                    value,
                    at,
                };
                (kinds::REF_UPDATE, encode(&update))
            })
            .collect();
        let placed = self.log.append_frames(&frames, false)?;
        self.log
            .append_marker(start, placed.iter().map(|(_, h)| h))?;
        Ok(true)
    }

    /// Checks every dropped frame's authorization against the complete log (see `batch`): a
    /// refusal is `StoreError::Dump`, and no `FORMAT` is written, so nothing opens the directory.
    /// Then makes the new log durable: one sync of the log, then of `segments/`. Then writes
    /// `FORMAT` (this rdy's store format: `upgrade` made the log say it too) and syncs the store
    /// directory. Writes no index.
    pub fn finish(mut self) -> Result<IngestReport, StoreError> {
        self.check_drops()?;
        self.log.sync()?;
        sync_dir(&self.dir.join(SEGMENTS_DIR))?;
        format::write_format_file(&self.dir, Format::CURRENT)?;
        sync_dir(&self.dir)?;
        Ok(IngestReport {
            batches: self.batches,
            frames: self.frames,
            end: self.log.end(),
        })
    }
}
