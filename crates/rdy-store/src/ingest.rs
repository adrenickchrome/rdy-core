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
//! A dump of a scalar store format (dump formats 1 to 3) gets one more batch after its own:
//! `upgrade` appends it, and it sets `meta/store_features` to the baseline features
//! (`crate::format::baseline_features`; FW-UP1, in its smallest form). The historical
//! `meta/store_format` refs the dump holds stay as they are, and no frame is re-encoded
//! (D-F0904). A log of a store format before 12 has no repo id: the upgrade batch gives it one
//! (RULING F2). A dump of a feature store (dump format 4) gets no batch: its log holds its
//! manifest, which the load keeps, unknown compatible features included.
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
//! A holder's dump (its first batch sets `meta/store_mode`) is checked by the holder's own
//! bootstrap (`crate::redaction_import`): its applies by their logical tuples, its dropped frames
//! as the spans those tuples derive. The mode is the one the first batch sets, read before any
//! dropped frame is judged, so a load never reads a holder's applies as a project's.
//!
//! Every batch's refs meet the holder rules (`crate::holder`) before the batch writes a byte:
//! the first batch sets the mode, a mode ref in a later batch is refused, and a holder's refs
//! stay in its families. The refusal is the store's own (`StoreError::Decode`).
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
use crate::dump_chain::chain_text;
use crate::dump_scan::{FrameAction, ScanPlan};
use crate::error::StoreError;
use crate::format::{self, DumpStore, StoreFeatures};
use crate::fsutil::sync_dir;
use crate::holder::{FirstBatch, RefRules, StoreMode};
use crate::kinds::{self, RefUpdate};
use crate::meta::{REF_REPO_ID, encode_repo_id};
use crate::redaction_io;
use crate::segment::{HEADER, Location, SegmentLog};
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
    /// Whether an ingested frame is a `REDACTION_APPLY`: `finish` then checks the log's
    /// authorization even when no frame was dropped.
    has_apply: bool,
    /// The dropped frames so far, in log order, at most `MAX_PENDING_DROPS`.
    pending: Vec<PendingDrop>,
    /// The zero bytes written for dropped frames not yet released, at most
    /// `MAX_UNAUTHORIZED_BYTES`.
    unauthorized: u64,
    /// The ref rules of the log so far: None until the first batch set the mode.
    rules: Option<RefRules>,
    /// The mode the first batch set (a project until then).
    mode: StoreMode,
    /// Whether an ingested frame is a carried record (kind REDACTION_CARRIED): `finish` then
    /// checks the log's authorization even when no frame was dropped.
    has_carried: bool,
    /// Whether `append_carried` succeeded: the carried batch comes once.
    carried_done: bool,
    /// Whether `upgrade` ran: the carried batch comes before it.
    upgraded: bool,
    /// The value of the last `meta/store_features` ref an ingested frame set.
    features_ref: Option<Vec<u8>>,
    /// The features `upgrade` gave the store, which `finish` mirrors in `FORMAT`.
    features: Option<StoreFeatures>,
}

/// What an ingest wrote: the dump's batches and their data frames (an upgrade batch is not
/// counted), and the end of the new log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngestReport {
    pub batches: u64,
    pub frames: u64,
    pub end: Location,
}

/// Whether `named`, the record a dropped frame at `loc` names, is a validated carried record of
/// `auth` whose `held: false` span at `loc` has exactly the frame's `identity` (kind, hash,
/// length, residue). Such a record was the span's proof when the dump that holds the frame was
/// written; a later apply that covers the span is its proof now (D-0VG72: "an apply that covers
/// a held:false span becomes that span's proof"), and the record stays as history.
fn names_carried(
    auth: &redaction_io::Authorization,
    named: &Hash,
    loc: Location,
    identity: (u8, Hash, u32, Option<Hash>),
) -> bool {
    auth.carried.iter().filter(|c| c.hash == *named).any(|c| {
        c.record.spans.iter().any(|s| {
            !s.held
                && (s.place.segment, s.place.offset) == (loc.segment, loc.offset)
                && (s.kind, s.hash, s.len, s.residue) == identity
        })
    })
}

fn dump_error(what: String) -> StoreError {
    StoreError::Dump { at: 0, what }
}

fn refused_allocation() -> StoreError {
    dump_error(drop_text::ALLOCATION.into())
}

