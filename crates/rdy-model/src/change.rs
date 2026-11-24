use crate::encoding::{
    CHANGE_ENCODING, CHANGE_FOREIGN_ENCODING, CHANGE_PORT_ENCODING, CHANGE_PORT_FOREIGN_ENCODING,
    ChangeLinksV1, EncodedIdentity, PortLinksV3, change_body, encoded_hash,
};
use crate::{ActorRun, Block, BlockId, Hash, RepoId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

/// Where a ported change came from: the source store's repo id and the source change's id. It is
/// data, not an object reference: no edge, no dependency.
///
/// Postcard writes the 16 repo-id bytes, then the 32 hash bytes. JSON writes both as lowercase
/// hex: `{"repo_id": "...", "change": "..."}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortedFrom {
    #[serde(with = "ported_hex::repo")]
    pub repo_id: RepoId,
    #[serde(with = "ported_hex::hash")]
    pub change: Hash,
}

/// What a change is linked to. It is the last field of `Change`.
///
/// The six options up to `briefing` are encoding 1's frozen links
/// ([`crate::encoding::ChangeLinksV1`]); encoding 0 had the first four
/// ([`crate::encoding::LegacyChangeLinksV0`]). `ported_from` is the seventh field, and only
/// change encodings 3 and 4 have it ([`crate::encoding::PortLinksV3`]).
///
/// Postcard reads the frozen six fields only, so no released body moves. It writes them too,
/// and a port's links in the seventh-field layout of encodings 3 and 4, so a port's postcard
/// form is its encoding-3 body. JSON writes all seven, and leaves out an absent `ported_from`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    /// The source of a ported change (encodings 3 and 4): data, not an object reference. None on
    /// every unported change; a replay copies its source's.
    pub ported_from: Option<PortedFrom>,
}

/// `ChangeLinks` in JSON: all seven fields, `ported_from` left out when absent.
#[derive(Serialize)]
#[serde(rename = "ChangeLinks")]
struct LinksJsonOut<'a> {
    intent: &'a Option<Hash>,
    alternative: &'a Option<AltChoice>,
    client_nonce: &'a Option<String>,
    imported_by: &'a Option<ActorRun>,
    replayed_from: &'a Option<Hash>,
    briefing: &'a Option<Hash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ported_from: Option<&'a PortedFrom>,
}

/// `ChangeLinks` read from JSON: an omitted option reads as None.
#[derive(Deserialize)]
#[serde(rename = "ChangeLinks")]
struct LinksJsonIn {
    intent: Option<Hash>,
    alternative: Option<AltChoice>,
    client_nonce: Option<String>,
    imported_by: Option<ActorRun>,
    replayed_from: Option<Hash>,
    briefing: Option<Hash>,
    #[serde(default)]
    ported_from: Option<PortedFrom>,
}

impl Serialize for ChangeLinks {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return LinksJsonOut {
                intent: &self.intent,
                alternative: &self.alternative,
                client_nonce: &self.client_nonce,
                imported_by: &self.imported_by,
                replayed_from: &self.replayed_from,
                briefing: &self.briefing,
                ported_from: self.ported_from.as_ref(),
            }
            .serialize(s);
        }
        match ChangeLinksV1::from_current(self) {
            Some(v1) => v1.serialize(s),
            None => PortLinksV3::of(self).serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for ChangeLinks {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ChangeLinks, D::Error> {
        if d.is_human_readable() {
            let l = LinksJsonIn::deserialize(d)?;
            return Ok(ChangeLinks {
                intent: l.intent,
                alternative: l.alternative,
                client_nonce: l.client_nonce,
                imported_by: l.imported_by,
                replayed_from: l.replayed_from,
                briefing: l.briefing,
                ported_from: l.ported_from,
            });
        }
        Ok(ChangeLinksV1::deserialize(d)?.into_current())
    }
}

/// A ported source as hex in a human-readable format, and as its own bytes otherwise.
mod ported_hex {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    fn parse<T: std::str::FromStr, E: serde::de::Error>(text: &str, what: &str) -> Result<T, E> {
        text.parse()
            .map_err(|_| E::custom(format!("bad {what} {text:?}")))
    }

    /// A repo id.
    pub mod repo {
        use super::*;
        use crate::RepoId;

        pub fn serialize<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.serialize_str(&r.to_string())
            } else {
                r.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RepoId, D::Error> {
            if d.is_human_readable() {
                parse(&String::deserialize(d)?, "repo id")
            } else {
                RepoId::deserialize(d)
            }
        }
    }

    /// A hash.
    pub mod hash {
        use super::*;
        use crate::Hash;

        pub fn serialize<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.serialize_str(&h.to_string())
            } else {
                h.serialize(s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
            if d.is_human_readable() {
                parse(&String::deserialize(d)?, "hash")
            } else {
                Hash::deserialize(d)
            }
        }
    }
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
    /// for a new change the hash of the encoding it is written in ([`Change::encoding`]: the
    /// version prefix and the body).
    pub fn id(&self) -> Hash {
        match self.read_identity {
            Some(ChangeReadIdentity::Stored(i)) => i.hash,
            Some(ChangeReadIdentity::Redacted(id)) => id,
            None => {
                let version = self.encoding();
                let body = change_body(self, version)
                    .expect("a change has a form in the encoding it chooses");
                encoded_hash(version, &body)
            }
        }
    }

    /// Whether an op of the change embeds a block with a foreign part (repo links).
    pub fn embeds_foreign(&self) -> bool {
        self.ops.iter().any(|op| match op {
            Op::Create { block } | Op::Update { block, .. } => !block.foreign.is_empty(),
            Op::Delete { .. } => false,
        })
    }

    /// Whether the change is a port: it records `links.ported_from`.
    pub fn is_ported(&self) -> bool {
        self.links.ported_from.is_some()
    }

    /// Whether the change copies another change's ops: a replay (`replayed_from`) or a port
    /// (`ported_from`). Merge callers treat both alike.
    pub fn is_replay_like(&self) -> bool {
        self.links.replayed_from.is_some() || self.is_ported()
    }

    /// The encoding a new change is written in, chosen by its value. An unported change takes
    /// the released [`CHANGE_ENCODING`], or [`CHANGE_FOREIGN_ENCODING`] when it embeds a block
    /// with a foreign part. A port takes [`CHANGE_PORT_ENCODING`], or
    /// [`CHANGE_PORT_FOREIGN_ENCODING`] when it embeds such a block.
    pub fn encoding(&self) -> u16 {
        match (self.is_ported(), self.embeds_foreign()) {
            (false, false) => CHANGE_ENCODING,
            (false, true) => CHANGE_FOREIGN_ENCODING,
            (true, false) => CHANGE_PORT_ENCODING,
            (true, true) => CHANGE_PORT_FOREIGN_ENCODING,
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
