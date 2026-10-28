//! The export of one tag's history as a sync batch (D-D2NPN, D-Q0AVZ).
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
use rdy_store::closure::{StoreSource, closure_before, write_batch};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{BatchReport, StreamHeader};
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
    let mut source = StoreSource { reader, end };
    let (_, totals) = write_batch(&mut source, &header, &found.records, out).map_err(to_rdy)?;
    read.ensure_current_epoch()?;
    Ok(BatchReport {
        header,
        keys: found.records,
        records: totals.records,
        bytes: totals.bytes,
        digest: totals.digest,
    })
}
