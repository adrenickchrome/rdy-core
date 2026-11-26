//! The upload of one tag's history to a hub push (D-Q0AVZ "closure walk", D-D2NPN; hub i-SG442,
//! i-QD955, tb-5TB84, r-79KGK). The history is the tag's full source history, private to the hub
//! repo (D-48NYM, D-VP42Y): never a filtered publication artifact, a dump or a project file.
//!
//! - **Capture.** [`PreparedHistory::capture`] writes HA1's complete export of the tag
//!   (`stream_export::write`, nothing known) to one anonymous temporary file, then checks it as
//!   a stream render does (`CheckedStream`). The source's redaction epoch is checked before and
//!   after. The file is unlinked when it is made, is never named and closes with the history.
//!   HA1's dependency order is kept apart from the checked stream's sorted lookup keys; each
//!   record's needs are read once, through HA1's `refs_of` and its dropped-proof edge (a
//!   placeholder's apply), with each kind-None edge resolved to the one record of its hash.
//! - **Known.** [`missing_keys`] asks the hub which keys it lacks, in chunks of at most 100,000
//!   sorted keys, and accepts each reply only as a sorted, unique subset of its chunk. A key is a
//!   kind and a hash: holding one kind of a hash says nothing of another.
//! - **What is sent.** Every removal placeholder, and the forward closure of each placeholder's
//!   apply and residue, always travels: a held original hash does not prove that the hub applied
//!   its removal, and an old tag still carries its later removal proofs. Any other record the hub
//!   holds is left out.
//! - **Batches.** The records left go in HA1's dependency order. Each batch is the longest next
//!   prefix whose encoded bytes (prelude, header, frames and trailer) and records fit the
//!   negotiated limits and whose prerequisites are at most 100,000: the exact distinct keys its
//!   records need that it does not hold, and the root tag when it does not hold it, in key
//!   order. Each is written by HA1's stream writer, which never cuts a frame. When nothing needs
//!   sending, one batch of no record names the root as its prerequisite: it opens the push's
//!   quarantine as the protocol has it. The whole upload stays within 1,024 batches,
//!   1,000,000 records and 2,147,483,648 bytes. Every batch is planned before the first is sent,
//!   so a record that fits no batch is `E_LIMIT` before anything is sent.
//! - **Requests.** The caller's context is asked for immediately before every known and objects
//!   request (so it can renew access and recheck the epoch), and every request names the one push
//!   id. A failure ends the upload as it is: nothing is retried, no other push id is chosen and
//!   no quarantine is deleted. A wire error, an uncertain one included, is returned unchanged.
//!   A reply outside its contract is `E_UPSTREAM_PROTOCOL`.
//!
//! The sync exchange sends a canon history the same way ([`PreparedHistory::canon`],
//! [`send_objects`]; D-H0BZN, D-KE83X): its records are Plan A's closure from the head above the
//! hub tip, read from the captured state, and the keys it names but does not hold are
//! prerequisites that never travel. When the hub tip is the head, the closure holds only the
//! attachments made since (later tags, signatures and answers), and every batch names the head
//! as a prerequisite. Each format-4 batch passes Plan A's batch check before it is sent. When
//! nothing is missing, no batch is sent; a push or restore whose write still moves canon or a
//! view then sends one batch of no record that names the root ([`send_root`]).
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_IO, E_LIMIT, RdyError, upstream_protocol};
use crate::hub_http::{BodyEncoding, zstd_frame};
use crate::hub_publish_wire::{
    CORE_BATCH_BYTES, CORE_BATCH_RECORDS, MAX_KNOWN_KEYS, ObjectPut, PUSH_ID_CHARS, PublishWire,
    UploadContext,
};
use crate::read_state::ReadState;
use crate::stream_error::{StreamError, to_rdy};
use crate::stream_export;
use crate::stream_take::uncovered_change;
use rdy_model::{CROCKFORD, Hash, RepoId};
use rdy_store::closure::{StoreSource, prepared_history_closure_before};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::{LIMIT_OBJECT_RECORDS, LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES};
use rdy_store::stream_source::CheckedStream;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{
    HEADER_FIXED, KEY_LEN, LIMIT_BATCH_FILES, MAGIC, MAX_BATCH_FILES, MAX_PREREQUISITES,
    MAX_RECORDS, MAX_STREAM_BYTES, StreamHeader, StreamPurpose, StreamRecord, StreamWriter,
    TRAILER_LEN,
};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};

/// The bytes of a stream frame's own header: length, kind and hash.
const FRAME_HEADER: u64 = 4 + 1 + 32;
/// The bytes of the prelude and the header frame with its fixed payload, besides the header's
/// feature manifest ([`PreparedHistory::manifest`]).
const BATCH_HEAD: u64 = MAGIC.len() as u64 + 4 + FRAME_HEADER + HEADER_FIXED as u64;
/// The bytes of a batch beyond its manifest, prerequisite keys and object frames: [`BATCH_HEAD`] and the
/// trailer frame.
const BATCH_FIXED: u64 = BATCH_HEAD + FRAME_HEADER + TRAILER_LEN as u64;

/// A push id (hub D-6BXJY, i-QD955; D-502J1): 128 random bits as one unsigned big-endian
/// integer in 26 lower-case Crockford base32 digits. The two leading bits are zero, so the
/// first digit is 0 to 7.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PushId(String);

impl PushId {
    /// A new push id from 16 random bytes: the repo-id byte source (on a seeded test thread, its
    /// own seeded stream, which never moves the `BlockId` stream). No repo id is made or stored.
    // a default push id would be a random one, which `Default` should not hide
    #[allow(clippy::new_without_default)]
    pub fn new() -> PushId {
        PushId::from_bytes(RepoId::new().0)
    }

    /// The push id of the 16 bytes `bytes`.
    pub fn from_bytes(bytes: [u8; 16]) -> PushId {
        let value = u128::from_be_bytes(bytes);
        let digits = (0..PUSH_ID_CHARS)
            .rev()
            .map(|i| char::from(CROCKFORD[((value >> (5 * i)) & 31) as usize].to_ascii_lowercase()))
            .collect();
        PushId(digits)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The push id `text` names: 26 lower-case Crockford base32 digits, the first 0 to 7.
    /// `None` for any other text.
    pub fn parse(text: &str) -> Option<PushId> {
        let digit = |b: u8| CROCKFORD.iter().any(|c| c.to_ascii_lowercase() == b);
        let ok = text.len() == PUSH_ID_CHARS
            && text.bytes().all(digit)
            && text
                .as_bytes()
                .first()
                .is_some_and(|b| (b'0'..=b'7').contains(b));
        ok.then(|| PushId(text.to_string()))
    }
}

/// The hub's batch limits from its state reply: the encoded bytes and the object records of one
/// batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchLimits {
    pub bytes: u64,
    pub records: u64,
}

