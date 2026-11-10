//! Object encodings (D-WWAK7: "Each object records its encoding version"; D-F0904). The one place
//! that turns a stored frame's raw kind and payload into a logical kind, an encoding version and
//! a body, and back.
//!
//! The frame header stays `len: u32 LE, kind: u8, hash: [u8; 32]`. An unmarked raw kind is a
//! frame of encoding 0: its payload is the body. A versioned frame's raw kind is
//! `logical_kind | 0x80`, and its payload is `version: u16 LE` and then the postcard body. A
//! marked frame of version 0 is not canonical and is refused. The frame's hash covers the whole
//! stored payload, the version included ([`rdy_model::encoding::encoded_hash`]). Logical kinds
//! stay below 128; REF_UPDATE (8) and BATCH_COMMIT (14) are never marked.
//!
//! Every writer asks [`encode`] for its kind's latest encoding, so no caller picks an obsolete
//! one; an import or a take passes verified raw frames on without re-encoding them. Every typed
//! read goes through [`decode_record`], which decodes a historical body into the current type:
//! a decoded value keeps the identity of the object it came from as metadata
//! ([`rdy_model::encoding::EncodedIdentity`]), never in its postcard or JSON encoding.
//!
//! The registered encodings ([`REGISTERED`]):
//! - CHANGE 1: the links end with `replayed_from` and `briefing`;
//! - LANDING 1: the record ends with `assessment` and `policy`;
//! - REDACTION_RESIDUE 1: a block residue records the content-2 hash (D-TX3KH: order left out),
//!   and every residue ends with its removed object's `source_encoding`;
//! - ASSESSMENT, DISPUTE, DISPUTE_RESOLUTION and CHECK_REQUEST start at encoding 1: they have no
//!   encoding 0.
//!
//! Every other kind's latest encoding is 0. An encoding-0 CHANGE, LANDING or REDACTION_RESIDUE
//! body decodes through its frozen type (`rdy_model::encoding::Legacy*V0`), never directly into
//! the larger current struct, and converts with the new options absent.
//!
//! Repo links (D-FK58A, store feature `foreign_copy`) register FOREIGN at encoding 1 and an
//! optional encoding for eight kinds ([`OPTIONAL`]). The writer chooses by value, never by kind
//! alone: a value takes its kind's optional encoding only when it holds a shape the latest
//! encoding has no form for (a block with a foreign part, a change that embeds one, a delivered
//! transition or a foreign side or target, a foreign pair side, a repo-links answer act, a
//! foreign assessment part, a `foreign_version` target), and its kind's latest encoding
//! otherwise, so every local value keeps its bytes and its hash. A reader holds each frame to
//! the same choice: an optional-encoding frame without such a shape, or a latest-encoding frame
//! with one, does not decode. A runtime type reads an optional-encoding frame only when it can
//! hold its shape; the registered wire types (`ReviewV1`, `SemanticTensionV1`, `AssessmentV2`,
//! `crate::transition::TransitionsV1`) read every encoding of their kind.
//!
//! A typed read of a CHANGE or a LANDING keeps the identity of its frame
//! (`Change::read_identity`, `LandingRecord::read_identity`). [`encode`] writes such a stored read
//! in its own encoding and refuses it when its bytes do not hash to the identity: a caller that
//! edits a read value clears the identity first. A read projection is never written.
use crate::bounded::{self, RecordError};
use crate::error::StoreError;
use crate::kinds;
use crate::transition::{Transitions, TransitionsV1};
use rdy_model::assessment::{Assessment, AssessmentV2};
use rdy_model::encoding::{
    ANSWER_FOREIGN_ENCODING, ASSESSMENT_FOREIGN_ENCODING, BLOCK_FOREIGN_ENCODING,
    CHANGE_FOREIGN_ENCODING, EncodedIdentity, ForeignBlockV1, ForeignChangeV2, LegacyChangeV0,
    LegacyLandingV0, LegacyRedactionResidueV0, PAIR_FOREIGN_ENCODING, REDACTION_FOREIGN_ENCODING,
    TRANSITION_FOREIGN_ENCODING, encoded_hash,
};
use rdy_model::redaction::{Redaction, RedactionResidue};
use rdy_model::review::ReviewV1;
use rdy_model::semantic::SemanticTensionV1;
use rdy_model::{
    Answer, Block, Change, ChangeReadIdentity, Hash, LandingRecord, Review, SemanticTension,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::any::{Any, TypeId};
use std::cell::RefCell;

/// The refusal of a stored read whose bytes no longer hash to its identity: an edited value that
/// kept the identity of the frame it was read from.
pub const IDENTITY_MISMATCH: &str = "stored record identity does not match its bytes";

/// The bit a versioned frame's raw kind sets.
pub const VERSIONED: u8 = 0x80;

/// Checks, before a writer appends it, that the frame of `value` in the encoding the writer
/// chooses for it ([`encode_new`]) fits one record: from version 1 on, the two-byte version
/// prefix counts with the body against [`bounded::MAX_RECORD_BYTES`], as a stream and a dump
/// bound the frame. A value in its kind's latest encoding is counted without building the
/// encoding.
pub fn fits_latest<T: Serialize + 'static>(kind: u8, value: &T) -> Result<(), RecordError> {
    let (version, len) = match shaped_body(kind, value) {
        Some((version, body)) => (version, body.len()),
        None => (
            latest(kind),
            postcard::experimental::serialized_size(value).map_err(|_| RecordError::Decode)?,
        ),
    };
    let prefix = if version == 0 { 0 } else { 2 };
    bounded::record_len(len.saturating_add(prefix))
}

