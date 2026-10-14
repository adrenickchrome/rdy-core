//! The dump stream (plan Dump and load, D-D2NPN, D-WWAK7): one whole project as one file. It is
//! the long-lived exchange format. From its first release, every later rdy reads every dump
//! format that rdy has released (D-69AY3). The stream copies the log's sealed batches in log
//! order, byte for byte. The log is in dependency order already, because a frame can only name
//! objects written before it.
//!
//! # Dump format 1
//!
//! Integers are little endian.
//!
//! - **Prelude:** the 8 bytes `RDYDUMP\n`, then `dump_format: u32` (1).
//! - **Records:** each is framed as a store frame: `len: u32`, `kind: u8`, `hash: [u8; 32]`
//!   (`Hash::of(payload)`), `payload`. Record kinds of the container: `HEADER = 0xF0`,
//!   `FILE = 0xF1`, `BATCH = 0xF2`, `TRAILER = 0xFF`. The frames of a batch keep their store
//!   kinds: exactly the kinds that the header's store format assigns (`frame_kinds`; for the pair
//!   (1, 10): 1 to 9, 13, and 15 to 19). Never 14 (the batch marker), never an unassigned kind.
//! - **Order (closed):** one `HEADER`; zero or more `FILE`; zero or more `BATCH`, each followed
//!   by exactly its `frames` store frames; one `TRAILER`; the end of the file.
//! - **`HEADER` payload:** `store_format: u32`, `log_end.segment: u32`, `log_end.offset: u64`,
//!   `rdy_len: u16`, `rdy: [u8; rdy_len]` (UTF-8, at most 256 bytes; the writer's version, for
//!   people only, never checked).
//! - **`FILE` payload:** `path_len: u16`, `path: [u8; path_len]` (UTF-8), then the file's bytes.
//!   `path` is one of the closed set: `.rdy.yaml`; `.rdyignore`; `.rdy/packs/<name>` where
//!   `<name>` uses only `A-Z a-z 0-9 . _ -`, does not start with `.`, and ends in `.yaml` or
//!   `.yml`. At most 256 `FILE` records; each file at most 16 MiB; paths strictly ascending, no
//!   repeats.
//! - **`BATCH` payload:** `start.segment: u32`, `start.offset: u64`, `frames: u32`,
//!   `digest: [u8; 32]` (the source batch's `BatchCommit`), then `kinds: [u8; 32]`: `Hash::of`
//!   over `kind: u8 || hash: [u8; 32]` of each of the batch's frames, in order. `frames` is at
//!   least 1. The next `frames` records are that batch's data frames, byte for byte as the log
//!   holds them.
//! - **`TRAILER` payload:** `batches: u64`, `frames: u64`, `files: u32`, `digest: [u8; 32]`:
//!   `Hash::of` over `kind: u8 || len: u64 || hash: [u8; 32]` (`len` the payload's length) of
//!   every record from `HEADER` up to the last record before the trailer, store frames included,
//!   in order.
//!
//! A record's hash covers its payload only, so the `BATCH` record's `kinds` and the trailer's
//! digest bind every record's kind (and the trailer every length and the order): a flipped kind
//! byte fails them.
//!
//! A reader checks every record's hash, the order above, each frame's kind, each batch's frame
//! count and both its digests, and the trailer's counts and digest. Any failure is
//! `StoreError::Dump`: rdy never repairs a dump.
use crate::error::StoreError;
use crate::kinds::{self, BatchCommit, batch_digest};
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::StoreReader;
use rdy_model::Hash;
use std::io::{self, Read, Write};

/// The 8 bytes every dump starts with.
pub const MAGIC: &[u8; 8] = b"RDYDUMP\n";
/// The dump format this rdy writes.
pub const DUMP_FORMAT: u32 = 1;
/// The `(dump_format, store_format)` pairs this rdy reads. A pair, once released, never leaves
/// this list (D-69AY3).
pub const READS: &[(u32, u32)] = &[(1, 10)];
/// At most this many `FILE` records.
pub const MAX_FILES: u32 = 256;
/// At most this many bytes in one project file.
pub const MAX_FILE_BYTES: usize = 16 << 20;
/// At most this many bytes in the header's `rdy` text.
const MAX_RDY_BYTES: usize = 256;

