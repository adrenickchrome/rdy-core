//! A project's fetch of a canon or view history from a remote (D-KE83X, D-3RSCW), and the read
//! of the remote's sync state that a sync may use.
//!
//! [`fetch`] takes the batch files of one format-4 history into the project through the same
//! union, closure and proof checks as a holder's take (`rdy_store::take_batch`). It writes one
//! sealed batch: the records the project lacks, `tag/<name>` for each fetched tag whose name is
//! free, and the remote's sync state with the root as its fetched tip or head. It moves no
//! canon or view ref, so the project reads the same canon and views after it. A fetched tag
//! whose name the project uses for another landing is not taken: the local tag stays, and the
//! name is listed. The first sync state write activates the `sync_state` feature in its batch.
//! A retry of a fetch already taken writes nothing.
//!
//! [`usable_canon_state`] and [`usable_view_state`] read one remote's state as a sync uses it
//! after the hub's state step: only when the reply's repo id is the state's, and, for canon,
//! when the reply's canon is not null. A changed URL keeps the state. The read never reads
//! another remote name's refs, and it removes nothing.
use crate::Rdy;
use crate::error::{E_BAD_INPUT, E_REPO_MISMATCH, RdyError};
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
/// names it took and those it kept local, the store's minimum rdy before and after it, and the
/// log's end after it. `unchanged` is true when it wrote nothing (a retry).
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
}

/// Takes the batch files `batches` of one fetched history for `target` into the project `rdy`
/// (see the module docs). Every refusal before the append leaves the project unchanged. A
/// holder is refused with `this store is a holder`.
pub fn fetch(
    rdy: &mut Rdy,
    batches: &[File],
    target: &FetchTarget,
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
    let taken = rdy
        .store
        .take_fetched::<Infallible>(batches, target, |_, _| Ok(()));
    match taken {
        Ok(t) => {
            if t.frames > 0 {
                republish(rdy, epoch)?;
            }
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
