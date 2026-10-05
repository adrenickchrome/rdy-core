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

/// A chosen member of an alternative set: the set's stable id (its first version's hash) and the
/// member, which is a view name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AltChoice {
    pub set: Hash,
    pub member: String,
}

/// What a change is linked to. It is the last field of `Change`, so a change without links
/// encodes as its other fields followed by three absent options.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLinks {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub view: String,
    pub ops: Vec<Op>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: ChangeLinks,
}

impl Change {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
