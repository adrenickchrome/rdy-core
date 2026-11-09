//! The sync batch (D-D2NPN: "records as store frames, header, trailer, dependency order,
//! verify-on-receive"; D-DAGNB: the repo id): one batch of a tag history, as one file. An upload
//! is one or more batches. Each batch stands alone: its own header, its own trailer, its own
//! prerequisites. Batches may arrive in any order and may repeat; the take resolves them as one
//! union (hub i-QD955, p-97JVE).
//!
//! A batch is not a dump: it holds no project file, no local ref, no batch marker and no physical
//! place in a log. It holds only portable objects, so two stores can exchange a history whatever
//! their own layouts are.
//!
//! # Stream format 2
//!
//! Integers are little endian. Stream format 1 (a scalar store format in its header) is not
//! read: it has no pre-release compatibility promise.
//!
//! - **Prelude:** the 8 bytes `RDYSYNC\n`, then `stream_format: u32` (2).
//! - **Frames:** each is framed as a store frame: `len: u32`, `kind: u8`, `hash: [u8; 32]`
//!   (`Hash::of(payload)`), `payload`.
//! - **Order (closed):** one `HEADER` (0xF0); zero or more object frames, each a portable
//!   object's raw frame, exactly as the store holds it ([`SYNC_KINDS`] by logical kind; a
//!   versioned frame keeps its raw kind and version prefix, `crate::codec`) or a `DROPPED`
//!   record (0xF3); one `TRAILER` (0xFF); the end of the file. A batch of no object frames is
//!   valid: its root is a prerequisite that the take must find.
//! - **`HEADER` payload:** `features_len: u32` and the store's feature manifest as canonical
//!   JSON (`crate::format`, at most 65,536 bytes), `repo_id: [u8; 16]`, `purpose: u8` (0:
//!   `tag_history`), `tag: [u8; 32]` (the root TAG's hash), `prerequisites: u32`, then that many
//!   logical keys `kind: u8 || hash: [u8; 32]`, strictly ascending. At most
//!   [`MAX_PREREQUISITES`] keys, so at most [`MAX_HEADER_BYTES`] bytes. No sequence number,
//!   upload id or end-of-upload mark. A manifest that is not valid, or that names an unknown
//!   incompatible feature, is E_STORE_FORMAT.
//! - **`DROPPED` payload:** the 104-byte body of a dump-4 `DROPPED` record
//!   (`crate::dump::ENCODED_DROPPED_LEN`): `logical_kind || encoding: u16 || original_hash ||
//!   original_len: u32 || apply_hash || has_residue: u8 || residue_hash`. Its logical key is the
//!   logical kind and the original hash. It carries no place in a source log.
//! - **`TRAILER` payload:** exactly 48 bytes: `records: u64` (the object frames), `bytes: u64`
//!   (every byte of the file before the trailer frame, prelude included), `digest: [u8; 32]`:
//!   `Hash::of` over `kind: u8 || len: u64 || hash: [u8; 32]` of the `HEADER` and every object
//!   frame, in wire order, as a dump's trailer has it. The prelude and the trailer are not in it.
//!
//! # A valid batch
//!
//! Beyond the framing: every object frame's typed record decodes within its bounds
//! (`crate::bounded`); no logical key repeats; every object a record needs (`refs_of`, and a
//! dropped object's residue) is an earlier record of the batch or a prerequisite; the
//! prerequisites are exactly the keys the records need that the batch does not hold, plus the
//! root TAG when the batch does not hold it. A kind-None link resolves to exactly one key of the
//! batch or of its prerequisites. A dropped object's apply is a reference but no forward
//! dependency: in the batch it may come before or after the dropped object (a removal proof
//! follows what it removes); absent from it, it is a prerequisite like any other reference.
//!
//! A batch check says only that: its framing and objects are valid. It never says that an upload
//! is complete or that a removal is authorized; the take checks those over every file.
//!
//! # Stream format 3
//!
//! Format 3 has format 2's prelude, frames, header and trailer, and adds a second purpose,
//! `link_delivery` (byte 1, D-DAPR8): one project's filtered view at a tag, which the cited side
//! writes and the citing side takes (repo links). The reader reads formats 2 and 3
//! ([`READ_FORMATS`]); format 2 knows only `tag_history`. A tag history is still written in
//! format 2 ([`STREAM_FORMAT`]), so a reader of the released format takes it; a link delivery is
//! written in format 3 ([`LINK_DELIVERY_FORMAT`]).
//!
//! - **Order (closed):** one `HEADER`; one `DELIVERY` (0xF4), a stream-only record; zero or more
//!   object records, each a `BLOCK`'s raw frame or a `DROPPED` record of a `BLOCK` (a redacted
//!   version travels as its residue); one `TRAILER`; the end of the file.
//! - **`HEADER`:** as format 2's, with purpose 1 and no prerequisite. `tag` is the hash of the
//!   cited tag's `TAG` object, for provenance only: the stream never carries that `TAG`, never
//!   lists it as a prerequisite, and the root-`TAG` rule of a tag history does not apply.
//! - **`DELIVERY` payload:** a [`DeliveryRecord`] in postcard: the publication `{tag, profile,
//!   profile_version}`, the selection digest and each selected block's `ForeignVersion`, in
//!   strict id order. The digest is the publication selection identity of the versions
//!   (`rdy_model::foreign::selection_identity`). Its bounds are a typed record's
//!   (`crate::bounded`): at most [`MAX_RECORD_BYTES`] bytes and [`MAX_DELIVERY_VERSIONS`]
//!   versions.
//! - **Objects:** exactly one per selected version, in the `DELIVERY`'s order: its key is the
//!   version's `BLOCK` hash, its encoding the version's, and a present block's id the
//!   version's id. No source parent, reference or blob is a dependency of the delivery, and no
//!   transitive foreign content enters it.
//! - **`TRAILER`:** as format 2's. Its record count and digest cover the `DELIVERY` and every
//!   object record. The `DELIVERY` is record 1.
//!
//! [`check_delivery`] checks one delivery whole, before any intake; [`check_batch`] refuses a
//! delivery, and [`check_delivery`] a tag history, with E_STREAM_PURPOSE.
use crate::bounded::{CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_RECORD_BYTES, RECORD_BYTES, RecordError};
use crate::dump::{
    ENCODED_DROPPED_LEN, Frame, LINK_LEN, check_dropped, encoded_dropped_payload, link,
    parse_encoded_dropped,
};
use crate::format::{MAX_MANIFEST_BYTES, StoreFeatures, check_features};
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef, refs_of};
use crate::segment::HEADER as FRAME_HEADER;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_HEADER_BYTES, LIMIT_HISTORY_DEPTH, LIMIT_OBJECT_RECORDS,
    LIMIT_PENDING_DROPPED_BYTES, LIMIT_PENDING_DROPS, LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES,
    Reason, StreamError,
};
use rdy_model::foreign::{ForeignPublication, ForeignVersion, selection_identity};
use rdy_model::{Block, Hash, RepoId};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read, Write};

/// The 8 bytes every sync batch starts with.
pub const MAGIC: &[u8; 8] = b"RDYSYNC\n";
/// The stream format a tag history is written in: the format Release 1 reads, so a reader of
/// the released format takes every tag history this rdy writes. This rdy reads every format of
/// [`READ_FORMATS`].
pub const STREAM_FORMAT: u32 = 2;
/// The stream format a link delivery is written in: the first that knows the purpose
/// `link_delivery` and its `DELIVERY` record.
pub const LINK_DELIVERY_FORMAT: u32 = 3;
/// The stream formats this rdy reads: every format released from Release 1 on.
pub const READ_FORMATS: [u32; 2] = [STREAM_FORMAT, LINK_DELIVERY_FORMAT];

/// The frame kinds of the stream's own records. Object frames keep their store kinds.
pub mod rec {
    pub const HEADER: u8 = 0xF0;
    /// A portable object whose payload an approved removal dropped.
    pub const DROPPED: u8 = 0xF3;
    /// A link delivery's stream-only record: the publication, the selection digest and each
    /// selected version ([`super::DeliveryRecord`]).
    pub const DELIVERY: u8 = 0xF4;
    pub const TRAILER: u8 = 0xFF;
}

