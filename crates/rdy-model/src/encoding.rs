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
//!
//! A block's foreign part (repo links) never enters a released layout. [`LegacyBlockV0`] freezes
//! the encoding-0 block layout that `Block`'s own serde writes, so a BLOCK of encoding 0 and the
//! blocks every CHANGE of encoding 0 or 1 embeds keep their bytes; neither has a form for a block
//! with a foreign part. Such a block has its own explicit layouts: [`ForeignBlockV1`]
//! ([`BLOCK_FOREIGN_ENCODING`]) and, embedded, [`ForeignChangeV2`] ([`CHANGE_FOREIGN_ENCODING`]).
//!
//! Repo links register optional encodings beside the released ones (D-FK58A, store feature
//! `foreign_copy`). A value takes its kind's optional encoding only when it holds a shape the
//! released encoding has no form for, and the released encoding otherwise, so every local value
//! keeps its bytes and its hash: BLOCK 1 and CHANGE 2 above, [`TRANSITION_FOREIGN_ENCODING`],
//! [`PAIR_FOREIGN_ENCODING`] (REVIEW and TENSION), [`ANSWER_FOREIGN_ENCODING`],
//! [`ASSESSMENT_FOREIGN_ENCODING`] and [`REDACTION_FOREIGN_ENCODING`]. A FOREIGN entry starts at
//! [`FOREIGN_ENCODING`].
//!
//! A port records its source in `ChangeLinks::ported_from`, the seventh link field (store
//! feature `ported_change`). Encodings 1 and 2 keep their frozen six link fields
//! ([`ChangeLinksV1`]). A port takes its own encodings, chosen by value: [`PortChangeV3`]
//! ([`CHANGE_PORT_ENCODING`]) and, when it embeds a block with a foreign part,
//! [`PortForeignChangeV4`] ([`CHANGE_PORT_FOREIGN_ENCODING`]). The residue of a removed port takes
//! [`PortResidueV2`] ([`RESIDUE_PORT_ENCODING`]); every other residue keeps encoding 1.
use crate::change::ChangeReadIdentity;
use crate::foreign::ForeignParts;
use crate::redaction::{BlockResidue, ChangeResidue, RedactionResidue, ResidualOp};
use crate::{
    ActorRun, AltChoice, ApprovalEvidence, Assurance, Block, BlockId, Change, ChangeLinks,
    FieldValue, Hash, Justification, LandingRecord, Link, Op, PortedFrom, Provenance,
    RelayEvidence,
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

/// The encoding of a BLOCK with a foreign part (repo links): [`ForeignBlockV1`]. A block with an
/// empty foreign part has encoding 0 alone, the released [`LegacyBlockV0`] layout.
pub const BLOCK_FOREIGN_ENCODING: u16 = 1;
/// The encoding of a CHANGE that embeds a block with a foreign part: [`ForeignChangeV2`].
/// Encodings 0 and 1 embed the frozen [`LegacyBlockV0`] layout and have no form for such a block.
pub const CHANGE_FOREIGN_ENCODING: u16 = 2;
/// The encoding of a ported CHANGE that embeds no block with a foreign part: [`PortChangeV3`].
pub const CHANGE_PORT_ENCODING: u16 = 3;
/// The encoding of a ported CHANGE that embeds a block with a foreign part:
/// [`PortForeignChangeV4`].
pub const CHANGE_PORT_FOREIGN_ENCODING: u16 = 4;
/// The encoding of the REDACTION_RESIDUE of a removed port: [`PortResidueV2`]. Block residues
/// and the residues of unported changes keep [`RESIDUE_ENCODING`].
pub const RESIDUE_PORT_ENCODING: u16 = 2;
/// The first and only encoding of a FOREIGN entry (`crate::foreign::ForeignEntry`, kind 29): the
/// kind has no encoding 0.
pub const FOREIGN_ENCODING: u16 = 1;
/// The first and only encoding of a SNAPSHOT_DELTA (`crate::SnapshotDelta`, kind 30): the kind
/// has no encoding 0. Its id is the hash of the stored payload, this prefix included.
pub const SNAPSHOT_DELTA_ENCODING: u16 = 1;
/// The encoding of a TRANSITION that holds a foreign shape: a move of cause `Delivered`, a
/// foreign tension side or a foreign suspect target (the store's `TransitionsV1`). Encoding 0
/// has no form for any of them.
pub const TRANSITION_FOREIGN_ENCODING: u16 = 1;
/// The encoding of a REVIEW or a TENSION with a foreign pair side
/// ([`crate::review::ReviewV1`], [`crate::semantic::SemanticTensionV1`]). Encoding 0 keeps the
/// released `PairItem` layout and every local pair key.
pub const PAIR_FOREIGN_ENCODING: u16 = 1;
/// The encoding of an ANSWER whose act is [`crate::AnswerAct::Adopt`] or
/// [`crate::AnswerAct::KeepPin`]: the encoding-0 body, whose act may then be one of those two.
pub const ANSWER_FOREIGN_ENCODING: u16 = 1;
/// The encoding of an ASSESSMENT with foreign reach or foreign tension evidence
/// ([`crate::assessment::AssessmentV2`]). Encoding 1 stays the local layout.
pub const ASSESSMENT_FOREIGN_ENCODING: u16 = 2;
/// The encoding of a REDACTION with a `foreign_version` target: the encoding-0 body, whose
/// target list may then hold one.
pub const REDACTION_FOREIGN_ENCODING: u16 = 1;

/// The encoding of a PLAN written by this rdy: a `crate::plan_encoding::PlanRecordV1`, either a
/// whole version or the difference from the version before it. Chosen by the plan writer, never
/// by the kind's latest encoding, which stays 0 (store feature `plan_delta`).
pub const PLAN_DELTA_ENCODING: u16 = 1;

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

/// A block in encoding 0, the released layout: the eight fields it has had since the first
/// store, and no foreign part. [`Block`]'s own serde reads and writes exactly this layout, so a
/// BLOCK of encoding 0 and every block a CHANGE of encoding 0 or 1 embeds keep their bytes.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Block")]
pub struct LegacyBlockV0 {
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub order: String,
    pub fields: BTreeMap<String, FieldValue>,
    pub body: String,
    pub links: BTreeMap<String, Vec<Link>>,
    pub pins: BTreeMap<BlockId, Hash>,
}

impl LegacyBlockV0 {
    /// The current block: an empty foreign part and no read identity.
    pub fn into_current(self) -> Block {
        Block {
            id: self.id,
            block_type: self.block_type,
            parent: self.parent,
            order: self.order,
            fields: self.fields,
            body: self.body,
            links: self.links,
            pins: self.pins,
            foreign: ForeignParts::default(),
            read_identity: None,
        }
    }

    /// The encoding-0 body of `b`, while its foreign part is empty.
    pub fn from_current(b: &Block) -> Option<LegacyBlockV0> {
        if !b.foreign.is_empty() {
            return None;
        }
        Some(LegacyBlockV0 {
            id: b.id.clone(),
            block_type: b.block_type.clone(),
            parent: b.parent.clone(),
            order: b.order.clone(),
            fields: b.fields.clone(),
            body: b.body.clone(),
            links: b.links.clone(),
            pins: b.pins.clone(),
        })
    }
}

/// A block in [`BLOCK_FOREIGN_ENCODING`]: the eight encoding-0 fields and then the foreign part,
/// a new layout, never an option appended to the released one. A BLOCK object takes it only when
/// its foreign part is not empty; a CHANGE of [`CHANGE_FOREIGN_ENCODING`] embeds every block in
/// it, with its foreign part empty or not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignBlockV1 {
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub order: String,
    pub fields: BTreeMap<String, FieldValue>,
    pub body: String,
    pub links: BTreeMap<String, Vec<Link>>,
    pub pins: BTreeMap<BlockId, Hash>,
    pub foreign: ForeignParts,
}

