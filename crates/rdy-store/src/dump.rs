//! The dump stream (plan Dump and load, D-D2NPN, D-WWAK7): one whole project as one file. It is
//! the long-lived exchange format. From its first release, every later rdy reads every dump
//! format that rdy has released (D-69AY3). The stream copies the log's sealed batches in log
//! order, byte for byte. The log is in dependency order already, because a frame can only name
//! objects written before it.
//!
//! # Dump format 1
//!
//! Integers are little endian.
//!
//! - **Prelude:** the 8 bytes `RDYDUMP\n`, then `dump_format: u32` (1).
//! - **Records:** each is framed as a store frame: `len: u32`, `kind: u8`, `hash: [u8; 32]`
//!   (`Hash::of(payload)`), `payload`. Record kinds of the container: `HEADER = 0xF0`,
//!   `FILE = 0xF1`, `BATCH = 0xF2`, `TRAILER = 0xFF`. The frames of a batch keep their store
//!   kinds: exactly the kinds that the header's store format assigns (`frame_kinds`; for the pair
//!   (1, 10): 1 to 9, 13, and 15 to 19; for (1, 11) and (1, 12): 1 to 10, 13, and 15 to 19).
//!   Never 14 (the batch marker), never an unassigned kind.
//! - **Order (closed):** one `HEADER`; zero or more `FILE`; zero or more `BATCH`, each followed
//!   by exactly its `frames` store frames; one `TRAILER`; the end of the file.
//! - **`HEADER` payload:** `store_format: u32`, `log_end.segment: u32`, `log_end.offset: u64`,
//!   `rdy_len: u16`, `rdy: [u8; rdy_len]` (UTF-8, at most 256 bytes; the writer's version, for
//!   people only, never checked).
//! - **`FILE` payload:** `path_len: u16`, `path: [u8; path_len]` (UTF-8), then the file's bytes.
//!   `path` is one of the closed set: `.rdy.yaml`; `.rdyignore`; `.rdy/packs/<name>` where
//!   `<name>` uses only `A-Z a-z 0-9 . _ -`, does not start with `.`, and ends in `.yaml` or
//!   `.yml`. At most 256 `FILE` records; each file at most 16 MiB; paths strictly ascending, no
//!   repeats.
//! - **`BATCH` payload:** `start.segment: u32`, `start.offset: u64`, `frames: u32`,
//!   `digest: [u8; 32]` (the source batch's `BatchCommit`), then `kinds: [u8; 32]`: `Hash::of`
//!   over `kind: u8 || hash: [u8; 32]` of each of the batch's frames, in order. `frames` is at
//!   least 1. The next `frames` records are that batch's data frames, byte for byte as the log
//!   holds them.
//! - **`TRAILER` payload:** `batches: u64`, `frames: u64`, `files: u32`, `digest: [u8; 32]`:
//!   `Hash::of` over `kind: u8 || len: u64 || hash: [u8; 32]` (`len` the payload's length) of
//!   every record from `HEADER` up to the last record before the trailer, store frames included,
//!   in order.
//!
//! A record's hash covers its payload only, so the `BATCH` record's `kinds` and the trailer's
//! digest bind every record's kind (and the trailer every length and the order): a flipped kind
//! byte fails them.
//!
//! A reader checks every record's hash, the order above, each frame's kind, each batch's frame
//! count and both its digests, and the trailer's counts and digest. Any failure is
//! `StoreError::Dump`: rdy never repairs a dump.
//!
//! # Dump format 2
//!
//! Dump format 2 carries store format 13 (the pair (2, 13); its frame kinds are those of (1, 12)
//! and 20 to 22), store format 14 (the pair (2, 14); its frame kinds are those of (2, 13) and
//! 11, the signature) and store format 15 (the pair (2, 15); store format 15 adds refs only, so
//! its frame kinds are those of (2, 14)). It keeps the magic, the container framing and the `HEADER`, `FILE`,
//! `BATCH` and `TRAILER` layouts of format 1, and adds one container kind, `DROPPED = 0xF3`
//! (D-3M81W: rdy "drops their bytes and keeps their hashes"). A `DROPPED` record stands only where
//! a batch's data frame is expected, and only under dump format 2. It is one source frame whose
//! payload an applied redaction removed. Its payload is exactly 102 bytes:
//!
//! `original_kind: u8 || original_hash: [u8; 32] || original_len: u32 || apply_hash: [u8; 32] ||
//! has_residue: u8 || residue_hash: [u8; 32]`.
//!
//! - `original_kind` is BLOCK, CHANGE or BLOB_CHUNK; no other kind is removable.
//! - `has_residue` is 0 or 1. With 0, `residue_hash` is 32 zero bytes; with 1, it names the
//!   residue of the frame. BLOCK and CHANGE have a residue; BLOB_CHUNK has none.
//! - `original_len` is at most `MAX_BLOB_BYTES` (64 MiB): no stored payload of any kind is
//!   longer. A longer claim is refused when the record is read, before anything is written.
//! - The record's own hash covers its 102 bytes, as for any record.
//! - The frame's place in the source log is implicit: the batch's start plus the original sizes
//!   (`37 + original_len`) of the frames before it. A dropped record counts as one source frame.
//! - The `BATCH` record's `digest` and `kinds` use the original `(kind, hash)`. The trailer's
//!   digest uses the container record as it is: `(0xF3, 102, record hash)`.
//!
//! The apply that authorizes a dropped frame comes later in the stream than the frame. So the
//! reader checks only the record's own rules; the ingest checks the authorization against the
//! complete stream before it finishes the store (`crate::ingest`).
//!
//! # Dump format 3
//!
//! Dump format 3 carries store format 16 (the pair (3, 16); its frame kinds are those of (2, 15)
//! and 23, the carried removal). It keeps the magic, the container framing and the `FILE`,
//! `BATCH` and `DROPPED` layouts of format 2. Its `HEADER` and `TRAILER` add the chain facts
//! (`crate::dump_chain`): a dump is either full, the whole log from its origin, or an increment,
//! the sealed batches after the end of the file before it.
//!
//! - **`HEADER` payload:** `kind: u8` (0 full, 1 increment; 2 to 255 do not parse),
//!   `store_format: u32`, `repo_id: [u8; 16]`, `base.segment: u32`, `base.offset: u64`,
//!   `base_batch: marker`, `prev: [u8; 32]`, `end.segment: u32`, `end.offset: u64`,
//!   `end_batch: marker`, `rdy_len: u16`, `rdy: [u8; rdy_len]` (UTF-8, at most 256 bytes). A
//!   marker is `start.segment: u32`, `start.offset: u64`, `frames: u32`, `digest: [u8; 32]` (48
//!   bytes). The fixed part is 175 bytes.
//! - **`TRAILER` payload (69 bytes):** `batches: u64`, `frames: u64`, `files: u32`,
//!   `removals: u32`, `has_first_removed: u8`, `first_removed.segment: u32`,
//!   `first_removed.offset: u64`, `digest: [u8; 32]` (the digest of format 1). The flag is 0 or
//!   1; with 0 the position is zero. `removals` counts the dump's `REDACTION_APPLY` frames;
//!   `first_removed` is the lowest local span those applies removed, absent when none has one.
//!   A dropped frame names its authority in its `DROPPED` record: a span of an apply of this
//!   file counts, a hole only a carried record explains does not ([`RemovalTally`]).
//!
//! A full dump has base `0.0`, a zero base marker and a zero `prev`. An increment holds at least
//! one batch. The first batch starts at `base`, or at offset zero of the next segment. The last
//! batch ends at the header's end (its start, plus `37 + original_len` per frame, plus the marker
//! frame), and its marker is `end_batch`; a dump of no batches ends at its base with its base
//! marker. The reader also checks `removals` against the dump's applies, and `first_removed`
//! against the lowest dropped frame whose authority is one of those applies; an increment may
//! instead name a span before its base, which an earlier file holds. Every place is checked
//! arithmetic: one past `u64::MAX` is `dump position overflows`. The chain links between files
//! are checked by the loader of a chain.
//!
//! A record payload holds at most `MAX_BLOB_BYTES` (64 MiB): a longer one is refused by its
//! length, before it is read or written. A carried removal record (kind 23) is decoded by the
//! bounded record decoder (`crate::bounded`) when it is read or written, and refused with that
//! decoder's error; the released kinds keep their own limits.
//!
//! The reader allocates only by fallible growth: a payload in chunks of at most `ERASE_BUFFER`
//! bytes, each frame slot, the trailer chain, both batch digests, the removal tally and each
//! project file. A refused allocation is the dump refusal `record allocation failed`, never an
//! abort.
use crate::dump_chain::{self, BatchMarker, DumpChainHeader, DumpKind, chain_text, follows};
use crate::error::StoreError;
use crate::kinds::{self, BatchCommit};
use crate::redaction::StoredFrame;
use crate::redaction_io::ERASE_BUFFER;
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::{MAX_BLOB_BYTES, StoreReader};
use rdy_model::{Hash, RepoId};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// The 8 bytes every dump starts with.
pub const MAGIC: &[u8; 8] = b"RDYDUMP\n";
/// The dump format this rdy writes: 3, of store format 16 (the pair (3, 16)).
pub const DUMP_FORMAT: u32 = 3;
/// The `(dump_format, store_format)` pairs this rdy reads. A pair, once released, never leaves
/// this list (D-69AY3). No (1, 13) pair exists: store formats 13 to 15 travel only in dump
/// format 2, and store format 16 only in dump format 3.
pub const READS: &[(u32, u32)] = &[
    (1, 10),
    (1, 11),
    (1, 12),
    (2, 13),
    (2, 14),
    (2, 15),
    (3, 16),
];
/// At most this many `FILE` records.
pub const MAX_FILES: u32 = 256;
/// At most this many bytes in one project file.
pub const MAX_FILE_BYTES: usize = 16 << 20;
/// At most this many bytes in the header's `rdy` text.
const MAX_RDY_BYTES: usize = 256;

