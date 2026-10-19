//! The stateless batch check (D-D2NPN verify-on-receive; hub i-QD955, i-VZ93S, r-X13HC,
//! r-E0RJX): one sync batch, read once from any reader, checked whole, and only then its keys.
//!
//! The check opens no store, reads no config or pack, writes no file and keeps no state between
//! calls: a new process can make each call (r-E0RJX). It holds one record buffer at a time (the
//! stream reader's), checks each record as it is read, and keeps only the batch's keys and
//! edges for its dependency rules. A failed batch returns no report, so the hub keeps nothing
//! of it. The hub owns the files of the batches that pass (D-WDKWQ); core neither renames nor
//! deletes them.
//!
//! A report says this batch's framing and objects are valid, never that its upload is complete
//! or a removal authorized. A record that another batch holds is a prerequisite here; the take
//! resolves prerequisites, the whole closure, removal proofs and every landing snapshot's limits
//! over all the upload's files.
use crate::error::{E_REPO_MISMATCH, RdyError};
use crate::publication;
use crate::stream_error::{Reason, StreamError, to_rdy};
use rdy_model::{Block, RepoId};
use rdy_store::bounded;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{self, StreamHeader, StreamRecord};
use std::io::Read;

pub use rdy_store::sync_stream::BatchReport;

/// Why a check stopped: the stream's own error, or an engine error that is already final.
enum Stop {
    Stream(StreamError),
    Core(RdyError),
}

impl From<StreamError> for Stop {
    fn from(e: StreamError) -> Stop {
        Stop::Stream(e)
    }
}

/// Checks one complete batch read from `input` and returns its report: its header, the logical
/// key of every object record in wire order, and its trailer's records, bytes and digest. The
/// report comes only after the framing, every hash, each record's bounded typed decode and local
/// limits, the batch's dependency rules, the trailer and the end of the input all pass.
///
/// With `expected_repo`, the header's repo id must be that one (E_REPO_MISMATCH, checked as soon
/// as the header is read). A new holder passes None, and is then created with the report's repo
/// id.
///
/// Each record's local limits are checked at once: a block's text, reference occurrence, field
/// depth and graph limits (the part of [`publication::check_snapshot_limits`] that needs no
/// other block), and a blob manifest's claimed length (the publication blob limit). A record
/// over one of them is refused with the publication limit's own text, positioned at the record.
/// A reference row's location and token bounds apply only when the snapshot lacks the target,
/// so the take checks them with each snapshot. A blob manifest must also be its own canonical
/// encoding with the chunk count its length gives, as a blob read requires (E_BAD_RECORD).
pub fn check_batch<R: Read>(
    input: R,
    expected_repo: Option<RepoId>,
) -> Result<BatchReport, RdyError> {
    let header = |h: &StreamHeader| match expected_repo {
        Some(target) if h.repo_id != target => Err(Stop::Core(repo_mismatch(&h.repo_id, &target))),
        _ => Ok(()),
    };
    sync_stream::check_batch_with(input, header, check_record).map_err(|e| match e {
        Stop::Stream(e) => to_rdy(e),
        Stop::Core(e) => e,
    })
}

/// E_REPO_MISMATCH: a stream of repo `source` offered to the holder of repo `target`.
fn repo_mismatch(source: &RepoId, target: &RepoId) -> RdyError {
    RdyError::new(
        E_REPO_MISMATCH,
        format!("stream repo {source} does not match holder repo {target}"),
        "use the holder for the stream's repo",
    )
}

/// The local limits of record `n`, whose frame starts at byte `at`. A record of another kind,
/// or a dropped one, has no local limit beyond its frame and typed decode.
fn check_record(n: u64, at: u64, r: &StreamRecord) -> Result<(), Stop> {
    let StreamRecord::Present { key, payload } = r else {
        return Ok(());
    };
    let bad = |reason| Stop::Stream(StreamError::bad_record(n, at, reason).with_object(Some(*key)));
    // the stream check's typed decode of these bytes already passed; this one reads the value
    let decoded = |e| Stop::Stream(StreamError::from_record(n, at, &e).with_object(Some(*key)));
    match key.kind {
        kinds::BLOCK => {
            let b: Block = bounded::decode(payload).map_err(decoded)?;
            publication::check_block_limits(&b).map_err(|e| positioned(n, at, *key, e))
        }
        kinds::BLOB_MANIFEST => {
            let m: BlobManifest = bounded::decode(payload).map_err(decoded)?;
            // the bytes `put_blob` writes, which a blob read requires: no other encoding of
            // the same manifest (such as a varint that is not minimal) is a blob
            if !m.is_encoding_of(payload) {
                return Err(bad(Reason::RecordDoesNotDecode));
            }
            let l = publication::limits();
            if m.len > l.blob {
                let e = publication::limit("blob", l.blob, "bytes");
                return Err(positioned(n, at, *key, e));
            }
            // the chunks `put_blob` writes: one per started chunk of the claimed length
            if !m.chunks_fit() {
                return Err(bad(Reason::RecordDoesNotDecode));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `e`, an engine limit error of record `n` at byte `at` with key `key`, with its position as
/// the stream conversion gives one: the same code, message and fix, and one positioned issue.
fn positioned(n: u64, at: u64, key: ObjectKey, e: RdyError) -> Stop {
    Stop::Core(to_rdy(StreamError {
        code: e.code,
        message: e.message,
        fix: e.fix,
        record: Some(n),
        offset: at,
        object: Some(key),
    }))
}