/// One object as the store writes it: its logical kind, its encoding version and its stored
/// payload (the version prefix included from version 1 on).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedObject {
    pub kind: u8,
    pub version: u16,
    pub payload: Vec<u8>,
}

impl EncodedObject {
    /// The frame's raw kind.
    pub fn raw_kind(&self) -> u8 {
        raw_kind(self.kind, self.version)
    }

    /// The object's hash: the stored payload's.
    pub fn hash(&self) -> Hash {
        Hash::of(&self.payload)
    }

    /// The object's identity: its hash and encoding version.
    pub fn identity(&self) -> EncodedIdentity {
        EncodedIdentity {
            hash: self.hash(),
            version: self.version,
        }
    }

    /// The `(raw kind, payload)` pair a store write takes.
    pub fn into_item(self) -> (u8, Vec<u8>) {
        (self.raw_kind(), self.payload)
    }
}

/// One stored frame, split: its logical kind, its encoding version and its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectBytes<'a> {
    pub kind: u8,
    pub version: u16,
    pub body: &'a [u8],
}

/// The raw kind of an object of logical kind `kind` and encoding `version`.
pub fn raw_kind(kind: u8, version: u16) -> u8 {
    if version == 0 { kind } else { kind | VERSIONED }
}

/// The logical kind of raw kind `raw`, by the kind rule alone (no payload is read, so it serves
/// an erased frame too).
pub fn logical_kind(raw: u8) -> u8 {
    raw & !VERSIONED
}

/// Whether `kind` is a local record that is never versioned.
fn never_versioned(kind: u8) -> bool {
    kind == kinds::REF_UPDATE || kind == kinds::BATCH_COMMIT
}

/// The encodings production registers past 0: (logical kind, latest version). A kind not here
/// has encoding 0 alone.
///
/// REDACTION_RESIDUE 1: the body of encoding 0, but a block residue's `content_hash` is the
/// content-2 hash of the removed block (order left out). An encoding-0 block residue holds the
/// content-1 hash an rdy before content-2 recorded; the residue's encoding alone decides which
/// derivation its proof check uses (`crate::redaction::residue_matches`).
///
/// CHANGE 1 and LANDING 1 append their new options; REDACTION_RESIDUE 1 also appends
/// `source_encoding`. The four carried kinds start at 1 ([`first`]).
pub const REGISTERED: &[(u8, u16)] = &[
    (kinds::CHANGE, 1),
    (kinds::LANDING, 1),
    (kinds::REDACTION_RESIDUE, 1),
    (kinds::ASSESSMENT, 1),
    (kinds::DISPUTE, 1),
    (kinds::DISPUTE_RESOLUTION, 1),
    (kinds::CHECK_REQUEST, 1),
    (kinds::FOREIGN, 1),
];

/// The optional encodings repo links register (D-FK58A, store feature `foreign_copy`): (logical
/// kind, version). A value takes one only when it holds a shape its kind's latest encoding has
/// no form for; every other value keeps its kind's latest encoding ([`latest`]). A frame of one
/// needs `foreign_copy` (`crate::format::required_features`).
pub const OPTIONAL: &[(u8, u16)] = &[
    (kinds::BLOCK, BLOCK_FOREIGN_ENCODING),
    (kinds::CHANGE, CHANGE_FOREIGN_ENCODING),
    (kinds::REVIEW, PAIR_FOREIGN_ENCODING),
    (kinds::TENSION, PAIR_FOREIGN_ENCODING),
    (kinds::TRANSITION, TRANSITION_FOREIGN_ENCODING),
    (kinds::ANSWER, ANSWER_FOREIGN_ENCODING),
    (kinds::REDACTION, REDACTION_FOREIGN_ENCODING),
    (kinds::ASSESSMENT, ASSESSMENT_FOREIGN_ENCODING),
];

/// The optional encoding of logical kind `kind` ([`OPTIONAL`]), if it has one.
pub fn optional(kind: u8) -> Option<u16> {
    OPTIONAL.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

/// The newest encoding of logical kind `kind` that this rdy reads: its latest encoding or its
/// optional one, whichever is higher. A frame of a higher version is an unknown encoding.
pub fn newest(kind: u8) -> u16 {
    latest(kind).max(optional(kind).unwrap_or(0))
}

/// The first encoding of logical kind `kind`: 1 for a kind registered after encodings existed
/// (it has no encoding 0), else 0.
pub fn first(kind: u8) -> u16 {
    match kind {
        kinds::ASSESSMENT
        | kinds::DISPUTE
        | kinds::DISPUTE_RESOLUTION
        | kinds::CHECK_REQUEST
        | kinds::FOREIGN => 1,
        _ => 0,
    }
}

thread_local! {
    /// Test-registered encodings of this thread: (logical kind, latest version).
    static TEST_ENCODINGS: RefCell<Vec<(u8, u16)>> = const { RefCell::new(Vec::new()) };
}

/// The latest encoding of logical kind `kind`: what every new object of it is written in, unless
/// the object holds a shape that only the kind's optional encoding has a form for
/// ([`OPTIONAL`]). It is the kind's [`REGISTERED`] version, else 0 (or a higher test-registered
/// one on this thread).
pub fn latest(kind: u8) -> u16 {
    let registered = REGISTERED
        .iter()
        .filter(|(k, _)| *k == kind)
        .map(|(_, v)| *v)
        .max()
        .unwrap_or(0);
    TEST_ENCODINGS.with(|t| {
        t.borrow()
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, v)| *v)
            .fold(registered, u16::max)
    })
}

