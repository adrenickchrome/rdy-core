//! Semantic tensions (plan M2e1): a pair of blocks at given contents, and the versions of a tension a reviewer's verdict opened.
use crate::foreign::ForeignId;
use crate::{ActorRun, BlockId, Hash, Provenance, RelayEvidence};
use serde::{Deserialize, Serialize};

/// One side of a pair: a block at a given content. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PairItem {
    pub block: BlockId,
    pub content: Hash,
}

/// The key of a pair: the same for its items in any order, and different when a content differs.
pub fn pair_key(items: &[PairItem]) -> Hash {
    let mut sorted: Vec<&PairItem> = items.iter().collect();
    sorted.sort();
    let mut bytes = b"rdy-pair-1\0".to_vec();
    for i in sorted {
        bytes.extend_from_slice(i.block.to_string().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(i.content.to_string().as_bytes());
        bytes.push(0);
    }
    Hash::of(&bytes)
}

/// A semantic tension version (kind TENSION). The first version comes with the verdict that opened it, with the reviewer's advice if any; the only later version is a person's acceptance of that advice (D-WMCFW). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTension {
    /// The pair key (`pair_key` of `items`).
    pub key: Hash,
    /// The two blocks at the contents the verdict judged.
    pub items: Vec<PairItem>,
    /// The review whose `tension` verdict opened the tension.
    pub review: Hash,
    /// The reviewer's reason that the tension should block, when the verdict advised it.
    pub advice: Option<String>,
    /// Whether the tension blocks: set only by a person's acceptance of the advice.
    pub blocking: bool,
    /// The person's words, as relayed, when an acceptance was relayed.
    pub relay: Option<RelayEvidence>,
    /// The agent that relayed the acceptance at the command line.
    pub relayed_by: Option<ActorRun>,
    /// The version this one follows; `None` for the first.
    pub prev: Option<Hash>,
    /// Who wrote this version, in which run and role, and when.
    pub provenance: Provenance,
}

/// One side of a pair in REVIEW and TENSION encoding 1 ([`crate::encoding::PAIR_FOREIGN_ENCODING`],
/// repo links): a local block at a content, as [`PairItem`] holds it, or a cited block at a
/// content. Variant order is format. A pair with a cited side is a
/// [`crate::ReviewSubject::ForeignPair`] and a [`SemanticTensionV1`]; a local pair keeps
/// [`PairItem`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PairSide {
    Local(PairItem),
    Foreign(ForeignPairItem),
}

impl PairSide {
    /// The local item, when the side is local.
    pub fn local(&self) -> Option<&PairItem> {
        match self {
            PairSide::Local(i) => Some(i),
            PairSide::Foreign(_) => None,
        }
    }
}

/// What a side names, without its content: a local block, or a cited block. Variant order is the
/// order sides sort in (a local side before a cited one).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PairTarget {
    Local(BlockId),
    Foreign(ForeignId),
}

impl PairSide {
    /// The block or cited block the side names.
    pub fn target(&self) -> PairTarget {
        match self {
            PairSide::Local(i) => PairTarget::Local(i.block.clone()),
            PairSide::Foreign(i) => PairTarget::Foreign(i.target.clone()),
        }
    }

    /// The content the side judged: a local block's content hash, or a cited block's content
    /// identity (never a BLOCK version hash).
    pub fn content(&self) -> Hash {
        match self {
            PairSide::Local(i) => i.content,
            PairSide::Foreign(i) => i.content,
        }
    }

    /// The block id the side names (a cited block's id in its own repo).
    pub fn block(&self) -> &BlockId {
        match self {
            PairSide::Local(i) => &i.block,
            PairSide::Foreign(i) => &i.target.id,
        }
    }
}

/// The key of a pair of sides. With every side local it is [`pair_key`] of the items, so every
/// released local pair key stays. With a cited side it hashes each side under the tag of its
/// kind, and a cited side adds its repo, so one block id in two repos is two pairs, and a cited
/// side is never a local one. The same for the sides in any order.
pub fn pair_key_sides(sides: &[PairSide]) -> Hash {
    if !has_foreign_side(sides) {
        let locals: Vec<PairItem> = sides.iter().filter_map(|s| s.local().cloned()).collect();
        return pair_key(&locals);
    }
    let mut sorted: Vec<&PairSide> = sides.iter().collect();
    sorted.sort();
    let mut bytes = b"rdy-pair-2\0".to_vec();
    for s in sorted {
        match s {
            PairSide::Local(i) => {
                bytes.extend_from_slice(b"L\0");
                bytes.extend_from_slice(i.block.to_string().as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(i.content.to_string().as_bytes());
            }
            PairSide::Foreign(i) => {
                bytes.extend_from_slice(b"F\0");
                bytes.extend_from_slice(i.target.repo.to_string().as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(i.target.id.to_string().as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(i.content.to_string().as_bytes());
            }
        }
        bytes.push(0);
    }
    Hash::of(&bytes)
}

/// A cited block at a content: its foreign id and the content identity the side judged, never a
/// BLOCK version hash. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ForeignPairItem {
    pub target: ForeignId,
    pub content: Hash,
}

/// Whether `sides` holds a foreign side.
pub fn has_foreign_side(sides: &[PairSide]) -> bool {
    sides.iter().any(|s| matches!(s, PairSide::Foreign(_)))
}

/// `items` as encoding-1 sides, each local.
pub fn local_sides(items: &[PairItem]) -> Vec<PairSide> {
    items.iter().cloned().map(PairSide::Local).collect()
}

/// `sides` as local items; `None` when one is foreign.
pub fn into_local_items(sides: Vec<PairSide>) -> Option<Vec<PairItem>> {
    sides
        .into_iter()
        .map(|s| match s {
            PairSide::Local(i) => Some(i),
            PairSide::Foreign(_) => None,
        })
        .collect()
}

/// A semantic tension version in TENSION encoding 1 ([`crate::encoding::PAIR_FOREIGN_ENCODING`]):
/// the fields of [`SemanticTension`] in their order, its items as [`PairSide`]s. A TENSION takes
/// it only when a side is foreign. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTensionV1 {
    pub key: Hash,
    pub items: Vec<PairSide>,
    pub review: Hash,
    pub advice: Option<String>,
    pub blocking: bool,
    pub relay: Option<RelayEvidence>,
    pub relayed_by: Option<ActorRun>,
    pub prev: Option<Hash>,
    pub provenance: Provenance,
}

impl From<SemanticTension> for SemanticTensionV1 {
    fn from(t: SemanticTension) -> SemanticTensionV1 {
        SemanticTensionV1::of(&t)
    }
}

impl SemanticTensionV1 {
    /// The layout of `t`, every side local.
    pub fn of(t: &SemanticTension) -> SemanticTensionV1 {
        SemanticTensionV1 {
            key: t.key,
            items: local_sides(&t.items),
            review: t.review,
            advice: t.advice.clone(),
            blocking: t.blocking,
            relay: t.relay.clone(),
            relayed_by: t.relayed_by.clone(),
            prev: t.prev,
            provenance: t.provenance.clone(),
        }
    }

    /// Whether a side is foreign: the one case that takes encoding 1.
    pub fn has_foreign(&self) -> bool {
        has_foreign_side(&self.items)
    }

    /// The runtime tension, when every side is local.
    pub fn into_local(self) -> Option<SemanticTension> {
        Some(SemanticTension {
            key: self.key,
            items: into_local_items(self.items)?,
            review: self.review,
            advice: self.advice,
            blocking: self.blocking,
            relay: self.relay,
            relayed_by: self.relayed_by,
            prev: self.prev,
            provenance: self.provenance,
        })
    }
}
