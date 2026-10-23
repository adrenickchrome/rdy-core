use crate::{ActorRun, Block, BlockId, Hash, hash_of};
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

/// Why a change was made. The variants are encoded by position, so a new one is appended and an
/// existing one is never moved (D-F0904): Implements is 0, Editorial 1, Reflects 2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Justification {
    Implements(Vec<BlockId>),
    Editorial {
        class: EditorialClass,
        reason: String,
    },
    /// Store format 18 (D-4WC6R): the change makes its blocks reflect the cited sources, in
    /// first-occurrence order, and, when `code` is set, the code state their `reflects` globs
    /// match.
    Reflects {
        sources: Vec<BlockId>,
        code: bool,
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
/// encodes as its other fields followed by four absent options.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLinks {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
    /// The importer (actor and run) of a change that rdy import wrote (FW-F7b); None on every other change.
    pub imported_by: Option<ActorRun>,
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
    /// The recorded id of a change read through its redaction residue: never serialized, `None`
    /// on every normal change and after decoding. Built only by `ChangeResidue::project`.
    #[serde(skip)]
    pub read_identity: Option<Hash>,
}

impl Change {
    /// The hash of the change's encoding. A residue projection returns its recorded id.
    pub fn id(&self) -> Hash {
        match self.read_identity {
            Some(id) => id,
            None => hash_of(self),
        }
    }
}
