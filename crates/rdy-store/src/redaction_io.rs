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
//! The scope is the active store files. File-system snapshots, backups, responses already sent,
//! old dumps and media remnants are outside it (`W_REDACTION_COPIES`); quarantine files are
//! possible copies (F2(c)), and nothing here lists or removes them.
use crate::error::StoreError;
use crate::index::IndexEntry;
use crate::kinds::{self, BlobManifest};
use crate::redaction::{is_redaction_kind, is_removable};
use crate::segment::{self, HEADER, Hole, Holes, Location, header_at};
use crate::store::{decode, encode};
use rdy_model::redaction::{
    MAX_RECORD_BYTES, Redaction, RedactionApply, RedactionResidue, ResidualOp, record_buffer,
};
use rdy_model::{Assurance, BlockId, Hash, LandingRecord, Role};
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

/// How one authorized span is known: the residue that replaces it for reads, and the apply and
/// request that authorize it (the earliest apply in log order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpanAuth {
    pub(crate) residue: Option<Hash>,
    pub(crate) apply: Hash,
    pub(crate) request: Hash,
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
}

impl Authorization {
    /// Whether an active redaction exists: a validated apply.
    pub(crate) fn is_active(&self) -> bool {
        !self.applies.is_empty()
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
            at.push((*loc, crate::redaction::request_entry(*hash, &payload)?));
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
        at.sort_by_key(|(l, _)| *l);
        Ok(at.into_iter().map(|(_, e)| e).collect())
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

/// One apply frame the header pass met in a sealed batch.
struct ApplyFrame {
    loc: Location,
    hash: Hash,
    /// None: its payload matched its hash but is no apply record.
    record: Option<RedactionApply>,
    /// The batch it belongs to (a counter in log order).
    batch: u64,
    /// The LANDING frames of its batch before it: (location, hash).
    landings: Vec<(Location, Hash)>,
}

/// The records of one batch while its marker is not yet met.
#[derive(Default)]
struct Pending {
    since: Vec<(u64, Hash)>,
    requests: Vec<(Location, Hash)>,
    residues: Vec<(Location, Hash)>,
    manifests: Vec<(Location, Hash)>,
    removable: Vec<Location>,
    landings: Vec<(Location, Hash)>,
    applies: Vec<ApplyFrame>,
}

/// What the header pass collected from the sealed batches, in log order.
#[derive(Default)]
struct Collected {
    requests: Vec<(Location, Hash)>,
    request_at: HashMap<Hash, Location>,
    residue_at: HashMap<Hash, Location>,
    manifest_at: HashMap<Hash, Location>,
    /// The start of every removable frame (BLOCK, CHANGE, BLOB_CHUNK), sorted.
    removable: Vec<Location>,
    applies: Vec<ApplyFrame>,
    /// Payloads of a batch not yet written (the live writer's), by location.
    memory: BTreeMap<Location, Vec<u8>>,
    batches: u64,
}

impl Collected {
    fn seal(&mut self, p: Pending) {
        for (loc, h) in &p.requests {
            self.request_at.entry(*h).or_insert(*loc);
        }
        self.requests.extend(p.requests);
        for (loc, h) in p.residues {
            self.residue_at.entry(h).or_insert(loc);
        }
        for (loc, h) in p.manifests {
            self.manifest_at.entry(h).or_insert(loc);
        }
        self.removable.extend(p.removable);
        self.applies.extend(p.applies);
        self.batches += 1;
    }

    /// Notes one data frame of the current batch: its kind decides what is kept. An apply's
    /// payload is given when it is hash-checked; an apply noted without one (over the record
    /// bound, or not matching its hash) is a broken record.
    fn note(&self, p: &mut Pending, loc: Location, kind: u8, hash: Hash, apply: Option<&[u8]>) {
        p.since.push((loc.offset, hash));
        match kind {
            kinds::REDACTION_APPLY => p.applies.push(ApplyFrame {
                loc,
                hash,
                record: apply.and_then(apply_record),
                batch: self.batches,
                landings: p.landings.clone(),
            }),
            kinds::LANDING => p.landings.push((loc, hash)),
            kinds::REDACTION => p.requests.push((loc, hash)),
            kinds::REDACTION_RESIDUE => p.residues.push((loc, hash)),
            kinds::BLOB_MANIFEST => p.manifests.push((loc, hash)),
            k if is_removable(k) => p.removable.push(loc),
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
fn apply_record(payload: &[u8]) -> Option<RedactionApply> {
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
fn read_payload(f: &File, at: u64, len: u32) -> Result<Vec<u8>, StoreError> {
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
fn collect(readers: &[File]) -> Result<Collected, StoreError> {
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
            } else if kind == kinds::REDACTION_APPLY {
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
fn read_record(
    readers: &[File],
    memory: &BTreeMap<Location, Vec<u8>>,
    loc: Location,
    kind: u8,
    hash: &Hash,
) -> Result<Option<Vec<u8>>, StoreError> {
    if let Some(p) = memory.get(&loc) {
        return Ok((Hash::of(p) == *hash).then(|| p.clone()));
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

/// The request an apply names, if the log holds it sealed before the apply and it decodes.
fn request_of(
    c: &Collected,
    readers: &[File],
    a: &ApplyFrame,
) -> Result<Option<(Location, Redaction)>, StoreError> {
    let Some(record) = &a.record else {
        return Ok(None);
    };
    let Some(loc) = c.request_at.get(&record.request).filter(|l| **l < a.loc) else {
        return Ok(None);
    };
    let Some(payload) = read_record(readers, &c.memory, *loc, kinds::REDACTION, &record.request)?
    else {
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
    for a in applies {
        if let Some((_, r)) = request_of(c, readers, a)? {
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

/// Validates `applies` (in log order) against the collected records and returns what they
/// authorize. Each apply needs: a record that decodes; in its batch, applies in strictly
/// ascending request order; the LANDING it names earlier in its own batch, approved by a person
/// (`human_approval`); its request sealed before it, whose carrier the landing landed and whose
/// report hash it binds; and every span of that request valid: strictly ascending, before the
/// request, a removable frame start whose header holds the span's kind, hash and length, a block
/// span with the block residue of that version of a manifest version, a change span with the
/// change residue of that change embedding a manifest version, a chunk span with no residue and
/// a chunk of a manifest blob, and a frame an earlier apply authorized kept as it was.
///
/// A failure is `RedactionCorrupt`: at the span for a span failure, else at the apply. For an
/// apply at or after `pending_from` (a batch the live writer has not written yet), a landing no
/// person approved is `RedactionHuman`.
fn validate(
    c: &Collected,
    readers: &[File],
    applies: &[&ApplyFrame],
    pending_from: Option<Location>,
) -> Result<Authorization, StoreError> {
    let mut auth = Authorization::default();
    let mut report_buf: Option<Vec<u8>> = None;
    let mut prev_in_batch: Option<(u64, Hash)> = None;
    for a in applies {
        let at = || StoreError::corrupt_at(a.loc);
        let record = a.record.as_ref().ok_or_else(at)?;
        if prev_in_batch.is_some_and(|(b, r)| b == a.batch && r >= record.request) {
            return Err(at());
        }
        prev_in_batch = Some((a.batch, record.request));
        // the same-batch human landing
        let (lloc, _) = a
            .landings
            .iter()
            .find(|(_, h)| *h == record.landing)
            .ok_or_else(at)?;
        let landing: LandingRecord = decode(
            &read_record(readers, &c.memory, *lloc, kinds::LANDING, &record.landing)?
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
        let (rloc, request) = request_of(c, readers, a)?.ok_or_else(at)?;
        if !landing.changes.contains(&request.carrier) {
            return Err(at());
        }
        let buf = match &mut report_buf {
            Some(b) => b,
            None => report_buf.insert(record_buffer().map_err(|_| StoreError::RedactionLimit {
                limit: crate::redaction::limit::RECORD,
            })?),
        };
        if request.manifest.report_hash(buf).map_err(|_| at())? != record.report_hash {
            return Err(at());
        }
        // its spans
        let versions: BTreeSet<(BlockId, Hash)> =
            request.manifest.versions.iter().cloned().collect();
        let version_hashes: HashSet<Hash> = versions.iter().map(|(_, v)| *v).collect();
        let mut chunks: Option<HashSet<Hash>> = None;
        let mut prev: Option<Location> = None;
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
            if c.removable.binary_search(&loc).is_err() {
                return Err(bad());
            }
            let f = readers.get(loc.segment as usize).ok_or_else(bad)?;
            if header_at(f, loc.offset, f.metadata()?.len())?
                != Some((span.len, span.kind, span.hash))
            {
                return Err(bad());
            }
            let residue = match span.kind {
                kinds::BLOCK | kinds::CHANGE => {
                    let r = span.residue.ok_or_else(bad)?;
                    let rloc = c
                        .residue_at
                        .get(&r)
                        .filter(|l| **l < a.loc)
                        .ok_or_else(bad)?;
                    let res: RedactionResidue = decode(
                        &read_record(readers, &c.memory, *rloc, kinds::REDACTION_RESIDUE, &r)?
                            .ok_or_else(bad)?,
                    )
                    .map_err(|_| bad())?;
                    let fits = match &res {
                        RedactionResidue::Block(b) => {
                            span.kind == kinds::BLOCK
                                && b.version == span.hash
                                && versions.contains(&(b.id.clone(), span.hash))
                        }
                        RedactionResidue::Change(ch) => {
                            span.kind == kinds::CHANGE
                                && ch.change == span.hash
                                && ch.ops.iter().any(|op| match op {
                                    ResidualOp::Create { version, .. }
                                    | ResidualOp::Update { version, .. } => {
                                        version_hashes.contains(version)
                                    }
                                    ResidualOp::Delete { .. } => false,
                                })
                        }
                    };
                    if !fits {
                        return Err(bad());
                    }
                    Some((r, res))
                }
                _ => {
                    if span.residue.is_some() {
                        return Err(bad());
                    }
                    if chunks.is_none() {
                        let mut set = HashSet::new();
                        for b in &request.manifest.blobs {
                            let Some(mloc) = c.manifest_at.get(b) else {
                                continue;
                            };
                            if let Some(p) =
                                read_record(readers, &c.memory, *mloc, kinds::BLOB_MANIFEST, b)?
                                && let Ok(m) = decode::<BlobManifest>(&p)
                            {
                                set.extend(m.chunks);
                            }
                        }
                        chunks = Some(set);
                    }
                    if !chunks.as_ref().is_some_and(|s| s.contains(&span.hash)) {
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
                        },
                    );
                    if let Some((r, res)) = residue {
                        auth.residues.insert(r, res);
                    }
                }
            }
        }
        auth.applies.push(Applied {
            loc: a.loc,
            hash: a.hash,
            record: record.clone(),
        });
    }
    Ok(auth)
}

/// The authorization bootstrap of an open: the header pass, then the validation of every apply
/// the acknowledged log holds. Which applies that is depends on the holes they authorize (the
/// marker rule's scan reads them as frames), so the scan's end is found with the applies'
/// unchecked holes and applies past it are left out until it is stable; the applies left are
/// validated, and any failure refuses the open. The index plays no part.
pub(crate) fn bootstrap(readers: &[File]) -> Result<Authorization, StoreError> {
    let c = collect(readers)?;
    if c.applies.is_empty() && c.requests.is_empty() {
        return Ok(Authorization::default());
    }
    let mut applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    let end = loop {
        let holes = candidate_holes(&c, readers, &applies)?;
        let end = segment::acknowledged_end(readers, &holes)?;
        let before = applies.len();
        applies.retain(|a| a.loc < end);
        if applies.len() == before {
            break end;
        }
    };
    let mut auth = validate(&c, readers, &applies, None)?;
    auth.requests = c
        .requests
        .iter()
        .filter(|(l, _)| *l < end)
        .copied()
        .collect();
    Ok(auth)
}

/// The authorization of a log whose every batch is whole (a load's ingest checked each batch
/// against its digest before it wrote its marker): the header pass, then the validation of every
/// apply of its sealed batches, as `bootstrap` validates them. Frames after the last marker (a
/// batch not yet sealed) play no part. A log with no apply authorizes nothing. Any failed apply
/// refuses it, as it refuses an open.
pub(crate) fn bootstrap_sealed(readers: &[File]) -> Result<Authorization, StoreError> {
    let c = collect(readers)?;
    if c.applies.is_empty() {
        return Ok(Authorization::default());
    }
    let applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    validate(&c, readers, &applies, None)
}

/// The bootstrap of a batch the live writer is about to seal: the header pass over the open log
/// (an open held batch on disk has no marker yet and does not count), then `batch` as one sealed
/// batch whose first frame goes at `start`, then the validation of every apply. Refuses before
/// anything is written: `RedactionHuman` for a new apply no person approved, `RedactionCorrupt`
/// for any other failure.
pub(crate) fn bootstrap_pending(
    readers: &[File],
    start: Location,
    batch: &[(u8, Vec<u8>)],
) -> Result<Authorization, StoreError> {
    let mut c = collect(readers)?;
    let mut p = Pending::default();
    let mut at = start;
    for (kind, payload) in batch {
        let hash = Hash::of(payload);
        c.note(&mut p, at, *kind, hash, Some(payload));
        c.memory.insert(at, payload.clone());
        at.offset += HEADER + payload.len() as u64;
    }
    c.seal(p);
    let applies: Vec<&ApplyFrame> = c.applies.iter().collect();
    let mut auth = validate(&c, readers, &applies, Some(start))?;
    auth.requests = c.requests.clone();
    Ok(auth)
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
