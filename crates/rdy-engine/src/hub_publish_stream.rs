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
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_IO, E_LIMIT, RdyError, upstream_protocol};
use crate::hub_publish_wire::{
    CORE_BATCH_BYTES, CORE_BATCH_RECORDS, MAX_KNOWN_KEYS, ObjectPut, PUSH_ID_CHARS, PublishWire,
    UploadContext,
};
use crate::read_state::ReadState;
use crate::stream_error::{StreamError, to_rdy};
use crate::stream_export;
use crate::stream_take::uncovered_change;
use rdy_model::{CROCKFORD, Hash, RepoId};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::{LIMIT_OBJECT_RECORDS, LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES};
use rdy_store::stream_source::CheckedStream;
use rdy_store::sync_stream::{
    HEADER_FIXED, KEY_LEN, LIMIT_BATCH_FILES, MAGIC, MAX_BATCH_FILES, MAX_PREREQUISITES,
    MAX_RECORDS, MAX_STREAM_BYTES, SYNC_KINDS, StreamHeader, StreamWriter, TRAILER_LEN,
};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

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

/// One tag's complete history, exported and checked, ready to send (see the module docs).
pub struct PreparedHistory {
    stream: CheckedStream<File>,
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
    /// The root tag's place in `order`.
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
fn scratch_error() -> RdyError {
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
            let refs = record.refs().map_err(|e| {
                to_rdy(StreamError::from_record(i as u64 + 1, 0, &e).with_object(Some(*key)))
            })?;
            let mut mine = Vec::with_capacity(refs.len() + 1);
            for r in refs {
                let target = match r.kind {
                    Some(kind) => ObjectKey { kind, hash: r.hash },
                    None => {
                        let mut found = SYNC_KINDS
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
            stream,
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
/// bytes.
fn write_batch(
    history: &PreparedHistory,
    batch: &Planned,
    scratch: &mut File,
) -> Result<u64, RdyError> {
    scratch.set_len(0).map_err(|_| scratch_error())?;
    scratch
        .seek(SeekFrom::Start(0))
        .map_err(|_| scratch_error())?;
    let header = StreamHeader::tag_history(
        history.features.clone(),
        history.repo_id,
        history.tag,
        batch.prerequisites.clone(),
    );
    let mut out = BufWriter::new(&mut *scratch);
    let mut w = StreamWriter::new(&mut out, &header).map_err(from_stream)?;
    for &p in &batch.places {
        let record = history
            .stream
            .read(history.order[p as usize])
            .map_err(from_stream)?
            .ok_or_else(inconsistent)?;
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
    Ok(written)
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
    let history = &*history;
    let missing = missing_keys(&history.stream.facts().objects, push, wire, context)?;
    let send: Vec<u32> = (0..history.order.len() as u32)
        .filter(|&p| {
            history.retained[p as usize]
                || missing.binary_search(&history.order[p as usize]).is_ok()
        })
        .collect();
    let batches = plan(history, &send, limits)?;
    let mut scratch = tempfile::tempfile().map_err(|_| scratch_error())?;
    let mut sent = HistorySent::default();
    let mut last = ObjectPut {
        records: 0,
        bytes: 0,
    };
    for batch in &batches {
        let len = write_batch(history, batch, &mut scratch)?;
        let cx = context()?;
        let mut body = (&scratch).take(len);
        let put = wire.put_objects(&cx, push.as_str(), &mut body, len)?;
        // the quarantine's distinct records and bytes so far: they never shrink and stay
        // within one upload's caps
        if put.records < last.records
            || put.bytes < last.bytes
            || put.records > MAX_RECORDS
            || put.bytes > MAX_STREAM_BYTES
        {
            return Err(upstream_protocol());
        }
        last = put;
        sent.batches += 1;
        sent.records += batch.places.len() as u64;
        sent.bytes += len;
    }
    Ok(sent)
}
