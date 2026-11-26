//! One record's typed value, decoded once (D-3PNRK; D-D2NPN verify-on-receive): the bounded
//! decode (`crate::bounded`) of one portable record, through the decoder its encoding selects
//! (`crate::codec`, `crate::plan_record`), that every check of the record reads.
//!
//! A batch check ([`crate::sync_stream::check_batch_with`]) decodes each present record into a
//! [`DecodedRecord`] once, after its frame and hash pass. Its edges
//! (`crate::object_refs::portable_refs_of`), its kind's record rules, the features it uses
//! (`crate::format::required_features`) and the caller's own check (the engine's local limits)
//! all read that one value; none of them decodes the bytes again. The value holds at most one
//! record, within the record bounds, and is dropped before the next frame is read. The record's
//! raw kind, encoding and original bytes stay the [`StreamRecord`]'s: a hash or a canonical-byte
//! check compares those bytes, never a re-encoding of the value.
//!
//! The kinds are closed: one variant per portable kind (`crate::object_refs::PORTABLE_KINDS`),
//! and [`DecodedRecord::Dropped`] for a record whose payload a removal dropped. A BLOB_CHUNK has
//! no typed value: its bytes stay the record's, checked against the chunk bound.
use crate::bounded::{self, CHUNK_BYTES, MAX_CHUNK_BYTES, RecordError};
use crate::codec::{self, ObjectBytes};
use crate::kinds::{self, BlobManifest};
use crate::plan_record::PlanRecord;
use crate::sync_stream::StreamRecord;
use crate::transition::TransitionsV1;
use crate::view::ViewOp;
use rdy_model::assessment::AssessmentV2;
use rdy_model::check_request::CheckRequest;
use rdy_model::dispute::{Dispute, DisputeResolution};
use rdy_model::redaction::{Redaction, RedactionApply, RedactionResidue};
use rdy_model::review::ReviewV1;
use rdy_model::semantic::SemanticTensionV1;
use rdy_model::signature::Signature;
use rdy_model::{
    AltSet, Answer, Block, Change, Intent, LandingRecord, Proposal, SnapshotDelta, StateSnapshot,
    Tag,
};

/// The typed value of one record: one variant per portable kind, each the type its edges, record
/// rules and features read; [`DecodedRecord::Dropped`] for a dropped record, which has no
/// payload. The values are boxed, so the record is small whatever its kind.
#[derive(Debug)]
pub enum DecodedRecord {
    Block(Box<Block>),
    Change(Box<Change>),
    Proposal(Box<Proposal>),
    Landing(Box<LandingRecord>),
    Snapshot(Box<StateSnapshot>),
    /// A blob chunk: its bytes are the record's own, with no typed value.
    BlobChunk,
    BlobManifest(Box<BlobManifest>),
    Tag(Box<Tag>),
    Answer(Box<Answer>),
    Signature(Box<Signature>),
    ViewOp(Box<ViewOp>),
    Intent(Box<Intent>),
    AltSet(Box<AltSet>),
    /// A review in its wire type, which reads every encoding of the kind.
    Review(Box<ReviewV1>),
    /// A semantic tension in its wire type, which reads every encoding of the kind.
    Tension(Box<SemanticTensionV1>),
    /// A transition in its wire type, which reads every encoding of the kind.
    Transition(Box<TransitionsV1>),
    Redaction(Box<Redaction>),
    RedactionResidue(Box<RedactionResidue>),
    RedactionApply(Box<RedactionApply>),
    /// A whole plan version or a delta, by its own frame: no predecessor is folded.
    Plan(Box<PlanRecord>),
    /// An assessment in its wire type, which reads every encoding of the kind.
    Assessment(Box<AssessmentV2>),
    Dispute(Box<Dispute>),
    DisputeResolution(Box<DisputeResolution>),
    CheckRequest(Box<CheckRequest>),
    SnapshotDelta(Box<SnapshotDelta>),
    /// A record whose payload an approved removal dropped: no value.
    Dropped,
}

impl DecodedRecord {
    /// The typed value of `record`: a present record's frame decoded within its bounds
    /// ([`DecodedRecord::decode_frame`]), or [`DecodedRecord::Dropped`].
    pub fn decode(record: &StreamRecord) -> Result<DecodedRecord, RecordError> {
        match record {
            StreamRecord::Present { payload, .. } => {
                DecodedRecord::decode_frame(record.raw_kind(), payload)
            }
            StreamRecord::Dropped { .. } => Ok(DecodedRecord::Dropped),
        }
    }

    /// The typed value of the frame of raw kind `raw` and stored payload `payload`. `Kind` for a
    /// kind that is not an assigned portable kind; then the codec's own refusals of the frame's
    /// encoding; then `Decode` or `Limit` for a body that is not exactly one bounded record of
    /// the kind in that encoding.
    pub fn decode_frame(raw: u8, payload: &[u8]) -> Result<DecodedRecord, RecordError> {
        if !crate::object_refs::is_portable(codec::logical_kind(raw)) {
            return Err(RecordError::Kind);
        }
        DecodedRecord::decode_split(&codec::split(raw, payload)?)
    }

