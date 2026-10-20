//! The pre-pass of a chain load (D-2FMRR: "Before it writes, the load reads the applies of every
//! file"; D-QF2HQ: a load may stop before those applies; D-69AY3). It reads a full dump and its
//! increments, in argument order, and decides what the load writes, before any target exists.
//!
//! # What it checks, in order (O11)
//!
//! 1. [`ChainSource::open`]: every envelope (`crate::dump::read_envelope`), and every link in
//!    argument order: a full dump first, then increments; dump format 3 for a chain; within a
//!    link the kind, the dump format, the repo, the store format, the base (the end of the file
//!    before it), the base marker (that file's end marker) and `prev` (that file's trailer
//!    digest). A single file of dump format 1 or 2 opens (the released single-file load reads
//!    it), but it takes no stop.
//! 2. [`scan`]: one forward framing pass per file, in argument order, with the dump reader's own
//!    checks (`crate::dump`: framing, lengths, order, counts, positions, both batch digests, the
//!    trailer's counts and digest, the batch ends, the end of the file). Retained data payloads
//!    are not read: the pass seeks over them, so their hashes are checked by the ingest, which
//!    reads them once. The pass reads and hashes the removal records (kinds 20 to 23), every
//!    `DROPPED` record, the `REF_UPDATE` records it needs (the mode, the repo id, the store
//!    format), and the `TAG` records a tag stop needs; it keeps the `FILE` records' names,
//!    lengths, hashes and offsets.
//! 3. The removal proofs, in physical source order: each apply by the store's own validator
//!    (`crate::redaction_io::Validator` for a project; HA1's logical proof, `sync_proof::check`,
//!    for a holder), reading its records by the offsets the forward pass noted, each distinct
//!    one at most once per proof. Every `DROPPED` record must name the valid apply that first
//!    covers its span, or a carried record of the same file that names that exact span with
//!    `held: false`. Each file's trailer removal facts must be what its applies resolve, and its
//!    header's repo and store format what the log's meta refs say. Proof failures are reported
//!    after the framing of every file.
//! 4. The stop: an exact batch end (`pos:`, never `0.0`), the first batch that holds the one
//!    landing a prefix names, or the batch of the TAG record of a name.
//! 5. Each file wholly after the stop is read whole, through its end, by the dump reader: it adds
//!    no batch to the load, so nothing else would hash it.
//!
//! # What it decides ([`ScanPlan`])
//!
//! The selected end, the files that own the project files, and for every retained frame a
//! removal claims, what the ingest writes ([`ScanPlan::frame_action`]): the frame's own bytes or
//! hole, or its original header and zeros. A payload that an apply at or before the stop removed
//! is never written, even for a moment: the pre-pass marks it, and it holds no payload byte
//! itself. For each later apply (or later carried record) that touches retained spans, one
//! carried record is prepared, in physical provenance order, with the source's request, apply,
//! landing, approver, reason class and targets, and its spans' `held` flags from the retained
//! frames; the residues its spans need and the retained prefix lacks come with it, by hash.
//!
//! The pass keeps at most two input files open, one envelope row per file (at most
//! [`MAX_INPUTS`]) and no vector sized from a declared count. Its maps grow by fallible
//! reservation from the records it reads; a refused growth is `record allocation failed`.
use crate::dump::{
    self, BYTES_AFTER, DumpHeader, DumpReader, DumpTrailer, Frame, PRELUDE_LEN, RecordHead,
    check_file_head, check_frame_head, check_last, check_trailer, damaged, drop_text, header_of,
    link, parse_batch_record, parse_dropped, read_body, read_envelope, read_head, read_prelude,
    rec, try_batch_digest, try_kinds_digest,
};
use crate::dump_chain::{BatchMarker, DumpKind, chain_text, follows};
use crate::error::StoreError;
use crate::format::{REF_STORE_FORMAT, decode_u32};
use crate::holder::{FirstBatch, StoreMode};
use crate::kinds;
use crate::meta::{REF_REPO_ID, decode_repo_id};
use crate::object_refs::ObjectKey;
use crate::redaction::is_removable;
use crate::redaction_carried::{check_bounds, decode_record};
use crate::redaction_import::{PROOF_KINDS, ReceiverCaps, Scanned, copy_fits, proof_error};
use crate::redaction_io::{
    ApplyFrame, Collected, ERASE_BUFFER, RecordAccess, Validator, apply_record,
};
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::decode;
use crate::stream_error::StreamError;
use crate::sync_proof::{self, ObjectSource, RemovalObject};
use crate::sync_stream::StreamRecord;
use rdy_model::redaction::{
    CarriedSpan, FramePlace, MAX_RECORD_BYTES, Redaction, RedactionCarried, RedactionResidue,
};
use rdy_model::{Hash, LandingRecord, Provenance, RepoId, Tag};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

/// The most input files of one chain: a full dump and 4096 increments.
pub const MAX_INPUTS: usize = 4097;

/// The refusal of carried records whose provenance disagrees (a `StoreError::Dump` text).
pub const PROVENANCE_CONFLICT: &str = "carried removal provenance conflicts";
/// The refusal of a header whose repo or store format the log's meta refs contradict.
pub const METADATA_MISMATCH: &str = "dump metadata does not match its header";

/// The most bytes of a `REF_UPDATE` payload the forward pass reads: the meta refs it needs are a
/// few bytes, and a longer record is none of them.
const REF_READ_MAX: u64 = ERASE_BUFFER as u64;

/// Where a load stops, as the scanner matches it against the physical batches of a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopMatch {
    /// The landings whose lowercase hex hash starts with this text.
    LandingPrefix(String),
    /// The TAG records of exactly this name.
    TagName(String),
    /// The batch that ends exactly here.
    Position(Location),
}

/// Why a stop selects no batch end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopFailure {
    /// No landing of the chain matches the prefix.
    MissingLanding(String),
    /// No TAG record of the chain has the name.
    MissingTag(String),
    /// The prefix matches more than one distinct landing.
    AmbiguousLanding(String),
    /// No batch of the chain ends here (or the place is the origin).
    NotBatchEnd(Location),
    /// Two distinct TAG records have the name: the chain's tags are damaged.
    DuplicateTag(String),
}

/// Why a file does not follow the file before it in a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainLink {
    /// The first file is an increment.
    FirstIsIncrement,
    /// A later file is a full dump.
    LaterIsFull,
    /// A file of dump format 1 or 2, which holds no chain facts, in a chain of several files.
    Legacy { format: u32 },
    /// A single file of dump format 1 or 2 with a stop.
    LegacyStop { format: u32 },
    /// The file's repo is not the chain's.
    Repo { file: RepoId, chain: RepoId },
    /// The file's store format is not the chain's.
    StoreFormat { file: u32, chain: u32 },
    /// The file starts elsewhere than the end of the file before it.
    Position { start: Location, prev_end: Location },
    /// The file follows another batch than the last batch of the file before it.
    Marker,
    /// The file names another predecessor digest than the trailer digest of the file before it.
    Digest { named: Hash, actual: Hash },
}

/// A refusal of the chain pre-pass.
#[derive(Debug)]
pub enum ScanError {
    /// Input file `file` (by argument index) failed to open or read, or a record of it failed.
    Input { file: usize, source: StoreError },
    /// Input file `file` does not follow the file before it.
    Link { file: usize, link: ChainLink },
    /// The stop selects no batch end.
    Stop(StopFailure),
}

/// One input's envelope: its dump format, header, trailer and length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub format: u32,
    pub header: DumpHeader,
    pub trailer: DumpTrailer,
    pub len: u64,
}

/// Which pass of the scanner read bytes of an input (test hook).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPass {
    /// The forward framing pass.
    Forward,
    /// A proof lookup by a recorded offset.
    Lookup,
    /// The whole read of a file after the stop.
    Verify,
}

/// One read of input bytes (test hook): the input, the byte offset, the length and the pass.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanRead {
    pub file: usize,
    pub offset: u64,
    pub len: u64,
    pub pass: ScanPass,
}

/// A test hook the scanner calls with each read of input bytes.
#[doc(hidden)]
pub type ReadHook = Box<dyn FnMut(ScanRead)>;

thread_local! {
    static READ_HOOK: RefCell<Option<ReadHook>> = const { RefCell::new(None) };
}

/// Test hook: the scanner on this thread calls `hook` with every read of input bytes (a counting
/// reader). None removes the hook.
#[doc(hidden)]
pub fn on_scan_read(hook: Option<ReadHook>) {
    READ_HOOK.with(|h| *h.borrow_mut() = hook);
}

fn report(r: ScanRead) {
    READ_HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook(r);
        }
    });
}

/// An input file that reports each read to the test hook.
struct Input {
    f: File,
    file: usize,
    pos: u64,
    pass: ScanPass,
}

