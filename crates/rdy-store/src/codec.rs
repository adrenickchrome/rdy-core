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
//! At this store every kind's latest encoding is 0.
use crate::bounded::{self, RecordError};
use crate::error::StoreError;
use crate::kinds;
use rdy_model::Hash;
use rdy_model::encoding::EncodedIdentity;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::any::TypeId;
use std::cell::RefCell;

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

thread_local! {
    /// Test-registered encodings of this thread: (logical kind, latest version).
    static TEST_ENCODINGS: RefCell<Vec<(u8, u16)>> = const { RefCell::new(Vec::new()) };
}

/// The latest encoding of logical kind `kind`: what every new object of it is written in. At
/// this store it is 0 for every kind.
pub fn latest(kind: u8) -> u16 {
    TEST_ENCODINGS.with(|t| {
        t.borrow()
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(0)
    })
}

/// Splits a stored frame: an unmarked raw kind is encoding 0 and its whole payload; a marked one
/// needs its two version bytes (`Decode`), a nonzero version (`NoncanonicalVersion`) that its
/// kind registers (`UnknownVersion`), and a kind that may be versioned (`Kind`).
pub fn split(kind: u8, payload: &[u8]) -> Result<ObjectBytes<'_>, RecordError> {
    if kind & VERSIONED == 0 {
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

/// `value` as a new object of logical kind `kind`, in that kind's latest encoding.
pub fn encode<T: Serialize + 'static>(kind: u8, value: &T) -> EncodedObject {
    let body = postcard::to_allocvec(value).expect("postcard encoding of an in-memory value");
    let version = latest(kind);
    let (_, payload) = frame(kind, version, body).expect("the latest encoding of a kind frames");
    EncodedObject {
        kind,
        version,
        payload,
    }
}

/// The `(raw kind, payload)` item of a new object of logical kind `kind` whose encoding-latest
/// body its writer built itself (a bounded encoder's record, or a blob chunk's bytes).
pub fn frame_latest(kind: u8, body: Vec<u8>) -> (u8, Vec<u8>) {
    frame(kind, latest(kind), body).expect("the latest encoding of a kind frames")
}

/// [`encode`]'s `(raw kind, payload)` item for a store write.
pub fn item<T: Serialize + 'static>(kind: u8, value: &T) -> (u8, Vec<u8>) {
    encode(kind, value).into_item()
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

/// [`decode_record`] of a split frame.
pub fn decode_split<T: DeserializeOwned + 'static>(o: &ObjectBytes<'_>) -> Result<T, StoreError> {
    if !dispatches(o.kind, TypeId::of::<T>()) {
        return Err(StoreError::Decode(format!(
            "an object of kind {} does not decode as {}",
            o.kind,
            std::any::type_name::<T>()
        )));
    }
    postcard::from_bytes(o.body).map_err(|e| StoreError::Decode(e.to_string()))
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
    if !dispatches(o.kind, TypeId::of::<T>()) {
        return Err(RecordError::Kind);
    }
    bounded::decode(o.body)
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
/// guard drops. Production registers no encoding but 0.
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