/// What the hub acknowledged: the batches, their object records and their encoded bytes,
/// framing included. It describes successful sends, not new hub records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistorySent {
    pub batches: u32,
    pub records: u64,
    pub bytes: u64,
}

/// Where a prepared history's records are read: the checked export of a tag, or the captured
/// state a canon history's closure was taken from.
///
/// The checked stream reads through a `RefCell`, so it sits behind a mutex: the upload's one
/// preparation worker and the caller may both read the history. Its lock is held only for one
/// read of its facts or one checked, positioned record read; a poisoned lock is recovered for
/// those reads alone (a worker's panic still fails the upload).
enum Source {
    Checked(Mutex<Box<CheckedStream<File>>>),
    State(Arc<ReadState>),
}

/// The checked stream behind `m`, a poisoned lock recovered (see [`Source`]).
fn checked(m: &Mutex<Box<CheckedStream<File>>>) -> MutexGuard<'_, Box<CheckedStream<File>>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One tag's complete history, exported and checked, or one canon history's closure, ready to
/// send (see the module docs).
pub struct PreparedHistory {
    source: Source,
    /// The stream's purpose: a tag history or a canon history.
    purpose: StreamPurpose,
    /// The places at the end of `order` that are prerequisites only: keys the records need
    /// that the history leaves to the receiver. They are never sent.
    externals: usize,
    /// The source store's features, which every batch's header names.
    features: rdy_store::format::StoreFeatures,
    /// The bytes of `features`' canonical JSON in every batch's header.
    manifest: u64,
    repo_id: RepoId,
    tag: Hash,
    /// HA1's records in dependency order.
    order: Vec<ObjectKey>,
    /// Each record's frame payload bytes, by its place in `order`.
    len: Vec<u64>,
    /// Each record's needs, by place: `deps[starts[i]..starts[i + 1]]`, sorted, no repeats.
    starts: Vec<usize>,
    deps: Vec<u32>,
    /// Whether the record always travels: a placeholder or a proof a placeholder needs.
    retained: Vec<bool>,
    /// The root's place in `order`: the tag, or the canon head landing.
    root: u32,
}

impl std::fmt::Debug for PreparedHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedHistory")
            .field("tag", &self.tag)
            .field("records", &self.order.len())
            .finish_non_exhaustive()
    }
}

/// E_IO: the upload scratch file could not be made, written or read.
pub(crate) fn scratch_error() -> RdyError {
    RdyError::new(
        E_IO,
        "the upload scratch file could not be used",
        "check the temporary directory and available disk space",
    )
}

/// A stream error of a scratch read or write as the scratch error; any other as the engine's.
fn from_stream(e: StreamError) -> RdyError {
    if e.code == E_IO {
        return scratch_error();
    }
    to_rdy(e)
}

/// E_INTERNAL: the checked stream disagrees with the export that wrote it.
fn inconsistent() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "the exported history does not match its checked stream",
        "report this as a bug in rdy",
    )
}

/// E_LIMIT naming HA1's stream limit `label`.
fn upload_limit(label: &str) -> RdyError {
    to_rdy(StreamError::limit(None, 0, label))
}

