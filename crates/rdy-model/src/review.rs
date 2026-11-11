//! Reviews (plan M2d2): a reviewer's verdict on a subject. A dismissal is a review with verdict
//! `Dismiss` of a tension, named by the tension's vkey, or of a mention, named by its block and
//! entity; a mention's latest review wins. A verdict is a review with verdict `Tension` or
//! `NoTension` of a pair; ref `pair/<key>` names the one that settles it.
use crate::semantic::{PairSide, has_foreign_side, into_local_items, local_sides};
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
            ReviewSubject::Check { .. } => None,
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
    /// A check request's case (appended: index 3): the CHECK_REQUEST it answers.
    Check { request: Hash },
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
    /// The check found nothing (appended: index 4).
    Pass,
    /// The check found something to look at (index 5).
    Concern,
    /// The check failed (index 6).
    Fail,
}

/// A note on one block. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub block: BlockId,
    pub note: String,
}

/// The subject of a review in REVIEW encoding 1 ([`crate::encoding::PAIR_FOREIGN_ENCODING`], repo
/// links): [`ReviewSubject`]'s variants in their order, a pair's items as [`PairSide`]s. Field
/// and variant order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewSubjectV1 {
    Tension {
        id: Hash,
        vkey: Hash,
        kind: String,
        severity: String,
        sides: Vec<String>,
        blocks: Vec<BlockId>,
    },
    Mention {
        block: BlockId,
        entity: BlockId,
    },
    Pair {
        key: Hash,
        items: Vec<PairSide>,
    },
    Check {
        request: Hash,
    },
}

impl ReviewSubjectV1 {
    /// The pair this subject names: its key and its sides.
    pub fn pair(&self) -> Option<(Hash, &[PairSide])> {
        match self {
            ReviewSubjectV1::Pair { key, items } => Some((*key, items.as_slice())),
            _ => None,
        }
    }
}

/// A review in REVIEW encoding 1: the fields of [`Review`] in their order, its subject as
/// [`ReviewSubjectV1`]. A REVIEW takes it only when its pair has a foreign side. A registered
/// wire shape; the engine reads and writes every review of a pair through it. Field order is
/// format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewV1 {
    pub subject: ReviewSubjectV1,
    pub case_hash: Option<Hash>,
    pub verdict: Verdict,
    pub reason: String,
    pub citations: Vec<Citation>,
    pub provenance: Provenance,
}

impl ReviewV1 {
    /// The layout of `r`, every pair side local.
    pub fn of(r: &Review) -> ReviewV1 {
        let subject = match &r.subject {
            ReviewSubject::Tension {
                id,
                vkey,
                kind,
                severity,
                sides,
                blocks,
            } => ReviewSubjectV1::Tension {
                id: *id,
                vkey: *vkey,
                kind: kind.clone(),
                severity: severity.clone(),
                sides: sides.clone(),
                blocks: blocks.clone(),
            },
            ReviewSubject::Mention { block, entity } => ReviewSubjectV1::Mention {
                block: block.clone(),
                entity: entity.clone(),
            },
            ReviewSubject::Pair { key, items } => ReviewSubjectV1::Pair {
                key: *key,
                items: local_sides(items),
            },
            ReviewSubject::Check { request } => ReviewSubjectV1::Check { request: *request },
        };
        ReviewV1 {
            subject,
            case_hash: r.case_hash,
            verdict: r.verdict,
            reason: r.reason.clone(),
            citations: r.citations.clone(),
            provenance: r.provenance.clone(),
        }
    }

    /// Whether its pair has a foreign side: the one case that takes encoding 1.
    pub fn has_foreign(&self) -> bool {
        matches!(&self.subject, ReviewSubjectV1::Pair { items, .. } if has_foreign_side(items))
    }

    /// The runtime review, when every pair side is local.
    pub fn into_local(self) -> Option<Review> {
        let subject = match self.subject {
            ReviewSubjectV1::Tension {
                id,
                vkey,
                kind,
                severity,
                sides,
                blocks,
            } => ReviewSubject::Tension {
                id,
                vkey,
                kind,
                severity,
                sides,
                blocks,
            },
            ReviewSubjectV1::Mention { block, entity } => ReviewSubject::Mention { block, entity },
            ReviewSubjectV1::Pair { key, items } => ReviewSubject::Pair {
                key,
                items: into_local_items(items)?,
            },
            ReviewSubjectV1::Check { request } => ReviewSubject::Check { request },
        };
        Some(Review {
            subject,
            case_hash: self.case_hash,
            verdict: self.verdict,
            reason: self.reason,
            citations: self.citations,
            provenance: self.provenance,
        })
    }
}
