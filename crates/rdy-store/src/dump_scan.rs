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
//!    trailer's counts and digest, the batch ends, the end of the file). Every batch must start
//!    where the batch before it ends (or at offset 0 of the next segment; a file's first batch
//!    where the file before it ends), so each place of the chain is one frame's. Retained data
//!    payloads are not read: the pass seeks over them, so their hashes are checked by the
//!    ingest, which reads them once. The pass reads and hashes the removal records (kinds 20 to
//!    23), every `DROPPED` record, every `REF_UPDATE` record whose name is one it needs (the
//!    mode, the repo id, the store format; it reads the name of each), and the `TAG` records a
//!    tag stop needs; it keeps the `FILE` records' names, lengths, hashes and offsets, and each
//!    batch's place and offsets (for the frame checks).
//! 3. The removal proofs, in physical source order: each apply by the store's own validator
//!    (`crate::redaction_io::Validator` for a project, whose span headers are read by a seek to
//!    their batch and a walk of its frame heads; HA1's logical proof, `sync_proof::check`, for a
//!    holder), reading its records by the offsets the forward pass noted, each distinct one at
//!    most once per proof. Every `DROPPED` record must name the valid apply that first covers
//!    its span, or a carried record of the same file that names that exact span with
//!    `held: false` (an earlier proof: a later apply that covers the span leaves the drop as it
//!    is). Each file's trailer removal facts must be what its applies resolve, and its header's
//!    repo and store format what the log's meta refs say. Proof failures are reported after the
//!    framing of every file.
//! 4. The meaning checks of the whole chain, as a log's bootstrap runs them (`Scan::meaning`):
//!    the carried records' own rules, and the applies' meaning against every frame (a project's
//!    `check_log`; a holder's copy and embedding rules), over walks of the chain's frames.
//! 5. The stop: an exact batch end (`pos:`, never `0.0`), the first batch that holds the one
//!    landing a prefix names, or the batch of the TAG record of a name.
//! 6. Each file wholly after the stop is read whole, through its end, by the dump reader: it adds
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
    self, BYTES_AFTER, DumpHeader, DumpReader, DumpStore, DumpTrailer, Frame, FrameKinds,
    PRELUDE_LEN, RecordHead, check_file_head, check_frame_head, check_last, check_trailer, damaged,
    drop_text, dropped_len, header_of, link, parse_batch_record, parse_dropped_of, read_body,
    read_envelope, read_head, read_prelude, rec, try_batch_digest, try_kinds_digest,
};
use crate::dump_chain::{BatchMarker, DumpKind, chain_text, follows};
use crate::error::StoreError;
use crate::format::{REF_STORE_FEATURES, REF_STORE_FORMAT, StoreFeatures, decode_u32};
use crate::holder::{FirstBatch, REF_STORE_MODE, StoreMode};
use crate::kinds::{self, BatchCommit};
use crate::meta::{REF_REPO_ID, decode_repo_id};
use crate::object_refs::ObjectKey;
use crate::redaction::is_removable;
use crate::redaction_carried::{check_bounds, decode_record};
use crate::redaction_import::{PROOF_KINDS, ReceiverCaps, Scanned, copy_fits, proof_error};
use crate::redaction_io::{
    ApplyFrame, CarriedFrame, Collected, ERASE_BUFFER, FrameLog, RecordAccess, Seen, Validator,
    apply_record,
};
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::stream_error::StreamError;
use crate::sync_proof::{self, ObjectSource, PortableRemoval, RemovalObject};
use crate::sync_stream::StreamRecord;
use rdy_model::redaction::{
    CarriedSpan, FramePlace, MAX_RECORD_BYTES, Redaction, RedactionCarried, RedactionResidue,
};
use rdy_model::{Hash, LandingRecord, Provenance, RepoId, Tag};
use std::borrow::Cow;
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
/// The refusal of a batch that does not start where the batch before it ends (or at offset 0 of
/// the next segment): every place of a chain is held by one frame only.
pub const BATCH_GAP: &str = "a batch does not follow the batch before it";

/// The bytes of a `REF_UPDATE` payload the forward pass reads first: enough for its name's
/// length and the longest name it needs (the mode, the repo id, the store format). A ref of one
/// of those names is then read whole, whatever its length, and decoded as the store decodes it
/// (postcard ignores trailing bytes).
const REF_NAME_READ: u64 = 128;

