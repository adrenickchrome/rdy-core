//! Object encodings (D-WWAK7: "Each object records its encoding version"). A stored object is
//! its kind, its encoding version and its body. Version 0 is the original encoding: the stored
//! payload is the body alone, so every object written before versioned encodings keeps its
//! bytes and its hash. From version 1 on, the stored payload is `version` (two bytes, little
//! endian) followed by the body, and the object's hash covers both.
//!
//! An [`EncodedIdentity`] is metadata a reader keeps beside a decoded value: the hash and the
//! encoding version of the stored object it came from. It is never part of a postcard or JSON
//! encoding, so a historical body decoded into a newer type keeps its original identity.
//!
//! Three kinds have a latest encoding past 0: CHANGE ([`CHANGE_ENCODING`]), LANDING
//! ([`LANDING_ENCODING`]) and REDACTION_RESIDUE ([`RESIDUE_ENCODING`]). Their encoding-0 bodies
//! are frozen here as the `Legacy*V0` types, which keep the exact positional fields of the
//! original declarations: a short old body is never decoded into the larger current struct. The
//! conversions into the current types add absent options and a zero source encoding; the
//! reverse conversions exist only while every newly added field is absent or zero.
use crate::change::ChangeReadIdentity;
use crate::redaction::{BlockResidue, ChangeResidue, RedactionResidue, ResidualOp};
use crate::{
    ActorRun, AltChoice, ApprovalEvidence, Assurance, Block, BlockId, Change, ChangeLinks,
    FieldValue, Hash, Justification, LandingRecord, Link, Op, Provenance, RelayEvidence,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The latest encoding of a CHANGE: its links end with `replayed_from` and `briefing`.
pub const CHANGE_ENCODING: u16 = 1;
/// The latest encoding of a LANDING: it ends with `assessment` and `policy`.
pub const LANDING_ENCODING: u16 = 1;
/// The latest encoding of a REDACTION_RESIDUE: a block residue's content hash is the content-2
/// hash, and each residue ends with its removed object's `source_encoding`.
pub const RESIDUE_ENCODING: u16 = 1;

/// The identity of a stored object: the hash of its stored payload and its encoding version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EncodedIdentity {
    pub hash: Hash,
    pub version: u16,
}

/// The hash of an object of encoding `version` whose body is `body`: `Hash::of(body)` for
/// version 0, and `Hash::of(version.to_le_bytes() || body)` from version 1 on, the hash of the
/// stored payload in both cases.
pub fn encoded_hash(version: u16, body: &[u8]) -> Hash {
    if version == 0 {
        return Hash::of(body);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&version.to_le_bytes());
    hasher.update(body);
    Hash(*hasher.finalize().as_bytes())
}

/// The content-1 hash of `block`: the domain `rdy-content-1\0` and the content encoding that
/// kept the order key, with the derived and engine fields left out and no tracking fields. It
/// serves only the proof check of an encoding-0 redaction residue (D-69AY3), which an rdy
/// before content-2 wrote with this hash of the intact block. No pin, tension, dismissal, plan
/// answer, pair key or premise ever calls it: those are content-2 only.
#[doc(hidden)]
pub fn legacy_content_1_hash(block: &Block) -> Hash {
    #[derive(serde::Serialize)]
    struct Content1<'a> {
        id: &'a BlockId,
        block_type: &'a String,
        parent: &'a Option<BlockId>,
        order: &'a String,
        fields: Vec<(&'a String, &'a FieldValue)>,
        body: &'a String,
        links: &'a BTreeMap<String, Vec<Link>>,
    }
    let content = Content1 {
        id: &block.id,
        block_type: &block.block_type,
        parent: &block.parent,
        order: &block.order,
        fields: block
            .fields
            .iter()
            .filter(|(k, _)| !crate::block::DERIVED_FIELDS.contains(&k.as_str()))
            .filter(|(k, _)| !crate::engine_fields::is_engine_field(k))
            .collect(),
        body: &block.body,
        links: &block.links,
    };
    let mut bytes = b"rdy-content-1\0".to_vec();
    bytes.extend(
        postcard::to_allocvec(&content)
            .expect("postcard encoding of an in-memory value cannot fail"),
    );
    Hash::of(&bytes)
}

