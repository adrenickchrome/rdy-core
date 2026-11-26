//! A project's fetch of a canon or view history from a remote (D-KE83X, D-3RSCW), and the read
//! of the remote's sync state that a sync may use.
//!
//! [`fetch`] takes the batch files of one format-4 history into the project through the same
//! union, closure and proof checks as a holder's take (`rdy_store::take_batch`). It writes one
//! sealed batch: the records the project lacks, `tag/<name>` for each fetched tag whose name is
//! free (with `answer/tag:<name>` for its relay answer, when that ref is free too), and the
//! remote's sync state with the root as its fetched tip or head. It moves no
//! canon or view ref, so the project reads the same canon and views after it. A fetched tag
//! whose name the project uses for another landing is not taken: the local tag stays, and the
//! name is listed. The first sync state write activates the `sync_state` feature in its batch.
//! A retry of a fetch already taken writes nothing.
//!
//! A fetched removal (D-FNRXT) is taken when a person's approval proves it: the fetch applies it
//! in its own batch, erases what it covers here once the batch is durable, and every view then
//! reads the stub. A local change that holds removed content and that no removal covers keeps
//! its bytes; the outcome lists one warning per such change
//! ([`crate::sync_redaction::UncoveredWarning`]), and reads repeat it.
//!
//! [`usable_canon_state`] and [`usable_view_state`] read one remote's state as a sync uses it
//! after the hub's state step: only when the reply's repo id is the state's, and, for canon,
//! when the reply's canon is not null. A changed URL keeps the state. The read never reads
//! another remote name's refs, and it removes nothing.
use crate::Rdy;
use crate::error::{E_BAD_INPUT, E_REPO_MISMATCH, RdyError};
use crate::stream_check::CheckedBatchFile;
use crate::stream_error::to_rdy;
use crate::stream_take::{StorePosition, republish};
use rdy_model::{Hash, RepoId};
use rdy_store::format::RdyVersion;
use rdy_store::sync_state::{SyncCanon, SyncView};
use rdy_store::take_batch::{FetchTarget, TakeError};
use rdy_store::{StoreError, StoreReader};
use std::convert::Infallible;
use std::fs::File;

const _: () = assert!(rdy_store::sync_state::MAX_URL_BYTES == crate::hub_types::MAX_URL_BYTES);

/// What one fetch did: the objects it wrote that the project lacked, its data frames, the tag
/// names it took and those it kept local, the store's minimum rdy before and after it, the
/// log's end after it, the removals it applied and what they removed here, and the warnings of
/// the uncovered local carriers after it. `unchanged` is true when it wrote nothing (a retry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchOutcome {
    pub records_taken: u64,
    pub frames_written: u64,
    pub tags_taken: Vec<String>,
    pub tags_kept: Vec<String>,
    pub min_rdy_before: Option<RdyVersion>,
    pub min_rdy_after: Option<RdyVersion>,
    pub end: StorePosition,
    pub unchanged: bool,
    /// The removal applies the fetch took, in hash order.
    pub removals: Vec<Hash>,
    /// The objects those removals removed: written as drops, or erased here.
    pub redacted_objects: u64,
    pub warnings: Vec<crate::sync_redaction::UncoveredWarning>,
}

/// Takes the batch files `batches` of one fetched history for `target` into the project `rdy`
/// (see the module docs). Every refusal before the append leaves the project unchanged. A
/// holder is refused with `this store is a holder`.
pub fn fetch(
    rdy: &mut Rdy,
    batches: &[File],
    target: &FetchTarget,
) -> Result<FetchOutcome, RdyError> {
    fetch_core(rdy, batches, None, target)
}

/// [`fetch`] of batch files that this operation's own check passed
/// ([`crate::stream_check::check_batch_file`]): the files are not checked again. The take reads
/// each file's bytes once more and keeps its check's result only for the exact bytes that check
/// read (`input changed during take` otherwise); the take's checks over the project, its
/// outcome and its refusals are [`fetch`]'s.
pub fn fetch_checked(
    rdy: &mut Rdy,
    batches: &[CheckedBatchFile],
    target: &FetchTarget,
) -> Result<FetchOutcome, RdyError> {
    let files = batch_files(batches)?;
    let checked: Vec<&rdy_store::sync_stream::CheckedBatch> = batches
        .iter()
        .map(CheckedBatchFile::store_checked)
        .collect();
    fetch_core(rdy, &files, Some(&checked), target)
}