impl Read for Input {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.f.read(buf)?;
        if n > 0 {
            report(ScanRead {
                file: self.file,
                offset: self.pos,
                len: n as u64,
                pass: self.pass,
            });
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Input {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = self.f.seek(to)?;
        Ok(self.pos)
    }
}

fn input(paths: &[PathBuf], file: usize, pass: ScanPass) -> Result<Input, ScanError> {
    let f = File::open(&paths[file]).map_err(|e| ScanError::Input {
        file,
        source: StoreError::Io(e),
    })?;
    Ok(Input {
        f,
        file,
        pos: 0,
        pass,
    })
}

/// The inputs of a chain, their envelopes read and their links checked.
#[derive(Debug)]
pub struct ChainSource {
    paths: Vec<PathBuf>,
    envelopes: Vec<Envelope>,
}

impl ChainSource {
    /// Opens `files` in argument order: reads each envelope, then checks it against the file
    /// before it (module docs, step 1). Refuses no files and more than [`MAX_INPUTS`] before it
    /// opens any. One file is open at a time.
    pub fn open(files: &[PathBuf]) -> Result<ChainSource, ScanError> {
        if files.is_empty() || files.len() > MAX_INPUTS {
            return Err(ScanError::Input {
                file: files.len().min(MAX_INPUTS),
                source: StoreError::Refused(format!(
                    "a dump chain holds 1 to {MAX_INPUTS} files; {} given",
                    files.len()
                )),
            });
        }
        let mut envelopes: Vec<Envelope> = Vec::new();
        for i in 0..files.len() {
            let mut r = input(files, i, ScanPass::Forward)?;
            let (format, header, trailer, len) =
                read_envelope(&mut r).map_err(|source| ScanError::Input { file: i, source })?;
            let env = Envelope {
                format,
                header,
                trailer,
                len,
            };
            if let Some(link) =
                check_link(i, files.len(), &env, envelopes.last(), envelopes.first())
            {
                return Err(ScanError::Link { file: i, link });
            }
            envelopes.try_reserve(1).map_err(|_| ScanError::Input {
                file: i,
                source: damaged(0, drop_text::ALLOCATION),
            })?;
            envelopes.push(env);
        }
        Ok(ChainSource {
            paths: files.to_vec(),
            envelopes,
        })
    }

    /// Each input's envelope, in argument order.
    pub fn envelopes(&self) -> &[Envelope] {
        &self.envelopes
    }

    /// The inputs' paths, in argument order.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// The dump format of the one input, when it is a single file of dump format 1 or 2 (the
    /// released single-file load reads it).
    pub fn legacy(&self) -> Option<u32> {
        match &self.envelopes[..] {
            [e] if e.header.chain.is_none() => Some(e.format),
            _ => None,
        }
    }
}

/// The link check of input `i` of `n` (O11's order within a link): its kind, its dump format,
/// then against the chain (`first`) and the file before it (`prev`), its repo, store format,
/// base, base marker and `prev` digest.
fn check_link(
    i: usize,
    n: usize,
    env: &Envelope,
    prev: Option<&Envelope>,
    first: Option<&Envelope>,
) -> Option<ChainLink> {
    let Some(chain) = env.header.chain else {
        return (n > 1).then_some(ChainLink::Legacy { format: env.format });
    };
    match (i, chain.kind) {
        (0, DumpKind::Increment) => return Some(ChainLink::FirstIsIncrement),
        (1.., DumpKind::Full) => return Some(ChainLink::LaterIsFull),
        _ => {}
    }
    let (Some(prev), Some(first)) = (prev, first) else {
        return None;
    };
    // the first file's own legacy refusal came at its own index
    let (Some(head), Some(before)) = (first.header.chain, prev.header.chain) else {
        return Some(ChainLink::Legacy {
            format: prev.format,
        });
    };
    if chain.repo_id != head.repo_id {
        return Some(ChainLink::Repo {
            file: chain.repo_id,
            chain: head.repo_id,
        });
    }
    if env.header.store_format != first.header.store_format {
        return Some(ChainLink::StoreFormat {
            file: env.header.store_format,
            chain: first.header.store_format,
        });
    }
    if chain.base != prev.header.log_end {
        return Some(ChainLink::Position {
            start: chain.base,
            prev_end: prev.header.log_end,
        });
    }
    if chain.base_batch != before.end_batch {
        return Some(ChainLink::Marker);
    }
    if chain.prev != prev.trailer.digest {
        return Some(ChainLink::Digest {
            named: chain.prev,
            actual: prev.trailer.digest,
        });
    }
    None
}

/// What the ingest does with one retained source frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAction {
    /// The source frame as it is: its payload, or its validated hole.
    Keep,
    /// The frame's original header, then zeros of its original length: `authority` is the
    /// retained apply, or the prepared carried record, that accounts for the zeros, and
    /// `residue` the residue it names (none for a blob chunk).
    Zero {
        authority: Hash,
        residue: Option<Hash>,
    },
}

/// The part of a chain one input plays for the selected end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRole {
    /// Every batch of the file is retained, and a later file holds the end.
    Retained,
    /// The file holds the selected end (or ends there): its batches up to it are retained, and
    /// its project files are the load's.
    Stop,
    /// The file lies wholly after the end: it adds no batch.
    After,
}

/// One `FILE` record of an input: its path, the length and hash of its record's payload, and
/// the record's byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFile {
    pub path: String,
    pub len: u64,
    pub hash: Hash,
    pub offset: u64,
}

/// What the scan found in one input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFacts {
    pub role: FileRole,
    /// Its `FILE` records, in order.
    pub files: Vec<ProjectFile>,
    /// Its batches, and how many of them the load retains.
    pub batches: u64,
    pub kept_batches: u64,
}

/// One frame a removal plan names, with the identity the ingest must find there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Planned {
    kind: u8,
    hash: Hash,
    len: u32,
    action: FrameAction,
}

/// The verified plan of a chain load: what the pre-pass checked and decided, with no payload byte
/// in it (module docs).
#[derive(Clone, Debug)]
pub struct ScanPlan {
    envelopes: Vec<Envelope>,
    facts: Vec<FileFacts>,
    mode: StoreMode,
    end: Location,
    explicit: bool,
    landing: Option<Hash>,
    tag: Option<String>,
    residues: Vec<RedactionResidue>,
    carried: Vec<RedactionCarried>,
    planned: HashMap<(u32, u64), Planned>,
}

impl ScanPlan {
    /// Each input's envelope, in argument order.
    pub fn envelopes(&self) -> &[Envelope] {
        &self.envelopes
    }

    /// What the scan found in each input, in argument order.
    pub fn files(&self) -> &[FileFacts] {
        &self.facts
    }

    /// The mode the chain's first batch sets.
    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    /// The selected end: the stop's batch end, or the chain's end without a stop.
    pub fn end(&self) -> Location {
        self.end
    }

    /// Whether a stop selected the end (a stop at the chain's own end included).
    pub fn explicit_stop(&self) -> bool {
        self.explicit
    }

    /// The landing a `landing:` or `tag:` stop selected.
    pub fn landing(&self) -> Option<Hash> {
        self.landing
    }

    /// The tag name a `tag:` stop selected.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// The input that owns the load's project files: the last one that contributes a retained
    /// batch (the stop's file).
    pub fn owner(&self) -> usize {
        self.facts
            .iter()
            .position(|f| f.role == FileRole::Stop)
            .unwrap_or(self.facts.len().saturating_sub(1))
    }

    /// The residues the prepared carried records need and the retained prefix lacks, by hash.
    pub fn residues(&self) -> &[RedactionResidue] {
        &self.residues
    }

    /// The prepared carried records, in physical provenance order.
    pub fn carried(&self) -> &[RedactionCarried] {
        &self.carried
    }

    /// The retained places the plan names, in log order: each must be a frame the ingest meets.
    pub fn planned_places(&self) -> Vec<Location> {
        let mut v: Vec<Location> = self
            .planned
            .keys()
            .map(|(segment, offset)| Location {
                segment: *segment,
                offset: *offset,
            })
            .collect();
        v.sort();
        v
    }

    /// What the ingest writes for the retained source frame at `place` whose original kind,
    /// hash and length are `kind`, `hash` and `len`. A frame the plan names with another
    /// identity is `dropped frame does not match its authorized span`.
    pub fn frame_action(
        &self,
        place: Location,
        kind: u8,
        hash: Hash,
        len: u32,
    ) -> Result<FrameAction, StoreError> {
        match self.planned.get(&(place.segment, place.offset)) {
            None => Ok(FrameAction::Keep),
            Some(p) if (p.kind, p.hash, p.len) == (kind, hash, len) => Ok(p.action),
            Some(_) => Err(damaged(0, drop_text::SPAN)),
        }
    }
}

/// The physical place `l` as a map key (`Location` is not hashed).
fn key(l: Location) -> (u32, u64) {
    (l.segment, l.offset)
}

fn place_of(p: FramePlace) -> Location {
    Location {
        segment: p.segment,
        offset: p.offset,
    }
}

/// One record whose payload a proof may read again: where it is in the inputs, and its header.
#[derive(Clone, Copy, Debug)]
struct Rec {
    file: usize,
    offset: u64,
    len: u32,
    kind: u8,
    hash: Hash,
}

/// One `DROPPED` record of the chain: where it is, its descriptor, and whether a validated
/// authorization of its exact span released it.
#[derive(Clone, Copy, Debug)]
struct DropRec {
    file: usize,
    offset: u64,
    kind: u8,
    hash: Hash,
    len: u32,
    apply: Hash,
    residue: Option<Hash>,
    released: bool,
}

/// One validated apply: where it is, its record, its batch's end and what it resolves.
struct ApplyFact {
    file: usize,
    at: Location,
    offset: u64,
    hash: Hash,
    /// The request and landing it names.
    named: (Hash, Hash),
    batch_end: Location,
    request: Option<Rec>,
    landing: Option<Rec>,
    /// A project's spans: its request's manifest places.
    spans: Vec<Location>,
    /// A holder's approved tuples.
    objects: BTreeMap<ObjectKey, RemovalObject>,
}

/// One carried record of the chain.
struct CarriedFact {
    file: usize,
    at: Location,
    offset: u64,
    hash: Hash,
    rec: Rec,
    batch_end: Location,
    apply: Hash,
}

/// One frame of a removable kind in a holder's chain.
#[derive(Clone, Copy, Debug)]
struct KeyCopy {
    at: Location,
    rec: Rec,
    dropped: bool,
}

