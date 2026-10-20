//! Payload erasure and the authorization bootstrap (plan Redaction; D-3M81W: rdy "drops their
//! bytes and keeps their hashes"; D-600CW: the redaction record never leaves the log).
//!
//! A sealed, synced REDACTION_APPLY authorizes its request's spans. From then on the removal is
//! irreversible: [`Store::finish_redactions`] overwrites each authorized payload with zeros through
//! one fixed buffer, in `(segment, offset)` order, syncs each affected segment and rebuilds the
//! derived index from the surviving data and the residues. No completion bit is stored: every
//! open recognizes zeroed spans and repeats the bounded work safely.
//!
//! The open never takes the index as authority. [`bootstrap`] reads frame headers and batch
//! markers first, collects the sealed apply records with their requests, residues and same-batch
//! human landings, and validates them; only then does the payload-verifying scan run, and only
//! the exact authorized spans may fail their original payload hash there (`segment::Hole`). An
//! unknown hole, a damaged header, a missing residue, a broken marker or a missing apply is
//! corruption (`StoreError::RedactionCorrupt`).
//!
//! Before any zero write (an erasure with work left, a load, the live writer) the bootstrap also
//! checks each apply's meaning against the log as the checked apply path does ([`check_log`]):
//! its request selects exactly what its manifest removes, every physical copy of the selected
//! content is a span, and every intact span is exactly what its residue records, so a removal
//! never erases unselected content, never leaves a readable copy and never changes the history
//! a residue replays.
//!
//! A carried record (kind REDACTION_CARRIED, `crate::redaction_carried`) is laid over the
//! validated applies before their check against the log: its `held: false` spans whose payload
//! is all zero join the holes, and a validated apply that covers a span stays its proof. The
//! check resolves a change residue's kept reference through a carried block residue, and
//! nothing else of it. Recognition is not erasure: only a span a validated apply covers is ever
//! erased ([`Authorization::erasable`]).
//!
//! The scope is the active store files. File-system snapshots, backups, responses already sent,
//! old dumps and media remnants are outside it (`W_REDACTION_COPIES`); quarantine files are
//! possible copies (F2(c)), and nothing here lists or removes them.
use crate::error::StoreError;
use crate::index::IndexEntry;
use crate::kinds::{self, BlobManifest};
use crate::redaction::{
    check_bounds, is_redaction_kind, is_removable, manifest_in_order, residual_refs,
    residual_versions, residue_matches, targets_agree,
};
use crate::segment::{self, HEADER, Hole, Holes, Location, header_at};
use crate::store::{decode, encode};
use rdy_model::redaction::{
    MAX_RECORD_BYTES, Redaction, RedactionApply, RedactionResidue, ResidualOp, record_buffer,
};
use rdy_model::{Assurance, Block, BlockId, Change, Hash, LandingRecord, Op, Role};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io;

/// The one buffer size of every erasure write and of the quarantine copy, in bytes.
pub const ERASE_BUFFER: usize = 65_536;

/// What a store's active redactions remove, and what a check hashed.
///
/// - `objects`: the distinct `(kind, hash)` objects of the active spans;
/// - `frames`: the active spans (physical frames), each counted once however many requests name
///   it;
/// - `bytes`: the payload lengths of those spans;
/// - `checked_payloads`: from `Store::verify_report`, the intact data frames whose payload was
///   hashed successfully (an erased payload is never hashed or counted here); from
///   `Store::finish_redactions`, the authorized payloads found intact, and hashed, before this
///   call erased them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RedactionCounts {
    pub objects: u64,
    pub frames: u64,
    pub bytes: u64,
    pub checked_payloads: u64,
}

/// What `Store::verify_report` checked: the data frames (batch markers not counted, removed
/// payloads counted), and the redaction counts when an active redaction exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub frames: usize,
    pub redaction: Option<RedactionCounts>,
}

/// A deterministic failpoint of the irreversible part of a removal (test hook). Tests arm one
/// for the current thread with [`arm_failpoint`]; nothing else does.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failpoint {
    /// Right after the apply batch is synced (the open: before its first zero write).
    AfterApplySync,
    /// After each zero-buffer write of an erasure.
    AfterZeroWrite,
    /// After the affected segments are synced.
    AfterSegmentSync,
    /// Before the rebuilt index replaces the old one.
    BeforeIndexPublish,
}

impl Failpoint {
    pub const ALL: [Failpoint; 4] = [
        Failpoint::AfterApplySync,
        Failpoint::AfterZeroWrite,
        Failpoint::AfterSegmentSync,
        Failpoint::BeforeIndexPublish,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Failpoint::AfterApplySync => "after_apply_sync",
            Failpoint::AfterZeroWrite => "after_zero_write",
            Failpoint::AfterSegmentSync => "after_segment_sync",
            Failpoint::BeforeIndexPublish => "before_index_publish",
        }
    }

    pub fn parse(s: &str) -> Option<Failpoint> {
        Failpoint::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// What an armed failpoint does when it is reached (test hook).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailAction {
    /// The step fails with an injected I/O error; the failpoint disarms.
    Fail,
    /// The process aborts, with no destructor run (a kill).
    Abort,
}

thread_local! {
    /// The armed failpoint, what it does, and at which of its hits (1 is the next one).
    static ARMED: Cell<Option<(Failpoint, FailAction, u32)>> = const { Cell::new(None) };
}

/// Test hook: arms `p` for this thread (or disarms with None), to act at its next hit. An open
/// or a write on this thread reaches it.
#[doc(hidden)]
pub fn arm_failpoint(p: Option<(Failpoint, FailAction)>) {
    ARMED.with(|a| a.set(p.map(|(p, action)| (p, action, 1))));
}

/// Test hook: arms `p` for this thread to act at its `nth` hit (1 is the next one); the hits
/// before it pass. `AfterZeroWrite` is hit once per zero buffer, so this fails or kills an
/// erasure inside a payload longer than one buffer.
#[doc(hidden)]
pub fn arm_failpoint_nth(p: Failpoint, action: FailAction, nth: u32) {
    ARMED.with(|a| a.set(Some((p, action, nth.max(1)))));
}

/// Reaches failpoint `p`: an injected error or an abort when it is armed for this hit, else
/// nothing.
pub(crate) fn hit(p: Failpoint) -> io::Result<()> {
    match ARMED.with(|a| a.get()) {
        Some((armed, action, n)) if armed == p && n > 1 => {
            ARMED.with(|a| a.set(Some((armed, action, n - 1))));
            Ok(())
        }
        Some((armed, FailAction::Abort, _)) if armed == p => std::process::abort(),
        Some((armed, FailAction::Fail, _)) if armed == p => {
            ARMED.with(|a| a.set(None));
            Err(io::Error::other(format!(
                "injected failure at redaction failpoint {}",
                p.name()
            )))
        }
        _ => Ok(()),
    }
}

/// How one span is known: the residue that replaces it for reads, and the record that is its
/// proof with the request it names. The proof is the earliest validated apply in log order that
/// covers the span; only while no apply covers it, the first carried record (kind
/// REDACTION_CARRIED) that accounts for its zeros (`carried`). A carried proof recognizes exact
/// zeros and never authorizes an erasure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpanAuth {
    pub(crate) residue: Option<Hash>,
    pub(crate) apply: Hash,
    pub(crate) request: Hash,
    pub(crate) carried: bool,
}

