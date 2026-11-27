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
//! (`crate::bounded`); no logical key repeats; every object a record needs (`portable_refs_of`,
//! and a dropped object's residue) is an earlier record of the batch or a prerequisite; the
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
//!   strict id order, and the delivery's shells (D-2CG1N: id and version only, no object). The
//!   digest is the publication selection identity of the versions and the shells
//!   (`rdy_model::foreign::delivery_selection`). Its bounds are a typed record's
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
//!
//! # Stream format 4
//!
//! Format 4 (D-KE83X) keeps format 2's prelude, frames, header and trailer, and adds two
//! history purposes. Format 2 and 3 never carry them: a purpose in a format that does not know
//! it is E_STREAM_PURPOSE. A tag history stays at format 2 and a link delivery at format 3.
//!
//! - **`canon_history` (byte 2):** the header's 32-byte root field (`tag`) holds the upload's
//!   head `LANDING`. The batch carries its link closure, the attachments of a tag history, and
//!   each `TAG` that names a carried landing with that tag's answers and signatures.
//! - **`view_history` (byte 3):** the root field holds the head `VIEW_OP` of one view. The batch
//!   carries the op chain and every record the ops name, with their own closures and
//!   attachments.
//! - **Root:** the root object is a record of the batch or a prerequisite, as a tag history's
//!   root TAG is (`root landing is not in the batch or its prerequisites`, `root view op is not
//!   in the batch or its prerequisites`).
//! - **Features:** the header's manifest lists only the features that the upload's records need,
//!   the same list in every batch of one upload, never the source store's whole manifest. Its
//!   minimum (`StoreFeatures::min_rdy`) is then the minimum that the pushed records need.
//! - **Order and limits:** as format 2's.
//!
//! # Stream format 5
//!
//! Format 5 (D-GGTYA, D-4PV25) keeps format 2's prelude, frames, header and trailer, and adds one
//! download purpose, `canon_state` (byte 4): the state of one canon tip, which a hub sends to a
//! receiver that has no history yet. Only format 5 knows it, and format 5 knows no other purpose:
//! a canon state in an older format, and another purpose in format 5, are E_STREAM_PURPOSE.
//!
//! - **Root:** the header's root field holds the tip `LANDING`, a record of the batch or a
//!   prerequisite (`root landing is not in the batch or its prerequisites`).
//! - **Records:** the selection of `crate::canon_state`: the tip landing, its canonical SNAPSHOT
//!   (made from the map for the stream: no store need hold it), every BLOCK that the map names
//!   with its blobs, the tip's snapshot delta, and the tip's attachments. A link outside that
//!   selection is a frontier edge, which the header lists as a prerequisite.
//! - **Features:** the records' needs, as format 4 lists them, and `partial_history`: the
//!   receiver must read the prerequisites as edges a later history fetch brings.
//! - **Checks:** a canon state is no history: [`check_batch`] refuses it at its header, so no
//!   holder push, publication, view or fork take admits it. [`check_canon_state_batch`] checks
//!   one file with the same rules, and `crate::canon_state::StateUnion` checks the files of one
//!   fetch against the state's link table.
use crate::bounded::{CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_RECORD_BYTES, RECORD_BYTES, RecordError};
use crate::decoded_record::DecodedRecord;
use crate::dump::{
    ENCODED_DROPPED_LEN, Frame, LINK_LEN, check_dropped, encoded_dropped_payload, link,
    parse_encoded_dropped,
};
use crate::format::{
    FEATURE_SNAPSHOT_DELTA, MAX_MANIFEST_BYTES, StoreFeature, StoreFeatures, check_features,
    required_features,
};
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef, portable_refs_of};
use crate::segment::HEADER as FRAME_HEADER;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_HEADER_BYTES, LIMIT_HISTORY_DEPTH, LIMIT_OBJECT_RECORDS,
    LIMIT_PENDING_DROPPED_BYTES, LIMIT_PENDING_DROPS, LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES,
    Reason, StreamError,
};
use rdy_model::foreign::{ForeignPublication, ForeignVersion};
use rdy_model::{Block, Hash, RepoId};
use serde::{Deserialize, Serialize, Serializer};
use std::cell::Cell;
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
/// The stream format a canon or view history is written in: the first that knows the purposes
/// `canon_history` and `view_history` (D-KE83X).
pub const HISTORY_FORMAT: u32 = 4;
/// The stream format a canon state is written in: the only one that knows the purpose
/// `canon_state` (D-GGTYA).
pub const CANON_STATE_FORMAT: u32 = 5;
/// The stream formats this rdy reads: every format released from Release 1 on.
pub const READ_FORMATS: [u32; 4] = [
    STREAM_FORMAT,
    LINK_DELIVERY_FORMAT,
    HISTORY_FORMAT,
    CANON_STATE_FORMAT,
];

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
pub const SYNC_KINDS: [u8; 24] = [
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
    kinds::SNAPSHOT_DELTA,
];

/// [`SYNC_KINDS`].
pub fn sync_kinds() -> &'static [u8] {
    &SYNC_KINDS
}

/// Whether an object frame of `kind` may stand in a sync batch ([`sync_kinds`]).
pub fn is_sync_kind(kind: u8) -> bool {
    sync_kinds().contains(&kind)
}

/// What a stream carries: a tag's history, written `tag_history` and as the byte 0, a link
/// delivery, written `link_delivery` and as the byte 1 (stream format 3 on), a canon history
/// rooted at a landing, `canon_history` and the byte 2, or one view's history rooted at a view
/// op, `view_history` and the byte 3 (stream format 4 on, D-KE83X), or the state of one canon
/// tip, `canon_state` and the byte 4 (stream format 5 only, D-GGTYA).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPurpose {
    TagHistory,
    LinkDelivery,
    CanonHistory,
    ViewHistory,
    CanonState,
}

impl StreamPurpose {
    /// The purpose's header byte.
    pub fn byte(self) -> u8 {
        match self {
            StreamPurpose::TagHistory => 0,
            StreamPurpose::LinkDelivery => 1,
            StreamPurpose::CanonHistory => 2,
            StreamPurpose::ViewHistory => 3,
            StreamPurpose::CanonState => 4,
        }
    }