/// The names of the refs the forward pass reads whole.
const SCANNED_REFS: [&str; 4] = [
    REF_STORE_MODE,
    REF_REPO_ID,
    REF_STORE_FORMAT,
    REF_STORE_FEATURES,
];

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
    /// The file's store (format or features) is not the chain's.
    StoreFormat { file: DumpStore, chain: DumpStore },
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
    /// A frame check by its batch: a span's frame header, and the walks of the meaning checks
    /// (the applies against every frame, the carried records' spans, a holder's copies).
    Check,
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
    if env.header.store != first.header.store {
        return Some(ChainLink::StoreFormat {
            file: env.header.store.clone(),
            chain: first.header.store.clone(),
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
    store: DumpStore,
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

    /// The store the retained prefix holds at the selected end. For a chain of a scalar store:
    /// the value of its last `meta/store_format` update before the end, or the chain's header
    /// format when the prefix sets none; a prefix that stops before a source upgrade batch holds
    /// the older format, and the load's own upgrade starts from it. For a chain of a feature
    /// store: the manifest of its last `meta/store_features` update before the end, or the
    /// chain's header manifest when the prefix sets none.
    pub fn store(&self) -> &DumpStore {
        &self.store
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

/// The input reads of proof lookups and frame checks: at most one file open at a time.
struct Lookups<'a> {
    paths: &'a [PathBuf],
    open: Option<Input>,
}

impl Lookups<'_> {
    /// The open input `file`, opened when another one is open (or none).
    fn file(&mut self, file: usize) -> Result<&mut Input, StoreError> {
        if self.open.as_ref().is_none_or(|i| i.file != file) {
            self.open = None;
            let f = File::open(&self.paths[file])?;
            self.open = Some(Input {
                f,
                file,
                pos: 0,
                pass: ScanPass::Lookup,
            });
        }
        Ok(self.open.as_mut().expect("an open input"))
    }

    /// Reads and hash-checks the record `r` by its offset.
    fn read(&mut self, r: &Rec) -> Result<Vec<u8>, StoreError> {
        let i = self.file(r.file)?;
        i.pass = ScanPass::Lookup;
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

    /// Fills `buf` from input `file` at byte `offset`, for a frame check.
    fn read_at(&mut self, file: usize, buf: &mut [u8], offset: u64) -> Result<(), StoreError> {
        let i = self.file(file)?;
        read_checked(&i.f, file, buf, offset)
    }
}

/// Fills `buf` from `f`, input `file`, at byte `offset`, and reports the read (a frame check).
/// The file ending first is a dump that ends early.
fn read_checked(f: &File, file: usize, buf: &mut [u8], offset: u64) -> Result<(), StoreError> {
    crate::pio::read_exact_at(f, buf, offset).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => damaged(offset, dump::ENDS_EARLY),
        _ => StoreError::Io(e),
    })?;
    report(ScanRead {
        file,
        offset,
        len: buf.len() as u64,
        pass: ScanPass::Check,
    });
    Ok(())
}

/// One batch of the chain, as the frame checks find it: where it starts in the log, its input,
/// the byte offsets of its `BATCH` record and of its first frame's head, and its marker.
#[derive(Clone, Copy, Debug)]
struct BatchAt {
    start: Location,
    file: usize,
    /// The dump format of the input that holds it.
    format: u32,
    record: u64,
    first: u64,
    marker: BatchMarker,
}

/// The chain's batches in log order (their starts strictly ascend: each batch follows the one
/// before it), grown by fallible reservation, and where the last frame check stopped.
#[derive(Default)]
struct Batches {
    at: Vec<BatchAt>,
    /// The batch, the frame's index in it, the place and the head offset of the last frame a
    /// check reached.
    cursor: Option<(usize, u32, u64, u64)>,
}

/// One frame of the chain as a frame check reads it: its original kind, hash and payload length,
/// whether a `DROPPED` record stands for it (its payload then reads as zeros), and its record's
/// input and offset. The batch marker after a batch's frames is a frame too (`marker`), as the
/// log holds one there.
#[derive(Clone, Debug)]
struct FrameAt {
    kind: u8,
    hash: Hash,
    len: u32,
    dropped: bool,
    file: usize,
    head: u64,
    marker: Option<Vec<u8>>,
}

/// The log encoding of batch `b`'s marker.
fn marker_bytes(m: &BatchMarker) -> Vec<u8> {
    crate::store::encode(&BatchCommit {
        start: m.start,
        frames: m.frames,
        digest: m.digest,
    })
}

impl Batches {
    /// The batch that holds `loc`: the last one that starts at or before it, in its segment.
    fn of(&self, loc: Location) -> Option<usize> {
        let i = self.at.partition_point(|b| b.start <= loc).checked_sub(1)?;
        (self.at[i].start.segment == loc.segment).then_some(i)
    }

    /// The frame of batch `b` whose head is at byte `head` of its input and whose place is
    /// `place`: its record's head, and a `DROPPED` record's descriptor.
    fn frame(
        &self,
        lookups: &mut Lookups<'_>,
        b: usize,
        place: u64,
        head: u64,
    ) -> Result<(FrameAt, u64, u64), StoreError> {
        let batch = &self.at[b];
        let mut h = [0u8; FRAME_HEADER as usize];
        lookups.read_at(batch.file, &mut h, head)?;
        frame_of(&h, batch, place, head, |buf, at| {
            lookups.read_at(batch.file, buf, at)
        })
    }
}

/// The frame of `batch` whose record head `h` was read at byte `head`, at log offset `place`;
/// `read` reads more of the input (a `DROPPED` record's descriptor). Returns it, the next
/// frame's place and the next record's offset.
fn frame_of(
    h: &[u8; FRAME_HEADER as usize],
    batch: &BatchAt,
    place: u64,
    head: u64,
    mut read: impl FnMut(&mut [u8], u64) -> Result<(), StoreError>,
) -> Result<(FrameAt, u64, u64), StoreError> {
    let rlen = u32::from_le_bytes(h[0..4].try_into().expect("4 bytes"));
    let (kind, hash, len, dropped) = if h[4] == rec::DROPPED {
        let want = dropped_len(batch.format);
        let mut buf = [0u8; dump::ENCODED_DROPPED_LEN];
        let d = &mut buf[..want];
        if rlen as usize != want {
            return Err(damaged(head, drop_text::WRONG_LENGTH));
        }
        read(d, head + FRAME_HEADER)?;
        let f = parse_dropped_of(batch.format, d).map_err(|what| damaged(head, what))?;
        (f.kind(), *f.hash(), f.original_len(), true)
    } else {
        (
            h[4],
            Hash(h[5..37].try_into().expect("32 bytes")),
            rlen,
            false,
        )
    };
    let next_place = place
        .checked_add(FRAME_HEADER)
        .and_then(|p| p.checked_add(u64::from(len)))
        .ok_or_else(|| damaged(head, chain_text::OVERFLOW))?;
    let next_head = head + FRAME_HEADER + u64::from(rlen);
    Ok((
        FrameAt {
            kind,
            hash,
            len,
            dropped,
            file: batch.file,
            head,
            marker: None,
        },
        next_place,
        next_head,
    ))
}

/// The marker frame of `batch` (its place is just past the batch's last frame).
fn marker_of(batch: &BatchAt) -> FrameAt {
    let bytes = marker_bytes(&batch.marker);
    FrameAt {
        kind: kinds::BATCH_COMMIT,
        hash: Hash::of(&bytes),
        len: bytes.len() as u32,
        dropped: false,
        file: batch.file,
        head: batch.record,
        marker: Some(bytes),
    }
}

/// The chain's records as the proof validators' source (`RecordAccess`, `FrameLog`): records by
/// the places the forward pass noted, read again by their offsets; a frame's header, and its
/// payload, by a seek to its batch and a walk of that batch's frame heads; and every frame of the
/// chain in log order for the meaning checks. A dropped frame reads as its original header and
/// zeros, as an erased frame of a log does.
struct ChainAccess<'a, 'p> {
    lookups: &'a mut Lookups<'p>,
    batches: &'a mut Batches,
    recs: &'a HashMap<(u32, u64), Rec>,
    /// The current batch's LANDING frames, by ascending place.
    batch: &'a [(Location, Rec)],
    drops: &'a HashMap<(u32, u64), DropRec>,
    /// Each input's length.
    lens: &'a [u64],
}

impl ChainAccess<'_, '_> {
    fn find(&self, loc: Location) -> Option<&Rec> {
        self.batch
            .binary_search_by_key(&loc, |(l, _)| *l)
            .ok()
            .map(|i| &self.batch[i].1)
            .or_else(|| self.recs.get(&key(loc)))
    }

    /// The frame that starts at `loc`, if one does: a seek to its batch, then a walk of the
    /// batch's frame heads (from the last check's place when it lies before `loc` in the same
    /// batch).
    fn frame_at(&mut self, loc: Location) -> Result<Option<FrameAt>, StoreError> {
        Ok(self
            .frame_holding(loc)?
            .filter(|(start, _)| *start == loc.offset)
            .map(|(_, f)| f))
    }

    /// The frame whose bytes hold `loc` (a frame or a marker of a batch), and its place.
    fn frame_holding(&mut self, loc: Location) -> Result<Option<(u64, FrameAt)>, StoreError> {
        let Some(b) = self.batches.of(loc) else {
            return Ok(None);
        };
        let batch = self.batches.at[b];
        let (mut n, mut place, mut head) = (0u32, batch.start.offset, batch.first);
        if let Some((cb, cn, cplace, chead)) = self.batches.cursor
            && cb == b
            && cplace <= loc.offset
        {
            // resume where the last check stopped
            (n, place, head) = (cn, cplace, chead);
        }
        while n < batch.marker.frames {
            let (f, next_place, next_head) = self.batches.frame(self.lookups, b, place, head)?;
            if loc.offset < next_place {
                self.batches.cursor = Some((b, n, place, head));
                return Ok(Some((place, f)));
            }
            (n, place, head) = (n + 1, next_place, next_head);
        }
        let m = marker_of(&batch);
        let end = place + FRAME_HEADER + u64::from(m.len);
        Ok((loc.offset < end).then_some((place, m)))
    }

    /// The input and record offset to blame for a refusal at `loc`: the record of the frame
    /// that holds it, else its batch's `BATCH` record, else the first input's first record.
    fn blame(&mut self, loc: Location) -> (usize, u64) {
        if let Ok(Some((_, f))) = self.frame_holding(loc) {
            return (f.file, f.head);
        }
        match self.batches.of(loc) {
            Some(b) => (self.batches.at[b].file, self.batches.at[b].record),
            None => (0, PRELUDE_LEN),
        }
    }

    /// The payload of frame `f` (at `loc`) of `len` bytes: zeros for a dropped frame.
    fn payload_of(&mut self, f: &FrameAt, len: u32) -> Result<Vec<u8>, StoreError> {
        if f.len != len {
            return Err(StoreError::corrupt_at(Location {
                segment: 0,
                offset: f.head,
            }));
        }
        if let Some(m) = &f.marker {
            return Ok(m.clone());
        }
        let mut buf = zeros(len, f.head)?;
        if !f.dropped {
            self.lookups
                .read_at(f.file, &mut buf, f.head + FRAME_HEADER)?;
        }
        Ok(buf)
    }
}

/// `len` zero bytes through a fallibly allocated buffer (`record allocation failed` at `at`).
fn zeros(len: u32, at: u64) -> Result<Vec<u8>, StoreError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len as usize)
        .map_err(|_| damaged(at, drop_text::ALLOCATION))?;
    buf.resize(len as usize, 0);
    Ok(buf)
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
        if let Some(r) = self.find(loc) {
            return Ok((r.len, r.kind, r.hash) == (len, kind, *hash));
        }
        Ok(self
            .frame_at(loc)?
            .is_some_and(|f| (f.len, f.kind, f.hash) == (len, kind, *hash)))
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

