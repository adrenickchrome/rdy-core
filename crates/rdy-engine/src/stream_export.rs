//! The export of one tag's history as a sync batch (D-D2NPN, D-Q0AVZ), and of a canon or view
//! history as a format-4 batch (D-KE83X, [`write_history`]).
//!
//! [`write`] reads one [`ReadState`]: the root is a tag the state captured, and the log is read
//! up to the state's captured end, so a landing, signature, answer or removal proof stored later
//! is not in it. The records are the tag's closure in dependency order (`rdy_store::closure`);
//! an object a removal dropped travels as its placeholder, never as its bytes. Removal proofs
//! applied after the tag was made are part of its closure, so an older tag still carries them.
//!
//! The redaction epoch is checked before and after. A removal that is applied while the batch is
//! written makes the call fail with `E_REDACTED_STATE`, and the caller discards what it wrote.
//! The call owns no output path and splits nothing: a host that wants several batches reads the
//! ordered records through the stream reader and writer.
use crate::error::{E_INTERNAL, E_IO, E_NOT_FOUND, RdyError};
use crate::history_facts::{MAX_KEYS, key_limit};
use crate::hub_http::{BodyEncoding, UPLOAD_BUFFER_BYTES, zstd_frame};
use crate::read_state::ReadState;
use crate::stream_error::to_rdy;
use rdy_model::Hash;
use rdy_store::closure::{
    HistoryClosure, StoreSource, closure_before, dependency_order, history_closure_before,
    write_batch,
};
use rdy_store::object_refs::{ObjectKey, ObjectRef};
use rdy_store::segment::Location;
use rdy_store::stream_error::{LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES, Reason, StreamError};
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{
    BatchReport, HEADER_FIXED, KEY_LEN, MAGIC, MAX_PREREQUISITES, MAX_STREAM_BYTES, RecordFeatures,
    StreamHeader, StreamPurpose, StreamRecord, TRAILER_LEN, sync_kinds,
};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;