/// The most bytes of one batch (and of one upload), prelude and trailer included.
pub const MAX_STREAM_BYTES: u64 = 2_147_483_648;
/// The most object records of one batch (and of one upload).
pub const MAX_RECORDS: u64 = 1_000_000;
/// The most dependency edges of one batch (and of one upload).
pub const MAX_EDGES: u64 = 1_000_000;
/// The longest dependency chain of one batch (and of one upload), in records.
pub const MAX_HISTORY_DEPTH: u64 = 100_000;
/// The most prerequisites of one header.
pub const MAX_PREREQUISITES: usize = 100_000;
/// The fixed bytes of a header payload, besides its manifest and its prerequisite keys.
pub const HEADER_FIXED: usize = 4 + 16 + 1 + 32 + 4;
/// The bytes of one prerequisite key in a header.
pub const KEY_LEN: usize = 1 + 32;
/// The most bytes of one header payload: [`HEADER_FIXED`], the largest manifest and
/// [`MAX_PREREQUISITES`] keys.
pub const MAX_HEADER_BYTES: usize = HEADER_FIXED + MAX_MANIFEST_BYTES + MAX_PREREQUISITES * KEY_LEN;
/// The exact length of a trailer payload.
pub const TRAILER_LEN: usize = 8 + 8 + 32;
/// The most batch files of one upload.
pub const MAX_BATCH_FILES: usize = 1_024;
/// The most bytes of the trailer's descriptor chain: the header and [`MAX_RECORDS`] records.
pub const MAX_DIGEST_BYTES: usize = (MAX_RECORDS as usize + 1) * LINK_LEN;
/// The most `DROPPED` records whose authorization is still to be checked.
pub const MAX_PENDING_DROPS: u64 = 1_000_000;
/// The most original bytes the `DROPPED` records whose authorization is still to be checked
/// claim.
pub const MAX_PENDING_DROPPED_BYTES: u64 = 1_073_741_824;
/// The limit label of an upload's batch files.
pub const LIMIT_BATCH_FILES: &str = "1024 batch files";
/// The limit label of the trailer's descriptor chain.
pub const LIMIT_DIGEST_BYTES: &str = "41000041 digest bytes";
/// The most versions one `DELIVERY` selects: a typed record's list bound.
pub const MAX_DELIVERY_VERSIONS: usize = crate::bounded::MAX_ELEMENTS;

/// The bytes of the prelude: the magic and the stream format.
const PRELUDE: u64 = MAGIC.len() as u64 + 4;

/// The store kinds an object frame may have: the portable kinds less TRANSITION, a batch's own
/// local record (no FILE, REF_UPDATE, BATCH_COMMIT or TRANSITION is portable). Ascending. A
/// kind-None link resolves among these.
pub const SYNC_KINDS: [u8; 23] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
    kinds::PLAN,
    kinds::ASSESSMENT,
    kinds::DISPUTE,
    kinds::DISPUTE_RESOLUTION,
    kinds::CHECK_REQUEST,
];

/// Whether an object frame of `kind` may stand in a sync batch.
pub fn is_sync_kind(kind: u8) -> bool {
    SYNC_KINDS.contains(&kind)
}

/// What a stream carries: a tag's history, written `tag_history` and as the byte 0, or a link
/// delivery, written `link_delivery` and as the byte 1 (stream format 3 on).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPurpose {
    TagHistory,
    LinkDelivery,
}

impl StreamPurpose {
    /// The purpose's header byte.
    pub fn byte(self) -> u8 {
        match self {
            StreamPurpose::TagHistory => 0,
            StreamPurpose::LinkDelivery => 1,
        }
    }

    /// The purpose of header byte `b`, if it is one.
    pub fn of(b: u8) -> Option<StreamPurpose> {
        match b {
            0 => Some(StreamPurpose::TagHistory),
            1 => Some(StreamPurpose::LinkDelivery),
            _ => None,
        }
    }

    /// The purpose's name, as JSON and the purpose refusal write it.
    pub fn name(self) -> &'static str {
        match self {
            StreamPurpose::TagHistory => "tag_history",
            StreamPurpose::LinkDelivery => "link_delivery",
        }
    }

    /// The first stream format that knows the purpose.
    pub fn first_format(self) -> u32 {
        match self {
            StreamPurpose::TagHistory => STREAM_FORMAT,
            StreamPurpose::LinkDelivery => LINK_DELIVERY_FORMAT,
        }
    }
}

/// The payload of a link delivery's `DELIVERY` record (D-DAPR8): the facts the citing side
/// cannot compute. `publication` names the cited tag, the profile and its version; `versions`
/// holds each selected block's version, content hash, encoding and derived facts, in strict id
/// order; `selection` is their publication selection identity. JSON writes hashes as full
/// lowercase hex. The field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRecord {
    pub publication: ForeignPublication,
    #[serde(with = "rdy_model::foreign::hex_one")]
    pub selection: Hash,
    pub versions: Vec<ForeignVersion>,
}

impl DeliveryRecord {
    /// The logical key of each selected version's `BLOCK`, in the delivery's order.
    pub fn keys(&self) -> impl Iterator<Item = ObjectKey> + '_ {
        self.versions.iter().map(|v| ObjectKey {
            kind: kinds::BLOCK,
            hash: v.version,
        })
    }
}

/// The rules of a `DELIVERY` record's own fields, shared by the writer and the reader: versions in
/// strict id order, no version hash twice (a `BLOCK` hash names one block), each in an encoding
/// a `BLOCK` has, and the selection digest their selection identity. The versions are within
/// their bound already, so the sort buffer is too.
fn delivery_rules(d: &DeliveryRecord) -> Result<(), Reason> {
    if d.versions.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(Reason::InvalidSelection);
    }
    let mut hashes: Vec<&Hash> = d.versions.iter().map(|v| &v.version).collect();
    hashes.sort_unstable();
    if hashes.windows(2).any(|w| w[0] == w[1]) {
        return Err(Reason::InvalidSelection);
    }
    let encodings = crate::codec::first(kinds::BLOCK)..=crate::codec::newest(kinds::BLOCK);
    if d.versions.iter().any(|v| !encodings.contains(&v.encoding)) {
        return Err(Reason::UnknownEncodingVersion);
    }
    match selection_identity(d.versions.iter().map(|v| (&v.id, &v.version))) {
        Some(s) if s == d.selection => Ok(()),
        _ => Err(Reason::InvalidSelection),
    }
}

/// Decodes a `DELIVERY` payload within a typed record's bounds (`crate::bounded`): exactly one
/// record, in its canonical encoding.
fn decode_delivery(p: &[u8]) -> Result<DeliveryRecord, RecordError> {
    let d: DeliveryRecord = crate::bounded::decode(p)?;
    if !crate::bounded::encodes_as(&d, p) {
        return Err(RecordError::Decode);
    }
    Ok(d)
}

/// A batch's header: the stream format, the source store's features, the source repo, the
/// purpose, the upload's root TAG and this batch's prerequisites (strictly ascending logical
/// keys). JSON writes the repo id and the hash as full lowercase hex and each key as
/// `{"kind", "hash"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamHeader {
    pub stream_format: u32,
    pub features: StoreFeatures,
    #[serde(serialize_with = "hex_repo")]
    pub repo_id: RepoId,
    pub purpose: StreamPurpose,
    #[serde(serialize_with = "hex_hash")]
    pub tag: Hash,
    pub prerequisites: Vec<ObjectKey>,
}

impl StreamHeader {
    /// The header of a tag history of this rdy's stream format, from a store of `features`, for
    /// `repo_id`, rooted at TAG `tag`.
    pub fn tag_history(
        features: StoreFeatures,
        repo_id: RepoId,
        tag: Hash,
        prerequisites: Vec<ObjectKey>,
    ) -> Self {
        StreamHeader {
            stream_format: STREAM_FORMAT,
            features,
            repo_id,
            purpose: StreamPurpose::TagHistory,
            tag,
            prerequisites,
        }
    }

