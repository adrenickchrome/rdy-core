use crate::{AltChoice, BlockId, Hash, Provenance, hash_of};
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
    /// The first version's id; `None` on the first version.
    pub stable: Option<Hash>,
    /// The version this one replaces; `None` on the first version.
    pub prev: Option<Hash>,
    pub title: String,
    pub view: String,
    pub changes: Vec<Hash>,
    pub alternative: Option<AltChoice>,
    pub provenance: Provenance,
    /// Canon's head when this version was written; `None` before the first landing.
    pub canon_at: Option<Hash>,
    /// The proposer's plain-words summary for the approver (FW-G7c); `None` when none was given.
    pub summary: Option<String>,
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
    pub changes: Vec<Hash>,
    pub chosen: Option<AltChoice>,
    /// Ids of check-result objects; always empty in M2.
    pub checks: Vec<Hash>,
    pub assurance: Assurance,
    pub relayed_by: Option<ActorRun>,
    pub relay_evidence: Option<RelayEvidence>,
    /// The proposal version the approver approved.
    pub approved_version: Option<Hash>,
    /// The landings this one replaces after a rebase.
    pub rebased_from: Vec<Hash>,
    /// How the approval was obtained, when that is recorded.
    pub evidence: Option<ApprovalEvidence>,
}

impl LandingRecord {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}

/// How strongly a landing's approval is attested. The declaration order is the wire format
/// (postcard's variant index) and the strength order: a relay is never stronger than `Local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Assurance {
    Policy,
    Relayed,
    Local,
    Authoritative,
}

impl Assurance {
    pub fn as_str(self) -> &'static str {
        match self {
            Assurance::Policy => "policy",
            Assurance::Relayed => "relayed",
            Assurance::Local => "local",
            Assurance::Authoritative => "authoritative",
        }
    }
}

/// An actor and, optionally, the run it acted in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRun {
    pub actor: String,
    pub run: Option<String>,
}

/// What a relayed approval quotes, and where the quote came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayEvidence {
    pub quoted: String,
    pub session_ref: String,
    /// The question the human was asked, when the relay recorded it.
    pub question: Option<String>,
}

/// How an approval was obtained. The declaration order is the wire format (postcard's variant
/// index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    /// Asked through the client's own elicitation.
    Elicited,
    /// Answered on a rendered approval card.
    Card,
    /// Quoted from free text.
    Text,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::Elicited => "elicited",
            EvidenceKind::Card => "card",
            EvidenceKind::Text => "text",
        }
    }
}

/// The record of how a landing's approval was obtained: its kind, the options offered, the one
/// chosen, and the card shown (if any).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalEvidence {
    pub kind: EvidenceKind,
    pub options: Vec<String>,
    pub choice: Option<String>,
    pub card_hash: Option<Hash>,
}

/// A name for a landing. In rdy core every tag is a version (addendum A): it freezes the snapshot
/// its landing names. Immutable: tags are never moved or deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub landing: Hash,
    pub message: Option<String>,
    pub tagged_by: Provenance,
}

impl Tag {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
