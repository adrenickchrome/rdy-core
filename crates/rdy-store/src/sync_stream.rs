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
//! # Stream format 1
//!
//! Integers are little endian.
//!
//! - **Prelude:** the 8 bytes `RDYSYNC\n`, then `stream_format: u32` (1).
//! - **Frames:** each is framed as a store frame: `len: u32`, `kind: u8`, `hash: [u8; 32]`
//!   (`Hash::of(payload)`), `payload`.
//! - **Order (closed):** one `HEADER` (0xF0); zero or more object frames, each a portable
//!   object of its store kind ([`SYNC_KINDS`]) or a `DROPPED` record (0xF3); one `TRAILER`
//!   (0xFF); the end of the file. A batch of no object frames is valid: its root is a
//!   prerequisite that the take must find.
//! - **`HEADER` payload:** `store_format: u32`, `repo_id: [u8; 16]`, `purpose: u8` (0:
//!   `tag_history`), `tag: [u8; 32]` (the root TAG's hash), `prerequisites: u32`, then that many
//!   keys `kind: u8 || hash: [u8; 32]`, strictly ascending. At most [`MAX_PREREQUISITES`] keys,
//!   so at most [`MAX_HEADER_BYTES`] bytes. No sequence number, upload id or end-of-upload mark.
//! - **`DROPPED` payload:** the 102-byte body of a dump's `DROPPED` record
//!   (`crate::dump::DROPPED_LEN`): `original_kind || original_hash || original_len: u32 ||
//!   apply_hash || has_residue: u8 || residue_hash`. Its logical key is the original kind and
//!   hash. It carries no place in a source log.
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
use crate::bounded::{CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_RECORD_BYTES, RECORD_BYTES, RecordError};
use crate::dump::{
    DROPPED_LEN, Frame, LINK_LEN, check_dropped, dropped_payload, link, parse_dropped,
};
use crate::format::STORE_FORMAT;
use crate::kinds;
use crate::object_refs::{ObjectKey, ObjectRef, refs_of};
use crate::segment::HEADER as FRAME_HEADER;
use crate::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_HEADER_BYTES, LIMIT_HISTORY_DEPTH, LIMIT_OBJECT_RECORDS,
    LIMIT_PENDING_DROPPED_BYTES, LIMIT_PENDING_DROPS, LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES,
    Reason, StreamError,
};
use rdy_model::{Hash, RepoId};
use serde::{Serialize, Serializer};
use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read, Write};

/// The 8 bytes every sync batch starts with.
pub const MAGIC: &[u8; 8] = b"RDYSYNC\n";
/// The stream format this rdy writes and reads.
pub const STREAM_FORMAT: u32 = 1;

/// The frame kinds of the stream's own records. Object frames keep their store kinds.
pub mod rec {
    pub const HEADER: u8 = 0xF0;
    /// A portable object whose payload an approved removal dropped.
    pub const DROPPED: u8 = 0xF3;
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
/// The fixed bytes of a header payload, before its prerequisite keys.
pub const HEADER_FIXED: usize = 4 + 16 + 1 + 32 + 4;
/// The bytes of one prerequisite key in a header.
pub const KEY_LEN: usize = 1 + 32;
/// The most bytes of one header payload: [`HEADER_FIXED`] plus [`MAX_PREREQUISITES`] keys.
pub const MAX_HEADER_BYTES: usize = HEADER_FIXED + MAX_PREREQUISITES * KEY_LEN;
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

/// The bytes of the prelude: the magic and the stream format.
const PRELUDE: u64 = MAGIC.len() as u64 + 4;

/// The store kinds an object frame may have: the portable kinds less TRANSITION, a batch's own
/// local record (no FILE, REF_UPDATE, BATCH_COMMIT or TRANSITION is portable). Ascending. A
/// kind-None link resolves among these.
pub const SYNC_KINDS: [u8; 18] = [
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
];

/// Whether an object frame of `kind` may stand in a sync batch.
pub fn is_sync_kind(kind: u8) -> bool {
    SYNC_KINDS.contains(&kind)
}

/// What a stream carries. One purpose exists: a tag's history, written `tag_history` and as the
/// byte 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPurpose {
    TagHistory,
}

impl StreamPurpose {
    /// The purpose's header byte.
    pub fn byte(self) -> u8 {
        match self {
            StreamPurpose::TagHistory => 0,
        }
    }