/// One validated apply record: where it is, its hash and its record.
#[derive(Clone, Debug)]
pub(crate) struct Applied {
    pub(crate) loc: Location,
    pub(crate) hash: Hash,
    pub(crate) record: RedactionApply,
}

/// What a log's validated redaction applies authorize, derived from the log alone.
#[derive(Clone, Debug, Default)]
pub(crate) struct Authorization {
    /// Every authorized span, by frame location: the holes of the log.
    pub(crate) holes: Holes,
    pub(crate) spans: BTreeMap<Location, SpanAuth>,
    /// The residues the spans name, decoded.
    pub(crate) residues: HashMap<Hash, RedactionResidue>,
    /// The validated applies, in log order.
    pub(crate) applies: Vec<Applied>,
    /// Every sealed request frame inside the acknowledged log, in log order.
    pub(crate) requests: Vec<(Location, Hash)>,
    /// A holder's logical authorization (`crate::redaction_import`): what each validated apply
    /// approves and the receiver spans each request's approved objects have. None in a project,
    /// whose spans are its requests' own places.
    pub(crate) imported: Option<crate::redaction_import::Imported>,
    /// The validated carried records (kind REDACTION_CARRIED) of the sealed log, in log order
    /// (`crate::redaction_carried::overlay`).
    pub(crate) carried: Vec<crate::redaction_carried::CarriedAt>,
}

impl Authorization {
    /// Whether an active redaction exists: a validated apply, or a span whose zeros a carried
    /// record accounts for.
    pub(crate) fn is_active(&self) -> bool {
        !self.applies.is_empty() || self.spans.values().any(|s| s.carried)
    }

    /// Whether the carried record frame at `loc` is one of the validated carried records.
    pub(crate) fn holds_carried(&self, loc: Location) -> bool {
        self.carried.binary_search_by_key(&loc, |c| c.at).is_ok()
    }

    /// The index entry of the validated carried record at `loc`, if it is one.
    pub(crate) fn carried_entry_at(&self, loc: Location) -> Option<IndexEntry> {
        let i = self.carried.binary_search_by_key(&loc, |c| c.at).ok()?;
        Some(crate::redaction_carried::entry_of(&self.carried[i]))
    }

    /// Whether the span at `loc` may be erased: a validated apply covers it. A span only a
    /// carried record explains is never erased.
    pub(crate) fn erasable(&self, loc: &Location) -> bool {
        self.spans.get(loc).is_some_and(|s| !s.carried)
    }

    /// Whether the apply frame at `loc` is one of the validated applies (kept in log order).
    pub(crate) fn holds_apply(&self, loc: Location) -> bool {
        self.applies.binary_search_by_key(&loc, |a| a.loc).is_ok()
    }

    /// The objects, frames and bytes of the authorized spans (`checked_payloads` zero).
    pub(crate) fn counts(&self) -> RedactionCounts {
        let objects: BTreeSet<(u8, Hash)> = self.holes.values().map(|h| (h.kind, h.hash)).collect();
        RedactionCounts {
            objects: objects.len() as u64,
            frames: self.holes.len() as u64,
            bytes: self.holes.values().map(|h| u64::from(h.len)).sum(),
            checked_payloads: 0,
        }
    }

    /// The index entries of the hole at `loc` (kind `kind`, hash `hash`), from its residue: the
    /// object, and for a change the producer and deleter entries its ops give, as the original
    /// payload gave them. A change hole with no change residue is `RedactionCorrupt` at it.
    pub(crate) fn entries_for_hole(
        &self,
        kind: u8,
        hash: Hash,
        loc: Location,
    ) -> Result<Vec<IndexEntry>, StoreError> {
        let mut out = vec![IndexEntry::Object { kind, hash, loc }];
        if kind == kinds::CHANGE {
            let residue = self
                .spans
                .get(&loc)
                .and_then(|s| s.residue)
                .and_then(|r| self.residues.get(&r));
            let Some(RedactionResidue::Change(c)) = residue else {
                return Err(StoreError::corrupt_at(loc));
            };
            for op in &c.ops {
                match op {
                    ResidualOp::Create { version, .. } | ResidualOp::Update { version, .. } => out
                        .push(IndexEntry::Producer {
                            version: *version,
                            change: hash,
                        }),
                    ResidualOp::Delete { id, .. } => out.push(IndexEntry::Deleted {
                        block: id.clone(),
                        change: hash,
                    }),
                }
            }
        }
        Ok(out)
    }

    /// The redaction index entries the log's records give before `upto`, in log order: each
    /// sealed request's, then each validated apply's at its place. Reads and checks each request
    /// frame again.
    pub(crate) fn index_entries(
        &self,
        readers: &[File],
        upto: Location,
    ) -> Result<Vec<IndexEntry>, StoreError> {
        let mut at: Vec<(Location, IndexEntry)> = vec![];
        for (loc, hash) in self.requests.iter().filter(|(l, _)| *l < upto) {
            let payload = read_record(readers, &BTreeMap::new(), *loc, kinds::REDACTION, hash)?
                .ok_or_else(|| StoreError::corrupt_at(*loc))?;
            at.push((*loc, self.request_entry(*hash, &payload)?));
        }
        for a in self.applies.iter().filter(|a| a.loc < upto) {
            at.push((
                a.loc,
                IndexEntry::RedactionApply {
                    apply: a.hash,
                    request: a.record.request,
                    loc: a.loc,
                },
            ));
        }
        for c in self.carried.iter().filter(|c| c.at < upto) {
            at.push((c.at, crate::redaction_carried::entry_of(c)));
        }
        at.sort_by_key(|(l, _)| *l);
        Ok(at.into_iter().map(|(_, e)| e).collect())
    }

    /// The index entry of the `REDACTION` frame `hash` with `payload`, as this log reads it: in a
    /// project, the request's own entry (its spans are its manifest's places in this log); in a
    /// holder, the same objects with the receiver spans its approval derived
    /// (`crate::redaction_import`), never the source places its manifest names. A holder request
    /// that no validated apply approves has no span.
    pub(crate) fn request_entry(
        &self,
        hash: Hash,
        payload: &[u8],
    ) -> Result<IndexEntry, StoreError> {
        let entry = crate::redaction::request_entry(hash, payload)?;
        match (&self.imported, entry) {
            (
                Some(imported),
                IndexEntry::RedactionRequest {
                    request,
                    carrier,
                    objects,
                    ..
                },
            ) => Ok(IndexEntry::RedactionRequest {
                request,
                carrier,
                objects,
                spans: imported.spans_of(&request),
            }),
            (_, e) => Ok(e),
        }
    }
}

/// The closed human-approval test of a landing (O19): an Approver, and either Local with no relay
/// fields or Relayed with both. Policy never passes.
pub(crate) fn human_approval(l: &LandingRecord) -> bool {
    l.approved_by.role == Role::Approver
        && match l.assurance {
            Assurance::Local => l.relayed_by.is_none() && l.relay_evidence.is_none(),
            Assurance::Relayed => l.relayed_by.is_some() && l.relay_evidence.is_some(),
            Assurance::Policy => false,
        }
}