/// Whether `version` of logical kind `kind` is one only a test hook registers on this thread
/// ([`test_encoding`]): it decodes as encoding 0 does.
fn is_test_version(kind: u8, version: u16) -> bool {
    let registered = REGISTERED
        .iter()
        .filter(|(k, _)| *k == kind)
        .map(|(_, v)| *v)
        .max()
        .unwrap_or(0);
    version > registered && version <= latest(kind)
}

/// Splits a stored frame: an unmarked raw kind is encoding 0 and its whole payload; a marked one
/// needs its two version bytes (`Decode`), a nonzero version (`NoncanonicalVersion`) that its
/// kind registers, latest or optional (`UnknownVersion`), and a kind that may be versioned
/// (`Kind`).
pub fn split(kind: u8, payload: &[u8]) -> Result<ObjectBytes<'_>, RecordError> {
    if kind & VERSIONED == 0 {
        if first(kind) > 0 {
            return Err(RecordError::UnknownVersion);
        }
        return Ok(ObjectBytes {
            kind,
            version: 0,
            body: payload,
        });
    }
    let logical = logical_kind(kind);
    if never_versioned(logical) {
        return Err(RecordError::Kind);
    }
    let Some((v, body)) = payload.split_first_chunk::<2>() else {
        return Err(RecordError::Decode);
    };
    let version = u16::from_le_bytes(*v);
    if version == 0 {
        return Err(RecordError::NoncanonicalVersion);
    }
    if version > newest(logical) {
        return Err(RecordError::UnknownVersion);
    }
    Ok(ObjectBytes {
        kind: logical,
        version,
        body,
    })
}

/// The stored frame of an object of logical kind `kind`, encoding `version` and body `body`: its
/// raw kind and payload. A marked or never-versioned kind with a version is `Kind`, a version its
/// kind does not register `UnknownVersion`.
pub fn frame(kind: u8, version: u16, body: Vec<u8>) -> Result<(u8, Vec<u8>), RecordError> {
    if kind & VERSIONED != 0 {
        return Err(RecordError::Kind);
    }
    if version < first(kind) {
        return Err(RecordError::UnknownVersion);
    }
    if version == 0 {
        return Ok((kind, body));
    }
    if never_versioned(kind) {
        return Err(RecordError::Kind);
    }
    if version > newest(kind) {
        return Err(RecordError::UnknownVersion);
    }
    let mut payload = Vec::with_capacity(2 + body.len());
    payload.extend_from_slice(&version.to_le_bytes());
    payload.extend_from_slice(&body);
    Ok((raw_kind(kind, version), payload))
}

fn postcard_of<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("postcard encoding of an in-memory value")
}

