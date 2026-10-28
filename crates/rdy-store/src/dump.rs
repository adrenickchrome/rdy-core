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
//! and 23, the carried removal), store format 17 (the pair (3, 17); those of (3, 16) and 24,
//! the change plan) and store format 18 (the pair (3, 18); exactly those of (3, 17), since 18
//! only appends a justification variant). It keeps the magic, the container framing and the `FILE`,
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
//!   An apply's spans are its request's, resolved as the store resolves them: a project's
//!   manifest places, a holder's local copies of the manifest's keys (HA1's logical rule). The
//!   authority a `DROPPED` record names is not the rule, and a hole only a carried record
//!   explains is no span ([`RemovalTally`]).
//!
//! A full dump has base `0.0`, a zero base marker and a zero `prev`. An increment holds at least
//! one batch. The first batch starts at `base`, or at offset zero of the next segment. The last
//! batch ends at the header's end (its start, plus `37 + original_len` per frame, plus the marker
//! frame), and its marker is `end_batch`; a dump of no batches ends at its base with its base
//! marker. The reader also checks `removals` against the dump's applies, and, in a full dump,
//! `first_removed` against the lowest span the file's own records resolve for those applies. An
//! increment after the origin does not hold the log's first batch, so either resolution may be
//! its log's; it may also name a span before its base, which an earlier file holds, and an
//! apply whose request an earlier file holds is resolved by the loader of the chain. Every
//! place is checked arithmetic: one past `u64::MAX` is `dump position overflows`. The chain links between files
//! are checked by the loader of a chain.
//!
//! # Dump format 4
//!
//! Dump format 4 carries a feature store (D-WWAK7): its store is a feature manifest, not a
//! format number. It keeps format 3's magic, container framing, `FILE`, `BATCH` and `TRAILER`
//! layouts and every format-3 rule.
//!
//! - **`HEADER` payload:** format 3's, with `store_format: u32` replaced, in its place, by the
//!   manifest: `features_len: u32` and the manifest's canonical JSON (`crate::format`, at most
//!   65,536 bytes). The reader applies the store's compatibility rules to it: a manifest that is
//!   not valid, or that names an unknown incompatible feature, is refused.
//! - **Frames:** the raw frames, byte for byte, of every kind of the latest store
//!   ([`current_frame_kinds`]). A versioned frame keeps its raw kind (`logical | 0x80`) and its
//!   version prefix (`crate::codec`); its version must be one its kind registers.
//! - **`DROPPED` payload (104 bytes):** `logical_kind: u8 || encoding: u16 || original_hash ||
//!   original_len: u32 || apply_hash || has_residue: u8 || residue_hash`: format 3's proof with
//!   the frame's encoding after its logical kind. The `BATCH` record's digests use the frame's
//!   raw kind. Until a removed versioned record has its residue adapter, the encoding is 0.
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
pub use crate::format::DumpStore;
use crate::format::StoreFeatures;
use crate::holder::{HOLDER_MODE, REF_STORE_MODE, StoreMode};
use crate::kinds::{self, BatchCommit};
use crate::redaction::StoredFrame;
use crate::redaction_io::ERASE_BUFFER;
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::{MAX_BLOB_BYTES, StoreReader};
use rdy_model::{Hash, RepoId};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// A test hook a batch walk calls with the location of each record it has read.
#[doc(hidden)]
pub type RecordHook = Box<dyn FnMut(Location)>;

thread_local! {
    static RECORD_HOOK: std::cell::RefCell<Option<RecordHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: a batch walk on this thread (a dump's, and a batch-marker lookup's) calls `hook`
/// with each record's location, after the walk has read the removal spans and that record and
/// before it reads the next one (a read barrier for race tests). None removes the hook.
#[doc(hidden)]
pub fn on_batch_walk_record(hook: Option<RecordHook>) {
    RECORD_HOOK.with(|h| *h.borrow_mut() = hook);
}

/// The 8 bytes every dump starts with.
pub const MAGIC: &[u8; 8] = b"RDYDUMP\n";
/// The dump format this rdy writes: 4, of a feature store.
pub const DUMP_FORMAT: u32 = 4;
/// The manifest grammars this rdy reads in a dump of `DUMP_FORMAT`.
pub const READS_FEATURES: &[u32] = &[1];
/// The released `(dump_format, store_format)` pairs of scalar stores this rdy reads. A pair, once
/// released, never leaves this list (D-69AY3). No (1, 13) pair exists: store formats 13 to 15
/// travel only in dump format 2, and store formats 16 to 18 only in dump format 3.
pub const READS: &[(u32, u32)] = &[
    (1, 10),
    (1, 11),
    (1, 12),
    (2, 13),
    (2, 14),
    (2, 15),
    (3, 16),
    (3, 17),
    (3, 18),
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
/// The length of a `DROPPED` record's payload in dump formats 2 and 3.
pub const DROPPED_LEN: usize = 1 + 32 + 4 + 32 + 1 + 32;
/// The length of a `DROPPED` record's payload in dump format 4 (and in a sync batch): the
/// encoding after the logical kind.
pub const ENCODED_DROPPED_LEN: usize = DROPPED_LEN + 2;

/// The length of a `DROPPED` record's payload in a dump of `format`.
pub(crate) fn dropped_len(format: u32) -> usize {
    if format >= 4 {
        ENCODED_DROPPED_LEN
    } else {
        DROPPED_LEN
    }
}

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

/// The 104 bytes of the dump-4 (and sync) `DROPPED` record of a dropped frame of logical kind
/// `kind` in encoding `encoding`: its logical kind, its encoding, then the format-3 proof.
pub(crate) fn encoded_dropped_payload(
    kind: u8,
    encoding: u16,
    hash: &Hash,
    len: u32,
    apply: &Hash,
    residue: Option<&Hash>,
) -> [u8; ENCODED_DROPPED_LEN] {
    let legacy = dropped_payload(crate::codec::logical_kind(kind), hash, len, apply, residue);
    let mut p = [0u8; ENCODED_DROPPED_LEN];
    p[0] = legacy[0];
    p[1..3].copy_from_slice(&encoding.to_le_bytes());
    p[3..].copy_from_slice(&legacy[1..]);
    p
}

/// Whether a removed frame of logical kind `kind` may have encoding `encoding`: a change (the
/// one removable kind with versioned encodings) of any encoding CHANGE registers, a block or a
/// blob chunk of encoding 0. Its residue names the same source encoding, which the proof checks.
pub(crate) fn dropped_encoding_allowed(kind: u8, encoding: u16) -> bool {
    encoding == 0 || (kind == kinds::CHANGE && encoding <= crate::codec::latest(kinds::CHANGE))
}

/// The encoding of a dropped frame of raw kind `raw`: 0 for an unmarked one; for a marked
/// change, CHANGE's latest encoding (the one versioned encoding a removable kind registers).
/// Any other marked kind is refused as an unknown encoding version.
pub(crate) fn dropped_encoding(raw: u8) -> Result<u16, Cow<'static, str>> {
    if raw & crate::codec::VERSIONED == 0 {
        return Ok(0);
    }
    let kind = crate::codec::logical_kind(raw);
    let encoding = crate::codec::latest(kind);
    if kind != kinds::CHANGE || encoding == 0 {
        return Err(crate::bounded::RecordError::UnknownVersion
            .to_string()
            .into());
    }
    Ok(encoding)
}

/// The rule of a dropped frame's encoding: a marked raw kind must be a versioned removable kind
/// ([`dropped_encoding`]).
pub(crate) fn check_encoded(kind: u8) -> Result<(), Cow<'static, str>> {
    dropped_encoding(kind).map(|_| ())
}