/// The record kinds of the container. Store frames keep their store kinds.
pub mod rec {
    pub const HEADER: u8 = 0xF0;
    pub const FILE: u8 = 0xF1;
    pub const BATCH: u8 = 0xF2;
    pub const TRAILER: u8 = 0xFF;
}

const BATCH_LEN: usize = 4 + 8 + 4 + 32 + 32;
const TRAILER_LEN: usize = 8 + 8 + 4 + 32;

/// One store frame of a batch, as the log holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub hash: Hash,
    pub payload: Vec<u8>,
}

/// The dump's header: the store format of its frames, the source log's end, and the writing
/// rdy's version (for people only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpHeader {
    pub store_format: u32,
    pub log_end: Location,
    pub rdy: String,
}

/// One sealed batch of the source log: its marker's start and digest, and its data frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpBatch {
    pub start: Location,
    pub digest: Hash,
    pub frames: Vec<Frame>,
}

/// One item of a dump after its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DumpItem {
    File { path: String, bytes: Vec<u8> },
    Batch(DumpBatch),
}

/// What a dump holds so far: batches, store frames, project files, and bytes of the stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DumpTotals {
    pub batches: u64,
    pub frames: u64,
    pub files: u32,
    pub bytes: u64,
}

/// True if `path` is one of the project files a dump carries (the closed set): `.rdy.yaml`,
/// `.rdyignore`, or `.rdy/packs/<name>` with a plain YAML file name.
pub fn is_project_file(path: &str) -> bool {
    if path == ".rdy.yaml" || path == ".rdyignore" {
        return true;
    }
    let Some(name) = path.strip_prefix(".rdy/packs/") else {
        return false;
    };
    !name.starts_with('.')
        && (name.ends_with(".yaml") || name.ends_with(".yml"))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The store kinds a batch's frames may have, for each `(dump_format, store_format)` pair of
/// `READS`: exactly the kinds that store format assigns, never the batch marker. The writer and
/// the reader both use this one table. A pair, once released, never changes its set.
const FRAME_KINDS: &[((u32, u32), &[u8])] = &[(
    (1, 10),
    &[
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::BLOB_CHUNK,
        kinds::BLOB_MANIFEST,
        kinds::REF_UPDATE,
        kinds::TAG,
        kinds::VIEW_OP,
        kinds::INTENT,
        kinds::ALT_SET,
        kinds::REVIEW,
        kinds::TENSION,
        kinds::TRANSITION,
    ],
)];

/// The store kinds a batch's frames may have in a dump of `dump_format` whose frames are of
/// `store_format`; None for a pair this rdy does not read.
pub fn frame_kinds(dump_format: u32, store_format: u32) -> Option<&'static [u8]> {
    FRAME_KINDS
        .iter()
        .find(|(pair, _)| *pair == (dump_format, store_format))
        .map(|(_, k)| *k)
}

/// The `BATCH` record's `kinds`: `Hash::of` over each frame's `kind || hash`, in order.
fn kinds_digest(frames: &[Frame]) -> Hash {
    let mut b = Vec::with_capacity(frames.len() * 33);
    for f in frames {
        b.push(f.kind);
        b.extend_from_slice(&f.hash.0);
    }
    Hash::of(&b)
}

/// Adds one record to the bytes the trailer's digest covers: `kind || len (u64) || hash`.
fn link(chain: &mut Vec<u8>, kind: u8, len: u64, hash: &Hash) {
    chain.push(kind);
    chain.extend_from_slice(&len.to_le_bytes());
    chain.extend_from_slice(&hash.0);
}

fn cannot_stand(kind: u8) -> String {
    format!("a record of kind {kind} cannot stand here")
}

fn not_carried(path: &str) -> String {
    format!("the project file path {path:?} is not one rdy carries")
}

fn too_large(path: &str) -> String {
    format!("the project file {path:?} is larger than 16 MiB")
}

fn header_payload(h: &DumpHeader) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 4 + 8 + 2 + h.rdy.len());
    p.extend_from_slice(&h.store_format.to_le_bytes());
    p.extend_from_slice(&h.log_end.segment.to_le_bytes());
    p.extend_from_slice(&h.log_end.offset.to_le_bytes());
    p.extend_from_slice(&(h.rdy.len() as u16).to_le_bytes());
    p.extend_from_slice(h.rdy.as_bytes());
    p
}