    /// The typed value of split frame `o`, through the one decoder its kind and encoding select.
    pub(crate) fn decode_split(o: &ObjectBytes<'_>) -> Result<DecodedRecord, RecordError> {
        let body = o.body;
        Ok(match o.kind {
            kinds::BLOCK => DecodedRecord::Block(Box::new(codec::decode_bounded_split(o)?)),
            kinds::CHANGE => DecodedRecord::Change(Box::new(codec::decode_bounded_split(o)?)),
            kinds::PROPOSAL => DecodedRecord::Proposal(Box::new(bounded::decode(body)?)),
            kinds::LANDING => DecodedRecord::Landing(Box::new(codec::decode_bounded_split(o)?)),
            kinds::SNAPSHOT => DecodedRecord::Snapshot(Box::new(bounded::decode(body)?)),
            kinds::BLOB_CHUNK => {
                if body.len() > MAX_CHUNK_BYTES {
                    return Err(RecordError::limit(CHUNK_BYTES, MAX_CHUNK_BYTES));
                }
                DecodedRecord::BlobChunk
            }
            kinds::BLOB_MANIFEST => DecodedRecord::BlobManifest(Box::new(bounded::decode(body)?)),
            kinds::TAG => DecodedRecord::Tag(Box::new(bounded::decode(body)?)),
            kinds::ANSWER => DecodedRecord::Answer(Box::new(codec::decode_bounded_split(o)?)),
            kinds::SIGNATURE => DecodedRecord::Signature(Box::new(bounded::decode(body)?)),
            kinds::VIEW_OP => DecodedRecord::ViewOp(Box::new(bounded::decode(body)?)),
            kinds::INTENT => DecodedRecord::Intent(Box::new(bounded::decode(body)?)),
            kinds::ALT_SET => DecodedRecord::AltSet(Box::new(bounded::decode(body)?)),
            kinds::REVIEW => DecodedRecord::Review(Box::new(codec::decode_bounded_split(o)?)),
            kinds::TENSION => DecodedRecord::Tension(Box::new(codec::decode_bounded_split(o)?)),
            kinds::TRANSITION => {
                DecodedRecord::Transition(Box::new(codec::decode_bounded_split(o)?))
            }
            kinds::REDACTION => DecodedRecord::Redaction(Box::new(codec::decode_bounded_split(o)?)),
            kinds::REDACTION_RESIDUE => {
                DecodedRecord::RedactionResidue(Box::new(codec::decode_bounded_split(o)?))
            }
            kinds::REDACTION_APPLY => {
                DecodedRecord::RedactionApply(Box::new(bounded::decode(body)?))
            }
            kinds::PLAN => DecodedRecord::Plan(Box::new(
                crate::plan_record::decode_body(o.version, body)?.0,
            )),
            kinds::ASSESSMENT => {
                DecodedRecord::Assessment(Box::new(codec::decode_bounded_split(o)?))
            }
            kinds::DISPUTE => DecodedRecord::Dispute(Box::new(codec::decode_bounded_split(o)?)),
            kinds::DISPUTE_RESOLUTION => {
                DecodedRecord::DisputeResolution(Box::new(codec::decode_bounded_split(o)?))
            }
            kinds::CHECK_REQUEST => {
                DecodedRecord::CheckRequest(Box::new(codec::decode_bounded_split(o)?))
            }
            kinds::SNAPSHOT_DELTA => {
                DecodedRecord::SnapshotDelta(Box::new(codec::decode_bounded_split(o)?))
            }
            _ => return Err(RecordError::Kind),
        })
    }

    /// The logical kind of the value; None for a dropped record.
    pub fn kind(&self) -> Option<u8> {
        Some(match self {
            DecodedRecord::Block(_) => kinds::BLOCK,
            DecodedRecord::Change(_) => kinds::CHANGE,
            DecodedRecord::Proposal(_) => kinds::PROPOSAL,
            DecodedRecord::Landing(_) => kinds::LANDING,
            DecodedRecord::Snapshot(_) => kinds::SNAPSHOT,
            DecodedRecord::BlobChunk => kinds::BLOB_CHUNK,
            DecodedRecord::BlobManifest(_) => kinds::BLOB_MANIFEST,
            DecodedRecord::Tag(_) => kinds::TAG,
            DecodedRecord::Answer(_) => kinds::ANSWER,
            DecodedRecord::Signature(_) => kinds::SIGNATURE,
            DecodedRecord::ViewOp(_) => kinds::VIEW_OP,
            DecodedRecord::Intent(_) => kinds::INTENT,
            DecodedRecord::AltSet(_) => kinds::ALT_SET,
            DecodedRecord::Review(_) => kinds::REVIEW,
            DecodedRecord::Tension(_) => kinds::TENSION,
            DecodedRecord::Transition(_) => kinds::TRANSITION,
            DecodedRecord::Redaction(_) => kinds::REDACTION,
            DecodedRecord::RedactionResidue(_) => kinds::REDACTION_RESIDUE,
            DecodedRecord::RedactionApply(_) => kinds::REDACTION_APPLY,
            DecodedRecord::Plan(_) => kinds::PLAN,
            DecodedRecord::Assessment(_) => kinds::ASSESSMENT,
            DecodedRecord::Dispute(_) => kinds::DISPUTE,
            DecodedRecord::DisputeResolution(_) => kinds::DISPUTE_RESOLUTION,
            DecodedRecord::CheckRequest(_) => kinds::CHECK_REQUEST,
            DecodedRecord::SnapshotDelta(_) => kinds::SNAPSHOT_DELTA,
            DecodedRecord::Dropped => return None,
        })
    }
}