impl PreparedHistory {
    /// Exports the history of the tag `tag_hash`, which `read` captured, to an anonymous
    /// temporary file and checks it (see the module docs).
    pub fn capture(read: &ReadState, tag_hash: Hash) -> Result<PreparedHistory, RdyError> {
        read.ensure_current_epoch()?;
        let mut file = tempfile::tempfile().map_err(|_| scratch_error())?;
        let report = {
            let mut out = BufWriter::new(&mut file);
            let report = stream_export::write(read, tag_hash, &BTreeSet::new(), &mut out)?;
            out.flush().map_err(|_| scratch_error())?;
            report
        };
        let stream = CheckedStream::open(file).map_err(from_stream)?;
        // the take's step 7: the hub's take would refuse the upload
        if let Some(h) = stream.uncovered_change() {
            return Err(uncovered_change(h));
        }
        let facts = stream.facts();
        if facts.root_hash != tag_hash
            || !report.header.prerequisites.is_empty()
            || report.keys.len() != facts.objects.len()
        {
            return Err(inconsistent());
        }
        let order = report.keys;
        let place: HashMap<ObjectKey, u32> = order
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, i as u32))
            .collect();
        let at = |k: &ObjectKey| place.get(k).copied().ok_or_else(inconsistent);
        let root = at(&ObjectKey {
            kind: kinds::TAG,
            hash: tag_hash,
        })?;
        let mut len = Vec::with_capacity(order.len());
        let mut starts = Vec::with_capacity(order.len() + 1);
        let mut deps: Vec<u32> = Vec::new();
        let mut seeds: Vec<u32> = Vec::new();
        for (i, key) in order.iter().enumerate() {
            let record = stream
                .read(*key)
                .map_err(from_stream)?
                .ok_or_else(inconsistent)?;
            // the captured store's links: the features its stream's header names
            let refs = record.refs(&report.header.features).map_err(|e| {
                to_rdy(StreamError::from_record(i as u64 + 1, 0, &e).with_object(Some(*key)))
            })?;
            let mut mine = Vec::with_capacity(refs.len() + 1);
            for r in refs {
                let target = match r.kind {
                    Some(kind) => ObjectKey { kind, hash: r.hash },
                    None => {
                        let mut found = rdy_store::sync_stream::sync_kinds()
                            .iter()
                            .map(|&kind| ObjectKey { kind, hash: r.hash })
                            .filter(|k| stream.holds(k));
                        let one = found.next().ok_or_else(inconsistent)?;
                        if found.next().is_some() {
                            return Err(inconsistent());
                        }
                        one
                    }
                };
                // HA1's order puts every need before the record that needs it
                let p = at(&target)?;
                if p as usize >= i {
                    return Err(inconsistent());
                }
                mine.push(p);
            }
            // a placeholder's apply is a reference that keeps no order; the placeholder, its
            // apply and its residue start the closure that always travels
            if let Some(proof) = record.proof() {
                mine.push(at(&proof)?);
                seeds.push(i as u32);
                seeds.extend(mine.iter().copied());
            }
            mine.sort_unstable();
            mine.dedup();
            starts.push(deps.len());
            deps.extend(mine);
            len.push(record.frame_len());
        }
        starts.push(deps.len());
        let mut retained = vec![false; order.len()];
        while let Some(p) = seeds.pop() {
            let p = p as usize;
            if retained[p] {
                continue;
            }
            retained[p] = true;
            seeds.extend(
                deps[starts[p]..starts[p + 1]]
                    .iter()
                    .filter(|d| !retained[**d as usize]),
            );
        }
        let repo_id = facts.repo_id;
        let features = report.header.features.clone();
        let manifest = features.to_json().len() as u64;
        read.ensure_current_epoch()?;
        Ok(PreparedHistory {
            source: Source::Checked(Mutex::new(Box::new(stream))),
            purpose: StreamPurpose::TagHistory,
            externals: 0,
            features,
            manifest,
            repo_id,
            tag: tag_hash,
            order,
            len,
            starts,
            deps,
            retained,
            root,
        })
    }

    /// The canon history from the head landing `head` above the hub tip `base` that `read`
    /// captured: Plan A's closure of the head (D-KE83X), whose records the hub may lack, in
    /// dependency order. The keys its records need beyond it are prerequisites that never
    /// travel. When `base` is the head, the head is such a prerequisite and the records are the
    /// attachments the base's links do not reach. Records are read from `read` when a batch is
    /// written.
    pub fn canon(
        read: Arc<ReadState>,
        head: Hash,
        base: Option<Hash>,
    ) -> Result<PreparedHistory, RdyError> {
        Self::history(read, StreamPurpose::CanonHistory, head, base)
    }

    /// The view history from the head view op `head` above the hub's view head `base` that
    /// `read` captured, in the canon form's way ([`PreparedHistory::canon`]): purpose
    /// `view_history`, the root a VIEW_OP.
    pub fn view(
        read: Arc<ReadState>,
        head: Hash,
        base: Option<Hash>,
    ) -> Result<PreparedHistory, RdyError> {
        Self::history(read, StreamPurpose::ViewHistory, head, base)
    }

    /// The canon or view history of `purpose` from `head` above `base`.
    fn history(
        read: Arc<ReadState>,
        purpose: StreamPurpose,
        head: Hash,
        base: Option<Hash>,
    ) -> Result<PreparedHistory, RdyError> {
        read.ensure_current_epoch()?;
        let repo_id = read.repo_id()?;
        let end = read.seq();
        // the walk's own facts of each record: no record is read or decoded again here
        let found = prepared_history_closure_before(
            read.reader(),
            purpose,
            head,
            &BTreeSet::new(),
            base,
            end,
        )
        .map_err(to_rdy)?;
        let facts = found.records;
        let found = found.history;
        let records = found.records.len();
        if facts.len() != records {
            return Err(inconsistent());
        }
        let mut order = found.records;
        order.extend(found.prerequisites.iter().copied());
        let place: HashMap<ObjectKey, u32> = order
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, i as u32))
            .collect();
        if place.len() != order.len() {
            return Err(inconsistent());
        }
        let at = |k: &ObjectKey| place.get(k).copied().ok_or_else(inconsistent);
        let root = at(&ObjectKey {
            kind: purpose.root_kind(),
            hash: head,
        })?;
        // the root is a record, unless the base's links hold it: then the base is the head
        if root as usize >= records && base != Some(head) {
            return Err(inconsistent());
        }
        let mut len = Vec::with_capacity(order.len());
        let mut starts = Vec::with_capacity(order.len() + 1);
        let mut deps: Vec<u32> = Vec::new();
        let mut seeds: Vec<u32> = Vec::new();
        for (i, fact) in facts.into_iter().enumerate() {
            if fact.key != order[i] || fact.frame_len < FRAME_HEADER {
                return Err(inconsistent());
            }
            let mut mine = Vec::with_capacity(fact.refs.len() + 1);
            for target in &fact.refs {
                // a record's need comes before it, or is a prerequisite
                let p = at(target)?;
                if (p as usize) >= i && (p as usize) < records {
                    return Err(inconsistent());
                }
                mine.push(p);
            }
            if let Some(proof) = fact.proof {
                mine.push(at(&proof)?);
                seeds.push(i as u32);
                seeds.extend(mine.iter().copied());
            }
            mine.sort_unstable();
            mine.dedup();
            starts.push(deps.len());
            deps.extend(mine);
            len.push(fact.frame_len - FRAME_HEADER);
        }
        for _ in records..order.len() {
            starts.push(deps.len());
            len.push(0);
        }
        starts.push(deps.len());
        let mut retained = vec![false; order.len()];
        while let Some(p) = seeds.pop() {
            let p = p as usize;
            if retained[p] || p >= records {
                continue;
            }
            retained[p] = true;
            seeds.extend(
                deps[starts[p]..starts[p + 1]]
                    .iter()
                    .filter(|d| !retained[**d as usize]),
            );
        }
        let features = found.features;
        let manifest = features.to_json().len() as u64;
        read.ensure_current_epoch()?;
        Ok(PreparedHistory {
            source: Source::State(read),
            purpose,
            externals: order.len() - records,
            features,
            manifest,
            repo_id,
            tag: head,
            order,
            len,
            starts,
            deps,
            retained,
            root,
        })
    }

    /// The places of the records, without the prerequisites only.
    fn records(&self) -> usize {
        self.order.len() - self.externals
    }

    /// The history's record keys in key order.
    pub fn keys(&self) -> Vec<ObjectKey> {
        match &self.source {
            Source::Checked(stream) => checked(stream).facts().objects.clone(),
            Source::State(_) => {
                let mut keys = self.order[..self.records()].to_vec();
                keys.sort_unstable();
                keys
            }
        }
    }

    /// The record of `key`, from the checked export or the captured state.
    fn read(&self, key: ObjectKey) -> Result<StreamRecord, RdyError> {
        let found = match &self.source {
            Source::Checked(stream) => checked(stream).read(key),
            Source::State(read) => StoreSource {
                reader: read.reader(),
                end: read.seq(),
            }
            .record(key),
        };
        found.map_err(from_stream)?.ok_or_else(inconsistent)
    }

    /// The captured read's epoch is still the store's, for a canon or view history; a tag
    /// history's checked export no longer reads the store.
    fn ensure_epoch(&self) -> Result<(), RdyError> {
        match &self.source {
            Source::State(read) => read.ensure_current_epoch(),
            Source::Checked(_) => Ok(()),
        }
    }

    fn deps_of(&self, p: u32) -> &[u32] {
        let p = p as usize;
        &self.deps[self.starts[p]..self.starts[p + 1]]
    }

    /// The 1-based record number and frame offset of place `p` in the exported stream.
    fn position(&self, p: u32) -> (u64, u64) {
        let before: u64 = self.len[..p as usize]
            .iter()
            .map(|l| FRAME_HEADER + l)
            .sum();
        (u64::from(p) + 1, BATCH_HEAD + self.manifest + before)
    }
}