/// The record kinds of the container. Store frames keep their store kinds.
pub mod rec {
    pub const HEADER: u8 = 0xF0;
    pub const FILE: u8 = 0xF1;
    pub const BATCH: u8 = 0xF2;
    /// A data frame whose payload a redaction removed (dump format 2 only).
    pub const DROPPED: u8 = 0xF3;
    pub const TRAILER: u8 = 0xFF;
}

const BATCH_LEN: usize = 4 + 8 + 4 + 32 + 32;
/// The length of a format-1 or format-2 `TRAILER` payload.
const LEGACY_TRAILER_LEN: usize = 8 + 8 + 4 + 32;
/// The length of a format-3 `TRAILER` payload.
pub const TRAILER_LEN: usize = 8 + 8 + 4 + 4 + 1 + 4 + 8 + 32;
/// The length of one marker in a format-3 `HEADER`.
const MARKER_LEN: usize = 4 + 8 + 4 + 32;
/// The length of a format-3 `HEADER` payload before its `rdy` text.
pub const HEADER_FIXED: usize = 1 + 4 + 16 + 4 + 8 + MARKER_LEN + 32 + 4 + 8 + MARKER_LEN + 2;
/// The length of a format-1 or format-2 `HEADER` payload before its `rdy` text.
const LEGACY_HEADER_FIXED: usize = 4 + 4 + 8 + 2;
/// The length of a `DROPPED` record's payload.
pub const DROPPED_LEN: usize = 1 + 32 + 4 + 32 + 1 + 32;

/// One store frame of a batch, as the log holds it: `Present` with its payload, or `Dropped`, a
/// frame whose payload an applied redaction removed. `Dropped` keeps the frame's original kind,
/// hash and payload length, the apply record that authorized the removal and the residue that
/// replaces it for reads (none for a blob chunk). An in-memory form only: no stored kind changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Present {
        kind: u8,
        hash: Hash,
        payload: Vec<u8>,
    },
    Dropped {
        kind: u8,
        hash: Hash,
        len: u32,
        apply: Hash,
        residue: Option<Hash>,
    },
}

impl Frame {
    /// The frame's store kind (the original kind of a dropped frame).
    pub fn kind(&self) -> u8 {
        match self {
            Frame::Present { kind, .. } | Frame::Dropped { kind, .. } => *kind,
        }
    }

    /// The frame's hash: of its payload, or the original hash of a dropped frame.
    pub fn hash(&self) -> &Hash {
        match self {
            Frame::Present { hash, .. } | Frame::Dropped { hash, .. } => hash,
        }
    }

    /// The length of the frame's payload in the source log: the payload's, or a dropped frame's
    /// original length. A present payload over `u32::MAX` bytes cannot be in a log; it reads as
    /// `u32::MAX`.
    pub fn original_len(&self) -> u32 {
        match self {
            Frame::Present { payload, .. } => u32::try_from(payload.len()).unwrap_or(u32::MAX),
            Frame::Dropped { len, .. } => *len,
        }
    }

    /// The payload of a present frame; None for a dropped frame, whose bytes are gone.
    pub fn present_payload(&self) -> Option<&[u8]> {
        match self {
            Frame::Present { payload, .. } => Some(payload),
            Frame::Dropped { .. } => None,
        }
    }
}

/// The 102 bytes of the `DROPPED` record of a dropped frame.
pub(crate) fn dropped_payload(
    kind: u8,
    hash: &Hash,
    len: u32,
    apply: &Hash,
    residue: Option<&Hash>,
) -> [u8; DROPPED_LEN] {
    let mut p = [0u8; DROPPED_LEN];
    p[0] = kind;
    p[1..33].copy_from_slice(&hash.0);
    p[33..37].copy_from_slice(&len.to_le_bytes());
    p[37..69].copy_from_slice(&apply.0);
    if let Some(r) = residue {
        p[69] = 1;
        p[70..102].copy_from_slice(&r.0);
    }
    p
}

/// A dump refusal of a dropped frame (the `what` of `StoreError::Dump`, closed).
pub(crate) mod drop_text {
    pub const NOT_ALLOWED: &str = "dropped frame is not allowed in this dump format";
    pub const WRONG_LENGTH: &str = "dropped frame has the wrong length";
    pub const INVALID_KIND: &str = "dropped frame has an invalid kind";
    pub const INVALID_RESIDUE: &str = "dropped frame has an invalid residue";
    pub const NO_AUTHORIZATION: &str = "dropped frame has no valid authorization";
    pub const SPAN: &str = "dropped frame does not match its authorized span";
    pub const ALLOCATION: &str = "record allocation failed";
    /// The start of the refusal of a dropped frame that claims more than a frame holds.
    pub(crate) const CLAIM_WORDS: &str = "a dropped frame claims ";
    /// More bytes of dropped frames wait for their authorization than the ingest's bound
    /// (`crate::ingest::MAX_UNAUTHORIZED_BYTES`).
    pub const BUDGET: &str =
        "dropped frames claim more than 1073741824 bytes before their authorization";

    /// The refusal of a dropped frame that claims `len` bytes, over `MAX_BLOB_BYTES`.
    pub fn claim(len: u32) -> String {
        format!(
            "{CLAIM_WORDS}{len} bytes; a frame holds at most {}",
            super::MAX_BLOB_BYTES
        )
    }
}

/// The record rules of a dropped frame, shared by the writer and the reader (and by the sync
/// stream's `DROPPED` record, `crate::sync_stream`): a removable kind, the residue its kind needs
/// (BLOCK and CHANGE one, BLOB_CHUNK none), and an original length of at most `MAX_BLOB_BYTES`.
pub(crate) fn check_dropped(
    kind: u8,
    residue: Option<&Hash>,
    len: u32,
) -> Result<(), Cow<'static, str>> {
    let needs_residue = match kind {
        kinds::BLOCK | kinds::CHANGE => true,
        kinds::BLOB_CHUNK => false,
        _ => return Err(drop_text::INVALID_KIND.into()),
    };
    if residue.is_some() != needs_residue {
        return Err(drop_text::INVALID_RESIDUE.into());
    }
    if u64::from(len) > MAX_BLOB_BYTES {
        return Err(drop_text::claim(len).into());
    }
    Ok(())
}

/// Decodes the payload of a `DROPPED` record: exactly 102 bytes, a removable kind, a residue flag
/// of 0 (with a zero hash) or 1, the residue its kind needs, and an original length of at most
/// `MAX_BLOB_BYTES`.
pub(crate) fn parse_dropped(p: &[u8]) -> Result<Frame, Cow<'static, str>> {
    if p.len() != DROPPED_LEN {
        return Err(drop_text::WRONG_LENGTH.into());
    }
    let kind = p[0];
    if !crate::redaction::is_removable(kind) {
        return Err(drop_text::INVALID_KIND.into());
    }
    let hash = Hash(p[1..33].try_into().expect("32 bytes"));
    let len = u32::from_le_bytes(p[33..37].try_into().expect("4 bytes"));
    let apply = Hash(p[37..69].try_into().expect("32 bytes"));
    let residue_hash = Hash(p[70..102].try_into().expect("32 bytes"));
    let residue = match p[69] {
        0 if residue_hash.0 == [0u8; 32] => None,
        1 => Some(residue_hash),
        _ => return Err(drop_text::INVALID_RESIDUE.into()),
    };
    check_dropped(kind, residue.as_ref(), len)?;
    Ok(Frame::Dropped {
        kind,
        hash,
        len,
        apply,
        residue,
    })
}

/// Whether a dump of `dump_format` may hold `DROPPED` records: format 2 does, format 1 never.
fn drops_allowed(dump_format: u32) -> bool {
    dump_format >= 2
}