impl FrameLog for ChainAccess<'_, '_> {
    fn header_at(&mut self, loc: Location) -> Result<Option<(u32, u8, Hash)>, StoreError> {
        Ok(self.frame_at(loc)?.map(|f| (f.len, f.kind, f.hash)))
    }

    fn payload(&mut self, loc: Location, len: u32) -> Result<Vec<u8>, StoreError> {
        let f = self
            .frame_at(loc)?
            .ok_or_else(|| StoreError::corrupt_at(loc))?;
        self.payload_of(&f, len).map_err(|e| match e {
            StoreError::RedactionCorrupt { .. } => StoreError::corrupt_at(loc),
            e => e,
        })
    }

    fn payload_is_zero(&mut self, loc: Location, len: u32) -> Result<bool, StoreError> {
        let f = self
            .frame_at(loc)?
            .ok_or_else(|| StoreError::corrupt_at(loc))?;
        if f.dropped {
            return Ok(true);
        }
        if f.len != len || f.marker.is_some() {
            return Err(StoreError::corrupt_at(loc));
        }
        let mut buf = vec![0u8; ERASE_BUFFER];
        let (mut at, end) = (
            f.head + FRAME_HEADER,
            f.head + FRAME_HEADER + u64::from(len),
        );
        while at < end {
            let n = (end - at).min(ERASE_BUFFER as u64) as usize;
            self.lookups.read_at(f.file, &mut buf[..n], at)?;
            if buf[..n].iter().any(|b| *b != 0) {
                return Ok(false);
            }
            at += n as u64;
        }
        Ok(true)
    }

