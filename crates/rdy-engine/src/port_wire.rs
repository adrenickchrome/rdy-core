//! The wire form of a port bundle, `rdy-port-bundle-1` (D-A3H7S, D-3DG3R). A bundle carries
//! landed changes of one source line to another line of its family. It is a value, not a dump
//! or a sync stream.
//!
//! The exact byte schema:
//!
//! ```text
//! bundle  = form len payload hash
//! form    = the 18 bytes "rdy-port-bundle-1\n"
//! len     = the payload's length in bytes, u64 little-endian
//! payload = record*
//! record  = a u32 little-endian length L, then L bytes: one tag byte and the record's body
//! hash    = BLAKE3 of form, len and payload (32 bytes)
//! ```
//!
//! The records come in this order. Each body is postcard, except a chunk's.
//!
//! 1. One header (tag 0): `{source: RepoId, fork_origins: Vec<RepoId>}`.
//! 2. For each landing, oldest first: a landing (tag 1), `{landing: Hash}`, then one change
//!    record (tag 2) for each of its changes, in landing order:
//!    `{hash: Hash, ops: Vec<BundleOpV1>, justification: Option<Justification>, message}`.
//! 3. For each blob, in ascending hash order: a manifest (tag 3), `{hash, len: u64, chunks}`.
//! 4. For each chunk, in ascending hash order: a chunk (tag 4), its 32-byte hash, then its bytes.
//!
//! `BundleOpV1` freezes the variants `Create {block}`, `Update {base, block}` and
//! `Delete {base, id}`, in that order. Each block uses model `ForeignBlockV1`, its foreign part
//! included even when it is empty. Ordinary `Op` serialization is never used.
//!
//! A bundle holds at most [`MAX_PORT_BUNDLE_BYTES`] bytes, all counted: form, lengths, records,
//! blobs and hash. Its size is [`BUNDLE_OVERHEAD`] plus, for each record, 4 plus its length. The
//! decoder checks the whole length before it reads a byte, and each record length before it
//! reads the record. A value of the wrong form, a wrong hash, a body that is not the canonical
//! postcard of its value, a duplicate landing, change, blob or chunk, or a blob that no op names
//! does not verify.
use crate::error::{E_BAD_INPUT, E_INTERNAL, RdyError};
use rdy_model::encoding::ForeignBlockV1;
use rdy_model::{Block, BlockId, Hash, Justification, Op, RepoId};
use rdy_store::bounded;
use rdy_store::codec;
use rdy_store::kinds::{self, BlobManifest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

/// The form line that starts every bundle of this form.
pub const PORT_BUNDLE_FORM: &[u8] = b"rdy-port-bundle-1\n";
/// The most bytes of one bundle: its whole value.
pub const MAX_PORT_BUNDLE_BYTES: usize = 67_108_864;
/// The bytes of a bundle outside its records: the form, the payload length and the hash.
pub const BUNDLE_OVERHEAD: usize = PORT_BUNDLE_FORM.len() + 8 + 32;
/// The bytes a record takes besides its body: its length and its tag.
pub const RECORD_OVERHEAD: usize = 4 + 1;

/// The message of a bundle that does not verify.
pub const PORT_UNVERIFIED_MESSAGE: &str = "the port bundle does not verify";
/// The fix of a bundle that does not verify.
pub const PORT_UNVERIFIED_FIX: &str = "make the bundle again in the source line";
/// The message of a bundle past its bounds on make.
pub const PORT_TOO_LARGE_MESSAGE: &str = "the port bundle is too large";
/// The fix of a bundle past its bounds on make.
pub const PORT_TOO_LARGE_FIX: &str = "carry fewer landings at a time";

const TAG_HEADER: u8 = 0;
const TAG_LANDING: u8 = 1;
const TAG_CHANGE: u8 = 2;
const TAG_MANIFEST: u8 = 3;
const TAG_CHUNK: u8 = 4;

/// `E_BAD_INPUT`: the bundle does not verify.
pub fn unverified() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_UNVERIFIED_MESSAGE, PORT_UNVERIFIED_FIX)
}