/// The offset after `frames` in the source log, from `start`: each frame takes `37 +
/// original_len` bytes. None when it passes `u64::MAX`.
fn batch_end(start: u64, frames: &[Frame]) -> Option<u64> {
    frames.iter().try_fold(start, |at, f| {
        at.checked_add(FRAME_HEADER)?
            .checked_add(u64::from(f.original_len()))
    })
}

/// The dump's header: the store format of its frames, the source log's end, the writing rdy's
/// version (for people only), and the chain facts of dump format 3 (None for formats 1 and 2,
/// which have none). A writer needs them: it writes dump format 3 only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpHeader {
    pub store_format: u32,
    pub log_end: Location,
    pub rdy: String,
    pub chain: Option<DumpChainHeader>,
}

/// One sealed batch of the source log: its marker's start and digest, and its data frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpBatch {
    pub start: Location,
    pub digest: Hash,
    pub frames: Vec<Frame>,
}

/// One item of a dump after its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DumpItem {
    File { path: String, bytes: Vec<u8> },
    Batch(DumpBatch),
}

/// What a dump holds so far: batches, store frames, project files, and bytes of the stream.
/// After the trailer (the writer's `finish`, or the reader's verified end of file) it also holds
/// the trailer's removal facts and digest; before, they are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DumpTotals {
    pub batches: u64,
    pub frames: u64,
    pub files: u32,
    pub bytes: u64,
    pub removals: u32,
    pub first_removed: Option<Location>,
    pub digest: Hash,
}

impl Default for DumpTotals {
    fn default() -> Self {
        DumpTotals {
            batches: 0,
            frames: 0,
            files: 0,
            bytes: 0,
            removals: 0,
            first_removed: None,
            digest: Hash([0u8; 32]),
        }
    }
}

/// A dump's trailer: its counts, its removal facts and the digest over every record before it.
/// A trailer of dump format 1 or 2 records no removal facts: zero and none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DumpTrailer {
    pub batches: u64,
    pub frames: u64,
    pub files: u32,
    pub removals: u32,
    pub first_removed: Option<Location>,
    pub digest: Hash,
}

/// The removal facts a format-3 trailer records: how many `REDACTION_APPLY` frames the dump
/// holds (each physical one counts), and the lowest local span those applies removed, or none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemovalFacts {
    pub removals: u32,
    pub first_removed: Option<Location>,
}

/// The removal facts of a dump, gathered batch by batch, in file order, from the frames the
/// checked source walker hands the writer or the reader reads: every `REDACTION_APPLY` frame
/// counts (each physical one), and the first removed span is the lowest place of a dropped frame
/// whose `DROPPED` record names one of those applies as its authority. A dropped frame that names
/// anything else, a carried record or an apply of no frame here, is not a span of the dump's
/// applies. Only the frames are read: no manifest position is trusted. Growth is fallible.
#[derive(Clone, Debug, Default)]
pub struct RemovalTally {
    /// The `REDACTION_APPLY` frames seen.
    applies: u64,
    /// The hashes of those frames.
    apply_hashes: HashSet<Hash>,
    /// The lowest dropped place seen for each authority a `DROPPED` record names.
    lowest: HashMap<Hash, Location>,
}

impl RemovalTally {
    /// Adds batch `b`. Refuses a place past `u64::MAX` (`dump position overflows`) and a refused
    /// growth (`record allocation failed`), as `StoreError::Refused`.
    pub fn observe(&mut self, b: &DumpBatch) -> Result<(), StoreError> {
        self.add(b).map_err(|what| StoreError::Refused(what.into()))
    }

    /// The facts of the batches added so far. Refuses more than `u32::MAX` applies (`dump
    /// position overflows`).
    pub fn facts(&self) -> Result<RemovalFacts, StoreError> {
        let removals = u32::try_from(self.applies)
            .map_err(|_| StoreError::Refused(chain_text::OVERFLOW.into()))?;
        Ok(RemovalFacts {
            removals,
            first_removed: self.first_removed(),
        })
    }

    /// The `REDACTION_APPLY` frames added so far.
    fn applies(&self) -> u64 {
        self.applies
    }

    /// The lowest place of a dropped frame whose authority is an apply added so far.
    fn first_removed(&self) -> Option<Location> {
        self.lowest
            .iter()
            .filter(|(authority, _)| self.apply_hashes.contains(*authority))
            .map(|(_, at)| *at)
            .min()
    }

    /// [`RemovalTally::observe`], with the refusal's text only.
    fn add(&mut self, b: &DumpBatch) -> Result<(), &'static str> {
        let mut at = b.start.offset;
        for f in &b.frames {
            match f {
                Frame::Present { kind, hash, .. } if *kind == kinds::REDACTION_APPLY => {
                    self.applies = self.applies.saturating_add(1);
                    self.apply_hashes
                        .try_reserve(1)
                        .map_err(|_| drop_text::ALLOCATION)?;
                    self.apply_hashes.insert(*hash);
                }
                Frame::Present { .. } => {}
                Frame::Dropped { apply, .. } => {
                    let place = Location {
                        segment: b.start.segment,
                        offset: at,
                    };
                    match self.lowest.get_mut(apply) {
                        Some(low) => *low = (*low).min(place),
                        None => {
                            self.lowest
                                .try_reserve(1)
                                .map_err(|_| drop_text::ALLOCATION)?;
                            self.lowest.insert(*apply, place);
                        }
                    }
                }
            }
            at = at
                .checked_add(FRAME_HEADER)
                .and_then(|a| a.checked_add(u64::from(f.original_len())))
                .ok_or(chain_text::OVERFLOW)?;
        }
        Ok(())
    }
}

/// True if `path` is one of the project files a dump carries (the closed set): `.rdy.yaml`,
/// `.rdyignore`, or `.rdy/packs/<name>` with a plain YAML file name.
pub fn is_project_file(path: &str) -> bool {
    if path == ".rdy.yaml" || path == ".rdyignore" {
        return true;
    }
    let Some(name) = path.strip_prefix(".rdy/packs/") else {
        return false;
    };
    !name.starts_with('.')
        && (name.ends_with(".yaml") || name.ends_with(".yml"))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The store kinds a batch's frames may have, for each `(dump_format, store_format)` pair of
/// `READS`: exactly the kinds that store format assigns, never the batch marker. The writer and
/// the reader both use this one table. A pair, once released, never changes its set.
const FRAME_KINDS: &[((u32, u32), &[u8])] = &[
    (
        (1, 10),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
        ],
    ),
    (
        (1, 11),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
        ],
    ),
    // store format 12 adds refs only (meta/repo_id, meta/origin): the kinds of 11
    (
        (1, 12),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
        ],
    ),
    // store format 13 adds the redaction kinds; dump format 2 adds the DROPPED record
    (
        (2, 13),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY,
        ],
    ),
    // store format 14 adds the signature
    (
        (2, 14),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::SIGNATURE,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY,
        ],
    ),
    // store format 15 adds refs only (meta/store_mode, a holder's answer/tag-object:<hash>): the kinds
    // of (2, 14)
    (
        (2, 15),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::SIGNATURE,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY,
        ],
    ),
    // store format 16 adds the carried removal; dump format 3 adds the chain facts
    (
        (3, 16),
        &[
            kinds::BLOCK,
            kinds::CHANGE,
            kinds::PROPOSAL,
            kinds::LANDING,
            kinds::SNAPSHOT,
            kinds::BLOB_CHUNK,
            kinds::BLOB_MANIFEST,
            kinds::REF_UPDATE,
            kinds::TAG,
            kinds::ANSWER,
            kinds::SIGNATURE,
            kinds::VIEW_OP,
            kinds::INTENT,
            kinds::ALT_SET,
            kinds::REVIEW,
            kinds::TENSION,
            kinds::TRANSITION,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY,
            kinds::REDACTION_CARRIED,
        ],
    ),
];

/// The store kinds a batch's frames may have in a dump of `dump_format` whose frames are of
/// `store_format`; None for a pair this rdy does not read.
pub fn frame_kinds(dump_format: u32, store_format: u32) -> Option<&'static [u8]> {
    FRAME_KINDS
        .iter()
        .find(|(pair, _)| *pair == (dump_format, store_format))
        .map(|(_, k)| *k)
}

/// The `BATCH` record's `kinds`: `Hash::of` over each frame's (original) `kind || hash`, in
/// order. None when its buffer cannot be allocated.
fn kinds_digest(frames: &[Frame]) -> Option<Hash> {
    let mut b = Vec::new();
    b.try_reserve_exact(frames.len().checked_mul(33)?).ok()?;
    for f in frames {
        b.push(f.kind());
        b.extend_from_slice(&f.hash().0);
    }
    Some(Hash::of(&b))
}

