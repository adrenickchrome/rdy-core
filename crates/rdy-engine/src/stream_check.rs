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
use rdy_model::RepoId;
use rdy_store::decoded_record::DecodedRecord;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{self, StreamHeader, StreamRecord};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

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
    check_batch_tally(input, expected_repo, |_, _, _| {})
}

/// [`check_batch`], telling `tally` each record's kind, frame start and frame payload bytes in
/// stream order (a fetch sizes its next get from them, TASK-3869).
pub fn check_batch_tally<R: Read>(
    input: R,
    expected_repo: Option<RepoId>,
    tally: impl FnMut(u8, u64, u64),
) -> Result<BatchReport, RdyError> {
    checked(input, expected_repo, tally, |input, header, record| {
        sync_stream::check_batch_with(input, header, record)
    })
}

/// One batch the engine's check passed whole: the store's checked batch
/// ([`sync_stream::CheckedBatch`]), made only after the repo and every local publication limit
/// passed too. It cannot be built from JSON, from a report or from a store's checked batch
/// alone, so a store check never stands in for the engine's limits.
#[derive(Debug)]
pub struct CheckedBatch {
    store: sync_stream::CheckedBatch,
}

impl CheckedBatch {
    /// The batch's report, as [`check_batch`] returns it.
    pub fn report(&self) -> &BatchReport {
        self.store.report()
    }

    /// The batch's report, the rest dropped.
    pub fn into_report(self) -> BatchReport {
        self.store.into_report()
    }

    /// What the store's check found, for the store stage that takes the batch.
    pub fn store_checked(&self) -> &sync_stream::CheckedBatch {
        &self.store
    }
}

/// [`check_batch_tally`] that also keeps what the check found ([`CheckedBatch`]), for a later
/// stage of the same operation. Its checks, their order and their refusals are
/// [`check_batch_tally`]'s.
pub fn check_batch_prepared_tally<R: Read>(
    input: R,
    expected_repo: Option<RepoId>,
    tally: impl FnMut(u8, u64, u64),
) -> Result<CheckedBatch, RdyError> {
    checked(input, expected_repo, tally, |input, header, record| {
        sync_stream::check_batch_prepared_with(input, header, record)
    })
    .map(|store| CheckedBatch { store })
}

/// One batch file that the engine's check passed whole ([`CheckedBatch`]), with the open file it
/// read: a capsule of one operation (a push's quarantine, a fetch's take), never stored or
/// serialized. It is made only by [`check_batch_file`], so a report alone, or a check of
/// other bytes, never stands for it; a later stage that reuses its check first reads the
/// file's bytes again and compares them with the check's binding.
///
/// ```compile_fail
/// use rdy_engine::stream_check::CheckedBatchFile;
/// fn forge(file: std::fs::File, report: rdy_engine::stream_check::BatchReport) -> CheckedBatchFile {
///     CheckedBatchFile { file, checked: report }
/// }
/// ```
#[derive(Debug)]
pub struct CheckedBatchFile {
    file: File,
    checked: CheckedBatch,
}

impl CheckedBatchFile {
    /// The batch's report, as [`check_batch`] returns it.
    pub fn report(&self) -> &BatchReport {
        self.checked.report()
    }

    /// The file the check read. A reader of it seeks to its start.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// What the store's check found, for the store stage that takes the file.
    pub fn store_checked(&self) -> &sync_stream::CheckedBatch {
        self.checked.store_checked()
    }

    /// `E_REPO_MISMATCH` unless the batch's repo is `repo`, as [`check_batch`] refuses a batch of
    /// another repo than the expected one.
    pub fn ensure_repo(&self, repo: RepoId) -> Result<(), RdyError> {
        let source = self.report().header.repo_id;
        if source != repo {
            return Err(repo_mismatch(&source, &repo));
        }
        Ok(())
    }
}

/// Checks the batch in `file`, read from its start, as [`check_batch_tally`] checks it (`tally`
/// gets each record's kind, frame start and payload bytes), and keeps the open file with what
/// the check found ([`CheckedBatchFile`]). The checks, their order and their refusals are
/// [`check_batch_tally`]'s.
pub fn check_batch_file(
    mut file: File,
    expected_repo: Option<RepoId>,
    tally: impl FnMut(u8, u64, u64),
) -> Result<CheckedBatchFile, RdyError> {
    file.seek(SeekFrom::Start(0)).map_err(stream_io)?;
    let checked = check_batch_prepared_tally(BufReader::new(&mut file), expected_repo, tally)?;
    Ok(CheckedBatchFile { file, checked })
}

/// An I/O error of a batch file: the stream's own error at its start.
fn stream_io(e: std::io::Error) -> RdyError {
    to_rdy(StreamError::io(Some(0), 0, &e))
}