/// `E_BAD_INPUT`: the bundle is past its bounds on make.
pub fn too_large() -> RdyError {
    RdyError::new(E_BAD_INPUT, PORT_TOO_LARGE_MESSAGE, PORT_TOO_LARGE_FIX)
}

/// The decoded content of a bundle: the source line, its counted origins, the landings with
/// their changes, and the blobs those changes show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleContent {
    pub source: RepoId,
    /// The source's counted fork origins, ascending, without the source.
    pub fork_origins: Vec<RepoId>,
    /// The landings, oldest first, each with its changes in landing order.
    pub landings: Vec<BundleLanding>,
    /// The blob manifests, ascending by hash.
    pub blobs: Vec<BundleBlob>,
    /// The blob chunks, ascending by hash.
    pub chunks: Vec<BundleChunk>,
}

/// One landing of a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleLanding {
    pub landing: Hash,
    pub changes: Vec<BundleChange>,
}

/// One change of a bundle: its source hash, its ops with their bases, its justification and
/// its message. Nothing else of the source change travels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleChange {
    pub hash: Hash,
    pub ops: Vec<Op>,
    pub justification: Option<Justification>,
    pub message: String,
}

/// One blob manifest: its hash, its length and its chunks in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleBlob {
    pub hash: Hash,
    pub len: u64,
    pub chunks: Vec<Hash>,
}

/// One blob chunk: its hash and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleChunk {
    pub hash: Hash,
    pub bytes: Vec<u8>,
}

impl BundleBlob {
    /// The manifest as the store writes it.
    pub fn manifest(&self) -> BlobManifest {
        BlobManifest {
            len: self.len,
            chunks: self.chunks.clone(),
        }
    }

    /// The hash of a blob with this length and these chunks: its manifest's stored hash.
    pub fn hash_of(len: u64, chunks: &[Hash]) -> Hash {
        codec::encode_new(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len,
                chunks: chunks.to_vec(),
            },
        )
        .hash()
    }
}

impl BundleChunk {
    /// The hash of a chunk with these bytes: its stored hash.
    pub fn hash_of(bytes: &[u8]) -> Hash {
        Hash::of(&codec::frame_latest(kinds::BLOB_CHUNK, bytes.to_vec()).1)
    }
}

/// An op in the bundle, frozen for form 1: `Op`'s variants in their order, each block in the
/// foreign-capable layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum BundleOpV1 {
    Create { block: ForeignBlockV1 },
    Update { base: Hash, block: ForeignBlockV1 },
    Delete { base: Hash, id: BlockId },
}

impl BundleOpV1 {
    fn of(op: &Op) -> BundleOpV1 {
        match op {
            Op::Create { block } => BundleOpV1::Create {
                block: ForeignBlockV1::of(block),
            },
            Op::Update { base, block } => BundleOpV1::Update {
                base: *base,
                block: ForeignBlockV1::of(block),
            },
            Op::Delete { base, id } => BundleOpV1::Delete {
                base: *base,
                id: id.clone(),
            },
        }
    }