    /// The header of a link delivery (stream format 3), from a store of `features`, for the
    /// cited repo `repo_id`, naming the cited TAG `tag` for provenance only. It has no
    /// prerequisite.
    pub fn link_delivery(features: StoreFeatures, repo_id: RepoId, tag: Hash) -> Self {
        StreamHeader {
            stream_format: LINK_DELIVERY_FORMAT,
            features,
            repo_id,
            purpose: StreamPurpose::LinkDelivery,
            tag,
            prerequisites: Vec::new(),
        }
    }

    /// The upload's root: the TAG the header names. Only a tag history has one; a link
    /// delivery's tag is provenance.
    pub fn root(&self) -> ObjectKey {
        ObjectKey {
            kind: kinds::TAG,
            hash: self.tag,
        }
    }
}

/// One object record of a batch: a portable object's original stored bytes (with its encoding:
/// the payload of a versioned record starts with its version), or the identity of one whose
/// payload an approved removal dropped (its encoding, its original length, the apply that
/// removed it and its residue, none for a blob chunk). The key is logical. JSON writes hashes as
/// full lowercase hex, a payload as lowercase hex and an absent residue as null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamRecord {
    Present {
        key: ObjectKey,
        encoding: u16,
        #[serde(serialize_with = "hex_bytes")]
        payload: Vec<u8>,
    },
    Dropped {
        key: ObjectKey,
        encoding: u16,
        original_len: u32,
        #[serde(serialize_with = "hex_hash")]
        apply: Hash,
        #[serde(serialize_with = "hex_opt")]
        residue: Option<Hash>,
    },
}

impl StreamRecord {
    /// The record's logical key.
    pub fn key(&self) -> ObjectKey {
        match self {
            StreamRecord::Present { key, .. } | StreamRecord::Dropped { key, .. } => *key,
        }
    }

    /// The bytes of the record's frame payload: the object's, or the 104 of a `DROPPED` record.
    pub fn frame_len(&self) -> u64 {
        match self {
            StreamRecord::Present { payload, .. } => payload.len() as u64,
            StreamRecord::Dropped { .. } => ENCODED_DROPPED_LEN as u64,
        }
    }

    /// A present record's body: its payload, less a versioned record's version
    /// (`crate::codec`); None for a dropped record. The refusal is the codec's.
    pub fn body(&self) -> Option<Result<&[u8], RecordError>> {
        match self {
            StreamRecord::Present { payload, .. } => {
                Some(crate::codec::split(self.raw_kind(), payload).map(|o| o.body))
            }
            StreamRecord::Dropped { .. } => None,
        }
    }

    /// A present record decoded as `T` within its bounds, through its encoding
    /// (`crate::codec`); None for a dropped record.
    pub fn typed<T: serde::de::DeserializeOwned + 'static>(
        &self,
    ) -> Option<Result<T, RecordError>> {
        match self {
            StreamRecord::Present { payload, .. } => {
                Some(crate::codec::decode_bounded(self.raw_kind(), payload))
            }
            StreamRecord::Dropped { .. } => None,
        }
    }

    /// The raw kind of the record's frame: the logical kind, with the high bit when the record
    /// is versioned (`crate::codec`).
    pub fn raw_kind(&self) -> u8 {
        match self {
            StreamRecord::Present { key, encoding, .. }
            | StreamRecord::Dropped { key, encoding, .. } => {
                crate::codec::raw_kind(key.kind, *encoding)
            }
        }
    }

    /// The objects the record needs before it, as `refs_of` gives them: a present record's
    /// references, a dropped record's residue (its apply is a later proof, see
    /// [`StreamRecord::proof`]). Sorted, no repeats.
    pub fn refs(&self) -> Result<Vec<ObjectRef>, RecordError> {
        match self {
            StreamRecord::Present { payload, .. } => refs_of(self.raw_kind(), payload),
            StreamRecord::Dropped { residue, .. } => Ok(residue
                .iter()
                .map(|r| ObjectRef {
                    kind: Some(kinds::REDACTION_RESIDUE),
                    hash: *r,
                })
                .collect()),
        }
    }
}

impl StreamRecord {
    /// The removal proof a dropped record names: its apply. It is a reference, but no forward
    /// dependency: an apply of the same batch may come before or after the record (a removal
    /// proof follows what it removes), and an absent one is a prerequisite.
    pub fn proof(&self) -> Option<ObjectKey> {
        match self {
            StreamRecord::Present { .. } => None,
            StreamRecord::Dropped { apply, .. } => Some(ObjectKey {
                kind: kinds::REDACTION_APPLY,
                hash: *apply,
            }),
        }
    }
}

/// What a batch's trailer states: its object records, its bytes before the trailer frame
/// (prelude included) and its digest. The whole file is `bytes` plus the trailer's 85 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StreamTotals {
    pub records: u64,
    pub bytes: u64,
    #[serde(serialize_with = "hex_hash")]
    pub digest: Hash,
}

/// What a valid batch holds: its header, the logical key of every object record in wire order
/// (not sorted), and its trailer's records, bytes and digest. It says the batch's framing and
/// objects are valid, never that an upload is complete or a removal authorized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BatchReport {
    pub header: StreamHeader,
    pub keys: Vec<ObjectKey>,
    pub records: u64,
    pub bytes: u64,
    #[serde(serialize_with = "hex_hash")]
    pub digest: Hash,
}

fn hex_hash<S: Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

fn hex_opt<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match h {
        Some(h) => s.collect_str(h),
        None => s.serialize_none(),
    }
}

fn hex_repo<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(r)
}

fn hex_bytes<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
    struct Hex<'a>(&'a [u8]);
    impl std::fmt::Display for Hex<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
        }
    }
    s.collect_str(&Hex(b))
}

/// The header's payload. The header's fields are checked first ([`check_header`]).
fn header_payload(h: &StreamHeader) -> Vec<u8> {
    let json = h.features.to_json();
    let mut p = Vec::with_capacity(HEADER_FIXED + json.len() + h.prerequisites.len() * KEY_LEN);
    p.extend_from_slice(&(json.len() as u32).to_le_bytes());
    p.extend_from_slice(&json);
    p.extend_from_slice(&h.repo_id.0);
    p.push(h.purpose.byte());
    p.extend_from_slice(&h.tag.0);
    p.extend_from_slice(&(h.prerequisites.len() as u32).to_le_bytes());
    for k in &h.prerequisites {
        p.push(k.kind);
        p.extend_from_slice(&k.hash.0);
    }
    p
}

/// Whether an object record of logical kind `kind` may stand in a stream of `purpose`: a sync
/// kind in a tag history, a `BLOCK` alone in a link delivery.
fn purpose_kind(purpose: StreamPurpose, kind: u8) -> bool {
    match purpose {
        StreamPurpose::TagHistory => is_sync_kind(kind),
        StreamPurpose::LinkDelivery => kind == kinds::BLOCK,
    }
}

/// The bad header error: record 0, at the header frame.
fn bad_header() -> StreamError {
    StreamError::bad_record(0, PRELUDE, Reason::InvalidHeader)
}

/// The rules of a header's fields, shared by the writer and the reader: a stream format this rdy
/// reads, a purpose that format knows (E_STREAM_PURPOSE otherwise: format 2 knows only
/// `tag_history`), a manifest this rdy reads (E_STORE_FORMAT otherwise), at most
/// [`MAX_PREREQUISITES`] prerequisites, each of a sync kind, strictly ascending, and none for a
/// link delivery.
fn check_header(h: &StreamHeader) -> Result<(), StreamError> {
    if !READ_FORMATS.contains(&h.stream_format) {
        return Err(bad_header());
    }
    if h.stream_format < h.purpose.first_format() {
        return Err(StreamError::purpose(Some(0), PRELUDE, h.purpose.byte()));
    }
    check_features(&h.features).map_err(|e| StreamError::store_format(Some(0), PRELUDE, &e))?;
    if h.prerequisites.len() > MAX_PREREQUISITES {
        return Err(StreamError::limit(Some(0), PRELUDE, LIMIT_PREREQUISITES));
    }
    if h.prerequisites.iter().any(|k| !is_sync_kind(k.kind))
        || h.prerequisites.windows(2).any(|w| w[0] >= w[1])
        || (h.purpose == StreamPurpose::LinkDelivery && !h.prerequisites.is_empty())
    {
        return Err(bad_header());
    }
    Ok(())
}