fn parse_header(p: &[u8]) -> Option<DumpHeader> {
    let mut c = Cursor(p);
    let store_format = c.u32()?;
    let segment = c.u32()?;
    let offset = c.u64()?;
    let rdy_len = c.u16()? as usize;
    if rdy_len > MAX_RDY_BYTES {
        return None;
    }
    let rdy = String::from_utf8(c.take(rdy_len)?.to_vec()).ok()?;
    if !c.0.is_empty() {
        return None;
    }
    Some(DumpHeader {
        store_format,
        log_end: Location { segment, offset },
        rdy,
    })
}

/// Reads little-endian fields off the front of a payload.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn hash(&mut self) -> Option<Hash> {
        Some(Hash(self.take(32)?.try_into().ok()?))
    }
}

/// Where the writer is in the closed order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Files,
    Batches,
}

/// Writes a dump: the prelude and header on `new`, then project files, then batches, then the
/// trailer on `finish`. It refuses anything out of the closed order with `StoreError::Refused`.
pub struct DumpWriter<W: Write> {
    w: W,
    phase: Phase,
    last_path: Option<String>,
    /// The store kinds a frame may have (`frame_kinds` of the header's pair).
    kinds: &'static [u8],
    /// `kind || len || hash` of every record so far (the trailer's digest covers them).
    chain: Vec<u8>,
    totals: DumpTotals,
}

impl<W: Write> DumpWriter<W> {
    /// Writes the prelude and the header. Refuses a header whose store format is not one this
    /// rdy reads in a dump of `DUMP_FORMAT` (`READS`): its reader would refuse the dump.
    pub fn new(w: W, header: &DumpHeader) -> Result<Self, StoreError> {
        if header.rdy.len() > MAX_RDY_BYTES {
            return Err(StoreError::Refused("the header does not parse".into()));
        }
        let Some(kinds) = READS
            .contains(&(DUMP_FORMAT, header.store_format))
            .then(|| frame_kinds(DUMP_FORMAT, header.store_format))
            .flatten()
        else {
            return Err(StoreError::Refused(store_format_not_read(
                header.store_format,
                DUMP_FORMAT,
            )));
        };
        let mut me = DumpWriter {
            w,
            phase: Phase::Files,
            last_path: None,
            kinds,
            chain: Vec::new(),
            totals: DumpTotals::default(),
        };
        me.w.write_all(MAGIC)?;
        me.w.write_all(&DUMP_FORMAT.to_le_bytes())?;
        me.totals.bytes = (MAGIC.len() + 4) as u64;
        me.record(rec::HEADER, &header_payload(header))?;
        Ok(me)
    }

    /// Writes one project file. Refuses a path outside the closed set, a path not after the
    /// previous one, a file over 16 MiB, more than `MAX_FILES` files, and any file after a batch.
    pub fn file(&mut self, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if self.phase != Phase::Files {
            return Err(StoreError::Refused(cannot_stand(rec::FILE)));
        }
        // a path longer than its `u16` length field could not be read back
        if !is_project_file(path) || path.len() > usize::from(u16::MAX) {
            return Err(StoreError::Refused(not_carried(path)));
        }
        if self.last_path.as_deref().is_some_and(|last| path <= last) {
            return Err(StoreError::Refused(cannot_stand(rec::FILE)));
        }
        if self.totals.files >= MAX_FILES {
            return Err(StoreError::Refused("too many project files".into()));
        }
        if bytes.len() > MAX_FILE_BYTES {
            return Err(StoreError::Refused(too_large(path)));
        }
        let mut p = Vec::with_capacity(2 + path.len() + bytes.len());
        p.extend_from_slice(&(path.len() as u16).to_le_bytes());
        p.extend_from_slice(path.as_bytes());
        p.extend_from_slice(bytes);
        self.record(rec::FILE, &p)?;
        self.last_path = Some(path.to_string());
        self.totals.files += 1;
        Ok(())
    }