/// Decodes the payload of a dump-4 (or sync) `DROPPED` record: exactly 104 bytes, an encoding
/// its kind may have when removed ([`dropped_encoding_allowed`]), then the rules of
/// [`parse_dropped`]. The frame's kind is its raw kind: the logical kind, marked when the
/// encoding is not 0.
pub(crate) fn parse_encoded_dropped(p: &[u8]) -> Result<Frame, Cow<'static, str>> {
    if p.len() != ENCODED_DROPPED_LEN {
        return Err(drop_text::WRONG_LENGTH.into());
    }
    let encoding = u16::from_le_bytes([p[1], p[2]]);
    if !dropped_encoding_allowed(p[0], encoding) {
        return Err(crate::bounded::RecordError::UnknownVersion
            .to_string()
            .into());
    }
    let mut legacy = [0u8; DROPPED_LEN];
    legacy[0] = p[0];
    legacy[1..].copy_from_slice(&p[3..]);
    match parse_dropped(&legacy)? {
        Frame::Dropped {
            kind,
            hash,
            len,
            apply,
            residue,
        } => Ok(Frame::Dropped {
            kind: crate::codec::raw_kind(kind, encoding),
            hash,
            len,
            apply,
            residue,
        }),
        f @ Frame::Present { .. } => Ok(f),
    }
}