/// The meaning of an apply that holds wherever its records are (shared with the portable proof
/// check of a sync stream, `crate::sync_proof`): its landing landed the request's carrier, the
/// request keeps its bounds, canonical manifest order and selector expansion as the checked apply
/// path has them (`check_bounds`, `manifest_in_order`, `targets_agree`; final review finding 1),
/// and the apply binds the request's report hash. False when any of them fails. `buf` is the
/// report buffer, allocated on first use; a refused allocation is `RedactionLimit`.
pub(crate) fn request_agrees(
    request: &Redaction,
    landing: &LandingRecord,
    record: &RedactionApply,
    buf: &mut Option<Vec<u8>>,
) -> Result<bool, StoreError> {
    if !landing.changes.contains(&request.carrier)
        || check_bounds(request).is_err()
        || !manifest_in_order(&request.manifest)
        || targets_agree(request).is_err()
    {
        return Ok(false);
    }
    let buf = match buf {
        Some(b) => b,
        None => buf.insert(record_buffer().map_err(|_| StoreError::RedactionLimit {
            limit: crate::redaction::limit::RECORD,
        })?),
    };
    Ok(request
        .manifest
        .report_hash(buf)
        .is_ok_and(|h| h == record.report_hash))
}

/// Whether `residue` is the residue a span of `kind` and `hash` binds (shared with
/// `crate::sync_proof`): a block residue of that version, listed with its id among the
/// manifest's `versions`, or a change residue of that change that embeds one of
/// `version_hashes`.
pub(crate) fn residue_fits(
    kind: u8,
    hash: &Hash,
    residue: &RedactionResidue,
    versions: &BTreeSet<(BlockId, Hash)>,
    version_hashes: &HashSet<Hash>,
) -> bool {
    match residue {
        RedactionResidue::Block(b) => {
            kind == kinds::BLOCK && b.version == *hash && versions.contains(&(b.id.clone(), *hash))
        }
        RedactionResidue::Change(ch) => {
            kind == kinds::CHANGE
                && ch.change == *hash
                && residual_versions(ch).any(|v| version_hashes.contains(v))
        }
    }
}

/// One apply frame the header pass met in a sealed batch.
pub(crate) struct ApplyFrame {
    pub(crate) loc: Location,
    pub(crate) hash: Hash,
    /// None: its payload matched its hash but is no apply record.
    pub(crate) record: Option<RedactionApply>,
    /// The batch it belongs to (a counter in log order).
    batch: u64,
    /// The LANDING frames of its batch before it: (location, hash).
    landings: Vec<(Location, Hash)>,
}

impl ApplyFrame {
    /// The apply frame at `loc` with hash `hash` and its record (None: none decodes), of batch
    /// number `batch` (a counter in log order), after the `landings` of its batch.
    pub(crate) fn new(
        loc: Location,
        hash: Hash,
        record: Option<RedactionApply>,
        batch: u64,
        landings: Vec<(Location, Hash)>,
    ) -> ApplyFrame {
        ApplyFrame {
            loc,
            hash,
            record,
            batch,
            landings,
        }
    }
}

/// One carried record frame (kind REDACTION_CARRIED) the header pass met in a sealed batch.
pub(crate) struct CarriedFrame {
    pub(crate) loc: Location,
    pub(crate) hash: Hash,
    /// None: its payload matched its hash but is no carried record in its bounded, canonical
    /// form, or it did not match its hash.
    pub(crate) record: Option<rdy_model::RedactionCarried>,
    /// Where its batch starts: every span it names ends before it.
    pub(crate) batch_start: Location,
}

/// The records of one batch while its marker is not yet met. Only redaction records and the
/// batch's landings are kept: a plain store's header pass retains nothing per content frame.
#[derive(Default)]
struct Pending {
    since: Vec<(u64, Hash)>,
    start: Option<Location>,
    requests: Vec<(Location, Hash)>,
    residues: Vec<(Location, Hash)>,
    landings: Vec<(Location, Hash)>,
    applies: Vec<ApplyFrame>,
    carried: Vec<CarriedFrame>,
}

/// What the header pass collected from the sealed batches, in log order. It holds the redaction
/// records and the landings of apply batches, never a location per content frame (final review
/// finding 8): the spans' frame boundaries and the blob manifests are looked up by the log pass
/// ([`check_log`]) only when an apply exists.
#[derive(Default)]
pub(crate) struct Collected {
    pub(crate) requests: Vec<(Location, Hash)>,
    request_at: HashMap<Hash, Location>,
    pub(crate) residue_at: HashMap<Hash, Location>,
    pub(crate) applies: Vec<ApplyFrame>,
    /// The carried record frames of the sealed batches, in log order.
    pub(crate) carried: Vec<CarriedFrame>,
    /// Frames of a batch not yet written (the live writer's): kind and payload, by location.
    pub(crate) memory: BTreeMap<Location, (u8, Vec<u8>)>,
    batches: u64,
    /// The end of the last batch whose marker matched it.
    pub(crate) sealed_end: Option<Location>,
}

impl Collected {
    /// Notes the sealed request frame `hash` at `loc` by its first place (a source that is not a
    /// segment log, `crate::dump_scan`).
    pub(crate) fn note_request(&mut self, loc: Location, hash: Hash) {
        self.request_at.entry(hash).or_insert(loc);
    }

    /// Notes the sealed residue frame `hash` at `loc` by its first place.
    pub(crate) fn note_residue(&mut self, loc: Location, hash: Hash) {
        self.residue_at.entry(hash).or_insert(loc);
    }

    /// The first place of the request `hash`, if a sealed batch holds one.
    pub(crate) fn request_place(&self, hash: &Hash) -> Option<Location> {
        self.request_at.get(hash).copied()
    }

    /// The first place of the residue `hash`, if a sealed batch holds one.
    pub(crate) fn residue_place(&self, hash: &Hash) -> Option<Location> {
        self.residue_at.get(hash).copied()
    }

    fn seal(&mut self, p: Pending) {
        for (loc, h) in &p.requests {
            self.request_at.entry(*h).or_insert(*loc);
        }
        self.requests.extend(p.requests);
        for (loc, h) in p.residues {
            self.residue_at.entry(h).or_insert(loc);
        }
        self.applies.extend(p.applies);
        self.carried.extend(p.carried);
        self.batches += 1;
    }

    /// Notes one data frame of the current batch: its kind decides what is kept. An apply's or
    /// carried record's payload is given when it is hash-checked; one noted without it (over the
    /// record bound, or not matching its hash) is a broken record.
    fn note(&self, p: &mut Pending, loc: Location, kind: u8, hash: Hash, payload: Option<&[u8]>) {
        p.since.push((loc.offset, hash));
        let start = *p.start.get_or_insert(loc);
        match kind {
            kinds::REDACTION_APPLY => p.applies.push(ApplyFrame {
                loc,
                hash,
                record: payload.and_then(apply_record),
                batch: self.batches,
                landings: p.landings.clone(),
            }),
            kinds::REDACTION_CARRIED => p.carried.push(CarriedFrame {
                loc,
                hash,
                record: payload.and_then(crate::redaction_carried::decode_record),
                batch_start: start,
            }),
            kinds::LANDING => p.landings.push((loc, hash)),
            kinds::REDACTION => p.requests.push((loc, hash)),
            kinds::REDACTION_RESIDUE => p.residues.push((loc, hash)),
            _ => {}
        }
    }
}