/// The engine's check of one batch through the store check `check`: the repo at the header,
/// then each record's tally and local limits on the store check's decoded value.
fn checked<R: Read, T>(
    input: R,
    expected_repo: Option<RepoId>,
    mut tally: impl FnMut(u8, u64, u64),
    check: impl FnOnce(
        R,
        &dyn Fn(&StreamHeader) -> Result<(), Stop>,
        &mut dyn FnMut(u64, u64, &StreamRecord, &DecodedRecord) -> Result<(), Stop>,
    ) -> Result<T, Stop>,
) -> Result<T, RdyError> {
    let header = |h: &StreamHeader| match expected_repo {
        Some(target) if h.repo_id != target => Err(Stop::Core(repo_mismatch(&h.repo_id, &target))),
        _ => Ok(()),
    };
    let mut record = |n: u64, at: u64, r: &StreamRecord, d: &DecodedRecord| {
        tally(r.key().kind, at, r.frame_len());
        check_record(n, at, r, d)
    };
    check(input, &header, &mut record).map_err(|e| match e {
        Stop::Stream(e) => to_rdy(e),
        Stop::Core(e) => e,
    })
}

/// Checks the files of one canon-state fetch (stream format 5, D-GGTYA): each file whole, as
/// [`check_batch`] checks a history (the framing, each record's bounded typed decode and local
/// limits, its dependencies and its prerequisites), then the files together against the
/// state's link table (`rdy_store::canon_state::StateUnion`): the root landing, its canonical
/// SNAPSHOT and its one delta, the map's block ids, and every never-absent link within the
/// union. Every file must carry one header (purpose, repo, root and features; `batch header
/// differs` otherwise). Another purpose is `E_STREAM_PURPOSE`. Returns each file's report, in
/// order, only when every check passes.
pub fn check_state_batches<R: Read>(
    inputs: Vec<R>,
    expected_repo: Option<RepoId>,
) -> Result<Vec<BatchReport>, RdyError> {
    use rdy_store::canon_state::StateUnion;
    use std::cell::RefCell;
    if inputs.is_empty() {
        return Err(to_rdy(StreamError::bad_record(0, 0, Reason::NoBatchFiles)));
    }
    let state: RefCell<(Option<StreamHeader>, Option<StateUnion>)> = RefCell::new((None, None));
    let mut reports = Vec::new();
    for input in inputs {
        let checked = checked(
            input,
            expected_repo,
            |_, _, _| {},
            |input, header, record| {
                sync_stream::check_canon_state_prepared_with(
                    input,
                    |h: &StreamHeader| {
                        header(h)?;
                        let mut s = state.borrow_mut();
                        match &s.0 {
                            Some(first)
                                if (first.repo_id, first.tag, &first.features)
                                    != (h.repo_id, h.tag, &h.features) =>
                            {
                                return Err(Stop::Stream(StreamError::bad_record(
                                    0,
                                    0,
                                    Reason::BatchHeaderDiffers,
                                )));
                            }
                            Some(_) => {}
                            None => {
                                s.1 = Some(StateUnion::new(h.tag, &h.features));
                                s.0 = Some(h.clone());
                            }
                        }
                        Ok(())
                    },
                    |n: u64, at: u64, r: &StreamRecord, d: &DecodedRecord| {
                        record(n, at, r, d)?;
                        let mut s = state.borrow_mut();
                        let (Some(h), Some(union)) = (&s.0.clone(), s.1.as_mut()) else {
                            return Ok(());
                        };
                        union.add(n, at, &h.features, r, d).map_err(Stop::Stream)
                    },
                )
            },
        )?;
        reports.push(checked.into_report());
    }
    let (_, union) = state.into_inner();
    union
        .expect("a checked file has a header")
        .finish()
        .map_err(to_rdy)?;
    Ok(reports)
}

/// E_REPO_MISMATCH: a stream of repo `source` offered to the holder of repo `target`.
fn repo_mismatch(source: &RepoId, target: &RepoId) -> RdyError {
    RdyError::new(
        E_REPO_MISMATCH,
        format!("stream repo {source} does not match holder repo {target}"),
        "use the holder for the stream's repo",
    )
}

/// The local limits of record `n`, whose frame starts at byte `at`, read from `d`, the store
/// check's one decoded value of it. A record of another kind, or a dropped one, has no local
/// limit beyond its frame and typed decode.
fn check_record(n: u64, at: u64, r: &StreamRecord, d: &DecodedRecord) -> Result<(), Stop> {
    let StreamRecord::Present { key, payload, .. } = r else {
        return Ok(());
    };
    let bad = |reason| Stop::Stream(StreamError::bad_record(n, at, reason).with_object(Some(*key)));
    match d {
        DecodedRecord::Block(b) => {
            publication::check_block_limits(b).map_err(|e| positioned(n, at, *key, e))
        }
        DecodedRecord::BlobManifest(m) => {
            let decoded =
                |e| Stop::Stream(StreamError::from_record(n, at, &e).with_object(Some(*key)));
            let body = rdy_store::codec::split(r.raw_kind(), payload)
                .map_err(decoded)?
                .body;
            // the bytes `put_blob` writes, which a blob read requires: no other encoding of
            // the same manifest (such as a varint that is not minimal) is a blob
            if !m.is_encoding_of(body) {
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