/// The `DROPPED` record parser of a dump of `format`.
pub(crate) fn parse_dropped_of(format: u32, p: &[u8]) -> Result<Frame, Cow<'static, str>> {
    if format >= 4 {
        parse_encoded_dropped(p)
    } else {
        parse_dropped(p)
    }
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
pub(crate) fn drops_allowed(dump_format: u32) -> bool {
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

/// The dump's header: the store of its frames (a scalar format up to dump format 3, a feature
/// manifest in dump format 4), the source log's end, the writing rdy's version (for people
/// only), and the chain facts of dump formats 3 and 4 (None for formats 1 and 2, which have
/// none). A writer needs them: it writes dump format 4 only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpHeader {
    pub store: DumpStore,
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
/// checked source walker hands the writer or the reader reads. Every `REDACTION_APPLY` frame
/// counts (each physical one). The first removed span is the lowest resolved local span of those
/// applies: each apply names its request, and the request's spans are resolved as the store
/// resolves them (`crate::redaction_io`, `crate::redaction_import`):
///
/// - in a project, the request's manifest places, which are places of this log;
/// - in a holder (its first batch sets `meta/store_mode`), every dropped frame of a key the
///   manifest lists (HA1's logical rule): a source place is never read, and a span covered by
///   an apply counts for every request that approves its key, whichever proof its `DROPPED`
///   record names (the earliest).
///
/// So a hole that only a carried record explains is no span, and the authority a `DROPPED`
/// record names is not the rule. [`RemovalTally::facts`] resolves from the batches alone (the
/// reader's view of a file); [`RemovalTally::resolved`] asks the store's own authorization (the
/// writer's). Growth is fallible.
#[derive(Clone, Debug, Default)]
pub struct RemovalTally {
    /// The `REDACTION_APPLY` frames seen.
    applies: u64,
    /// The requests those applies name, once each (an apply that does not decode names none).
    requests: HashSet<Hash>,
    /// Whether an apply frame did not decode.
    undecoded: bool,
    /// The manifest spans of each `REDACTION` frame seen that decodes: place, kind and hash.
    manifests: HashMap<Hash, Vec<(Location, u8, Hash)>>,
    /// The lowest place of each key (kind, hash) of a dropped frame.
    dropped: HashMap<(u8, Hash), Location>,
    /// The mode the first batch sets, when the first batch seen starts at the origin.
    mode: Option<StoreMode>,
    /// Whether a batch was seen.
    seen: bool,
}

impl RemovalTally {
    /// Adds batch `b`. Refuses a place past `u64::MAX` (`dump position overflows`) and a refused
    /// growth (`record allocation failed`), as `StoreError::Refused`.
    pub fn observe(&mut self, b: &DumpBatch) -> Result<(), StoreError> {
        self.add(b).map_err(|what| StoreError::Refused(what.into()))
    }

    /// The facts of the batches added so far, resolved from them alone: in a holder by HA1's
    /// logical rule, otherwise by the manifest places. Refuses more than `u32::MAX` applies
    /// (`dump position overflows`).
    pub fn facts(&self) -> Result<RemovalFacts, StoreError> {
        self.with_first(self.first_removed())
    }

    /// The first removed span the batches alone resolve.
    fn first_removed(&self) -> Option<Location> {
        match self.mode {
            Some(StoreMode::Holder) => self.holder_first(),
            _ => self.project_first(),
        }
    }

    /// The facts of the batches added so far, with each apply's spans resolved by the store
    /// `reader` reads, before `upto`: the request's spans in the store's redaction rows, which
    /// the open checks against the log's validated authorization. A project's are its manifest
    /// places; a holder's are the local copies HA1's logical rule derives
    /// (`crate::redaction_import`), never the source's places. This is the writer's view;
    /// [`RemovalTally::facts`] reads the same facts back from a file.
    pub fn resolved(
        &self,
        reader: &StoreReader,
        upto: Location,
    ) -> Result<RemovalFacts, StoreError> {
        let mut first: Option<Location> = None;
        for request in &self.requests {
            let (_, spans) = reader.index().redaction_request_items(request)?;
            for p in spans.iter().map(|s| s.loc).filter(|p| *p < upto) {
                first = Some(first.map_or(p, |f| f.min(p)));
            }
        }
        self.with_first(first)
    }

    fn with_first(&self, first_removed: Option<Location>) -> Result<RemovalFacts, StoreError> {
        let removals = u32::try_from(self.applies)
            .map_err(|_| StoreError::Refused(chain_text::OVERFLOW.into()))?;
        Ok(RemovalFacts {
            removals,
            first_removed,
        })
    }

    /// The `REDACTION_APPLY` frames added so far.
    fn applies(&self) -> u64 {
        self.applies
    }

    /// The mode the batches set, when they start at the origin.
    fn mode(&self) -> Option<StoreMode> {
        self.mode
    }

    /// Whether every apply added so far names a request whose record was added too: only then
    /// do the batches alone resolve its spans.
    fn resolves_all(&self) -> bool {
        !self.undecoded && self.requests.iter().all(|r| self.manifests.contains_key(r))
    }

    /// The spans of the applies' requests that the batches hold.
    fn manifest_spans(&self) -> impl Iterator<Item = &(Location, u8, Hash)> {
        self.requests
            .iter()
            .filter_map(|r| self.manifests.get(r))
            .flatten()
    }

    /// The lowest manifest place of the applies' requests (a project's resolution).
    fn project_first(&self) -> Option<Location> {
        self.manifest_spans().map(|(at, _, _)| *at).min()
    }

    /// The lowest dropped frame of a key the applies' requests list (a holder's resolution).
    fn holder_first(&self) -> Option<Location> {
        self.manifest_spans()
            .filter_map(|(_, kind, hash)| self.dropped.get(&(*kind, *hash)).copied())
            .min()
    }

    /// [`RemovalTally::observe`], with the refusal's text only. Every allocation is fallible
    /// (the reader's rule): the records it needs are read in place ([`wire`]), never decoded
    /// into owned values.
    fn add(&mut self, b: &DumpBatch) -> Result<(), &'static str> {
        let first = !self.seen;
        self.seen = true;
        let origin = b.start.segment == 0 && b.start.offset == 0;
        // the first batch of the log sets its mode (as `crate::holder::FirstBatch` reads it)
        let mut mode = (first && origin).then_some(StoreMode::Project);
        let mut at = b.start.offset;
        for f in &b.frames {
            let place = Location {
                segment: b.start.segment,
                offset: at,
            };
            match f {
                Frame::Present { kind, payload, .. } if *kind == kinds::REF_UPDATE => {
                    if mode.is_some() && wire::ref_name(payload) == Some(REF_STORE_MODE.as_bytes())
                    {
                        // a mode value other than `holder` is damage the load refuses; here the
                        // mode stays unknown
                        mode = match wire::ref_value(payload) {
                            Some(v) if v == HOLDER_MODE => Some(StoreMode::Holder),
                            _ => None,
                        };
                    }
                }
                Frame::Present { kind, payload, .. } if *kind == kinds::REDACTION_APPLY => {
                    self.applies = self.applies.saturating_add(1);
                    match wire::apply_request(payload) {
                        Some(request) => {
                            self.requests
                                .try_reserve(1)
                                .map_err(|_| drop_text::ALLOCATION)?;
                            self.requests.insert(request);
                        }
                        None => self.undecoded = true,
                    }
                }
                Frame::Present {
                    kind,
                    hash,
                    payload,
                } if *kind == kinds::REDACTION && !self.manifests.contains_key(hash) => {
                    let mut spans = Vec::new();
                    if wire::request_spans(payload, &mut spans)? {
                        self.manifests
                            .try_reserve(1)
                            .map_err(|_| drop_text::ALLOCATION)?;
                        self.manifests.insert(*hash, spans);
                    }
                }
                Frame::Present { .. } => {}
                Frame::Dropped { kind, hash, .. } => match self.dropped.get_mut(&(*kind, *hash)) {
                    Some(low) => *low = (*low).min(place),
                    None => {
                        self.dropped
                            .try_reserve(1)
                            .map_err(|_| drop_text::ALLOCATION)?;
                        self.dropped.insert((*kind, *hash), place);
                    }
                },
            }
            at = at
                .checked_add(FRAME_HEADER)
                .and_then(|a| a.checked_add(u64::from(f.original_len())))
                .ok_or(chain_text::OVERFLOW)?;
        }
        if first && origin {
            self.mode = mode;
        }
        Ok(())
    }
}

/// The few fields the removal tally reads from records, read in place from their postcard
/// bytes (D-F0904: no existing kind is ever re-encoded, so these layouts are frozen), with no
/// allocation: a dump reader's every allocation is fallible. A record these readers cannot
/// read names nothing; whether it is a valid record is the load's check.
mod wire {
    use super::drop_text;
    use crate::segment::Location;
    use rdy_model::Hash;

