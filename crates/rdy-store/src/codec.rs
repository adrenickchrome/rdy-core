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
//! A typed read of a CHANGE or a LANDING keeps the identity of its frame
//! (`Change::read_identity`, `LandingRecord::read_identity`). [`encode`] writes such a stored read
//! in its own encoding and refuses it when its bytes do not hash to the identity: a caller that
//! edits a read value clears the identity first. A read projection is never written.
use crate::bounded::{self, RecordError};
use crate::error::StoreError;
use crate::kinds;
use rdy_model::encoding::{
    EncodedIdentity, LegacyChangeV0, LegacyLandingV0, LegacyRedactionResidueV0, encoded_hash,
};
use rdy_model::redaction::RedactionResidue;
use rdy_model::{Change, ChangeReadIdentity, Hash, LandingRecord};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::any::{Any, TypeId};
use std::cell::RefCell;

/// The refusal of a stored read whose bytes no longer hash to its identity: an edited value that
/// kept the identity of the frame it was read from.
pub const IDENTITY_MISMATCH: &str = "stored record identity does not match its bytes";

/// The bit a versioned frame's raw kind sets.
pub const VERSIONED: u8 = 0x80;

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
];

/// The first encoding of logical kind `kind`: 1 for a kind registered after encodings existed
/// (it has no encoding 0), else 0.
pub fn first(kind: u8) -> u16 {
    match kind {
        kinds::ASSESSMENT | kinds::DISPUTE | kinds::DISPUTE_RESOLUTION | kinds::CHECK_REQUEST => 1,
        _ => 0,
    }
}

thread_local! {
    /// Test-registered encodings of this thread: (logical kind, latest version).
    static TEST_ENCODINGS: RefCell<Vec<(u8, u16)>> = const { RefCell::new(Vec::new()) };
}

/// The latest encoding of logical kind `kind`: what every new object of it is written in. It is
/// the kind's [`REGISTERED`] version, else 0 (or a higher test-registered one on this thread).
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

/// Splits a stored frame: an unmarked raw kind is encoding 0 and its whole payload; a marked one
/// needs its two version bytes (`Decode`), a nonzero version (`NoncanonicalVersion`) that its
/// kind registers (`UnknownVersion`), and a kind that may be versioned (`Kind`).
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
    if version > latest(logical) {
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
    if version > latest(kind) {
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

/// The body of change `c` in encoding `version`: the frozen encoding-0 shape, or the current.
fn change_body(c: &Change, version: u16) -> Result<Vec<u8>, StoreError> {
    match version {
        0 => Ok(postcard_of(
            &LegacyChangeV0::from_current(c).ok_or_else(mismatch)?,
        )),
        _ => Ok(postcard_of(c)),
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

/// `value` as a new object of logical kind `kind`, in that kind's latest encoding, its read
/// identity (if any) ignored. For a kind whose values have none: every kind but CHANGE and
/// LANDING, whose writers use [`encode`].
pub fn encode_new<T: Serialize + ?Sized>(kind: u8, value: &T) -> EncodedObject {
    let body = postcard_of(value);
    let version = latest(kind);
    let (_, payload) = frame(kind, version, body).expect("the latest encoding of a kind frames");
    EncodedObject {
        kind,
        version,
        payload,
    }
}

/// [`encode_new`]'s `(raw kind, payload)` item.
pub fn item_new<T: Serialize + ?Sized>(kind: u8, value: &T) -> (u8, Vec<u8>) {
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
        _ => &[],
    };
    want.contains(&t)
}

fn decode_error(e: RecordError) -> StoreError {
    StoreError::Decode(e.to_string())
}

/// The stored frame of raw kind `kind` with payload `payload`, decoded as `T`: the frame is
/// split ([`split`]), its logical kind must decode into `T`, and its body (of any encoding the
/// kind registers) decodes into the current type.
pub fn decode_record<T: DeserializeOwned + 'static>(
    kind: u8,
    payload: &[u8],
) -> Result<T, StoreError> {
    let o = split(kind, payload).map_err(decode_error)?;
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
        _ => reader.read(o.body),
    }
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
/// the bytes a writer of that encoding writes for it. No allocation beyond the comparison for
/// a kind with one shape.
pub fn is_canonical<T: Serialize + 'static>(o: &ObjectBytes<'_>, value: &T) -> bool {
    let any: &dyn Any = value;
    let body = if let Some(c) = as_change(any) {
        change_body(c, o.version).ok()
    } else if let Some(l) = as_landing(any) {
        landing_body(l, o.version).ok()
    } else if let Some(r) = any.downcast_ref::<RedactionResidue>() {
        residue_body(r, o.version)
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
    let o = split(kind, payload).map_err(decode_error)?;
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
