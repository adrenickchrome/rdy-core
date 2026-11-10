//! The citing side of a cited project's removal (repo links; D-CN4QJ, D-3M81W): the take of the
//! portable removal proofs one stream carries into a project's foreign copy, and the count of
//! the foreign bytes a project holds for one cited repo.
//!
//! [`take_foreign_removal`] runs under the store's one writer. Before anything is written it
//! checks:
//!
//! 1. The stream's length, within the stream byte bound (`E_LIMIT`). The adapter checks it
//!    before it clones the caller's handle.
//! 2. The store is a project (`this store is a holder`), and `repo` is not this project's own
//!    (`a project cannot cite itself`).
//! 3. The whole stream (`rdy_store::stream_source::CheckedStream`): a complete `tag_history`
//!    batch of the cited project (another purpose is `E_STREAM_PURPOSE`), with every rule of a
//!    take with nothing held, its approved removals checked and no change their approvals leave
//!    uncovered (`E_REDACTION_STALE`). Its header names `repo` (`E_REPO_MISMATCH`).
//!
//! Then each `REDACTION_APPLY` the stream holds, in key order, goes to the store's own take
//! (`Store::take_foreign_removal`), which checks its proof against `repo` and seals one batch
//! per proof that erases a held foreign copy. Proof verification is the store's alone. Right
//! before each store take the caller's file is checked again (the same file identity and
//! length); each record the store reads is read again from the file and checked against the
//! key and length the whole check found (`input changed during take` otherwise).
//!
//! A proof that erases nothing writes nothing; its versions are reported. A proof the stream
//! carries that fails its check is `E_REDACTION_CORRUPT` with the portable-proof message and
//! fix; a record its proof needs that the stream lacks stays `E_UNRESOLVED_REF`. A failure after
//! an earlier proof of the same stream sealed its batch leaves that batch: each sealed batch is
//! its own authority, and taking the stream again reports its versions as already removed.
//!
//! [`foreign_bytes`] counts what a project retains for one cited repo: every FOREIGN entry of the
//! repo (deliveries, revocations and removals, on live and revoked sources) and every BLOCK
//! frame a delivery of it holds, an erased one too, so a host's leak guard never undercounts.
use crate::Rdy;
use crate::error::{E_IO, E_REDACTION_CORRUPT, E_REPO_MISMATCH, RdyError};
use crate::foreign_delivery::cites_itself;
use crate::holder::is_a_holder;
use crate::read_state::ReadState;
use crate::stream_error::to_rdy;
use crate::stream_take::{StorePosition, republish, uncovered_change};
use rdy_model::{BlockId, Hash, RepoId};
use rdy_store::StoreError;
use rdy_store::fileid::FileId;
use rdy_store::foreign_removal::{ForeignProof, ForeignRemovalError};
use rdy_store::kinds;
use rdy_store::stream_error::{LIMIT_TOTAL_BYTES, Reason, StreamError};
use rdy_store::stream_source::CheckedStream;
use rdy_store::sync_stream::MAX_STREAM_BYTES;
use std::fs::File;

/// What the take did with one removal proof the stream carries, each list in `(id, version)`
/// order: `applied` (erased here by this take), `missing` (a version the proof approves that no
/// delivery of the repo holds here, or whose BLOCK is not stored), `kept_local` (local content
/// of the same version, never erased) and `already_removed` (an earlier removal erased it).
/// `entry` is the new FOREIGN `Removal` entry, None when this proof wrote nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovalTaken {
    pub apply: Hash,
    pub entry: Option<Hash>,
    pub applied: Vec<(BlockId, Hash)>,
    pub missing: Vec<(BlockId, Hash)>,
    pub kept_local: Vec<(BlockId, Hash)>,
    pub already_removed: Vec<(BlockId, Hash)>,
}

/// What one removal take did: the cited repo, one row per removal proof of the stream (in apply
/// key order), where the log ends, and whether nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRemovalTaken {
    pub repo: RepoId,
    pub removals: Vec<RemovalTaken>,
    pub end: StorePosition,
    pub unchanged: bool,
}

/// E_IO: the caller's stream file cannot be read.
fn io(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("stream input: {e}"),
        "check that the stream file can be read",
    )
}

/// E_BAD_RECORD: the caller's file changed after the take checked it.
fn changed_input() -> RdyError {
    to_rdy(StreamError::bad_record(
        0,
        0,
        Reason::InputChangedDuringTake,
    ))
}

/// E_REPO_MISMATCH: a stream of repo `source` offered as the removal of repo `target`.
fn repo_mismatch(source: &RepoId, target: &RepoId) -> RdyError {
    RdyError::new(
        E_REPO_MISMATCH,
        format!("stream repo {source} does not match repo {target}"),
        "take the stream for the repo it names",
    )
}

/// A stream failure as the engine's: a removal proof that fails its check is
/// `E_REDACTION_CORRUPT`, with the portable-proof message and fix; any other keeps its own code.
fn stream_error(e: StreamError) -> RdyError {
    let bad_proof = e.message.ends_with(Reason::InvalidRemovalProof.as_str());
    let mut out = to_rdy(e);
    if bad_proof {
        out.code = E_REDACTION_CORRUPT;
        for issue in &mut out.issues {
            issue.code = E_REDACTION_CORRUPT;
        }
    }
    out
}

/// The store take's refusal as the engine's.
fn take_error(e: ForeignRemovalError) -> RdyError {
    match e {
        ForeignRemovalError::Store(e) => e.into(),
        ForeignRemovalError::Proof(e) => stream_error(e),
        ForeignRemovalError::RepoMismatch { source, target } => repo_mismatch(&source, &target),
    }
}