    /// A cursor over postcard bytes.
    struct Cur<'a>(&'a [u8]);

    impl<'a> Cur<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            if self.0.len() < n {
                return None;
            }
            let (head, rest) = self.0.split_at(n);
            self.0 = rest;
            Some(head)
        }

        fn byte(&mut self) -> Option<u8> {
            self.take(1).map(|b| b[0])
        }

        /// An unsigned LEB128 varint of at most `max` bytes.
        fn varint(&mut self, max: usize) -> Option<u64> {
            let mut v = 0u64;
            for i in 0..max {
                let b = self.byte()?;
                v |= u64::from(b & 0x7f) << (7 * i);
                if b & 0x80 == 0 {
                    return Some(v);
                }
            }
            None
        }

        fn u32(&mut self) -> Option<u32> {
            u32::try_from(self.varint(5)?).ok()
        }

        fn u64(&mut self) -> Option<u64> {
            self.varint(10)
        }

        fn len(&mut self) -> Option<usize> {
            usize::try_from(self.varint(10)?).ok()
        }

        fn hash(&mut self) -> Option<Hash> {
            Some(Hash(self.take(32)?.try_into().ok()?))
        }

        /// A string or byte string: its length, then its bytes.
        fn bytes(&mut self) -> Option<&'a [u8]> {
            let n = self.len()?;
            self.take(n)
        }

        fn option<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<Option<T>> {
            match self.byte()? {
                0 => Some(None),
                1 => f(self).map(Some),
                _ => None,
            }
        }

        /// A sequence, each element read by `f`. Its count is the record's own: the bounds of an
        /// existing record are the checks that wrote it, never the sync decoder's smaller ones.
        /// Every element reads at least one byte, so a count the bytes cannot hold ends at the
        /// end of the payload.
        fn seq(&mut self, mut f: impl FnMut(&mut Self) -> Option<()>) -> Option<usize> {
            let n = self.len()?;
            for _ in 0..n {
                f(self)?;
            }
            Some(n)
        }
    }

    /// The request a `REDACTION_APPLY` record names: its first field of three hashes.
    pub(super) fn apply_request(payload: &[u8]) -> Option<Hash> {
        if payload.len() != 96 {
            return None;
        }
        Cur(payload).hash()
    }

    /// The name of a `REF_UPDATE` record (`name`, `value`, `at`).
    pub(super) fn ref_name(payload: &[u8]) -> Option<&[u8]> {
        Cur(payload).bytes()
    }

    /// The value of a `REF_UPDATE` record.
    pub(super) fn ref_value(payload: &[u8]) -> Option<&[u8]> {
        let mut c = Cur(payload);
        c.bytes()?;
        c.bytes()
    }

    /// The manifest spans of a `REDACTION` record, as place, kind and hash, into `out`
    /// (fallibly grown). Ok(false) when the record cannot be read; a refused growth is
    /// `record allocation failed`. The layout: `carrier`, `actor` (`actor`, `run`, `role`,
    /// `at_ms`), `targets`, `reason_class`, `reason`, then the manifest's `versions`, `blobs`,
    /// `shared_blobs` and `spans` (each `place`, `kind`, `hash`, `len`, `residue`); the fields
    /// after `spans` are not read.
    pub(super) fn request_spans(
        payload: &[u8],
        out: &mut Vec<(Location, u8, Hash)>,
    ) -> Result<bool, &'static str> {
        let mut c = Cur(payload);
        let Some(()) = head(&mut c) else {
            return Ok(false);
        };
        // no count bound but the payload's own (the existing record limits): a count the
        // remaining bytes cannot hold is a record that cannot be read, so the reservation never
        // passes what the payload holds
        let Some(n) = c.len().filter(|n| *n <= c.0.len() / MIN_SPAN_BYTES) else {
            return Ok(false);
        };
        out.try_reserve_exact(n)
            .map_err(|_| drop_text::ALLOCATION)?;
        for _ in 0..n {
            let span = (|| {
                let place = Location {
                    segment: c.u32()?,
                    offset: c.u64()?,
                };
                let kind = c.byte()?;
                let hash = c.hash()?;
                c.u32()?;
                c.option(|c| c.hash())?;
                Some((place, kind, hash))
            })();
            match span {
                Some(s) => out.push(s),
                None => return Ok(false),
            }
        }
        Ok(true)
    }

    /// The fewest bytes one encoded manifest span takes: one-byte varint segment, offset and
    /// length, the kind, the 32-byte hash and an absent residue.
    const MIN_SPAN_BYTES: usize = 1 + 1 + 1 + 32 + 1 + 1;

    /// Reads a request's fields before its manifest's spans.
    fn head(c: &mut Cur) -> Option<()> {
        c.hash()?; // carrier
        c.bytes()?; // actor.actor
        c.option(|c| c.bytes())?; // actor.run
        if c.u32()? > 3 {
            return None; // actor.role
        }
        c.u64()?; // actor.at_ms (zigzag)
        c.seq(|c| match c.u32()? {
            0 => c.bytes().map(|_| ()),
            1 => {
                c.bytes()?;
                c.hash().map(|_| ())
            }
            2 => c.hash().map(|_| ()),
            _ => None,
        })?; // targets
        if c.u32()? > 3 {
            return None; // reason_class
        }
        c.bytes()?; // reason
        c.seq(|c| {
            c.bytes()?;
            c.hash().map(|_| ())
        })?; // manifest.versions
        c.seq(|c| c.hash().map(|_| ()))?; // manifest.blobs
        c.seq(|c| c.hash().map(|_| ()))?; // manifest.shared_blobs
        Some(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rdy_model::redaction::{
            AssessmentSupport, DropSpan, FramePlace, ReasonClass, RedactTarget, Redaction,
            RedactionApply, RedactionManifest,
        };
        use rdy_model::{BlockId, Provenance, Role};

        /// hc1: the in-place reads agree with the records' own decoding, for every variant and
        /// option the read layout walks, and a cut record reads as nothing.
        #[test]
        fn hc1_in_place_reads_match_the_decoded_records() {
            let id = BlockId::parse("b-00001").unwrap();
            let spans = vec![
                DropSpan {
                    place: FramePlace {
                        segment: 3,
                        offset: u64::MAX - 1,
                    },
                    kind: 1,
                    hash: Hash([7; 32]),
                    len: u32::MAX,
                    residue: Some(Hash([8; 32])),
                },
                DropSpan {
                    place: FramePlace {
                        segment: u32::MAX,
                        offset: 0,
                    },
                    kind: 6,
                    hash: Hash([9; 32]),
                    len: 300,
                    residue: None,
                },
            ];
            let r = Redaction {
                carrier: Hash([1; 32]),
                actor: Provenance {
                    actor: "agent:w".repeat(40),
                    run: Some("run-1".into()),
                    role: Role::Approver,
                    at_ms: -1_234_567_890_123,
                },
                targets: vec![
                    RedactTarget::AllVersions(id.clone()),
                    RedactTarget::Version {
                        id: id.clone(),
                        version: Hash([2; 32]),
                    },
                    RedactTarget::Blob(Hash([3; 32])),
                ],
                reason_class: ReasonClass::Retraction,
                reason: "why ".repeat(100),
                manifest: RedactionManifest {
                    versions: vec![(id.clone(), Hash([2; 32]))],
                    blobs: vec![Hash([3; 32])],
                    shared_blobs: vec![Hash([4; 32]), Hash([5; 32])],
                    spans: spans.clone(),
                    inside: vec![],
                    outside: vec![],
                    quarantine: vec!["q".into()],
                    assessment: AssessmentSupport::NotImplemented,
                },
            };
            let bytes = postcard::to_allocvec(&r).unwrap();
            let mut out = vec![];
            assert_eq!(request_spans(&bytes, &mut out), Ok(true));
            let want: Vec<(Location, u8, Hash)> = spans
                .iter()
                .map(|s| {
                    (
                        Location {
                            segment: s.place.segment,
                            offset: s.place.offset,
                        },
                        s.kind,
                        s.hash,
                    )
                })
                .collect();
            assert_eq!(out, want);
            // a span count the remaining bytes cannot hold reads as nothing, and reserves nothing
            let mut c = Cur(&bytes);
            head(&mut c).unwrap();
            let at = bytes.len() - c.0.len();
            assert_eq!(bytes[at], 2, "the span count");
            let mut forged = bytes[..at].to_vec();
            forged.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]); // 2^35
            forged.extend_from_slice(&bytes[at + 1..]);
            let mut out = vec![];
            assert_eq!(request_spans(&forged, &mut out), Ok(false));
            assert_eq!(out.capacity(), 0);
            // a record cut inside its spans reads as nothing
            let spans_at = bytes.len() - 6;
            let mut out = vec![];
            assert_eq!(request_spans(&bytes[..spans_at / 2], &mut out), Ok(false));
            let apply = RedactionApply {
                request: Hash([6; 32]),
                landing: Hash([2; 32]),
                report_hash: Hash([3; 32]),
            };
            let a = postcard::to_allocvec(&apply).unwrap();
            assert_eq!(apply_request(&a), Some(Hash([6; 32])));
            assert_eq!(apply_request(&a[..95]), None);
            let u = postcard::to_allocvec(&crate::kinds::RefUpdate {
                name: "meta/store_mode".into(),
                value: b"holder".to_vec(),
                at: Location {
                    segment: 0,
                    offset: 0,
                },
            })
            .unwrap();
            assert_eq!(ref_name(&u), Some(&b"meta/store_mode"[..]));
            assert_eq!(ref_value(&u), Some(&b"holder"[..]));
        }
    }
}