/// A holder's logical proof state (HA1): the first frame of each proof key, every copy of a
/// removable key, and the approved tuples by their earliest apply.
#[derive(Default)]
struct Holder {
    first: HashMap<ObjectKey, (Rec, bool)>,
    copies: HashMap<ObjectKey, Vec<KeyCopy>>,
    approved: HashMap<ObjectKey, (RemovalObject, Hash)>,
    residues: HashMap<Hash, RedactionResidue>,
    scanned: Scanned,
    over: Option<StoreError>,
    spans: u64,
}

/// The input reads of proof lookups: at most one file open at a time.
struct Lookups<'a> {
    paths: &'a [PathBuf],
    open: Option<Input>,
}

impl Lookups<'_> {
    /// Reads and hash-checks the record `r` by its offset.
    fn read(&mut self, r: &Rec) -> Result<Vec<u8>, StoreError> {
        if self.open.as_ref().is_none_or(|i| i.file != r.file) {
            self.open = None;
            let f = File::open(&self.paths[r.file])?;
            self.open = Some(Input {
                f,
                file: r.file,
                pos: 0,
                pass: ScanPass::Lookup,
            });
        }
        let i = self.open.as_mut().expect("an open input");
        i.seek(SeekFrom::Start(r.offset))?;
        let mut head = [0u8; FRAME_HEADER as usize];
        dump::read_exact_at(i, &mut head, r.offset)?;
        let len = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes"));
        if len != r.len || head[4] != r.kind || head[5..37] != r.hash.0 {
            return Err(damaged(r.offset, dump::HASH_MISMATCH));
        }
        read_body(
            i,
            &RecordHead {
                at: r.offset,
                len: u64::from(len),
                kind: r.kind,
                hash: r.hash,
            },
        )
    }
}

/// The chain's records as the apply validator's source (`RecordAccess`): records by the places
/// the forward pass noted, read again by their offsets. A span's header that the pass did not
/// keep (a retained data frame) is taken on trust here: the plan names that frame with the
/// span's identity, the ingest checks it when it meets the frame (`ScanPlan::frame_action`), and
/// the loaded store's own bootstrap checks the whole log.
struct ChainAccess<'a, 'p> {
    lookups: &'a mut Lookups<'p>,
    recs: &'a HashMap<(u32, u64), Rec>,
    batch: &'a [(Location, Rec)],
    drops: &'a HashMap<(u32, u64), DropRec>,
}

impl ChainAccess<'_, '_> {
    fn find(&self, loc: Location) -> Option<&Rec> {
        self.batch
            .iter()
            .find(|(l, _)| *l == loc)
            .map(|(_, r)| r)
            .or_else(|| self.recs.get(&key(loc)))
    }
}

impl RecordAccess for ChainAccess<'_, '_> {
    fn has_frame(
        &mut self,
        loc: Location,
        len: u32,
        kind: u8,
        hash: &Hash,
    ) -> Result<bool, StoreError> {
        if let Some(d) = self.drops.get(&key(loc)) {
            return Ok((d.len, d.kind, d.hash) == (len, kind, *hash));
        }
        Ok(match self.find(loc) {
            Some(r) => (r.len, r.kind, r.hash) == (len, kind, *hash),
            None => true,
        })
    }

    fn record(
        &mut self,
        loc: Location,
        kind: u8,
        hash: &Hash,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(r) = self.find(loc).copied() else {
            return Ok(None);
        };
        if r.kind != kind || r.hash != *hash {
            return Ok(None);
        }
        self.lookups.read(&r).map(Some)
    }
}

/// A holder's chain as a proof source (`sync_proof::ObjectSource`): the first frame of each
/// proof key, served when it is present, as HA1's own log source serves it; each record read at
/// most once per proof.
struct ChainObjects<'a, 'p> {
    lookups: &'a mut Lookups<'p>,
    first: &'a HashMap<ObjectKey, (Rec, bool)>,
    cache: HashMap<ObjectKey, Option<StreamRecord>>,
}

impl ObjectSource for ChainObjects<'_, '_> {
    fn record(&mut self, k: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        if let Some(r) = self.cache.get(&k) {
            return Ok(r.clone());
        }
        let got = match self.first.get(&k) {
            Some((r, false)) if r.len as usize <= MAX_RECORD_BYTES => match self.lookups.read(r) {
                Ok(payload) => Some(StreamRecord::Present { key: k, payload }),
                Err(StoreError::Io(e)) => return Err(StreamError::io(None, 0, &e)),
                Err(_) => None,
            },
            _ => None,
        };
        self.cache
            .try_reserve(1)
            .map_err(|_| StreamError::allocation(None, 0))?;
        self.cache.insert(k, got.clone());
        Ok(got)
    }
}

/// One proof event of a batch, handled in physical order once the batch's framing is checked.
enum Event {
    Drop {
        at: Location,
        offset: u64,
        frame: Frame,
    },
    Apply {
        at: Location,
        offset: u64,
        hash: Hash,
        payload: Vec<u8>,
        landings: Vec<(Location, Hash)>,
    },
    Carried {
        at: Location,
        offset: u64,
        hash: Hash,
        rec: Rec,
        payload: Vec<u8>,
    },
    Copy {
        at: Location,
        rec: Rec,
    },
}

/// The zero, one or many candidate state of a stop.
#[derive(Clone, Debug, Default)]
enum Candidate {
    #[default]
    None,
    /// The one candidate so far: its object hash, the landing it names (a tag's), and once its
    /// first batch ended, that batch's end, input and index in it.
    One {
        hash: Hash,
        landing: Hash,
        at: Option<(Location, usize, u64)>,
    },
    Many,
}

impl Candidate {
    /// Takes a match of object `hash` (naming `landing`) in the current batch.
    fn take(&mut self, hash: Hash, landing: Hash) {
        match self {
            Candidate::None => {
                *self = Candidate::One {
                    hash,
                    landing,
                    at: None,
                }
            }
            Candidate::One { hash: h, .. } if *h != hash => *self = Candidate::Many,
            _ => {}
        }
    }

    /// Ends the current batch at `end` (input `file`, its batch `index`).
    fn batch_end(&mut self, end: Location, file: usize, index: u64) {
        if let Candidate::One { at, .. } = self
            && at.is_none()
        {
            *at = Some((end, file, index));
        }
    }
}

/// The forward pass's state of one input.
struct FileScan {
    file: usize,
    format: u32,
    len: u64,
    at: u64,
    chain: Vec<u8>,
    files: Vec<ProjectFile>,
    last_path: Option<String>,
    batches: u64,
    frames: u64,
    applies: u64,
    last: Option<(Location, BatchMarker)>,
    /// The validated applies of this input (indexes into the scan's applies).
    validated: Vec<usize>,
}

/// The state of a scan (module docs).
struct Scan<'p> {
    paths: &'p [PathBuf],
    envs: &'p [Envelope],
    stop: Option<&'p StopMatch>,
    lookups: Lookups<'p>,
    mode: Option<StoreMode>,
    first_batch: FirstBatch,
    batch_no: u64,
    /// The stop's candidates.
    pos: Option<(usize, u64)>,
    landing: Candidate,
    tag: Candidate,
    /// The meta refs the log set so far: the repo id and the store format.
    repo_ref: Option<Vec<u8>>,
    format_ref: Option<Vec<u8>>,
    /// Removal proofs.
    collected: Collected,
    recs: HashMap<(u32, u64), Rec>,
    batch_landings: Vec<(Location, Rec)>,
    validator: Validator,
    applies: Vec<ApplyFact>,
    carried: Vec<CarriedFact>,
    drops: HashMap<(u32, u64), DropRec>,
    unreleased: u64,
    holder: Holder,
    /// The first proof failure: reported after the framing of every input (O11).
    proof_error: Option<ScanError>,
    /// The input of a failed proof's record, when it is not the input of the batch being
    /// checked (a dropped frame or a copy of an earlier file).
    blame: Option<usize>,
    facts: Vec<FileFacts>,
}

fn alloc(file: usize, at: u64) -> ScanError {
    ScanError::Input {
        file,
        source: damaged(at, drop_text::ALLOCATION),
    }
}

/// Pushes `v` onto `out` by fallible growth.
fn push<T>(out: &mut Vec<T>, v: T, file: usize, at: u64) -> Result<(), ScanError> {
    out.try_reserve(1).map_err(|_| alloc(file, at))?;
    out.push(v);
    Ok(())
}

/// Scans `src` with `stop` (module docs, steps 2 to 5) and returns the verified plan. No target
/// exists while it runs, and it writes nothing.
pub fn scan(src: &mut ChainSource, stop: Option<StopMatch>) -> Result<ScanPlan, ScanError> {
    if let Some(format) = src.legacy() {
        return Err(ScanError::Link {
            file: 0,
            link: if stop.is_some() {
                ChainLink::LegacyStop { format }
            } else {
                ChainLink::Legacy { format }
            },
        });
    }
    let mut s = Scan {
        paths: &src.paths,
        envs: &src.envelopes,
        stop: stop.as_ref(),
        lookups: Lookups {
            paths: &src.paths,
            open: None,
        },
        mode: None,
        first_batch: FirstBatch::default(),
        batch_no: 0,
        pos: None,
        landing: Candidate::None,
        tag: Candidate::None,
        repo_ref: None,
        format_ref: None,
        collected: Collected::default(),
        recs: HashMap::new(),
        batch_landings: Vec::new(),
        validator: Validator::default(),
        applies: Vec::new(),
        carried: Vec::new(),
        drops: HashMap::new(),
        unreleased: 0,
        holder: Holder::default(),
        proof_error: None,
        blame: None,
        facts: Vec::new(),
    };
    for i in 0..s.envs.len() {
        s.forward(i)?;
    }
    if let Some(e) = s.proof_error.take() {
        return Err(e);
    }
    s.unreleased_drop()?;
    if s.holder_has_applies()
        && let Some(e) = s.holder.over.take()
    {
        return Err(ScanError::Input {
            file: s.applies[0].file,
            source: e,
        });
    }
    let (end, stop_file, stop_index) = s.resolve()?;
    // the files wholly after the end are read whole now: no batch of theirs reaches the ingest
    for i in stop_file + 1..s.envs.len() {
        verify_whole(s.paths, i)?;
    }
    for (i, f) in s.facts.iter_mut().enumerate() {
        f.role = if i < stop_file {
            FileRole::Retained
        } else if i == stop_file {
            FileRole::Stop
        } else {
            FileRole::After
        };
        f.kept_batches = match f.role {
            FileRole::Retained => f.batches,
            FileRole::Stop => stop_index,
            FileRole::After => 0,
        };
    }
    let (planned, carried, residues) = s.plan(end)?;
    let (landing, tag) = match (&stop, &s.landing, &s.tag) {
        (Some(StopMatch::LandingPrefix(_)), Candidate::One { hash, .. }, _) => (Some(*hash), None),
        (Some(StopMatch::TagName(n)), _, Candidate::One { landing, .. }) => {
            (Some(*landing), Some(n.clone()))
        }
        _ => (None, None),
    };
    Ok(ScanPlan {
        envelopes: src.envelopes.clone(),
        facts: s.facts,
        mode: s.mode.unwrap_or(StoreMode::Project),
        end,
        explicit: stop.is_some(),
        landing,
        tag,
        residues,
        carried,
        planned,
    })
}