/// The change behind `any`: bare, boxed or a `'static` reference.
fn as_change(any: &dyn Any) -> Option<&Change> {
    any.downcast_ref::<Change>()
        .or_else(|| any.downcast_ref::<Box<Change>>().map(|b| &**b))
        .or_else(|| any.downcast_ref::<&'static Change>().copied())
}

/// The landing behind `any`: bare, boxed or a `'static` reference.
fn as_landing(any: &dyn Any) -> Option<&LandingRecord> {
    any.downcast_ref::<LandingRecord>()
        .or_else(|| any.downcast_ref::<Box<LandingRecord>>().map(|b| &**b))
        .or_else(|| any.downcast_ref::<&'static LandingRecord>().copied())
}

/// The refusal of a value whose body has no form in encoding `version` (an encoding-1 option
/// set on a stored read of encoding 0): its bytes cannot match its identity.
fn mismatch() -> StoreError {
    StoreError::Refused(IDENTITY_MISMATCH.into())
}

/// The body of change `c` in encoding `version`: the frozen encoding-0 shape, the current
/// shape (encoding 1, a change that embeds no foreign block), or the foreign shape (encoding 2,
/// a change that embeds one). A change with no form in `version` is the mismatch refusal.
fn change_body(c: &Change, version: u16) -> Result<Vec<u8>, StoreError> {
    match version {
        0 => Ok(postcard_of(
            &LegacyChangeV0::from_current(c).ok_or_else(mismatch)?,
        )),
        CHANGE_FOREIGN_ENCODING if c.embeds_foreign() => Ok(postcard_of(&ForeignChangeV2::of(c))),
        CHANGE_FOREIGN_ENCODING => Err(mismatch()),
        _ if c.embeds_foreign() => Err(mismatch()),
        _ => Ok(postcard_of(c)),
    }
}

/// The encoding and body of a new `value` whose body is not its own postcard encoding in its
/// kind's latest encoding: a value with a shape that needs its kind's optional encoding
/// ([`OPTIONAL`]), or a registered wire value with no such shape, written in its runtime form.
/// None for every other value.
fn shaped_body<T: Serialize + 'static>(kind: u8, value: &T) -> Option<(u16, Vec<u8>)> {
    let any: &dyn Any = value;
    let local = latest(kind);
    if let Some(b) = any.downcast_ref::<Block>() {
        return (!b.foreign.is_empty())
            .then(|| (BLOCK_FOREIGN_ENCODING, postcard_of(&ForeignBlockV1::of(b))));
    }
    if let Some(c) = as_change(any) {
        return c.embeds_foreign().then(|| {
            (
                CHANGE_FOREIGN_ENCODING,
                postcard_of(&ForeignChangeV2::of(c)),
            )
        });
    }
    if let Some(a) = any.downcast_ref::<Answer>() {
        return a
            .act
            .is_foreign()
            .then(|| (ANSWER_FOREIGN_ENCODING, postcard_of(a)));
    }
    if let Some(r) = any.downcast_ref::<Redaction>() {
        return (r.encoding() == REDACTION_FOREIGN_ENCODING)
            .then(|| (REDACTION_FOREIGN_ENCODING, postcard_of(r)));
    }
    if let Some(t) = any.downcast_ref::<Transitions>() {
        return t.has_foreign().then(|| {
            (
                TRANSITION_FOREIGN_ENCODING,
                postcard_of(&TransitionsV1::of(t)),
            )
        });
    }
    if let Some(w) = any.downcast_ref::<TransitionsV1>() {
        return Some(match w.has_foreign() {
            true => (TRANSITION_FOREIGN_ENCODING, postcard_of(w)),
            false => (local, postcard_of(&w.clone().into_local()?)),
        });
    }
    if let Some(w) = any.downcast_ref::<ReviewV1>() {
        return Some(match w.has_foreign() {
            true => (PAIR_FOREIGN_ENCODING, postcard_of(w)),
            false => (local, postcard_of(&w.clone().into_local()?)),
        });
    }
    if let Some(w) = any.downcast_ref::<SemanticTensionV1>() {
        return Some(match w.has_foreign() {
            true => (PAIR_FOREIGN_ENCODING, postcard_of(w)),
            false => (local, postcard_of(&w.clone().into_local()?)),
        });
    }
    if let Some(w) = any.downcast_ref::<AssessmentV2>() {
        return Some(match w.has_foreign() {
            true => (ASSESSMENT_FOREIGN_ENCODING, postcard_of(w)),
            false => (local, postcard_of(&w.clone().into_local()?)),
        });
    }
    None
}

/// Whether `body`, of a frame of logical kind `kind` and encoding `version`, is the body the
/// writer writes for `value` ([`shaped_body`], else its kind's latest encoding): None for a type
/// that has no optional encoding.
fn shaped_canonical(kind: u8, version: u16, body: &[u8], any: &dyn Any) -> Option<bool> {
    let local = latest(kind);
    let local_form = |v: &dyn erased::Encode| version == local && v.encodes_as(body);
    let shaped =
        |form: Option<(u16, Vec<u8>)>| form.is_some_and(|(v, b)| v == version && b == body);
    if let Some(b) = any.downcast_ref::<Block>() {
        return Some(match b.foreign.is_empty() {
            true => local_form(b),
            false => shaped(shaped_body(kind, b)),
        });
    }
    if let Some(a) = any.downcast_ref::<Answer>() {
        return Some(match a.act.is_foreign() {
            true => version == ANSWER_FOREIGN_ENCODING && bounded::encodes_as(a, body),
            false => local_form(a),
        });
    }
    if let Some(r) = any.downcast_ref::<Redaction>() {
        return Some(match r.encoding() == REDACTION_FOREIGN_ENCODING {
            true => version == REDACTION_FOREIGN_ENCODING && bounded::encodes_as(r, body),
            false => local_form(r),
        });
    }
    if let Some(t) = any.downcast_ref::<Transitions>() {
        return Some(match t.has_foreign() {
            true => shaped(shaped_body(kind, t)),
            false => local_form(t),
        });
    }
    if let Some(r) = any.downcast_ref::<Review>() {
        return Some(local_form(r));
    }
    if let Some(t) = any.downcast_ref::<SemanticTension>() {
        return Some(local_form(t));
    }
    if let Some(a) = any.downcast_ref::<Assessment>() {
        return Some(local_form(a));
    }
    if any.is::<TransitionsV1>()
        || any.is::<ReviewV1>()
        || any.is::<SemanticTensionV1>()
        || any.is::<AssessmentV2>()
    {
        return Some(shaped(wire_body(kind, any)));
    }
    None
}

/// [`shaped_body`] of a registered wire value behind `any`.
fn wire_body(kind: u8, any: &dyn Any) -> Option<(u16, Vec<u8>)> {
    if let Some(w) = any.downcast_ref::<TransitionsV1>() {
        return shaped_body(kind, w);
    }
    if let Some(w) = any.downcast_ref::<ReviewV1>() {
        return shaped_body(kind, w);
    }
    if let Some(w) = any.downcast_ref::<SemanticTensionV1>() {
        return shaped_body(kind, w);
    }
    if let Some(w) = any.downcast_ref::<AssessmentV2>() {
        return shaped_body(kind, w);
    }
    None
}

/// A value whose postcard encoding can be compared with bytes without building it.
mod erased {
    pub(super) trait Encode {
        fn encodes_as(&self, bytes: &[u8]) -> bool;
    }

    impl<T: serde::Serialize> Encode for T {
        fn encodes_as(&self, bytes: &[u8]) -> bool {
            crate::bounded::encodes_as(self, bytes)
        }
    }
}

/// The body of landing `l` in encoding `version`.
fn landing_body(l: &LandingRecord, version: u16) -> Result<Vec<u8>, StoreError> {
    match version {
        0 => Ok(postcard_of(
            &LegacyLandingV0::from_current(l).ok_or_else(mismatch)?,
        )),
        _ => Ok(postcard_of(l)),
    }
}

/// The body of residue `r` in encoding `version`: encoding 0 is the frozen shape, which has no
/// source encoding and no encoding-1 link option. `None` when `r` has no form there.
pub fn residue_body(r: &RedactionResidue, version: u16) -> Option<Vec<u8>> {
    match version {
        0 => LegacyRedactionResidueV0::from_current(r).map(|v| postcard_of(&v)),
        _ => Some(postcard_of(r)),
    }
}

/// The stored object of a stored read of encoding `id.version` whose body is `body`, when its
/// bytes still hash to the read's identity.
fn stored(kind: u8, id: EncodedIdentity, body: Vec<u8>) -> Result<EncodedObject, StoreError> {
    if encoded_hash(id.version, &body) != id.hash {
        return Err(mismatch());
    }
    let (_, payload) =
        frame(kind, id.version, body).map_err(|e| StoreError::Decode(e.to_string()))?;
    Ok(EncodedObject {
        kind,
        version: id.version,
        payload,
    })
}

/// `value` as an object of logical kind `kind`: a new value in that kind's latest encoding. A
/// stored read of a CHANGE or a LANDING keeps the encoding it was read in, and is `Refused`
/// ([`IDENTITY_MISMATCH`]) when its bytes no longer hash to its identity, before any byte is
/// appended or deduplicated. A change that is a read projection is `ProjectedWrite`; the store's
/// own guards name the removal first where they can.
pub fn encode<T: Serialize + 'static>(kind: u8, value: &T) -> Result<EncodedObject, StoreError> {
    let any: &dyn Any = value;
    if let Some(c) = as_change(any) {
        match c.read_identity {
            Some(ChangeReadIdentity::Redacted(_)) => return Err(StoreError::ProjectedWrite),
            Some(ChangeReadIdentity::Stored(id)) => {
                return stored(kind, id, change_body(c, id.version)?);
            }
            None => {}
        }
    } else if let Some(l) = as_landing(any)
        && let Some(id) = l.read_identity
    {
        return stored(kind, id, landing_body(l, id.version)?);
    }
    Ok(encode_new(kind, value))
}