    fn carried_record<'f>(
        &mut self,
        f: &'f CarriedFrame,
    ) -> Result<Option<Cow<'f, RedactionCarried>>, StoreError> {
        // the chain keeps no record: it reads this one again by its place
        let Some(r) = self.find(f.loc).copied() else {
            return Ok(None);
        };
        let Some(p) = self.record(f.loc, kinds::REDACTION_CARRIED, &f.hash)? else {
            return Ok(None);
        };
        let record = decode_record(&p).map_err(|_| damaged(r.offset, drop_text::ALLOCATION))?;
        Ok(record.map(Cow::Owned))
    }

    fn keeps_carried(&self) -> bool {
        false
    }

    fn walk(
        &mut self,
        upto: Location,
        memory: &BTreeMap<Location, (u8, Vec<u8>)>,
        f: &mut dyn FnMut(&Seen<'_>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        // a frame counts when it ends at or before `upto`, as a segment log's walk has it
        let inside = |loc: Location, end: u64| {
            loc.segment < upto.segment || (loc.segment == upto.segment && end <= upto.offset)
        };
        for b in 0..self.batches.at.len() {
            let batch = self.batches.at[b];
            if batch.start.segment > upto.segment {
                break;
            }
            let input = self.lookups.file(batch.file)?;
            let file: &File = &input.f;
            let mut w = Window::new(file, batch.file, self.lens[batch.file]);
            let (mut place, mut head) = (batch.start.offset, batch.first);
            for _ in 0..batch.marker.frames {
                let mut h = [0u8; FRAME_HEADER as usize];
                h.copy_from_slice(w.get(head, FRAME_HEADER as usize)?);
                let (fr, next_place, next_head) = frame_of(&h, &batch, place, head, |buf, at| {
                    buf.copy_from_slice(w.get(at, buf.len())?);
                    Ok(())
                })?;
                let loc = Location {
                    segment: batch.start.segment,
                    offset: place,
                };
                if !inside(loc, next_place) {
                    return Ok(());
                }
                let read = || -> Result<Vec<u8>, StoreError> {
                    let mut buf = zeros(fr.len, fr.head)?;
                    if !fr.dropped {
                        read_checked(file, fr.file, &mut buf, fr.head + FRAME_HEADER)?;
                    }
                    Ok(buf)
                };
                f(&Seen::read_by(loc, fr.kind, fr.hash, fr.len, &read))?;
                (place, head) = (next_place, next_head);
            }
            let m = marker_of(&batch);
            let loc = Location {
                segment: batch.start.segment,
                offset: place,
            };
            if !inside(loc, place + FRAME_HEADER + u64::from(m.len)) {
                return Ok(());
            }
            let bytes = m.marker.as_deref().expect("a marker's bytes");
            f(&Seen::in_memory(loc, m.kind, m.hash, bytes))?;
        }
        for (loc, (kind, payload)) in memory {
            f(&Seen::in_memory(*loc, *kind, Hash::of(payload), payload))?;
        }
        Ok(())
    }
}

/// A read window over one input for the frame walks: heads of a sequential walk cost one read
/// per `ERASE_BUFFER` bytes.
struct Window<'f> {
    f: &'f File,
    file: usize,
    len: u64,
    buf: Vec<u8>,
    start: u64,
    filled: usize,
}