impl ForeignBlockV1 {
    /// The layout of `b`, its foreign part as it is.
    pub fn of(b: &Block) -> ForeignBlockV1 {
        ForeignBlockV1 {
            id: b.id.clone(),
            block_type: b.block_type.clone(),
            parent: b.parent.clone(),
            order: b.order.clone(),
            fields: b.fields.clone(),
            body: b.body.clone(),
            links: b.links.clone(),
            pins: b.pins.clone(),
            foreign: b.foreign.clone(),
        }
    }

    /// The current block, with no read identity.
    pub fn into_current(self) -> Block {
        Block {
            id: self.id,
            block_type: self.block_type,
            parent: self.parent,
            order: self.order,
            fields: self.fields,
            body: self.body,
            links: self.links,
            pins: self.pins,
            foreign: self.foreign,
            read_identity: None,
        }
    }
}

/// An op of a change in [`CHANGE_FOREIGN_ENCODING`] or [`CHANGE_PORT_FOREIGN_ENCODING`]:
/// [`Op`]'s variants in their order, each block in the [`ForeignBlockV1`] layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForeignOpV2 {
    Create { block: ForeignBlockV1 },
    Update { base: Hash, block: ForeignBlockV1 },
    Delete { base: Hash, id: BlockId },
}

impl ForeignOpV2 {
    /// The layout of `op`.
    pub fn of(op: &Op) -> ForeignOpV2 {
        match op {
            Op::Create { block } => ForeignOpV2::Create {
                block: ForeignBlockV1::of(block),
            },
            Op::Update { base, block } => ForeignOpV2::Update {
                base: *base,
                block: ForeignBlockV1::of(block),
            },
            Op::Delete { base, id } => ForeignOpV2::Delete {
                base: *base,
                id: id.clone(),
            },
        }
    }