/// A URL clone's take of a canon state (D-GGTYA, D-4PV25): the format-5 batches `batches`, which
/// the caller checked together (`stream_check::check_state_batches`), taken by
/// `Store::take_canon_state`, which checks each again, with the incomplete history ref
/// `history`, in one sealed batch that moves canon to the state's root.
/// The state is then published as a chain move (canon had no head before).
pub(crate) fn take_state(
    rdy: &mut Rdy,
    batches: &[File],
    target: &FetchTarget,
    history: &rdy_store::sync_state::SyncHistory,
) -> Result<FetchOutcome, RdyError> {
    fetch_core_with(rdy, batches, None, Some(history), target, true)
}

/// The completion take of an incomplete history (D-GGTYA, R1): the staged canon history
/// batches `batches` at the frontier `target.root`, each checked once by this operation, in one
/// checked batch (`Store::take_fetched_checked`) that moves no canon or view ref, so local
/// landings made since the state take stay. Its batch carries the remote's sync state, so its
/// plans read as fetched ones.
///
/// The take logs no moves. The history ref is still incomplete, so the state after the take
/// reads the older landings only in part (their canon membership is rebuilt by the
/// completion's reindex): moves computed from it would name canon sides with no landing, and
/// the completion would re-key them. The drift moves the older landings cause are logged once,
/// under cause `recheck`, by the completion ([`complete_history`]), from the state logged here
/// before the take: a handle that logged nothing since it opened logs what moved while no rdy
/// ran first (`Rdy::reconcile_transitions`, as a daemon does after ready).
pub(crate) fn take_history_checked(
    rdy: &mut Rdy,
    batches: &[CheckedBatchFile],
    target: &FetchTarget,
) -> Result<FetchOutcome, RdyError> {
    rdy.reconcile_transitions()?;
    let files = batch_files(batches)?;
    let checked: Vec<&rdy_store::sync_stream::CheckedBatch> = batches
        .iter()
        .map(CheckedBatchFile::store_checked)
        .collect();
    fetch_core_with(rdy, &files, Some(&checked), None, target, false)
}

/// The second and last history ref write (D-GGTYA, R1, N1): `Store::complete_history` of
/// remote `remote` at `frontier` while the redaction epoch is still `epoch` (the values the
/// full check saw), then the state published again over the rebuilt index as a chain move,
/// which logs the moves the older landings cause under cause `recheck` (a batch of its own,
/// as `Rdy::invalidate` logs one). A refusal writes nothing.
pub(crate) fn complete_history(
    rdy: &mut Rdy,
    remote: &str,
    frontier: Hash,
    epoch: u64,
) -> Result<(), RdyError> {
    rdy.seal_open()?;
    // a recheck computes its moves from a logged state (see `take_history_checked`)
    rdy.reconcile_transitions()?;
    rdy.store.complete_history(remote, frontier, epoch)?;
    rdy.publish_chain_moved()
}

/// A second handle on each checked batch's file, for the take.
fn batch_files(batches: &[CheckedBatchFile]) -> Result<Vec<File>, RdyError> {
    let mut files = Vec::with_capacity(batches.len());
    for b in batches {
        files.push(
            b.file()
                .try_clone()
                .map_err(|e| to_rdy(rdy_store::stream_error::StreamError::io(Some(0), 0, &e)))?,
        );
    }
    Ok(files)
}

/// [`fetch`] and [`fetch_checked`]: one take, with the files' checked batches when there are.
fn fetch_core(
    rdy: &mut Rdy,
    batches: &[File],
    checked: Option<&[&rdy_store::sync_stream::CheckedBatch]>,
    target: &FetchTarget,
) -> Result<FetchOutcome, RdyError> {
    fetch_core_with(rdy, batches, checked, None, target, false)
}