/// `value` as a new object of logical kind `kind`, its read identity (if any) ignored: in its
/// kind's optional encoding when it holds a shape only that encoding has a form for
/// ([`OPTIONAL`]), else in its kind's latest encoding. For a kind whose values have no read
/// identity: every kind but CHANGE and LANDING, whose writers use [`encode`].
pub fn encode_new<T: Serialize + 'static>(kind: u8, value: &T) -> EncodedObject {
    let (version, body) =
        shaped_body(kind, value).unwrap_or_else(|| (latest(kind), postcard_of(value)));
    let (_, payload) = frame(kind, version, body).expect("the chosen encoding of a kind frames");
    EncodedObject {
        kind,
        version,
        payload,
    }
}

/// [`encode_new`]'s `(raw kind, payload)` item.
pub fn item_new<T: Serialize + 'static>(kind: u8, value: &T) -> (u8, Vec<u8>) {
    encode_new(kind, value).into_item()
}

/// The `(raw kind, payload)` item of a new object of logical kind `kind` whose encoding-latest
/// body its writer built itself (a bounded encoder's record, or a blob chunk's bytes).
pub fn frame_latest(kind: u8, body: Vec<u8>) -> (u8, Vec<u8>) {
    frame(kind, latest(kind), body).expect("the latest encoding of a kind frames")
}

/// [`encode`]'s `(raw kind, payload)` item for a store write.
pub fn item<T: Serialize + 'static>(kind: u8, value: &T) -> Result<(u8, Vec<u8>), StoreError> {
    Ok(encode(kind, value)?.into_item())
}

/// The Rust types each logical kind decodes into (the exact assigned kinds at this store). A
/// pair not here is refused: no body is decoded as a type it was never written as.
fn dispatches(kind: u8, t: TypeId) -> bool {
    use crate::kinds::*;
    let want: &[TypeId] = match kind {
        BLOCK => &[TypeId::of::<rdy_model::Block>()],
        CHANGE => &[TypeId::of::<rdy_model::Change>()],
        PROPOSAL => &[TypeId::of::<rdy_model::Proposal>()],
        LANDING => &[TypeId::of::<rdy_model::LandingRecord>()],
        SNAPSHOT => &[TypeId::of::<rdy_model::StateSnapshot>()],
        BLOB_MANIFEST => &[TypeId::of::<BlobManifest>()],
        REF_UPDATE => &[TypeId::of::<RefUpdate>()],
        TAG => &[TypeId::of::<rdy_model::Tag>()],
        ANSWER => &[TypeId::of::<rdy_model::Answer>()],
        SIGNATURE => &[TypeId::of::<rdy_model::signature::Signature>()],
        VIEW_OP => &[TypeId::of::<crate::view::ViewOp>()],
        BATCH_COMMIT => &[TypeId::of::<BatchCommit>()],
        INTENT => &[TypeId::of::<rdy_model::Intent>()],
        ALT_SET => &[TypeId::of::<rdy_model::AltSet>()],
        REVIEW => &[TypeId::of::<rdy_model::Review>()],
        TENSION => &[TypeId::of::<rdy_model::SemanticTension>()],
        TRANSITION => &[TypeId::of::<crate::transition::Transitions>()],
        REDACTION => &[TypeId::of::<rdy_model::Redaction>()],
        REDACTION_RESIDUE => &[TypeId::of::<rdy_model::RedactionResidue>()],
        REDACTION_APPLY => &[TypeId::of::<rdy_model::RedactionApply>()],
        REDACTION_CARRIED => &[TypeId::of::<rdy_model::RedactionCarried>()],
        PLAN => &[TypeId::of::<rdy_model::Plan>()],
        ASSESSMENT => &[TypeId::of::<rdy_model::assessment::Assessment>()],
        DISPUTE => &[TypeId::of::<rdy_model::dispute::Dispute>()],
        DISPUTE_RESOLUTION => &[TypeId::of::<rdy_model::dispute::DisputeResolution>()],
        CHECK_REQUEST => &[TypeId::of::<rdy_model::check_request::CheckRequest>()],
        FOREIGN => &[TypeId::of::<rdy_model::foreign::ForeignEntry>()],
        _ => &[],
    };
    // the registered wire types read every encoding of their kind
    let wire: &[TypeId] = match kind {
        REVIEW => &[TypeId::of::<ReviewV1>()],
        TENSION => &[TypeId::of::<SemanticTensionV1>()],
        TRANSITION => &[TypeId::of::<TransitionsV1>()],
        ASSESSMENT => &[TypeId::of::<AssessmentV2>()],
        _ => &[],
    };
    want.contains(&t) || wire.contains(&t)
}