/// A read window over one file: header reads of a sequential walk cost one positioned read per
/// `ERASE_BUFFER` bytes.
struct Window<'f> {
    f: &'f File,
    file_len: u64,
    buf: Vec<u8>,
    start: u64,
    filled: usize,
}

impl<'f> Window<'f> {
    fn new(f: &'f File, file_len: u64) -> Window<'f> {
        Window {
            f,
            file_len,
            buf: vec![0u8; ERASE_BUFFER],
            start: 0,
            filled: 0,
        }
    }

    /// The `n` bytes at `at` (`n` at most `ERASE_BUFFER`; the caller checked they are in the
    /// file).
    fn get(&mut self, at: u64, n: usize) -> io::Result<&[u8]> {
        let inside = at >= self.start && at + n as u64 <= self.start + self.filled as u64;
        if !inside {
            let want = (self.file_len - at).min(ERASE_BUFFER as u64) as usize;
            crate::pio::read_exact_at(self.f, &mut self.buf[..want], at)?;
            self.start = at;
            self.filled = want;
        }
        let from = (at - self.start) as usize;
        Ok(&self.buf[from..from + n])
    }
}

/// The one structural check of an apply payload, shared by the open and the live writer: at most
/// `MAX_RECORD_BYTES` bytes, and exactly the canonical encoding of the record it decodes to
/// (postcard ignores trailing bytes, so a decode alone accepts a padded payload). None: no apply
/// record.
pub(crate) fn apply_record(payload: &[u8]) -> Option<RedactionApply> {
    if payload.len() > MAX_RECORD_BYTES {
        return None;
    }
    let record = decode::<RedactionApply>(payload).ok()?;
    (encode(&record) == payload).then_some(record)
}

/// Refuses, before anything is written, a redaction record among `items` over the record bound
/// (`E_REDACTION_LIMIT`, `16777216 bytes per record`).
pub(crate) fn refuse_oversized_records(items: &[(u8, Vec<u8>)]) -> Result<(), StoreError> {
    if items
        .iter()
        .any(|(kind, payload)| is_redaction_kind(*kind) && payload.len() > MAX_RECORD_BYTES)
    {
        return Err(StoreError::RedactionLimit {
            limit: crate::redaction::limit::RECORD,
        });
    }
    Ok(())
}

/// Reads `len` payload bytes at `at` into a fallibly allocated buffer.
pub(crate) fn read_payload(f: &File, at: u64, len: u32) -> Result<Vec<u8>, StoreError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len as usize).map_err(|_| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "cannot allocate a record buffer",
        ))
    })?;
    buf.resize(len as usize, 0);
    crate::pio::read_exact_at(f, &mut buf, at)?;
    Ok(buf)
}

/// The header pass over every segment: frame headers with checked lengths and the batch markers.
/// Only apply payloads and markers are read and hashed; the other records are noted by place and
/// read when an apply needs them. A batch counts once its marker matches it (by the header
/// hashes).
///
/// It reads what the marker rule's scan reads, so the two passes never disagree on a batch:
/// - a marker of any length counts when it matches its hash and its batch (as the scan has it);
/// - an apply over `MAX_RECORD_BYTES`, or one that does not match its hash, is noted as a broken
///   apply (never read past the bound), which refuses the open if the acknowledged log holds it;
/// - a torn header or a marker that does not match ends the walk of the last segment, where the
///   scan stops too. In a sealed segment (one a later segment follows) it is damage and refuses
///   the open (`segment::sealed_damage`): the frames behind it could not be validated.
pub(crate) fn collect(readers: &[File]) -> Result<Collected, StoreError> {
    let mut c = Collected::default();
    let last = readers.len().saturating_sub(1);
    for (i, f) in readers.iter().enumerate() {
        let seg = i as u32;
        let file_len = f.metadata()?.len();
        let mut w = Window::new(f, file_len);
        let mut p = Pending::default();
        let mut offset = 0u64;
        while let Some(payload_start) = offset.checked_add(HEADER).filter(|e| *e <= file_len) {
            let h = w.get(offset, HEADER as usize)?;
            let len = u32::from_le_bytes(h[0..4].try_into().expect("four bytes"));
            let kind = h[4];
            let hash = Hash(h[5..37].try_into().expect("32 bytes"));
            let Some(end) = payload_start
                .checked_add(u64::from(len))
                .filter(|e| *e <= file_len)
            else {
                break;
            };
            let loc = Location {
                segment: seg,
                offset,
            };
            if kind == kinds::BATCH_COMMIT {
                let payload = if len as usize <= ERASE_BUFFER {
                    w.get(payload_start, len as usize)?.to_vec()
                } else {
                    read_payload(f, payload_start, len)?
                };
                if Hash::of(&payload) != hash || !segment::marker_matches(seg, &p.since, &payload) {
                    break;
                }
                c.seal(std::mem::take(&mut p));
                c.sealed_end = Some(Location {
                    segment: seg,
                    offset: end,
                });
            } else if kind == kinds::REDACTION_APPLY || kind == kinds::REDACTION_CARRIED {
                let payload = if len as usize > MAX_RECORD_BYTES {
                    None
                } else if len as usize <= ERASE_BUFFER {
                    Some(w.get(payload_start, len as usize)?.to_vec())
                } else {
                    Some(read_payload(f, payload_start, len)?)
                };
                let checked = payload.filter(|b| Hash::of(b) == hash);
                c.note(&mut p, loc, kind, hash, checked.as_deref());
            } else {
                c.note(&mut p, loc, kind, hash, None);
            }
            offset = end;
        }
        if i < last && offset < file_len {
            return Err(segment::sealed_damage(f, seg, offset, file_len));
        }
    }
    Ok(c)
}

/// The record of `kind` with hash `hash` at `loc`, read and hash-checked: from `memory` (a batch
/// not yet written) or the log. None when the frame there is not that record or does not match
/// its hash.
pub(crate) fn read_record(
    readers: &[File],
    memory: &BTreeMap<Location, (u8, Vec<u8>)>,
    loc: Location,
    kind: u8,
    hash: &Hash,
) -> Result<Option<Vec<u8>>, StoreError> {
    if let Some((k, p)) = memory.get(&loc) {
        return Ok((*k == kind && Hash::of(p) == *hash).then(|| p.clone()));
    }
    let Some(f) = readers.get(loc.segment as usize) else {
        return Ok(None);
    };
    let Some((len, k, h)) = header_at(f, loc.offset, f.metadata()?.len())? else {
        return Ok(None);
    };
    if k != kind || h != *hash {
        return Ok(None);
    }
    let payload = read_payload(f, loc.offset + HEADER, len)?;
    Ok((Hash::of(&payload) == *hash).then_some(payload))
}

/// Where the apply validator reads the records it needs (the proof source seam): a frame's
/// header by its place, and a record by its place, kind and hash. The log's segment files (with
/// a batch not yet written) are one source ([`LogAccess`]); the files of a dump chain are another
/// (`crate::dump_scan`). The validator's rules are the same over both.
pub(crate) trait RecordAccess {
    /// Whether a frame starts at `loc` whose header holds `len`, `kind` and `hash`.
    fn has_frame(
        &mut self,
        loc: Location,
        len: u32,
        kind: u8,
        hash: &Hash,
    ) -> Result<bool, StoreError>;

