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