/// A change's links in encoding 0: the four options it had then.
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyChangeLinksV0 {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
    pub imported_by: Option<ActorRun>,
}

impl LegacyChangeLinksV0 {
    /// The current links: the encoding-1 options absent.
    pub fn into_current(self) -> ChangeLinks {
        ChangeLinks {
            intent: self.intent,
            alternative: self.alternative,
            client_nonce: self.client_nonce,
            imported_by: self.imported_by,
            replayed_from: None,
            briefing: None,
        }
    }

    /// The encoding-0 links of `l`, while its encoding-1 options are absent.
    pub fn from_current(l: &ChangeLinks) -> Option<LegacyChangeLinksV0> {
        if l.replayed_from.is_some() || l.briefing.is_some() {
            return None;
        }
        Some(LegacyChangeLinksV0 {
            intent: l.intent,
            alternative: l.alternative.clone(),
            client_nonce: l.client_nonce.clone(),
            imported_by: l.imported_by.clone(),
        })
    }
}

/// A change in encoding 0: the six fields and the four-option links.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyChangeV0 {
    pub view: String,
    pub ops: Vec<Op>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: LegacyChangeLinksV0,
}

impl LegacyChangeV0 {
    /// The current change, with the stored read identity `identity` of the frame it came from.
    pub fn into_current(self, identity: EncodedIdentity) -> Change {
        Change {
            view: self.view,
            ops: self.ops,
            deps: self.deps,
            provenance: self.provenance,
            justification: self.justification,
            message: self.message,
            links: self.links.into_current(),
            read_identity: Some(ChangeReadIdentity::Stored(identity)),
        }
    }

    /// The encoding-0 body of `c`, while its encoding-1 link options are absent.
    pub fn from_current(c: &Change) -> Option<LegacyChangeV0> {
        Some(LegacyChangeV0 {
            view: c.view.clone(),
            ops: c.ops.clone(),
            deps: c.deps.clone(),
            provenance: c.provenance.clone(),
            justification: c.justification.clone(),
            message: c.message.clone(),
            links: LegacyChangeLinksV0::from_current(&c.links)?,
        })
    }
}

/// A landing in encoding 0: the thirteen fields it had then.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyLandingV0 {
    pub proposal: Hash,
    pub prev: Option<Hash>,
    pub state: Hash,
    pub approved_by: Provenance,
    pub changes: Vec<Hash>,
    pub chosen: Option<AltChoice>,
    pub checks: Vec<Hash>,
    pub assurance: Assurance,
    pub relayed_by: Option<ActorRun>,
    pub relay_evidence: Option<RelayEvidence>,
    pub approved_version: Option<Hash>,
    pub rebased_from: Vec<Hash>,
    pub evidence: Option<ApprovalEvidence>,
}

impl LegacyLandingV0 {
    /// The current landing, with the stored read identity `identity`.
    pub fn into_current(self, identity: EncodedIdentity) -> LandingRecord {
        LandingRecord {
            proposal: self.proposal,
            prev: self.prev,
            state: self.state,
            approved_by: self.approved_by,
            changes: self.changes,
            chosen: self.chosen,
            checks: self.checks,
            assurance: self.assurance,
            relayed_by: self.relayed_by,
            relay_evidence: self.relay_evidence,
            approved_version: self.approved_version,
            rebased_from: self.rebased_from,
            evidence: self.evidence,
            assessment: None,
            policy: None,
            read_identity: Some(identity),
        }
    }

