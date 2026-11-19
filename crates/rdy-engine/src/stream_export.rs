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
use crate::error::{E_INTERNAL, E_NOT_FOUND, RdyError};
use crate::history_facts::{MAX_KEYS, key_limit};
use crate::read_state::ReadState;
use crate::stream_error::to_rdy;
use rdy_model::Hash;
use rdy_store::closure::{
    HistoryClosure, StoreSource, closure_before, history_closure_before,
    history_closure_named_before, write_batch,
};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{
    BatchReport, RecordFeatures, StreamHeader, StreamPurpose, StreamRecord,
};
use std::collections::BTreeSet;
use std::io::Write;

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
    let found = history_closure_before(reader, purpose, root, known, base, end).map_err(to_rdy)?;
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
    let found = history_closure_before(reader, purpose, root, &BTreeSet::new(), base, end)
        .map_err(to_rdy)?;
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
    let reader = read.reader();
    let end = read.seq();
    let whole = history_closure_before(reader, purpose, root, &BTreeSet::new(), None, end)
        .map_err(to_rdy)?;
    let held: BTreeSet<ObjectKey> = whole.records.iter().copied().collect();
    if let Some(k) = wanted.iter().find(|k| !held.contains(k)) {
        return Err(to_rdy(StreamError::unresolved(0, 0, *k)));
    }
    // the closure less everything not named: the walk goes through what it leaves out, so the
    // batch holds exactly the named records. A named delta of a landing the batch leaves out
    // is reached on its own; it names that landing as a prerequisite. An unnamed delta of a
    // named landing stays out and is a later batch's (the upload holds one delta per landing
    // across its batches, D-7K4Z0).
    let known: BTreeSet<ObjectKey> = held.difference(&wanted).copied().collect();
    let deltas: Vec<ObjectKey> = wanted
        .iter()
        .copied()
        .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
        .collect();
    let mut found = history_closure_named_before(reader, purpose, root, &known, &deltas, end)
        .map_err(to_rdy)?;
    // every batch of one history lists the features its whole closure needs, so the batches of
    // one get carry one header, as the files of one upload must (`batch header differs`)
    found.features = whole.features;
    let emitted: BTreeSet<ObjectKey> = found.records.iter().copied().collect();
    if emitted != wanted {
        return Err(RdyError::new(
            E_INTERNAL,
            "the named keys are not the records of their batch",
            "report this as a bug in rdy",
        ));
    }
    emit_history(read, purpose, root, found, out)
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