/// Reads input `i` whole with the dump reader: every record's hash, every check, through its end.
fn verify_whole(paths: &[PathBuf], i: usize) -> Result<(), ScanError> {
    let r = input(paths, i, ScanPass::Verify)?;
    let fail = |source| ScanError::Input { file: i, source };
    let (mut rd, _, _) = DumpReader::open(BufReader::new(r)).map_err(fail)?;
    while rd.next().map_err(fail)?.is_some() {}
    Ok(())
}

impl<'p> Scan<'p> {
    fn fail(&self, file: usize, source: StoreError) -> ScanError {
        ScanError::Input { file, source }
    }

    /// Records the first proof failure; later proof work stops.
    fn proof_failed(&mut self, file: usize, source: StoreError) {
        if self.proof_error.is_none() {
            self.proof_error = Some(ScanError::Input { file, source });
        }
    }

    fn proving(&self) -> bool {
        self.proof_error.is_none()
    }

    /// The forward framing pass of input `i`.
    fn forward(&mut self, i: usize) -> Result<(), ScanError> {
        let envs: &'p [Envelope] = self.envs;
        let env = &envs[i];
        let links = env.header.chain.expect("a format-3 chain file");
        let mut r =
            BufReader::with_capacity(ERASE_BUFFER, input(self.paths, i, ScanPass::Forward)?);
        let format = read_prelude(&mut r).map_err(|e| self.fail(i, e))?;
        let mut f = FileScan {
            file: i,
            format,
            len: env.len,
            at: PRELUDE_LEN,
            chain: Vec::new(),
            files: Vec::new(),
            last_path: None,
            batches: 0,
            frames: 0,
            applies: 0,
            last: None,
            validated: Vec::new(),
        };
        // the header, checked against the envelope the links were checked on
        let head = read_head(&mut r, f.at, format, false).map_err(|e| self.fail(i, e))?;
        if head.kind != rec::HEADER {
            return Err(self.fail(i, damaged(head.at, dump::cannot_stand(head.kind))));
        }
        let payload = read_body(&mut r, &head).map_err(|e| self.fail(i, e))?;
        let (header, _) = header_of(format, head.at, &payload).map_err(|e| self.fail(i, e))?;
        if header != env.header {
            return Err(self.fail(i, damaged(head.at, dump::TRAILER_MISMATCH)));
        }
        self.link(&mut f, &head)?;
        f.at = head.end();
        loop {
            let head = read_head(&mut r, f.at, format, false).map_err(|e| self.fail(i, e))?;
            match head.kind {
                rec::FILE if f.batches == 0 => self.file_record(&mut r, &mut f, &head)?,
                rec::BATCH => self.batch(&mut r, &mut f, &head, &links)?,
                rec::TRAILER => {
                    self.trailer(&mut r, &mut f, &head, &links)?;
                    break;
                }
                k => return Err(self.fail(i, damaged(head.at, dump::cannot_stand(k)))),
            }
        }
        self.end_of_file(&f, &links);
        let facts = FileFacts {
            role: FileRole::After,
            files: std::mem::take(&mut f.files),
            batches: f.batches,
            kept_batches: 0,
        };
        push(&mut self.facts, facts, i, f.at)?;
        Ok(())
    }

    fn link(&self, f: &mut FileScan, head: &RecordHead) -> Result<(), ScanError> {
        link(&mut f.chain, head.kind, head.len, &head.hash).ok_or_else(|| alloc(f.file, head.at))
    }

    /// Skips the payload of `head`, which must lie inside the file.
    fn skip(
        &self,
        r: &mut BufReader<Input>,
        f: &FileScan,
        head: &RecordHead,
    ) -> Result<(), ScanError> {
        if head.end() > f.len {
            return Err(self.fail(f.file, damaged(head.at, dump::ENDS_EARLY)));
        }
        let n = i64::try_from(head.len).map_err(|_| alloc(f.file, head.at))?;
        r.seek_relative(n)
            .map_err(|e| self.fail(f.file, StoreError::Io(e)))
    }

    /// One `FILE` record: its path is read and checked, its bytes are skipped.
    fn file_record(
        &mut self,
        r: &mut BufReader<Input>,
        f: &mut FileScan,
        head: &RecordHead,
    ) -> Result<(), ScanError> {
        if head.end() > f.len {
            return Err(self.fail(f.file, damaged(head.at, dump::ENDS_EARLY)));
        }
        // the length field, then the path: at most 2 + 65535 bytes
        let mut lenb = [0u8; 2];
        let want = head.len.min(2) as usize;
        dump::read_exact_at(r, &mut lenb[..want], head.at).map_err(|e| self.fail(f.file, e))?;
        let path_len = if want == 2 {
            u64::from(u16::from_le_bytes(lenb))
        } else {
            0
        };
        let take = path_len.min(head.len - want as u64);
        let mut prefix = Vec::new();
        prefix
            .try_reserve_exact(want + take as usize)
            .map_err(|_| alloc(f.file, head.at))?;
        prefix.extend_from_slice(&lenb[..want]);
        prefix.resize(want + take as usize, 0);
        dump::read_exact_at(r, &mut prefix[want..], head.at).map_err(|e| self.fail(f.file, e))?;
        let path = check_file_head(
            head.at,
            &prefix,
            head.len,
            f.last_path.as_deref(),
            f.files.len() as u32,
        )
        .map_err(|e| self.fail(f.file, e))?
        .to_string();
        let rest = head.len - want as u64 - take;
        let n = i64::try_from(rest).map_err(|_| alloc(f.file, head.at))?;
        r.seek_relative(n)
            .map_err(|e| self.fail(f.file, StoreError::Io(e)))?;
        self.link(f, head)?;
        f.last_path = Some(path.clone());
        push(
            &mut f.files,
            ProjectFile {
                path,
                len: head.len,
                hash: head.hash,
                offset: head.at,
            },
            f.file,
            head.at,
        )?;
        f.at = head.end();
        Ok(())
    }