    /// The purpose of header byte `b`, if it is one.
    pub fn of(b: u8) -> Option<StreamPurpose> {
        match b {
            0 => Some(StreamPurpose::TagHistory),
            1 => Some(StreamPurpose::LinkDelivery),
            2 => Some(StreamPurpose::CanonHistory),
            3 => Some(StreamPurpose::ViewHistory),
            4 => Some(StreamPurpose::CanonState),
            _ => None,
        }
    }

    /// The purpose's name, as JSON and the purpose refusal write it.
    pub fn name(self) -> &'static str {
        match self {
            StreamPurpose::TagHistory => "tag_history",
            StreamPurpose::LinkDelivery => "link_delivery",
            StreamPurpose::CanonHistory => "canon_history",
            StreamPurpose::ViewHistory => "view_history",
            StreamPurpose::CanonState => "canon_state",
        }
    }

    /// The first stream format that knows the purpose.
    pub fn first_format(self) -> u32 {
        match self {
            StreamPurpose::TagHistory => STREAM_FORMAT,
            StreamPurpose::LinkDelivery => LINK_DELIVERY_FORMAT,
            StreamPurpose::CanonHistory | StreamPurpose::ViewHistory => HISTORY_FORMAT,
            StreamPurpose::CanonState => CANON_STATE_FORMAT,
        }
    }

    /// Whether the purpose carries a history of portable objects: a tag, canon or view history.
    /// A link delivery and a canon state do not: a canon state is a download only, so no take of
    /// a history admits it.
    pub fn is_history(self) -> bool {
        !matches!(
            self,
            StreamPurpose::LinkDelivery | StreamPurpose::CanonState
        )
    }

    /// Whether a stream of format `format` may carry the purpose: format 5 carries only a canon
    /// state, which no other format carries; formats 2 to 4 carry each purpose from its first
    /// format on.
    pub fn in_format(self, format: u32) -> bool {
        if self == StreamPurpose::CanonState || format == CANON_STATE_FORMAT {
            return self == StreamPurpose::CanonState && format == CANON_STATE_FORMAT;
        }
        format >= self.first_format()
    }

    /// The store kind of the purpose's root object: a `TAG`, a `LANDING` or a `VIEW_OP`. A link
    /// delivery's root field names a `TAG` for provenance only.
    pub fn root_kind(self) -> u8 {
        match self {
            StreamPurpose::TagHistory | StreamPurpose::LinkDelivery => kinds::TAG,
            StreamPurpose::CanonHistory | StreamPurpose::CanonState => kinds::LANDING,
            StreamPurpose::ViewHistory => kinds::VIEW_OP,
        }
    }

    /// The refusal reason of a history batch whose root is neither a record nor a prerequisite.
    fn root_not_listed(self) -> Reason {
        match self {
            StreamPurpose::CanonHistory | StreamPurpose::CanonState => Reason::RootLandingNotListed,
            StreamPurpose::ViewHistory => Reason::RootViewOpNotListed,
            StreamPurpose::TagHistory | StreamPurpose::LinkDelivery => Reason::RootTagNotListed,
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
    /// Every shell of the delivery, in id order (D-2CG1N): no `BLOCK` record follows for one.
    pub shells: Vec<rdy_model::foreign::ForeignShell>,
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
    match rdy_model::foreign::delivery_selection(
        d.versions.iter().map(|v| (&v.id, &v.version)),
        &d.shells,
    ) {
        Ok(s) if s == d.selection => Ok(()),
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

/// A batch's header: the stream format, the features (the source store's in format 2 and 3, the
/// upload's record needs in format 4), the source repo, the purpose, the upload's root and this
/// batch's prerequisites (strictly ascending logical keys). The root field keeps its name `tag`
/// for every purpose: a TAG's hash in a tag history, a LANDING's in a canon history, a VIEW_OP's
/// in a view history ([`StreamHeader::root`] gives its typed key). JSON writes the repo id and
/// the hash as full lowercase hex and each key as `{"kind", "hash"}`.
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

    /// The header of a canon history (stream format 4, D-KE83X) for `repo_id`, rooted at the
    /// head LANDING `landing`. `features` are the features that the upload's records need.
    pub fn canon_history(
        features: StoreFeatures,
        repo_id: RepoId,
        landing: Hash,
        prerequisites: Vec<ObjectKey>,
    ) -> Self {
        StreamHeader {
            stream_format: HISTORY_FORMAT,
            features,
            repo_id,
            purpose: StreamPurpose::CanonHistory,
            tag: landing,
            prerequisites,
        }
    }

    /// The header of a view history (stream format 4, D-KE83X) for `repo_id`, rooted at the
    /// head VIEW_OP `op` of one view. `features` are the features that the upload's records
    /// need.
    pub fn view_history(
        features: StoreFeatures,
        repo_id: RepoId,
        op: Hash,
        prerequisites: Vec<ObjectKey>,
    ) -> Self {
        StreamHeader {
            stream_format: HISTORY_FORMAT,
            features,
            repo_id,
            purpose: StreamPurpose::ViewHistory,
            tag: op,
            prerequisites,
        }
    }

    /// The header of a canon state (stream format 5, D-GGTYA) for `repo_id`, rooted at the tip
    /// LANDING `landing`. `features` are the features that the state's records need, with
    /// `partial_history`; `prerequisites` are the frontier edges the state omits.
    pub fn canon_state(
        features: StoreFeatures,
        repo_id: RepoId,
        landing: Hash,
        prerequisites: Vec<ObjectKey>,
    ) -> Self {
        StreamHeader {
            stream_format: CANON_STATE_FORMAT,
            features,
            repo_id,
            purpose: StreamPurpose::CanonState,
            tag: landing,
            prerequisites,
        }
    }

    /// The upload's root: the object the root field names, of the purpose's root kind
    /// ([`StreamPurpose::root_kind`]). A link delivery's tag is provenance.
    pub fn root(&self) -> ObjectKey {
        ObjectKey {
            kind: self.purpose.root_kind(),
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

    /// The objects the record needs before it, in a history of `features` (an incoming
    /// stream's declared features, a held store's own), as `portable_refs_of` gives them (no
    /// foreign-only edge: foreign content is never a tag's history, D-FK58A): a present record's
    /// references, a dropped record's residue (its apply is a later proof, see
    /// [`StreamRecord::proof`]). Sorted, no repeats.
    pub fn refs(&self, features: &StoreFeatures) -> Result<Vec<ObjectRef>, RecordError> {
        match self {
            StreamRecord::Present { payload, .. } => {
                portable_refs_of(features, self.raw_kind(), payload)
            }
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
    /// [`StreamRecord::refs`] of the record whose one decoded value is `d`
    /// ([`DecodedRecord::decode`] of this record): the same links, read from the value.
    pub(crate) fn refs_decoded(
        &self,
        features: &StoreFeatures,
        d: &DecodedRecord,
    ) -> Result<Vec<ObjectRef>, RecordError> {
        match self {
            StreamRecord::Present { .. } => {
                crate::object_refs::portable_refs_of_decoded(features, d)
            }
            StreamRecord::Dropped { .. } => self.refs(features),
        }
    }

    /// The features the record uses (`format::required_features`), read from its one decoded
    /// value `d`; a dropped record uses none.
    pub(crate) fn features_decoded(&self, d: &DecodedRecord) -> Vec<StoreFeature> {
        match self {
            StreamRecord::Present { payload, .. } => {
                match crate::codec::split(self.raw_kind(), payload) {
                    Ok(o) => crate::format::features_of(&o, Some(d)),
                    Err(_) => vec![],
                }
            }
            StreamRecord::Dropped { .. } => vec![],
        }
    }

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
/// (not sorted), the features its records need, and its trailer's records, bytes and digest. It
/// says the batch's framing and objects are valid, never that an upload is complete or a removal
/// authorized.
///
/// `record_features` has the type and JSON form of `header.features`: the union of
/// `format::required_features` over the batch's present records (the rule the take uses; a
/// dropped record needs none), sorted by name. It is empty when no record needs a feature. It
/// reports the records only: `header.features` stays what the header says, and no receiver is
/// refused for either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BatchReport {
    pub header: StreamHeader,
    pub keys: Vec<ObjectKey>,
    pub record_features: StoreFeatures,
    pub records: u64,
    pub bytes: u64,
    #[serde(serialize_with = "hex_hash")]
    pub digest: Hash,
}

/// The union of the features the records added to it need (`format::required_features`), for a
/// [`BatchReport`]'s `record_features`.
#[derive(Debug, Default)]
pub struct RecordFeatures(Vec<StoreFeature>);

impl RecordFeatures {
    /// Adds what `r` needs; a dropped record needs nothing.
    pub fn add(&mut self, r: &StreamRecord) {
        if let StreamRecord::Present { payload, .. } = r {
            for f in required_features(r.raw_kind(), payload) {
                if !self.0.iter().any(|g| g.name == f.name) {
                    self.0.push(f);
                }
            }
        }
    }

    /// Adds `used`, the features one record needs.
    fn add_all(&mut self, used: Vec<StoreFeature>) {
        for f in used {
            if !self.0.iter().any(|g| g.name == f.name) {
                self.0.push(f);
            }
        }
    }

    /// The place of `f` in the union, in the order of first use: added when new. A union past
    /// `u16` places is refused (no registered set comes near it).
    fn index_of(&mut self, f: &StoreFeature) -> Result<u16, ()> {
        let i = match self.0.iter().position(|g| g.name == f.name) {
            Some(i) => i,
            None => {
                self.0.try_reserve(1).map_err(|_| ())?;
                self.0.push(f.clone());
                self.0.len() - 1
            }
        };
        u16::try_from(i).map_err(|_| ())
    }

    /// The union, as a manifest sorted by name.
    pub fn finish(self) -> StoreFeatures {
        StoreFeatures::of(self.0)
    }

    /// [`RecordFeatures::finish`], and for each feature in the order of first use its place in
    /// the sorted manifest.
    fn finish_with_order(self) -> (StoreFeatures, Vec<u16>) {
        let manifest = StoreFeatures::of(self.0.clone());
        let order = self
            .0
            .iter()
            .map(|f| {
                manifest
                    .features
                    .iter()
                    .position(|g| g.name == f.name)
                    .expect("the manifest holds the union") as u16
            })
            .collect();
        (manifest, order)
    }
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
/// kind in a tag, canon or view history, a `BLOCK` alone in a link delivery.
fn purpose_kind(purpose: StreamPurpose, kind: u8) -> bool {
    // a canon state carries sync kinds, as a history does
    match purpose {
        StreamPurpose::LinkDelivery => kind == kinds::BLOCK,
        _ => is_sync_kind(kind),
    }
}

/// The bad header error: record 0, at the header frame.
fn bad_header() -> StreamError {
    StreamError::bad_record(0, PRELUDE, Reason::InvalidHeader)
}

/// The rules of a header's fields, shared by the writer and the reader: a stream format this rdy
/// reads, a purpose that format knows (E_STREAM_PURPOSE otherwise: format 2 knows only
/// `tag_history`, format 3 adds `link_delivery`, format 4 `canon_history` and `view_history`,
/// format 5 knows `canon_state` alone), a manifest this rdy reads (E_STORE_FORMAT otherwise), at most
/// [`MAX_PREREQUISITES`] prerequisites, each of a sync kind, strictly ascending, and none for a
/// link delivery.
fn check_header(h: &StreamHeader) -> Result<(), StreamError> {
    if !READ_FORMATS.contains(&h.stream_format) {
        return Err(bad_header());
    }
    if !h.purpose.in_format(h.stream_format) {
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
    if count > caps().prerequisites {
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
struct Pending {
    drops: u64,
    bytes: u64,
    max_drops: u64,
}

impl Default for Pending {
    fn default() -> Self {
        Pending {
            drops: 0,
            bytes: 0,
            max_drops: MAX_PENDING_DROPS,
        }
    }
}

impl Pending {
    fn add(&mut self, record: u64, at: u64, len: u32) -> Result<(), StreamError> {
        let drops = self.drops + 1;
        if drops > self.max_drops {
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
    /// Writes the prelude and `header`, a tag, canon or view history's or a canon state's, in its
    /// stream format. Refuses a link delivery (E_STREAM_PURPOSE: it starts with [`StreamWriter::new_delivery`]), a
    /// header of a stream or store format this rdy does not read, more than
    /// [`MAX_PREREQUISITES`] prerequisites, or prerequisites of a kind that is not a sync kind
    /// or not strictly ascending.
    pub fn new(w: W, header: &StreamHeader) -> Result<Self, StreamError> {
        if !header.purpose.is_history() && header.purpose != StreamPurpose::CanonState {
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

/// What an open requires of a stream's purpose: one purpose, or any history purpose.
#[derive(Clone, Copy)]
enum Expect {
    Purpose(StreamPurpose),
    History,
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
    /// The frame kind and hash of the last object record `next` returned.
    last_frame: (u8, Hash),
    caps: CheckCaps,
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
        Self::open_inner(r, Some(Expect::Purpose(purpose)))
    }

    /// [`StreamReader::open`] for a stream of a history purpose: a tag, canon or view history.
    /// A link delivery is E_STREAM_PURPOSE naming `tag_history`, refused at the header.
    pub fn open_history(r: R) -> Result<(Self, StreamHeader), StreamError> {
        Self::open_inner(r, Some(Expect::History))
    }

    /// A link delivery's `DELIVERY` record, read and checked with the header; None for a tag
    /// history.
    pub fn delivery(&self) -> Option<&DeliveryRecord> {
        self.delivery.as_ref()
    }

    fn open_inner(r: R, expected: Option<Expect>) -> Result<(Self, StreamHeader), StreamError> {
        let caps = caps();
        let mut me = StreamReader {
            r,
            purpose: StreamPurpose::TagHistory,
            delivery: None,
            offset: 0,
            records: 0,
            last: (0, PRELUDE),
            pending: Pending {
                max_drops: caps.pending_drops,
                ..Pending::default()
            },
            chain: Chain::default(),
            failed: None,
            totals: None,
            last_frame: (0, Hash([0; 32])),
            caps,
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
        let named = match expected {
            Some(Expect::Purpose(p)) => p,
            Some(Expect::History) | None => StreamPurpose::TagHistory,
        };
        let header = parse_header(&p, format, named)?;
        let fits = match expected {
            None => true,
            Some(Expect::Purpose(p)) => header.purpose == p,
            Some(Expect::History) => header.purpose.is_history(),
        };
        if !fits {
            return Err(wrong_purpose(header.purpose.byte(), named));
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
        if at + FRAME_HEADER + len > self.caps.total_bytes {
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

    /// The frame kind (a raw object kind, or `DROPPED`) and the verified frame hash of the last
    /// object record `next` returned.
    pub(crate) fn last_frame(&self) -> (u8, Hash) {
        self.last_frame
    }

    fn step(&mut self) -> Result<Option<StreamRecord>, StreamError> {
        let n = self.records + 1;
        let at = self.offset;
        let (len, kind, hash) = self.head(n)?;
        if kind == rec::TRAILER {
            if len != TRAILER_LEN as u64 {
                return Err(StreamError::truncated(Some(n), at));
            }
            if at + FRAME_HEADER + len > self.caps.total_bytes {
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
        if n > self.caps.records {
            return Err(StreamError::limit(Some(n), at, LIMIT_OBJECT_RECORDS));
        }
        if at + FRAME_HEADER + len > self.caps.total_bytes {
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
        self.last_frame = (kind, hash);
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
    depth_within(record, at, deps, MAX_HISTORY_DEPTH)
}

/// [`depth_of`] within `max` (a batch check's [`CheckCaps::depth`]), with the bound's text.
fn depth_within(
    record: u64,
    at: u64,
    deps: impl IntoIterator<Item = u64>,
    max: u64,
) -> Result<u64, StreamError> {
    let d = deps.into_iter().max().map_or(1, |d| d + 1);
    if d > max {
        return Err(StreamError::limit(Some(record), at, LIMIT_HISTORY_DEPTH));
    }
    Ok(d)
}

/// The count and byte bounds a batch read ([`StreamReader`]) and check ([`check_batch`])
/// keep: [`CheckCaps::RELEASED`], the constants of this module, unless a test lowers them on its
/// thread ([`lower_caps_for_tests`]). A lowered bound keeps the released bound's text.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckCaps {
    /// [`MAX_RECORDS`].
    pub records: u64,
    /// [`MAX_EDGES`].
    pub edges: u64,
    /// [`MAX_HISTORY_DEPTH`].
    pub depth: u64,
    /// [`MAX_PREREQUISITES`].
    pub prerequisites: usize,
    /// [`MAX_PENDING_DROPS`].
    pub pending_drops: u64,
    /// [`MAX_STREAM_BYTES`].
    pub total_bytes: u64,
}

impl CheckCaps {
    /// The bounds every batch keeps.
    pub const RELEASED: CheckCaps = CheckCaps {
        records: MAX_RECORDS,
        edges: MAX_EDGES,
        depth: MAX_HISTORY_DEPTH,
        prerequisites: MAX_PREREQUISITES,
        pending_drops: MAX_PENDING_DROPS,
        total_bytes: MAX_STREAM_BYTES,
    };
}

thread_local! {
    /// The bounds of this thread's batch reads and checks.
    static CAPS: Cell<CheckCaps> = const { Cell::new(CheckCaps::RELEASED) };
}

/// The bounds of this thread's batch reads and checks.
pub(crate) fn caps() -> CheckCaps {
    CAPS.with(Cell::get)
}

/// While held, this thread's batch reads and checks keep the earlier bounds again on drop.
#[doc(hidden)]
#[must_use]
pub struct CapsGuard(CheckCaps);

impl Drop for CapsGuard {
    fn drop(&mut self) {
        CAPS.with(|c| c.set(self.0));
    }
}

/// Test hook: this thread's batch reads and checks keep `caps` until the guard drops, so a
/// test can reach a bound and pass it by one with a small batch. A writer keeps the released
/// bounds. Production never lowers them.
#[doc(hidden)]
pub fn lower_caps_for_tests(caps: CheckCaps) -> CapsGuard {
    CapsGuard(CAPS.with(|c| c.replace(caps)))
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
///
/// The rules read `d`, the record's one decoded value ([`DecodedRecord`]); a canonical-byte
/// check compares that value with the record's own bytes.
fn record_rules(
    n: u64,
    offset: u64,
    r: &StreamRecord,
    d: &DecodedRecord,
) -> Result<(), StreamError> {
    let StreamRecord::Present { key, payload, .. } = r else {
        return Ok(());
    };
    let split = || {
        crate::codec::split(r.raw_kind(), payload)
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))
    };
    match d {
        // a SNAPSHOT's bytes are the canonical encoding of its map, with or without the
        // feature: postcard also decodes aliases (non-minimal varints, keys out of order or
        // repeated) that a landing's `state` could never fold to (SD1, D-7K4Z0)
        DecodedRecord::Snapshot(s) => {
            if crate::codec::is_canonical(&split()?, s.as_ref()) {
                return Ok(());
            }
            Err(StreamError::broken_rule(
                n,
                offset,
                &crate::snapshot::noncanonical_snapshot(&key.hash),
            )
            .with_object(Some(*key)))
        }
        DecodedRecord::SnapshotDelta(s) => {
            if crate::codec::is_canonical(&split()?, s.as_ref()) {
                return Ok(());
            }
            Err(StreamError::from_record(n, offset, &RecordError::Decode).with_object(Some(*key)))
        }
        // the plan record rules (`crate::plan`), with fsck's own text, of the frame alone: a
        // whole version's, or a delta's own fields and answer lists; its chain, bound proposal
        // and seal, and a delta's hydrated version, need the records around it, which the
        // whole-upload check (`crate::stream_source::check_typed_context`) checks before a take
        // appends
        DecodedRecord::Plan(p) => {
            let rule = match p.as_ref() {
                crate::plan_record::PlanRecord::Delta(d) if !d.lists_valid() => {
                    Some(crate::plan::INVALID_CHAIN)
                }
                p => crate::plan::invalid_record(&p.as_record_plan()),
            };
            match rule {
                Some(rule) => {
                    Err(
                        StreamError::broken_rule(n, offset, &crate::plan::damage(&key.hash, rule))
                            .with_object(Some(*key)),
                    )
                }
                None => Ok(()),
            }
        }
        DecodedRecord::Answer(a) => match crate::answer::invalid(a) {
            Some(rule) => {
                Err(
                    StreamError::broken_rule(n, offset, &crate::answer::damage(&key.hash, rule))
                        .with_object(Some(*key)),
                )
            }
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// The record rules of a present record as the batch check applied them before it shared one
/// decoded value: each rule decodes the record's bytes again. A test's baseline for the
/// refusals of the shared check ([`record_rules`]); no check calls it.
#[doc(hidden)]
pub fn record_rules_decoding_again_for_tests(
    n: u64,
    offset: u64,
    r: &StreamRecord,
) -> Result<(), StreamError> {
    let StreamRecord::Present { key, payload, .. } = r else {
        return Ok(());
    };
    // a SNAPSHOT's bytes are the canonical encoding of its map, with or without the feature:
    // postcard also decodes aliases (non-minimal varints, keys out of order or repeated) that a
    // landing's `state` could never fold to (SD1, D-7K4Z0)
    if key.kind == crate::kinds::SNAPSHOT {
        return match crate::codec::decode_canonical::<rdy_model::StateSnapshot>(
            r.raw_kind(),
            payload,
        ) {
            Ok(_) => Ok(()),
            Err(RecordError::Decode)
                if crate::codec::decode_bounded::<rdy_model::StateSnapshot>(
                    r.raw_kind(),
                    payload,
                )
                .is_ok() =>
            {
                Err(StreamError::broken_rule(
                    n,
                    offset,
                    &crate::snapshot::noncanonical_snapshot(&key.hash),
                )
                .with_object(Some(*key)))
            }
            Err(e) => Err(StreamError::from_record(n, offset, &e).with_object(Some(*key))),
        };
    }
    if key.kind == crate::kinds::SNAPSHOT_DELTA {
        return crate::codec::decode_canonical::<rdy_model::SnapshotDelta>(r.raw_kind(), payload)
            .map(|_| ())
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)));
    }
    let split = crate::codec::split(r.raw_kind(), payload)
        .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))?;
    let payload = split.body;
    if key.kind == crate::kinds::PLAN {
        // the plan record rules (`crate::plan`), with fsck's own text, of the frame alone: a
        // whole version's, or a delta's own fields and answer lists; its chain, bound proposal
        // and seal, and a delta's hydrated version, need the records around it, which the
        // whole-upload check (`crate::stream_source::check_typed_context`) checks before a take
        // appends
        let (p, _) = crate::plan_record::decode_body(split.version, payload)
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(*key)))?;
        let rule = match &p {
            crate::plan_record::PlanRecord::Delta(d) if !d.lists_valid() => {
                Some(crate::plan::INVALID_CHAIN)
            }
            _ => crate::plan::invalid_record(&p.as_record_plan()),
        };
        return match rule {
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
/// object record's bounded typed decode (`portable_refs_of`) and its kind's record rules (an
/// ANSWER's relay and evidence rules), no repeated logical key, at most
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
    check_batch_with(input, |_| Ok(()), |_, _, _, _| Ok(()))
}

/// [`check_batch`] with two checks of the caller's own, in the same single pass: `on_header`
/// runs once the header is read, before any object record; `on_record` runs on every object
/// record once its frame, its typed decode, its edges and its record rules pass, with the
/// record's number, its frame offset and its one decoded value ([`DecodedRecord`]), before the
/// next record is read. Either one's error ends the check, and no report is returned. The record
/// buffer is still the reader's one buffer, and the value the check's one value: `on_record`
/// borrows both, and both are dropped before the next frame is read.
///
/// A batch is a tag, canon or view history: a link delivery is E_STREAM_PURPOSE at its header,
/// before `on_header` runs ([`check_delivery`] checks it). A canon or view history's root is its
/// LANDING or VIEW_OP, with its own `root ... is not in the batch or its prerequisites` reason.
pub fn check_batch_with<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    on_record: impl FnMut(u64, u64, &StreamRecord, &DecodedRecord) -> Result<(), E>,
) -> Result<BatchReport, E> {
    check_inner(input, on_header, on_record, false, false).map(|(report, _)| report)
}

/// [`check_batch_with`] that also keeps what the check found, for a later stage of the same
/// operation: a [`CheckedBatch`], made only once every check of the batch passes. It keeps the
/// facts of each frame, never a decoded value or a payload.
pub fn check_batch_prepared_with<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    on_record: impl FnMut(u64, u64, &StreamRecord, &DecodedRecord) -> Result<(), E>,
) -> Result<CheckedBatch, E> {
    let (report, facts) = check_inner(input, on_header, on_record, true, false)?;
    let facts = facts.expect("a prepared check keeps its facts");
    Ok(CheckedBatch { report, facts })
}

/// Checks one complete canon state (stream format 5, D-GGTYA) as [`check_batch`] checks a
/// history: the framing, each record's bounded typed decode and record rules, no repeated key,
/// the edge bound and the dependency rules, the root landing among the records or the
/// prerequisites, and no unneeded prerequisite. Another purpose is E_STREAM_PURPOSE naming
/// `canon_state`, at its header. It says only that this file is valid: the files of one fetch
/// are checked against the state's link table together (`crate::canon_state::StateUnion`).
pub fn check_canon_state_batch<R: Read>(input: R) -> Result<BatchReport, StreamError> {
    check_inner(input, |_| Ok(()), |_, _, _, _| Ok(()), false, true).map(|(report, _)| report)
}

/// [`check_canon_state_batch`] with the caller's two checks of [`check_batch_with`], that also
/// keeps what the check found ([`CheckedBatch`]), as [`check_batch_prepared_with`] keeps it.
pub fn check_canon_state_prepared_with<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    on_record: impl FnMut(u64, u64, &StreamRecord, &DecodedRecord) -> Result<(), E>,
) -> Result<CheckedBatch, E> {
    let (report, facts) = check_inner(input, on_header, on_record, true, true)?;
    let facts = facts.expect("a prepared check keeps its facts");
    Ok(CheckedBatch { report, facts })
}

/// What a batch check found of one frame, as a [`CheckedBatch`] keeps it: no payload and no
/// decoded value.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckedFrame {
    raw_kind: u8,
    key: ObjectKey,
    offset: u64,
    frame_len: u32,
    frame_hash: Hash,
    dropped: bool,
    proof: Option<ObjectKey>,
    index_links: u64,
    links_end: u32,
    features_end: u32,
}

impl CheckedFrame {
    /// The record's raw kind: its logical kind, with the high bit when it is versioned
    /// ([`StreamRecord::raw_kind`]); a dropped record's names the encoding it was dropped in.
    pub fn raw_kind(&self) -> u8 {
        self.raw_kind
    }

    /// The record's logical key.
    pub fn key(&self) -> ObjectKey {
        self.key
    }

    /// The byte at which the record's frame starts.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The bytes of the frame's payload: the object's, or the 104 of a `DROPPED` record.
    pub fn frame_len(&self) -> u32 {
        self.frame_len
    }

    /// The frame's hash: `Hash::of` its payload, as its frame header states it and the check
    /// verified.
    pub fn frame_hash(&self) -> Hash {
        self.frame_hash
    }

    /// Whether the frame is a `DROPPED` record.
    pub fn is_dropped(&self) -> bool {
        self.dropped
    }

    /// The removal proof a dropped record names ([`StreamRecord::proof`]).
    pub fn proof(&self) -> Option<ObjectKey> {
        self.proof
    }

    /// The record's object links as a store's index counts them: `portable_refs_of` with no
    /// feature, so a LANDING counts its `state` under `snapshot_delta` too (the holder's edge
    /// count, `links_entry`); 0 for a dropped record and a blob chunk. It is not the number of
    /// the record's portable links ([`CheckedBatch::links`]).
    pub fn index_links(&self) -> u64 {
        self.index_links
    }
}

/// The facts a prepared check keeps besides its report: each frame's, each frame's resolved
/// links and needed features (flat, each frame's end in its own entry), the input's exact length
/// and its byte binding.
#[derive(Debug, PartialEq, Eq)]
struct Facts {
    frames: Vec<CheckedFrame>,
    links: Vec<ObjectKey>,
    /// The places in `links` (ascending) of the links whose own ref named no kind: the check
    /// resolved them within the batch and its prerequisites only.
    untyped: Vec<u32>,
    features: Vec<u16>,
    input_len: u64,
    binding: Hash,
}

/// One batch a check passed whole, and what the check found: its report, and each frame's raw
/// kind, key, offset, length, hash, resolved portable links, removal proof, needed features and
/// index link count, with the input's exact length and its byte binding
/// ([`CheckedBatch::binding`]). It is made only by [`check_batch_prepared_with`], after the
/// trailer, the end of the input and every dependency rule pass. It cannot be built from JSON or
/// from a caller's report, and it holds no payload and no decoded value.
///
/// It says what [`BatchReport`] says, for the exact bytes it checked: a later stage that reuses
/// it must first show that its input is those bytes (the same binding).
#[derive(Debug, PartialEq, Eq)]
pub struct CheckedBatch {
    report: BatchReport,
    facts: Facts,
}

impl CheckedBatch {
    /// The batch's report, as [`check_batch`] returns it.
    pub fn report(&self) -> &BatchReport {
        &self.report
    }

    /// The batch's report, the rest dropped.
    pub fn into_report(self) -> BatchReport {
        self.report
    }

    /// Each object frame's facts, in wire order.
    pub fn frames(&self) -> &[CheckedFrame] {
        &self.facts.frames
    }

    /// The object links of frame `i` (in wire order), each resolved to the key of a record of
    /// the batch or of a prerequisite, in the order of the record's sorted links: a present
    /// record's portable links under the header's features, a dropped record's residue.
    pub fn links(&self, i: usize) -> &[ObjectKey] {
        let start = i
            .checked_sub(1)
            .map_or(0, |p| self.facts.frames[p].links_end as usize);
        &self.facts.links[start..self.facts.frames[i].links_end as usize]
    }

    /// The object links of frame `i` (in wire order) as the record states them
    /// ([`StreamRecord::refs`] under the header's features): [`CheckedBatch::links`], with no
    /// kind on a link whose own ref named none. A later stage that resolves links over more
    /// records than the batch (an upload's union, a holder) reads these, so a link that the
    /// batch alone resolved is resolved again there.
    pub fn refs(&self, i: usize) -> Vec<ObjectRef> {
        let start = i
            .checked_sub(1)
            .map_or(0, |p| self.facts.frames[p].links_end as usize);
        let end = self.facts.frames[i].links_end as usize;
        let from = self
            .facts
            .untyped
            .partition_point(|u| (*u as usize) < start);
        let mut untyped = self.facts.untyped[from..].iter().peekable();
        (start..end)
            .map(|j| {
                let k = self.facts.links[j];
                let none = untyped.next_if(|u| **u as usize == j).is_some();
                ObjectRef {
                    kind: (!none).then_some(k.kind),
                    hash: k.hash,
                }
            })
            .collect()
    }

    /// The features frame `i` (in wire order) needs (`format::required_features`), each one of
    /// the report's `record_features`, sorted by name.
    pub fn features(&self, i: usize) -> impl Iterator<Item = &StoreFeature> + '_ {
        let start = i
            .checked_sub(1)
            .map_or(0, |p| self.facts.frames[p].features_end as usize);
        self.facts.features[start..self.facts.frames[i].features_end as usize]
            .iter()
            .map(|f| &self.report.record_features.features[*f as usize])
    }

    /// The exact length of the input the check read: the batch's bytes and its trailer frame,
    /// up to the end of the input.
    pub fn input_len(&self) -> u64 {
        self.facts.input_len
    }

    /// The binding of the exact bytes the check read ([`batch_binding`]): with the stream's
    /// framing, every byte of the input, its header, each frame's kind, length and payload
    /// (encoding tags and dropped-record fields included), its trailer and its end.
    pub fn binding(&self) -> Hash {
        self.facts.binding
    }
}

/// The binding of a batch's exact bytes: the hash of its stream format, its exact length and
/// its trailer's digest. A reader that verifies the framing ([`StreamReader`]: the prelude,
/// every frame's hash, the trailer and the end of the input) reads it again from the same bytes,
/// and from no others: the digest covers the header's and each object frame's kind, length and
/// payload hash, and the trailer and the length the rest.
pub fn batch_binding(header: &StreamHeader, totals: &StreamTotals) -> Hash {
    let mut b = Vec::with_capacity(BINDING_TAG.len() + 4 + 8 + 32);
    b.extend_from_slice(BINDING_TAG);
    b.extend_from_slice(&header.stream_format.to_le_bytes());
    b.extend_from_slice(&input_len(totals).to_le_bytes());
    b.extend_from_slice(&totals.digest.0);
    Hash::of(&b)
}

/// The domain of [`batch_binding`].
const BINDING_TAG: &[u8] = b"rdy-checked-batch-1\0";

/// The totals the trailer of the batch file `f` states, read from its last frame with no payload
/// read or hashed: what a count of a staged file's records needs before its full check. The frame
/// must be a `TRAILER` of [`TRAILER_LEN`] bytes whose `bytes` end exactly where the frame starts;
/// anything else is E_BAD_RECORD. The full check of the file stays [`check_batch`]'s.
pub fn read_trailer_totals(f: &mut std::fs::File) -> Result<StreamTotals, StreamError> {
    use std::io::{Seek, SeekFrom};
    let io = |e: std::io::Error| StreamError::io(None, 0, &e);
    let len = f.metadata().map_err(io)?.len();
    let frame = FRAME_HEADER + TRAILER_LEN as u64;
    let bad = |at: u64| StreamError::bad_record(0, at, Reason::InvalidHeader);
    if len < frame {
        return Err(bad(0));
    }
    let at = len - frame;
    f.seek(SeekFrom::Start(at)).map_err(io)?;
    let mut b = [0u8; (FRAME_HEADER as usize) + TRAILER_LEN];
    f.read_exact(&mut b).map_err(io)?;
    let n = u32::from_le_bytes(b[0..4].try_into().expect("four bytes")) as usize;
    if n != TRAILER_LEN || b[4] != rec::TRAILER {
        return Err(bad(at));
    }
    let p = &b[FRAME_HEADER as usize..];
    let totals = StreamTotals {
        records: u64::from_le_bytes(p[0..8].try_into().expect("eight bytes")),
        bytes: u64::from_le_bytes(p[8..16].try_into().expect("eight bytes")),
        digest: Hash(p[16..48].try_into().expect("32 bytes")),
    };
    if totals.bytes != at {
        return Err(bad(at));
    }
    Ok(totals)
}

/// The exact length of a batch whose trailer states `totals`: its bytes and its trailer frame.
fn input_len(totals: &StreamTotals) -> u64 {
    totals.bytes + FRAME_HEADER + TRAILER_LEN as u64
}

thread_local! {
    /// The batch checks (`check_inner`) this thread ran.
    static CHECKS: Cell<u64> = const { Cell::new(0) };
}

fn count_check() {
    CHECKS.with(|c| c.set(c.get() + 1));
}

/// For tests: how many whole batch checks ([`check_batch_with`], [`check_batch_prepared_with`]
/// and the calls over them) this thread ran so far. A test reads the difference over a call.
#[doc(hidden)]
pub fn batch_checks_for_tests() -> u64 {
    CHECKS.with(Cell::get)
}

/// Reads `input` again as the exact bytes that `checked` checked, and gives each object record
/// to `each` with its number in wire order, its checked frame and its place (record number and
/// frame offset). It is no batch check: it makes no typed decode and keeps none of the batch's
/// dependency rules. The reader ([`StreamReader`]) keeps the framing: the prelude, each frame's
/// payload hash, a dropped record's descriptor, the trailer and the end of the input, each
/// refused with its own error. Then the bytes must be the ones `checked` checked: the same
/// header, each frame's kind, raw encoding, offset, length and payload hash, and the batch's
/// binding ([`CheckedBatch::binding`], with its trailer and exact length). Anything else is
/// `input changed during take`, at the first record that differs (at the header for a header,
/// and at the last record for the trailer, a missing or an extra record). Path, length or time
/// of a file are never read as proof: only matching bytes keep the earlier check's result.
/// `each`'s error ends the read. Returns the header.
pub fn read_checked<R: Read, E: From<StreamError>>(
    input: R,
    checked: &CheckedBatch,
    mut each: impl FnMut(usize, &CheckedFrame, StreamRecord, (u64, u64)) -> Result<(), E>,
) -> Result<StreamHeader, E> {
    let changed =
        |(n, at): (u64, u64)| StreamError::bad_record(n, at, Reason::InputChangedDuringTake);
    let (mut reader, header) = StreamReader::open_history(input)?;
    if header != checked.report.header {
        return Err(changed((0, PRELUDE)).into());
    }
    let frames = checked.frames();
    let mut i = 0usize;
    while let Some(r) = reader.next()? {
        let place = reader.position();
        let (frame_kind, frame_hash) = reader.last_frame();
        let same = frames.get(i).is_some_and(|f| {
            f.key == r.key()
                && f.raw_kind == r.raw_kind()
                && f.offset == place.1
                && u64::from(f.frame_len) == r.frame_len()
                && f.frame_hash == frame_hash
                && f.dropped == (frame_kind == rec::DROPPED)
        });
        if !same {
            return Err(changed(place).with_object(Some(r.key())).into());
        }
        each(i, &frames[i], r, place)?;
        i += 1;
    }
    let last = reader.position();
    let totals = reader.finish()?;
    if i != frames.len()
        || totals.records != checked.report.records
        || batch_binding(&header, &totals) != checked.binding()
    {
        return Err(changed(last).into());
    }
    Ok(header)
}

/// The check of [`check_batch_with`] and [`check_batch_prepared_with`], one loop for both; with
/// `keep`, it also keeps the facts of a [`CheckedBatch`]. With `state` it reads a canon state
/// (format 5) and refuses every other purpose; without it, a history, and refuses a canon state.
fn check_inner<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    mut on_record: impl FnMut(u64, u64, &StreamRecord, &DecodedRecord) -> Result<(), E>,
    keep: bool,
    state: bool,
) -> Result<(BatchReport, Option<Facts>), E> {
    let caps = caps();
    count_check();
    let (mut reader, header) = if state {
        StreamReader::open_for(input, StreamPurpose::CanonState)?
    } else {
        StreamReader::open_history(input)?
    };
    on_header(&header)?;
    let deltas = header.features.has(FEATURE_SNAPSHOT_DELTA);
    let mut keys: Vec<ObjectKey> = Vec::new();
    let mut record_features = RecordFeatures::default();
    let mut at: Vec<u64> = Vec::new();
    let mut index: HashMap<ObjectKey, u32> = HashMap::new();
    let mut edges: Vec<ObjectRef> = Vec::new();
    let mut ends: Vec<u32> = Vec::new();
    // the removal proofs the dropped records name, which keep no order, each with the index of
    // its dropped record
    let mut proofs: Vec<(ObjectKey, usize)> = Vec::new();
    // a prepared check's frames, and each frame's needed features (indexes into
    // `record_features`, in the order of their first use)
    let mut frames: Vec<CheckedFrame> = Vec::new();
    let mut needs: Vec<u16> = Vec::new();
    while let Some(r) = reader.next()? {
        let (n, offset) = reader.position();
        let key = r.key();
        let grow = || StreamError::allocation(Some(n), offset);
        let refused =
            |e: RecordError| StreamError::from_record(n, offset, &e).with_object(Some(key));
        if index.contains_key(&key) {
            return Err(StreamError::bad_record(n, offset, Reason::ObjectIsRepeated)
                .with_object(Some(key))
                .into());
        }
        // the record's one typed value, which every check below reads
        let d = DecodedRecord::decode(&r).map_err(refused)?;
        // the links follow the features the stream declares (`portable_refs_of`)
        let refs = r.refs_decoded(&header.features, &d).map_err(refused)?;
        record_rules(n, offset, &r, &d)?;
        on_record(n, offset, &r, &d)?;
        let used = r.features_decoded(&d);
        let proof = r.proof();
        if (edges.len() + proofs.len() + refs.len() + usize::from(proof.is_some())) as u64
            > caps.edges
        {
            return Err(StreamError::limit(Some(n), offset, LIMIT_GRAPH_EDGES)
                .with_object(Some(key))
                .into());
        }
        if keep {
            // the store's index counts a landing's `state` whatever the features
            // (`links_entry`): only a landing under `snapshot_delta` counts another number
            let index_links = match (&r, &d) {
                (StreamRecord::Dropped { .. }, _) | (_, DecodedRecord::BlobChunk) => 0,
                (_, DecodedRecord::Landing(_)) if deltas => {
                    crate::object_refs::portable_refs_of_decoded(&StoreFeatures::of(vec![]), &d)
                        .map_err(refused)?
                        .len() as u64
                }
                _ => refs.len() as u64,
            };
            needs.try_reserve(used.len()).map_err(|_| grow())?;
            for f in &used {
                let i = record_features.index_of(f).map_err(|_| grow())?;
                needs.push(i);
            }
            let (frame_kind, frame_hash) = reader.last_frame();
            frames.try_reserve(1).map_err(|_| grow())?;
            frames.push(CheckedFrame {
                raw_kind: r.raw_kind(),
                key,
                offset,
                frame_len: r.frame_len() as u32,
                frame_hash,
                dropped: frame_kind == rec::DROPPED,
                proof,
                index_links,
                links_end: (edges.len() + refs.len()) as u32,
                features_end: needs.len() as u32,
            });
        } else {
            record_features.add_all(used);
        }
        drop(d);
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
    // a prepared check's resolved links, one per edge, and the places of the untyped ones
    let mut links: Vec<ObjectKey> = Vec::new();
    let mut untyped: Vec<u32> = Vec::new();
    if keep {
        links
            .try_reserve_exact(edges.len())
            .map_err(|_| StreamError::allocation(Some(0), PRELUDE))?;
    }
    let mut start = 0usize;
    for (i, end) in ends.iter().enumerate() {
        let n = i as u64 + 1;
        let fail = |reason| StreamError::bad_record(n, at[i], reason).with_object(Some(keys[i]));
        let mut deps = Vec::new();
        for e in &edges[start..*end as usize] {
            let target = match e.kind {
                Some(kind) => ObjectKey { kind, hash: e.hash },
                None => {
                    let mut found = sync_kinds()
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
            if keep {
                if e.kind.is_none() {
                    untyped
                        .try_reserve(1)
                        .map_err(|_| StreamError::allocation(Some(n), at[i]))?;
                    untyped.push(links.len() as u32);
                }
                links.push(target);
            }
        }
        depth.push(
            depth_within(n, at[i], deps, caps.depth).map_err(|e| e.with_object(Some(keys[i])))?,
        );
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
            return Err(at_header(header.purpose.root_not_listed()).into());
        }
        needed.insert(root);
    }
    // every needed key is a prerequisite, so the first listed key that is not needed is the
    // smallest unneeded one
    if let Some(extra) = prerequisites.iter().find(|k| !needed.contains(k)) {
        return Err(at_header(Reason::UnneededPrerequisite(*extra)).into());
    }
    // every check passed: only now are the facts kept
    let (record_features, facts) = if keep {
        let (sorted, order) = record_features.finish_with_order();
        for f in &mut needs {
            *f = order[*f as usize];
        }
        // each frame's features in the report's (name) order
        let mut begin = 0usize;
        for frame in &frames {
            let end = frame.features_end as usize;
            needs[begin..end].sort_unstable();
            begin = end;
        }
        let facts = Facts {
            frames,
            links,
            untyped,
            features: needs,
            input_len: input_len(&totals),
            binding: batch_binding(&header, &totals),
        };
        (sorted, Some(facts))
    } else {
        (record_features.finish(), None)
    };
    Ok((
        BatchReport {
            header,
            keys,
            record_features,
            records: totals.records,
            bytes: totals.bytes,
            digest: totals.digest,
        },
        facts,
    ))
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