/// The keys of `keys` (strictly ascending, at most 1,000,000) that the hub lacks, in key order:
/// one known request per chunk of at most 100,000 keys under push `push`, each right after a
/// fresh `context`. A reply that is not a sorted, unique subset of its chunk is
/// `E_UPSTREAM_PROTOCOL`.
pub fn missing_keys(
    keys: &[ObjectKey],
    push: &PushId,
    wire: &dyn PublishWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
) -> Result<Vec<ObjectKey>, RdyError> {
    missing_with(keys, push, &PublishObjects(wire), context)
}

/// One planned batch: its places, its prerequisites and its encoded bytes.
struct Planned {
    places: Vec<u32>,
    prerequisites: Vec<ObjectKey>,
    bytes: u64,
}

/// The prerequisites and size of one batch while it is built. Places are stamped with the
/// batch's generation, so a new batch starts without clearing.
struct Tally<'a> {
    history: &'a PreparedHistory,
    generation: u32,
    member: Vec<u32>,
    needed: Vec<u32>,
    count: Vec<u32>,
    /// The places a member needs that are not members, the root not counted.
    absent: usize,
    root_member: bool,
    records: u64,
    frames: u64,
    /// The places `needed` stamped this generation.
    touched: Vec<u32>,
}

impl<'a> Tally<'a> {
    fn new(history: &'a PreparedHistory) -> Tally<'a> {
        let n = history.order.len();
        Tally {
            history,
            generation: 0,
            member: vec![0; n],
            needed: vec![0; n],
            count: vec![0; n],
            absent: 0,
            root_member: false,
            records: 0,
            frames: 0,
            touched: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.generation += 1;
        self.absent = 0;
        self.root_member = false;
        self.records = 0;
        self.frames = 0;
        self.touched.clear();
    }

    fn is_needed(&self, p: u32) -> bool {
        self.needed[p as usize] == self.generation && self.count[p as usize] > 0
    }

    fn is_member(&self, p: u32) -> bool {
        self.member[p as usize] == self.generation
    }

    fn add(&mut self, p: u32) {
        let root = self.history.root;
        if p == root {
            self.root_member = true;
        } else if self.is_needed(p) {
            // a later proof an earlier placeholder of the batch named
            self.absent -= 1;
        }
        self.member[p as usize] = self.generation;
        for &d in self.history.deps_of(p) {
            if self.is_member(d) {
                continue;
            }
            let i = d as usize;
            if self.needed[i] != self.generation {
                self.needed[i] = self.generation;
                self.count[i] = 0;
                self.touched.push(d);
            }
            self.count[i] += 1;
            if self.count[i] == 1 && d != root {
                self.absent += 1;
            }
        }
        self.records += 1;
        self.frames += FRAME_HEADER + self.history.len[p as usize];
    }

    fn prerequisites(&self) -> usize {
        self.absent + usize::from(!self.root_member)
    }

    fn bytes(&self) -> u64 {
        BATCH_FIXED
            + self.history.manifest
            + KEY_LEN as u64 * self.prerequisites() as u64
            + self.frames
    }

    /// The batch's prerequisites in key order.
    fn keys(&self) -> Vec<ObjectKey> {
        let order = &self.history.order;
        let mut out: Vec<ObjectKey> = self
            .touched
            .iter()
            .filter(|&&d| self.is_needed(d) && !self.is_member(d))
            .map(|&d| order[d as usize])
            .collect();
        if !self.root_member {
            out.push(order[self.history.root as usize]);
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// E_LIMIT: the record at place `p` (none: the batch of no record) fits no batch of the hub's
/// limit. The issue names its place in the exported stream.
fn record_too_big(history: &PreparedHistory, p: Option<u32>) -> RdyError {
    let (record, offset) = p.map_or((None, 0), |p| {
        let (n, at) = history.position(p);
        (Some(n), at)
    });
    to_rdy(StreamError {
        code: E_LIMIT,
        message: "a stream record does not fit the hub batch limit".into(),
        fix: "ask the operator to raise the batch limit".into(),
        record,
        offset,
        object: Some(history.order[p.unwrap_or(history.root) as usize]),
    })
}

/// The batches of the places `send`, in order (see the module docs), within the upload caps.
fn plan(
    history: &PreparedHistory,
    send: &[u32],
    limits: BatchLimits,
) -> Result<Vec<Planned>, RdyError> {
    let bytes_limit = limits.bytes.min(CORE_BATCH_BYTES).min(MAX_STREAM_BYTES);
    let records_limit = limits.records.min(CORE_BATCH_RECORDS).min(MAX_RECORDS);
    let mut tally = Tally::new(history);
    let mut out: Vec<Planned> = Vec::new();
    let (mut records, mut bytes) = (0u64, 0u64);
    let mut start = 0usize;
    loop {
        // the longest next prefix that fits: its records and bytes only grow with it (a frame
        // is longer than the prerequisite key it can replace), so the scan ends at the first
        // prefix over either; its prerequisites can shrink, so each prefix is judged
        tally.reset();
        let mut best = 0usize;
        let mut k = 0usize;
        while start + k < send.len() && tally.records < records_limit {
            tally.add(send[start + k]);
            k += 1;
            if tally.bytes() > bytes_limit {
                break;
            }
            if tally.prerequisites() <= MAX_PREREQUISITES {
                best = k;
            }
        }
        if best == 0 && start < send.len() {
            let p = send[start];
            tally.reset();
            tally.add(p);
            let fits = records_limit > 0 && tally.bytes() <= bytes_limit;
            return Err(if fits {
                upload_limit(LIMIT_PREREQUISITES)
            } else {
                record_too_big(history, Some(p))
            });
        }
        tally.reset();
        for &p in &send[start..start + best] {
            tally.add(p);
        }
        if tally.bytes() > bytes_limit {
            // only the batch of no record gets here
            return Err(record_too_big(history, None));
        }
        let batch = Planned {
            places: send[start..start + best].to_vec(),
            prerequisites: tally.keys(),
            bytes: tally.bytes(),
        };
        records += batch.places.len() as u64;
        bytes = bytes.saturating_add(batch.bytes);
        out.push(batch);
        if out.len() > MAX_BATCH_FILES {
            return Err(upload_limit(LIMIT_BATCH_FILES));
        }
        if records > MAX_RECORDS {
            return Err(upload_limit(LIMIT_OBJECT_RECORDS));
        }
        if bytes > MAX_STREAM_BYTES {
            return Err(upload_limit(LIMIT_TOTAL_BYTES));
        }
        start += best;
        if start >= send.len() {
            return Ok(out);
        }
    }
}

/// Writes the batch `batch` of `history` to `scratch` from its start and rewinds it; returns its
/// bytes. `stop` is checked before each record: once it is set, the write ends with
/// [`stopped`].
fn write_batch(
    history: &PreparedHistory,
    batch: &Planned,
    scratch: &mut File,
    stop: &AtomicBool,
) -> Result<u64, RdyError> {
    scratch.set_len(0).map_err(|_| scratch_error())?;
    scratch
        .seek(SeekFrom::Start(0))
        .map_err(|_| scratch_error())?;
    let header = match history.purpose {
        StreamPurpose::CanonHistory => StreamHeader::canon_history(
            history.features.clone(),
            history.repo_id,
            history.tag,
            batch.prerequisites.clone(),
        ),
        StreamPurpose::ViewHistory => StreamHeader::view_history(
            history.features.clone(),
            history.repo_id,
            history.tag,
            batch.prerequisites.clone(),
        ),
        _ => StreamHeader::tag_history(
            history.features.clone(),
            history.repo_id,
            history.tag,
            batch.prerequisites.clone(),
        ),
    };
    if let Source::State(read) = &history.source {
        read.ensure_current_epoch()?;
    }
    let mut out = BufWriter::new(&mut *scratch);
    let mut w = StreamWriter::new(&mut out, &header).map_err(from_stream)?;
    for &p in &batch.places {
        if stop.load(Ordering::Acquire) {
            return Err(stopped());
        }
        let record = history.read(history.order[p as usize])?;
        w.record(&record).map_err(from_stream)?;
    }
    let (_, totals) = w.finish().map_err(from_stream)?;
    out.flush().map_err(|_| scratch_error())?;
    drop(out);
    let written = totals.bytes + FRAME_HEADER + TRAILER_LEN as u64;
    if written != batch.bytes {
        return Err(inconsistent());
    }
    scratch
        .seek(SeekFrom::Start(0))
        .map_err(|_| scratch_error())?;
    // a canon history batch passes Plan A's batch check before it is sent
    if history.purpose != StreamPurpose::TagHistory {
        crate::stream_check::check_batch((&*scratch).take(written), Some(history.repo_id))?;
        scratch
            .seek(SeekFrom::Start(0))
            .map_err(|_| scratch_error())?;
    }
    Ok(written)
}

/// The known and put steps a history upload uses: the publication upload's, or the sync
/// exchange's.
pub trait ObjectWire {
    /// The keys of `keys` the hub lacks for push `push`.
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError>;
    /// One batch of `len` bytes into the quarantine of push `push`.
    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError>;
    /// One prepared batch into the quarantine of push `push`. The default sends the batch's
    /// own bytes through [`ObjectWire::put`].
    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        let (raw, len) = body.identity()?;
        self.put(cx, push, raw, len)
    }
}

/// The publication upload's wire as an [`ObjectWire`].
struct PublishObjects<'a>(&'a dyn PublishWire);

impl ObjectWire for PublishObjects<'_> {
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.0.known(cx, push, keys)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put_objects(cx, push, body, len)
    }

    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put_objects_prepared(cx, push, body)
    }
}