    /// One batch: its record and its frames, by their heads; the payloads the proofs, the meta
    /// refs and the stop need are read and hashed; the rest are skipped.
    fn batch(
        &mut self,
        r: &mut BufReader<Input>,
        f: &mut FileScan,
        head: &RecordHead,
        links: &crate::dump_chain::DumpChainHeader,
    ) -> Result<(), ScanError> {
        let i = f.file;
        let n = f.batches + 1;
        let payload = read_body(r, head).map_err(|e| self.fail(i, e))?;
        self.link(f, head)?;
        let Some((start, count, digest, kinds_digest)) = parse_batch_record(&payload) else {
            return Err(self.fail(i, damaged(head.at, dump::cannot_stand(head.kind))));
        };
        let allowed = dump::frame_kinds(f.format, self.envs[i].header.store_format).unwrap_or(&[]);
        let first_of_log = self.mode.is_none();
        self.batch_landings.clear();
        let mut pairs: Vec<(u8, Hash)> = Vec::new();
        let mut events: Vec<Event> = Vec::new();
        let mut landings: Vec<(Location, Hash)> = Vec::new();
        let mut place = start.offset;
        let mut overflow = false;
        let mut matched_landing = false;
        let mut matched_tag = false;
        f.at = head.end();
        while pairs.len() < count as usize {
            let fh = read_head(r, f.at, f.format, true).map_err(|e| self.fail(i, e))?;
            check_frame_head(n, pairs.len(), count, &fh).map_err(|e| self.fail(i, e))?;
            self.link(f, &fh)?;
            let at = Location {
                segment: start.segment,
                offset: place,
            };
            let (kind, hash, len) = if fh.kind == rec::DROPPED {
                let p = read_body(r, &fh).map_err(|e| self.fail(i, e))?;
                let frame = parse_dropped(&p).map_err(|what| self.fail(i, damaged(fh.at, what)))?;
                let id = (frame.kind(), *frame.hash(), frame.original_len());
                if self.mode == Some(StoreMode::Holder) || first_of_log {
                    let r = Rec {
                        file: i,
                        offset: fh.at,
                        len: id.2,
                        kind: id.0,
                        hash: id.1,
                    };
                    self.holder_count(r, true)?;
                }
                push(
                    &mut events,
                    Event::Drop {
                        at,
                        offset: fh.at,
                        frame,
                    },
                    i,
                    fh.at,
                )?;
                id
            } else {
                let len = fh.len as u32;
                let rec_ = Rec {
                    file: i,
                    offset: fh.at,
                    len,
                    kind: fh.kind,
                    hash: fh.hash,
                };
                self.present(
                    r,
                    f,
                    &fh,
                    at,
                    rec_,
                    &mut events,
                    &mut landings,
                    first_of_log,
                )
                .map(|(l, t)| {
                    matched_landing |= l;
                    matched_tag |= t;
                })?;
                (fh.kind, fh.hash, len)
            };
            if !allowed.contains(&kind) {
                return Err(self.fail(i, damaged(fh.at, dump::cannot_stand(kind))));
            }
            push(&mut pairs, (kind, hash), i, fh.at)?;
            match place
                .checked_add(FRAME_HEADER)
                .and_then(|p| p.checked_add(u64::from(len)))
            {
                Some(p) => place = p,
                None => overflow = true,
            }
            f.at = fh.end();
        }
        if overflow {
            return Err(self.fail(i, damaged(head.at, chain_text::OVERFLOW)));
        }
        let refused = || alloc(i, head.at);
        let frame_digest = try_batch_digest(pairs.iter().map(|(_, h)| h)).ok_or_else(refused)?;
        let kinds = try_kinds_digest(pairs.iter().map(|(k, h)| (*k, h))).ok_or_else(refused)?;
        if frame_digest != digest || kinds != kinds_digest {
            return Err(self.fail(
                i,
                damaged(head.at, format!("batch {n} does not match its digest")),
            ));
        }
        // the batch's physical end and marker, and the base it follows
        let marker = BatchMarker {
            start,
            frames: count,
            digest,
        };
        if f.last.is_none() && !follows(links.base, start) {
            return Err(self.fail(i, damaged(head.at, chain_text::FIRST_BATCH)));
        }
        let end = place
            .checked_add(FRAME_HEADER)
            .and_then(|p| p.checked_add(marker.encoded_len()?))
            .map(|offset| Location {
                segment: start.segment,
                offset,
            })
            .ok_or_else(|| self.fail(i, damaged(head.at, chain_text::OVERFLOW)))?;
        f.last = Some((end, marker));
        f.batches += 1;
        f.frames += u64::from(count);
        self.batch_no += 1;
        if first_of_log {
            let creation = self.first_batch.creation(end);
            self.mode = Some(creation.mode);
        }
        // the stop's candidates end with the batch
        if let Some(StopMatch::Position(p)) = self.stop
            && *p == end
            && self.pos.is_none()
        {
            self.pos = Some((i, f.batches));
        }
        if matched_landing {
            self.landing.batch_end(end, i, f.batches);
        }
        if matched_tag {
            self.tag.batch_end(end, i, f.batches);
        }
        for e in events {
            if !self.proving() {
                break;
            }
            if let Err(source) = self.event(f, e, end) {
                let file = self.blame.take().unwrap_or(i);
                self.proof_failed(file, source);
            }
        }
        Ok(())
    }