/// The E_STREAM_PURPOSE refusal of a header of purpose byte `actual` where `expected` is read.
fn wrong_purpose(actual: u8, expected: StreamPurpose) -> StreamError {
    StreamError::purpose_for(Some(0), PRELUDE, actual, expected.name())
}

/// Parses a header payload of stream format `format` (its length checked against
/// [`MAX_HEADER_BYTES`] already). An unknown purpose byte is refused as not `expected`.
fn parse_header(
    p: &[u8],
    format: u32,
    expected: StreamPurpose,
) -> Result<StreamHeader, StreamError> {
    if p.len() < HEADER_FIXED {
        return Err(bad_header());
    }
    let manifest_len = u32::from_le_bytes(p[0..4].try_into().expect("4 bytes")) as usize;
    if manifest_len > MAX_MANIFEST_BYTES {
        let e = crate::format::ManifestError("manifest exceeds 65536 bytes".into());
        return Err(StreamError::store_format(
            Some(0),
            PRELUDE,
            &e.store_error(),
        ));
    }
    if p.len() < HEADER_FIXED + manifest_len {
        return Err(bad_header());
    }
    let features = match StoreFeatures::parse_fallible(&p[4..4 + manifest_len]) {
        Ok(f) => f,
        Err(crate::format::ParseFailure::Manifest(e)) => {
            return Err(StreamError::store_format(
                Some(0),
                PRELUDE,
                &e.store_error(),
            ));
        }
        Err(crate::format::ParseFailure::Allocation) => {
            return Err(StreamError::allocation(Some(0), PRELUDE));
        }
    };
    // the rest of the header, as it was before the manifest
    let p = &p[manifest_len..];
    let repo_id = RepoId(p[4..20].try_into().expect("16 bytes"));
    let purpose = StreamPurpose::of(p[20]).ok_or_else(|| wrong_purpose(p[20], expected))?;
    let tag = Hash(p[21..53].try_into().expect("32 bytes"));
    let count = u32::from_le_bytes(p[53..57].try_into().expect("4 bytes")) as usize;
    if count > MAX_PREREQUISITES {
        return Err(StreamError::limit(Some(0), PRELUDE, LIMIT_PREREQUISITES));
    }
    if p.len() != HEADER_FIXED + count * KEY_LEN {
        return Err(bad_header());
    }
    let mut prerequisites = Vec::new();
    prerequisites
        .try_reserve_exact(count)
        .map_err(|_| StreamError::allocation(Some(0), PRELUDE))?;
    for k in p[HEADER_FIXED..].as_chunks::<KEY_LEN>().0 {
        prerequisites.push(ObjectKey {
            kind: k[0],
            hash: Hash(k[1..].try_into().expect("32 bytes")),
        });
    }
    let h = StreamHeader {
        stream_format: format,
        features,
        repo_id,
        purpose,
        tag,
        prerequisites,
    };
    check_header(&h)?;
    Ok(h)
}

/// The trailer chain: the descriptors of the header and every object frame so far, bounded by
/// [`MAX_DIGEST_BYTES`] and grown fallibly.
#[derive(Default)]
struct Chain(Vec<u8>);

impl Chain {
    fn add(
        &mut self,
        record: u64,
        at: u64,
        kind: u8,
        len: u64,
        hash: &Hash,
    ) -> Result<(), StreamError> {
        if self.0.len() + LINK_LEN > MAX_DIGEST_BYTES {
            return Err(StreamError::limit(Some(record), at, LIMIT_DIGEST_BYTES));
        }
        link(&mut self.0, kind, len, hash).ok_or(StreamError::allocation(Some(record), at))
    }

    fn digest(&self) -> Hash {
        Hash::of(&self.0)
    }
}

/// The bounds a record's payload must keep before it is read: a blob chunk's
/// [`MAX_CHUNK_BYTES`], a typed record's [`MAX_RECORD_BYTES`].
fn payload_bound(record: u64, at: u64, kind: u8, len: u64) -> Result<(), StreamError> {
    let refused =
        |name, maximum| StreamError::from_record(record, at, &RecordError::limit(name, maximum));
    if kind == kinds::BLOB_CHUNK {
        if len > MAX_CHUNK_BYTES as u64 {
            return Err(refused(CHUNK_BYTES, MAX_CHUNK_BYTES));
        }
    } else if len > MAX_RECORD_BYTES as u64 {
        return Err(refused(RECORD_BYTES, MAX_RECORD_BYTES));
    }
    Ok(())
}

/// The dropped-record budget of one batch: [`MAX_PENDING_DROPS`] records claiming at most
/// [`MAX_PENDING_DROPPED_BYTES`] original bytes.
#[derive(Default)]
struct Pending {
    drops: u64,
    bytes: u64,
}

impl Pending {
    fn add(&mut self, record: u64, at: u64, len: u32) -> Result<(), StreamError> {
        let drops = self.drops + 1;
        if drops > MAX_PENDING_DROPS {
            return Err(StreamError::limit(Some(record), at, LIMIT_PENDING_DROPS));
        }
        let bytes = self.bytes + u64::from(len);
        if bytes > MAX_PENDING_DROPPED_BYTES {
            return Err(StreamError::limit(
                Some(record),
                at,
                LIMIT_PENDING_DROPPED_BYTES,
            ));
        }
        (self.drops, self.bytes) = (drops, bytes);
        Ok(())
    }
}

/// Writes one batch: the prelude and header on `new` (with a link delivery's `DELIVERY` on
/// `new_delivery`), each object record on `record`, the trailer on `finish`. Every check runs
/// before a record's first byte, so a refused record writes nothing. The writer checks the
/// framing rules: kinds, hashes, dropped records, repeated keys and every count and byte bound,
/// and a `DELIVERY`'s own rules. The caller supplies records in dependency order with the exact
/// prerequisites ([`crate::closure`]), or a delivery's selected versions in its order;
/// [`check_batch`] and [`check_delivery`] verify them.
pub struct StreamWriter<W: Write> {
    w: W,
    purpose: StreamPurpose,
    offset: u64,
    records: u64,
    keys: BTreeSet<ObjectKey>,
    pending: Pending,
    chain: Chain,
}

impl<W: Write> StreamWriter<W> {
    /// Writes the prelude and `header`, a tag history's, in its stream format. Refuses another
    /// purpose (E_STREAM_PURPOSE: a link delivery starts with [`StreamWriter::new_delivery`]), a
    /// header of a stream or store format this rdy does not read, more than
    /// [`MAX_PREREQUISITES`] prerequisites, or prerequisites of a kind that is not a sync kind
    /// or not strictly ascending.
    pub fn new(w: W, header: &StreamHeader) -> Result<Self, StreamError> {
        if header.purpose != StreamPurpose::TagHistory {
            return Err(wrong_purpose(
                header.purpose.byte(),
                StreamPurpose::TagHistory,
            ));
        }
        check_header(header)?;
        Self::start(w, header)
    }