    /// The purpose of header byte `b`, if it is one.
    pub fn of(b: u8) -> Option<StreamPurpose> {
        (b == 0).then_some(StreamPurpose::TagHistory)
    }
}

/// A batch's header: the stream and store formats, the source repo, the purpose, the upload's
/// root TAG and this batch's prerequisites (strictly ascending keys). JSON writes the repo id and
/// the hash as full lowercase hex and each key as `{"kind", "hash"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StreamHeader {
    pub stream_format: u32,
    pub store_format: u32,
    #[serde(serialize_with = "hex_repo")]
    pub repo_id: RepoId,
    pub purpose: StreamPurpose,
    #[serde(serialize_with = "hex_hash")]
    pub tag: Hash,
    pub prerequisites: Vec<ObjectKey>,
}

impl StreamHeader {
    /// The header of a tag history of this rdy's formats, for `repo_id`, rooted at TAG `tag`.
    pub fn tag_history(repo_id: RepoId, tag: Hash, prerequisites: Vec<ObjectKey>) -> Self {
        StreamHeader {
            stream_format: STREAM_FORMAT,
            store_format: STORE_FORMAT,
            repo_id,
            purpose: StreamPurpose::TagHistory,
            tag,
            prerequisites,
        }
    }

    /// The upload's root: the TAG the header names.
    pub fn root(&self) -> ObjectKey {
        ObjectKey {
            kind: kinds::TAG,
            hash: self.tag,
        }
    }
}

