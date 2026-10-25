use crate::encoding::{CHANGE_ENCODING, EncodedIdentity, encoded_hash};
use crate::{ActorRun, Block, BlockId, Hash};
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
/// encodes as its other fields followed by six absent options (encoding 1; encoding 0 had the
/// first four, `crate::encoding::LegacyChangeLinksV0`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLinks {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
    /// The importer (actor and run) of a change that rdy import wrote (FW-F7b); None on every other change.
    pub imported_by: Option<ActorRun>,
    /// The change this one replays (encoding 1): a reference to a CHANGE, never one of its
    /// dependencies. None on every ordinary change.
    pub replayed_from: Option<Hash>,
    /// The identity of the briefing the writer read (encoding 1): a token identity, not an
    /// object hash. None on every ordinary change; a replay copies its source's.
    pub briefing: Option<Hash>,
}

/// The transient identity of a change that was read, never serialized: a stored read keeps the
/// identity of the frame it came from; a read projection of a removed change keeps its recorded
/// id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeReadIdentity {
    /// Read from a stored frame: its hash and encoding version.
    Stored(EncodedIdentity),
    /// The read projection of a residue: the removed change's recorded id.
    Redacted(Hash),
}

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct Change {
    pub view: String,
    pub ops: Vec<Op>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: ChangeLinks,
    /// The identity of a read change: never serialized, `None` on every new change. A store's
    /// typed read sets `Stored` (the frame's hash and encoding); `ChangeResidue::project` alone
    /// sets `Redacted`. A caller that edits a stored read clears it first: the store refuses a
    /// value whose stored identity does not match its bytes.
    #[serde(skip)]
    pub read_identity: Option<ChangeReadIdentity>,
}

impl Change {
    /// The change's id: a stored read's recorded hash, a residue projection's recorded id, and
    /// for a new change the hash of its latest encoding ([`CHANGE_ENCODING`]: the version prefix
    /// and the body).
    pub fn id(&self) -> Hash {
        match self.read_identity {
            Some(ChangeReadIdentity::Stored(i)) => i.hash,
            Some(ChangeReadIdentity::Redacted(id)) => id,
            None => encoded_hash(
                CHANGE_ENCODING,
                &postcard::to_allocvec(self)
                    .expect("postcard encoding of an in-memory value cannot fail"),
            ),
        }
    }

    /// The recorded id of a read projection, if this change is one.
    pub fn redacted_id(&self) -> Option<Hash> {
        match self.read_identity {
            Some(ChangeReadIdentity::Redacted(h)) => Some(h),
            _ => None,
        }
    }

    /// The stored identity of a typed read, if this change is one.
    pub fn stored_identity(&self) -> Option<EncodedIdentity> {
        match self.read_identity {
            Some(ChangeReadIdentity::Stored(i)) => Some(i),
            _ => None,
        }
    }
}

/// Equal fields and the same projection identity: a stored read equals the same change built
/// anew, as before reads kept their identity.
impl PartialEq for Change {
    fn eq(&self, o: &Change) -> bool {
        self.view == o.view
            && self.ops == o.ops
            && self.deps == o.deps
            && self.provenance == o.provenance
            && self.justification == o.justification
            && self.message == o.message
            && self.links == o.links
            && self.redacted_id() == o.redacted_id()
    }
}