/// `kinds::batch_digest` of `hashes`, with its buffer taken by fallible allocation. None when it
/// cannot be allocated.
pub(crate) fn try_batch_digest<'a>(
    hashes: impl ExactSizeIterator<Item = &'a Hash>,
) -> Option<Hash> {
    let mut b = Vec::new();
    b.try_reserve_exact(hashes.len().checked_mul(32)?).ok()?;
    for h in hashes {
        b.extend_from_slice(&h.0);
    }
    Some(Hash::of(&b))
}

/// The bytes one record adds to the trailer chain (a dump's, and a sync stream's,
/// `crate::sync_stream`).
pub(crate) const LINK_LEN: usize = 1 + 8 + 32;

/// Adds one record to the bytes the trailer's digest covers: `kind || len (u64) || hash`. Its
/// growth is fallible: None when it cannot be allocated.
pub(crate) fn link(chain: &mut Vec<u8>, kind: u8, len: u64, hash: &Hash) -> Option<()> {
    chain.try_reserve(LINK_LEN).ok()?;
    chain.push(kind);
    chain.extend_from_slice(&len.to_le_bytes());
    chain.extend_from_slice(&hash.0);
    Some(())
}

/// A copy of `b` in a fallibly allocated buffer.
fn try_copy(b: &[u8]) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(b.len()).ok()?;
    v.extend_from_slice(b);
    Some(v)
}

/// A copy of `s` in a fallibly allocated string.
fn try_string(s: &str) -> Option<String> {
    let mut o = String::new();
    o.try_reserve_exact(s.len()).ok()?;
    o.push_str(s);
    Some(o)
}

fn cannot_stand(kind: u8) -> String {
    format!("a record of kind {kind} cannot stand here")
}

/// The bounded decode of a carried removal record (kind 23), the only kind of a dump frame that
/// is decoded on the way: the decoder's own error when it refuses one. Other kinds pass.
fn check_carried(kind: u8, payload: &[u8]) -> Result<(), String> {
    if kind != kinds::REDACTION_CARRIED {
        return Ok(());
    }
    crate::bounded::decode::<rdy_model::RedactionCarried>(payload)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn not_carried(path: &str) -> String {
    format!("the project file path {path:?} is not one rdy carries")
}

fn too_large(path: &str) -> String {
    format!("the project file {path:?} is larger than 16 MiB")
}

fn put_location(p: &mut Vec<u8>, l: &Location) {
    p.extend_from_slice(&l.segment.to_le_bytes());
    p.extend_from_slice(&l.offset.to_le_bytes());
}

fn put_marker(p: &mut Vec<u8>, m: &BatchMarker) {
    put_location(p, &m.start);
    p.extend_from_slice(&m.frames.to_le_bytes());
    p.extend_from_slice(&m.digest.0);
}

/// The format-3 header payload of `h` with its chain facts `c`.
fn header_payload(h: &DumpHeader, c: &DumpChainHeader) -> Vec<u8> {
    let mut p = Vec::with_capacity(HEADER_FIXED + h.rdy.len());
    p.push(c.kind.byte());
    p.extend_from_slice(&h.store_format.to_le_bytes());
    p.extend_from_slice(&c.repo_id.0);
    put_location(&mut p, &c.base);
    put_marker(&mut p, &c.base_batch);
    p.extend_from_slice(&c.prev.0);
    put_location(&mut p, &h.log_end);
    put_marker(&mut p, &c.end_batch);
    p.extend_from_slice(&(h.rdy.len() as u16).to_le_bytes());
    p.extend_from_slice(h.rdy.as_bytes());
    p
}

/// Parses the header payload of a dump of `format`. Err: the refusal (it does not parse, its
/// text cannot be allocated, or a full dump's base facts are not the origin's).
fn parse_header(format: u32, p: &[u8]) -> Result<DumpHeader, &'static str> {
    let mut c = Cursor(p);
    let fields = (|| {
        let (kind, store_format) = if format >= 3 {
            (Some(DumpKind::of_byte(c.u8()?)?), c.u32()?)
        } else {
            (None, c.u32()?)
        };
        let mut chain = match kind {
            Some(kind) => Some(DumpChainHeader {
                kind,
                repo_id: RepoId(c.take(16)?.try_into().ok()?),
                base: c.location()?,
                base_batch: c.marker()?,
                prev: c.hash()?,
                // read after the end, below
                end_batch: BatchMarker::ZERO,
            }),
            None => None,
        };
        let log_end = c.location()?;
        if let Some(chain) = chain.as_mut() {
            chain.end_batch = c.marker()?;
        }
        let rdy_len = c.u16()? as usize;
        if rdy_len > MAX_RDY_BYTES {
            return None;
        }
        let rdy = std::str::from_utf8(c.take(rdy_len)?).ok()?;
        c.0.is_empty()
            .then_some((store_format, log_end, rdy, chain))
    })();
    let (store_format, log_end, rdy, chain) = fields.ok_or(chain_text::HEADER)?;
    if chain.is_some_and(|c| c.kind == DumpKind::Full && !c.has_origin_base()) {
        return Err(chain_text::FULL_BASE);
    }
    Ok(DumpHeader {
        store_format,
        log_end,
        rdy: try_string(rdy).ok_or(drop_text::ALLOCATION)?,
        chain,
    })
}

/// The format-3 trailer payload: the counts, the removal facts and the digest.
fn trailer_payload(t: &DumpTrailer) -> Vec<u8> {
    let mut p = Vec::with_capacity(TRAILER_LEN);
    p.extend_from_slice(&t.batches.to_le_bytes());
    p.extend_from_slice(&t.frames.to_le_bytes());
    p.extend_from_slice(&t.files.to_le_bytes());
    p.extend_from_slice(&t.removals.to_le_bytes());
    p.push(u8::from(t.first_removed.is_some()));
    put_location(&mut p, &t.first_removed.unwrap_or(BatchMarker::ZERO.start));
    p.extend_from_slice(&t.digest.0);
    p
}

/// Parses a trailer payload of a dump of `format`: exactly its length, and in format 3 a flag
/// of 0 (with a zero position) or 1. None when it does not parse.
fn parse_trailer(format: u32, p: &[u8]) -> Option<DumpTrailer> {
    let mut c = Cursor(p);
    let batches = c.u64()?;
    let frames = c.u64()?;
    let files = c.u32()?;
    let (removals, first_removed) = if format >= 3 {
        let removals = c.u32()?;
        let flag = c.u8()?;
        let at = c.location()?;
        let first = match flag {
            0 if at == BatchMarker::ZERO.start => None,
            1 => Some(at),
            _ => return None,
        };
        (removals, first)
    } else {
        (0, None)
    };
    let digest = c.hash()?;
    c.0.is_empty().then_some(DumpTrailer {
        batches,
        frames,
        files,
        removals,
        first_removed,
        digest,
    })
}

/// Reads little-endian fields off the front of a payload.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn hash(&mut self) -> Option<Hash> {
        Some(Hash(self.take(32)?.try_into().ok()?))
    }
    fn location(&mut self) -> Option<Location> {
        Some(Location {
            segment: self.u32()?,
            offset: self.u64()?,
        })
    }
    fn marker(&mut self) -> Option<BatchMarker> {
        Some(BatchMarker {
            start: self.location()?,
            frames: self.u32()?,
            digest: self.hash()?,
        })
    }
}

/// Where the writer is in the closed order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Files,
    Batches,
}

/// Writes a dump: the prelude and header on `new`, then project files, then batches, then the
/// trailer on `finish`. It refuses anything out of the closed order with `StoreError::Refused`.
pub struct DumpWriter<W: Write> {
    w: W,
    phase: Phase,
    last_path: Option<String>,
    /// The store kinds a frame may have (`frame_kinds` of the header's pair).
    kinds: &'static [u8],
    /// `kind || len || hash` of every record so far (the trailer's digest covers them).
    chain: Vec<u8>,
    totals: DumpTotals,
}

impl<W: Write> DumpWriter<W> {
    /// Writes the prelude and the header, always of `DUMP_FORMAT` (there is no writer of an
    /// older format). Refuses a header with no chain facts or an `rdy` text over 256 bytes (the
    /// header does not parse), and a header whose store format is not one this rdy reads in a
    /// dump of `DUMP_FORMAT` (`READS`): its reader would refuse the dump. The chain facts are
    /// the caller's: the reader checks them against the batches.
    pub fn new(w: W, header: &DumpHeader) -> Result<Self, StoreError> {
        let Some(chain) = header
            .chain
            .as_ref()
            .filter(|_| header.rdy.len() <= MAX_RDY_BYTES)
        else {
            return Err(StoreError::Refused(chain_text::HEADER.into()));
        };
        let Some(kinds) = READS
            .contains(&(DUMP_FORMAT, header.store_format))
            .then(|| frame_kinds(DUMP_FORMAT, header.store_format))
            .flatten()
        else {
            return Err(StoreError::Refused(store_format_not_read(
                header.store_format,
                DUMP_FORMAT,
            )));
        };
        let mut me = DumpWriter {
            w,
            phase: Phase::Files,
            last_path: None,
            kinds,
            chain: Vec::new(),
            totals: DumpTotals::default(),
        };
        me.w.write_all(MAGIC)?;
        me.w.write_all(&DUMP_FORMAT.to_le_bytes())?;
        me.totals.bytes = (MAGIC.len() + 4) as u64;
        me.record(rec::HEADER, &header_payload(header, chain))?;
        Ok(me)
    }