    /// Writes one sealed batch: its `BATCH` record, then its frames byte for byte. Refuses a
    /// frame of a kind that cannot stand in a batch (a batch marker or an unassigned kind among
    /// them), a frame whose hash is not its payload's, and a batch whose digest is not its
    /// frames'.
    pub fn batch(&mut self, b: &DumpBatch) -> Result<(), StoreError> {
        let n = self.totals.batches + 1;
        // every check before the first byte, so a refused batch writes nothing; no store writes
        // a batch of no frames
        if b.frames.is_empty() {
            return Err(StoreError::Refused(cannot_stand(rec::BATCH)));
        }
        for f in &b.frames {
            if !self.kinds.contains(&f.kind) {
                return Err(StoreError::Refused(cannot_stand(f.kind)));
            }
            if Hash::of(&f.payload) != f.hash {
                return Err(StoreError::Refused(
                    "a record's hash does not match its bytes".into(),
                ));
            }
        }
        if batch_digest(b.frames.iter().map(|f| &f.hash)) != b.digest {
            return Err(StoreError::Refused(format!(
                "batch {n} does not match its digest"
            )));
        }
        let count = u32::try_from(b.frames.len()).map_err(|_| {
            StoreError::Refused(format!(
                "batch {n} holds {} frames, and its record says {}",
                b.frames.len(),
                u32::MAX
            ))
        })?;
        let mut p = Vec::with_capacity(BATCH_LEN);
        p.extend_from_slice(&b.start.segment.to_le_bytes());
        p.extend_from_slice(&b.start.offset.to_le_bytes());
        p.extend_from_slice(&count.to_le_bytes());
        p.extend_from_slice(&b.digest.0);
        p.extend_from_slice(&kinds_digest(&b.frames).0);
        self.record(rec::BATCH, &p)?;
        for f in &b.frames {
            // the hash was checked above: each payload is hashed once
            self.write_record(f.kind, f.hash, &f.payload)?;
        }
        self.phase = Phase::Batches;
        self.totals.batches += 1;
        self.totals.frames += b.frames.len() as u64;
        Ok(())
    }

    /// Writes the trailer and returns the writer and what the dump holds.
    pub fn finish(mut self) -> Result<(W, DumpTotals), StoreError> {
        let mut p = Vec::with_capacity(TRAILER_LEN);
        p.extend_from_slice(&self.totals.batches.to_le_bytes());
        p.extend_from_slice(&self.totals.frames.to_le_bytes());
        p.extend_from_slice(&self.totals.files.to_le_bytes());
        p.extend_from_slice(&Hash::of(&self.chain).0);
        self.record(rec::TRAILER, &p)?;
        self.w.flush()?;
        Ok((self.w, self.totals))
    }

    /// Writes one record with its hash.
    fn record(&mut self, kind: u8, payload: &[u8]) -> Result<(), StoreError> {
        self.write_record(kind, Hash::of(payload), payload)
    }

    /// Writes one record whose payload hashes to `hash`, and adds it to the running digest (the
    /// trailer's own is not).
    fn write_record(&mut self, kind: u8, hash: Hash, payload: &[u8]) -> Result<(), StoreError> {
        let len =
            u32::try_from(payload.len()).map_err(|_| StoreError::Refused(cannot_stand(kind)))?;
        self.w.write_all(&len.to_le_bytes())?;
        self.w.write_all(&[kind])?;
        self.w.write_all(&hash.0)?;
        self.w.write_all(payload)?;
        if kind != rec::TRAILER {
            link(&mut self.chain, kind, payload.len() as u64, &hash);
        }
        self.totals.bytes += FRAME_HEADER + payload.len() as u64;
        Ok(())
    }
}

/// Reads a dump and checks every record on the way (verify on receive). `next` returns `None`
/// only after a verified trailer and the end of the file. After an error the reader stays
/// failed: every later `next` returns that error again.
pub struct DumpReader<R: Read> {
    r: R,
    phase: ReadPhase,
    /// The error that ended the read, if one did.
    failed: Option<StoreError>,
    last_path: Option<String>,
    /// The store kinds a frame may have (`frame_kinds` of the dump's pair).
    kinds: &'static [u8],
    /// `kind || len || hash` of every record read so far, the trailer's excepted.
    chain: Vec<u8>,
    totals: DumpTotals,
}

/// Where the reader is in the closed order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadPhase {
    Files,
    Batches,
    Done,
}

/// One record as read: where it started, its kind, its hash (checked) and its payload.
struct Record {
    at: u64,
    kind: u8,
    hash: Hash,
    payload: Vec<u8>,
}