    /// The runtime op.
    pub fn into_current(self) -> Op {
        match self {
            ForeignOpV2::Create { block } => Op::Create {
                block: block.into_current(),
            },
            ForeignOpV2::Update { base, block } => Op::Update {
                base,
                block: block.into_current(),
            },
            ForeignOpV2::Delete { base, id } => Op::Delete { base, id },
        }
    }
}

/// A change's links in encodings 1 and 2, frozen: the six options up to `briefing`. It is the
/// postcard form of [`ChangeLinks`]; a port's links have no form here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "ChangeLinks")]
pub struct ChangeLinksV1 {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
    pub imported_by: Option<ActorRun>,
    pub replayed_from: Option<Hash>,
    pub briefing: Option<Hash>,
}

impl ChangeLinksV1 {
    /// The current links: no port source.
    pub fn into_current(self) -> ChangeLinks {
        ChangeLinks {
            intent: self.intent,
            alternative: self.alternative,
            client_nonce: self.client_nonce,
            imported_by: self.imported_by,
            replayed_from: self.replayed_from,
            briefing: self.briefing,
            ported_from: None,
        }
    }

    /// The encoding-1 links of `l`, while it records no port source.
    pub fn from_current(l: &ChangeLinks) -> Option<ChangeLinksV1> {
        if l.ported_from.is_some() {
            return None;
        }
        Some(ChangeLinksV1 {
            intent: l.intent,
            alternative: l.alternative.clone(),
            client_nonce: l.client_nonce.clone(),
            imported_by: l.imported_by.clone(),
            replayed_from: l.replayed_from,
            briefing: l.briefing,
        })
    }
}

/// A port's links in encodings 3 and 4: the six frozen options, then `ported_from` as the
/// seventh field. A reader refuses a port body whose `ported_from` is absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "ChangeLinks")]
pub struct PortLinksV3 {
    pub intent: Option<Hash>,
    pub alternative: Option<AltChoice>,
    pub client_nonce: Option<String>,
    pub imported_by: Option<ActorRun>,
    pub replayed_from: Option<Hash>,
    pub briefing: Option<Hash>,
    pub ported_from: Option<PortedFrom>,
}