    /// Writes one project file. Refuses a path outside the closed set, a path not after the
    /// previous one, a file over 16 MiB, more than `MAX_FILES` files, and any file after a batch.
    pub fn file(&mut self, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if self.phase != Phase::Files {
            return Err(StoreError::Refused(cannot_stand(rec::FILE)));
        }
        // a path longer than its `u16` length field could not be read back
        if !is_project_file(path) || path.len() > usize::from(u16::MAX) {
            return Err(StoreError::Refused(not_carried(path)));
        }
        if self.last_path.as_deref().is_some_and(|last| path <= last) {
            return Err(StoreError::Refused(cannot_stand(rec::FILE)));
        }
        if self.totals.files >= MAX_FILES {
            return Err(StoreError::Refused("too many project files".into()));
        }
        if bytes.len() > MAX_FILE_BYTES {
            return Err(StoreError::Refused(too_large(path)));
        }
        let mut p = Vec::with_capacity(2 + path.len() + bytes.len());
        p.extend_from_slice(&(path.len() as u16).to_le_bytes());
        p.extend_from_slice(path.as_bytes());
        p.extend_from_slice(bytes);
        self.record(rec::FILE, &p)?;
        self.last_path = Some(path.to_string());
        self.totals.files += 1;
        Ok(())
    }

    /// Writes one sealed batch: its `BATCH` record, then its frames byte for byte, each dropped
    /// frame as its `DROPPED` record. Refuses a frame of a kind that cannot stand in a batch (a
    /// batch marker or an unassigned kind among them), a frame whose hash is not its payload's, a
    /// payload over `MAX_BLOB_BYTES`, a carried removal record the bounded decoder refuses (its
    /// error), a dropped frame in a dump format that has no `DROPPED` record or one that breaks
    /// its record rules, a batch with a dropped frame whose original frames run past `u64::MAX`
    /// (`dump position overflows`), and a batch whose digest is not its frames'.
    pub fn batch(&mut self, b: &DumpBatch) -> Result<(), StoreError> {
        let n = self.totals.batches + 1;
        // every check before the first byte, so a refused batch writes nothing; no store writes
        // a batch of no frames
        if b.frames.is_empty() {
            return Err(StoreError::Refused(cannot_stand(rec::BATCH)));
        }
        let mut dropped = false;
        for f in &b.frames {
            if !self.kinds.contains(&f.kind()) {
                return Err(StoreError::Refused(cannot_stand(f.kind())));
            }
            match f {
                Frame::Present {
                    kind,
                    hash,
                    payload,
                } => {
                    if payload.len() as u64 > MAX_BLOB_BYTES {
                        return Err(StoreError::Refused(cannot_stand(*kind)));
                    }
                    if Hash::of(payload) != *hash {
                        return Err(StoreError::Refused(
                            "a record's hash does not match its bytes".into(),
                        ));
                    }
                    check_carried(*kind, payload).map_err(StoreError::Refused)?;
                }
                Frame::Dropped {
                    kind, residue, len, ..
                } => {
                    if !drops_allowed(DUMP_FORMAT) {
                        return Err(StoreError::Refused(drop_text::NOT_ALLOWED.into()));
                    }
                    check_dropped(*kind, residue.as_ref(), *len)
                        .map_err(|what| StoreError::Refused(what.into()))?;
                    dropped = true;
                }
            }
        }
        if dropped && batch_end(b.start.offset, &b.frames).is_none() {
            return Err(StoreError::Refused(chain_text::OVERFLOW.into()));
        }
        let refused_allocation = || StoreError::Refused(drop_text::ALLOCATION.into());
        if try_batch_digest(b.frames.iter().map(|f| f.hash())).ok_or_else(refused_allocation)?
            != b.digest
        {
            return Err(StoreError::Refused(format!(
                "batch {n} does not match its digest"
            )));
        }
        let kinds = kinds_digest(&b.frames).ok_or_else(refused_allocation)?;
        let count = u32::try_from(b.frames.len()).map_err(|_| {
            StoreError::Refused(format!(
                "batch {n} holds {} frames, and its record says {}",
                b.frames.len(),
                u32::MAX
            ))
        })?;
        let mut p = Vec::with_capacity(BATCH_LEN);
        p.extend_from_slice(&b.start.segment.to_le_bytes());
        p.extend_from_slice(&b.start.offset.to_le_bytes());
        p.extend_from_slice(&count.to_le_bytes());
        p.extend_from_slice(&b.digest.0);
        p.extend_from_slice(&kinds.0);
        self.record(rec::BATCH, &p)?;
        for f in &b.frames {
            match f {
                // the hash was checked above: each payload is hashed once
                Frame::Present {
                    kind,
                    hash,
                    payload,
                } => self.write_record(*kind, *hash, payload)?,
                Frame::Dropped {
                    kind,
                    hash,
                    len,
                    apply,
                    residue,
                } => {
                    let p = dropped_payload(*kind, hash, *len, apply, residue.as_ref());
                    self.record(rec::DROPPED, &p)?;
                }
            }
        }
        self.phase = Phase::Batches;
        self.totals.batches += 1;
        self.totals.frames += b.frames.len() as u64;
        Ok(())
    }

    /// Writes the trailer with the removal facts `facts`, which the checked source walker
    /// supplies (the reader checks them against the batches), and returns the writer and what
    /// the dump holds.
    pub fn finish(mut self, facts: RemovalFacts) -> Result<(W, DumpTotals), StoreError> {
        let digest = Hash::of(&self.chain);
        let p = trailer_payload(&DumpTrailer {
            batches: self.totals.batches,
            frames: self.totals.frames,
            files: self.totals.files,
            removals: facts.removals,
            first_removed: facts.first_removed,
            digest,
        });
        self.record(rec::TRAILER, &p)?;
        self.w.flush()?;
        self.totals.removals = facts.removals;
        self.totals.first_removed = facts.first_removed;
        self.totals.digest = digest;
        Ok((self.w, self.totals))
    }

    /// Writes one record with its hash.
    fn record(&mut self, kind: u8, payload: &[u8]) -> Result<(), StoreError> {
        self.write_record(kind, Hash::of(payload), payload)
    }

    /// Writes one record whose payload hashes to `hash`, and adds it to the running digest (the
    /// trailer's own is not).
    fn write_record(&mut self, kind: u8, hash: Hash, payload: &[u8]) -> Result<(), StoreError> {
        let len =
            u32::try_from(payload.len()).map_err(|_| StoreError::Refused(cannot_stand(kind)))?;
        self.w.write_all(&len.to_le_bytes())?;
        self.w.write_all(&[kind])?;
        self.w.write_all(&hash.0)?;
        self.w.write_all(payload)?;
        if kind != rec::TRAILER {
            link(&mut self.chain, kind, payload.len() as u64, &hash)
                .ok_or_else(|| StoreError::Refused(drop_text::ALLOCATION.into()))?;
        }
        self.totals.bytes += FRAME_HEADER + payload.len() as u64;
        Ok(())
    }
}

/// Reads a dump and checks every record on the way (verify on receive). `next` returns `None`
/// only after a verified trailer and the end of the file. After an error the reader stays
/// failed: every later `next` returns that error again.
pub struct DumpReader<R: Read> {
    r: R,
    phase: ReadPhase,
    /// The error that ended the read, if one did.
    failed: Option<StoreError>,
    last_path: Option<String>,
    /// The store kinds a frame may have (`frame_kinds` of the dump's pair).
    kinds: &'static [u8],
    /// Whether the dump's format has `DROPPED` records.
    drops: bool,
    /// `kind || len || hash` of every record read so far, the trailer's excepted.
    chain: Vec<u8>,
    totals: DumpTotals,
    /// The dump format, the header's end and its chain facts (format 3).
    format: u32,
    log_end: Location,
    links: Option<DumpChainHeader>,
    /// The physical end and the marker of the last batch read (format 3).
    last: Option<(Location, BatchMarker)>,
    /// The removal facts of the batches read (format 3).
    tally: RemovalTally,
    /// The trailer, once it and the end of the file are verified.
    trailer: Option<DumpTrailer>,
}

/// Where the reader is in the closed order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadPhase {
    Files,
    Batches,
    Done,
}

/// One record as read: where it started, its kind, its hash (checked) and its payload.
struct Record {
    at: u64,
    kind: u8,
    hash: Hash,
    payload: Vec<u8>,
}