/// The name of logical kind `kind`, as this module names kinds (`kinds::TAG` is `TAG`), or its
/// number when this rdy assigns it no name.
fn kind_name(kind: u8) -> String {
    use crate::kinds::*;
    let name = match kind {
        BLOCK => "BLOCK",
        CHANGE => "CHANGE",
        PROPOSAL => "PROPOSAL",
        LANDING => "LANDING",
        SNAPSHOT => "SNAPSHOT",
        BLOB_CHUNK => "BLOB_CHUNK",
        BLOB_MANIFEST => "BLOB_MANIFEST",
        REF_UPDATE => "REF_UPDATE",
        TAG => "TAG",
        ANSWER => "ANSWER",
        SIGNATURE => "SIGNATURE",
        VIEW_OP => "VIEW_OP",
        BATCH_COMMIT => "BATCH_COMMIT",
        INTENT => "INTENT",
        ALT_SET => "ALT_SET",
        REVIEW => "REVIEW",
        TENSION => "TENSION",
        TRANSITION => "TRANSITION",
        REDACTION => "REDACTION",
        REDACTION_RESIDUE => "REDACTION_RESIDUE",
        REDACTION_APPLY => "REDACTION_APPLY",
        REDACTION_CARRIED => "REDACTION_CARRIED",
        PLAN => "PLAN",
        ASSESSMENT => "ASSESSMENT",
        DISPUTE => "DISPUTE",
        DISPUTE_RESOLUTION => "DISPUTE_RESOLUTION",
        CHECK_REQUEST => "CHECK_REQUEST",
        FOREIGN => "FOREIGN",
        _ => return kind.to_string(),
    };
    name.to_string()
}

/// The refusal (E_STORE_FORMAT) of a stored frame of raw kind `raw` and payload `payload` whose
/// encoding this rdy does not read: a compatible feature never authorizes guessing an encoding.
pub(crate) fn unknown_encoding(raw: u8, payload: &[u8]) -> StoreError {
    let version = if raw & VERSIONED == 0 {
        0
    } else {
        payload
            .first_chunk::<2>()
            .map_or(0, |v| u16::from_le_bytes(*v))
    };
    StoreError::Format {
        message: format!(
            "the store holds an object this rdy cannot read: kind {}, encoding {version}",
            kind_name(logical_kind(raw))
        ),
        fix: "open the project with a newer rdy".into(),
    }
}

/// The store error of a stored frame of raw kind `raw` and payload `payload` that [`split`]
/// refused with `e`: an encoding this rdy does not read is [`unknown_encoding`], any other
/// refusal a frame that does not decode.
fn stored_error(raw: u8, payload: &[u8], e: RecordError) -> StoreError {
    match e {
        RecordError::UnknownVersion => unknown_encoding(raw, payload),
        e => StoreError::Decode(e.to_string()),
    }
}

/// [`split`] of a stored frame, with the store's refusals ([`stored_error`]): what every read
/// and replay of the store's own frames uses.
pub(crate) fn split_stored(raw: u8, payload: &[u8]) -> Result<ObjectBytes<'_>, StoreError> {
    split(raw, payload).map_err(|e| stored_error(raw, payload, e))
}

/// The stored frame of raw kind `kind` with payload `payload`, decoded as `T`: the frame is
/// split ([`split`]), its logical kind must decode into `T`, and its body (of any encoding the
/// kind registers) decodes into the current type.
pub fn decode_record<T: DeserializeOwned + 'static>(
    kind: u8,
    payload: &[u8],
) -> Result<T, StoreError> {
    let o = split_stored(kind, payload)?;
    decode_split(&o)
}

/// `value`, known by its kind's dispatch to be a `T`, as a `T`.
fn cast<T: 'static, U: 'static>(value: U) -> Option<T> {
    let any: Box<dyn Any> = Box::new(value);
    any.downcast::<T>().ok().map(|b| *b)
}

/// How a body is decoded: by postcard alone (a store read), or within the record bounds
/// (`crate::bounded`, a read of outside bytes).
#[derive(Clone, Copy)]
enum Reader {
    Plain,
    Bounded,
}

/// Why [`body_of`] refused: a plain read's postcard error, or a bounded read's refusal.
enum BodyError {
    Postcard(postcard::Error),
    Record(RecordError),
}

impl From<RecordError> for BodyError {
    fn from(e: RecordError) -> Self {
        BodyError::Record(e)
    }
}

impl BodyError {
    fn into_store(self) -> StoreError {
        match self {
            BodyError::Postcard(e) => StoreError::Decode(e.to_string()),
            BodyError::Record(e) => StoreError::Decode(e.to_string()),
        }
    }

    fn into_record(self) -> RecordError {
        match self {
            BodyError::Postcard(_) => RecordError::Decode,
            BodyError::Record(e) => e,
        }
    }
}