    /// One present data frame of a batch: its payload is read when the proofs, the meta refs or
    /// the stop need it, and skipped otherwise. Returns whether it matched a landing or a tag
    /// stop.
    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        r: &mut BufReader<Input>,
        f: &mut FileScan,
        fh: &RecordHead,
        at: Location,
        rec_: Rec,
        events: &mut Vec<Event>,
        landings: &mut Vec<(Location, Hash)>,
        first_of_log: bool,
    ) -> Result<(bool, bool), ScanError> {
        let i = f.file;
        let next = Location {
            segment: at.segment,
            offset: at.offset + FRAME_HEADER + fh.len,
        };
        let holder = self.mode == Some(StoreMode::Holder);
        if holder || first_of_log {
            self.holder_count(rec_, false)?;
        }
        let mut matched = (false, false);
        match fh.kind {
            kinds::REDACTION
            | kinds::REDACTION_RESIDUE
            | kinds::REDACTION_APPLY
            | kinds::REDACTION_CARRIED => {
                if fh.len > MAX_RECORD_BYTES as u64 {
                    // a removal record over the record bound is no record: a proof that needs
                    // it fails, and a carried one is refused as the reader refuses it
                    if fh.kind == kinds::REDACTION_CARRIED {
                        let p = read_body(r, fh).map_err(|e| self.fail(i, e))?;
                        dump::check_carried(fh.kind, &p)
                            .map_err(|what| self.fail(i, damaged(fh.at, what)))?;
                    }
                    self.skip(r, f, fh)?;
                    return Ok(matched);
                }
                let p = read_body(r, fh).map_err(|e| self.fail(i, e))?;
                dump::check_carried(fh.kind, &p)
                    .map_err(|what| self.fail(i, damaged(fh.at, what)))?;
                match fh.kind {
                    kinds::REDACTION => {
                        self.collected.note_request(at, fh.hash);
                        self.note(at, rec_, i, fh.at)?;
                    }
                    kinds::REDACTION_RESIDUE => {
                        self.collected.note_residue(at, fh.hash);
                        self.note(at, rec_, i, fh.at)?;
                    }
                    kinds::REDACTION_APPLY => {
                        f.applies += 1;
                        push(
                            events,
                            Event::Apply {
                                at,
                                offset: fh.at,
                                hash: fh.hash,
                                payload: p,
                                landings: landings.clone(),
                            },
                            i,
                            fh.at,
                        )?;
                    }
                    _ => push(
                        events,
                        Event::Carried {
                            at,
                            offset: fh.at,
                            hash: fh.hash,
                            rec: rec_,
                            payload: p,
                        },
                        i,
                        fh.at,
                    )?,
                }
            }
            kinds::REF_UPDATE if fh.len <= REF_READ_MAX => {
                let p = read_body(r, fh).map_err(|e| self.fail(i, e))?;
                if first_of_log {
                    self.first_batch
                        .frame(fh.kind, Some(&p), next)
                        .map_err(|e| self.fail(i, e))?;
                }
                if let Some(u) = crate::holder::decode_ref(&p) {
                    if u.name == REF_REPO_ID {
                        self.repo_ref = Some(u.value);
                    } else if u.name == REF_STORE_FORMAT {
                        self.format_ref = Some(u.value);
                    }
                }
            }
            kinds::TAG if matches!(self.stop, Some(StopMatch::TagName(_))) => {
                let p = read_body(r, fh).map_err(|e| self.fail(i, e))?;
                if let (Some(StopMatch::TagName(name)), Ok(t)) = (self.stop, decode::<Tag>(&p))
                    && t.name == *name
                {
                    self.tag.take(fh.hash, t.landing);
                    matched.1 = true;
                }
            }
            _ => {
                if fh.kind == kinds::LANDING {
                    push(landings, (at, fh.hash), i, fh.at)?;
                    push(&mut self.batch_landings, (at, rec_), i, fh.at)?;
                    if let Some(StopMatch::LandingPrefix(prefix)) = self.stop
                        && fh.hash.to_string().starts_with(prefix.as_str())
                    {
                        self.landing.take(fh.hash, fh.hash);
                        matched.0 = true;
                    }
                }
                if (holder || first_of_log) && is_removable(fh.kind) {
                    push(events, Event::Copy { at, rec: rec_ }, i, fh.at)?;
                }
                self.skip(r, f, fh)?;
            }
        }
        Ok(matched)
    }

    /// Notes a record a proof may read again.
    fn note(&mut self, at: Location, r: Rec, file: usize, off: u64) -> Result<(), ScanError> {
        if self.recs.contains_key(&key(at)) {
            return Ok(());
        }
        self.recs.try_reserve(1).map_err(|_| alloc(file, off))?;
        self.recs.insert(key(at), r);
        Ok(())
    }

    /// A holder's scan counts (HA1's caps) and its first frame of each proof key (a dropped
    /// one is never served, as HA1's own log source serves none).
    fn holder_count(&mut self, r: Rec, dropped: bool) -> Result<(), ScanError> {
        let caps = ReceiverCaps::current();
        if self.holder.over.is_none()
            && let Err(e) = self.holder.scanned.add(r.len, &caps)
        {
            self.holder.over = Some(e);
        }
        let k = ObjectKey {
            kind: r.kind,
            hash: r.hash,
        };
        if self.holder.over.is_none()
            && PROOF_KINDS.contains(&k.kind)
            && !self.holder.first.contains_key(&k)
        {
            self.holder
                .first
                .try_reserve(1)
                .map_err(|_| alloc(r.file, r.offset))?;
            self.holder.first.insert(k, (r, dropped));
        }
        Ok(())
    }

    /// Whether the chain holds a holder's apply.
    fn holder_has_applies(&self) -> bool {
        self.mode == Some(StoreMode::Holder) && !self.applies.is_empty()
    }

    /// One proof event, in physical order, of a batch that ends at `end`.
    fn event(&mut self, f: &mut FileScan, e: Event, end: Location) -> Result<(), StoreError> {
        match e {
            Event::Drop { at, offset, frame } => self.on_drop(f.file, at, offset, frame),
            Event::Apply {
                at,
                offset,
                hash,
                payload,
                landings,
            } => {
                let idx = match self.mode {
                    Some(StoreMode::Holder) => self.holder_apply(f.file, at, offset, hash, end)?,
                    _ => self.native_apply(f.file, at, offset, hash, &payload, landings, end)?,
                };
                f.validated.push(idx);
                Ok(())
            }
            Event::Carried {
                at,
                offset,
                hash,
                rec,
                payload,
            } => self.on_carried(f.file, at, offset, hash, rec, &payload, end),
            Event::Copy { at, rec } => self.on_copy(at, rec, false),
        }
    }

    /// A `DROPPED` record: kept until a validated authorization of its exact span releases it,
    /// within the ingest's bounds (at most 1,000,000 records, at most 1 GiB not yet released).
    fn on_drop(
        &mut self,
        file: usize,
        at: Location,
        offset: u64,
        frame: Frame,
    ) -> Result<(), StoreError> {
        let Frame::Dropped {
            kind,
            hash,
            len,
            apply,
            residue,
        } = frame
        else {
            return Ok(());
        };
        if self.drops.len() >= crate::ingest::MAX_PENDING_DROPS
            || self.drops.try_reserve(1).is_err()
        {
            return Err(damaged(offset, drop_text::ALLOCATION));
        }
        let unreleased = self
            .unreleased
            .checked_add(u64::from(len))
            .filter(|u| *u <= crate::ingest::MAX_UNAUTHORIZED_BYTES)
            .ok_or_else(|| damaged(offset, drop_text::BUDGET))?;
        self.unreleased = unreleased;
        self.drops.insert(
            key(at),
            DropRec {
                file,
                offset,
                kind,
                hash,
                len,
                apply,
                residue,
                released: false,
            },
        );
        if self.mode == Some(StoreMode::Holder) {
            let rec_ = Rec {
                file,
                offset,
                len,
                kind,
                hash,
            };
            self.on_copy(at, rec_, true)?;
        }
        Ok(())
    }

    /// Releases the drop at `at` for the span of `authority` with `identity`, or refuses it.
    fn release(
        &mut self,
        at: Location,
        authority: Hash,
        identity: (u8, Hash, u32, Option<Hash>),
        file: Option<usize>,
    ) -> Result<(), StoreError> {
        let Some(d) = self.drops.get_mut(&key(at)) else {
            return Ok(());
        };
        if d.released {
            return Ok(());
        }
        self.blame = Some(d.file);
        if file.is_some_and(|f| f != d.file) || d.apply != authority {
            return Err(damaged(d.offset, drop_text::NO_AUTHORIZATION));
        }
        if (d.kind, d.hash, d.len) != (identity.0, identity.1, identity.2) {
            return Err(damaged(d.offset, drop_text::SPAN));
        }
        if d.residue != identity.3 {
            return Err(damaged(d.offset, drop_text::INVALID_RESIDUE));
        }
        d.released = true;
        self.blame = None;
        self.unreleased -= u64::from(d.len);
        Ok(())
    }

    /// The refusal of an apply's or carried record's failed proof at `offset`: a corrupt span or
    /// record is a dump refusal (at a dropped frame's place, its span does not match; elsewhere,
    /// no valid authorization); limits and i/o pass through.
    fn proof_refusal(&self, e: StoreError, offset: u64) -> StoreError {
        match e {
            StoreError::RedactionCorrupt { segment, offset: o } => {
                let what = if self.drops.contains_key(&(segment, o)) {
                    drop_text::SPAN
                } else {
                    drop_text::NO_AUTHORIZATION
                };
                damaged(offset, what)
            }
            StoreError::RedactionHuman | StoreError::Decode(_) => {
                damaged(offset, drop_text::NO_AUTHORIZATION)
            }
            e => e,
        }
    }

    /// A project's apply: the store's validator, over the chain's records.
    #[allow(clippy::too_many_arguments)]
    fn native_apply(
        &mut self,
        file: usize,
        at: Location,
        offset: u64,
        hash: Hash,
        payload: &[u8],
        landings: Vec<(Location, Hash)>,
        end: Location,
    ) -> Result<usize, StoreError> {
        let record = apply_record(payload);
        let frame = ApplyFrame::new(at, hash, record.clone(), self.batch_no, landings);
        let checked = {
            let mut access = ChainAccess {
                lookups: &mut self.lookups,
                recs: &self.recs,
                batch: &self.batch_landings,
                drops: &self.drops,
            };
            self.validator
                .apply(&self.collected, &mut access, &frame, None)
        };
        checked.map_err(|e| self.proof_refusal(e, offset))?;
        let record = record.expect("a validated apply decodes");
        let spans = self.validator.take_last_spans();
        // the drops this apply is the first to cover
        for loc in &spans {
            let auth = self.validator.auth.spans[loc];
            if auth.apply != hash {
                continue;
            }
            let hole = self.validator.auth.holes[loc];
            self.release(
                *loc,
                hash,
                (hole.kind, hole.hash, hole.len, auth.residue),
                None,
            )?;
        }
        let request = self
            .collected
            .request_place(&record.request)
            .and_then(|l| self.recs.get(&key(l)).copied());
        let landing = self
            .batch_landings
            .iter()
            .find(|(_, r)| r.hash == record.landing)
            .map(|(_, r)| *r);
        self.applies.push(ApplyFact {
            file,
            at,
            offset,
            hash,
            named: (record.request, record.landing),
            batch_end: end,
            request,
            landing,
            spans,
            objects: BTreeMap::new(),
        });
        Ok(self.applies.len() - 1)
    }

    /// A holder's apply: HA1's logical proof over the chain's first frames, then its tuples
    /// against every copy of their keys (HA1's rule 2).
    fn holder_apply(
        &mut self,
        file: usize,
        at: Location,
        offset: u64,
        hash: Hash,
        end: Location,
    ) -> Result<usize, StoreError> {
        if let Some(e) = self.holder.over.take() {
            return Err(e);
        }
        let (checked, residues) = {
            let mut objects = ChainObjects {
                lookups: &mut self.lookups,
                first: &self.holder.first,
                cache: HashMap::new(),
            };
            let checked = sync_proof::check(&mut objects, hash);
            // the residues its tuples name, read once for this proof (from its cache)
            let mut residues: Vec<(Hash, Option<RedactionResidue>)> = Vec::new();
            if let Ok(removal) = &checked {
                for t in removal.objects.values() {
                    let Some(r) = t.residue else { continue };
                    if self.holder.residues.contains_key(&r)
                        || residues.iter().any(|(h, _)| *h == r)
                    {
                        continue;
                    }
                    let k = ObjectKey {
                        kind: kinds::REDACTION_RESIDUE,
                        hash: r,
                    };
                    let res = match objects.record(k) {
                        Ok(Some(StreamRecord::Present { payload, .. })) => {
                            crate::bounded::decode::<RedactionResidue>(&payload).ok()
                        }
                        _ => None,
                    };
                    residues
                        .try_reserve(1)
                        .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
                    residues.push((r, res));
                }
            }
            (checked, residues)
        };
        let removal = checked
            .map_err(|e| proof_error(e, at))
            .map_err(|e| self.proof_refusal(e, offset))?;
        for (r, res) in residues {
            let res = res.ok_or_else(|| damaged(offset, drop_text::NO_AUTHORIZATION))?;
            self.holder
                .residues
                .try_reserve(1)
                .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
            self.holder.residues.insert(r, res);
        }
        let first = |kind: u8, hash: Hash| {
            self.holder
                .first
                .get(&ObjectKey { kind, hash })
                .map(|(r, _)| *r)
        };
        let landing = first(kinds::LANDING, removal.landing);
        let request = first(kinds::REDACTION, removal.request);
        // one tuple per key across every approval (HA1's derive)
        for (k, t) in &removal.objects {
            if let Some((first, _)) = self.holder.approved.get(k)
                && first != t
            {
                return Err(damaged(offset, drop_text::NO_AUTHORIZATION));
            }
        }
        for (k, t) in &removal.objects {
            if self.holder.approved.contains_key(k) {
                continue;
            }
            self.holder
                .approved
                .try_reserve(1)
                .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
            self.holder.approved.insert(*k, (*t, hash));
            let copies = self.holder.copies.get(k).cloned().unwrap_or_default();
            for c in copies {
                self.check_copy(*k, &c)?;
            }
        }
        self.applies.push(ApplyFact {
            file,
            at,
            offset,
            hash,
            named: (removal.request, removal.landing),
            batch_end: end,
            request,
            landing,
            spans: Vec::new(),
            objects: removal.objects,
        });
        Ok(self.applies.len() - 1)
    }

    /// A frame of a removable kind in a holder's chain: kept as a copy of its key, and checked
    /// at once when an apply already approves the key.
    fn on_copy(&mut self, at: Location, r: Rec, dropped: bool) -> Result<(), StoreError> {
        if self.mode != Some(StoreMode::Holder) || self.holder.over.is_some() {
            return Ok(());
        }
        let k = ObjectKey {
            kind: r.kind,
            hash: r.hash,
        };
        let c = KeyCopy {
            at,
            rec: r,
            dropped,
        };
        let list = match self.holder.copies.get_mut(&k) {
            Some(l) => l,
            None => {
                self.holder
                    .copies
                    .try_reserve(1)
                    .map_err(|_| damaged(r.offset, drop_text::ALLOCATION))?;
                self.holder.copies.entry(k).or_default()
            }
        };
        list.try_reserve(1)
            .map_err(|_| damaged(r.offset, drop_text::ALLOCATION))?;
        list.push(c);
        if self.holder.approved.contains_key(&k) {
            self.check_copy(k, &c)?;
        }
        Ok(())
    }

    /// Checks one copy of an approved key against its tuple (HA1's rule 2), and releases it when
    /// it is a dropped frame its earliest approving apply names.
    fn check_copy(&mut self, k: ObjectKey, c: &KeyCopy) -> Result<(), StoreError> {
        let (tuple, authority) = self.holder.approved[&k];
        self.holder.spans += 1;
        if self.holder.spans > ReceiverCaps::current().spans {
            return Err(StoreError::RedactionLimit {
                limit: crate::redaction::limit::FRAMES,
            });
        }
        if c.dropped {
            return self.release(
                c.at,
                authority,
                (k.kind, k.hash, tuple.len, tuple.residue),
                None,
            );
        }
        let intact = if tuple.residue.is_some() && c.rec.len as usize <= MAX_RECORD_BYTES {
            self.lookups.read(&c.rec).ok()
        } else {
            None
        };
        if !copy_fits(
            k.kind,
            c.rec.len,
            intact.as_deref(),
            &tuple,
            &self.holder.residues,
        ) {
            self.blame = Some(c.rec.file);
            return Err(damaged(c.rec.offset, drop_text::NO_AUTHORIZATION));
        }
        Ok(())
    }

    /// A carried record of the chain: it must decode in its bounded canonical form; it releases
    /// the dropped frames of its own input that it names with `held: false`, with their exact
    /// identity.
    #[allow(clippy::too_many_arguments)]
    fn on_carried(
        &mut self,
        file: usize,
        at: Location,
        offset: u64,
        hash: Hash,
        rec_: Rec,
        payload: &[u8],
        end: Location,
    ) -> Result<(), StoreError> {
        let record =
            decode_record(payload).ok_or_else(|| damaged(offset, drop_text::NO_AUTHORIZATION))?;
        for s in record.spans.iter().filter(|s| !s.held) {
            self.release(
                place_of(s.place),
                hash,
                (s.kind, s.hash, s.len, s.residue),
                Some(file),
            )?;
        }
        self.carried
            .try_reserve(1)
            .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
        self.carried.push(CarriedFact {
            file,
            at,
            offset,
            hash,
            rec: rec_,
            batch_end: end,
            apply: record.apply,
        });
        Ok(())
    }

    /// The checks of an input's trailer: its counts and digest, its last batch, its removal
    /// count, and the end of the file just past it.
    fn trailer(
        &mut self,
        r: &mut BufReader<Input>,
        f: &mut FileScan,
        head: &RecordHead,
        links: &crate::dump_chain::DumpChainHeader,
    ) -> Result<(), ScanError> {
        let i = f.file;
        let p = read_body(r, head).map_err(|e| self.fail(i, e))?;
        let t = check_trailer(
            head.at,
            f.format,
            &p,
            (f.batches, f.frames, f.files.len() as u32),
            Hash::of(&f.chain),
        )
        .map_err(|e| self.fail(i, e))?;
        check_last(head.at, links, f.last, self.envs[i].header.log_end)
            .map_err(|e| self.fail(i, e))?;
        if u64::from(t.removals) != f.applies {
            return Err(self.fail(i, damaged(head.at, chain_text::REMOVALS)));
        }
        if t != self.envs[i].trailer {
            return Err(self.fail(i, damaged(head.at, dump::TRAILER_MISMATCH)));
        }
        if head.end() != f.len {
            return Err(self.fail(i, damaged(head.end(), BYTES_AFTER)));
        }
        f.at = head.end();
        Ok(())
    }

    /// The proof checks at an input's end: its trailer's first removed span is the lowest
    /// resolved span of its validated applies, and its header's repo and store format are what
    /// the log's meta refs say.
    fn end_of_file(&mut self, f: &FileScan, links: &crate::dump_chain::DumpChainHeader) {
        let env = &self.envs[f.file];
        let repo_ok = self
            .repo_ref
            .as_deref()
            .and_then(decode_repo_id)
            .is_some_and(|id| id == links.repo_id);
        let format_ok = self
            .format_ref
            .as_deref()
            .is_none_or(|v| decode_u32(v) == Some(env.header.store_format));
        if !repo_ok || !format_ok {
            self.proof_failed(f.file, damaged(PRELUDE_LEN, METADATA_MISMATCH));
            return;
        }
        if !self.proving() {
            return;
        }
        let file_end = env.header.log_end;
        let mut first: Option<Location> = None;
        for idx in &f.validated {
            let a = &self.applies[*idx];
            let low = match self.mode {
                Some(StoreMode::Holder) => a
                    .objects
                    .keys()
                    .filter_map(|k| self.holder.copies.get(k))
                    .flatten()
                    .map(|c| c.at)
                    .filter(|l| *l < file_end)
                    .min(),
                _ => a.spans.iter().copied().min(),
            };
            if let Some(l) = low {
                first = Some(first.map_or(l, |x| x.min(l)));
            }
        }
        if first != env.trailer.first_removed {
            let at = env.len - (FRAME_HEADER + dump::TRAILER_LEN as u64);
            self.proof_failed(f.file, damaged(at, chain_text::REMOVALS));
        }
    }

    /// Refuses the first dropped frame (in physical order) that no validated authorization
    /// released.
    fn unreleased_drop(&self) -> Result<(), ScanError> {
        let first = self
            .drops
            .iter()
            .filter(|(_, d)| !d.released)
            .min_by_key(|(k, _)| **k);
        match first {
            Some((_, d)) => Err(ScanError::Input {
                file: d.file,
                source: damaged(d.offset, drop_text::NO_AUTHORIZATION),
            }),
            None => Ok(()),
        }
    }

    /// Resolves the stop: the selected end, the input that holds it, and how many of that
    /// input's batches it retains.
    fn resolve(&self) -> Result<(Location, usize, u64), ScanError> {
        let last = self.envs.len() - 1;
        let Some(stop) = self.stop else {
            return Ok((
                self.envs[last].header.log_end,
                last,
                self.facts[last].batches,
            ));
        };
        match stop {
            StopMatch::Position(p) => match self.pos {
                Some((file, index)) if *p != BatchMarker::ZERO.start => Ok((*p, file, index)),
                _ => Err(ScanError::Stop(StopFailure::NotBatchEnd(*p))),
            },
            StopMatch::LandingPrefix(prefix) => match &self.landing {
                Candidate::None => {
                    Err(ScanError::Stop(StopFailure::MissingLanding(prefix.clone())))
                }
                Candidate::Many => Err(ScanError::Stop(StopFailure::AmbiguousLanding(
                    prefix.clone(),
                ))),
                Candidate::One { at, .. } => {
                    let (end, file, index) = at.expect("a matched batch ended");
                    Ok((end, file, index))
                }
            },
            StopMatch::TagName(name) => match &self.tag {
                Candidate::None => Err(ScanError::Stop(StopFailure::MissingTag(name.clone()))),
                Candidate::Many => Err(ScanError::Stop(StopFailure::DuplicateTag(name.clone()))),
                Candidate::One { at, .. } => {
                    let (end, file, index) = at.expect("a matched batch ended");
                    Ok((end, file, index))
                }
            },
        }
    }

    /// Reads and decodes record `r`.
    fn read_typed<T: serde::de::DeserializeOwned>(&mut self, r: &Rec) -> Result<T, ScanError> {
        let p = self.lookups.read(r).map_err(|source| ScanError::Input {
            file: r.file,
            source,
        })?;
        decode::<T>(&p).map_err(|_| ScanError::Input {
            file: r.file,
            source: damaged(r.offset, drop_text::NO_AUTHORIZATION),
        })
    }

    /// The provenance of an apply: its request, apply, landing, approver, reason class and
    /// targets, read from its records.
    fn apply_provenance(&mut self, idx: usize) -> Result<SourceProvenance, ScanError> {
        let (file, offset, hash, named, request, landing) = {
            let a = &self.applies[idx];
            (a.file, a.offset, a.hash, a.named, a.request, a.landing)
        };
        let missing = || ScanError::Input {
            file,
            source: damaged(offset, drop_text::NO_AUTHORIZATION),
        };
        let request: Redaction = self.read_typed(&request.ok_or_else(missing)?)?;
        let landing: LandingRecord = self.read_typed(&landing.ok_or_else(missing)?)?;
        Ok(SourceProvenance {
            request: named.0,
            apply: hash,
            landing: named.1,
            approved_by: landing.approved_by,
            reason_class: request.reason_class,
            targets: request.targets,
        })
    }

    /// The carried record of the chain at `idx`, read again.
    fn carried_record(&mut self, idx: usize) -> Result<RedactionCarried, ScanError> {
        let (rec_, offset) = (self.carried[idx].rec, self.carried[idx].offset);
        let p = self
            .lookups
            .read(&rec_)
            .map_err(|source| ScanError::Input {
                file: rec_.file,
                source,
            })?;
        decode_record(&p).ok_or_else(|| ScanError::Input {
            file: rec_.file,
            source: damaged(offset, drop_text::NO_AUTHORIZATION),
        })
    }

    /// The plan of a load that ends at `end` (module docs): the retained frames' actions, the
    /// carried records of later removals and the residues they need.
    #[allow(clippy::type_complexity)]
    fn plan(
        &mut self,
        end: Location,
    ) -> Result<
        (
            HashMap<(u32, u64), Planned>,
            Vec<RedactionCarried>,
            Vec<RedactionResidue>,
        ),
        ScanError,
    > {
        let holder = self.mode == Some(StoreMode::Holder);
        let pre: HashSet<Hash> = self
            .applies
            .iter()
            .filter(|a| a.batch_end <= end)
            .map(|a| a.hash)
            .collect();
        // the retained carried records, by their source apply
        let mut kept: HashMap<Hash, usize> = HashMap::new();
        for (i, c) in self.carried.iter().enumerate() {
            if c.batch_end <= end {
                kept.entry(c.apply).or_insert(i);
            }
        }
        let kept_hashes: HashSet<Hash> = self
            .carried
            .iter()
            .filter(|c| c.batch_end <= end)
            .map(|c| c.hash)
            .collect();
        // the later removals that may touch the retained prefix, in physical order
        let mut sources: Vec<(Location, bool, usize)> = Vec::new();
        for (i, a) in self.applies.iter().enumerate() {
            if a.batch_end > end {
                sources.push((a.at, true, i));
            }
        }
        for (i, c) in self.carried.iter().enumerate() {
            if c.batch_end > end {
                sources.push((c.at, false, i));
            }
        }
        sources.sort_by_key(|(at, _, _)| *at);
        // identity -> (provenance, spans by place), in first physical order
        let mut order: Vec<Hash> = Vec::new();
        let mut records: HashMap<
            Hash,
            (
                SourceProvenance,
                BTreeMap<Location, CarriedSpan>,
                usize,
                u64,
            ),
        > = HashMap::new();
        for (_, is_apply, idx) in sources {
            let (file, offset, prov, spans) = if is_apply {
                let prov = self.apply_provenance(idx)?;
                let spans = self.apply_spans(idx, end);
                (
                    self.applies[idx].file,
                    self.applies[idx].offset,
                    prov,
                    spans,
                )
            } else {
                let r = self.carried_record(idx)?;
                let spans: Vec<CarriedSpan> = r
                    .spans
                    .iter()
                    .filter(|s| place_of(s.place) < end)
                    .map(|s| CarriedSpan {
                        held: !self.drops.contains_key(&key(place_of(s.place))),
                        ..s.clone()
                    })
                    .collect();
                let prov = SourceProvenance {
                    request: r.request,
                    apply: r.apply,
                    landing: r.landing,
                    approved_by: r.approved_by,
                    reason_class: r.reason_class,
                    targets: r.targets,
                };
                (
                    self.carried[idx].file,
                    self.carried[idx].offset,
                    prov,
                    spans,
                )
            };
            let conflict = || ScanError::Input {
                file,
                source: damaged(offset, PROVENANCE_CONFLICT),
            };
            let identity = prov.apply;
            // already retained: a carried record of this source apply, or the apply itself
            if let Some(k) = kept.get(&identity).copied() {
                let r = self.carried_record(k)?;
                if !prov.same_as_carried(&r) {
                    return Err(conflict());
                }
                continue;
            }
            if pre.contains(&identity) {
                let a = self
                    .applies
                    .iter()
                    .position(|a| a.hash == identity)
                    .expect("a retained apply");
                if self.apply_provenance(a)? != prov {
                    return Err(conflict());
                }
                continue;
            }
            match records.get_mut(&identity) {
                Some((p, have, _, _)) => {
                    if *p != prov {
                        return Err(conflict());
                    }
                    for s in spans {
                        let at = place_of(s.place);
                        match have.get(&at) {
                            Some(h) if *h != s => return Err(conflict()),
                            Some(_) => {}
                            None => {
                                have.insert(at, s);
                            }
                        }
                    }
                }
                None => {
                    let mut have = BTreeMap::new();
                    for s in spans {
                        let at = place_of(s.place);
                        if have.get(&at).is_some_and(|h| *h != s) {
                            return Err(conflict());
                        }
                        have.insert(at, s);
                    }
                    order.push(identity);
                    records.insert(identity, (prov, have, file, offset));
                }
            }
        }
        // the records, with their hashes, in physical provenance order
        let mut carried: Vec<RedactionCarried> = Vec::new();
        let mut hash_of: HashMap<Hash, Hash> = HashMap::new();
        for identity in order {
            let (prov, spans, file, _) = records.remove(&identity).expect("a record");
            if spans.is_empty() {
                continue;
            }
            let record = RedactionCarried {
                request: prov.request,
                apply: prov.apply,
                landing: prov.landing,
                approved_by: prov.approved_by,
                reason_class: prov.reason_class,
                targets: prov.targets,
                spans: spans.into_values().collect(),
            };
            let bytes =
                check_bounds(&record).map_err(|source| ScanError::Input { file, source })?;
            hash_of.insert(identity, Hash::of(&bytes));
            carried.push(record);
        }
        // the residues they need that the retained prefix lacks, by hash
        let mut residues: BTreeMap<Hash, RedactionResidue> = BTreeMap::new();
        for r in &carried {
            for s in &r.spans {
                let Some(h) = s.residue else { continue };
                if residues.contains_key(&h)
                    || self.collected.residue_place(&h).is_some_and(|l| l < end)
                {
                    continue;
                }
                let Some(at) = self.collected.residue_place(&h) else {
                    return Err(ScanError::Input {
                        file: 0,
                        source: StoreError::corrupt_at(place_of(s.place)),
                    });
                };
                let rec_ = self.recs[&key(at)];
                let res: RedactionResidue = self.read_typed(&rec_)?;
                residues.insert(h, res);
            }
        }
        // the actions of the retained frames
        let mut planned: HashMap<(u32, u64), Planned> = HashMap::new();
        let mut put = |at: Location, p: Planned| -> Result<(), ScanError> {
            planned.try_reserve(1).map_err(|_| alloc(0, 0))?;
            planned.entry(key(at)).or_insert(p);
            Ok(())
        };
        let authority_of = |x: Hash, carried_apply: Option<Hash>| -> Option<Hash> {
            let identity = carried_apply.unwrap_or(x);
            hash_of.get(&identity).copied().or_else(|| {
                kept.get(&identity)
                    .map(|k| self.carried[*k].hash)
                    .or_else(|| pre.contains(&identity).then_some(identity))
            })
        };
        // dropped frames: an existing hole of a retained proof, or zeros of a later one
        let mut drops: Vec<(&(u32, u64), &DropRec)> = self
            .drops
            .iter()
            .filter(|(k, _)| {
                Location {
                    segment: k.0,
                    offset: k.1,
                } < end
            })
            .collect();
        drops.sort_by_key(|(k, _)| **k);
        for (k, d) in drops {
            let at = Location {
                segment: k.0,
                offset: k.1,
            };
            let action = if pre.contains(&d.apply) || kept_hashes.contains(&d.apply) {
                FrameAction::Keep
            } else {
                let carried_apply = self
                    .carried
                    .iter()
                    .find(|c| c.hash == d.apply)
                    .map(|c| c.apply);
                match authority_of(d.apply, carried_apply) {
                    Some(authority) => FrameAction::Zero {
                        authority,
                        residue: d.residue,
                    },
                    None => FrameAction::Keep,
                }
            };
            put(
                at,
                Planned {
                    kind: d.kind,
                    hash: d.hash,
                    len: d.len,
                    action,
                },
            )?;
        }
        // present frames a removal claims: zeros when a retained apply first covers them, else
        // the frame as it is (a later removal: `held: true`)
        if holder {
            let mut copies: Vec<(Location, ObjectKey, KeyCopy)> = Vec::new();
            for (k, (_, _)) in self.holder.approved.iter() {
                for c in self.holder.copies.get(k).into_iter().flatten() {
                    if !c.dropped && c.at < end {
                        copies.push((c.at, *k, *c));
                    }
                }
            }
            copies.sort_by_key(|(at, _, _)| *at);
            for (at, k, c) in copies {
                let (tuple, authority) = self.holder.approved[&k];
                let action = if pre.contains(&authority) {
                    FrameAction::Zero {
                        authority,
                        residue: tuple.residue,
                    }
                } else {
                    FrameAction::Keep
                };
                put(
                    at,
                    Planned {
                        kind: k.kind,
                        hash: k.hash,
                        len: c.rec.len,
                        action,
                    },
                )?;
            }
        } else {
            let spans: Vec<(Location, crate::redaction_io::SpanAuth)> = self
                .validator
                .auth
                .spans
                .range(..end)
                .filter(|(l, _)| !self.drops.contains_key(&key(**l)))
                .map(|(l, s)| (*l, *s))
                .collect();
            for (at, s) in spans {
                let hole = self.validator.auth.holes[&at];
                let action = if pre.contains(&s.apply) {
                    FrameAction::Zero {
                        authority: s.apply,
                        residue: s.residue,
                    }
                } else {
                    FrameAction::Keep
                };
                put(
                    at,
                    Planned {
                        kind: hole.kind,
                        hash: hole.hash,
                        len: hole.len,
                        action,
                    },
                )?;
            }
        }
        Ok((planned, carried, residues.into_values().collect()))
    }

    /// The retained spans of the later apply `idx`, each `held` while its retained frame holds
    /// its bytes.
    fn apply_spans(&self, idx: usize, end: Location) -> Vec<CarriedSpan> {
        let a = &self.applies[idx];
        let held = |at: Location| !self.drops.contains_key(&key(at));
        let span =
            |at: Location, kind: u8, hash: Hash, len: u32, residue: Option<Hash>| CarriedSpan {
                place: FramePlace {
                    segment: at.segment,
                    offset: at.offset,
                },
                kind,
                hash,
                len,
                residue,
                held: held(at),
            };
        match self.mode {
            Some(StoreMode::Holder) => {
                let mut out: Vec<CarriedSpan> = Vec::new();
                for (k, t) in &a.objects {
                    for c in self.holder.copies.get(k).into_iter().flatten() {
                        if c.at < end {
                            out.push(span(c.at, k.kind, k.hash, c.rec.len, t.residue));
                        }
                    }
                }
                out
            }
            _ => a
                .spans
                .iter()
                .filter(|l| **l < end)
                .map(|l| {
                    let hole = self.validator.auth.holes[l];
                    let residue = self.validator.auth.spans[l].residue;
                    span(*l, hole.kind, hole.hash, hole.len, residue)
                })
                .collect(),
        }
    }
}

/// The provenance a carried record copies from its source removal.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceProvenance {
    request: Hash,
    apply: Hash,
    landing: Hash,
    approved_by: Provenance,
    reason_class: rdy_model::redaction::ReasonClass,
    targets: Vec<rdy_model::redaction::RedactTarget>,
}

impl SourceProvenance {
    fn same_as_carried(&self, r: &RedactionCarried) -> bool {
        (
            &self.request,
            &self.apply,
            &self.landing,
            &self.approved_by,
            &self.reason_class,
            &self.targets,
        ) == (
            &r.request,
            &r.apply,
            &r.landing,
            &r.approved_by,
            &r.reason_class,
            &r.targets,
        )
    }
}