/// A copy of an error of the reader, to return again on every later `next`.
fn again(e: &StoreError) -> StoreError {
    match e {
        StoreError::Dump { at, what } => damaged(*at, what.clone()),
        StoreError::Io(io) => StoreError::Io(io::Error::new(io.kind(), io.to_string())),
        other => StoreError::Decode(other.to_string()),
    }
}

fn damaged(at: u64, what: impl Into<String>) -> StoreError {
    StoreError::Dump {
        at,
        what: what.into(),
    }
}

const ENDS_EARLY: &str = "the file ends before its trailer";

/// What a refusal of the reader (`StoreError::Dump`) means for the person: the caller picks the
/// fix by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpRefusal {
    /// The file is not a dump: it does not start with the magic.
    NotADump,
    /// A dump format, or a store format in a dump, that this rdy does not read.
    FormatNotRead,
    /// Damage at a byte.
    Damaged,
    /// Damage at a byte: dropped frames claim more bytes than rdy writes for them, one frame
    /// over `MAX_BLOB_BYTES` or all those that wait for their authorization over the ingest's
    /// bound. Its fix differs: such a dump may not be rdy's at all.
    Claim,
}

impl DumpRefusal {
    /// The kind of the reader's refusal `what`. It reads the texts this module writes
    /// (`NOT_A_DUMP`, `dump_format_not_read`, `store_format_not_read`, `drop_text::claim`,
    /// `drop_text::BUDGET`), so a text and its kind change together.
    pub fn of(what: &str) -> DumpRefusal {
        if what == NOT_A_DUMP {
            DumpRefusal::NotADump
        } else if what.ends_with(NOT_READ)
            && (what.starts_with(DUMP_FORMAT_WORDS) || what.starts_with(STORE_FORMAT_WORDS))
        {
            DumpRefusal::FormatNotRead
        } else if what.starts_with(drop_text::CLAIM_WORDS) || what == drop_text::BUDGET {
            DumpRefusal::Claim
        } else {
            DumpRefusal::Damaged
        }
    }
}

/// The refusal of a file that does not start with `MAGIC`.
const NOT_A_DUMP: &str = "it does not start with RDYDUMP";
/// The end of a refusal of a format this rdy does not read.
const NOT_READ: &str = " is not one this rdy reads";
const DUMP_FORMAT_WORDS: &str = "dump format ";
const STORE_FORMAT_WORDS: &str = "store format ";

/// The refusal of dump format `format`.
fn dump_format_not_read(format: u32) -> String {
    format!("{DUMP_FORMAT_WORDS}{format}{NOT_READ}")
}

/// The refusal of store format `store_format` in a dump of format `format`.
fn store_format_not_read(store_format: u32, format: u32) -> String {
    format!("{STORE_FORMAT_WORDS}{store_format} in a dump of format {format}{NOT_READ}")
}

/// The length of the prelude: the magic and the dump format.
const PRELUDE_LEN: u64 = MAGIC.len() as u64 + 4;