    /// Writes the prelude, `header` and the `DELIVERY` record `delivery` (record 1) of a link
    /// delivery. Refuses another purpose (E_STREAM_PURPOSE), a header out of its rules (any
    /// prerequisite included), and a `DELIVERY` over its bounds (E_LIMIT: more than
    /// [`MAX_DELIVERY_VERSIONS`] versions, more than [`MAX_RECORD_BYTES`] bytes, or any other
    /// typed-record bound) or out of its own rules: versions not in strict id order or a
    /// selection digest that is not theirs (`invalid delivered selection`), or a version in an
    /// encoding a `BLOCK` does not have. The object records follow on `record`, in the
    /// delivery's order.
    pub fn new_delivery(
        w: W,
        header: &StreamHeader,
        delivery: &DeliveryRecord,
    ) -> Result<Self, StreamError> {
        if header.purpose != StreamPurpose::LinkDelivery {
            return Err(wrong_purpose(
                header.purpose.byte(),
                StreamPurpose::LinkDelivery,
            ));
        }
        check_header(header)?;
        let at = PRELUDE + FRAME_HEADER + header_payload(header).len() as u64;
        let refused = |e: &RecordError| StreamError::from_record(1, at, e);
        if delivery.versions.len() > MAX_DELIVERY_VERSIONS {
            return Err(refused(&RecordError::limit(
                crate::bounded::COLLECTION_ELEMENTS,
                MAX_DELIVERY_VERSIONS,
            )));
        }
        crate::bounded::fits(delivery).map_err(|e| refused(&e))?;
        let p = postcard::to_allocvec(delivery).map_err(|_| refused(&RecordError::Decode))?;
        // the reader's own decode, so no bound the reader keeps is left to it
        decode_delivery(&p).map_err(|e| refused(&e))?;
        delivery_rules(delivery).map_err(|reason| StreamError::bad_record(1, at, reason))?;
        let mut me = Self::start(w, header)?;
        me.within(1, at, p.len() as u64)?;
        let hash = Hash::of(&p);
        me.chain.add(1, at, rec::DELIVERY, p.len() as u64, &hash)?;
        me.frame(rec::DELIVERY, &hash, &p)?;
        me.records = 1;
        Ok(me)
    }

    /// Writes the prelude, in the header's stream format, and `header`, whose rules the caller
    /// checked.
    fn start(w: W, header: &StreamHeader) -> Result<Self, StreamError> {
        let mut me = StreamWriter {
            w,
            purpose: header.purpose,
            offset: 0,
            records: 0,
            keys: BTreeSet::new(),
            pending: Pending::default(),
            chain: Chain::default(),
        };
        me.raw(MAGIC)?;
        me.raw(&header.stream_format.to_le_bytes())?;
        let p = header_payload(header);
        let hash = Hash::of(&p);
        me.chain
            .add(0, PRELUDE, rec::HEADER, p.len() as u64, &hash)?;
        me.frame(rec::HEADER, &hash, &p)?;
        Ok(me)
    }

    /// Writes one object record: a present record as its own frame, a dropped one as its
    /// `DROPPED` record. Refuses a kind that is not a sync kind (in a link delivery, any kind
    /// but `BLOCK`), a payload over its bound or not matching its key's hash, a dropped record
    /// that breaks its record rules, a key already written, and any count or byte bound.
    pub fn record(&mut self, r: &StreamRecord) -> Result<(), StreamError> {
        let n = self.records + 1;
        let at = self.offset;
        let key = r.key();
        let bad = |reason| StreamError::bad_record(n, at, reason).with_object(Some(key));
        if n > MAX_RECORDS {
            return Err(StreamError::limit(Some(n), at, LIMIT_OBJECT_RECORDS));
        }
        if !purpose_kind(self.purpose, key.kind) {
            return Err(bad(Reason::UnknownKind));
        }
        if self.keys.contains(&key) {
            return Err(bad(Reason::ObjectIsRepeated));
        }
        self.within(n, at, r.frame_len())?;
        let raw = r.raw_kind();
        let (kind, hash, payload): (u8, Hash, std::borrow::Cow<[u8]>) = match r {
            StreamRecord::Present { payload, .. } => {
                payload_bound(n, at, key.kind, payload.len() as u64)?;
                // the raw frame as the store holds it: its encoding must split
                crate::codec::split(raw, payload)
                    .map_err(|e| StreamError::from_record(n, at, &e).with_object(Some(key)))?;
                if Hash::of(payload) != key.hash {
                    return Err(bad(Reason::HashMismatch));
                }
                (raw, key.hash, payload.as_slice().into())
            }
            StreamRecord::Dropped {
                encoding,
                original_len,
                apply,
                residue,
                ..
            } => {
                // a removed frame's encoding: its residue names the same one (the proof checks)
                if !crate::dump::dropped_encoding_allowed(key.kind, *encoding) {
                    return Err(bad(Reason::UnknownEncodingVersion));
                }
                check_dropped(key.kind, residue.as_ref(), *original_len)
                    .map_err(|_| bad(Reason::InvalidDroppedRecord))?;
                self.pending.add(n, at, *original_len)?;
                let body = encoded_dropped_payload(
                    key.kind,
                    *encoding,
                    &key.hash,
                    *original_len,
                    apply,
                    residue.as_ref(),
                );
                (rec::DROPPED, Hash::of(&body), body.to_vec().into())
            }
        };
        self.chain.add(n, at, kind, payload.len() as u64, &hash)?;
        self.keys.insert(key);
        self.frame(kind, &hash, &payload)?;
        self.records = n;
        Ok(())
    }

    /// Writes the trailer and returns the writer and what the trailer states.
    pub fn finish(mut self) -> Result<(W, StreamTotals), StreamError> {
        let at = self.offset;
        self.within(self.records + 1, at, TRAILER_LEN as u64)?;
        let totals = StreamTotals {
            records: self.records,
            bytes: at,
            digest: self.chain.digest(),
        };
        let mut p = Vec::with_capacity(TRAILER_LEN);
        p.extend_from_slice(&totals.records.to_le_bytes());
        p.extend_from_slice(&totals.bytes.to_le_bytes());
        p.extend_from_slice(&totals.digest.0);
        self.frame(rec::TRAILER, &Hash::of(&p), &p)?;
        let at = self.offset;
        self.w
            .flush()
            .map_err(|e| StreamError::io(Some(self.records + 1), at, &e))?;
        Ok((self.w, totals))
    }

    /// Refuses a frame of `len` payload bytes at `at` that would take the batch over
    /// [`MAX_STREAM_BYTES`].
    fn within(&self, record: u64, at: u64, len: u64) -> Result<(), StreamError> {
        if at + FRAME_HEADER + len > MAX_STREAM_BYTES {
            return Err(StreamError::limit(Some(record), at, LIMIT_TOTAL_BYTES));
        }
        Ok(())
    }

    fn frame(&mut self, kind: u8, hash: &Hash, payload: &[u8]) -> Result<(), StreamError> {
        let mut head = [0u8; FRAME_HEADER as usize];
        head[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        head[4] = kind;
        head[5..37].copy_from_slice(&hash.0);
        self.raw(&head)?;
        self.raw(payload)
    }

    fn raw(&mut self, b: &[u8]) -> Result<(), StreamError> {
        let at = self.offset;
        self.w
            .write_all(b)
            .map_err(|e| StreamError::io(Some(self.records), at, &e))?;
        self.offset += b.len() as u64;
        Ok(())
    }
}

/// Reads one batch and checks its framing on the way (verify on receive): the prelude, the
/// header, every frame's hash, kind and bounds, each `DROPPED` record's body, the trailer's
/// length, counts and digest, and the end of the file after it. Typed records are not decoded
/// here; [`check_batch`] does that. A failure ends the read: every later call returns the same
/// error, and no read resumes after it.
pub struct StreamReader<R: Read> {
    r: R,
    purpose: StreamPurpose,
    delivery: Option<DeliveryRecord>,
    offset: u64,
    records: u64,
    last: (u64, u64),
    pending: Pending,
    chain: Chain,
    failed: Option<StreamError>,
    totals: Option<StreamTotals>,
}

impl<R: Read> StreamReader<R> {
    /// Reads the prelude and the header, and a link delivery's `DELIVERY` record. Refuses
    /// another magic or a stream format not in [`READ_FORMATS`] and a header that is not the
    /// first frame (`invalid header`), stream format 1 (E_STORE_FORMAT), a header over
    /// [`MAX_HEADER_BYTES`], a store format other than this rdy's (E_STORE_FORMAT), a purpose
    /// byte its stream format does not know (E_STREAM_PURPOSE, as not `tag_history`), and
    /// prerequisites out of their rules. A link delivery's `DELIVERY` must follow the header
    /// (`delivery record out of order`), within its bounds and its own rules
    /// ([`StreamWriter::new_delivery`]); [`StreamReader::delivery`] returns it.
    pub fn open(r: R) -> Result<(Self, StreamHeader), StreamError> {
        Self::open_inner(r, None)
    }

