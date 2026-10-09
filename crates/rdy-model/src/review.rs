//! Reviews (plan M2d2): a reviewer's verdict on a subject. A dismissal is a review with verdict
//! `Dismiss` of a tension; the store names it by the tension's vkey.
use crate::{BlockId, Hash, Provenance};
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
    pub findings: Vec<Finding>,
    /// Who reviewed, in which run and role, and when.
    pub provenance: Provenance,
}

impl Review {
    /// The vkey of the subject, if the subject has one.
    pub fn vkey(&self) -> Option<Hash> {
        match &self.subject {
            ReviewSubject::Tension { vkey, .. } => Some(*vkey),
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
}

/// The verdict of a review. Field order is format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The subject needs no action.
    Dismiss,
}

/// A note on one block. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub block: BlockId,
    pub note: String,
}