/// A copy of an error of the reader, to return again on every later `next`.
fn again(e: &StoreError) -> StoreError {
    match e {
        StoreError::Dump { at, what } => damaged(*at, what.clone()),
        StoreError::Io(io) => StoreError::Io(io::Error::new(io.kind(), io.to_string())),
        other => StoreError::Decode(other.to_string()),
    }
}

fn damaged(at: u64, what: impl Into<String>) -> StoreError {
    StoreError::Dump {
        at,
        what: what.into(),
    }
}

const ENDS_EARLY: &str = "the file ends before its trailer";

/// What a refusal of the reader (`StoreError::Dump`) means for the person: the caller picks the
/// fix by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpRefusal {
    /// The file is not a dump: it does not start with the magic.
    NotADump,
    /// A dump format, or a store format in a dump, that this rdy does not read.
    FormatNotRead,
    /// Damage at a byte.
    Damaged,
}

impl DumpRefusal {
    /// The kind of the reader's refusal `what`. It reads the texts this module writes
    /// (`NOT_A_DUMP`, `dump_format_not_read`, `store_format_not_read`), so a text and its kind
    /// change together.
    pub fn of(what: &str) -> DumpRefusal {
        if what == NOT_A_DUMP {
            DumpRefusal::NotADump
        } else if what.ends_with(NOT_READ)
            && (what.starts_with(DUMP_FORMAT_WORDS) || what.starts_with(STORE_FORMAT_WORDS))
        {
            DumpRefusal::FormatNotRead
        } else {
            DumpRefusal::Damaged
        }
    }
}

/// The refusal of a file that does not start with `MAGIC`.
const NOT_A_DUMP: &str = "it does not start with RDYDUMP";
/// The end of a refusal of a format this rdy does not read.
const NOT_READ: &str = " is not one this rdy reads";
const DUMP_FORMAT_WORDS: &str = "dump format ";
const STORE_FORMAT_WORDS: &str = "store format ";

/// The refusal of dump format `format`.
fn dump_format_not_read(format: u32) -> String {
    format!("{DUMP_FORMAT_WORDS}{format}{NOT_READ}")
}

/// The refusal of store format `store_format` in a dump of format `format`.
fn store_format_not_read(store_format: u32, format: u32) -> String {
    format!("{STORE_FORMAT_WORDS}{store_format} in a dump of format {format}{NOT_READ}")
}

impl<R: Read> DumpReader<R> {
    /// Reads the prelude and the header. Refuses a file that does not start with the magic, and
    /// a `(dump_format, store_format)` pair outside `READS`. Returns the reader, the dump format
    /// and the header.
    pub fn open(r: R) -> Result<(Self, u32, DumpHeader), StoreError> {
        let mut me = DumpReader {
            r,
            phase: ReadPhase::Files,
            failed: None,
            last_path: None,
            kinds: &[],
            chain: Vec::new(),
            totals: DumpTotals::default(),
        };
        let mut magic = Vec::with_capacity(MAGIC.len());
        (&mut me.r)
            .take(MAGIC.len() as u64)
            .read_to_end(&mut magic)?;
        if magic != MAGIC {
            return Err(damaged(0, NOT_A_DUMP));
        }
        me.totals.bytes = MAGIC.len() as u64;
        let mut format = [0u8; 4];
        me.exact(&mut format, me.totals.bytes)?;
        me.totals.bytes += 4;
        let format = u32::from_le_bytes(format);
        if !READS.iter().any(|(d, _)| *d == format) {
            return Err(damaged(MAGIC.len() as u64, dump_format_not_read(format)));
        }
        let first = me.record()?;
        if first.kind != rec::HEADER {
            return Err(damaged(first.at, cannot_stand(first.kind)));
        }
        let header = parse_header(&first.payload)
            .ok_or_else(|| damaged(first.at, "the header does not parse"))?;
        let Some(kinds) = READS
            .contains(&(format, header.store_format))
            .then(|| frame_kinds(format, header.store_format))
            .flatten()
        else {
            return Err(damaged(
                first.at,
                store_format_not_read(header.store_format, format),
            ));
        };
        me.kinds = kinds;
        me.link(&first);
        Ok((me, format, header))
    }