impl PortLinksV3 {
    /// The layout of `l`.
    pub fn of(l: &ChangeLinks) -> PortLinksV3 {
        PortLinksV3 {
            intent: l.intent,
            alternative: l.alternative.clone(),
            client_nonce: l.client_nonce.clone(),
            imported_by: l.imported_by.clone(),
            replayed_from: l.replayed_from,
            briefing: l.briefing,
            ported_from: l.ported_from,
        }
    }

    /// The current links.
    pub fn into_current(self) -> ChangeLinks {
        ChangeLinks {
            intent: self.intent,
            alternative: self.alternative,
            client_nonce: self.client_nonce,
            imported_by: self.imported_by,
            replayed_from: self.replayed_from,
            briefing: self.briefing,
            ported_from: self.ported_from,
        }
    }
}

/// A change in [`CHANGE_FOREIGN_ENCODING`]: the fields of encoding 1 in their order, its ops
/// embedding [`ForeignBlockV1`] blocks. Its links are the frozen six fields: the postcard form of
/// [`ChangeLinks`] is [`ChangeLinksV1`]. Encoding 1's body stays unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignChangeV2 {
    pub view: String,
    pub ops: Vec<ForeignOpV2>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: ChangeLinks,
}

impl ForeignChangeV2 {
    /// The encoding-2 body of `c`.
    pub fn of(c: &Change) -> ForeignChangeV2 {
        ForeignChangeV2 {
            view: c.view.clone(),
            ops: c.ops.iter().map(ForeignOpV2::of).collect(),
            deps: c.deps.clone(),
            provenance: c.provenance.clone(),
            justification: c.justification.clone(),
            message: c.message.clone(),
            links: c.links.clone(),
        }
    }

    /// The current change, with the stored read identity `identity` of the frame it came from.
    pub fn into_current(self, identity: EncodedIdentity) -> Change {
        Change {
            view: self.view,
            ops: self
                .ops
                .into_iter()
                .map(ForeignOpV2::into_current)
                .collect(),
            deps: self.deps,
            provenance: self.provenance,
            justification: self.justification,
            message: self.message,
            links: self.links,
            read_identity: Some(ChangeReadIdentity::Stored(identity)),
        }
    }
}

/// A port in [`CHANGE_PORT_ENCODING`]: the fields of encoding 1 in their order, its links
/// [`PortLinksV3`]. Encoding 1's body stays unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortChangeV3 {
    pub view: String,
    pub ops: Vec<Op>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: PortLinksV3,
}

impl PortChangeV3 {
    /// The encoding-3 body of `c`.
    pub fn of(c: &Change) -> PortChangeV3 {
        PortChangeV3 {
            view: c.view.clone(),
            ops: c.ops.clone(),
            deps: c.deps.clone(),
            provenance: c.provenance.clone(),
            justification: c.justification.clone(),
            message: c.message.clone(),
            links: PortLinksV3::of(&c.links),
        }
    }

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
}

/// A port in [`CHANGE_PORT_FOREIGN_ENCODING`]: the fields of encoding 2 in their order, its ops
/// embedding [`ForeignBlockV1`] blocks and its links [`PortLinksV3`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForeignChangeV4 {
    pub view: String,
    pub ops: Vec<ForeignOpV2>,
    pub deps: Vec<Hash>,
    pub provenance: Provenance,
    pub justification: Option<Justification>,
    pub message: String,
    pub links: PortLinksV3,
}

impl PortForeignChangeV4 {
    /// The encoding-4 body of `c`.
    pub fn of(c: &Change) -> PortForeignChangeV4 {
        PortForeignChangeV4 {
            view: c.view.clone(),
            ops: c.ops.iter().map(ForeignOpV2::of).collect(),
            deps: c.deps.clone(),
            provenance: c.provenance.clone(),
            justification: c.justification.clone(),
            message: c.message.clone(),
            links: PortLinksV3::of(&c.links),
        }
    }