    /// [`StreamReader::open`] for a stream that must be of `purpose`: another purpose is
    /// E_STREAM_PURPOSE naming `purpose`, refused at the header before any later record is read.
    pub fn open_for(r: R, purpose: StreamPurpose) -> Result<(Self, StreamHeader), StreamError> {
        Self::open_inner(r, Some(purpose))
    }

    /// A link delivery's `DELIVERY` record, read and checked with the header; None for a tag
    /// history.
    pub fn delivery(&self) -> Option<&DeliveryRecord> {
        self.delivery.as_ref()
    }

    fn open_inner(
        r: R,
        expected: Option<StreamPurpose>,
    ) -> Result<(Self, StreamHeader), StreamError> {
        let mut me = StreamReader {
            r,
            purpose: StreamPurpose::TagHistory,
            delivery: None,
            offset: 0,
            records: 0,
            last: (0, PRELUDE),
            pending: Pending::default(),
            chain: Chain::default(),
            failed: None,
            totals: None,
        };
        let mut magic = [0u8; MAGIC.len()];
        let got = me.fill(&mut magic, 0, 0)?;
        if magic[..got] != MAGIC[..got] {
            return Err(StreamError::bad_record(0, 0, Reason::InvalidHeader));
        }
        if got < magic.len() {
            return Err(StreamError::truncated(Some(0), 0));
        }
        let mut format = [0u8; 4];
        me.exact(&mut format, 0, MAGIC.len() as u64)?;
        let format = u32::from_le_bytes(format);
        if format == 1 {
            return Err(StreamError::sync_format_one(MAGIC.len() as u64));
        }
        if !READ_FORMATS.contains(&format) {
            return Err(StreamError::bad_record(
                0,
                MAGIC.len() as u64,
                Reason::InvalidHeader,
            ));
        }
        me.offset = PRELUDE;
        let (len, kind, hash) = me.head(0)?;
        if kind != rec::HEADER {
            return Err(bad_header());
        }
        if len > MAX_HEADER_BYTES as u64 {
            return Err(StreamError::limit(Some(0), PRELUDE, LIMIT_HEADER_BYTES));
        }
        let p = me.payload(0, PRELUDE, len, &hash)?;
        let header = parse_header(&p, format, expected.unwrap_or(StreamPurpose::TagHistory))?;
        if let Some(expected) = expected
            && header.purpose != expected
        {
            return Err(wrong_purpose(header.purpose.byte(), expected));
        }
        me.chain.add(0, PRELUDE, rec::HEADER, len, &hash)?;
        me.offset = PRELUDE + FRAME_HEADER + len;
        me.purpose = header.purpose;
        if header.purpose == StreamPurpose::LinkDelivery {
            me.read_delivery()?;
        }
        Ok((me, header))
    }

    /// Reads a link delivery's `DELIVERY`, record 1, right after the header: refused by its frame
    /// header alone when it is another frame or over [`MAX_RECORD_BYTES`], then its hash, its
    /// bounded canonical decode and its own rules.
    fn read_delivery(&mut self) -> Result<(), StreamError> {
        let at = self.offset;
        let (len, kind, hash) = self.head(1)?;
        if kind != rec::DELIVERY {
            return Err(StreamError::bad_record(1, at, Reason::DeliveryOutOfOrder));
        }
        payload_bound(1, at, kind, len)?;
        if at + FRAME_HEADER + len > MAX_STREAM_BYTES {
            return Err(StreamError::limit(Some(1), at, LIMIT_TOTAL_BYTES));
        }
        let p = self.payload(1, at, len, &hash)?;
        let delivery = decode_delivery(&p).map_err(|e| StreamError::from_record(1, at, &e))?;
        delivery_rules(&delivery).map_err(|reason| StreamError::bad_record(1, at, reason))?;
        self.chain.add(1, at, rec::DELIVERY, len, &hash)?;
        self.records = 1;
        self.last = (1, at);
        self.offset = at + FRAME_HEADER + len;
        self.delivery = Some(delivery);
        Ok(())
    }

    /// The next object record; `None` after a verified trailer at the end of the file.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<StreamRecord>, StreamError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        if self.totals.is_some() {
            return Ok(None);
        }
        let out = self.step();
        if let Err(e) = &out {
            self.failed = Some(e.clone());
        }
        out
    }

    /// Reads to the end, which must be the trailer and the end of the file, and returns what the
    /// trailer states. Records not yet read are read and checked, and dropped.
    pub fn finish(mut self) -> Result<StreamTotals, StreamError> {
        while self.next()?.is_some() {}
        Ok(self.totals.expect("a verified trailer"))
    }

    /// The record number and frame offset of the last record `next` returned (the header's,
    /// 0 and 12, before the first).
    pub fn position(&self) -> (u64, u64) {
        self.last
    }

    fn step(&mut self) -> Result<Option<StreamRecord>, StreamError> {
        let n = self.records + 1;
        let at = self.offset;
        let (len, kind, hash) = self.head(n)?;
        if kind == rec::TRAILER {
            if len != TRAILER_LEN as u64 {
                return Err(StreamError::truncated(Some(n), at));
            }
            if at + FRAME_HEADER + len > MAX_STREAM_BYTES {
                return Err(StreamError::limit(Some(n), at, LIMIT_TOTAL_BYTES));
            }
            let p = self.payload(n, at, len, &hash)?;
            let records = u64::from_le_bytes(p[0..8].try_into().expect("8 bytes"));
            let bytes = u64::from_le_bytes(p[8..16].try_into().expect("8 bytes"));
            let digest = Hash(p[16..48].try_into().expect("32 bytes"));
            let totals = StreamTotals {
                records: self.records,
                bytes: at,
                digest: self.chain.digest(),
            };
            if (records, bytes, digest) != (totals.records, totals.bytes, totals.digest) {
                return Err(StreamError::truncated(Some(n), at));
            }
            let end = at + FRAME_HEADER + len;
            let mut extra = [0u8; 1];
            if self.fill(&mut extra, n + 1, end)? > 0 {
                return Err(StreamError::bad_record(n + 1, end, Reason::TrailingBytes));
            }
            self.offset = end;
            self.totals = Some(totals);
            return Ok(None);
        }
        let bad = |reason| StreamError::bad_record(n, at, reason);
        if kind == rec::DELIVERY && self.purpose == StreamPurpose::LinkDelivery {
            return Err(bad(Reason::DeliveryOutOfOrder));
        }
        if kind != rec::DROPPED && !purpose_kind(self.purpose, crate::codec::logical_kind(kind)) {
            return Err(bad(Reason::UnknownKind));
        }
        if n > MAX_RECORDS {
            return Err(StreamError::limit(Some(n), at, LIMIT_OBJECT_RECORDS));
        }
        if at + FRAME_HEADER + len > MAX_STREAM_BYTES {
            return Err(StreamError::limit(Some(n), at, LIMIT_TOTAL_BYTES));
        }
        let record = if kind == rec::DROPPED {
            // refused by its header alone, before its payload is read
            if len != ENCODED_DROPPED_LEN as u64 {
                return Err(bad(Reason::InvalidDroppedRecord));
            }
            let p = self.payload(n, at, len, &hash)?;
            // a versioned removal has no residue adapter yet
            if !crate::dump::dropped_encoding_allowed(p[0], u16::from_le_bytes([p[1], p[2]])) {
                return Err(bad(Reason::UnknownEncodingVersion));
            }
            let Ok(Frame::Dropped {
                kind,
                hash: original,
                len: original_len,
                apply,
                residue,
            }) = parse_encoded_dropped(&p)
            else {
                return Err(bad(Reason::InvalidDroppedRecord));
            };
            // a removable kind (`parse_dropped`), so a sync kind; a link delivery drops a BLOCK
            let key = ObjectKey {
                kind: crate::codec::logical_kind(kind),
                hash: original,
            };
            if !purpose_kind(self.purpose, key.kind) {
                return Err(bad(Reason::UnknownKind));
            }
            self.pending
                .add(n, at, original_len)
                .map_err(|e| e.with_object(Some(key)))?;
            StreamRecord::Dropped {
                key,
                encoding: u16::from_le_bytes([p[1], p[2]]),
                original_len,
                apply,
                residue,
            }
        } else {
            // the logical key; the frame keeps its raw kind and its encoding
            let key = ObjectKey {
                kind: crate::codec::logical_kind(kind),
                hash,
            };
            payload_bound(n, at, key.kind, len).map_err(|e| e.with_object(Some(key)))?;
            let payload = self
                .payload(n, at, len, &hash)
                .map_err(|e| e.with_object(Some(key)))?;
            let encoding = crate::codec::split(kind, &payload)
                .map_err(|e| StreamError::from_record(n, at, &e).with_object(Some(key)))?
                .version;
            StreamRecord::Present {
                key,
                encoding,
                payload,
            }
        };
        self.chain.add(n, at, kind, len, &hash)?;
        self.records = n;
        self.last = (n, at);
        self.offset = at + FRAME_HEADER + len;
        Ok(Some(record))
    }

    /// Reads the frame header of record `record` at the current offset: its payload length, kind
    /// and hash. The end of the file there is a stream that ends before its trailer.
    fn head(&mut self, record: u64) -> Result<(u64, u8, Hash), StreamError> {
        let mut head = [0u8; FRAME_HEADER as usize];
        self.exact(&mut head, record, self.offset)?;
        let len = u64::from(u32::from_le_bytes(head[0..4].try_into().expect("4 bytes")));
        Ok((
            len,
            head[4],
            Hash(head[5..37].try_into().expect("32 bytes")),
        ))
    }

    /// Reads the `len` payload bytes of the frame at `at` (its length bound checked already)
    /// into one fallibly reserved buffer, and checks them against `hash`.
    fn payload(
        &mut self,
        record: u64,
        at: u64,
        len: u64,
        hash: &Hash,
    ) -> Result<Vec<u8>, StreamError> {
        let mut p = Vec::new();
        p.try_reserve_exact(len as usize)
            .map_err(|_| StreamError::allocation(Some(record), at))?;
        p.resize(len as usize, 0);
        self.exact(&mut p, record, at)?;
        if Hash::of(&p) != *hash {
            return Err(StreamError::bad_record(record, at, Reason::HashMismatch));
        }
        Ok(p)
    }

    /// Fills `buf`, or fails: the end of the file is E_STREAM_TRUNCATED at record `record` and
    /// byte `at`.
    fn exact(&mut self, buf: &mut [u8], record: u64, at: u64) -> Result<(), StreamError> {
        if self.fill(buf, record, at)? < buf.len() {
            return Err(StreamError::truncated(Some(record), at));
        }
        Ok(())
    }

    /// Reads into `buf` until it is full or the file ends; returns the bytes read.
    fn fill(&mut self, buf: &mut [u8], record: u64, at: u64) -> Result<usize, StreamError> {
        let mut got = 0;
        while got < buf.len() {
            match self.r.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(StreamError::io(Some(record), at, &e)),
            }
        }
        Ok(got)
    }
}