/// Reads the prelude off `r`: the magic, then a dump format this rdy reads (some pair of
/// `READS`). A file that does not start with the magic is not a dump.
fn read_prelude(r: &mut impl Read) -> Result<u32, StoreError> {
    // the magic, or as much of it as the file holds: a shorter file is not a dump
    let mut magic = [0u8; MAGIC.len()];
    let mut got = 0;
    while got < magic.len() {
        match r.read(&mut magic[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    if got < magic.len() || &magic != MAGIC {
        return Err(damaged(0, NOT_A_DUMP));
    }
    let mut format = [0u8; 4];
    read_exact_at(r, &mut format, MAGIC.len() as u64)?;
    let format = u32::from_le_bytes(format);
    if !READS.iter().any(|(d, _)| *d == format) {
        return Err(damaged(MAGIC.len() as u64, dump_format_not_read(format)));
    }
    Ok(format)
}

/// Fills `buf` from `r`, or fails with a dump that ends before its trailer (blamed on `at`).
fn read_exact_at(r: &mut impl Read, buf: &mut [u8], at: u64) -> Result<(), StoreError> {
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(damaged(at, ENDS_EARLY)),
        Err(e) => Err(e.into()),
    }
}

/// The header of a dump of `format` from the `HEADER` payload `p` read at `at`, and the store
/// kinds its batches may hold. Refuses a payload that does not parse and a pair outside `READS`.
fn header_of(format: u32, at: u64, p: &[u8]) -> Result<(DumpHeader, &'static [u8]), StoreError> {
    let header = parse_header(format, p).map_err(|what| damaged(at, what))?;
    let Some(kinds) = READS
        .contains(&(format, header.store_format))
        .then(|| frame_kinds(format, header.store_format))
        .flatten()
    else {
        return Err(damaged(
            at,
            store_format_not_read(header.store_format, format),
        ));
    };
    Ok((header, kinds))
}

/// Reads only a dump's envelope from `r`: the prelude, the `HEADER` record and the fixed-length
/// final `TRAILER` record, each record's own hash checked. Returns the dump format, the header,
/// the trailer and the file's length. The body between them is neither read nor checked, so its
/// digest is not verified here: only a whole read (`DumpReader`) verifies a dump.
pub fn read_envelope<R: Read + Seek>(
    r: &mut R,
) -> Result<(u32, DumpHeader, DumpTrailer, u64), StoreError> {
    r.seek(SeekFrom::Start(0))?;
    let format = read_prelude(r)?;
    let at = PRELUDE_LEN;
    let mut head = [0u8; FRAME_HEADER as usize];
    read_exact_at(r, &mut head, at)?;
    let len = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes")) as usize;
    if head[4] != rec::HEADER {
        return Err(damaged(at, cannot_stand(head[4])));
    }
    let fixed = if format >= 3 {
        HEADER_FIXED
    } else {
        LEGACY_HEADER_FIXED
    };
    if len > fixed + MAX_RDY_BYTES {
        return Err(damaged(at, chain_text::HEADER));
    }
    let mut payload = vec![0u8; len];
    read_exact_at(r, &mut payload, at)?;
    if Hash::of(&payload).0 != head[5..37] {
        return Err(damaged(at, "a record's hash does not match its bytes"));
    }
    let (header, _) = header_of(format, at, &payload)?;
    let body = at + FRAME_HEADER + len as u64;
    let trailer_len = if format >= 3 {
        TRAILER_LEN
    } else {
        LEGACY_TRAILER_LEN
    };
    let framed = FRAME_HEADER + trailer_len as u64;
    let file_len = r.seek(SeekFrom::End(0))?;
    let Some(at) = file_len.checked_sub(framed).filter(|t| *t >= body) else {
        return Err(damaged(body, ENDS_EARLY));
    };
    r.seek(SeekFrom::Start(at))?;
    let mut rec_bytes = [0u8; FRAME_HEADER as usize + TRAILER_LEN];
    let rec_bytes = &mut rec_bytes[..framed as usize];
    read_exact_at(r, rec_bytes, at)?;
    let (head, payload) = rec_bytes.split_at(FRAME_HEADER as usize);
    let len = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes")) as usize;
    if head[4] != rec::TRAILER || len != trailer_len {
        return Err(damaged(at, chain_text::TRAILER));
    }
    if Hash::of(payload).0 != head[5..37] {
        return Err(damaged(at, "a record's hash does not match its bytes"));
    }
    let trailer = parse_trailer(format, payload).ok_or_else(|| damaged(at, chain_text::TRAILER))?;
    Ok((format, header, trailer, file_len))
}

impl<R: Read> DumpReader<R> {
    /// Reads the prelude and the header. Refuses a file that does not start with the magic, and
    /// a `(dump_format, store_format)` pair outside `READS`. Returns the reader, the dump format
    /// and the header.
    pub fn open(r: R) -> Result<(Self, u32, DumpHeader), StoreError> {
        let mut me = DumpReader {
            r,
            phase: ReadPhase::Files,
            failed: None,
            last_path: None,
            kinds: &[],
            drops: false,
            chain: Vec::new(),
            totals: DumpTotals::default(),
            format: 0,
            log_end: BatchMarker::ZERO.start,
            links: None,
            last: None,
            tally: RemovalTally::default(),
            trailer: None,
        };
        let format = read_prelude(&mut me.r)?;
        // the record bound of format 3 holds from its header on
        me.format = format;
        me.totals.bytes = PRELUDE_LEN;
        me.drops = drops_allowed(format);
        let first = me.record(false)?;
        if first.kind != rec::HEADER {
            return Err(damaged(first.at, cannot_stand(first.kind)));
        }
        let (header, kinds) = header_of(format, first.at, &first.payload)?;
        me.kinds = kinds;
        me.format = format;
        me.log_end = header.log_end;
        me.links = header.chain;
        me.link(&first)?;
        Ok((me, format, header))
    }

    /// The trailer, once the reader has verified it and the end of the file (`next` returned
    /// `None`); None before.
    pub fn trailer(&self) -> Option<&DumpTrailer> {
        self.trailer.as_ref()
    }

    /// The next project file or batch; `None` after a verified trailer at the end of the file.
    /// After an error, every later call returns that error again: never an item or `None`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<DumpItem>, StoreError> {
        if let Some(e) = &self.failed {
            return Err(again(e));
        }
        let out = self.step();
        if let Err(e) = &out {
            self.failed = Some(again(e));
        }
        out
    }

    /// One step of `next`.
    fn step(&mut self) -> Result<Option<DumpItem>, StoreError> {
        if self.phase == ReadPhase::Done {
            return Ok(None);
        }
        let record = self.record(false)?;
        match record.kind {
            rec::FILE if self.phase == ReadPhase::Files => {
                let item = self.file(&record)?;
                self.link(&record)?;
                Ok(Some(item))
            }
            rec::BATCH => {
                self.phase = ReadPhase::Batches;
                self.link(&record)?;
                self.batch(&record).map(|b| Some(DumpItem::Batch(b)))
            }
            rec::TRAILER => {
                self.read_trailer(&record)?;
                Ok(None)
            }
            k => Err(damaged(record.at, cannot_stand(k))),
        }
    }

    /// What the reader has read so far.
    pub fn totals(&self) -> DumpTotals {
        self.totals
    }

    /// The byte offset of the next record.
    pub fn offset(&self) -> u64 {
        self.totals.bytes
    }

    fn file(&mut self, record: &Record) -> Result<DumpItem, StoreError> {
        let mut c = Cursor(&record.payload);
        let Some(path_len) = c.u16() else {
            return Err(damaged(record.at, not_carried("")));
        };
        let Some(raw) = c.take(path_len as usize) else {
            return Err(damaged(
                record.at,
                not_carried(&String::from_utf8_lossy(c.0)),
            ));
        };
        let Ok(path) = std::str::from_utf8(raw) else {
            return Err(damaged(
                record.at,
                not_carried(&String::from_utf8_lossy(raw)),
            ));
        };
        if !is_project_file(path) {
            return Err(damaged(record.at, not_carried(path)));
        }
        if self.last_path.as_deref().is_some_and(|last| path <= last) {
            return Err(damaged(record.at, cannot_stand(rec::FILE)));
        }
        if self.totals.files >= MAX_FILES {
            return Err(damaged(record.at, "too many project files"));
        }
        if c.0.len() > MAX_FILE_BYTES {
            return Err(damaged(record.at, too_large(path)));
        }
        let refused = || damaged(record.at, drop_text::ALLOCATION);
        let item = DumpItem::File {
            path: try_string(path).ok_or_else(refused)?,
            bytes: try_copy(c.0).ok_or_else(refused)?,
        };
        self.last_path = Some(try_string(path).ok_or_else(refused)?);
        self.totals.files += 1;
        Ok(item)
    }

    fn batch(&mut self, record: &Record) -> Result<DumpBatch, StoreError> {
        let n = self.totals.batches + 1;
        let mut c = Cursor(&record.payload);
        let parsed = (|| {
            let segment = c.u32()?;
            let offset = c.u64()?;
            let frames = c.u32()?;
            let digest = c.hash()?;
            let kinds = c.hash()?;
            c.0.is_empty()
                .then_some((Location { segment, offset }, frames, digest, kinds))
        })();
        // no store writes a batch of no frames
        let Some((start, count, digest, kinds)) = parsed.filter(|p| p.1 > 0) else {
            return Err(damaged(record.at, cannot_stand(record.kind)));
        };
        // no buffer is sized from an unread count: frames are pushed as they are read, each slot
        // taken by fallible growth
        let mut frames: Vec<Frame> = Vec::new();
        let mut dropped = false;
        while frames.len() < count as usize {
            let f = self.record(true)?;
            if f.kind != rec::DROPPED && f.kind >= rec::HEADER {
                return Err(damaged(
                    f.at,
                    format!(
                        "batch {n} holds {} frames, and its record says {count}",
                        frames.len()
                    ),
                ));
            }
            // the trailer's digest covers the container record as it is
            self.link(&f)?;
            let frame = if f.kind == rec::DROPPED {
                dropped = true;
                parse_dropped(&f.payload).map_err(|what| damaged(f.at, what))?
            } else {
                Frame::Present {
                    kind: f.kind,
                    hash: f.hash,
                    payload: f.payload,
                }
            };
            if !self.kinds.contains(&frame.kind()) {
                return Err(damaged(f.at, cannot_stand(frame.kind())));
            }
            if let Some(payload) = frame.present_payload() {
                check_carried(frame.kind(), payload).map_err(|what| damaged(f.at, what))?;
            }
            frames
                .try_reserve(1)
                .map_err(|_| damaged(f.at, drop_text::ALLOCATION))?;
            frames.push(frame);
        }
        // a dropped frame's place is the batch start plus the original sizes before it; past
        // u64::MAX it has none (a position overflow from format 3 on)
        if dropped && batch_end(start.offset, &frames).is_none() {
            let what = if self.format >= 3 {
                chain_text::OVERFLOW
            } else {
                drop_text::SPAN
            };
            return Err(damaged(record.at, what));
        }
        let refused = || damaged(record.at, drop_text::ALLOCATION);
        if try_batch_digest(frames.iter().map(|f| f.hash())).ok_or_else(refused)? != digest
            || kinds_digest(&frames).ok_or_else(refused)? != kinds
        {
            return Err(damaged(
                record.at,
                format!("batch {n} does not match its digest"),
            ));
        }
        let batch = DumpBatch {
            start,
            digest,
            frames,
        };
        if let Some(links) = self.links {
            self.follow(record.at, &links, &batch)?;
        }
        self.totals.batches += 1;
        self.totals.frames += batch.frames.len() as u64;
        Ok(batch)
    }

    /// The format-3 checks of one batch read at `at`: the first follows the header's base; each
    /// one's physical end and marker, and its removal facts, are kept for the trailer.
    fn follow(
        &mut self,
        at: u64,
        links: &DumpChainHeader,
        b: &DumpBatch,
    ) -> Result<(), StoreError> {
        if self.last.is_none() && !follows(links.base, b.start) {
            return Err(damaged(at, chain_text::FIRST_BATCH));
        }
        let end = dump_chain::batch_end(b).ok_or_else(|| damaged(at, chain_text::OVERFLOW))?;
        self.last = Some((end, BatchMarker::of(b)));
        self.tally.add(b).map_err(|what| damaged(at, what))
    }

    fn read_trailer(&mut self, record: &Record) -> Result<(), StoreError> {
        let mismatch = || {
            damaged(
                record.at,
                "the trailer's counts or digest do not match the dump",
            )
        };
        let t = match parse_trailer(self.format, &record.payload) {
            Some(t) => t,
            None if self.format >= 3 => return Err(damaged(record.at, chain_text::TRAILER)),
            None => return Err(mismatch()),
        };
        if t.batches != self.totals.batches
            || t.frames != self.totals.frames
            || t.files != self.totals.files
            || t.digest != Hash::of(&self.chain)
        {
            return Err(mismatch());
        }
        if let Some(links) = self.links {
            self.check_ends(record.at, &links, &t)?;
        }
        let mut extra = [0u8; 1];
        let at = self.totals.bytes;
        loop {
            match self.r.read(&mut extra) {
                Ok(0) => break,
                Ok(_) => return Err(damaged(at, "bytes follow the trailer")),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.phase = ReadPhase::Done;
        self.totals.removals = t.removals;
        self.totals.first_removed = t.first_removed;
        self.totals.digest = t.digest;
        self.trailer = Some(t);
        Ok(())
    }

    /// The format-3 checks at trailer `t` (read at `at`): an increment holds a batch; the last
    /// batch (or, with none, the base) ends at the header's end with the header's end marker; the
    /// trailer counts the dump's applies; its first removed span is the lowest dropped frame of
    /// those applies (none without one), or, with an apply, a span before the base, which only
    /// an increment has: an earlier file holds it.
    fn check_ends(
        &self,
        at: u64,
        links: &DumpChainHeader,
        t: &DumpTrailer,
    ) -> Result<(), StoreError> {
        let (end, marker) = match self.last {
            Some(last) => last,
            None if links.kind == DumpKind::Increment => {
                return Err(damaged(at, chain_text::EMPTY_INCREMENT));
            }
            None => (links.base, links.base_batch),
        };
        if end != self.log_end || marker != links.end_batch {
            return Err(damaged(at, chain_text::LAST_BATCH));
        }
        // a hole that names no apply of this file (a carried record's, say) is no span of its
        // applies; whether every hole is authorized is the ingest's check
        let earlier = t.removals > 0 && t.first_removed.is_some_and(|q| q < links.base);
        let first_ok = t.first_removed == self.tally.first_removed() || earlier;
        if u64::from(t.removals) != self.tally.applies() || !first_ok {
            return Err(damaged(at, chain_text::REMOVALS));
        }
        Ok(())
    }

    /// Adds a read record to the bytes the trailer's digest covers; a refused growth is
    /// `record allocation failed` at the record.
    fn link(&mut self, r: &Record) -> Result<(), StoreError> {
        link(&mut self.chain, r.kind, r.payload.len() as u64, &r.hash)
            .ok_or_else(|| damaged(r.at, drop_text::ALLOCATION))
    }

    /// Reads one record and checks its hash. The end of the file inside a record, or where a
    /// record should start, is a dump that ends before its trailer. `frame` says a batch's data
    /// frame is expected: there a `DROPPED` record is refused by its header alone, before its
    /// payload is read, in a dump format without one or with another length than 102 bytes. From
    /// dump format 3 on, a record that claims more than `MAX_BLOB_BYTES` is refused by its header
    /// alone (`record allocation failed`). The payload is read in chunks of at most
    /// `ERASE_BUFFER` bytes, each taken by fallible growth: a length the header claims is never
    /// reserved before its bytes arrive.
    fn record(&mut self, frame: bool) -> Result<Record, StoreError> {
        let at = self.totals.bytes;
        let mut head = [0u8; FRAME_HEADER as usize];
        self.exact(&mut head, at)?;
        let len = u32::from_le_bytes(head[0..4].try_into().unwrap()) as u64;
        let kind = head[4];
        let hash = Hash(head[5..37].try_into().unwrap());
        if frame && kind == rec::DROPPED {
            if !self.drops {
                return Err(damaged(at, drop_text::NOT_ALLOWED));
            }
            if len != DROPPED_LEN as u64 {
                return Err(damaged(at, drop_text::WRONG_LENGTH));
            }
        }
        // no record of format 3 holds more than a frame can: its claim is never read
        if self.format >= 3 && len > MAX_BLOB_BYTES {
            return Err(damaged(at, drop_text::ALLOCATION));
        }
        let mut payload: Vec<u8> = Vec::new();
        let mut left = len;
        while left > 0 {
            let n = left.min(ERASE_BUFFER as u64) as usize;
            payload
                .try_reserve(n)
                .map_err(|_| damaged(at, drop_text::ALLOCATION))?;
            let from = payload.len();
            payload.resize(from + n, 0);
            self.exact(&mut payload[from..], at)?;
            left -= n as u64;
        }
        if Hash::of(&payload) != hash {
            return Err(damaged(at, "a record's hash does not match its bytes"));
        }
        self.totals.bytes = at + FRAME_HEADER + len;
        Ok(Record {
            at,
            kind,
            hash,
            payload,
        })
    }

    /// Fills `buf`, or fails with a dump that ends before its trailer (blamed on `at`).
    fn exact(&mut self, buf: &mut [u8], at: u64) -> Result<(), StoreError> {
        read_exact_at(&mut self.r, buf, at)
    }
}

impl StoreReader {
    /// Calls `f` with every sealed batch of `[ORIGIN, upto)`, in log order: its marker's start
    /// and digest, and its data frames byte for byte; a frame whose payload an applied redaction
    /// removed is `Frame::Dropped`, with its apply and residue (`for_each_record_from`). Each
    /// marker is checked against the frames it closes (start, count, digest); a marker that
    /// closes no frames does not match. A mismatch, or frames after the last marker, is
    /// `StoreError::Decode`.
    pub fn for_each_batch(
        &self,
        upto: Location,
        mut f: impl FnMut(DumpBatch) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.for_each_batch_after(BatchMarker::ZERO.start, upto, &mut f)
    }

    /// [`StoreReader::for_each_batch`] of the sealed batches of `[base, end)`: the suffix of the
    /// log after the batch that ends at `base`. The first batch starts at `base`, or at offset
    /// zero of the next segment when the log rolled there.
    ///
    /// Before any batch is visited, `base` and `end` are each checked to be a physical batch end
    /// by [`StoreReader::batch_marker_at`]: their segment is walked from offset zero, frame by
    /// frame. So a place inside a payload, even one whose bytes hold a hash-valid frame and
    /// marker, is never a boundary, and a place inside a frame or a batch is refused. An `end`
    /// before `base` is refused; an `end` equal to it visits nothing. A refusal is
    /// `StoreError::Decode` (or the walk's own error).
    pub fn for_each_batch_after(
        &self,
        base: Location,
        end: Location,
        visit: &mut dyn FnMut(DumpBatch) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if end < base {
            return Err(StoreError::Decode(format!(
                "segment {} offset {} is before segment {} offset {}; run rdy fsck",
                end.segment, end.offset, base.segment, base.offset
            )));
        }
        // offset zero of a segment is a boundary; any other place must close a batch
        for at in [base, end] {
            if at.offset > 0 && self.batch_marker_at(at)?.is_none() {
                return Err(StoreError::Decode(format!(
                    "segment {} offset {} is not a batch end; run rdy fsck",
                    at.segment, at.offset
                )));
            }
        }
        let mut first = true;
        self.walk_batches(base, end, true, &mut |marker, frames| {
            if std::mem::take(&mut first) && !follows(base, marker.start) {
                return Err(StoreError::Decode(format!(
                    "batch at segment {} offset {} does not follow segment {} offset {}; run rdy fsck",
                    marker.start.segment, marker.start.offset, base.segment, base.offset
                )));
            }
            visit(DumpBatch {
                start: marker.start,
                digest: marker.digest,
                frames,
            })
        })
    }

    /// The marker of the batch that ends exactly at `end`: None for offset zero of a segment
    /// (no batch of that segment ends there; the origin is `0.0`). The walk reads `end`'s
    /// segment from offset zero to `end` by the record walker, checking each frame boundary and
    /// each batch marker on the way, and no earlier segment. An `end` inside a frame, or after
    /// frames that no marker closes, is `StoreError::Decode`.
    pub fn batch_marker_at(&self, end: Location) -> Result<Option<BatchMarker>, StoreError> {
        if end.offset == 0 {
            return Ok(None);
        }
        let from = Location {
            segment: end.segment,
            offset: 0,
        };
        let mut last = None;
        self.walk_batches(from, end, false, &mut |marker, _| {
            last = Some(marker);
            Ok(())
        })?;
        Ok(last)
    }

    /// Walks the records of `[from, upto)` and calls `visit` with each batch's checked marker
    /// and, with `keep`, its frames (without, an empty list: only the hashes are kept for the
    /// digest). A marker that does not match the frames it closes, or frames after the last
    /// marker, is `StoreError::Decode`.
    fn walk_batches(
        &self,
        from: Location,
        upto: Location,
        keep: bool,
        visit: &mut dyn FnMut(BatchMarker, Vec<Frame>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut pending: Vec<Frame> = Vec::new();
        let mut hashes: Vec<Hash> = Vec::new();
        let mut first: Option<Location> = None;
        self.for_each_record_from(from, upto, |loc, frame, _next| {
            let (kind, payload) = match frame {
                StoredFrame::Present { kind, payload, .. } => (kind, payload),
                StoredFrame::Dropped {
                    kind,
                    hash,
                    len,
                    apply,
                    residue,
                } => {
                    first.get_or_insert(loc);
                    hashes.push(hash);
                    if keep {
                        pending.push(Frame::Dropped {
                            kind,
                            hash,
                            len,
                            apply,
                            residue,
                        });
                    }
                    return Ok(());
                }
            };
            if kind != kinds::BATCH_COMMIT {
                let StoredFrame::Present { hash, .. } = frame else {
                    unreachable!("a present frame")
                };
                first.get_or_insert(loc);
                hashes.push(hash);
                if keep {
                    pending.push(Frame::Present {
                        kind,
                        hash,
                        payload: payload.to_vec(),
                    });
                }
                return Ok(());
            }
            let start = first.take().unwrap_or(loc);
            let mismatch = || {
                StoreError::Decode(format!(
                    "batch at segment {} offset {} does not match its marker; run rdy fsck",
                    start.segment, start.offset
                ))
            };
            let marker = BatchCommit::decode(payload).ok_or_else(mismatch)?;
            let frames = std::mem::take(&mut pending);
            let digest = kinds::batch_digest(hashes.iter());
            let count = std::mem::take(&mut hashes).len();
            // a marker of no frames closes no batch (a store's open treats one as torn)
            if count == 0
                || marker.start != start
                || marker.frames as usize != count
                || digest != marker.digest
            {
                return Err(mismatch());
            }
            visit(BatchMarker::of_commit(&marker), frames)
        })?;
        if !hashes.is_empty() {
            return Err(StoreError::Decode(format!(
                "the log does not end at a batch marker at segment {} offset {}; run rdy fsck",
                upto.segment, upto.offset
            )));
        }
        Ok(())
    }
}