/// The longest pack file name a dump carries, in bytes, its `.yaml` or `.yml` extension counted.
pub const MAX_PACK_FILE_NAME_BYTES: usize = 255;

/// True if `path` is one of the project files a dump carries (the closed set): `.rdy.yaml`,
/// `.rdyignore`, or `.rdy/packs/<name>` with a plain YAML file name of at most
/// `MAX_PACK_FILE_NAME_BYTES` bytes.
pub fn is_project_file(path: &str) -> bool {
    if path == ".rdy.yaml" || path == ".rdyignore" {
        return true;
    }
    let Some(name) = path.strip_prefix(".rdy/packs/") else {
        return false;
    };
    name.len() <= MAX_PACK_FILE_NAME_BYTES
        && !name.starts_with('.')
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
    // store format 17 adds the change plan
    (
        (3, 17),
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
            kinds::PLAN,
        ],
    ),
    // store format 18 appends a justification variant: the kinds of (3, 17), unchanged
    (
        (3, 18),
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
            kinds::PLAN,
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

/// The logical kinds of the latest store: every assigned kind but the batch marker, the kinds a
/// dump-4 frame may have (a versioned frame's logical kind among them). Ascending: the kinds of
/// store format 18 and the four carried kinds.
pub const CURRENT_FRAME_KINDS: [u8; 26] = [
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
    kinds::PLAN,
    kinds::ASSESSMENT,
    kinds::DISPUTE,
    kinds::DISPUTE_RESOLUTION,
    kinds::CHECK_REQUEST,
];

/// [`CURRENT_FRAME_KINDS`].
pub fn current_frame_kinds() -> &'static [u8] {
    &CURRENT_FRAME_KINDS
}

/// The raw kinds a dump's batch frames may have: a released pair's exact set, or the latest
/// store's kinds, where a marked raw kind needs a kind that registers versioned encodings.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameKinds {
    Legacy(&'static [u8]),
    Current,
}

impl FrameKinds {
    /// The frame kinds of a dump of `format` with header `header`; None for a pair this rdy does
    /// not read.
    pub(crate) fn of(format: u32, store: &DumpStore) -> Option<FrameKinds> {
        match store {
            DumpStore::Legacy(n) if READS.contains(&(format, *n)) => {
                frame_kinds(format, *n).map(FrameKinds::Legacy)
            }
            DumpStore::Features(_) if format == DUMP_FORMAT => Some(FrameKinds::Current),
            _ => None,
        }
    }

    /// Whether a frame of raw kind `raw` may stand in a batch.
    pub(crate) fn allows(self, raw: u8) -> bool {
        match self {
            FrameKinds::Legacy(k) => k.contains(&raw),
            FrameKinds::Current => {
                let kind = crate::codec::logical_kind(raw);
                current_frame_kinds().contains(&kind)
                    && (raw == kind || crate::codec::latest(kind) > 0)
            }
        }
    }