    fn into_current(self) -> Op {
        match self {
            BundleOpV1::Create { block } => Op::Create {
                block: block.into_current(),
            },
            BundleOpV1::Update { base, block } => Op::Update {
                base,
                block: block.into_current(),
            },
            BundleOpV1::Delete { base, id } => Op::Delete { base, id },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct HeaderV1 {
    source: RepoId,
    fork_origins: Vec<RepoId>,
}

#[derive(Serialize, Deserialize)]
struct LandingV1 {
    landing: Hash,
}

#[derive(Serialize, Deserialize)]
struct ChangeV1 {
    hash: Hash,
    ops: Vec<BundleOpV1>,
    justification: Option<Justification>,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct ManifestV1 {
    hash: Hash,
    len: u64,
    chunks: Vec<Hash>,
}

/// The postcard bytes of `v`, refused as too large past one record's bound.
fn postcard<T: Serialize>(v: &T) -> Result<Vec<u8>, RdyError> {
    let mut n = 256usize;
    loop {
        let mut buf = vec![0u8; n];
        if let Ok(used) = rdy_model::redaction::encode_into(v, &mut buf) {
            let len = used.len();
            buf.truncate(len);
            return Ok(buf);
        }
        if n >= bounded::MAX_RECORD_BYTES {
            return Err(too_large());
        }
        n = n.saturating_mul(2).min(bounded::MAX_RECORD_BYTES);
    }
}

/// The body of one record: its tag, then its bytes.
fn record(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(tag);
    out.extend_from_slice(body);
    out
}

/// The exact byte count of a bundle as its records are added: [`BUNDLE_OVERHEAD`], then 4 plus
/// each record's length. Past [`MAX_PORT_BUNDLE_BYTES`] is the make refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteCount(usize);

impl ByteCount {
    /// The count of a bundle with no record yet.
    pub fn new() -> ByteCount {
        ByteCount(BUNDLE_OVERHEAD)
    }

    /// Adds one record whose body (after its tag) is `body` bytes. Past the bound is refused.
    pub fn add(&mut self, body: usize) -> Result<(), RdyError> {
        self.0 = self
            .0
            .checked_add(RECORD_OVERHEAD)
            .and_then(|n| n.checked_add(body))
            .filter(|n| *n <= MAX_PORT_BUNDLE_BYTES)
            .ok_or_else(too_large)?;
        Ok(())
    }

    /// The bytes counted so far.
    pub fn bytes(&self) -> usize {
        self.0
    }
}

/// The body bytes of the header record of `source` and `origins`.
pub(crate) fn header_body(source: RepoId, origins: &[RepoId]) -> Result<Vec<u8>, RdyError> {
    postcard(&HeaderV1 {
        source,
        fork_origins: origins.to_vec(),
    })
}

/// The body bytes of the landing record of `landing`.
pub(crate) fn landing_body(landing: Hash) -> Result<Vec<u8>, RdyError> {
    postcard(&LandingV1 { landing })
}

/// The body bytes of the change record of `c`.
pub(crate) fn change_body(c: &BundleChange) -> Result<Vec<u8>, RdyError> {
    postcard(&ChangeV1 {
        hash: c.hash,
        ops: c.ops.iter().map(BundleOpV1::of).collect(),
        justification: c.justification.clone(),
        message: c.message.clone(),
    })
}

/// The body bytes of the manifest record of `b`.
pub(crate) fn manifest_body(b: &BundleBlob) -> Result<Vec<u8>, RdyError> {
    postcard(&ManifestV1 {
        hash: b.hash,
        len: b.len,
        chunks: b.chunks.clone(),
    })
}

/// The body length of the chunk record of a chunk of `len` bytes.
pub(crate) fn chunk_body_len(len: usize) -> usize {
    32 + len
}

/// Encodes `c` as a bundle, counting every byte as it goes: past [`MAX_PORT_BUNDLE_BYTES`] is
/// the make refusal, before the bytes are built. It checks bounds only; [`decode_bundle`]
/// checks the structure.
pub fn encode_bundle(c: &BundleContent) -> Result<Vec<u8>, RdyError> {
    let mut count = ByteCount::new();
    let mut records: Vec<Vec<u8>> = Vec::new();
    let mut push = |count: &mut ByteCount, tag: u8, body: Vec<u8>| -> Result<(), RdyError> {
        count.add(body.len())?;
        records.push(record(tag, &body));
        Ok(())
    };
    push(
        &mut count,
        TAG_HEADER,
        header_body(c.source, &c.fork_origins)?,
    )?;
    for l in &c.landings {
        push(&mut count, TAG_LANDING, landing_body(l.landing)?)?;
        for ch in &l.changes {
            push(&mut count, TAG_CHANGE, change_body(ch)?)?;
        }
    }
    for b in &c.blobs {
        push(&mut count, TAG_MANIFEST, manifest_body(b)?)?;
    }
    for k in &c.chunks {
        count.add(chunk_body_len(k.bytes.len()))?;
        let mut body = Vec::with_capacity(chunk_body_len(k.bytes.len()));
        body.extend_from_slice(&k.hash.0);
        body.extend_from_slice(&k.bytes);
        records.push(record(TAG_CHUNK, &body));
    }
    Ok(assemble(count.bytes(), &records))
}

/// The bundle of `records`, whose whole size is `total`: the form, the payload length, each
/// record with its length, then the hash.
fn assemble(total: usize, records: &[Vec<u8>]) -> Vec<u8> {
    let payload = total - BUNDLE_OVERHEAD;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(PORT_BUNDLE_FORM);
    out.extend_from_slice(&(payload as u64).to_le_bytes());
    for r in records {
        out.extend_from_slice(&(r.len() as u32).to_le_bytes());
        out.extend_from_slice(r);
    }
    let hash = Hash::of(&out);
    out.extend_from_slice(&hash.0);
    debug_assert_eq!(out.len(), total);
    out
}

/// The hash a bundle ends with.
pub fn bundle_hash(bytes: &[u8]) -> Option<Hash> {
    let tail = bytes.len().checked_sub(32)?;
    Some(Hash(bytes[tail..].try_into().ok()?))
}

/// Reads the records of a payload, one at a time, each length checked against what is left
/// before the record is read.
struct Records<'a> {
    rest: &'a [u8],
}

impl<'a> Records<'a> {
    /// The next record's tag and body, or None at the end.
    fn next(&mut self) -> Result<Option<(u8, &'a [u8])>, RdyError> {
        if self.rest.is_empty() {
            return Ok(None);
        }
        let (len, rest) = self.rest.split_at_checked(4).ok_or_else(unverified)?;
        let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
        if len == 0 || len > rest.len() {
            return Err(unverified());
        }
        let (r, rest) = rest.split_at(len);
        self.rest = rest;
        Ok(Some((r[0], &r[1..])))
    }

    /// The tag of the next record, without reading it.
    fn peek(&self) -> Option<u8> {
        self.rest.get(4).copied()
    }
}

/// One postcard record body as exactly one canonical `T`, within the record bounds.
fn decode_record<T: Serialize + serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, RdyError> {
    let v: T = bounded::decode(body).map_err(|_| unverified())?;
    if !bounded::encodes_as(&v, body) {
        return Err(unverified());
    }
    Ok(v)
}

/// Decodes and verifies `bytes` as a bundle: the length first, then the form, the payload
/// length and the hash, then each record in order, then the structure. Every fault is the
/// one refusal, [`unverified`].
pub fn decode_bundle(bytes: &[u8]) -> Result<BundleContent, RdyError> {
    if bytes.len() > MAX_PORT_BUNDLE_BYTES || bytes.len() < BUNDLE_OVERHEAD {
        return Err(unverified());
    }
    let (form, rest) = bytes.split_at(PORT_BUNDLE_FORM.len());
    if form != PORT_BUNDLE_FORM {
        return Err(unverified());
    }
    let (len, rest) = rest.split_at(8);
    let len = u64::from_le_bytes(len.try_into().expect("eight bytes"));
    if len != (bytes.len() - BUNDLE_OVERHEAD) as u64 {
        return Err(unverified());
    }
    let end = bytes.len() - 32;
    if bundle_hash(bytes) != Some(Hash::of(&bytes[..end])) {
        return Err(unverified());
    }
    let mut records = Records {
        rest: &rest[..rest.len() - 32],
    };
    // the header
    let header: HeaderV1 = match records.next()? {
        Some((TAG_HEADER, body)) => decode_record(body)?,
        _ => return Err(unverified()),
    };
    // the landings, each with its changes
    let mut landings: Vec<BundleLanding> = Vec::new();
    while records.peek() == Some(TAG_LANDING) {
        let (_, body) = records.next()?.expect("a peeked record");
        let l: LandingV1 = decode_record(body)?;
        let mut changes = Vec::new();
        while records.peek() == Some(TAG_CHANGE) {
            let (_, body) = records.next()?.expect("a peeked record");
            let c: ChangeV1 = decode_record(body)?;
            changes.push(BundleChange {
                hash: c.hash,
                ops: c.ops.into_iter().map(BundleOpV1::into_current).collect(),
                justification: c.justification,
                message: c.message,
            });
        }
        landings.push(BundleLanding {
            landing: l.landing,
            changes,
        });
    }
    // the manifests
    let mut blobs: Vec<BundleBlob> = Vec::new();
    while records.peek() == Some(TAG_MANIFEST) {
        let (_, body) = records.next()?.expect("a peeked record");
        let m: ManifestV1 = decode_record(body)?;
        blobs.push(BundleBlob {
            hash: m.hash,
            len: m.len,
            chunks: m.chunks,
        });
    }
    // the chunks
    let mut chunks: Vec<BundleChunk> = Vec::new();
    while let Some((tag, body)) = records.next()? {
        if tag != TAG_CHUNK || body.len() < 32 || body.len() - 32 > bounded::MAX_CHUNK_BYTES {
            return Err(unverified());
        }
        let (hash, data) = body.split_at(32);
        chunks.push(BundleChunk {
            hash: Hash(hash.try_into().expect("32 bytes")),
            bytes: data.to_vec(),
        });
    }
    let content = BundleContent {
        source: header.source,
        fork_origins: header.fork_origins,
        landings,
        blobs,
        chunks,
    };
    check_structure(&content)?;
    Ok(content)
}

/// The blobs a block shows: a figure's light and dark blob (none for any other block).
pub(crate) fn block_blobs(b: &Block) -> Vec<Hash> {
    crate::reader::review::figure_blobs(b)
}

/// The structure rules of a decoded bundle: at least one landing; origins ascending and without
/// the source; at most [`crate::port::MAX_PORT_CHANGES`] changes; no landing or change twice; no
/// removed projection; blobs and chunks ascending with no duplicate, each hash right, each
/// manifest's chunks held and their lengths its length, each chunk used, and the blobs exactly
/// those the ops show.
fn check_structure(c: &BundleContent) -> Result<(), RdyError> {
    let ascending = |hs: &mut dyn Iterator<Item = Hash>| {
        let mut last: Option<Hash> = None;
        for h in hs {
            if last.is_some_and(|l| l >= h) {
                return false;
            }
            last = Some(h);
        }
        true
    };
    if c.landings.is_empty() {
        return Err(unverified());
    }
    let mut last: Option<RepoId> = None;
    for o in &c.fork_origins {
        if *o == c.source || last.is_some_and(|l| l >= *o) {
            return Err(unverified());
        }
        last = Some(*o);
    }
    let mut seen_landings = HashSet::new();
    let mut seen_changes = HashSet::new();
    let mut shown: BTreeSet<Hash> = BTreeSet::new();
    let mut n = 0usize;
    for l in &c.landings {
        if !seen_landings.insert(l.landing) {
            return Err(unverified());
        }
        for ch in &l.changes {
            n += 1;
            if n > crate::port::MAX_PORT_CHANGES || !seen_changes.insert(ch.hash) {
                return Err(unverified());
            }
            for op in &ch.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    if block.is_redacted() {
                        return Err(unverified());
                    }
                    shown.extend(block_blobs(block));
                }
            }
        }
    }
    if !ascending(&mut c.blobs.iter().map(|b| b.hash))
        || !ascending(&mut c.chunks.iter().map(|k| k.hash))
    {
        return Err(unverified());
    }
    let mut lens = std::collections::HashMap::new();
    for k in &c.chunks {
        if BundleChunk::hash_of(&k.bytes) != k.hash {
            return Err(unverified());
        }
        lens.insert(k.hash, k.bytes.len() as u64);
    }
    let mut used: HashSet<Hash> = HashSet::new();
    for b in &c.blobs {
        if BundleBlob::hash_of(b.len, &b.chunks) != b.hash {
            return Err(unverified());
        }
        let mut sum = 0u64;
        for k in &b.chunks {
            let Some(len) = lens.get(k) else {
                return Err(unverified());
            };
            sum = sum.checked_add(*len).ok_or_else(unverified)?;
            used.insert(*k);
        }
        if sum != b.len {
            return Err(unverified());
        }
    }
    if used.len() != c.chunks.len() {
        return Err(unverified());
    }
    let carried: BTreeSet<Hash> = c.blobs.iter().map(|b| b.hash).collect();
    if carried != shown {
        return Err(unverified());
    }
    Ok(())
}

/// `E_INTERNAL`: a source object does not read as the store wrote it.
pub(crate) fn damaged(what: &str, h: &Hash) -> RdyError {
    RdyError::new(
        E_INTERNAL,
        format!("{what} {h} does not read as the store wrote it"),
        "run rdy fsck",
    )
}