    /// The record of `kind` with hash `hash` at `loc`, read and hash-checked; None when the
    /// frame there is not that record or does not match its hash.
    fn record(
        &mut self,
        loc: Location,
        kind: u8,
        hash: &Hash,
    ) -> Result<Option<Vec<u8>>, StoreError>;
}

/// The log's segment files, and the frames of a batch not yet written, as a [`RecordAccess`].
pub(crate) struct LogAccess<'a> {
    pub(crate) readers: &'a [File],
    pub(crate) memory: &'a BTreeMap<Location, (u8, Vec<u8>)>,
}

impl RecordAccess for LogAccess<'_> {
    fn has_frame(
        &mut self,
        loc: Location,
        len: u32,
        kind: u8,
        hash: &Hash,
    ) -> Result<bool, StoreError> {
        let Some(f) = self.readers.get(loc.segment as usize) else {
            return Ok(false);
        };
        Ok(header_at(f, loc.offset, f.metadata()?.len())? == Some((len, kind, *hash)))
    }

    fn record(
        &mut self,
        loc: Location,
        kind: u8,
        hash: &Hash,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        read_record(self.readers, self.memory, loc, kind, hash)
    }
}

/// The request an apply names, if the log holds it sealed before the apply and it decodes.
fn request_of(
    c: &Collected,
    access: &mut dyn RecordAccess,
    a: &ApplyFrame,
) -> Result<Option<(Location, Redaction)>, StoreError> {
    let Some(record) = &a.record else {
        return Ok(None);
    };
    let Some(loc) = c.request_at.get(&record.request).filter(|l| **l < a.loc) else {
        return Ok(None);
    };
    let Some(payload) = access.record(*loc, kinds::REDACTION, &record.request)? else {
        return Ok(None);
    };
    Ok(decode::<Redaction>(&payload).ok().map(|r| (*loc, r)))
}

/// The holes `applies` would authorize, by their requests' spans alone, unchecked: what the scan
/// reads as frames while it decides which applies the acknowledged log holds.
fn candidate_holes(
    c: &Collected,
    readers: &[File],
    applies: &[&ApplyFrame],
) -> Result<Holes, StoreError> {
    let mut holes = Holes::new();
    let mut access = LogAccess {
        readers,
        memory: &c.memory,
    };
    for a in applies {
        if let Some((_, r)) = request_of(c, &mut access, a)? {
            for s in &r.manifest.spans {
                holes
                    .entry(Location {
                        segment: s.place.segment,
                        offset: s.place.offset,
                    })
                    .or_insert(Hole {
                        kind: s.kind,
                        hash: s.hash,
                        len: s.len,
                    });
            }
        }
    }
    Ok(holes)
}

/// What one validated apply's request selects, kept for the log pass ([`check_log`]).
struct Selected {
    /// Where the apply and its request are.
    apply: Location,
    request: Location,
    /// The manifest's versions, and the ids of its all-versions targets.
    versions: Vec<(BlockId, Hash)>,
    all_ids: Vec<BlockId>,
    /// The manifest's blobs.
    blobs: Vec<Hash>,
    /// Each span: its place, kind and hash.
    spans: Vec<(Location, u8, Hash)>,
}

/// Validates `applies` (in log order) against the collected records and returns what they
/// authorize, with what each one's request selects. Each apply needs: a record that decodes; in
/// its batch, applies in strictly ascending request order; the LANDING it names earlier in its
/// own batch, approved by a person (`human_approval`); its request sealed before it, whose
/// carrier the landing landed and whose report hash it binds; the request's bounds, canonical
/// order and selector expansion as the checked apply path has them (`check_bounds`,
/// `manifest_in_order`, `targets_agree`); and every span of that request valid: strictly
/// ascending, before the request, of a removable kind, a frame whose header holds the span's
/// kind, hash and length, a block span with the block residue of that version of a manifest
/// version, a change span with the change residue of that change embedding a manifest version,
/// a chunk span with no residue, and a frame an earlier apply authorized kept as it was.
///
/// That each span is a frame start, each chunk span a chunk of a manifest blob, and the rest of
/// the request's meaning against the log, are checked by [`check_log`].
///
/// A failure is `RedactionCorrupt`: at the span for a span failure, else at the apply. For an
/// apply at or after `pending_from` (a batch the live writer has not written yet), a landing no
/// person approved is `RedactionHuman`.
fn validate(
    c: &Collected,
    readers: &[File],
    applies: &[&ApplyFrame],
    pending_from: Option<Location>,
) -> Result<(Authorization, Vec<Selected>), StoreError> {
    let mut v = Validator::default();
    let mut access = LogAccess {
        readers,
        memory: &c.memory,
    };
    for a in applies {
        v.apply(c, &mut access, a, pending_from)?;
    }
    Ok((v.auth, v.selected))
}

/// The validation of applies in log order ([`validate`]), one apply at a time: what the applies
/// validated so far authorize, and what each one's request selects. A source that reads its
/// records as it walks them (a dump chain, `crate::dump_scan`) validates each apply when it meets
/// it; the rules are [`validate`]'s.
#[derive(Default)]
pub(crate) struct Validator {
    /// What the validated applies authorize: each span by the earliest apply that covers it.
    pub(crate) auth: Authorization,
    selected: Vec<Selected>,
    report_buf: Option<Vec<u8>>,
    prev_in_batch: Option<(u64, Hash)>,
}

