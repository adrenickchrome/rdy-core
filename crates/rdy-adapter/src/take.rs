//! The take of one upload into a holder (D-D2NPN "verify-on-receive", D-BPYN8, D-CC90E,
//! D-WDKWQ; hub p-97JVE, r-FAG02, i-SG442).
//!
//! The take is core's (`rdy_engine::stream_take::take`): it checks every batch file of the
//! upload as one union over the holder and writes the closure, its approved removals and the tag
//! in one sealed batch, or nothing. This module only hands it the hub's files through the
//! store's one writer: it bounds the input, clones each file handle for the writer and passes
//! the owned outcome back. Every error is the engine's, unchanged.
//!
//! The files stay the hub's, on success and on failure: core reads them and never unlinks them,
//! and its clones are closed when the call returns. A clone may share the caller's cursor, so the
//! store's take seeks each handle before it reads and checks every frame it appends against the
//! one it checked: a caller must not read, seek or change the files during the call, and if it
//! does the take refuses before the seal. Upload ids, file lifetime, retries, queues and trust
//! are the hub's (D-WDKWQ); the `&mut` handle orders this process's writes.
use rdy_engine::engine::with_writer_result;
use rdy_engine::stream_error::to_rdy;
use rdy_engine::{E_IO, Local, RdyError, stream_take};
use rdy_store::take_batch::check_input_bounds;
use std::fs::File;

pub use rdy_engine::stream_take::{StorePosition, TakeOutcome};
pub use rdy_store::take_batch::UploadTarget;

/// Takes the batch files `batches` of one upload for `target` into the holder behind `local`
/// (see the module docs). The bounds are checked before anything is allocated or cloned.
pub(crate) fn take(
    local: &Local,
    batches: &[File],
    target: UploadTarget,
) -> Result<TakeOutcome, RdyError> {
    check_input_bounds(batches.len(), &target).map_err(to_rdy)?;
    // at most the bounded number of files: the writer's closure must own what it reads
    let mut files = Vec::with_capacity(batches.len());
    for f in batches {
        files.push(f.try_clone().map_err(|e| {
            RdyError::new(
                E_IO,
                format!("stream input: {e}"),
                "check that the stream file can be read",
            )
        })?);
    }
    with_writer_result(local, move |rdy| stream_take::take(rdy, &files, target))
}