/// Writes the history of the tag `tag_hash` as one complete batch to `out`, less the objects in
/// `known` (at most [`MAX_KEYS`]), which the header names as prerequisites when a written record
/// needs them. The tag must be one `read` captured. Returns what the batch holds, its keys in
/// wire order.
pub fn write(
    read: &ReadState,
    tag_hash: Hash,
    known: &BTreeSet<ObjectKey>,
    out: impl Write,
) -> Result<BatchReport, RdyError> {
    if known.len() > MAX_KEYS {
        return Err(key_limit());
    }
    read.ensure_current_epoch()?;
    if !read.tags.iter().any(|(_, t)| t.id() == tag_hash) {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("no captured tag {tag_hash}"),
            "export a tag the snapshot holds",
        ));
    }
    let reader = read.reader();
    let Some(repo_id) = reader.repo_id()? else {
        return Err(RdyError::new(
            E_INTERNAL,
            "the store has no repo id",
            "report this as a bug in rdy",
        ));
    };
    let end = read.seq();
    let found = closure_before(reader, tag_hash, known, end).map_err(to_rdy)?;
    let header = StreamHeader::tag_history(
        reader.store_features()?,
        repo_id,
        tag_hash,
        found.prerequisites,
    );
    let mut source = Features {
        inner: StoreSource { reader, end },
        seen: RecordFeatures::default(),
    };
    let (_, totals) = write_batch(&mut source, &header, &found.records, out).map_err(to_rdy)?;
    let record_features = source.seen.finish();
    read.ensure_current_epoch()?;
    Ok(BatchReport {
        header,
        keys: found.records,
        record_features,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}

/// Writes the canon history (`purpose` canon_history, `root` a landing) or view history
/// (view_history, `root` a view op) that `read` holds as one complete format-4 batch to `out`
/// (D-KE83X). It omits the keys of `known` (at most [`MAX_KEYS`]) and the forward-link closure
/// of `base` (a landing or view op the receiver holds); the attachments of those records still
/// travel. The header lists only the features the batch's records need, and names as
/// prerequisites what a written record needs and the batch omits. Returns what the batch holds,
/// its keys in wire order.
pub fn write_history(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
    known: &BTreeSet<ObjectKey>,
    out: impl Write,
) -> Result<BatchReport, RdyError> {
    if known.len() > MAX_KEYS {
        return Err(key_limit());
    }
    read.ensure_current_epoch()?;
    let reader = read.reader();
    let end = read.seq();
    let found = walk_history(reader, purpose, root, known, base, end).map_err(to_rdy)?;
    emit_history(read, purpose, root, found, out)
}

/// Writes the records of a found history closure as one format-4 batch and checks the redaction
/// epoch again. The caller has checked the epoch before it read the closure.
fn emit_history(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    found: HistoryClosure,
    out: impl Write,
) -> Result<BatchReport, RdyError> {
    let reader = read.reader();
    let Some(repo_id) = reader.repo_id()? else {
        return Err(RdyError::new(
            E_INTERNAL,
            "the store has no repo id",
            "report this as a bug in rdy",
        ));
    };
    let end = read.seq();
    let header = match purpose {
        StreamPurpose::ViewHistory => {
            StreamHeader::view_history(found.features, repo_id, root, found.prerequisites)
        }
        _ => StreamHeader::canon_history(found.features, repo_id, root, found.prerequisites),
    };
    let mut source = Features {
        inner: StoreSource { reader, end },
        seen: RecordFeatures::default(),
    };
    let (_, totals) = write_batch(&mut source, &header, &found.records, out).map_err(to_rdy)?;
    let record_features = source.seen.finish();
    read.ensure_current_epoch()?;
    Ok(BatchReport {
        header,
        keys: found.records,
        record_features,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}

/// One page of a history's keys (see [`history_page`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPage {
    /// The page's keys, in the order [`write_history`] writes them.
    pub keys: Vec<ObjectKey>,
    /// The last key of `keys` when more keys follow it; None at the end.
    pub next: Option<ObjectKey>,
}

/// Gives a page of at most `limit` keys (1 to [`MAX_KEYS`]) of the canon or view history of
/// `root`, less the link closure of `base`, in the order [`write_history`] writes them, after
/// the key `after` (from the start when None). A `base` that `read` does not hold counts as
/// none. A root that `read` does not hold, and an `after` that the history does not hold, are
/// `E_UNRESOLVED_REF`; a purpose other than canon_history or view_history is
/// `E_STREAM_PURPOSE`. It reads up to the state's captured log end and checks the redaction
/// epoch before and after, as [`write_history`] does.
pub fn history_page(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
    after: Option<ObjectKey>,
    limit: usize,
) -> Result<KeyPage, RdyError> {
    if limit == 0 || limit > MAX_KEYS {
        return Err(key_limit());
    }
    read.ensure_current_epoch()?;
    let reader = read.reader();
    let end = read.seq();
    let base = match purpose {
        StreamPurpose::CanonHistory | StreamPurpose::ViewHistory => base.filter(|b| {
            StoreSource { reader, end }
                .holds(purpose.root_kind(), b)
                .unwrap_or(false)
        }),
        _ => base,
    };
    if base.is_none() {
        // the whole history: one captured closure, kept for this call only
        return Selection::build(read, purpose, root)?.page(read, after, limit);
    }
    // an incremental page keeps its own walk, which charges only what the pull holds; it
    // never builds the whole history's closure and key set
    let found = walk_history(reader, purpose, root, &BTreeSet::new(), base, end).map_err(to_rdy)?;
    let start = match after {
        None => 0,
        Some(k) => match found.records.iter().position(|r| *r == k) {
            Some(i) => i + 1,
            None => return Err(to_rdy(StreamError::unresolved(0, 0, k))),
        },
    };
    let rest = &found.records[start..];
    let keys: Vec<ObjectKey> = rest.iter().take(limit).copied().collect();
    let next = if rest.len() > keys.len() {
        keys.last().copied()
    } else {
        None
    };
    read.ensure_current_epoch()?;
    Ok(KeyPage { keys, next })
}

/// Writes the records that `keys` name of the canon or view history of `root` as one complete
/// format-4 batch with that purpose and root, in the order [`write_history`] writes a batch of
/// that set. The header names as prerequisites what a written record needs and the batch omits,
/// and the root when the batch does not hold it. The batch holds exactly the named keys, the
/// snapshot deltas (kind 30) included: a landing and its delta may travel in separate batches of
/// one upload. At most [`MAX_KEYS`] keys count; a repeated key counts once. A key that the
/// root's history, as `read` captured it, does not hold is
/// `E_UNRESOLVED_REF` for that key, and nothing is written. A root that `read` does not hold is
/// `E_UNRESOLVED_REF`; a purpose other than canon_history or view_history is `E_STREAM_PURPOSE`.
/// It checks the redaction epoch before and after, as [`write_history`] does.
pub fn write_history_keys(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    keys: &[ObjectKey],
    out: impl Write,
) -> Result<BatchReport, RdyError> {
    let wanted: BTreeSet<ObjectKey> = keys.iter().copied().collect();
    if wanted.len() > MAX_KEYS {
        return Err(key_limit());
    }
    read.ensure_current_epoch()?;
    Selection::build(read, purpose, root)?.write_keys(read, keys, out)
}

thread_local! {
    /// The history walks this thread's calls made, and the whole-history closures they kept.
    static WALKS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// For tests: how many history walks (`history_closure_before`) the calls of this module made on
/// this thread, and how many whole-history closures with their key sets (a [`PreparedPull`]'s,
/// and the ones the one-call forms build for their own call) they kept, in all.
#[doc(hidden)]
pub fn walks_for_tests() -> (u64, u64) {
    WALKS.get()
}

fn count_walk() {
    let (walks, selections) = WALKS.get();
    WALKS.set((walks + 1, selections));
}

fn count_selection() {
    let (walks, selections) = WALKS.get();
    WALKS.set((walks, selections + 1));
}

/// [`history_closure_before`], counted.
fn walk_history(
    reader: &rdy_store::StoreReader,
    purpose: StreamPurpose,
    root: Hash,
    known: &BTreeSet<ObjectKey>,
    base: Option<Hash>,
    end: Location,
) -> Result<HistoryClosure, StreamError> {
    count_walk();
    history_closure_before(reader, purpose, root, known, base, end)
}

/// The frame header of a segment frame: a length, a kind and a hash.
const FRAME_HEADER: u64 = 4 + 1 + 32;

/// The size of a batch of `frames` frames that hold `payload` bytes in all, with `prereqs`
/// prerequisites and a header whose features are `manifest` bytes of JSON.
fn batch_bytes(manifest: usize, prereqs: usize, frames: u64, payload: u64) -> u64 {
    let header = (HEADER_FIXED + manifest + prereqs * KEY_LEN) as u64;
    MAGIC.len() as u64
        + 4
        + FRAME_HEADER
        + header
        + frames * FRAME_HEADER
        + payload
        + FRAME_HEADER
        + TRAILER_LEN as u64
}

/// One history, walked once as `read` captured it: its records in dependency order (the
/// closure of its root with nothing known and no base), the features they need, and the index
/// of each record's place. Nothing else of a record is kept. A pull reads pages and subsets
/// from it, and writes the subsets' records from the store at the captured end.
struct Selection {
    purpose: StreamPurpose,
    root: Hash,
    found: HistoryClosure,
    index: HashMap<ObjectKey, usize>,
}

impl Selection {
    /// Walks the history of `root` as `read` holds it. The root and the purpose are refused as
    /// [`write_history`] refuses them.
    fn build(read: &ReadState, purpose: StreamPurpose, root: Hash) -> Result<Selection, RdyError> {
        read.ensure_current_epoch()?;
        let found = walk_history(
            read.reader(),
            purpose,
            root,
            &BTreeSet::new(),
            None,
            read.seq(),
        )
        .map_err(to_rdy)?;
        count_selection();
        let grow = |_| to_rdy(StreamError::allocation(None, 0));
        let mut index = HashMap::new();
        index.try_reserve(found.records.len()).map_err(grow)?;
        for (i, k) in found.records.iter().enumerate() {
            index.insert(*k, i);
        }
        read.ensure_current_epoch()?;
        Ok(Selection {
            purpose,
            root,
            found,
            index,
        })
    }

    fn root_key(&self) -> ObjectKey {
        ObjectKey {
            kind: self.purpose.root_kind(),
            hash: self.root,
        }
    }

    /// The page of at most `limit` keys after `after`, from the captured order.
    fn page(
        &self,
        read: &ReadState,
        after: Option<ObjectKey>,
        limit: usize,
    ) -> Result<KeyPage, RdyError> {
        if limit == 0 || limit > MAX_KEYS {
            return Err(key_limit());
        }
        read.ensure_current_epoch()?;
        let start = match after {
            None => 0,
            Some(k) => match self.index.get(&k) {
                Some(i) => i + 1,
                None => return Err(to_rdy(StreamError::unresolved(0, 0, k))),
            },
        };
        let rest = &self.found.records[start..];
        let keys: Vec<ObjectKey> = rest.iter().take(limit).copied().collect();
        let next = if rest.len() > keys.len() {
            keys.last().copied()
        } else {
            None
        };
        read.ensure_current_epoch()?;
        Ok(KeyPage { keys, next })
    }

    /// The batch of exactly the records `keys` name: in the dependency order of that set
    /// alone, with the root and every link or proof a record needs and the set leaves out as
    /// prerequisites, and the features of the whole history. A key a landing's delta shares
    /// travels only when it is named.
    fn write_keys(
        &self,
        read: &ReadState,
        keys: &[ObjectKey],
        out: impl Write,
    ) -> Result<BatchReport, RdyError> {
        let wanted: BTreeSet<ObjectKey> = keys.iter().copied().collect();
        if wanted.len() > MAX_KEYS {
            return Err(key_limit());
        }
        read.ensure_current_epoch()?;
        if let Some(k) = wanted.iter().find(|k| !self.index.contains_key(k)) {
            return Err(to_rdy(StreamError::unresolved(0, 0, *k)));
        }
        let (found, bytes) = self.subset(read, &wanted)?;
        read.ensure_current_epoch()?;
        let report = emit_history(read, self.purpose, self.root, found, out)?;
        debug_assert_eq!(
            bytes,
            report.bytes + FRAME_HEADER + TRAILER_LEN as u64,
            "the size check measures the batch"
        );
        Ok(report)
    }

    /// The records of `wanted` (keys of this history) as a batch: each read for its links and
    /// proof, no other record; and the batch's size, which is checked against the stream bound.
    fn subset(
        &self,
        read: &ReadState,
        wanted: &BTreeSet<ObjectKey>,
    ) -> Result<(HistoryClosure, u64), RdyError> {
        let reader = read.reader();
        let end = read.seq();
        let features = reader.store_features()?;
        let mut source = StoreSource { reader, end };
        let mut children: BTreeMap<ObjectKey, Vec<ObjectKey>> = BTreeMap::new();
        let mut proofs: Vec<Option<ObjectKey>> = Vec::with_capacity(wanted.len());
        let mut payload = 0u64;
        for k in wanted {
            let record = source
                .record(*k)
                .map_err(to_rdy)?
                .ok_or_else(|| to_rdy(StreamError::unresolved(0, 0, *k)))?;
            let refs = record
                .refs(&features)
                .map_err(|e| to_rdy(StreamError::from_record(0, 0, &e).with_object(Some(*k))))?;
            let mut kids = Vec::with_capacity(refs.len());
            for r in refs {
                kids.push(self.resolve(*k, r).map_err(to_rdy)?);
            }
            kids.sort_unstable();
            kids.dedup();
            payload += record.frame_len();
            proofs.push(record.proof());
            children.insert(*k, kids);
        }
        let records = dependency_order(self.root_key(), &children).map_err(to_rdy)?;
        // what the batch's records need or name as a proof, and the batch leaves out
        let mut prerequisites: BTreeSet<ObjectKey> = BTreeSet::new();
        let limit = || to_rdy(StreamError::limit(None, 0, LIMIT_PREREQUISITES));
        for (kids, proof) in children.values().zip(&proofs) {
            for c in kids.iter().chain(proof) {
                if !children.contains_key(c) {
                    prerequisites.insert(*c);
                    if prerequisites.len() > MAX_PREREQUISITES {
                        return Err(limit());
                    }
                }
            }
        }
        if !children.contains_key(&self.root_key()) {
            prerequisites.insert(self.root_key());
        }
        if prerequisites.len() > MAX_PREREQUISITES {
            return Err(limit());
        }
        let bytes = batch_bytes(
            self.found.features.to_json().len(),
            prerequisites.len(),
            wanted.len() as u64,
            payload,
        );
        if bytes > MAX_STREAM_BYTES {
            return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
        }
        Ok((
            HistoryClosure {
                records,
                prerequisites: prerequisites.into_iter().collect(),
                // every batch of one history lists the features its whole closure needs, so
                // the batches of one get carry one header, as the files of one upload must
                // (`batch header differs`)
                features: self.found.features.clone(),
            },
            bytes,
        ))
    }

    /// The key a link of record `from` names: its own kind, or for a kind-None link the one
    /// kind of its hash that the captured history holds (the walk has refused an ambiguous or
    /// foreign link already). A hash the history does not hold is kind 0, a prerequisite.
    fn resolve(&self, from: ObjectKey, r: ObjectRef) -> Result<ObjectKey, StreamError> {
        if let Some(kind) = r.kind {
            return Ok(ObjectKey { kind, hash: r.hash });
        }
        let mut found = None;
        for &kind in sync_kinds() {
            let key = ObjectKey { kind, hash: r.hash };
            if self.index.contains_key(&key) {
                if found.is_some() {
                    return Err(StreamError::bad_record(0, 0, Reason::AmbiguousObjectKind)
                        .with_object(Some(from)));
                }
                found = Some(key);
            }
        }
        Ok(found.unwrap_or(ObjectKey {
            kind: 0,
            hash: r.hash,
        }))
    }
}

/// One caller-owned read operation of a history: a first pull (D-H0BZN). It walks the history
/// of `root` once, in [`PreparedPull::new`], and keeps that closure with its key set and the
/// read state that captured it. Its known pages and every get read that closure, so a pull of
/// many gets walks the history once, and the pages and gets of one pull use one canon tip and
/// one set of view heads. It reads up to the state's captured log end and checks the redaction
/// epoch before and after each call, as [`write_history`] does: a record, signature, answer or
/// removal stored later is not in this pull, and a removal refuses the next call
/// (`E_REDACTED_STATE`).
///
/// The caller owns it: one operation, no process cache and no file. Dropping it releases the
/// captured state. Only a history with no base is prepared; an incremental read keeps its own
/// walk ([`history_page`] with a base).
pub struct PreparedPull {
    read: Arc<ReadState>,
    selection: Selection,
}

impl fmt::Debug for PreparedPull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedPull")
            .field("purpose", &self.selection.purpose)
            .field("root", &self.selection.root)
            .field("records", &self.selection.found.records.len())
            .finish_non_exhaustive()
    }
}

impl PreparedPull {
    /// Walks the canon history (`purpose` canon_history, `root` a landing) or view history
    /// (view_history, `root` a view op) of `root` that `read` holds, once, with nothing known
    /// and no base. Another purpose is `E_STREAM_PURPOSE`; a root `read` does not hold is
    /// `E_UNRESOLVED_REF`; the walk's own refusals (a record that does not decode, a missing
    /// dependency, the bounds of one upload) are [`write_history`]'s.
    pub fn new(
        read: Arc<ReadState>,
        purpose: StreamPurpose,
        root: Hash,
    ) -> Result<PreparedPull, RdyError> {
        let selection = Selection::build(&read, purpose, root)?;
        Ok(PreparedPull { read, selection })
    }

    /// A page of at most `limit` keys (1 to [`MAX_KEYS`]) after `after` (from the start when
    /// None), in the order [`write_history`] writes the whole history; `next` is the page's
    /// last key when more follow. An `after` outside the history is `E_UNRESOLVED_REF`.
    pub fn page(&self, after: Option<ObjectKey>, limit: usize) -> Result<KeyPage, RdyError> {
        self.selection.page(&self.read, after, limit)
    }

    /// Writes exactly the records that `keys` name as one complete format-4 batch, in the
    /// order [`write_history_keys`] writes that set. At most [`MAX_KEYS`] distinct keys count;
    /// a key outside the history is `E_UNRESOLVED_REF` and nothing is written.
    pub fn write_keys(&self, keys: &[ObjectKey], out: impl Write) -> Result<BatchReport, RdyError> {
        self.selection.write_keys(&self.read, keys, out)
    }

    /// [`PreparedPull::write_keys`] for a get reply: the batch goes to an anonymous scratch
    /// file, then as one zstd frame (`accept_zstd`, [`zstd_frame`]; the identity bytes when the
    /// frame is not smaller) or as the batch's own bytes. The report counts the batch's raw
    /// bytes; `wire_bytes` counts what the reply sends. No source or scratch error produces a
    /// reply.
    pub fn write_keys_encoded(
        &self,
        keys: &[ObjectKey],
        accept_zstd: bool,
    ) -> Result<EncodedHistory, RdyError> {
        let scratch = || {
            RdyError::new(
                E_IO,
                "the upload scratch file could not be used",
                "check the temporary directory and available disk space",
            )
        };
        let file = tempfile::tempfile().map_err(|_| scratch())?;
        let mut out = BufWriter::with_capacity(UPLOAD_BUFFER_BYTES, file);
        let report = self.write_keys(keys, &mut out)?;
        let mut file = out.into_inner().map_err(|_| scratch())?;
        let len = file.stream_position().map_err(|_| scratch())?;
        file.seek(SeekFrom::Start(0)).map_err(|_| scratch())?;
        if accept_zstd {
            let (body, wire_bytes, encoding) = zstd_frame(&mut file, len)?;
            return Ok(EncodedHistory {
                body,
                wire_bytes,
                encoding,
                report,
            });
        }
        Ok(EncodedHistory {
            body: file,
            wire_bytes: len,
            encoding: BodyEncoding::Identity,
            report,
        })
    }
}

/// A get reply: the batch of a prepared pull's keys as it goes on the wire.
pub struct EncodedHistory {
    /// The reply body, read from its start: an anonymous scratch file.
    pub body: File,
    /// The body's bytes on the wire: the `Content-Length`.
    pub wire_bytes: u64,
    /// How the body is encoded: [`BodyEncoding::Zstd`] sends `Content-Encoding: zstd`.
    pub encoding: BodyEncoding,
    /// What the batch holds; its `bytes` count the batch before encoding.
    pub report: BatchReport,
}

impl fmt::Debug for EncodedHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodedHistory")
            .field("wire_bytes", &self.wire_bytes)
            .field("encoding", &self.encoding)
            .field("records", &self.report.records)
            .finish_non_exhaustive()
    }
}

/// A source that notes the features of every record it hands to the writer.
struct Features<S> {
    inner: S,
    seen: RecordFeatures,
}

impl<S: ObjectSource> ObjectSource for Features<S> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let r = self.inner.record(key)?;
        if let Some(r) = &r {
            self.seen.add(r);
        }
        Ok(r)
    }

    fn snapshot_delta_of(
        &mut self,
        landing: &rdy_model::Hash,
    ) -> Result<Option<rdy_model::Hash>, StreamError> {
        self.inner.snapshot_delta_of(landing)
    }
}