impl Reader {
    fn read<T: DeserializeOwned>(self, body: &[u8]) -> Result<T, BodyError> {
        match self {
            Reader::Plain => postcard::from_bytes(body).map_err(BodyError::Postcard),
            Reader::Bounded => Ok(bounded::decode(body)?),
        }
    }
}

/// The body of split frame `o`, of the type its logical kind dispatches to, decoded in its
/// encoding: an encoding-0 CHANGE, LANDING or REDACTION_RESIDUE through its frozen shape. A
/// change or landing keeps its frame's identity. `T` is checked by the caller ([`dispatches`]).
fn body_of<T: DeserializeOwned + 'static>(
    o: &ObjectBytes<'_>,
    reader: Reader,
) -> Result<T, BodyError> {
    let identity = || EncodedIdentity {
        hash: encoded_hash(o.version, o.body),
        version: o.version,
    };
    let wrong = BodyError::Record(RecordError::Kind);
    match (o.kind, o.version) {
        (kinds::CHANGE, version) => {
            let c = if version == 0 {
                reader
                    .read::<LegacyChangeV0>(o.body)?
                    .into_current(identity())
            } else if version == CHANGE_FOREIGN_ENCODING {
                // encoding 2 embeds a foreign block; a change that embeds none is encoding 1
                let c = reader
                    .read::<ForeignChangeV2>(o.body)?
                    .into_current(identity());
                if !c.embeds_foreign() {
                    return Err(BodyError::Record(RecordError::Decode));
                }
                c
            } else {
                let mut c: Change = reader.read(o.body)?;
                c.read_identity = Some(ChangeReadIdentity::Stored(identity()));
                c
            };
            cast(c).ok_or(wrong)
        }
        (kinds::LANDING, version) => {
            let l = if version == 0 {
                reader
                    .read::<LegacyLandingV0>(o.body)?
                    .into_current(identity())
            } else {
                let mut l: LandingRecord = reader.read(o.body)?;
                l.read_identity = Some(identity());
                l
            };
            cast(l).ok_or(wrong)
        }
        (kinds::REDACTION_RESIDUE, 0) => cast(
            reader
                .read::<LegacyRedactionResidueV0>(o.body)?
                .into_current(),
        )
        .ok_or(wrong),
        (kinds::BLOCK, _) => shaped::<Block, ForeignBlockV1, T>(
            o,
            reader,
            Shape {
                optional: BLOCK_FOREIGN_ENCODING,
                wire_foreign: |w| !w.foreign.is_empty(),
                local_foreign: |_| false,
                of: ForeignBlockV1::of,
                into_local: |w| Some(w.into_current()),
            },
        ),
        (kinds::ANSWER, _) => shaped::<Answer, Answer, T>(
            o,
            reader,
            Shape {
                optional: ANSWER_FOREIGN_ENCODING,
                wire_foreign: |a| a.act.is_foreign(),
                local_foreign: |a| a.act.is_foreign(),
                of: Answer::clone,
                into_local: Some,
            },
        ),
        (kinds::REDACTION, _) => shaped::<Redaction, Redaction, T>(
            o,
            reader,
            Shape {
                optional: REDACTION_FOREIGN_ENCODING,
                wire_foreign: |r| r.encoding() == REDACTION_FOREIGN_ENCODING,
                local_foreign: |r| r.encoding() == REDACTION_FOREIGN_ENCODING,
                of: Redaction::clone,
                into_local: Some,
            },
        ),
        (kinds::TRANSITION, _) => shaped::<Transitions, TransitionsV1, T>(
            o,
            reader,
            Shape {
                optional: TRANSITION_FOREIGN_ENCODING,
                wire_foreign: TransitionsV1::has_foreign,
                local_foreign: Transitions::has_foreign,
                of: TransitionsV1::of,
                into_local: TransitionsV1::into_local,
            },
        ),
        (kinds::REVIEW, _) => shaped::<Review, ReviewV1, T>(
            o,
            reader,
            Shape {
                optional: PAIR_FOREIGN_ENCODING,
                wire_foreign: ReviewV1::has_foreign,
                local_foreign: |_| false,
                of: ReviewV1::of,
                into_local: ReviewV1::into_local,
            },
        ),
        (kinds::TENSION, _) => shaped::<SemanticTension, SemanticTensionV1, T>(
            o,
            reader,
            Shape {
                optional: PAIR_FOREIGN_ENCODING,
                wire_foreign: SemanticTensionV1::has_foreign,
                local_foreign: |_| false,
                of: SemanticTensionV1::of,
                into_local: SemanticTensionV1::into_local,
            },
        ),
        (kinds::ASSESSMENT, _) => shaped::<Assessment, AssessmentV2, T>(
            o,
            reader,
            Shape {
                optional: ASSESSMENT_FOREIGN_ENCODING,
                wire_foreign: AssessmentV2::has_foreign,
                local_foreign: |_| false,
                of: AssessmentV2::of,
                into_local: AssessmentV2::into_local,
            },
        ),
        _ => reader.read(o.body),
    }
}

/// How a kind with an optional encoding reads: `L` is its runtime type, `W` the layout of its
/// optional encoding.
struct Shape<L, W> {
    /// The optional encoding.
    optional: u16,
    /// Whether a value of the optional encoding holds a shape only that encoding has a form for.
    wire_foreign: fn(&W) -> bool,
    /// Whether a value of the latest encoding holds such a shape (a runtime type that has one).
    local_foreign: fn(&L) -> bool,
    /// The optional layout of a runtime value.
    of: fn(&L) -> W,
    /// The runtime value of an optional-encoding value, when the runtime type can hold it.
    into_local: fn(W) -> Option<L>,
}

