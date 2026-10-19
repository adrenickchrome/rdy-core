use rdy_model::Hash;
use serde::{Deserialize, Serialize};

pub const BLOCK: u8 = 1;
pub const CHANGE: u8 = 2;
pub const PROPOSAL: u8 = 3;
pub const LANDING: u8 = 4;
pub const SNAPSHOT: u8 = 5;
pub const BLOB_CHUNK: u8 = 6;
pub const BLOB_MANIFEST: u8 = 7;
pub const REF_UPDATE: u8 = 8;
pub const TAG: u8 = 9;
/// An answer (`rdy_model::Answer`; plan Decision intake): refs `answer/<item>` and
/// `pending/<person>/<item>` name the latest one (`crate::answer`).
pub const ANSWER: u8 = 10;
/// A detached signature (`rdy_model::signature::Signature`, D-1Z951): it names its subject, a
/// landing or a tag, and never changes it. The index maps each subject to its signatures
/// (`crate::signature`).
pub const SIGNATURE: u8 = 11;
// Code 12 is unassigned.
/// One op of a view's chain (`crate::view::ViewOp`).
pub const VIEW_OP: u8 = 13;
/// Ends every write batch: names the batch's first frame, its frame count and a digest of the
/// frame hashes. Neither an object nor an index entry.
pub const BATCH_COMMIT: u8 = 14;
/// An intent version (`rdy_model::Intent`); ref `intent/<stable>` names the latest.
pub const INTENT: u8 = 15;
/// An alternative set version (`rdy_model::AltSet`); ref `alt/<stable>` names the latest.
pub const ALT_SET: u8 = 16;
/// A review (rdy_model::Review): refs dismiss/<vkey>, mention/<block>/<entity> and pair/<key> name
/// dismissals, mention reviews and verdicts.
pub const REVIEW: u8 = 17;
/// A semantic tension version (rdy_model::SemanticTension); ref semantic/<key> names the latest.
pub const TENSION: u8 = 18;
/// One batch's tension and suspect moves (`crate::transition::Transitions`; plan M2e2, D-Z0EMP):
/// at most one per batch, its last data frame. Never deduped: two batches may log the same bytes.
pub const TRANSITION: u8 = 19;
/// A content removal request (`rdy_model::Redaction`; plan Redaction, D-600CW): it names its
/// carrier change and binds the complete manifest. Derived tables map the carrier to it.
pub const REDACTION: u8 = 20;
/// What survives a removed block version or change (`rdy_model::RedactionResidue`, D-3M81W).
pub const REDACTION_RESIDUE: u8 = 21;
/// The human apply record that activates a request (`rdy_model::RedactionApply`): it shares a
/// sealed batch with the landing it names.
pub const REDACTION_APPLY: u8 = 22;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    pub name: String,
    pub value: Vec<u8>,
    pub at: crate::segment::Location,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    pub len: u64,
    pub chunks: Vec<Hash>,
}

impl BlobManifest {
    /// Whether `payload`, the bytes this manifest was decoded from, is its own canonical
    /// encoding, as `put_blob` writes it. Postcard also decodes a varint that is not minimal, so
    /// other bytes (under another hash) can give the same manifest; they are no blob a store
    /// reads or writes.
    pub fn is_encoding_of(&self, payload: &[u8]) -> bool {
        crate::store::encode(self) == payload
    }

    /// Whether the chunk count is the one `put_blob` writes for the length: one chunk for each
    /// started [`MAX_CHUNK_BYTES`](crate::bounded::MAX_CHUNK_BYTES) of it.
    pub fn chunks_fit(&self) -> bool {
        self.chunks.len() as u64 == self.len.div_ceil(crate::bounded::MAX_CHUNK_BYTES as u64)
    }
}

/// The marker that ends a batch (kind `BATCH_COMMIT`): `start` is the location of the batch's
/// first data frame, `frames` how many data frames the batch holds, and `digest` is
/// `batch_digest` of their hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchCommit {
    pub start: crate::segment::Location,
    pub frames: u32,
    pub digest: Hash,
}

impl BatchCommit {
    /// The marker a `BATCH_COMMIT` frame's `payload` holds; None if it does not decode.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        postcard::from_bytes(payload).ok()
    }
}

/// `Hash::of` over the concatenated 32-byte frame hashes, in batch order.
pub fn batch_digest<'a>(hashes: impl IntoIterator<Item = &'a Hash>) -> Hash {
    let mut bytes = Vec::new();
    for h in hashes {
        bytes.extend_from_slice(&h.0);
    }
    Hash::of(&bytes)
}
