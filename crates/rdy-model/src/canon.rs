use crate::assessment::PolicyRef;
use crate::encoding::{EncodedIdentity, LANDING_ENCODING, encoded_hash};
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

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
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
    /// The ASSESSMENT of the approved version (encoding 1). None from every product writer.
    pub assessment: Option<Hash>,
    /// The authority policy block version that accepted the landing (encoding 1). Set only with
    /// assurance `policy` and no relay. Set only by the policy landing writer
    /// (`Store::prepare_land_by_policy`); None from every other product writer.
    pub policy: Option<PolicyRef>,
    /// The identity of a stored read: never serialized, `None` on every new landing. A caller
    /// that edits a stored read clears it first.
    #[serde(skip)]
    pub read_identity: Option<EncodedIdentity>,
}

impl LandingRecord {
    /// The landing's id: a stored read's recorded hash, else the hash of its latest encoding
    /// ([`LANDING_ENCODING`]: the version prefix and the body).
    pub fn id(&self) -> Hash {
        match self.read_identity {
            Some(i) => i.hash,
            None => encoded_hash(
                LANDING_ENCODING,
                &postcard::to_allocvec(self)
                    .expect("postcard encoding of an in-memory value cannot fail"),
            ),
        }
    }
}

/// Equal fields: a stored read equals the same landing built anew.
impl PartialEq for LandingRecord {
    fn eq(&self, o: &LandingRecord) -> bool {
        self.proposal == o.proposal
            && self.prev == o.prev
            && self.state == o.state
            && self.approved_by == o.approved_by
            && self.changes == o.changes
            && self.chosen == o.chosen
            && self.checks == o.checks
            && self.assurance == o.assurance
            && self.relayed_by == o.relayed_by
            && self.relay_evidence == o.relay_evidence
            && self.approved_version == o.approved_version
            && self.rebased_from == o.rebased_from
            && self.evidence == o.evidence
            && self.assessment == o.assessment
            && self.policy == o.policy
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
}

impl Assurance {
    pub fn as_str(self) -> &'static str {
        match self {
            Assurance::Policy => "policy",
            Assurance::Relayed => "relayed",
            Assurance::Local => "local",
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