/// One object record of a batch: a portable object's original bytes, or the identity of one
/// whose payload an approved removal dropped (its original length, the apply that removed it
/// and its residue, none for a blob chunk). JSON writes hashes as full lowercase hex, a payload
/// as lowercase hex and an absent residue as null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamRecord {
    Present {
        key: ObjectKey,
        #[serde(serialize_with = "hex_bytes")]
        payload: Vec<u8>,
    },
    Dropped {
        key: ObjectKey,
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

    /// The bytes of the record's frame payload: the object's, or the 102 of a `DROPPED` record.
    pub fn frame_len(&self) -> u64 {
        match self {
            StreamRecord::Present { payload, .. } => payload.len() as u64,
            StreamRecord::Dropped { .. } => DROPPED_LEN as u64,
        }
    }

    /// The objects the record needs before it, as `refs_of` gives them: a present record's
    /// references, a dropped record's residue (its apply is a later proof, see
    /// [`StreamRecord::proof`]). Sorted, no repeats.
    pub fn refs(&self) -> Result<Vec<ObjectRef>, RecordError> {
        match self {
            StreamRecord::Present { key, payload } => refs_of(key.kind, payload),
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
    let mut p = Vec::with_capacity(HEADER_FIXED + h.prerequisites.len() * KEY_LEN);
    p.extend_from_slice(&h.store_format.to_le_bytes());
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

/// The bad header error: record 0, at the header frame.
fn bad_header() -> StreamError {
    StreamError::bad_record(0, PRELUDE, Reason::InvalidHeader)
}

/// The rules of a header's fields, shared by the writer and the reader: this rdy's store format,
/// at most [`MAX_PREREQUISITES`] prerequisites, each of a sync kind, strictly ascending.
fn check_header(h: &StreamHeader) -> Result<(), StreamError> {
    if h.stream_format != STREAM_FORMAT {
        return Err(bad_header());
    }
    if h.store_format != STORE_FORMAT {
        return Err(StreamError::store_format(
            Some(0),
            PRELUDE,
            h.store_format,
            STORE_FORMAT,
        ));
    }
    if h.prerequisites.len() > MAX_PREREQUISITES {
        return Err(StreamError::limit(Some(0), PRELUDE, LIMIT_PREREQUISITES));
    }
    if h.prerequisites.iter().any(|k| !is_sync_kind(k.kind))
        || h.prerequisites.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(bad_header());
    }
    Ok(())
}

/// Parses a header payload (its length checked against [`MAX_HEADER_BYTES`] already).
fn parse_header(p: &[u8]) -> Result<StreamHeader, StreamError> {
    if p.len() < HEADER_FIXED {
        return Err(bad_header());
    }
    let store_format = u32::from_le_bytes(p[0..4].try_into().expect("4 bytes"));
    if store_format != STORE_FORMAT {
        return Err(StreamError::store_format(
            Some(0),
            PRELUDE,
            store_format,
            STORE_FORMAT,
        ));
    }
    let repo_id = RepoId(p[4..20].try_into().expect("16 bytes"));
    let purpose = StreamPurpose::of(p[20]).ok_or(StreamError::purpose(Some(0), PRELUDE, p[20]))?;
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
        stream_format: STREAM_FORMAT,
        store_format,
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

/// Writes one batch: the prelude and header on `new`, each object record on `record`, the
/// trailer on `finish`. Every check runs before a record's first byte, so a refused record
/// writes nothing. The writer checks the framing rules: kinds, hashes, dropped records, repeated
/// keys and every count and byte bound. The caller supplies records in dependency order with the
/// exact prerequisites ([`crate::closure`]); [`check_batch`] verifies both.
pub struct StreamWriter<W: Write> {
    w: W,
    offset: u64,
    records: u64,
    keys: BTreeSet<ObjectKey>,
    pending: Pending,
    chain: Chain,
}

impl<W: Write> StreamWriter<W> {
    /// Writes the prelude and `header`. Refuses a header of another stream or store format, more
    /// than [`MAX_PREREQUISITES`] prerequisites, or prerequisites of a kind that is not a sync
    /// kind or not strictly ascending.
    pub fn new(w: W, header: &StreamHeader) -> Result<Self, StreamError> {
        check_header(header)?;
        let mut me = StreamWriter {
            w,
            offset: 0,
            records: 0,
            keys: BTreeSet::new(),
            pending: Pending::default(),
            chain: Chain::default(),
        };
        me.raw(MAGIC)?;
        me.raw(&STREAM_FORMAT.to_le_bytes())?;
        let p = header_payload(header);
        let hash = Hash::of(&p);
        me.chain
            .add(0, PRELUDE, rec::HEADER, p.len() as u64, &hash)?;
        me.frame(rec::HEADER, &hash, &p)?;
        Ok(me)
    }

    /// Writes one object record: a present record as its own frame, a dropped one as its
    /// `DROPPED` record. Refuses a kind that is not a sync kind, a payload over its bound or not
    /// matching its key's hash, a dropped record that breaks its record rules, a key already
    /// written, and any count or byte bound.
    pub fn record(&mut self, r: &StreamRecord) -> Result<(), StreamError> {
        let n = self.records + 1;
        let at = self.offset;
        let key = r.key();
        let bad = |reason| StreamError::bad_record(n, at, reason).with_object(Some(key));
        if n > MAX_RECORDS {
            return Err(StreamError::limit(Some(n), at, LIMIT_OBJECT_RECORDS));
        }
        if !is_sync_kind(key.kind) {
            return Err(bad(Reason::UnknownKind));
        }
        if self.keys.contains(&key) {
            return Err(bad(Reason::ObjectIsRepeated));
        }
        self.within(n, at, r.frame_len())?;
        let (kind, hash, payload): (u8, Hash, std::borrow::Cow<[u8]>) = match r {
            StreamRecord::Present { payload, .. } => {
                payload_bound(n, at, key.kind, payload.len() as u64)?;
                if Hash::of(payload) != key.hash {
                    return Err(bad(Reason::HashMismatch));
                }
                (key.kind, key.hash, payload.as_slice().into())
            }
            StreamRecord::Dropped {
                original_len,
                apply,
                residue,
                ..
            } => {
                check_dropped(key.kind, residue.as_ref(), *original_len)
                    .map_err(|_| bad(Reason::InvalidDroppedRecord))?;
                self.pending.add(n, at, *original_len)?;
                let body =
                    dropped_payload(key.kind, &key.hash, *original_len, apply, residue.as_ref());
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
    offset: u64,
    records: u64,
    last: (u64, u64),
    pending: Pending,
    chain: Chain,
    failed: Option<StreamError>,
    totals: Option<StreamTotals>,
}

impl<R: Read> StreamReader<R> {
    /// Reads the prelude and the header. Refuses another magic or stream format and a header that
    /// is not the first frame (`invalid header`), a header over [`MAX_HEADER_BYTES`], a store
    /// format other than this rdy's (E_STORE_FORMAT), a purpose other than `tag_history`
    /// (E_STREAM_PURPOSE), and prerequisites out of their rules.
    pub fn open(r: R) -> Result<(Self, StreamHeader), StreamError> {
        let mut me = StreamReader {
            r,
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
        if u32::from_le_bytes(format) != STREAM_FORMAT {
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
        let header = parse_header(&p)?;
        me.chain.add(0, PRELUDE, rec::HEADER, len, &hash)?;
        me.offset = PRELUDE + FRAME_HEADER + len;
        Ok((me, header))
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
        if kind != rec::DROPPED && !is_sync_kind(kind) {
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
            if len != DROPPED_LEN as u64 {
                return Err(bad(Reason::InvalidDroppedRecord));
            }
            let p = self.payload(n, at, len, &hash)?;
            let Ok(Frame::Dropped {
                kind,
                hash: original,
                len: original_len,
                apply,
                residue,
            }) = parse_dropped(&p)
            else {
                return Err(bad(Reason::InvalidDroppedRecord));
            };
            // a removable kind (`parse_dropped`), so a sync kind
            let key = ObjectKey {
                kind,
                hash: original,
            };
            self.pending
                .add(n, at, original_len)
                .map_err(|e| e.with_object(Some(key)))?;
            StreamRecord::Dropped {
                key,
                original_len,
                apply,
                residue,
            }
        } else {
            let key = ObjectKey { kind, hash };
            payload_bound(n, at, kind, len).map_err(|e| e.with_object(Some(key)))?;
            let payload = self
                .payload(n, at, len, &hash)
                .map_err(|e| e.with_object(Some(key)))?;
            StreamRecord::Present { key, payload }
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

/// Checks one complete batch and returns its report: the framing ([`StreamReader`]), each
/// object record's bounded typed decode (`refs_of`), no repeated logical key, at most
/// [`MAX_EDGES`] edges, and the batch's dependency rules. Only when every check passes are its
/// keys returned.
///
/// Every edge must resolve to an earlier record of the batch or to a prerequisite, and every
/// removal proof a dropped record names to a record of the batch (before or after it) or to a
/// prerequisite. A kind-None edge resolves to exactly one of them by hash (two or more:
/// `ambiguous object kind`). An edge to the record itself or to a later record is `dependency
/// cycle` at the record that needs it; an edge or proof to neither, an absent root TAG that is
/// no prerequisite and a prerequisite that no record needs are each `invalid header` at the
/// header. No order between batch files is
/// required, and E_UNRESOLVED_REF is never returned here: that needs the whole upload.
pub fn check_batch<R: Read>(input: R) -> Result<BatchReport, StreamError> {
    check_batch_with(input, |_| Ok(()), |_, _, _| Ok(()))
}

/// [`check_batch`] with two checks of the caller's own, in the same single pass: `on_header`
/// runs once the header is read, before any object record; `on_record` runs on every object
/// record once its frame, its typed decode and its key pass, with the record's number and frame
/// offset, before the next record is read. Either one's error ends the check, and no report is
/// returned. The record buffer is still the reader's one buffer: `on_record` borrows the record.
pub fn check_batch_with<R: Read, E: From<StreamError>>(
    input: R,
    on_header: impl FnOnce(&StreamHeader) -> Result<(), E>,
    mut on_record: impl FnMut(u64, u64, &StreamRecord) -> Result<(), E>,
) -> Result<BatchReport, E> {
    let (mut reader, header) = StreamReader::open(input)?;
    on_header(&header)?;
    let mut keys: Vec<ObjectKey> = Vec::new();
    let mut at: Vec<u64> = Vec::new();
    let mut index: HashMap<ObjectKey, u32> = HashMap::new();
    let mut edges: Vec<ObjectRef> = Vec::new();
    let mut ends: Vec<u32> = Vec::new();
    // the removal proofs the dropped records name, which keep no order
    let mut proofs: Vec<ObjectKey> = Vec::new();
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
            proofs.push(p);
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
                    let first = found.next().ok_or_else(bad_header)?;
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
                None => return Err(bad_header().into()),
            }
        }
        depth.push(depth_of(n, at[i], deps).map_err(|e| e.with_object(Some(keys[i])))?);
        start = *end as usize;
    }
    // a removal proof: anywhere in the batch, or a prerequisite
    for p in &proofs {
        if !index.contains_key(p) {
            if !is_prerequisite(p) {
                return Err(bad_header().into());
            }
            needed.insert(*p);
        }
    }
    let root = header.root();
    if !index.contains_key(&root) {
        if !is_prerequisite(&root) {
            return Err(bad_header().into());
        }
        needed.insert(root);
    }
    if needed.len() != prerequisites.len() {
        return Err(bad_header().into());
    }
    Ok(BatchReport {
        header,
        keys,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}
