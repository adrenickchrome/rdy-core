//! The publication take through the adapter (D-91DZW, D-D2NPN, D-BPYN8, D-WDKWQ; hub D-40ZGT,
//! tb-G28CM).
//!
//! The take, the profile facts and their checks are core's
//! (`rdy_engine::publication_take::take_publication`): core checks the upload over the holder,
//! resolves the requested profile in the root tag's snapshot, calls the host's verdict once with
//! those facts and only then appends and seals, all under the store's one writer. This module only
//! hands it the hub's files: it bounds the input, clones each file handle for the writer and
//! passes the owned result back. Every core error is core's, unchanged; the verdict's own error
//! comes back as [`PublicationRefusal::Verdict`], unchanged.
//!
//! The verdict runs on the writer's thread, inside the take. It must read only what the host
//! loaded before the call and must not call this adapter on the same store: that would wait for
//! its own writer. A verdict that panics is `E_INTERNAL` and appends nothing. The hub owns its
//! policy checks, their order, site, profile and audience matching, public site settings and its
//! publication records; this callback is no person-facing policy command (the chat path is
//! `rdy_publish`, with CLI parity). The files stay the hub's, as for [`crate::take`].
use rdy_engine::engine::with_writer_result;
use rdy_engine::{E_IO, Local, RdyError, publication_take};
use std::fs::File;

pub use rdy_engine::publication_take::{
    PublicationFacts, PublicationRefusal, PublicationTaken, PublicationTarget,
};

/// Takes the batch files `batches` of one upload for `target` into the holder behind `local`,
/// with the host's `verdict` between core's checks and the append (see the module docs). The
/// bounds are checked before anything is allocated or cloned.
pub(crate) fn take_publication<E: Send + 'static>(
    local: &Local,
    batches: &[File],
    target: PublicationTarget,
    verdict: impl FnOnce(&PublicationFacts) -> Result<(), E> + Send + 'static,
) -> Result<PublicationTaken, PublicationRefusal<E>> {
    publication_take::check_bounds(batches.len(), &target)?;
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
    // the take's own result travels as the write's owned value; a write or seal that fails
    // around it is core's
    with_writer_result(local, move |rdy| {
        Ok(publication_take::take_publication(
            rdy, &files, target, verdict,
        ))
    })
    .unwrap_or_else(|e| Err(PublicationRefusal::Core(e)))
}
