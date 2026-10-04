use crate::{hash_of, BlockId, Hash, Provenance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub blocks: BTreeMap<BlockId, Hash>,
}

impl StateSnapshot {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub title: String,
    pub view: String,
    pub changes: Vec<Hash>,
    pub provenance: Provenance,
}

impl Proposal {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRecord {
    pub proposal: Hash,
    pub prev: Option<Hash>,
    pub state: Hash,
    pub approved_by: Provenance,
}

impl LandingRecord {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