    /// The current change, with the stored read identity `identity` of the frame it came from.
    pub fn into_current(self, identity: EncodedIdentity) -> Change {
        Change {
            view: self.view,
            ops: self
                .ops
                .into_iter()
                .map(ForeignOpV2::into_current)
                .collect(),
            deps: self.deps,
            provenance: self.provenance,
            justification: self.justification,
            message: self.message,
            links: self.links.into_current(),
            read_identity: Some(ChangeReadIdentity::Stored(identity)),
        }
    }
}

/// The body of change `c` in encoding `version`. Encoding 0 is the frozen [`LegacyChangeV0`];
/// encodings 1 to 4 each hold one shape, and `c` must choose that encoding by its value
/// ([`Change::encoding`]). None when `c` has no form in `version`.
pub fn change_body(c: &Change, version: u16) -> Option<Vec<u8>> {
    let body = match version {
        0 => postcard::to_allocvec(&LegacyChangeV0::from_current(c)?),
        _ if c.encoding() != version => return None,
        CHANGE_ENCODING => postcard::to_allocvec(c),
        CHANGE_FOREIGN_ENCODING => postcard::to_allocvec(&ForeignChangeV2::of(c)),
        CHANGE_PORT_ENCODING => postcard::to_allocvec(&PortChangeV3::of(c)),
        CHANGE_PORT_FOREIGN_ENCODING => postcard::to_allocvec(&PortForeignChangeV4::of(c)),
        _ => return None,
    };
    Some(body.expect("postcard encoding of an in-memory value cannot fail"))
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
    /// The current links: the later options absent.
    pub fn into_current(self) -> ChangeLinks {
        ChangeLinks {
            intent: self.intent,
            alternative: self.alternative,
            client_nonce: self.client_nonce,
            imported_by: self.imported_by,
            replayed_from: None,
            briefing: None,
            ported_from: None,
        }
    }

    /// The encoding-0 links of `l`, while its later options are absent.
    pub fn from_current(l: &ChangeLinks) -> Option<LegacyChangeLinksV0> {
        if l.replayed_from.is_some() || l.briefing.is_some() || l.ported_from.is_some() {
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

    /// The encoding-0 body of `c`, while its encoding-1 link options are absent and every block
    /// it embeds has an empty foreign part.
    pub fn from_current(c: &Change) -> Option<LegacyChangeV0> {
        if c.ops.iter().any(|op| match op {
            Op::Create { block } | Op::Update { block, .. } => !block.foreign.is_empty(),
            Op::Delete { .. } => false,
        }) {
            return None;
        }
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

/// A REDACTION_RESIDUE in [`RESIDUE_PORT_ENCODING`]: the encoding-1 body of a change residue,
/// its `source_encoding` included, then the removed port's `ported_from`. Only the residue of a
/// port takes it; a reader refuses one that names no port source or holds a block residue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortResidueV2 {
    /// The encoding-1 body: its links hold no port source.
    pub residue: RedactionResidue,
    pub ported_from: Option<PortedFrom>,
}

impl PortResidueV2 {
    /// The encoding-2 body of `r`, when `r` is the residue of a port.
    pub fn of(r: &RedactionResidue) -> Option<PortResidueV2> {
        let RedactionResidue::Change(c) = r else {
            return None;
        };
        let ported_from = c.links.ported_from?;
        let mut body = c.clone();
        body.links.ported_from = None;
        Some(PortResidueV2 {
            residue: RedactionResidue::Change(body),
            ported_from: Some(ported_from),
        })
    }

    /// The current residue: a change residue's links regain `ported_from`. A block residue
    /// ignores it, and its encoding then tells the reader to refuse it.
    pub fn into_current(self) -> RedactionResidue {
        match self.residue {
            RedactionResidue::Change(mut c) => {
                c.links.ported_from = self.ported_from;
                RedactionResidue::Change(c)
            }
            block => block,
        }
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