    /// The encoding rule of a present frame: in a dump of the latest store, its raw kind and
    /// payload split (`crate::codec`). Err is the codec's refusal.
    pub(crate) fn check_payload(self, raw: u8, payload: &[u8]) -> Result<(), String> {
        match self {
            FrameKinds::Legacy(_) => Ok(()),
            FrameKinds::Current => crate::codec::split(raw, payload)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }
}

/// The `BATCH` record's `kinds`: `Hash::of` over each frame's (original) `kind || hash`, in
/// order. None when its buffer cannot be allocated.
fn kinds_digest(frames: &[Frame]) -> Option<Hash> {
    try_kinds_digest(frames.iter().map(|f| (f.kind(), f.hash())))
}

/// The `BATCH` record's `kinds` of the frames whose (original) kind and hash `pairs` gives, in
/// order (shared with the chain scanner, `crate::dump_scan`). None when its buffer cannot be
/// allocated.
pub(crate) fn try_kinds_digest<'a>(
    pairs: impl ExactSizeIterator<Item = (u8, &'a Hash)>,
) -> Option<Hash> {
    let mut b = Vec::new();
    b.try_reserve_exact(pairs.len().checked_mul(33)?).ok()?;
    for (kind, hash) in pairs {
        b.push(kind);
        b.extend_from_slice(&hash.0);
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

/// The name a `REF_UPDATE` payload starts with (its first field), when `payload` holds all of
/// it: the chain scanner reads this much of every ref before it decides to read the rest.
pub(crate) fn ref_name(payload: &[u8]) -> Option<&[u8]> {
    wire::ref_name(payload)
}

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

pub(crate) fn cannot_stand(kind: u8) -> String {
    format!("a record of kind {kind} cannot stand here")
}

/// The bounded decode of a carried removal record (kind 23), the only kind of a dump frame that
/// is decoded on the way: the decoder's own error when it refuses one. Other kinds pass. The
/// check decodes the record in place (`redaction_carried::check_record`), so it allocates
/// nothing, as the reader's other checks allocate only by fallible growth.
pub(crate) fn check_carried(kind: u8, payload: &[u8]) -> Result<(), String> {
    if kind != kinds::REDACTION_CARRIED {
        return Ok(());
    }
    crate::redaction_carried::check_record(payload).map_err(|e| e.to_string())
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

/// The format-4 header payload of `h` with its chain facts `c` and manifest `features`.
fn header_payload(h: &DumpHeader, c: &DumpChainHeader, features: &StoreFeatures) -> Vec<u8> {
    let json = features.to_json();
    let mut p = Vec::with_capacity(HEADER_FIXED + json.len() + h.rdy.len());
    p.push(c.kind.byte());
    p.extend_from_slice(&(json.len() as u32).to_le_bytes());
    p.extend_from_slice(&json);
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
/// text cannot be allocated, its manifest is not valid, or a full dump's base facts are not the
/// origin's).
fn parse_header(format: u32, p: &[u8]) -> Result<DumpHeader, Cow<'static, str>> {
    let mut c = Cursor(p);
    let mut manifest: Option<crate::format::ParseFailure> = None;
    let fields = (|| {
        let (kind, store) = if format >= 4 {
            let kind = DumpKind::of_byte(c.u8()?)?;
            let len = c.u32()? as usize;
            if len > crate::format::MAX_MANIFEST_BYTES {
                manifest = Some(crate::format::ParseFailure::Manifest(
                    crate::format::ManifestError("manifest exceeds 65536 bytes".into()),
                ));
                return None;
            }
            let json = c.take(len)?;
            // every allocation of the reader is fallible: the manifest's too
            match StoreFeatures::parse_fallible(json) {
                Ok(m) => (Some(kind), DumpStore::Features(m)),
                Err(e) => {
                    manifest = Some(e);
                    return None;
                }
            }
        } else if format >= 3 {
            (
                Some(DumpKind::of_byte(c.u8()?)?),
                DumpStore::Legacy(c.u32()?),
            )
        } else {
            (None, DumpStore::Legacy(c.u32()?))
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
        c.0.is_empty().then_some((store, log_end, rdy, chain))
    })();
    match manifest {
        Some(crate::format::ParseFailure::Manifest(e)) => {
            return Err(format!("store feature manifest is invalid: {e}").into());
        }
        Some(crate::format::ParseFailure::Allocation) => return Err(drop_text::ALLOCATION.into()),
        None => {}
    }
    let (store, log_end, rdy, chain) = fields.ok_or(chain_text::HEADER)?;
    if chain.is_some_and(|c| c.kind == DumpKind::Full && !c.has_origin_base()) {
        return Err(chain_text::FULL_BASE.into());
    }
    Ok(DumpHeader {
        store,
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
pub(crate) fn parse_trailer(format: u32, p: &[u8]) -> Option<DumpTrailer> {
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
    /// The store kinds a frame may have.
    kinds: FrameKinds,
    /// `kind || len || hash` of every record so far (the trailer's digest covers them).
    chain: Vec<u8>,
    totals: DumpTotals,
}

impl<W: Write> DumpWriter<W> {
    /// Writes the prelude and the header, always of `DUMP_FORMAT` (there is no writer of an
    /// older format). Refuses a header with no chain facts or an `rdy` text over 256 bytes (the
    /// header does not parse), a header of a scalar store (no dump of `DUMP_FORMAT` holds one),
    /// and a manifest the reader would refuse (an invalid one, or one with an unknown
    /// incompatible feature). The chain facts are the caller's: the reader checks them against
    /// the batches.
    pub fn new(w: W, header: &DumpHeader) -> Result<Self, StoreError> {
        let Some(chain) = header
            .chain
            .as_ref()
            .filter(|_| header.rdy.len() <= MAX_RDY_BYTES)
        else {
            return Err(StoreError::Refused(chain_text::HEADER.into()));
        };
        let features = match &header.store {
            DumpStore::Features(m) => m,
            DumpStore::Legacy(n) => {
                return Err(StoreError::Refused(store_format_not_read(*n, DUMP_FORMAT)));
            }
        };
        crate::format::check_features(features)?;
        let kinds = FrameKinds::Current;
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
        me.record(rec::HEADER, &header_payload(header, chain, features))?;
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
            if !self.kinds.allows(f.kind()) {
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
                    self.kinds
                        .check_payload(*kind, payload)
                        .map_err(StoreError::Refused)?;
                }
                Frame::Dropped {
                    kind, residue, len, ..
                } => {
                    if !drops_allowed(DUMP_FORMAT) {
                        return Err(StoreError::Refused(drop_text::NOT_ALLOWED.into()));
                    }
                    check_encoded(*kind).map_err(|what| StoreError::Refused(what.into()))?;
                    check_dropped(crate::codec::logical_kind(*kind), residue.as_ref(), *len)
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
                    let encoding =
                        dropped_encoding(*kind).map_err(|w| StoreError::Refused(w.into()))?;
                    let p = encoded_dropped_payload(
                        crate::codec::logical_kind(*kind),
                        encoding,
                        hash,
                        *len,
                        apply,
                        residue.as_ref(),
                    );
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
    /// The store kinds a frame may have.
    kinds: FrameKinds,
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

pub(crate) fn damaged(at: u64, what: impl Into<String>) -> StoreError {
    StoreError::Dump {
        at,
        what: what.into(),
    }
}

pub(crate) const ENDS_EARLY: &str = "the file ends before its trailer";

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
pub(crate) const PRELUDE_LEN: u64 = MAGIC.len() as u64 + 4;

/// Reads the prelude off `r`: the magic, then a dump format this rdy reads (some pair of
/// `READS`). A file that does not start with the magic is not a dump.
pub(crate) fn read_prelude(r: &mut impl Read) -> Result<u32, StoreError> {
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
    if format != DUMP_FORMAT && !READS.iter().any(|(d, _)| *d == format) {
        return Err(damaged(MAGIC.len() as u64, dump_format_not_read(format)));
    }
    Ok(format)
}

/// The dump format the prelude at the start of `r` names, read alone (the magic and the format,
/// as the reader checks them): which reader a file needs, before any other byte is read.
pub fn dump_format_of(r: &mut impl Read) -> Result<u32, StoreError> {
    read_prelude(r)
}

/// Fills `buf` from `r`, or fails with a dump that ends before its trailer (blamed on `at`).
pub(crate) fn read_exact_at(r: &mut impl Read, buf: &mut [u8], at: u64) -> Result<(), StoreError> {
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(damaged(at, ENDS_EARLY)),
        Err(e) => Err(e.into()),
    }
}

/// The header of a dump of `format` from the `HEADER` payload `p` read at `at`, and the store
/// kinds its batches may hold. Refuses a payload that does not parse and a pair outside `READS`.
pub(crate) fn header_of(
    format: u32,
    at: u64,
    p: &[u8],
) -> Result<(DumpHeader, FrameKinds), StoreError> {
    let header = parse_header(format, p).map_err(|what| damaged(at, what))?;
    let Some(kinds) = FrameKinds::of(format, &header.store) else {
        let n = match &header.store {
            DumpStore::Legacy(n) => *n,
            DumpStore::Features(_) => 0,
        };
        return Err(damaged(at, store_format_not_read(n, format)));
    };
    // the store's compatibility rules: an unknown incompatible feature refuses the dump
    if let DumpStore::Features(m) = &header.store {
        crate::format::check_features(m)?;
    }
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
    let fixed = if format >= 4 {
        HEADER_FIXED + crate::format::MAX_MANIFEST_BYTES
    } else if format >= 3 {
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

/// The head of one record of a dump: where it starts, its payload length, its kind and the hash
/// its header names (not yet checked against the payload).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RecordHead {
    pub(crate) at: u64,
    pub(crate) len: u64,
    pub(crate) kind: u8,
    pub(crate) hash: Hash,
}

impl RecordHead {
    /// The byte offset just past the record.
    pub(crate) fn end(&self) -> u64 {
        self.at + FRAME_HEADER + self.len
    }
}

/// The text of a refused trailer (counts or digest).
pub(crate) const TRAILER_MISMATCH: &str = "the trailer's counts or digest do not match the dump";
/// The text of a file with bytes after its trailer.
pub(crate) const BYTES_AFTER: &str = "bytes follow the trailer";
/// The text of a record whose payload does not match its hash.
pub(crate) const HASH_MISMATCH: &str = "a record's hash does not match its bytes";

/// Reads the head of the record at `at` off `r` of a dump of `format`, and checks it by its
/// header alone (shared by the reader and the chain scanner, `crate::dump_scan`). The end of the
/// file inside the head is a dump that ends before its trailer. `frame` says a batch's data frame
/// is expected: there a `DROPPED` record is refused in a dump format without one, or with another
/// length than 102 bytes. From dump format 3 on, a record that claims more than `MAX_BLOB_BYTES`
/// is refused (`record allocation failed`): its claim is never read.
pub(crate) fn read_head(
    r: &mut impl Read,
    at: u64,
    format: u32,
    frame: bool,
) -> Result<RecordHead, StoreError> {
    let mut head = [0u8; FRAME_HEADER as usize];
    read_exact_at(r, &mut head, at)?;
    let len = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes")) as u64;
    let kind = head[4];
    let hash = Hash(head[5..37].try_into().expect("32 bytes"));
    if frame && kind == rec::DROPPED {
        if !drops_allowed(format) {
            return Err(damaged(at, drop_text::NOT_ALLOWED));
        }
        if len != dropped_len(format) as u64 {
            return Err(damaged(at, drop_text::WRONG_LENGTH));
        }
    }
    if format >= 3 && len > MAX_BLOB_BYTES {
        return Err(damaged(at, drop_text::ALLOCATION));
    }
    Ok(RecordHead {
        at,
        len,
        kind,
        hash,
    })
}

/// Reads the payload of the record whose head is `head` off `r`, which stands just past the head,
/// and checks it against the head's hash. The payload is read in chunks of at most
/// `ERASE_BUFFER` bytes, each taken by fallible growth: a length the head claims is never
/// reserved before its bytes arrive.
pub(crate) fn read_body(r: &mut impl Read, head: &RecordHead) -> Result<Vec<u8>, StoreError> {
    let mut payload: Vec<u8> = Vec::new();
    let mut left = head.len;
    while left > 0 {
        let n = left.min(ERASE_BUFFER as u64) as usize;
        payload
            .try_reserve(n)
            .map_err(|_| damaged(head.at, drop_text::ALLOCATION))?;
        let from = payload.len();
        payload.resize(from + n, 0);
        read_exact_at(r, &mut payload[from..], head.at)?;
        left -= n as u64;
    }
    if Hash::of(&payload) != head.hash {
        return Err(damaged(head.at, HASH_MISMATCH));
    }
    Ok(payload)
}

/// The fields of a `BATCH` record's payload: its start, its frame count (at least 1: no store
/// writes a batch of no frames), its digest and its kinds digest. None when it does not parse.
pub(crate) fn parse_batch_record(p: &[u8]) -> Option<(Location, u32, Hash, Hash)> {
    let mut c = Cursor(p);
    let segment = c.u32()?;
    let offset = c.u64()?;
    let frames = c.u32()?;
    let digest = c.hash()?;
    let kinds = c.hash()?;
    (c.0.is_empty() && frames > 0).then_some((Location { segment, offset }, frames, digest, kinds))
}

/// Refuses the head of the `have`-th data frame of batch `n`, whose record says `count`: a
/// container record where a data frame is expected.
pub(crate) fn check_frame_head(
    n: u64,
    have: usize,
    count: u32,
    head: &RecordHead,
) -> Result<(), StoreError> {
    if head.kind != rec::DROPPED && head.kind >= rec::HEADER {
        return Err(damaged(
            head.at,
            format!("batch {n} holds {have} frames, and its record says {count}"),
        ));
    }
    Ok(())
}

/// The path of the `FILE` record read at `at`, whose payload is `payload_len` bytes long and
/// starts with `head` (the whole payload, or at least its length field and path), checked as the
/// reader checks it: a path of the closed set, after `last_path`, at most `MAX_FILES` files
/// (`files` before it), and a file of at most 16 MiB.
pub(crate) fn check_file_head<'a>(
    at: u64,
    head: &'a [u8],
    payload_len: u64,
    last_path: Option<&str>,
    files: u32,
) -> Result<&'a str, StoreError> {
    let mut c = Cursor(head);
    let Some(path_len) = c.u16() else {
        return Err(damaged(at, not_carried("")));
    };
    let Some(raw) = c.take(path_len as usize) else {
        return Err(damaged(at, not_carried(&String::from_utf8_lossy(c.0))));
    };
    let Ok(path) = std::str::from_utf8(raw) else {
        return Err(damaged(at, not_carried(&String::from_utf8_lossy(raw))));
    };
    if !is_project_file(path) {
        return Err(damaged(at, not_carried(path)));
    }
    if last_path.is_some_and(|last| path <= last) {
        return Err(damaged(at, cannot_stand(rec::FILE)));
    }
    if files >= MAX_FILES {
        return Err(damaged(at, "too many project files"));
    }
    if payload_len - 2 - u64::from(path_len) > MAX_FILE_BYTES as u64 {
        return Err(damaged(at, too_large(path)));
    }
    Ok(path)
}

/// Parses the trailer payload `p` of a dump of `format` read at `at`, and checks its counts and
/// digest against what was read: `read` (batches, frames, files), and `digest`, the digest of
/// every record before it.
pub(crate) fn check_trailer(
    at: u64,
    format: u32,
    p: &[u8],
    read: (u64, u64, u32),
    digest: Hash,
) -> Result<DumpTrailer, StoreError> {
    let t = match parse_trailer(format, p) {
        Some(t) => t,
        None if format >= 3 => return Err(damaged(at, chain_text::TRAILER)),
        None => return Err(damaged(at, TRAILER_MISMATCH)),
    };
    if (t.batches, t.frames, t.files) != read || t.digest != digest {
        return Err(damaged(at, TRAILER_MISMATCH));
    }
    Ok(t)
}

/// The format-3 checks of a dump's end, at its trailer read at `at`: an increment holds a batch,
/// and the last batch (`last`: its physical end and marker; with none, the base) ends at the
/// header's end `log_end` with the header's end marker.
pub(crate) fn check_last(
    at: u64,
    links: &DumpChainHeader,
    last: Option<(Location, BatchMarker)>,
    log_end: Location,
) -> Result<(), StoreError> {
    let (end, marker) = match last {
        Some(last) => last,
        None if links.kind == DumpKind::Increment => {
            return Err(damaged(at, chain_text::EMPTY_INCREMENT));
        }
        None => (links.base, links.base_batch),
    };
    if end != log_end || marker != links.end_batch {
        return Err(damaged(at, chain_text::LAST_BATCH));
    }
    Ok(())
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
            kinds: FrameKinds::Legacy(&[]),
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
        let path = check_file_head(
            record.at,
            &record.payload,
            record.payload.len() as u64,
            self.last_path.as_deref(),
            self.totals.files,
        )?;
        let refused = || damaged(record.at, drop_text::ALLOCATION);
        let item = DumpItem::File {
            path: try_string(path).ok_or_else(refused)?,
            bytes: try_copy(&record.payload[2 + path.len()..]).ok_or_else(refused)?,
        };
        self.last_path = Some(try_string(path).ok_or_else(refused)?);
        self.totals.files += 1;
        Ok(item)
    }

    fn batch(&mut self, record: &Record) -> Result<DumpBatch, StoreError> {
        let n = self.totals.batches + 1;
        // no store writes a batch of no frames
        let Some((start, count, digest, kinds)) = parse_batch_record(&record.payload) else {
            return Err(damaged(record.at, cannot_stand(record.kind)));
        };
        // no buffer is sized from an unread count: frames are pushed as they are read, each slot
        // taken by fallible growth
        let mut frames: Vec<Frame> = Vec::new();
        let mut dropped = false;
        while frames.len() < count as usize {
            let f = self.record(true)?;
            check_frame_head(
                n,
                frames.len(),
                count,
                &RecordHead {
                    at: f.at,
                    len: f.payload.len() as u64,
                    kind: f.kind,
                    hash: f.hash,
                },
            )?;
            // the trailer's digest covers the container record as it is
            self.link(&f)?;
            let frame = if f.kind == rec::DROPPED {
                dropped = true;
                parse_dropped_of(self.format, &f.payload).map_err(|what| damaged(f.at, what))?
            } else {
                Frame::Present {
                    kind: f.kind,
                    hash: f.hash,
                    payload: f.payload,
                }
            };
            if !self.kinds.allows(frame.kind()) {
                return Err(damaged(f.at, cannot_stand(frame.kind())));
            }
            if let Some(payload) = frame.present_payload() {
                check_carried(frame.kind(), payload).map_err(|what| damaged(f.at, what))?;
                self.kinds
                    .check_payload(frame.kind(), payload)
                    .map_err(|what| damaged(f.at, what))?;
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
        let t = check_trailer(
            record.at,
            self.format,
            &record.payload,
            (self.totals.batches, self.totals.frames, self.totals.files),
            Hash::of(&self.chain),
        )?;
        if let Some(links) = self.links {
            self.check_ends(record.at, &links, &t)?;
        }
        let mut extra = [0u8; 1];
        let at = self.totals.bytes;
        loop {
            match self.r.read(&mut extra) {
                Ok(0) => break,
                Ok(_) => return Err(damaged(at, BYTES_AFTER)),
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
        check_last(at, links, self.last, self.log_end)?;
        // the lowest resolved span of the file's applies; a hole only a carried record
        // explains is none. Whether every hole is authorized is the ingest's check.
        let first_ok = if links.kind == DumpKind::Full || self.tally.mode().is_some() {
            // the file holds the whole log: its mode, requests and spans
            t.first_removed == self.tally.first_removed()
        } else {
            // an increment after the origin: its first batch is not the log's, so either
            // resolution may be the log's; a span before its base is in an earlier file; and an
            // apply whose request an earlier file holds is resolved by the loader of the chain
            {
                !self.tally.resolves_all()
                    || t.first_removed == self.tally.project_first()
                    || t.first_removed == self.tally.holder_first()
                    || (t.removals > 0 && t.first_removed.is_some_and(|q| q < links.base))
            }
        };
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
        let head = read_head(&mut self.r, self.totals.bytes, self.format, frame)?;
        let payload = read_body(&mut self.r, &head)?;
        self.totals.bytes = head.end();
        Ok(Record {
            at: head.at,
            kind: head.kind,
            hash: head.hash,
            payload,
        })
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
            RECORD_HOOK.with(|h| {
                if let Some(hook) = h.borrow_mut().as_mut() {
                    hook(loc);
                }
            });
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
