//! The chain facts of dump format 3 (`crate::dump`): what a full dump or an increment says about
//! where it starts and ends in the source log, and how a file follows the one before it. These
//! are transport types, never stored objects.
//!
//! A position is physical: a batch ends just past its marker frame, at its start plus the
//! original size (`37 + len`) of each of its frames, plus `37` and the length of its encoded
//! marker. A full dump starts at the origin (`0.0`), with a zero base marker and a zero `prev`. An
//! increment starts at the end of the file before it: its `base`, `base_batch` and `prev` are
//! that file's end, end marker and trailer digest, and it holds at least one batch. Its first
//! batch starts at `base`, or at offset zero of the next segment when the source log rolled
//! there.
//!
//! In JSON every hash is 64 lowercase hex digits and every repo id 32.
use crate::dump::DumpBatch;
use crate::kinds::BatchCommit;
use crate::segment::{HEADER as FRAME_HEADER, Location, MARKER_MAX};
use crate::transition::hex;
use rdy_model::{Hash, RepoId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The refusals of the format-3 codec (the `what` of `StoreError::Dump` on read, the text of
/// `StoreError::Refused` on write; closed).
pub mod chain_text {
    pub const HEADER: &str = "the header does not parse";
    pub const TRAILER: &str = "the trailer does not parse";
    pub const FULL_BASE: &str = "a full dump has nonzero base facts";
    pub const EMPTY_INCREMENT: &str = "an increment holds no batches";
    pub const FIRST_BATCH: &str = "the first batch does not follow the base";
    pub const LAST_BATCH: &str = "the last batch does not match the header";
    pub const REMOVALS: &str = "the removal facts do not match the batches";
    pub const OVERFLOW: &str = "dump position overflows";
}

/// Whether a dump holds the whole log from its origin (`Full`) or the sealed batches after the
/// end of the file before it (`Increment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpKind {
    Full,
    Increment,
}

impl DumpKind {
    /// The header's kind byte: 0 for full, 1 for increment.
    pub(crate) fn byte(self) -> u8 {
        match self {
            DumpKind::Full => 0,
            DumpKind::Increment => 1,
        }
    }

    /// The kind of a header's kind byte; None for 2 to 255.
    pub(crate) fn of_byte(b: u8) -> Option<DumpKind> {
        match b {
            0 => Some(DumpKind::Full),
            1 => Some(DumpKind::Increment),
            _ => None,
        }
    }
}

/// The facts of one source batch marker: its first frame's location, its frame count and its
/// digest, as the log's `BATCH_COMMIT` holds them. All zero names no batch (the origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchMarker {
    pub start: Location,
    pub frames: u32,
    #[serde(with = "hex::one")]
    pub digest: Hash,
}

impl BatchMarker {
    /// The marker of no batch: the origin, no frames, a zero digest.
    pub const ZERO: BatchMarker = BatchMarker {
        start: Location {
            segment: 0,
            offset: 0,
        },
        frames: 0,
        digest: Hash([0u8; 32]),
    };

    /// The marker of batch `b`: its start, its frame count (`u32::MAX` past that) and its digest.
    pub fn of(b: &DumpBatch) -> BatchMarker {
        BatchMarker {
            start: b.start,
            frames: u32::try_from(b.frames.len()).unwrap_or(u32::MAX),
            digest: b.digest,
        }
    }

    /// The facts of a decoded log marker.
    pub fn of_commit(c: &BatchCommit) -> BatchMarker {
        BatchMarker {
            start: c.start,
            frames: c.frames,
            digest: c.digest,
        }
    }

    /// The length of this marker's payload as the log encodes it (the store's own serializer);
    /// None for a marker the log could not hold.
    pub(crate) fn encoded_len(&self) -> Option<u64> {
        let marker = BatchCommit {
            start: self.start,
            frames: self.frames,
            digest: self.digest,
        };
        let mut buf = [0u8; MARKER_MAX as usize];
        postcard::to_slice(&marker, &mut buf)
            .ok()
            .map(|b| b.len() as u64)
    }
}

/// Where a dump chain ends: its repo, its store format, the end of its last file in the source
/// log, that file's end marker and its trailer digest. The next increment starts here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainEnd {
    #[serde(serialize_with = "hex_repo", deserialize_with = "repo_hex")]
    pub repo_id: RepoId,
    pub store_format: u32,
    pub end: Location,
    pub end_batch: BatchMarker,
    #[serde(with = "hex::one")]
    pub digest: Hash,
}

/// The chain facts of a dump format 3 header: the dump's kind and repo, where it starts (`base`,
/// the marker of the batch that ends there, and the trailer digest of the file before it) and the
/// marker of its last batch. The end itself is the header's `log_end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DumpChainHeader {
    pub kind: DumpKind,
    #[serde(serialize_with = "hex_repo", deserialize_with = "repo_hex")]
    pub repo_id: RepoId,
    pub base: Location,
    pub base_batch: BatchMarker,
    #[serde(with = "hex::one")]
    pub prev: Hash,
    pub end_batch: BatchMarker,
}

impl DumpChainHeader {
    /// The chain facts of a full dump of repo `repo_id` whose last batch is `end_batch`: the
    /// origin base, a zero base marker and a zero `prev`.
    pub fn full(repo_id: RepoId, end_batch: BatchMarker) -> DumpChainHeader {
        DumpChainHeader {
            kind: DumpKind::Full,
            repo_id,
            base: BatchMarker::ZERO.start,
            base_batch: BatchMarker::ZERO,
            prev: Hash([0u8; 32]),
            end_batch,
        }
    }

    /// The chain facts of an increment that follows `prev` and ends at the marker `end_batch`.
    pub fn after(prev: &ChainEnd, end_batch: BatchMarker) -> DumpChainHeader {
        DumpChainHeader {
            kind: DumpKind::Increment,
            repo_id: prev.repo_id,
            base: prev.end,
            base_batch: prev.end_batch,
            prev: prev.digest,
            end_batch,
        }
    }

    /// Whether a full dump's base facts are the origin's: base `0.0`, a zero marker, a zero
    /// `prev`.
    pub(crate) fn has_origin_base(&self) -> bool {
        self.base == BatchMarker::ZERO.start
            && self.base_batch == BatchMarker::ZERO
            && self.prev == Hash([0u8; 32])
    }
}

/// Whether `start`, a first batch's start, follows `base`: the same place, or offset zero of the
/// next segment (the source log rolled there).
pub(crate) fn follows(base: Location, start: Location) -> bool {
    start == base || (start.offset == 0 && base.segment.checked_add(1) == Some(start.segment))
}

/// The physical place just past batch `b`'s marker: its start plus the original size of each of
/// its frames, plus the marker frame. None when it passes `u64::MAX` (or the marker could not be
/// encoded).
pub fn batch_end(b: &DumpBatch) -> Option<Location> {
    let after_frames = b.frames.iter().try_fold(b.start.offset, |at, f| {
        at.checked_add(FRAME_HEADER)?
            .checked_add(u64::from(f.original_len()))
    })?;
    let offset = after_frames
        .checked_add(FRAME_HEADER)?
        .checked_add(BatchMarker::of(b).encoded_len()?)?;
    Some(Location {
        segment: b.start.segment,
        offset,
    })
}

fn hex_repo<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(r)
}

fn repo_hex<'de, D: Deserializer<'de>>(d: D) -> Result<RepoId, D::Error> {
    let text = String::deserialize(d)?;
    match text.parse::<RepoId>() {
        Ok(id) if id.to_string() == text => Ok(id),
        _ => Err(serde::de::Error::custom(format!("bad repo id {text:?}"))),
    }
}