/// The resolved dependencies of a batch's records and the order rule they keep, shared by
/// [`check_batch`] and [`crate::closure`]: each record's depth is one more than the deepest
/// in-batch record it needs, at most [`MAX_HISTORY_DEPTH`].
pub(crate) fn depth_of(
    record: u64,
    at: u64,
    deps: impl IntoIterator<Item = u64>,
) -> Result<u64, StreamError> {
    let d = deps.into_iter().max().map_or(1, |d| d + 1);
    if d > MAX_HISTORY_DEPTH {
        return Err(StreamError::limit(Some(record), at, LIMIT_HISTORY_DEPTH));
    }
    Ok(d)
}

/// The rules the store keeps for every record of a kind, whatever the log around it, that a
/// batch check applies to each present record before it is counted: an ANSWER keeps the answer
/// record rules (`crate::answer`: relay and evidence together, an evidence kind only with a
/// relay, card evidence with its card hash, the plan acts' items, notes and targets), and a PLAN
/// its own record rules (`crate::plan`), with fsck's own text. Its targets rule is the
/// batch's edge rule; the typed rules that need other records (a plan's chain and seal, a
/// KeepSuspect's BLOCK targets) are the whole upload's (`crate::stream_source`). A take checks
/// every file of an upload this way, and the upload whole, so no take keeps an answer or a plan
/// that the holder's fsck would refuse.
fn record_rules(n: u64, offset: u64, r: &StreamRecord) -> Result<(), StreamError> {
    let StreamRecord::Present { key, payload, .. } = r else {
        return Ok(());
    };
    let payload = crate::codec::split(r.raw_kind(), payload)
        .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))?
        .body;
    if key.kind == crate::kinds::PLAN {
        // the plan record rules (`crate::plan`), with fsck's own text; its chain, bound
        // proposal and seal need the records around it, which the whole-upload check
        // (`crate::stream_source::check_typed_context`) checks before a take appends
        let p = crate::plan::decode_plan(payload)
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))?;
        return match crate::plan::invalid_record(&p) {
            Some(rule) => {
                Err(
                    StreamError::broken_rule(n, offset, &crate::plan::damage(&key.hash, rule))
                        .with_object(Some(*key)),
                )
            }
            None => Ok(()),
        };
    }
    if key.kind != crate::kinds::ANSWER {
        return Ok(());
    }
    let a: rdy_model::Answer = crate::bounded::decode(payload)
        .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))?;
    match crate::answer::invalid(&a) {
        Some(rule) => {
            Err(
                StreamError::broken_rule(n, offset, &crate::answer::damage(&key.hash, rule))
                    .with_object(Some(*key)),
            )
        }
        None => Ok(()),
    }
}

/// Checks one complete batch and returns its report: the framing ([`StreamReader`]), each
/// object record's bounded typed decode (`refs_of`) and its kind's record rules (an ANSWER's
/// relay and evidence rules), no repeated logical key, at most
/// [`MAX_EDGES`] edges, and the batch's dependency rules. Only when every check passes are its
/// keys returned.
///
/// Every edge must resolve to an earlier record of the batch or to a prerequisite, and every
/// removal proof a dropped record names to a record of the batch (before or after it) or to a
/// prerequisite. A kind-None edge resolves to exactly one of them by hash (two or more:
/// `ambiguous object kind`). An edge to the record itself or to a later record is `dependency
/// cycle` at the record that needs it. An edge or proof to neither is `unlisted dependency` at
/// the record that names it, with the edge's object (its hash alone for a kind-None edge).
///
/// The checks keep one order: the edges in record order, then the proofs in the order of their
/// dropped records, then the root, then the prerequisites. An absent root TAG that is no
/// prerequisite is `root tag is not in the batch or its prerequisites`, and a prerequisite that
/// no record needs is `unneeded prerequisite` with the smallest such key; both are at the header.
/// `invalid header` stays for a header's own fields. No order between batch files is required,
/// and E_UNRESOLVED_REF is never returned here: that needs the whole upload.
pub fn check_batch<R: Read>(input: R) -> Result<BatchReport, StreamError> {
    check_batch_with(input, |_| Ok(()), |_, _, _| Ok(()))
}