/// Sends `history` to the hub under push `push` (see the module docs): the known step, then each
/// batch as one objects request, each request right after a fresh `context`. Returns what the
/// hub acknowledged.
pub fn send_history(
    history: &mut PreparedHistory,
    push: &PushId,
    limits: BatchLimits,
    wire: &dyn PublishWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
) -> Result<HistorySent, RdyError> {
    let mut sent = HistorySent::default();
    let missing = missing_with(&history.keys(), push, &PublishObjects(wire), context)?;
    send_objects(
        history,
        &missing,
        push,
        limits,
        &PublishObjects(wire),
        context,
        &mut || Ok(()),
        &mut sent,
    )?;
    Ok(sent)
}

/// [`missing_keys`] over any [`ObjectWire`].
pub fn missing_with(
    keys: &[ObjectKey],
    push: &PushId,
    wire: &dyn ObjectWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
) -> Result<Vec<ObjectKey>, RdyError> {
    if keys.len() as u64 > MAX_RECORDS {
        return Err(upload_limit(LIMIT_OBJECT_RECORDS));
    }
    if keys.windows(2).any(|w| w[0] >= w[1]) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the object keys are not in strictly ascending order",
            "sort the object keys and remove repeats",
        ));
    }
    let mut missing: Vec<ObjectKey> = Vec::new();
    for chunk in keys.chunks(MAX_KNOWN_KEYS) {
        let cx = context()?;
        let reply = wire.known(&cx, push.as_str(), chunk)?;
        let subset = reply.len() <= chunk.len()
            && reply.windows(2).all(|w| w[0] < w[1])
            && reply.iter().all(|k| chunk.binary_search(k).is_ok());
        if !subset || missing.len() + reply.len() > MAX_RECORDS as usize {
            return Err(upstream_protocol());
        }
        missing.extend(reply);
    }
    Ok(missing)
}

/// Sends the records of `history` that `missing` names (and every record that always travels)
/// under push `push`, each batch right after a fresh `context`; `before_put` runs once, before
/// the first put. `sent` counts each acknowledged batch as it comes, so a caller that gets an
/// error still knows what the hub acknowledged. Every batch is planned before the first is
/// sent. A tag history with nothing to send sends one batch of no record; a canon history sends
/// none.
#[allow(clippy::too_many_arguments)]
pub fn send_objects(
    history: &PreparedHistory,
    missing: &[ObjectKey],
    push: &PushId,
    limits: BatchLimits,
    wire: &dyn ObjectWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
    before_put: &mut dyn FnMut() -> Result<(), RdyError>,
    sent: &mut HistorySent,
) -> Result<(), RdyError> {
    let send: Vec<u32> = (0..history.records() as u32)
        .filter(|&p| {
            history.retained[p as usize]
                || missing.binary_search(&history.order[p as usize]).is_ok()
        })
        .collect();
    if send.is_empty() && history.purpose != StreamPurpose::TagHistory {
        return Ok(());
    }
    let batches = plan(history, &send, limits)?;
    put_batches(history, &batches, push, wire, context, before_put, sent)
}