impl<'f> Window<'f> {
    fn new(f: &'f File, file: usize, len: u64) -> Window<'f> {
        Window {
            f,
            file,
            len,
            buf: Vec::new(),
            start: 0,
            filled: 0,
        }
    }

    /// The `n` bytes at `at` (`n` at most `ERASE_BUFFER`).
    fn get(&mut self, at: u64, n: usize) -> Result<&[u8], StoreError> {
        let inside = at >= self.start && at + n as u64 <= self.start + self.filled as u64;
        if !inside {
            if at.checked_add(n as u64).is_none_or(|e| e > self.len) {
                return Err(damaged(at, dump::ENDS_EARLY));
            }
            if self.buf.is_empty() {
                self.buf = zeros(ERASE_BUFFER as u32, at)?;
            }
            let want = (self.len - at).min(ERASE_BUFFER as u64) as usize;
            read_checked(self.f, self.file, &mut self.buf[..want], at)?;
            self.start = at;
            self.filled = want;
        }
        let from = (at - self.start) as usize;
        Ok(&self.buf[from..from + n])
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
                Ok(payload) => crate::codec::split(r.kind, &payload)
                    .ok()
                    .map(|o| o.version)
                    .map(|encoding| StreamRecord::Present {
                        key: k,
                        encoding,
                        payload,
                    }),
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
/// An event holds no payload: an apply its decoded record (96 bytes) and how many of the batch's
/// landings come before it, a carried record only where it is (each check reads it again by
/// its offset, one record at a time).
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
        record: Option<rdy_model::redaction::RedactionApply>,
        landings: usize,
    },
    Carried {
        at: Location,
        offset: u64,
        hash: Hash,
        rec: Rec,
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
    features_ref: Option<Vec<u8>>,
    /// Each `meta/store_features` update of the log: its record's place, its decoded value, and
    /// its input and offset there.
    features_updates: Vec<(Location, Option<StoreFeatures>, usize, u64)>,
    /// Each `meta/store_format` update of the log: its record's place, its decoded value, and
    /// its input and offset there.
    format_updates: Vec<(Location, Option<u32>, usize, u64)>,
    /// Removal proofs.
    collected: Collected,
    recs: HashMap<(u32, u64), Rec>,
    /// The current batch's LANDING frames, by ascending place, and their hashes.
    batch_landings: Vec<(Location, Rec)>,
    landing_hashes: Vec<(Location, Hash)>,
    /// The chain's batches, for the frame checks; each input's length.
    batches: Batches,
    lens: Vec<u64>,
    /// The record hash of each carried record's source apply: one record per source apply.
    carried_by_apply: HashMap<Hash, Hash>,
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

/// Scans `src` with `stop` (module docs, steps 2 to 6) and returns the verified plan. No target
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
        format_updates: Vec::new(),
        features_ref: None,
        features_updates: Vec::new(),
        collected: Collected::default(),
        recs: HashMap::new(),
        batch_landings: Vec::new(),
        landing_hashes: Vec::new(),
        batches: Batches::default(),
        lens: src.envelopes.iter().map(|e| e.len).collect(),
        carried_by_apply: HashMap::new(),
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
    s.meaning()?;
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
    let store = s.store_at(end)?;
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
        store,
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

    /// The store in force at `end` (a batch end): the last update of a record before it, or the
    /// chain's header store without one; a chain of a scalar store reads its store format
    /// updates, a chain of a feature store its features updates. An update in force that does
    /// not decode is damage at its record.
    fn store_at(&self, end: Location) -> Result<DumpStore, ScanError> {
        match &self.envs[0].header.store {
            DumpStore::Legacy(n) => match self.format_updates.iter().rev().find(|u| u.0 < end) {
                None => Ok(DumpStore::Legacy(*n)),
                Some((_, Some(v), _, _)) => Ok(DumpStore::Legacy(*v)),
                Some((_, None, file, offset)) => {
                    Err(self.fail(*file, damaged(*offset, METADATA_MISMATCH)))
                }
            },
            DumpStore::Features(m) => {
                match self.features_updates.iter().rev().find(|u| u.0 < end) {
                    Some((_, Some(v), _, _)) => Ok(DumpStore::Features(v.clone())),
                    Some((_, None, file, offset)) => {
                        Err(self.fail(*file, damaged(*offset, METADATA_MISMATCH)))
                    }
                    // a prefix with no features ref that holds a scalar store format is that
                    // store's log before its move to features (a moved released store): the
                    // load upgrades it as it upgrades a released dump
                    None => match self.format_updates.iter().rev().find(|u| u.0 < end) {
                        Some((_, Some(v), _, _)) => Ok(DumpStore::Legacy(*v)),
                        Some((_, None, file, offset)) => {
                            Err(self.fail(*file, damaged(*offset, METADATA_MISMATCH)))
                        }
                        None => Ok(DumpStore::Features(m.clone())),
                    },
                }
            }
        }
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
        // every batch follows the one before it (the base for a file's first): one frame holds
        // each place of the chain, before any frame of this batch is read
        match f.last {
            None if !follows(links.base, start) => {
                return Err(self.fail(i, damaged(head.at, chain_text::FIRST_BATCH)));
            }
            Some((prev_end, _)) if !follows(prev_end, start) => {
                return Err(self.fail(i, damaged(head.at, BATCH_GAP)));
            }
            _ => {}
        }
        self.batches
            .at
            .try_reserve(1)
            .map_err(|_| alloc(i, head.at))?;
        self.batches.at.push(BatchAt {
            start,
            file: i,
            format: f.format,
            record: head.at,
            first: head.end(),
            marker: BatchMarker {
                start,
                frames: count,
                digest,
            },
        });
        let allowed = FrameKinds::of(f.format, &self.envs[i].header.store);
        let first_of_log = self.mode.is_none();
        self.batch_landings.clear();
        self.landing_hashes.clear();
        let mut pairs: Vec<(u8, Hash)> = Vec::new();
        let mut events: Vec<Event> = Vec::new();
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
                let frame = parse_dropped_of(f.format, &p)
                    .map_err(|what| self.fail(i, damaged(fh.at, what)))?;
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
                self.present(r, f, &fh, at, rec_, &mut events, first_of_log)
                    .map(|(l, t)| {
                        matched_landing |= l;
                        matched_tag |= t;
                    })?;
                (fh.kind, fh.hash, len)
            };
            if !allowed.is_some_and(|a| a.allows(kind)) {
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
        // the batch's physical end and marker
        let marker = BatchMarker {
            start,
            frames: count,
            digest,
        };
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
            if let Err(source) = self.event(f, e, start, end) {
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
        first_of_log: bool,
    ) -> Result<(bool, bool), ScanError> {
        let i = f.file;
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
                        self.collected
                            .note_request(at, fh.hash)
                            .map_err(|_| alloc(i, fh.at))?;
                        self.note(at, rec_, i, fh.at)?;
                    }
                    kinds::REDACTION_RESIDUE => {
                        self.collected
                            .note_residue(at, fh.hash)
                            .map_err(|_| alloc(i, fh.at))?;
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
                                record: apply_record(&p),
                                landings: self.landing_hashes.len(),
                            },
                            i,
                            fh.at,
                        )?;
                    }
                    _ => {
                        // the checks read the record again by its place
                        self.note(at, rec_, i, fh.at)?;
                        push(
                            events,
                            Event::Carried {
                                at,
                                offset: fh.at,
                                hash: fh.hash,
                                rec: rec_,
                            },
                            i,
                            fh.at,
                        )?
                    }
                }
            }
            kinds::REF_UPDATE => {
                // the name first; a ref of a name the scan needs is read whole, hashed, and
                // decoded as the store decodes it, whatever its length
                let Some(p) = self.ref_payload(r, f, fh)? else {
                    return Ok(matched);
                };
                if first_of_log {
                    let next = at
                        .offset
                        .checked_add(FRAME_HEADER)
                        .and_then(|o| o.checked_add(fh.len))
                        .map(|offset| Location {
                            segment: at.segment,
                            offset,
                        })
                        .ok_or_else(|| self.fail(i, damaged(fh.at, chain_text::OVERFLOW)))?;
                    self.first_batch
                        .frame(fh.kind, Some(&p), next)
                        .map_err(|e| self.fail(i, e))?;
                }
                if let Some(u) = crate::holder::decode_ref(&p) {
                    if u.name == REF_REPO_ID {
                        self.repo_ref = Some(u.value);
                    } else if u.name == REF_STORE_FORMAT {
                        push(
                            &mut self.format_updates,
                            (at, decode_u32(&u.value), i, fh.at),
                            i,
                            fh.at,
                        )?;
                        self.format_ref = Some(u.value);
                    } else if u.name == REF_STORE_FEATURES {
                        push(
                            &mut self.features_updates,
                            (at, StoreFeatures::parse(&u.value).ok(), i, fh.at),
                            i,
                            fh.at,
                        )?;
                        self.features_ref = Some(u.value);
                    }
                }
            }
            kinds::TAG if matches!(self.stop, Some(StopMatch::TagName(_))) => {
                let p = read_body(r, fh).map_err(|e| self.fail(i, e))?;
                if let (Some(StopMatch::TagName(name)), Ok(t)) =
                    (self.stop, crate::codec::decode_record::<Tag>(fh.kind, &p))
                    && t.name == *name
                {
                    self.tag.take(fh.hash, t.landing);
                    matched.1 = true;
                }
            }
            _ => {
                if fh.kind == kinds::LANDING {
                    push(&mut self.landing_hashes, (at, fh.hash), i, fh.at)?;
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

    /// The payload of the `REF_UPDATE` record `fh` when its name is one the scan needs (the
    /// mode, the repo id, the store format), read whole and checked against its hash; None for
    /// any other ref, whose rest is skipped.
    fn ref_payload(
        &self,
        r: &mut BufReader<Input>,
        f: &FileScan,
        fh: &RecordHead,
    ) -> Result<Option<Vec<u8>>, ScanError> {
        let i = f.file;
        if fh.end() > f.len {
            return Err(self.fail(i, damaged(fh.at, dump::ENDS_EARLY)));
        }
        let mut p: Vec<u8> = Vec::new();
        let first = fh.len.min(REF_NAME_READ) as usize;
        p.try_reserve_exact(first).map_err(|_| alloc(i, fh.at))?;
        p.resize(first, 0);
        dump::read_exact_at(r, &mut p, fh.at).map_err(|e| self.fail(i, e))?;
        let wanted = dump::ref_name(&p)
            .is_some_and(|name| SCANNED_REFS.iter().any(|n| n.as_bytes() == name));
        if !wanted {
            let n = i64::try_from(fh.len - first as u64).map_err(|_| alloc(i, fh.at))?;
            r.seek_relative(n)
                .map_err(|e| self.fail(i, StoreError::Io(e)))?;
            return Ok(None);
        }
        let mut left = fh.len - first as u64;
        while left > 0 {
            let n = left.min(ERASE_BUFFER as u64) as usize;
            p.try_reserve(n).map_err(|_| alloc(i, fh.at))?;
            let from = p.len();
            p.resize(from + n, 0);
            dump::read_exact_at(r, &mut p[from..], fh.at).map_err(|e| self.fail(i, e))?;
            left -= n as u64;
        }
        if Hash::of(&p) != fh.hash {
            return Err(self.fail(i, damaged(fh.at, dump::HASH_MISMATCH)));
        }
        Ok(Some(p))
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

    /// One proof event, in physical order, of a batch that starts at `start` and ends at `end`.
    fn event(
        &mut self,
        f: &mut FileScan,
        e: Event,
        start: Location,
        end: Location,
    ) -> Result<(), StoreError> {
        match e {
            Event::Drop { at, offset, frame } => self.on_drop(f.file, at, offset, frame),
            Event::Apply {
                at,
                offset,
                hash,
                record,
                landings,
            } => {
                let idx = match self.mode {
                    Some(StoreMode::Holder) => self.holder_apply(f.file, at, offset, hash, end)?,
                    _ => self.native_apply(f.file, at, offset, hash, record, landings, end)?,
                };
                f.validated.push(idx);
                Ok(())
            }
            Event::Carried {
                at,
                offset,
                hash,
                rec,
            } => self.on_carried(f.file, at, offset, hash, rec, start, end),
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
        // one frame holds each place: a second dropped record of a place is no frame of it
        if self.drops.contains_key(&key(at)) {
            return Err(damaged(offset, drop_text::SPAN));
        }
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
        record: Option<rdy_model::redaction::RedactionApply>,
        landings: usize,
        end: Location,
    ) -> Result<usize, StoreError> {
        let frame = ApplyFrame::new(at, hash, record.clone(), self.batch_no, Vec::new());
        let checked = {
            let mut access = ChainAccess {
                lookups: &mut self.lookups,
                batches: &mut self.batches,
                recs: &self.recs,
                batch: &self.batch_landings,
                drops: &self.drops,
                lens: &self.lens,
            };
            self.validator.apply_after(
                &self.collected,
                &mut access,
                &frame,
                &self.landing_hashes[..landings],
                None,
            )
        };
        checked.map_err(|e| self.proof_refusal(e, offset))?;
        let record = record.expect("a validated apply decodes");
        let spans = self.validator.last_spans();
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
        let landing = self.batch_landings[..landings]
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
                        Ok(Some(r @ StreamRecord::Present { .. })) => match &r {
                            StreamRecord::Present { payload, .. } => {
                                crate::codec::decode_bounded::<RedactionResidue>(
                                    r.raw_kind(),
                                    payload,
                                )
                                .ok()
                            }
                            _ => None,
                        },
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

    /// A carried record of the chain: it must decode in its bounded canonical form, and be the
    /// only record of its source apply (an identical copy aside: `carried removal provenance
    /// conflicts`); it releases the dropped frames that name it and that it names with
    /// `held: false`, each of its own input and with its exact identity. A dropped frame that
    /// names another proof is left to that proof (an apply that covers a span after a carried
    /// record recognized it becomes its proof: D-0VG72), and an unreleased one is refused at the
    /// end. The record is read again by its offset and released after these checks; the meaning
    /// checks keep only where it is, and read it again themselves.
    #[allow(clippy::too_many_arguments)]
    fn on_carried(
        &mut self,
        file: usize,
        at: Location,
        offset: u64,
        hash: Hash,
        rec_: Rec,
        start: Location,
        end: Location,
    ) -> Result<(), StoreError> {
        let payload = self.lookups.read(&rec_)?;
        let record = decode_record(&payload)
            .map_err(|_| damaged(offset, drop_text::ALLOCATION))?
            .ok_or_else(|| damaged(offset, drop_text::NO_AUTHORIZATION))?;
        drop(payload);
        // one record per source apply: another record of the same source apply conflicts
        match self.carried_by_apply.get(&record.apply) {
            Some(h) if *h != hash => return Err(damaged(offset, PROVENANCE_CONFLICT)),
            Some(_) => {}
            None => {
                self.carried_by_apply
                    .try_reserve(1)
                    .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
                self.carried_by_apply.insert(record.apply, hash);
            }
        }
        // the dropped frames that name this record: each must be of its own input, with the
        // span's exact identity; a drop that names another proof waits for that proof
        for s in record.spans.iter().filter(|s| !s.held) {
            let place = place_of(s.place);
            if self.drops.get(&key(place)).is_some_and(|d| d.apply == hash) {
                self.release(place, hash, (s.kind, s.hash, s.len, s.residue), Some(file))?;
            }
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
        // where it is, for the meaning checks: they read the record again (`carried_record`)
        self.collected
            .carried
            .try_reserve(1)
            .map_err(|_| damaged(offset, drop_text::ALLOCATION))?;
        self.collected.carried.push(CarriedFrame {
            loc: at,
            hash,
            record: None,
            batch_start: start,
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
        let format_ok = match &env.header.store {
            DumpStore::Legacy(n) => self
                .format_ref
                .as_deref()
                .is_none_or(|v| decode_u32(v) == Some(*n)),
            DumpStore::Features(m) => self
                .features_ref
                .as_deref()
                .is_none_or(|v| StoreFeatures::parse(v).is_ok_and(|f| f == *m)),
        };
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

    /// The meaning checks of the whole chain, before any target exists, as the store's bootstrap
    /// runs them on a log (the applies are already proved, in physical order, by the forward
    /// pass): for a project, the carried records laid over the validated applies
    /// (`redaction_carried::overlay`: each record's own bounds, one record per source apply,
    /// ascending spans before its batch, each span against its frame and residue) and the
    /// applies' meaning against every frame (`check_log`: every copy covered, all-versions
    /// coverage, the residue of every intact span); for a holder, HA1's rules 2 and 3 over every
    /// frame (each copy of an approved tuple, and no uncovered embedding) and the carried
    /// overlay. Every file is checked, those wholly after any stop too. A failure is blamed on
    /// the record of the frame it names.
    fn meaning(&mut self) -> Result<(), ScanError> {
        if self.applies.is_empty() && self.collected.carried.is_empty() {
            return Ok(());
        }
        let upto = self.envs[self.envs.len() - 1].header.log_end;
        let checked = {
            let mut access = ChainAccess {
                lookups: &mut self.lookups,
                batches: &mut self.batches,
                recs: &self.recs,
                batch: &[],
                drops: &self.drops,
                lens: &self.lens,
            };
            match self.mode {
                Some(StoreMode::Holder) => {
                    let applies: Vec<(Location, Hash)> =
                        self.applies.iter().map(|a| (a.at, a.hash)).collect();
                    let removals: BTreeMap<Hash, PortableRemoval> = self
                        .applies
                        .iter()
                        .map(|a| {
                            (
                                a.hash,
                                PortableRemoval {
                                    request: a.named.0,
                                    apply: a.hash,
                                    landing: a.named.1,
                                    objects: a.objects.clone(),
                                },
                            )
                        })
                        .collect();
                    crate::redaction_import::check_proved(
                        &self.collected,
                        &mut access,
                        upto,
                        &applies,
                        removals,
                        self.holder.residues.clone(),
                    )
                }
                _ => self
                    .validator
                    .check_meaning(&self.collected, &mut access, upto),
            }
        };
        match checked {
            Ok(()) => Ok(()),
            Err(StoreError::RedactionCorrupt { segment, offset }) => {
                let loc = Location { segment, offset };
                let what = if self.drops.contains_key(&key(loc)) {
                    drop_text::SPAN
                } else {
                    drop_text::NO_AUTHORIZATION
                };
                let (file, at) = ChainAccess {
                    lookups: &mut self.lookups,
                    batches: &mut self.batches,
                    recs: &self.recs,
                    batch: &[],
                    drops: &self.drops,
                    lens: &self.lens,
                }
                .blame(loc);
                Err(ScanError::Input {
                    file,
                    source: damaged(at, what),
                })
            }
            Err(e) => {
                let file = self
                    .applies
                    .first()
                    .map(|a| a.file)
                    .or_else(|| self.carried.first().map(|c| c.file))
                    .unwrap_or(0);
                let source = match e {
                    StoreError::RedactionHuman | StoreError::Decode(_) => {
                        damaged(PRELUDE_LEN, drop_text::NO_AUTHORIZATION)
                    }
                    StoreError::Io(e) if e.kind() == io::ErrorKind::OutOfMemory => {
                        damaged(PRELUDE_LEN, drop_text::ALLOCATION)
                    }
                    e => e,
                };
                Err(ScanError::Input { file, source })
            }
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
    fn read_typed<T: serde::de::DeserializeOwned + 'static>(
        &mut self,
        r: &Rec,
    ) -> Result<T, ScanError> {
        let p = self.lookups.read(r).map_err(|source| ScanError::Input {
            file: r.file,
            source,
        })?;
        crate::codec::decode_record::<T>(r.kind, &p).map_err(|_| ScanError::Input {
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
        let input = |what| ScanError::Input {
            file: rec_.file,
            source: damaged(offset, what),
        };
        decode_record(&p)
            .map_err(|_| input(drop_text::ALLOCATION))?
            .ok_or_else(|| input(drop_text::NO_AUTHORIZATION))
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
            // already retained: a carried record of this source apply (a later carried record
            // must be that very record), or the apply itself
            if let Some(k) = kept.get(&identity).copied() {
                if !is_apply && self.carried[idx].hash != self.carried[k].hash {
                    return Err(conflict());
                }
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
            // the first apply that covers the place: when it is retained, the place is its
            // hole whatever else the dropped record names
            let first_cover = if holder {
                self.holder
                    .approved
                    .get(&ObjectKey {
                        kind: d.kind,
                        hash: d.hash,
                    })
                    .map(|(t, a)| (*a, t.residue))
            } else {
                self.validator
                    .auth
                    .spans
                    .get(&at)
                    .filter(|s| !s.carried)
                    .map(|s| (s.apply, s.residue))
            };
            let action = if let Some((apply, residue)) = first_cover
                && pre.contains(&apply)
                && d.apply != apply
            {
                FrameAction::Zero {
                    authority: apply,
                    residue,
                }
            } else if pre.contains(&d.apply) || kept_hashes.contains(&d.apply) {
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