impl Validator {
    /// Validates `a`, the next apply in log order, with its records read through `access` (see
    /// [`validate`]), and adds what it authorizes. After a failure the validator is not used
    /// again: a caller stops at the first failure.
    pub(crate) fn apply(
        &mut self,
        c: &Collected,
        access: &mut dyn RecordAccess,
        a: &ApplyFrame,
        pending_from: Option<Location>,
    ) -> Result<(), StoreError> {
        let auth = &mut self.auth;
        let at = || StoreError::corrupt_at(a.loc);
        let record = a.record.as_ref().ok_or_else(at)?;
        if self
            .prev_in_batch
            .is_some_and(|(b, r)| b == a.batch && r >= record.request)
        {
            return Err(at());
        }
        self.prev_in_batch = Some((a.batch, record.request));
        // the same-batch human landing
        let (lloc, _) = a
            .landings
            .iter()
            .find(|(_, h)| *h == record.landing)
            .ok_or_else(at)?;
        let landing: LandingRecord = decode(
            &access
                .record(*lloc, kinds::LANDING, &record.landing)?
                .ok_or_else(at)?,
        )
        .map_err(|_| at())?;
        if !human_approval(&landing) {
            return Err(if pending_from.is_some_and(|p| a.loc >= p) {
                StoreError::RedactionHuman
            } else {
                at()
            });
        }
        // the request, its carrier's landing and its report
        let (rloc, request) = request_of(c, access, a)?.ok_or_else(at)?;
        if !request_agrees(&request, &landing, record, &mut self.report_buf)? {
            return Err(at());
        }
        // its spans
        let versions: BTreeSet<(BlockId, Hash)> =
            request.manifest.versions.iter().cloned().collect();
        let version_hashes: HashSet<Hash> = versions.iter().map(|(_, v)| *v).collect();
        // each distinct residue of this request is read once
        let mut read: HashMap<Hash, RedactionResidue> = HashMap::new();
        let mut prev: Option<Location> = None;
        let mut spans = Vec::with_capacity(request.manifest.spans.len());
        for span in &request.manifest.spans {
            let loc = Location {
                segment: span.place.segment,
                offset: span.place.offset,
            };
            let bad = || StoreError::corrupt_at(loc);
            if prev.is_some_and(|p| p >= loc) || loc >= rloc || !is_removable(span.kind) {
                return Err(bad());
            }
            prev = Some(loc);
            if !access.has_frame(loc, span.len, span.kind, &span.hash)? {
                return Err(bad());
            }
            let residue = match span.kind {
                kinds::BLOCK | kinds::CHANGE => {
                    let r = span.residue.ok_or_else(bad)?;
                    let res = match read.get(&r) {
                        Some(res) => res.clone(),
                        None => {
                            let rloc = c
                                .residue_at
                                .get(&r)
                                .filter(|l| **l < a.loc)
                                .ok_or_else(bad)?;
                            let res: RedactionResidue = decode(
                                &access
                                    .record(*rloc, kinds::REDACTION_RESIDUE, &r)?
                                    .ok_or_else(bad)?,
                            )
                            .map_err(|_| bad())?;
                            read.insert(r, res.clone());
                            res
                        }
                    };
                    if !residue_fits(span.kind, &span.hash, &res, &versions, &version_hashes) {
                        return Err(bad());
                    }
                    Some((r, res))
                }
                _ => {
                    if span.residue.is_some() {
                        return Err(bad());
                    }
                    None
                }
            };
            let hole = Hole {
                kind: span.kind,
                hash: span.hash,
                len: span.len,
            };
            match auth.holes.get(&loc) {
                Some(h) => {
                    if *h != hole || auth.spans[&loc].residue != span.residue {
                        return Err(bad());
                    }
                }
                None => {
                    auth.holes.insert(loc, hole);
                    auth.spans.insert(
                        loc,
                        SpanAuth {
                            residue: span.residue,
                            apply: a.hash,
                            request: record.request,
                            carried: false,
                        },
                    );
                    if let Some((r, res)) = residue {
                        auth.residues.insert(r, res);
                    }
                }
            }
            spans.push((loc, span.kind, span.hash));
        }
        auth.applies.push(Applied {
            loc: a.loc,
            hash: a.hash,
            record: record.clone(),
        });
        self.selected.push(Selected {
            apply: a.loc,
            request: rloc,
            all_ids: request
                .targets
                .iter()
                .filter_map(|t| match t {
                    rdy_model::redaction::RedactTarget::AllVersions(id) => Some(id.clone()),
                    _ => None,
                })
                .collect(),
            versions: request.manifest.versions,
            blobs: request.manifest.blobs,
            spans,
        });
        Ok(())
    }

    /// Takes the span places of the last validated apply's request, in manifest order, and
    /// forgets the rest of what it selects: a source that runs no log pass ([`check_log`]) keeps
    /// one apply's selection at a time.
    pub(crate) fn take_last_spans(&mut self) -> Vec<Location> {
        self.selected
            .pop()
            .map(|s| s.spans.into_iter().map(|(l, _, _)| l).collect())
            .unwrap_or_default()
    }
}

/// One frame the log pass reads: its place, kind, hash and length, and how to read its payload.
pub(crate) struct Seen<'a> {
    pub(crate) loc: Location,
    pub(crate) kind: u8,
    pub(crate) hash: Hash,
    pub(crate) len: u32,
    source: Source<'a>,
}

/// Where a frame's payload is: in a segment file, or in a batch not yet written.
enum Source<'a> {
    File(&'a File),
    Memory(&'a [u8]),
}

impl Seen<'_> {
    /// The frame's payload, read through a fallibly allocated buffer.
    fn payload(&self) -> Result<Vec<u8>, StoreError> {
        match self.source {
            Source::File(f) => read_payload(f, self.loc.offset + HEADER, self.len),
            Source::Memory(p) => Ok(p.to_vec()),
        }
    }

    /// The payload when it is intact (it matches the frame's hash); None for an erased, partly
    /// erased or damaged one.
    pub(crate) fn intact(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let p = self.payload()?;
        Ok((Hash::of(&p) == self.hash).then_some(p))
    }
}