/// Sends one batch of no record of `history` under push `push`, right after a fresh `context`:
/// its root is its only prerequisite; `before_put` runs once, before the put. A push or restore
/// sends it when the hub holds every record yet the write moves canon or a view, so the push
/// still names the root the write moves to.
pub fn send_root(
    history: &PreparedHistory,
    push: &PushId,
    limits: BatchLimits,
    wire: &dyn ObjectWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
    before_put: &mut dyn FnMut() -> Result<(), RdyError>,
    sent: &mut HistorySent,
) -> Result<(), RdyError> {
    let batches = plan(history, &[], limits)?;
    put_batches(history, &batches, push, wire, context, before_put, sent)
}

/// E_INTERNAL: a preparation that the upload stopped. The upload has already failed, and that
/// failure is the one returned.
fn stopped() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "the upload preparation stopped",
        "retry the push; report the error if it repeats",
    )
}

/// The message of [`upload_start_error`].
pub const UPLOAD_START_MESSAGE: &str = "the upload preparation could not start";
/// The fix of [`upload_start_error`].
pub const UPLOAD_START_FIX: &str = "retry the push";
/// The message of [`upload_failed_error`].
pub const UPLOAD_FAILED_MESSAGE: &str = "the upload preparation failed";
/// The fix of [`upload_failed_error`].
pub const UPLOAD_FAILED_FIX: &str = "retry the push; report the error if it repeats";

/// E_IO: the upload's preparation worker could not be started.
fn upload_start_error() -> RdyError {
    RdyError::new(E_IO, UPLOAD_START_MESSAGE, UPLOAD_START_FIX)
}

/// E_INTERNAL: the upload's preparation worker panicked. Its payload is never shown.
fn upload_failed_error() -> RdyError {
    RdyError::new(E_INTERNAL, UPLOAD_FAILED_MESSAGE, UPLOAD_FAILED_FIX)
}

/// What a worker thread of an upload runs first: a test's registrations, made again on the
/// worker (a thread-local registration does not cross threads). Its guards stay on the worker
/// until it ends.
pub type UploadWorkerSetup = Arc<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync>;

thread_local! {
    static WORKER_SETUP: RefCell<Option<UploadWorkerSetup>> = const { RefCell::new(None) };
    static PROBE: RefCell<Option<Arc<UploadProbe>>> = const { RefCell::new(None) };
}

/// Runs `f` with `setup` installed for the upload workers this thread starts: the canon
/// capture of a push and the batch preparation of an upload each run `setup` before their work
/// and keep what it returns until they end. The prior setup comes back when `f` returns or
/// panics. Production installs none.
#[doc(hidden)]
pub fn with_upload_worker_setup_for_tests<T>(setup: UploadWorkerSetup, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<UploadWorkerSetup>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prior = self.0.take();
            WORKER_SETUP.with(|c| *c.borrow_mut() = prior);
        }
    }
    let prior = WORKER_SETUP.with(|c| c.borrow_mut().replace(setup));
    let _restore = Restore(prior);
    f()
}

/// The setup a worker started by this thread runs first, if a test installed one.
pub(crate) fn upload_worker_setup() -> Option<UploadWorkerSetup> {
    WORKER_SETUP.with(|c| c.borrow().clone())
}

/// A point in one batch's preparation that [`UploadProbe::on_stage`] sees.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadStage {
    /// The batch is written and checked.
    Built,
    /// The batch is framed (or kept as identity when the frame is not smaller).
    Compressed,
}

/// What a test sees of the uploads this thread runs ([`with_upload_probe_for_tests`]): the
/// bodies, scratch files and workers live at once (now and at most), the workers started, and
/// each prepared and compressed body. `on_stage` runs on the thread that prepares a batch, with
/// the stage and the batch's place in the upload; its error fails that preparation as a build
/// or compression error would.
#[doc(hidden)]
#[derive(Default)]
pub struct UploadProbe {
    pub live_bodies: AtomicUsize,
    pub max_bodies: AtomicUsize,
    pub live_scratch: AtomicUsize,
    pub max_scratch: AtomicUsize,
    pub live_workers: AtomicUsize,
    pub max_workers: AtomicUsize,
    pub workers_started: AtomicUsize,
    pub prepared: AtomicUsize,
    pub compressed: AtomicUsize,
    #[allow(clippy::type_complexity)]
    pub on_stage: Mutex<Option<Box<dyn Fn(UploadStage, usize) -> Result<(), RdyError> + Send>>>,
}

impl UploadProbe {
    fn up(now: &AtomicUsize, max: &AtomicUsize, by: usize) {
        let n = now.fetch_add(by, Ordering::SeqCst) + by;
        max.fetch_max(n, Ordering::SeqCst);
    }

    fn down(now: &AtomicUsize, by: usize) {
        now.fetch_sub(by, Ordering::SeqCst);
    }

    /// Runs the test's stage hook of `probe`, if any.
    fn stage(
        probe: &Option<Arc<UploadProbe>>,
        stage: UploadStage,
        place: usize,
    ) -> Result<(), RdyError> {
        let Some(p) = probe else {
            return Ok(());
        };
        let hook = p.on_stage.lock().unwrap_or_else(|e| e.into_inner());
        match hook.as_ref() {
            Some(f) => f(stage, place),
            None => Ok(()),
        }
    }
}

/// Runs `f` with `probe` counting the uploads this thread runs (a test hook).
#[doc(hidden)]
pub fn with_upload_probe_for_tests<T>(probe: Arc<UploadProbe>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<UploadProbe>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prior = self.0.take();
            PROBE.with(|c| *c.borrow_mut() = prior);
        }
    }
    let prior = PROBE.with(|c| c.borrow_mut().replace(probe));
    let _restore = Restore(prior);
    f()
}

/// The probe of this thread's uploads, if a test installed one.
pub(crate) fn upload_probe() -> Option<Arc<UploadProbe>> {
    PROBE.with(|c| c.borrow().clone())
}

/// A live worker thread of an upload, as the probe counts it (nothing without a probe).
pub(crate) struct WorkerMark(Option<Arc<UploadProbe>>);

impl WorkerMark {
    pub(crate) fn new(probe: Option<Arc<UploadProbe>>) -> WorkerMark {
        if let Some(p) = &probe {
            p.workers_started.fetch_add(1, Ordering::SeqCst);
            UploadProbe::up(&p.live_workers, &p.max_workers, 1);
        }
        WorkerMark(probe)
    }
}

impl Drop for WorkerMark {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            UploadProbe::down(&p.live_workers, 1);
        }
    }
}

/// One batch of a history upload, written by the stream writer, checked as its purpose checks
/// it and, when the push negotiated zstd, framed once ([`zstd_frame`]): its raw scratch file and
/// length, and the framed file and length when the frame is smaller. The raw file stays, so a
/// fresh context that cannot take the frame sends the raw bytes. Only an upload makes one: no
/// caller supplies its bytes or a report. Every read starts from its file's start.
pub struct PreparedObjectBody {
    raw: File,
    raw_len: u64,
    framed: Option<File>,
    wire_len: u64,
    encoding: BodyEncoding,
    probe: Option<Arc<UploadProbe>>,
}

