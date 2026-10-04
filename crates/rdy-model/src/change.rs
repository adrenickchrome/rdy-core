use crate::{Block, BlockId, Hash, hash_of};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Reader,
    Writer,
    Reviewer,
    Approver,
}

impl Role {
    pub fn can_write(&self) -> bool {
        !matches!(self, Role::Reader)
    }
    pub fn can_land(&self) -> bool {
        matches!(self, Role::Approver)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub actor: String,
    pub run: Option<String>,
    pub role: Role,
    pub at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorialClass {
    Typo,
    Restructure,
    Clarify,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Justification {
    Implements(BlockId),
    Editorial {
        class: EditorialClass,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Create { block: Block },
    Update { base: Hash, block: Block },
    Delete { base: Hash, id: BlockId },
}

impl Op {
    pub fn block_id(&self) -> &BlockId {
        match self {
            Op::Create { block } | Op::Update { block, .. } => &block.id,
            Op::Delete { id, .. } => id,
        }
    }
    pub fn base(&self) -> Option<Hash> {
        match self {
            Op::Create { .. } => None,
            Op::Update { base, .. } | Op::Delete { base, .. } => Some(*base),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub view: String,
    pub ops: Vec<Op>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
}

impl Change {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