    /// The encoding-0 body of `l`, while its encoding-1 fields are absent.
    pub fn from_current(l: &LandingRecord) -> Option<LegacyLandingV0> {
        if l.assessment.is_some() || l.policy.is_some() {
            return None;
        }
        Some(LegacyLandingV0 {
            proposal: l.proposal,
            prev: l.prev,
            state: l.state,
            approved_by: l.approved_by.clone(),
            changes: l.changes.clone(),
            chosen: l.chosen.clone(),
            checks: l.checks.clone(),
            assurance: l.assurance,
            relayed_by: l.relayed_by.clone(),
            relay_evidence: l.relay_evidence.clone(),
            approved_version: l.approved_version,
            rebased_from: l.rebased_from.clone(),
            evidence: l.evidence.clone(),
        })
    }
}

/// A block residue in encoding 0: no source encoding.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyBlockResidueV0 {
    pub version: Hash,
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub order: String,
    pub content_hash: Hash,
    pub frozen: bool,
}

/// A change residue in encoding 0: the four-option links and no source encoding.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyChangeResidueV0 {
    pub change: Hash,
    pub view: String,
    pub ops: Vec<ResidualOp>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: LegacyChangeLinksV0,
}

/// A redaction residue in encoding 0. Its block residue holds the content-1 hash an rdy before
/// content-2 recorded.
#[doc(hidden)]
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyRedactionResidueV0 {
    Block(LegacyBlockResidueV0),
    Change(LegacyChangeResidueV0),
}

impl LegacyRedactionResidueV0 {
    /// The current residue: absent link options and source encoding 0, the only encoding an
    /// object had before encodings existed.
    pub fn into_current(self) -> RedactionResidue {
        match self {
            LegacyRedactionResidueV0::Block(b) => RedactionResidue::Block(BlockResidue {
                version: b.version,
                id: b.id,
                block_type: b.block_type,
                parent: b.parent,
                order: b.order,
                content_hash: b.content_hash,
                frozen: b.frozen,
                source_encoding: 0,
            }),
            LegacyRedactionResidueV0::Change(c) => RedactionResidue::Change(ChangeResidue {
                change: c.change,
                view: c.view,
                ops: c.ops,
                deps: c.deps,
                provenance: c.provenance,
                justification: c.justification,
                message: c.message,
                links: c.links.into_current(),
                source_encoding: 0,
            }),
        }
    }

    /// The encoding-0 body of `r`, while its source encoding is 0 and its encoding-1 link
    /// options are absent.
    pub fn from_current(r: &RedactionResidue) -> Option<LegacyRedactionResidueV0> {
        Some(match r {
            RedactionResidue::Block(b) => {
                if b.source_encoding != 0 {
                    return None;
                }
                LegacyRedactionResidueV0::Block(LegacyBlockResidueV0 {
                    version: b.version,
                    id: b.id.clone(),
                    block_type: b.block_type.clone(),
                    parent: b.parent.clone(),
                    order: b.order.clone(),
                    content_hash: b.content_hash,
                    frozen: b.frozen,
                })
            }
            RedactionResidue::Change(c) => {
                if c.source_encoding != 0 {
                    return None;
                }
                LegacyRedactionResidueV0::Change(LegacyChangeResidueV0 {
                    change: c.change,
                    view: c.view.clone(),
                    ops: c.ops.clone(),
                    deps: c.deps.clone(),
                    provenance: c.provenance.clone(),
                    justification: c.justification.clone(),
                    message: c.message.clone(),
                    links: LegacyChangeLinksV0::from_current(&c.links)?,
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok1_encoded_hash_covers_the_version_prefix() {
        let body = b"a body";
        assert_eq!(encoded_hash(0, body), Hash::of(body));
        let mut framed = 1u16.to_le_bytes().to_vec();
        framed.extend_from_slice(body);
        assert_eq!(encoded_hash(1, body), Hash::of(&framed));
        assert_ne!(encoded_hash(1, body), encoded_hash(2, body));
        assert_ne!(encoded_hash(1, body), Hash::of(body));
    }
}