/// The bound of a take's input, checked before the caller's handle is cloned or any of the
/// stream is read: a stream of at most [`MAX_STREAM_BYTES`] (`stream_len`).
pub fn check_removal_bounds(stream_len: u64) -> Result<(), RdyError> {
    if stream_len > MAX_STREAM_BYTES {
        return Err(to_rdy(StreamError::limit(None, 0, LIMIT_TOTAL_BYTES)));
    }
    Ok(())
}

/// The log's end, as a take reports it.
fn end_of(rdy: &Rdy) -> StorePosition {
    let end = rdy.store.log_end();
    StorePosition {
        segment: end.segment,
        offset: end.offset,
    }
}

/// Takes the removal proofs `stream` carries from the cited repo `repo` into the project `rdy`
/// (see the module docs). The file stays the caller's; it must not change during the call.
pub fn take_foreign_removal(
    rdy: &mut Rdy,
    stream: &File,
    repo: &RepoId,
) -> Result<ForeignRemovalTaken, RdyError> {
    let len = stream.metadata().map_err(io)?.len();
    check_removal_bounds(len)?;
    if rdy.is_holder() {
        return Err(is_a_holder());
    }
    if rdy.store.reader().repo_id()? == Some(*repo) {
        return Err(cites_itself());
    }
    // a take is a batch of its own: one a raw write left open is sealed and published first
    rdy.seal_open()?;
    let file_id = FileId::of_file(stream).map_err(io)?;
    let checked = CheckedStream::open(stream).map_err(stream_error)?;
    if let Some(h) = checked.uncovered_change() {
        return Err(uncovered_change(h));
    }
    let facts = checked.facts();
    if facts.repo_id != *repo {
        return Err(repo_mismatch(&facts.repo_id, repo));
    }
    let applies: Vec<Hash> = facts
        .objects
        .iter()
        .filter(|k| k.kind == kinds::REDACTION_APPLY)
        .map(|k| k.hash)
        .collect();
    let epoch = rdy.store.redaction_epoch();
    let mut removals = Vec::with_capacity(applies.len());
    let mut wrote = false;
    let mut failed: Option<RdyError> = None;
    for apply in applies {
        // the caller's file, once more, right before the store reads and writes
        let same_file = FileId::of_file(stream).is_ok_and(|now| now == file_id)
            && stream.metadata().is_ok_and(|m| m.len() == len);
        if !same_file {
            failed = Some(changed_input());
            break;
        }
        let mut source = &checked;
        // the writer's held batches are off for the take, which seals its own batch, as for the
        // store's other self-sealing writes
        if let Err(e) = rdy.store.set_held(false) {
            failed = Some(e.into());
            break;
        }
        let taken = rdy.store.take_foreign_removal(
            repo,
            ForeignProof {
                repo: facts.repo_id,
                apply,
                source: &mut source,
            },
        );
        let held = rdy.store.set_held(true);
        match taken {
            Ok(t) => {
                wrote |= t.entry.is_some();
                removals.push(RemovalTaken {
                    apply,
                    entry: t.entry,
                    applied: t.erased,
                    missing: t.skipped,
                    kept_local: t.kept_local,
                    already_removed: t.already_removed,
                });
                if let Err(e) = held {
                    failed = Some(e.into());
                    break;
                }
            }
            Err(e) => {
                if matches!(e, ForeignRemovalError::Store(StoreError::RedactionPending)) {
                    wrote = true;
                }
                failed = Some(take_error(e));
                break;
            }
        }
    }
    if wrote {
        // a sealed removal (finished or not) starts the shared caches over, as a native one does
        let published = republish(rdy, epoch);
        if failed.is_none() {
            published?;
        }
    }
    if let Some(e) = failed {
        return Err(e);
    }
    Ok(ForeignRemovalTaken {
        repo: *repo,
        removals,
        end: end_of(rdy),
        unchanged: !wrote,
    })
}

/// The foreign bytes the state `read` retains for the cited repo `repo` (see the module docs), as
/// they stood at its watermark: a later delivery, revocation or removal is not in its count. A
/// state read before a later removal is `E_REDACTED_STATE`, as every content read is.
pub fn foreign_bytes(read: &ReadState, repo: &RepoId) -> Result<u64, RdyError> {
    read.ensure_current_epoch()?;
    let watermark = read.foreign()?.watermark();
    Ok(read.reader().foreign_bytes_before(repo, watermark)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_store::object_refs::ObjectKey;

    /// A proof that fails its check is E_REDACTION_CORRUPT with the portable-proof message and
    /// fix; a missing record and every other stream failure keep their own code.
    #[test]
    fn rpl26_invalid_removal_proof_is_redaction_corrupt() {
        let key = ObjectKey {
            kind: kinds::REDACTION_APPLY,
            hash: Hash([1; 32]),
        };
        let bad = StreamError::bad_record(0, 0, Reason::InvalidRemovalProof).with_object(Some(key));
        let e = stream_error(bad.clone());
        assert_eq!(e.code, E_REDACTION_CORRUPT);
        assert_eq!(e.message, bad.message);
        assert_eq!(e.fix, bad.fix);
        assert!(e.issues.iter().all(|i| i.code == E_REDACTION_CORRUPT));
        let e = take_error(ForeignRemovalError::Proof(bad));
        assert_eq!(e.code, E_REDACTION_CORRUPT);

        let missing = StreamError::unresolved(1, 0, key);
        assert_eq!(stream_error(missing).code, "E_UNRESOLVED_REF");
        let other = StreamError::bad_record(0, 0, Reason::HashMismatch);
        assert_eq!(stream_error(other).code, "E_BAD_RECORD");
        assert_eq!(
            check_removal_bounds(MAX_STREAM_BYTES + 1).unwrap_err().code,
            "E_LIMIT"
        );
        assert!(check_removal_bounds(MAX_STREAM_BYTES).is_ok());
    }
}