    /// The next project file or batch; `None` after a verified trailer at the end of the file.
    /// After an error, every later call returns that error again: never an item or `None`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<DumpItem>, StoreError> {
        if let Some(e) = &self.failed {
            return Err(again(e));
        }
        let out = self.step();
        if let Err(e) = &out {
            self.failed = Some(again(e));
        }
        out
    }

    /// One step of `next`.
    fn step(&mut self) -> Result<Option<DumpItem>, StoreError> {
        if self.phase == ReadPhase::Done {
            return Ok(None);
        }
        let record = self.record()?;
        match record.kind {
            rec::FILE if self.phase == ReadPhase::Files => {
                let item = self.file(&record)?;
                self.link(&record);
                Ok(Some(item))
            }
            rec::BATCH => {
                self.phase = ReadPhase::Batches;
                self.link(&record);
                self.batch(&record).map(|b| Some(DumpItem::Batch(b)))
            }
            rec::TRAILER => {
                self.trailer(&record)?;
                Ok(None)
            }
            k => Err(damaged(record.at, cannot_stand(k))),
        }
    }

    /// What the reader has read so far.
    pub fn totals(&self) -> DumpTotals {
        self.totals
    }

    /// The byte offset of the next record.
    pub fn offset(&self) -> u64 {
        self.totals.bytes
    }

    fn file(&mut self, record: &Record) -> Result<DumpItem, StoreError> {
        let mut c = Cursor(&record.payload);
        let Some(path_len) = c.u16() else {
            return Err(damaged(record.at, not_carried("")));
        };
        let Some(raw) = c.take(path_len as usize) else {
            return Err(damaged(
                record.at,
                not_carried(&String::from_utf8_lossy(c.0)),
            ));
        };
        let Ok(path) = std::str::from_utf8(raw) else {
            return Err(damaged(
                record.at,
                not_carried(&String::from_utf8_lossy(raw)),
            ));
        };
        if !is_project_file(path) {
            return Err(damaged(record.at, not_carried(path)));
        }
        if self.last_path.as_deref().is_some_and(|last| path <= last) {
            return Err(damaged(record.at, cannot_stand(rec::FILE)));
        }
        if self.totals.files >= MAX_FILES {
            return Err(damaged(record.at, "too many project files"));
        }
        if c.0.len() > MAX_FILE_BYTES {
            return Err(damaged(record.at, too_large(path)));
        }
        self.last_path = Some(path.to_string());
        self.totals.files += 1;
        Ok(DumpItem::File {
            path: path.to_string(),
            bytes: c.0.to_vec(),
        })
    }

    fn batch(&mut self, record: &Record) -> Result<DumpBatch, StoreError> {
        let n = self.totals.batches + 1;
        let mut c = Cursor(&record.payload);
        let parsed = (|| {
            let segment = c.u32()?;
            let offset = c.u64()?;
            let frames = c.u32()?;
            let digest = c.hash()?;
            let kinds = c.hash()?;
            c.0.is_empty()
                .then_some((Location { segment, offset }, frames, digest, kinds))
        })();
        // no store writes a batch of no frames
        let Some((start, count, digest, kinds)) = parsed.filter(|p| p.1 > 0) else {
            return Err(damaged(record.at, cannot_stand(record.kind)));
        };
        // no buffer is sized from an unread count: frames are pushed as they are read
        let mut frames = Vec::new();
        while frames.len() < count as usize {
            let f = self.record()?;
            if f.kind >= rec::HEADER {
                return Err(damaged(
                    f.at,
                    format!(
                        "batch {n} holds {} frames, and its record says {count}",
                        frames.len()
                    ),
                ));
            }
            if !self.kinds.contains(&f.kind) {
                return Err(damaged(f.at, cannot_stand(f.kind)));
            }
            self.link(&f);
            frames.push(Frame {
                kind: f.kind,
                hash: f.hash,
                payload: f.payload,
            });
        }
        if batch_digest(frames.iter().map(|f| &f.hash)) != digest || kinds_digest(&frames) != kinds
        {
            return Err(damaged(
                record.at,
                format!("batch {n} does not match its digest"),
            ));
        }
        self.totals.batches += 1;
        self.totals.frames += frames.len() as u64;
        Ok(DumpBatch {
            start,
            digest,
            frames,
        })
    }

    fn trailer(&mut self, record: &Record) -> Result<(), StoreError> {
        let mismatch = || {
            damaged(
                record.at,
                "the trailer's counts or digest do not match the dump",
            )
        };
        let mut c = Cursor(&record.payload);
        let batches = c.u64().ok_or_else(mismatch)?;
        let frames = c.u64().ok_or_else(mismatch)?;
        let files = c.u32().ok_or_else(mismatch)?;
        let digest = c.hash().ok_or_else(mismatch)?;
        if !c.0.is_empty()
            || batches != self.totals.batches
            || frames != self.totals.frames
            || files != self.totals.files
            || digest != Hash::of(&self.chain)
        {
            return Err(mismatch());
        }
        let mut extra = [0u8; 1];
        let at = self.totals.bytes;
        loop {
            match self.r.read(&mut extra) {
                Ok(0) => break,
                Ok(_) => return Err(damaged(at, "bytes follow the trailer")),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.phase = ReadPhase::Done;
        Ok(())
    }

    /// Adds a read record to the bytes the trailer's digest covers.
    fn link(&mut self, r: &Record) {
        link(&mut self.chain, r.kind, r.payload.len() as u64, &r.hash);
    }

    /// Reads one record and checks its hash. The end of the file inside a record, or where a
    /// record should start, is a dump that ends before its trailer.
    fn record(&mut self) -> Result<Record, StoreError> {
        let at = self.totals.bytes;
        let mut head = [0u8; FRAME_HEADER as usize];
        self.exact(&mut head, at)?;
        let len = u32::from_le_bytes(head[0..4].try_into().unwrap()) as u64;
        let kind = head[4];
        let hash = Hash(head[5..37].try_into().unwrap());
        let mut payload = Vec::new();
        (&mut self.r).take(len).read_to_end(&mut payload)?;
        if (payload.len() as u64) < len {
            return Err(damaged(at, ENDS_EARLY));
        }
        if Hash::of(&payload) != hash {
            return Err(damaged(at, "a record's hash does not match its bytes"));
        }
        self.totals.bytes = at + FRAME_HEADER + len;
        Ok(Record {
            at,
            kind,
            hash,
            payload,
        })
    }

    /// Fills `buf`, or fails with a dump that ends before its trailer (blamed on `at`).
    fn exact(&mut self, buf: &mut [u8], at: u64) -> Result<(), StoreError> {
        match self.r.read_exact(buf) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(damaged(at, ENDS_EARLY)),
            Err(e) => Err(e.into()),
        }
    }
}