/// Calls `f` with every frame header of the log before `upto` (checked lengths; a torn frame
/// ends a segment's walk), then with every frame of `memory`, in log order. Payloads are read
/// only when `f` asks for them.
pub(crate) fn for_each_seen(
    readers: &[File],
    upto: Location,
    memory: &BTreeMap<Location, (u8, Vec<u8>)>,
    mut f: impl FnMut(&Seen<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    for (i, file) in readers.iter().enumerate() {
        let seg = i as u32;
        if seg > upto.segment {
            break;
        }
        let file_len = file.metadata()?.len();
        let stop = if seg == upto.segment {
            upto.offset.min(file_len)
        } else {
            file_len
        };
        let mut w = Window::new(file, file_len);
        let mut offset = 0u64;
        while let Some(payload_start) = offset.checked_add(HEADER).filter(|e| *e <= stop) {
            let h = w.get(offset, HEADER as usize)?;
            let len = u32::from_le_bytes(h[0..4].try_into().expect("four bytes"));
            let kind = h[4];
            let hash = Hash(h[5..37].try_into().expect("32 bytes"));
            let Some(end) = payload_start
                .checked_add(u64::from(len))
                .filter(|e| *e <= stop)
            else {
                break;
            };
            f(&Seen {
                loc: Location {
                    segment: seg,
                    offset,
                },
                kind,
                hash,
                len,
                source: Source::File(file),
            })?;
            offset = end;
        }
    }
    for (loc, (kind, payload)) in memory {
        f(&Seen {
            loc: *loc,
            kind: *kind,
            hash: Hash::of(payload),
            len: payload.len() as u32,
            source: Source::Memory(payload),
        })?;
    }
    Ok(())
}

/// The meaning of the validated applies against the log, which no index takes part in. Two
/// passes over the frame headers before `upto` (then the frames of `memory`, a batch not yet
/// written), both run whenever an apply exists, whatever the erasure has done (final review
/// fix round 2, findings 1 and 2: a span list that is empty or already zero proves nothing):
///
/// 1. Every authorized span is a frame start, every manifest blob has its manifest before its
///    request, and every chunk span is a chunk of one of its request's blobs. This pass stops at
///    the last request and keeps no location per content frame.
/// 2. The request means exactly what its manifest removes, and the removal destroys nothing
///    else, as the checked apply path proves it (`Store::check_selection`, `check_spans`,
///    `check_coverage`; final review findings 1 and 2):
///    - every manifest version is a stored version of the id it is listed with (read from its
///      intact payload, or from its block residue once erased);
///    - an all-versions target lists every version of its id stored before its apply;
///    - every intact payload of a span is exactly what its residue records: a block's identity,
///      a change's id, audit fields, dependencies and every op in order;
///    - every `(id, version)` a change residue keeps by reference is a stored block of that id
///      before its apply: an intact block frame whose payload has that id, a span an apply
///      authorized whose block residue has it, or a zero block frame that a validated carried
///      record explains, whose block residue has it;
///    - no intact frame outside the authorized spans holds selected content: a block of a
///      selected version, a change that embeds one, or a chunk of a selected blob (each
///      physical copy is a span);
///    - every chunk of a selected blob has a frame.
///
/// The holes of `auth` are the applies' spans; the carried records' spans (`SpanAuth::carried`,
/// laid over by `redaction_carried::overlay` before this check) take part only in the
/// resolution of a kept reference: they never count as a span of an apply, as a selected
/// version found, or as a copy the selection covers.
///
/// A failure is `RedactionCorrupt`: at the frame or span for a frame failure, else at the apply.
fn check_log(
    readers: &[File],
    memory: &BTreeMap<Location, (u8, Vec<u8>)>,
    auth: &Authorization,
    sel: &[Selected],
    upto: Location,
) -> Result<(), StoreError> {
    if sel.is_empty() {
        return Ok(());
    }
    // pass 1: span boundaries and blob manifests, up to the last request
    let last_request = sel.iter().map(|s| s.request).max().expect("an apply");
    let blobs: HashSet<Hash> = sel.iter().flat_map(|s| s.blobs.iter().copied()).collect();
    let mut chunks_of: HashMap<Hash, Vec<Hash>> = HashMap::new();
    let mut at_frame: BTreeSet<Location> = BTreeSet::new();
    for_each_seen(readers, last_request.min(upto), &BTreeMap::new(), |f| {
        if auth.erasable(&f.loc) {
            at_frame.insert(f.loc);
        }
        if f.kind == kinds::BLOB_MANIFEST
            && blobs.contains(&f.hash)
            && !chunks_of.contains_key(&f.hash)
            && let Some(p) = f.intact()?
            && let Ok(m) = decode::<BlobManifest>(&p)
        {
            chunks_of.insert(f.hash, m.chunks);
        }
        Ok(())
    })?;
    if let Some(loc) = auth
        .holes
        .keys()
        .find(|l| auth.erasable(l) && !at_frame.contains(l))
    {
        return Err(StoreError::corrupt_at(*loc));
    }
    let mut sel_chunks: Vec<HashSet<Hash>> = Vec::with_capacity(sel.len());
    for s in sel {
        let mut set = HashSet::new();
        for b in &s.blobs {
            set.extend(
                chunks_of
                    .get(b)
                    .ok_or_else(|| StoreError::corrupt_at(s.apply))?
                    .iter()
                    .copied(),
            );
        }
        if let Some((loc, ..)) = s
            .spans
            .iter()
            .find(|(_, kind, hash)| *kind == kinds::BLOB_CHUNK && !set.contains(hash))
        {
            return Err(StoreError::corrupt_at(*loc));
        }
        sel_chunks.push(set);
    }

    // pass 2: the request's meaning, and every physical copy
    let sel_versions: Vec<HashSet<Hash>> = sel
        .iter()
        .map(|s| s.versions.iter().map(|(_, v)| *v).collect())
        .collect();
    let selected_versions: HashSet<Hash> = sel_versions.iter().flatten().copied().collect();
    let mut by_id: HashMap<BlockId, Vec<usize>> = HashMap::new();
    for (i, s) in sel.iter().enumerate() {
        for id in &s.all_ids {
            by_id.entry(id.clone()).or_default().push(i);
        }
    }
    let selected_chunks: HashSet<Hash> = sel_chunks.iter().flatten().copied().collect();
    // the (id, version) pairs each change residue keeps by reference, with the earliest apply
    // that needs a stored block of it
    let mut kept: HashMap<(BlockId, Hash), Location> = HashMap::new();
    for (i, s) in sel.iter().enumerate() {
        for (loc, kind, _) in &s.spans {
            if *kind != kinds::CHANGE {
                continue;
            }
            let residue = auth.spans[loc].residue.and_then(|r| auth.residues.get(&r));
            let Some(RedactionResidue::Change(c)) = residue else {
                return Err(StoreError::corrupt_at(*loc));
            };
            for (id, v) in residual_refs(c).filter(|(_, v)| !sel_versions[i].contains(*v)) {
                let need = kept.entry((id.clone(), *v)).or_insert(s.apply);
                *need = (*need).min(s.apply);
            }
        }
    }
    let kept_versions: HashSet<Hash> = kept.keys().map(|(_, v)| *v).collect();
    let mut id_of: HashMap<Hash, BlockId> = HashMap::new();
    let mut kept_found: HashSet<(BlockId, Hash)> = HashSet::new();
    let mut seen_chunks: HashSet<Hash> = HashSet::new();
    let residue_of = |loc: &Location| auth.spans[loc].residue.and_then(|r| auth.residues.get(&r));
    // the id a carried record's block residue gives a zero frame it explains
    let carried_id = |loc: &Location| match auth.spans.get(loc) {
        Some(s) if s.carried => match s.residue.and_then(|r| auth.residues.get(&r)) {
            Some(RedactionResidue::Block(b)) => Some(b.id.clone()),
            _ => None,
        },
        _ => None,
    };
    for_each_seen(readers, upto, memory, |f| {
        let hole = auth.erasable(&f.loc);
        match f.kind {
            kinds::BLOCK => {
                if !selected_versions.contains(&f.hash)
                    && !kept_versions.contains(&f.hash)
                    && by_id.is_empty()
                {
                    return Ok(());
                }
                let id = match (hole, f.intact()?) {
                    (true, Some(p)) => {
                        let r = residue_of(&f.loc).ok_or_else(|| StoreError::corrupt_at(f.loc))?;
                        if !residue_matches(f.kind, &p, r) {
                            return Err(StoreError::corrupt_at(f.loc));
                        }
                        decode::<Block>(&p)
                            .map_err(|_| StoreError::corrupt_at(f.loc))?
                            .id
                    }
                    (true, None) => match residue_of(&f.loc) {
                        Some(RedactionResidue::Block(b)) => b.id.clone(),
                        _ => return Err(StoreError::corrupt_at(f.loc)),
                    },
                    (false, Some(p)) => {
                        if selected_versions.contains(&f.hash) {
                            // an intact copy of selected content that no span lists
                            return Err(StoreError::corrupt_at(f.loc));
                        }
                        match decode::<Block>(&p) {
                            Ok(b) => b.id,
                            Err(_) => return Ok(()),
                        }
                    }
                    // zeros a carried record explains: its block residue names the version a
                    // change residue may keep, and nothing else (no selection, no copy)
                    (false, None) => {
                        if let Some(id) = carried_id(&f.loc) {
                            let key = (id, f.hash);
                            if kept.get(&key).is_some_and(|need| f.loc < *need) {
                                kept_found.insert(key);
                            }
                        }
                        // otherwise a damaged payload outside the holes: the payload scan
                        // refuses it, and it keeps no reference
                        return Ok(());
                    }
                };
                if kept_versions.contains(&f.hash) {
                    let key = (id.clone(), f.hash);
                    if kept.get(&key).is_some_and(|need| f.loc < *need) {
                        kept_found.insert(key);
                    }
                }
                if selected_versions.contains(&f.hash) {
                    id_of.entry(f.hash).or_insert_with(|| id.clone());
                }
                for i in by_id.get(&id).into_iter().flatten() {
                    if f.loc < sel[*i].apply && !sel_versions[*i].contains(&f.hash) {
                        return Err(StoreError::corrupt_at(sel[*i].apply));
                    }
                }
            }
            kinds::CHANGE => {
                if hole {
                    if let Some(p) = f.intact()? {
                        let r = residue_of(&f.loc).ok_or_else(|| StoreError::corrupt_at(f.loc))?;
                        if !residue_matches(f.kind, &p, r) {
                            return Err(StoreError::corrupt_at(f.loc));
                        }
                    }
                } else if let Some(p) = f.intact()?
                    && let Ok(c) = decode::<Change>(&p)
                    && c.ops.iter().any(|op| match op {
                        Op::Create { block } | Op::Update { block, .. } => {
                            selected_versions.contains(&block.version_hash())
                        }
                        Op::Delete { .. } => false,
                    })
                {
                    // an intact change that embeds a selected version and no span lists
                    return Err(StoreError::corrupt_at(f.loc));
                }
            }
            kinds::BLOB_CHUNK if selected_chunks.contains(&f.hash) => {
                seen_chunks.insert(f.hash);
                if !hole {
                    return Err(StoreError::corrupt_at(f.loc));
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    for (i, s) in sel.iter().enumerate() {
        let wrong = s.versions.iter().any(|(id, v)| id_of.get(v) != Some(id))
            || sel_chunks[i].iter().any(|c| !seen_chunks.contains(c));
        if wrong {
            return Err(StoreError::corrupt_at(s.apply));
        }
    }
    if let Some((_, need)) = kept.iter().find(|(r, _)| !kept_found.contains(*r)) {
        return Err(StoreError::corrupt_at(*need));
    }
    Ok(())
}

/// The authorization bootstrap of an open: the header pass, then the validation of every apply
/// the acknowledged log holds. Which applies that is depends on the holes they authorize (the
/// marker rule's scan reads them as frames), so the scan's end is found with the applies'
/// unchecked holes (with the carried records' `held: false` spans) and applies and carried
/// records past it are left out until it is stable; the applies left are validated, the carried
/// records left are laid over them (`redaction_carried::overlay`), and the applies are checked
/// against the log (`check_log`, both its passes, whatever the erasure has done; a kept reference
/// may resolve through a carried residue), and any failure refuses the open. The index plays no part. A load checks a complete ingested log with it before it
/// finishes the store.
pub(crate) fn bootstrap(readers: &[File]) -> Result<Authorization, StoreError> {
    let c = collect(readers)?;
    if c.applies.is_empty() && c.requests.is_empty() && c.carried.is_empty() {
        return Ok(Authorization::default());
    }
    let mut applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    let mut below = Location {
        segment: u32::MAX,
        offset: u64::MAX,
    };
    let end = loop {
        let mut holes = candidate_holes(&c, readers, &applies)?;
        crate::redaction_carried::candidate_holes(&c, below, &mut holes);
        let end = segment::acknowledged_end(readers, &holes)?;
        let before = (applies.len(), carried_below(&c, below));
        applies.retain(|a| a.loc < end);
        below = end;
        if (applies.len(), carried_below(&c, below)) == before {
            break end;
        }
    };
    let (mut auth, sel) = validate(&c, readers, &applies, None)?;
    crate::redaction_carried::overlay(&c, readers, &mut auth, end)?;
    check_log(readers, &c.memory, &auth, &sel, end)?;
    auth.requests = c
        .requests
        .iter()
        .filter(|(l, _)| *l < end)
        .copied()
        .collect();
    Ok(auth)
}

/// How many carried record frames lie before `below`.
fn carried_below(c: &Collected, below: Location) -> usize {
    c.carried.iter().filter(|f| f.loc < below).count()
}

/// The authorization of a log whose every batch is whole (a load's ingest checked each batch
/// against its digest before it wrote its marker): the header pass, then the validation of every
/// apply of its sealed batches, as `bootstrap` validates them, and checked against the sealed
/// batches (`check_log`), with its carried records over them. Frames after the last marker (a
/// batch not yet sealed) play no part. A log with no apply and no carried record authorizes
/// nothing. Any failed apply or carried record refuses it, as it refuses an open.
pub(crate) fn bootstrap_sealed(readers: &[File]) -> Result<Authorization, StoreError> {
    let c = collect(readers)?;
    let upto = c.sealed_end.unwrap_or(Location {
        segment: 0,
        offset: 0,
    });
    sealed_of(&c, readers, upto)
}

/// The authorization of the sealed batches `c` collected, as [`bootstrap_sealed`] has it: the
/// validated applies, the carried records before `carried_upto` over them (a batch of
/// `c.memory` included), then the applies checked against the sealed log on disk.
pub(crate) fn sealed_of(
    c: &Collected,
    readers: &[File],
    carried_upto: Location,
) -> Result<Authorization, StoreError> {
    if c.applies.is_empty() && c.carried.is_empty() {
        return Ok(Authorization::default());
    }
    let applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    let (mut auth, sel) = validate(c, readers, &applies, None)?;
    crate::redaction_carried::overlay(c, readers, &mut auth, carried_upto)?;
    let upto = c.sealed_end.unwrap_or(Location {
        segment: 0,
        offset: 0,
    });
    check_log(readers, &BTreeMap::new(), &auth, &sel, upto)?;
    Ok(auth)
}

/// The bootstrap of a batch the live writer is about to seal: the header pass over the open log
/// (an open held batch on disk has no marker yet and does not count), then `batch` as one sealed
/// batch whose first frame goes at `start`, then the validation of every apply and its check
/// against the log up to `start` and the batch (`check_log`). Refuses before anything is
/// written: `RedactionHuman` for a new apply no person approved, `RedactionCorrupt` for any
/// other failure.
pub(crate) fn bootstrap_pending(
    readers: &[File],
    start: Location,
    batch: &[(u8, Vec<u8>)],
) -> Result<Authorization, StoreError> {
    let mut c = collect(readers)?;
    c.push_batch(start, batch);
    let applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    let (mut auth, sel) = validate(&c, readers, &applies, Some(start))?;
    crate::redaction_carried::overlay(&c, readers, &mut auth, start)?;
    check_log(readers, &c.memory, &auth, &sel, start)?;
    auth.requests = c.requests.clone();
    Ok(auth)
}

impl Collected {
    /// Notes `batch` as one sealed batch not yet written, whose first frame goes at `start`: its
    /// frames are kept in `memory`, and the end of the sealed log moves past it (its marker is
    /// not counted). Returns the place after its last data frame.
    pub(crate) fn push_batch(&mut self, start: Location, batch: &[(u8, Vec<u8>)]) -> Location {
        let mut p = Pending::default();
        let mut at = start;
        for (kind, payload) in batch {
            let hash = Hash::of(payload);
            self.note(&mut p, at, *kind, hash, Some(payload));
            self.memory.insert(at, (*kind, payload.clone()));
            at.offset += HEADER + payload.len() as u64;
        }
        self.seal(p);
        at
    }
}

/// The span `loc` of `auth` as a `DropSpan` for `SegmentLog::zero_payload`.
pub(crate) fn drop_span(
    loc: Location,
    hole: &Hole,
    auth: &SpanAuth,
) -> rdy_model::redaction::DropSpan {
    rdy_model::redaction::DropSpan {
        place: rdy_model::redaction::FramePlace {
            segment: loc.segment,
            offset: loc.offset,
        },
        kind: hole.kind,
        hash: hole.hash,
        len: hole.len,
        residue: auth.residue,
    }
}