impl std::fmt::Debug for PreparedObjectBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedObjectBody")
            .field("raw_len", &self.raw_len)
            .field("wire_len", &self.wire_len)
            .field("encoding", &self.encoding)
            .finish_non_exhaustive()
    }
}

impl PreparedObjectBody {
    /// Writes and checks batch `batch` of `history` ([`write_batch`]), then frames it when
    /// `zstd`. `place` is the batch's place in the upload, for the probe.
    fn prepare(
        history: &PreparedHistory,
        batch: &Planned,
        zstd: bool,
        stop: &AtomicBool,
        probe: Option<Arc<UploadProbe>>,
        place: usize,
    ) -> Result<PreparedObjectBody, RdyError> {
        let raw = tempfile::tempfile().map_err(|_| scratch_error())?;
        if let Some(p) = &probe {
            UploadProbe::up(&p.live_bodies, &p.max_bodies, 1);
            UploadProbe::up(&p.live_scratch, &p.max_scratch, 1);
        }
        let mut body = PreparedObjectBody {
            raw,
            raw_len: 0,
            framed: None,
            wire_len: 0,
            encoding: BodyEncoding::Identity,
            probe,
        };
        body.raw_len = write_batch(history, batch, &mut body.raw, stop)?;
        body.wire_len = body.raw_len;
        if let Some(p) = &body.probe {
            p.prepared.fetch_add(1, Ordering::SeqCst);
        }
        UploadProbe::stage(&body.probe, UploadStage::Built, place)?;
        if zstd {
            if stop.load(Ordering::Acquire) {
                return Err(stopped());
            }
            body.compress(place)?;
        }
        Ok(body)
    }

    /// Frames the raw bytes once, at level 3 with their checksum and size: the frame is kept
    /// when it is smaller, else the raw bytes stay the body. The unused output is closed here.
    fn compress(&mut self, place: usize) -> Result<(), RdyError> {
        debug_assert!(self.framed.is_none(), "a body is framed once");
        self.raw
            .seek(SeekFrom::Start(0))
            .map_err(|_| scratch_error())?;
        if let Some(p) = &self.probe {
            // the frame and the copy that zstd_frame writes
            UploadProbe::up(&p.live_scratch, &p.max_scratch, 2);
        }
        let framed = zstd_frame(&mut (&self.raw).take(self.raw_len), self.raw_len);
        if let Some(p) = &self.probe {
            UploadProbe::down(&p.live_scratch, 2);
            p.compressed.fetch_add(1, Ordering::SeqCst);
        }
        let (file, len, encoding) = framed?;
        if encoding == BodyEncoding::Zstd {
            if let Some(p) = &self.probe {
                UploadProbe::up(&p.live_scratch, &p.max_scratch, 1);
            }
            self.framed = Some(file);
            self.wire_len = len;
            self.encoding = BodyEncoding::Zstd;
        }
        UploadProbe::stage(&self.probe, UploadStage::Compressed, place)
    }

    /// The batch's bytes before any compression.
    pub fn raw_len(&self) -> u64 {
        self.raw_len
    }

    /// The bytes it sends as [`PreparedObjectBody::encoding`].
    pub fn wire_len(&self) -> u64 {
        self.wire_len
    }

    /// Its encoding when the context takes zstd: the frame when it is smaller, else identity.
    pub fn encoding(&self) -> BodyEncoding {
        self.encoding
    }

    /// The raw bytes from their start and their length, for an identity send.
    pub fn identity(&mut self) -> Result<(&mut dyn Read, u64), RdyError> {
        self.raw
            .seek(SeekFrom::Start(0))
            .map_err(|_| scratch_error())?;
        Ok((&mut self.raw, self.raw_len))
    }

    /// What goes on the wire for a context that takes zstd (`zstd`) or not: the frame, its
    /// length and [`BodyEncoding::Zstd`] when it has one and the context takes it, else the raw
    /// bytes as identity; from its start.
    pub fn wire(&mut self, zstd: bool) -> Result<(&mut File, u64, BodyEncoding), RdyError> {
        match &mut self.framed {
            Some(framed) if zstd => {
                framed
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| scratch_error())?;
                Ok((framed, self.wire_len, BodyEncoding::Zstd))
            }
            _ => {
                self.raw
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| scratch_error())?;
                Ok((&mut self.raw, self.raw_len, BodyEncoding::Identity))
            }
        }
    }
}

impl Drop for PreparedObjectBody {
    fn drop(&mut self) {
        if let Some(p) = &self.probe {
            UploadProbe::down(&p.live_bodies, 1);
            UploadProbe::down(&p.live_scratch, 1 + usize::from(self.framed.is_some()));
        }
    }
}

// the one preparation worker borrows the history while the caller sends
const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    fn sent<T: Send>() {}
    shared::<PreparedHistory>();
    sent::<PreparedObjectBody>();
    sent::<RdyError>();
};

/// One batch of [`serial_bodies_for_tests`]: its raw bytes, and its frame when the frame is
/// smaller.
#[doc(hidden)]
pub type SerialBody = (Vec<u8>, Option<Vec<u8>>);

/// The bodies of `batches` of `history` as the serial upload before the pipeline wrote them:
/// one scratch file rewritten per batch, each framed once when `zstd`. Each is (raw bytes,
/// framed bytes when the frame is smaller). A test hook: the reference a pipelined upload's
/// bytes are compared with.
#[doc(hidden)]
pub fn serial_bodies_for_tests(
    history: &PreparedHistory,
    missing: &[ObjectKey],
    limits: BatchLimits,
    zstd: bool,
) -> Result<Vec<SerialBody>, RdyError> {
    let send: Vec<u32> = (0..history.records() as u32)
        .filter(|&p| {
            history.retained[p as usize]
                || missing.binary_search(&history.order[p as usize]).is_ok()
        })
        .collect();
    let batches = plan(history, &send, limits)?;
    let mut scratch = tempfile::tempfile().map_err(|_| scratch_error())?;
    let never = AtomicBool::new(false);
    let mut out = Vec::new();
    for batch in &batches {
        let len = write_batch(history, batch, &mut scratch, &never)?;
        let mut raw = Vec::new();
        (&scratch)
            .take(len)
            .read_to_end(&mut raw)
            .map_err(|_| scratch_error())?;
        let framed = if zstd {
            scratch
                .seek(SeekFrom::Start(0))
                .map_err(|_| scratch_error())?;
            let (mut file, _, encoding) = zstd_frame(&mut (&scratch).take(len), len)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|_| scratch_error())?;
            (encoding == BodyEncoding::Zstd).then_some(bytes)
        } else {
            None
        };
        out.push((raw, framed));
    }
    Ok(out)
}

