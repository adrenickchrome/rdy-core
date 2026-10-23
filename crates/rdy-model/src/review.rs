//! Reviews (plan M2d2): a reviewer's verdict on a subject. A dismissal is a review with verdict
//! `Dismiss` of a tension, named by the tension's vkey, or of a mention, named by its block and
//! entity; a mention's latest review wins. A verdict is a review with verdict `Tension` or
//! `NoTension` of a pair; ref `pair/<key>` names the one that settles it.
use crate::{BlockId, Hash, PairItem, Provenance};
use serde::{Deserialize, Serialize};

/// A review (kind REVIEW). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    /// What the review is about.
    pub subject: ReviewSubject,
    /// The hash of the case the reviewer read, if any.
    pub case_hash: Option<Hash>,
    /// What the reviewer decided.
    pub verdict: Verdict,
    /// Why, in the reviewer's words.
    pub reason: String,
    /// Notes on single blocks.
    pub citations: Vec<Citation>,
    /// Who reviewed, in which run and role, and when.
    pub provenance: Provenance,
}

impl Review {
    /// The vkey of the subject, if the subject has one.
    pub fn vkey(&self) -> Option<Hash> {
        match &self.subject {
            ReviewSubject::Tension { vkey, .. } => Some(*vkey),
            ReviewSubject::Mention { .. } => None,
            ReviewSubject::Pair { .. } => None,
        }
    }
}

/// The subject of a review. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewSubject {
    /// A tension as it was when reviewed: its id, its vkey, its kind and severity, the labels of
    /// its sides and the blocks it involves.
    Tension {
        id: Hash,
        vkey: Hash,
        kind: String,
        severity: String,
        sides: Vec<String>,
        blocks: Vec<BlockId>,
    },
    /// A mention of an entity in a block's text, as a passing mention (FW-CV4): the block and the entity.
    Mention { block: BlockId, entity: BlockId },
    /// A candidate pair of blocks at given contents (plan M2e1): the pair key and the two items.
    Pair { key: Hash, items: Vec<PairItem> },
}

/// The verdict of a review. Field order is format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The subject needs no action.
    Dismiss,
    /// The subject needs action again: a mention restored after a dismissal (FW-CV4).
    Restore,
    /// The pair disagrees.
    Tension,
    /// The pair does not disagree.
    NoTension,
}

/// A note on one block. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub block: BlockId,
    pub note: String,
}