/// The body of split frame `o` of a kind with an optional encoding, as `T` (its runtime type
/// `L` or its wire type `W`). The optional encoding reads `W` and holds a foreign shape; any
/// other encoding of the kind reads `L` (a test-registered one as encoding 0 does) and holds
/// none. A frame that breaks the writer's choice, or whose shape `T` cannot hold, is `Decode`.
fn shaped<L, W, T>(o: &ObjectBytes<'_>, reader: Reader, s: Shape<L, W>) -> Result<T, BodyError>
where
    L: DeserializeOwned + 'static,
    W: DeserializeOwned + 'static,
    T: 'static,
{
    let refused = || BodyError::Record(RecordError::Decode);
    let wrong = || BodyError::Record(RecordError::Kind);
    let wants_wire =
        TypeId::of::<T>() == TypeId::of::<W>() && TypeId::of::<W>() != TypeId::of::<L>();
    if o.version == s.optional && !is_test_version(o.kind, o.version) {
        let w: W = reader.read(o.body)?;
        if !(s.wire_foreign)(&w) {
            return Err(refused());
        }
        if wants_wire {
            return cast(w).ok_or_else(wrong);
        }
        return cast((s.into_local)(w).ok_or_else(refused)?).ok_or_else(wrong);
    }
    let l: L = reader.read(o.body)?;
    if (s.local_foreign)(&l) && !is_test_version(o.kind, o.version) {
        return Err(refused());
    }
    if wants_wire {
        return cast((s.of)(&l)).ok_or_else(wrong);
    }
    cast(l).ok_or_else(wrong)
}

/// [`decode_record`] of a split frame.
pub fn decode_split<T: DeserializeOwned + 'static>(o: &ObjectBytes<'_>) -> Result<T, StoreError> {
    if !dispatches(o.kind, TypeId::of::<T>()) {
        return Err(StoreError::Decode(format!(
            "an object of kind {} does not decode as {}",
            o.kind,
            std::any::type_name::<T>()
        )));
    }
    body_of(o, Reader::Plain).map_err(BodyError::into_store)
}

/// Whether `body`, of split frame `o`, is the canonical encoding of `value` in `o`'s encoding:
/// the bytes a writer of that encoding writes for it. For a kind with an optional encoding
/// ([`OPTIONAL`]), `o`'s encoding must also be the one the writer chooses for `value`. No
/// allocation beyond the comparison for a value in its kind's latest encoding.
pub fn is_canonical<T: Serialize + 'static>(o: &ObjectBytes<'_>, value: &T) -> bool {
    let any: &dyn Any = value;
    let body = if let Some(c) = as_change(any) {
        change_body(c, o.version).ok()
    } else if let Some(l) = as_landing(any) {
        landing_body(l, o.version).ok()
    } else if let Some(r) = any.downcast_ref::<RedactionResidue>() {
        residue_body(r, o.version)
    } else if let Some(ok) = shaped_canonical(o.kind, o.version, o.body, any) {
        return ok;
    } else {
        return bounded::encodes_as(value, o.body);
    };
    body.is_some_and(|b| b == o.body)
}

/// [`decode_record`] with its identity: the value and the hash and version of the stored frame.
pub fn decode_identified<T: DeserializeOwned + 'static>(
    kind: u8,
    payload: &[u8],
) -> Result<(T, EncodedIdentity), StoreError> {
    let o = split_stored(kind, payload)?;
    let value = decode_split(&o)?;
    Ok((
        value,
        EncodedIdentity {
            hash: Hash::of(payload),
            version: o.version,
        },
    ))
}

/// The bounded decode (`crate::bounded`) of a frame from outside bytes: split, then the body as
/// exactly one bounded `T`. The refusal is the record layer's.
pub fn decode_bounded<T: DeserializeOwned + 'static>(
    kind: u8,
    payload: &[u8],
) -> Result<T, RecordError> {
    let o = split(kind, payload)?;
    decode_bounded_split(&o)
}

/// [`decode_bounded`] of a split frame: the body in its own encoding, within the bounds.
pub fn decode_bounded_split<T: DeserializeOwned + 'static>(
    o: &ObjectBytes<'_>,
) -> Result<T, RecordError> {
    if !dispatches(o.kind, TypeId::of::<T>()) {
        return Err(RecordError::Kind);
    }
    body_of(o, Reader::Bounded).map_err(BodyError::into_record)
}

/// While held, this thread's logical kind `kind` has encodings 1 to `version`, each decoding as
/// encoding 0 does, and its latest encoding is `version`.
#[doc(hidden)]
#[must_use]
pub struct TestEncoding(u8);

impl Drop for TestEncoding {
    fn drop(&mut self) {
        let kind = self.0;
        TEST_ENCODINGS.with(|t| t.borrow_mut().retain(|(k, _)| *k != kind));
    }
}

/// Test hook: registers encodings 1 to `version` of logical kind `kind` on this thread, until the
/// guard drops. Production registers only [`REGISTERED`].
#[doc(hidden)]
pub fn test_encoding(kind: u8, version: u16) -> TestEncoding {
    assert!(version > 0 && kind & VERSIONED == 0 && !never_versioned(kind));
    TEST_ENCODINGS.with(|t| {
        let mut t = t.borrow_mut();
        t.retain(|(k, _)| *k != kind);
        t.push((kind, version));
    });
    TestEncoding(kind)
}