/// The refusal of a record over the record bound (`16777216 bytes per record`).
fn record_too_big() -> StoreError {
    StoreError::RedactionLimit {
        limit: crate::redaction::limit::RECORD,
    }
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
            has_apply: false,
            pending: Vec::new(),
            unauthorized: 0,
            rules: None,
            mode: StoreMode::Project,
            has_carried: false,
            carried_done: false,
            upgraded: false,
            features_ref: None,
            features: None,
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
    /// Each failure is `StoreError::Dump`, but a ref the holder rules refuse is the store's own
    /// `StoreError::Decode`, before the batch writes a byte. Nothing is synced until `finish`.
    pub fn batch(&mut self, n: u64, b: &DumpBatch) -> Result<(), StoreError> {
        self.take_batch(n, b, None)
    }

    /// [`Ingest::batch`] of a chain load, by the verified plan of its pre-pass
    /// (`crate::dump_scan`): for each original physical frame of `b`, `plan` decides what is
    /// written at its place ([`ScanPlan::frame_action`]). A `Keep` frame is written as `batch`
    /// writes it: its payload, or its validated hole. A `Zero` frame is written directly as its
    /// original 37-byte header and zeros of its original length, through the dropped-frame
    /// writer (`ERASE_BUFFER` bytes at a time): a present payload is checked against its hash
    /// and never written, not even for a moment. Its authority is the plan's (the retained apply,
    /// or the carried record the load appends later), and it waits for `finish` as any dropped
    /// frame does, within the same bounds: the frame counts as a pending drop and its length as
    /// zeros not yet validated. The batch's frame hashes, digest, marker and places are the
    /// source's. A frame the plan names with another identity is refused before the batch writes
    /// a byte (`dropped frame does not match its authorized span`).
    pub fn batch_with_plan(
        &mut self,
        n: u64,
        b: &DumpBatch,
        plan: &ScanPlan,
    ) -> Result<(), StoreError> {
        self.take_batch(n, b, Some(plan))
    }

    /// `batch`, with each frame's action from `plan` when there is one (`batch_with_plan`).
    fn take_batch(
        &mut self,
        n: u64,
        b: &DumpBatch,
        plan: Option<&ScanPlan>,
    ) -> Result<(), StoreError> {
        for f in &b.frames {
            if let Frame::Dropped { len, .. } = f
                && u64::from(*len) > MAX_BLOB_BYTES
            {
                return Err(dump_error(drop_text::claim(*len)));
            }
        }
        // every frame's action before the first byte, so a refused batch writes nothing
        let mut actions: Vec<FrameAction> = Vec::new();
        if let Some(plan) = plan {
            actions
                .try_reserve_exact(b.frames.len())
                .map_err(|_| refused_allocation())?;
            let mut place = b.start;
            for f in &b.frames {
                let action = plan.frame_action(place, f.kind(), *f.hash(), f.original_len())?;
                if let (FrameAction::Zero { .. }, Frame::Present { hash, payload, .. }) =
                    (action, f)
                    && Hash::of(payload) != *hash
                {
                    return Err(dump_error(format!("batch {n} does not match its digest")));
                }
                actions.push(action);
                place.offset = place
                    .offset
                    .checked_add(HEADER)
                    .and_then(|o| o.checked_add(u64::from(f.original_len())))
                    .ok_or_else(|| dump_error(chain_text::OVERFLOW.into()))?;
            }
        }
        self.check_refs(b)?;
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
        for (i, f) in b.frames.iter().enumerate() {
            // a frame the plan zeroes is written as a dropped frame with the plan's authority
            let zeroed = match actions.get(i) {
                Some(FrameAction::Zero { authority, residue }) => Some(Frame::Dropped {
                    kind: f.kind(),
                    hash: *f.hash(),
                    len: f.original_len(),
                    apply: *authority,
                    residue: *residue,
                }),
                _ => None,
            };
            match zeroed.as_ref().unwrap_or(f) {
                Frame::Present { kind, payload, .. } => {
                    let (_, hash) = self.log.append_unrolled(*kind, payload)?;
                    hashes.push(hash);
                    self.has_apply |= *kind == kinds::REDACTION_APPLY;
                    self.has_carried |= *kind == kinds::REDACTION_CARRIED;
                    if *kind == kinds::REF_UPDATE
                        && let Ok(u) = postcard::from_bytes::<RefUpdate>(payload)
                    {
                        sets_repo_id |= u.name == REF_REPO_ID;
                        if u.name == format::REF_STORE_FEATURES {
                            self.features_ref = Some(u.value);
                        }
                    }
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
                    // the header keeps the raw kind; the pending record names the logical kind,
                    // as the authorization's holes do (`crate::codec`)
                    let loc = self.log.append_dropped(*kind, hash, *len)?;
                    let logical = crate::codec::logical_kind(*kind);
                    self.pending.push(PendingDrop {
                        record: dropped_payload(logical, hash, *len, apply, residue.as_ref()),
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
        if let Some(r) = &mut self.rules {
            r.commit_batch();
        }
        self.batches += 1;
        self.frames += hashes.len() as u64;
        Ok(())
    }

    /// Judges batch `b`'s refs by the holder rules (`crate::holder::RefRules`) before it is
    /// written: the first batch's mode refs set the mode, then each present ref of `b` is judged,
    /// then its end, which needs the TAG object of each of its answer refs in it or in an
    /// earlier batch. A dropped frame is never a ref or a tag (only blocks, changes and chunks
    /// are removable). `b`'s tags count for later batches only once `batch` takes it
    /// (`commit_batch`).
    fn check_refs(&mut self, b: &DumpBatch) -> Result<(), StoreError> {
        let first = self.batches == 0;
        if first {
            self.rules = None;
        }
        let refs = || {
            b.frames.iter().filter_map(|f| match f {
                Frame::Present {
                    kind: kinds::REF_UPDATE,
                    payload,
                    ..
                } => Some(&payload[..]),
                _ => None,
            })
        };
        let rules = match &mut self.rules {
            Some(r) => r,
            None => {
                let mut scan = FirstBatch::default();
                for p in refs() {
                    scan.frame(kinds::REF_UPDATE, Some(p), b.start)?;
                }
                let creation = scan.creation(b.start);
                self.mode = creation.mode;
                self.rules.insert(RefRules::new(creation))
            }
        };
        // a batch refused earlier left its refs: they never count
        rules.abandon_batch();
        // the batch's TAG objects, which its answer refs may name
        for f in &b.frames {
            if let Frame::Present {
                kind: kinds::TAG,
                payload,
                ..
            } = f
            {
                rules.on_tag(payload);
            }
        }
        let judged = refs()
            .try_for_each(|p| rules.on_ref_frame(first, p))
            .and_then(|()| rules.check_marker());
        if judged.is_err() {
            rules.abandon_batch();
        }
        judged
    }

    /// Releases from the bound the bytes of every dropped frame not yet released whose exact span
    /// the log's sealed batches validly authorize (`redaction_io::bootstrap_sealed`): an
    /// authorized span at its location with its original kind, hash and length, by the apply and
    /// with the residue its record names, which `check_drops` then accepts. When any apply of
    /// the sealed batches fails its validation, nothing is released (`finish` refuses such a log
    /// anyway). An I/O error stays one.
    fn release_authorized(&mut self) -> Result<(), StoreError> {
        let auth = match self.bootstrap(true) {
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
                && (span.apply == apply
                    || names_carried(&auth, &apply, p.loc, (kind, hash, len, residue)))
                && span.residue == residue
            {
                p.released = true;
                self.unauthorized -= u64::from(len);
            }
        }
        Ok(())
    }

    /// Checks the log's redaction applies and every dropped frame against the complete log,
    /// before the store is finished. It runs when a frame was dropped or an apply was ingested:
    /// the store's authorization bootstrap with its semantic pass
    /// (`redaction_io::bootstrap`: a request selects exactly what its manifest removes, every
    /// physical copy of selected content is a span, and every intact span is what its residue
    /// records) must accept the log, and must hold each dropped frame's location as an
    /// authorized span with its original kind, hash and length, from the first valid apply that
    /// authorizes it, which the record names (or a validated carried record that names that
    /// exact span `held: false`, the span's proof when its dump was written: `names_carried`),
    /// and with the residue the record names. Refusals
    /// (`StoreError::Dump`): a span the log's applies place elsewhere or with another header is
    /// `dropped frame does not match its authorized span`; no valid apply for it, or another
    /// apply than the record names, is `dropped frame has no valid authorization`; another
    /// residue is `dropped frame has an invalid residue`. An I/O error stays one.
    fn check_drops(&self) -> Result<(), StoreError> {
        if self.pending.is_empty() && !self.has_apply && !self.has_carried {
            return Ok(());
        }
        let refuse = |what: &str| Err(dump_error(what.into()));
        let auth = match self.bootstrap(false) {
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
            if span.apply != apply
                && !names_carried(&auth, &apply, p.loc, (kind, hash, len, residue))
            {
                return refuse(drop_text::NO_AUTHORIZATION);
            }
            if span.residue != residue {
                return refuse(drop_text::INVALID_RESIDUE);
            }
        }
        Ok(())
    }

    /// The log's authorization by the bootstrap of its mode: a holder's derives its spans from
    /// its approvals (`redaction_import::bootstrap_holder`, which reads the sealed batches only);
    /// a project's is `redaction_io::bootstrap`, or `bootstrap_sealed` when `sealed`.
    fn bootstrap(&self, sealed: bool) -> Result<redaction_io::Authorization, StoreError> {
        match (self.mode, sealed) {
            (StoreMode::Holder, _) => crate::redaction_import::bootstrap_holder(self.log.readers()),
            (StoreMode::Project, true) => redaction_io::bootstrap_sealed(self.log.readers()),
            (StoreMode::Project, false) => redaction_io::bootstrap(self.log.readers()),
        }
    }

    /// The end of the log so far.
    pub fn end(&self) -> Location {
        self.log.end()
    }

    /// The logged upgrade step (FW-UP1, D-WWAK7, D-F0904), in its smallest form. `from` is the
    /// store of the dump's log. The cases (closed):
    /// - a feature store: nothing is appended, and the store keeps the manifest its log holds,
    ///   which must be `from`'s (`dump metadata does not match its header` otherwise); a
    ///   manifest this rdy may not write is refused;
    /// - a scalar store format up to 18: one batch is appended after the dump's batches. It holds
    ///   the `REF_UPDATE` that sets `meta/store_features` to the baseline features, salted with
    ///   the log end as every ref write is; then, when no ingested frame set `meta/repo_id`, the
    ///   `REF_UPDATE` of `meta/repo_id` with a new repo id, salted with the log end plus one
    ///   (RULING F2); then its marker. Every earlier byte is kept, the historical
    ///   `meta/store_format` refs among them;
    /// - a later scalar format: refused with `StoreError::Dump`, and nothing is appended.
    ///
    /// Returns whether it appended the batch. Call it once, after the last `batch` (and after
    /// `append_carried`, if it runs).
    pub fn upgrade(&mut self, from: &DumpStore) -> Result<bool, StoreError> {
        self.upgrade_with(from, None)
    }

    /// [`Ingest::upgrade`] of a chain prefix: when no ingested frame set `meta/repo_id`, the
    /// upgrade batch sets `repo`, the id the chain's headers name, not a new one. A prefix
    /// that stops before the source's own upgrade batch is the same repo as the chain.
    pub fn upgrade_in_repo(&mut self, from: &DumpStore, repo: RepoId) -> Result<bool, StoreError> {
        self.upgrade_with(from, Some(repo))
    }

    fn upgrade_with(&mut self, from: &DumpStore, repo: Option<RepoId>) -> Result<bool, StoreError> {
        self.upgraded = true;
        let from = match from {
            DumpStore::Features(m) => {
                format::check_writable(m)?;
                if self
                    .features_ref
                    .as_ref()
                    .is_some_and(|v| *v != m.to_json())
                {
                    return Err(dump_error(crate::dump_scan::METADATA_MISMATCH.into()));
                }
                self.features = Some(m.clone());
                return Ok(false);
            }
            DumpStore::Legacy(n) => *n,
        };
        if from > format::LAST_SCALAR_STORE_FORMAT {
            return Err(dump_error(format!(
                "store format {from} is newer than the last scalar store format {}",
                format::LAST_SCALAR_STORE_FORMAT
            )));
        }
        let features = format::baseline_features();
        let start = self.log.end();
        let mut refs = vec![(format::REF_STORE_FEATURES, features.to_json())];
        if !self.has_repo_id {
            refs.push((
                REF_REPO_ID,
                encode_repo_id(&repo.unwrap_or_else(RepoId::new)),
            ));
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
        self.features = Some(features);
        Ok(true)
    }

    /// Appends the carried records of a stopped load as one sealed batch after the source prefix
    /// (D-QF2HQ: a carried record explains exact zero bytes and never authorizes an erasure).
    /// It runs once, before `upgrade` and `finish`: a call after one that succeeded, or after
    /// `upgrade`, is `StoreError::Dump`; a refused call writes nothing and counts for nothing.
    /// The batch holds the residues of `residues` that the log does not hold yet, once each and
    /// sorted by residue hash, then `records` in the caller's order (its validated source-apply
    /// order). A residue is framed in the encoding whose frame hash a record names (a residue
    /// written before content-2 keeps encoding 0), else in its kind's latest encoding. Before a
    /// byte is written:
    ///
    /// - each record keeps its bounds (`redaction_carried::check_bounds`: more than 100,000
    ///   spans is `RedactionLimit` `100000 carried spans`, more than 16,777,216 encoded bytes
    ///   `RedactionLimit` `16777216 bytes per record`), and so does each residue;
    /// - the log as it would be, this batch included, passes the bootstrap of its mode with its
    ///   carried records (`redaction_carried::overlay`): every span checked against the retained
    ///   prefix, every `held: false` payload all zero. A failure is that bootstrap's refusal
    ///   (`RedactionCorrupt`).
    ///
    /// An empty `records` writes nothing. The batch counts in no source batch or frame total.
    pub fn append_carried(
        &mut self,
        residues: &[rdy_model::redaction::RedactionResidue],
        records: &[rdy_model::RedactionCarried],
    ) -> Result<(), StoreError> {
        if self.carried_done || self.upgraded {
            return Err(dump_error(
                "carried records are appended once, after the source batches and before the upgrade"
                    .into(),
            ));
        }
        if records.is_empty() {
            self.carried_done = true;
            return Ok(());
        }
        let mut encoded: Vec<Vec<u8>> = Vec::new();
        encoded
            .try_reserve_exact(records.len())
            .map_err(|_| refused_allocation())?;
        for r in records {
            encoded.push(crate::redaction_carried::check_bounds(r)?);
        }
        let mut c = redaction_io::collect(self.log.readers())?;
        // the residues the log does not hold yet, once each, by residue hash, each within the
        // record bound
        let mut missing: std::collections::BTreeMap<Hash, (u8, Vec<u8>)> = Default::default();
        if !residues.is_empty() {
            // the residue hashes the records name: each names its residue's source frame, whose
            // hash covers the encoding that frame records (`crate::codec`)
            let named: std::collections::HashSet<Hash> = records
                .iter()
                .flat_map(|r| r.spans.iter().filter_map(|s| s.residue))
                .collect();
            let latest = crate::codec::latest(kinds::REDACTION_RESIDUE);
            let mut buf = rdy_model::redaction::record_buffer().map_err(|_| record_too_big())?;
            for r in residues {
                let bytes =
                    rdy_model::redaction::encode_into(r, &mut buf).map_err(|_| record_too_big())?;
                // the residue's frame in the encoding whose hash a record names (an old
                // residue keeps encoding 0, in its frozen shape), else in its kind's latest
                // encoding
                let frame_in = |v: u16| {
                    let body = if v == latest {
                        Some(bytes.to_vec())
                    } else {
                        crate::codec::residue_body(r, v)
                    };
                    body.map(|b| {
                        crate::codec::frame(kinds::REDACTION_RESIDUE, v, b)
                            .expect("a registered encoding of a residue frames")
                    })
                };
                let frame = (0..latest)
                    .filter_map(frame_in)
                    .find(|f| named.contains(&Hash::of(&f.1)))
                    .or_else(|| frame_in(latest))
                    .expect("the latest encoding holds every residue");
                let h = Hash::of(&frame.1);
                if !c.residue_at.contains_key(&h) && !missing.contains_key(&h) {
                    missing.insert(h, frame);
                }
            }
        }
        let mut frames: Vec<(u8, Vec<u8>)> = missing.into_values().collect();
        frames.extend(
            encoded
                .into_iter()
                .map(|b| crate::codec::frame_latest(kinds::REDACTION_CARRIED, b)),
        );
        let start = self.log.end();
        let after = c.push_batch(start, &frames)?;
        match self.mode {
            StoreMode::Holder => {
                crate::redaction_import::holder_of(&c, self.log.readers(), start, after)?;
            }
            StoreMode::Project => {
                redaction_io::sealed_of(&c, self.log.readers(), after)?;
            }
        }
        let placed = self.log.append_frames(&frames, false)?;
        self.log
            .append_marker(start, placed.iter().map(|(_, h)| h))?;
        self.has_carried = true;
        self.carried_done = true;
        Ok(())
    }

    /// Checks every dropped frame's authorization against the complete log (see `batch`): a
    /// refusal is `StoreError::Dump`, and no `FORMAT` is written, so nothing opens the directory.
    /// Then makes the new log durable: one sync of the log, then of `segments/`. Then writes
    /// `FORMAT`, the mirror of the features the log holds (`upgrade`'s; without `upgrade`, the
    /// last features ref ingested, else the baseline), and syncs the store directory. Writes no
    /// index.
    pub fn finish(mut self) -> Result<IngestReport, StoreError> {
        self.check_drops()?;
        self.log.sync()?;
        sync_dir(&self.dir.join(SEGMENTS_DIR))?;
        let features = match self.features.take() {
            Some(f) => f,
            None => match &self.features_ref {
                Some(v) => format::decode_features(v)?,
                None => format::baseline_features(),
            },
        };
        format::write_format_file(&self.dir, &features)?;
        sync_dir(&self.dir)?;
        Ok(IngestReport {
            batches: self.batches,
            frames: self.frames,
            end: self.log.end(),
        })
    }
}