/// [`check_batch`] with two checks of the caller's own, in the same single pass: `on_header`
/// runs once the header is read, before any object record; `on_record` runs on every object
/// record once its frame, its typed decode and its key pass, with the record's number and frame
/// offset, before the next record is read. Either one's error ends the check, and no report is
/// returned. The record buffer is still the reader's one buffer: `on_record` borrows the record.
///
/// A batch is a tag history: a link delivery is E_STREAM_PURPOSE at its header, before
/// `on_header` runs ([`check_delivery`] checks it).
pub fn check_batch_with<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    mut on_record: impl FnMut(u64, u64, &StreamRecord) -> Result<(), E>,
) -> Result<BatchReport, E> {
    let (mut reader, header) = StreamReader::open_for(input, StreamPurpose::TagHistory)?;
    on_header(&header)?;
    let mut keys: Vec<ObjectKey> = Vec::new();
    let mut at: Vec<u64> = Vec::new();
    let mut index: HashMap<ObjectKey, u32> = HashMap::new();
    let mut edges: Vec<ObjectRef> = Vec::new();
    let mut ends: Vec<u32> = Vec::new();
    // the removal proofs the dropped records name, which keep no order, each with the index of
    // its dropped record
    let mut proofs: Vec<(ObjectKey, usize)> = Vec::new();
    while let Some(r) = reader.next()? {
        let (n, offset) = reader.position();
        let key = r.key();
        let grow = || StreamError::allocation(Some(n), offset);
        if index.contains_key(&key) {
            return Err(StreamError::bad_record(n, offset, Reason::ObjectIsRepeated)
                .with_object(Some(key))
                .into());
        }
        let refs = r
            .refs()
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(key)))?;
        record_rules(n, offset, &r)?;
        on_record(n, offset, &r)?;
        let proof = r.proof();
        if (edges.len() + proofs.len() + refs.len() + usize::from(proof.is_some())) as u64
            > MAX_EDGES
        {
            return Err(StreamError::limit(Some(n), offset, LIMIT_GRAPH_EDGES)
                .with_object(Some(key))
                .into());
        }
        edges.try_reserve(refs.len()).map_err(|_| grow())?;
        edges.extend(refs);
        if let Some(p) = proof {
            proofs.try_reserve(1).map_err(|_| grow())?;
            proofs.push((p, keys.len()));
        }
        keys.try_reserve(1).map_err(|_| grow())?;
        at.try_reserve(1).map_err(|_| grow())?;
        ends.try_reserve(1).map_err(|_| grow())?;
        index.try_reserve(1).map_err(|_| grow())?;
        index.insert(key, keys.len() as u32);
        keys.push(key);
        at.push(offset);
        ends.push(edges.len() as u32);
    }
    let totals = reader.finish()?;
    let prerequisites = &header.prerequisites;
    let is_prerequisite = |k: &ObjectKey| prerequisites.binary_search(k).is_ok();
    let mut needed: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut depth: Vec<u64> = Vec::new();
    depth
        .try_reserve_exact(keys.len())
        .map_err(|_| StreamError::allocation(Some(0), PRELUDE))?;
    let mut start = 0usize;
    for (i, end) in ends.iter().enumerate() {
        let n = i as u64 + 1;
        let fail = |reason| StreamError::bad_record(n, at[i], reason).with_object(Some(keys[i]));
        let mut deps = Vec::new();
        for e in &edges[start..*end as usize] {
            let target = match e.kind {
                Some(kind) => ObjectKey { kind, hash: e.hash },
                None => {
                    let mut found = SYNC_KINDS
                        .iter()
                        .map(|&kind| ObjectKey { kind, hash: e.hash })
                        .filter(|k| index.contains_key(k) || is_prerequisite(k));
                    let Some(first) = found.next() else {
                        return Err(fail(Reason::UnlistedDependency(*e)).into());
                    };
                    if found.next().is_some() {
                        return Err(fail(Reason::AmbiguousObjectKind).into());
                    }
                    first
                }
            };
            match index.get(&target) {
                Some(&j) if (j as usize) < i => deps.push(depth[j as usize]),
                Some(_) => return Err(fail(Reason::DependencyCycle).into()),
                None if is_prerequisite(&target) => {
                    needed.insert(target);
                }
                None => return Err(fail(Reason::UnlistedDependency(*e)).into()),
            }
        }
        depth.push(depth_of(n, at[i], deps).map_err(|e| e.with_object(Some(keys[i])))?);
        start = *end as usize;
    }
    // a removal proof: anywhere in the batch, or a prerequisite
    for &(p, i) in &proofs {
        if !index.contains_key(&p) {
            if !is_prerequisite(&p) {
                let named = ObjectRef {
                    kind: Some(p.kind),
                    hash: p.hash,
                };
                return Err(StreamError::bad_record(
                    i as u64 + 1,
                    at[i],
                    Reason::UnlistedDependency(named),
                )
                .with_object(Some(keys[i]))
                .into());
            }
            needed.insert(p);
        }
    }
    let at_header = |reason| StreamError::bad_record(0, PRELUDE, reason);
    let root = header.root();
    if !index.contains_key(&root) {
        if !is_prerequisite(&root) {
            return Err(at_header(Reason::RootTagNotListed).into());
        }
        needed.insert(root);
    }
    // every needed key is a prerequisite, so the first listed key that is not needed is the
    // smallest unneeded one
    if let Some(extra) = prerequisites.iter().find(|k| !needed.contains(k)) {
        return Err(at_header(Reason::UnneededPrerequisite(*extra)).into());
    }
    Ok(BatchReport {
        header,
        keys,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}

/// What a valid link delivery holds: its header, its `DELIVERY`, the logical key of every object
/// record in wire order (the selected versions' `BLOCK` keys, in the delivery's order), and its
/// trailer's records (the `DELIVERY` and the object records), bytes and digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryReport {
    pub header: StreamHeader,
    pub delivery: DeliveryRecord,
    pub keys: Vec<ObjectKey>,
    pub records: u64,
    pub bytes: u64,
    #[serde(serialize_with = "hex_hash")]
    pub digest: Hash,
}

/// Checks one complete link delivery and returns its report, before any intake: the framing
/// ([`StreamReader`]: the order, every hash, the bounds, the trailer and the end of the file),
/// the `DELIVERY`'s bounds and own rules, and the complete selection. Each object record must be
/// the next version the `DELIVERY` selects (else `object is outside the delivered selection`
/// at the record): its `BLOCK` key and encoding, and for a present record the block id of its
/// bounded typed decode. A dropped record keeps its record rules: its residue and a removable
/// encoding. A selected version with no record is E_UNRESOLVED_REF (`stream record 1 needs
/// <key>`) at the `DELIVERY`, for the first one. No reference of a block, no TAG and no
/// prerequisite is resolved: the selection and the payload hashes make the delivery complete.
/// A tag history is E_STREAM_PURPOSE at its header.
pub fn check_delivery<R: Read>(input: R) -> Result<DeliveryReport, StreamError> {
    check_delivery_with(input, |_, _, _| Ok(()))
}

/// [`check_delivery`] with a check of the caller's own in the same single pass: `on_record`
/// runs on every object record once its frame and its selection checks pass, with the record's
/// number and frame offset, before the next record is read. Its error ends the check, and no
/// report is returned.
pub fn check_delivery_with<R: Read, E: From<StreamError>>(
    input: R,
    mut on_record: impl FnMut(u64, u64, &StreamRecord) -> Result<(), E>,
) -> Result<DeliveryReport, E> {
    let (mut reader, header) = StreamReader::open_for(input, StreamPurpose::LinkDelivery)?;
    let delivery = reader
        .delivery
        .take()
        .expect("a link delivery reads its DELIVERY with its header");
    let delivery_at = reader.position().1;
    let mut keys: Vec<ObjectKey> = Vec::new();
    keys.try_reserve_exact(delivery.versions.len())
        .map_err(|_| StreamError::allocation(Some(1), delivery_at))?;
    let mut next = delivery.versions.iter();
    while let Some(r) = reader.next()? {
        let (n, offset) = reader.position();
        let key = r.key();
        let outside =
            || StreamError::bad_record(n, offset, Reason::OutsideSelection).with_object(Some(key));
        let Some(v) = next.next() else {
            return Err(outside().into());
        };
        let encoding = match &r {
            StreamRecord::Present { encoding, .. } | StreamRecord::Dropped { encoding, .. } => {
                *encoding
            }
        };
        if key.hash != v.version || encoding != v.encoding {
            return Err(outside().into());
        }
        if let Some(block) = r.typed::<Block>() {
            let block = block
                .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(key)))?;
            if block.id != v.id {
                return Err(outside().into());
            }
        }
        on_record(n, offset, &r)?;
        keys.push(key);
    }
    let totals = reader.finish()?;
    if let Some(v) = next.next() {
        let missing = ObjectKey {
            kind: kinds::BLOCK,
            hash: v.version,
        };
        return Err(StreamError::unresolved(1, delivery_at, missing)
            .with_object(Some(missing))
            .into());
    }
    Ok(DeliveryReport {
        header,
        delivery,
        keys,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}