impl StoreReader {
    /// Calls `f` with every sealed batch of `[ORIGIN, upto)`, in log order: its marker's start
    /// and digest, and its data frames byte for byte. Each marker is checked against the frames
    /// it closes (start, count, digest); a marker that closes no frames does not match. A
    /// mismatch, or frames after the last marker, is `StoreError::Decode`.
    pub fn for_each_batch(
        &self,
        upto: Location,
        mut f: impl FnMut(DumpBatch) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let origin = Location {
            segment: 0,
            offset: 0,
        };
        let mut pending: Vec<Frame> = Vec::new();
        let mut first: Option<Location> = None;
        self.for_each_frame_from(origin, upto, |loc, kind, hash, payload, _next| {
            if kind != kinds::BATCH_COMMIT {
                first.get_or_insert(loc);
                pending.push(Frame {
                    kind,
                    hash,
                    payload: payload.to_vec(),
                });
                return Ok(());
            }
            let start = first.take().unwrap_or(loc);
            let mismatch = || {
                StoreError::Decode(format!(
                    "batch at segment {} offset {} does not match its marker; run rdy fsck",
                    start.segment, start.offset
                ))
            };
            let marker = BatchCommit::decode(payload).ok_or_else(mismatch)?;
            let frames = std::mem::take(&mut pending);
            // a marker of no frames closes no batch (a store's open treats one as torn)
            if frames.is_empty()
                || marker.start != start
                || marker.frames as usize != frames.len()
                || batch_digest(frames.iter().map(|f| &f.hash)) != marker.digest
            {
                return Err(mismatch());
            }
            f(DumpBatch {
                start: marker.start,
                digest: marker.digest,
                frames,
            })
        })?;
        if !pending.is_empty() {
            return Err(StoreError::Decode(format!(
                "the log does not end at a batch marker at segment {} offset {}; run rdy fsck",
                upto.segment, upto.offset
            )));
        }
        Ok(())
    }
}
