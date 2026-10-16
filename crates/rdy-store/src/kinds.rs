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
// Codes 11-12 are unassigned.
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