/// One take: a canon state's take with `history`, else a fetch; then the publish, as a chain
/// move when `chain_moved`.
fn fetch_core_with(
    rdy: &mut Rdy,
    batches: &[File],
    checked: Option<&[&rdy_store::sync_stream::CheckedBatch]>,
    history: Option<&rdy_store::sync_state::SyncHistory>,
    target: &FetchTarget,
    chain_moved: bool,
) -> Result<FetchOutcome, RdyError> {
    if rdy.is_holder() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "this store is a holder",
            "fetch into a project",
        ));
    }
    // a fetch is a batch of its own: one a raw write left open is sealed and published first
    rdy.seal_open()?;
    let epoch = rdy.store.redaction_epoch();
    let taken = match (checked, history) {
        (Some(c), Some(h)) => {
            rdy.store
                .take_canon_state_checked::<Infallible>(batches, c, target, h, |_, _| Ok(()))
        }
        (None, Some(h)) => rdy
            .store
            .take_canon_state::<Infallible>(batches, target, h, |_, _| Ok(())),
        (Some(c), None) => {
            rdy.store
                .take_fetched_checked::<Infallible>(batches, c, target, |_, _| Ok(()))
        }
        (None, None) => rdy
            .store
            .take_fetched::<Infallible>(batches, target, |_, _| Ok(())),
    };
    match taken {
        Ok(t) => {
            if t.frames > 0 && chain_moved {
                crate::stream_take::renew_after_removal(rdy, epoch);
                rdy.publish_chain_moved()?;
            } else if t.frames > 0 {
                republish(rdy, epoch)?;
            }
            let warnings = rdy.read().uncovered_warnings()?;
            Ok(FetchOutcome {
                records_taken: t.new_objects,
                frames_written: t.frames,
                tags_taken: t.tags_taken,
                tags_kept: t.tags_kept,
                min_rdy_before: t.min_rdy_before,
                min_rdy_after: t.min_rdy_after,
                end: StorePosition {
                    segment: t.end.segment,
                    offset: t.end.offset,
                },
                unchanged: t.frames == 0,
                removals: t.applies,
                redacted_objects: t.redacted_objects,
                warnings,
            })
        }
        Err(e) => Err(fetch_error(e)),
    }
}

/// A fetch's error as the engine's.
fn fetch_error(e: TakeError<Infallible>) -> RdyError {
    match e {
        TakeError::Validation(never) => match never {},
        TakeError::Store(e) => e.into(),
        TakeError::Stream(e) => to_rdy(e),
        TakeError::UncoveredChange(h) => crate::stream_take::uncovered_change(h),
        TakeError::ModeMismatch => RdyError::new(
            E_BAD_INPUT,
            "this store is a holder",
            "fetch into a project",
        ),
        TakeError::RepoMismatch { source, target } => RdyError::new(
            E_REPO_MISMATCH,
            format!("stream repo {source} does not match project repo {target}"),
            "fetch from the remote of the project's repo",
        ),
    }
}

/// The hub's reply to a sync's state step, as the state rule reads it: the hub repo's id and its
/// canon tip, each null when the hub has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubState {
    pub repo_id: Option<RepoId>,
    pub canon: Option<Hash>,
}

/// The canon state of remote `remote` that a sync may use after the hub's reply `hub`: the
/// state when the reply's repo id is the state's and its canon is not null; else None. A
/// changed URL keeps the state. It reads only this remote's ref and changes nothing.
pub fn usable_canon_state(
    reader: &StoreReader,
    remote: &str,
    hub: &HubState,
) -> Result<Option<SyncCanon>, RdyError> {
    let Some(state) = reader.sync_canon(remote).map_err(store)? else {
        return Ok(None);
    };
    if hub.canon.is_none() || hub.repo_id != Some(state.repo_id) {
        return Ok(None);
    }
    Ok(Some(state))
}

/// The state of view `name` of remote `remote` that a sync may use after the hub's reply
/// `hub`: the state when the reply's repo id is the state's; else None. A changed URL keeps
/// the state. It reads only this remote's ref and changes nothing.
pub fn usable_view_state(
    reader: &StoreReader,
    remote: &str,
    name: &str,
    hub: &HubState,
) -> Result<Option<SyncView>, RdyError> {
    let Some(state) = reader.sync_view(remote, name).map_err(store)? else {
        return Ok(None);
    };
    if hub.repo_id != Some(state.repo_id) {
        return Ok(None);
    }
    Ok(Some(state))
}

fn store(e: StoreError) -> RdyError {
    e.into()
}
