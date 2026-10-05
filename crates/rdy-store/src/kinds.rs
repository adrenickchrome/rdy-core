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
/// Ends every write batch: names the batch's first frame, its frame count and a digest of the
/// frame hashes. Neither an object nor an index entry.
pub const BATCH_COMMIT: u8 = 14;

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

/// `Hash::of` over the concatenated 32-byte frame hashes, in batch order.
pub fn batch_digest<'a>(hashes: impl IntoIterator<Item = &'a Hash>) -> Hash {
    let mut bytes = Vec::new();
    for h in hashes {
        bytes.extend_from_slice(&h.0);
    }
    Hash::of(&bytes)
}