/// Writes and puts each batch of `batches` in order (see [`send_objects`]).
///
/// The first batch is written and checked here; `before_put` then runs, and the first fresh
/// context tells whether the push sends zstd (its negotiated state), so the first batch is
/// framed then. With more batches, one scoped worker prepares the next batch (write, check and
/// frame) while this thread sends the current one: two batch slots, never more. The worker gets
/// a job only once this thread took the batch before it and released the one before that; it
/// holds no token and asks for no context. Before every put the captured epoch is checked
/// again. On any failure the job channel closes, the next body is dropped and the worker is
/// joined; an earlier batch's failure wins over a later preparation's, and nothing after a
/// failed batch is sent.
fn put_batches(
    history: &PreparedHistory,
    batches: &[Planned],
    push: &PushId,
    wire: &dyn ObjectWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
    before_put: &mut dyn FnMut() -> Result<(), RdyError>,
    sent: &mut HistorySent,
) -> Result<(), RdyError> {
    let Some(first) = batches.first() else {
        return Ok(());
    };
    let probe = upload_probe();
    let never = AtomicBool::new(false);
    let mut current = PreparedObjectBody::prepare(history, first, false, &never, probe.clone(), 0)?;
    before_put()?;
    let cx = context()?;
    let zstd = cx.zstd;
    if zstd {
        current.compress(0)?;
    }
    let mut last = ObjectPut {
        records: 0,
        bytes: 0,
    };
    if batches.len() == 1 {
        return put_one(history, first, push, wire, &cx, current, &mut last, sent);
    }
    let setup = upload_worker_setup();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let (jobs, job_rx) = sync_channel::<usize>(1);
        let (results_tx, results) = sync_channel::<Result<PreparedObjectBody, RdyError>>(0);
        let worker = {
            let (stop, probe, setup) = (&stop, probe.clone(), setup.clone());
            std::thread::Builder::new()
                .name("rdy-upload-prepare".into())
                .spawn_scoped(s, move || {
                    let _guards = setup.as_ref().map(|f| f());
                    let _mark = WorkerMark::new(probe.clone());
                    produce(history, batches, zstd, stop, probe, job_rx, results_tx);
                })
                .map_err(|_| upload_start_error())?
        };
        let outcome = drive(
            history, batches, push, wire, context, cx, current, &jobs, &results, &mut last, sent,
        );
        // close the job channel, drop the next body, and wait for the worker
        stop.store(true, Ordering::Release);
        drop(jobs);
        drop(results);
        let joined = worker.join();
        match (outcome, joined) {
            (Err(Drive::Failed(e)), _) => Err(e),
            (Err(Drive::WorkerGone), _) | (Ok(()), Err(_)) => Err(upload_failed_error()),
            (Ok(()), Ok(())) => Ok(()),
        }
    })
}

/// How the caller's part of a pipelined upload ended early: an error of its own (or one the
/// worker returned), or a worker that ended without a result.
enum Drive {
    Failed(RdyError),
    WorkerGone,
}

impl From<RdyError> for Drive {
    fn from(e: RdyError) -> Drive {
        Drive::Failed(e)
    }
}

/// The preparation worker: each job names the next batch; its body (or its error) goes back
/// on the rendezvous channel. It ends when the job channel closes, the caller stops it, a
/// result cannot be handed over, or a preparation fails.
fn produce(
    history: &PreparedHistory,
    batches: &[Planned],
    zstd: bool,
    stop: &AtomicBool,
    probe: Option<Arc<UploadProbe>>,
    jobs: Receiver<usize>,
    results: SyncSender<Result<PreparedObjectBody, RdyError>>,
) {
    for i in jobs {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let body = PreparedObjectBody::prepare(history, &batches[i], zstd, stop, probe.clone(), i);
        let failed = body.is_err();
        if results.send(body).is_err() || failed {
            break;
        }
    }
}

/// The caller's part of a pipelined upload: each batch in order, the first already prepared and
/// its context `cx` taken.
#[allow(clippy::too_many_arguments)]
fn drive(
    history: &PreparedHistory,
    batches: &[Planned],
    push: &PushId,
    wire: &dyn ObjectWire,
    context: &mut dyn FnMut() -> Result<UploadContext, RdyError>,
    cx: UploadContext,
    current: PreparedObjectBody,
    jobs: &SyncSender<usize>,
    results: &Receiver<Result<PreparedObjectBody, RdyError>>,
    last: &mut ObjectPut,
    sent: &mut HistorySent,
) -> Result<(), Drive> {
    let mut cx = Some(cx);
    let mut current = Some(current);
    for (i, batch) in batches.iter().enumerate() {
        let body = match current.take() {
            Some(b) => b,
            None => match results.recv() {
                Ok(b) => b?,
                Err(_) => return Err(Drive::WorkerGone),
            },
        };
        let cx = match cx.take() {
            Some(cx) => cx,
            None => context()?,
        };
        // the next batch is prepared while this one is sent
        if i + 1 < batches.len() && jobs.send(i + 1).is_err() {
            return Err(Drive::WorkerGone);
        }
        put_one(history, batch, push, wire, &cx, body, last, sent)?;
    }
    Ok(())
}

/// Puts `body`, batch `batch` of `history`, under push `push` with the fresh context `cx`,
/// after the epoch check, and counts it in `sent`. The body is released when it returns.
#[allow(clippy::too_many_arguments)]
fn put_one(
    history: &PreparedHistory,
    batch: &Planned,
    push: &PushId,
    wire: &dyn ObjectWire,
    cx: &UploadContext,
    mut body: PreparedObjectBody,
    last: &mut ObjectPut,
    sent: &mut HistorySent,
) -> Result<(), RdyError> {
    history.ensure_epoch()?;
    let put = wire.put_prepared(cx, push.as_str(), &mut body)?;
    // the quarantine's distinct records and bytes so far: they never shrink and stay
    // within one upload's caps
    if put.records < last.records
        || put.bytes < last.bytes
        || put.records > MAX_RECORDS
        || put.bytes > MAX_STREAM_BYTES
    {
        return Err(upstream_protocol());
    }
    *last = put;
    sent.batches += 1;
    sent.records += batch.places.len() as u64;
    sent.bytes += body.raw_len;
    Ok(())
}
