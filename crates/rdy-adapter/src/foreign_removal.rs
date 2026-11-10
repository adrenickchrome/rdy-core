//! The citing side of a cited project's removal through the adapter (repo links; D-CN4QJ,
//! D-DAPR8; hub tb-G28CM rows 69 and 80): the take of a cited project's removal proofs into this
//! project store, and the count of the foreign bytes it holds for one cited repo.
//!
//! The take and its checks are core's (`rdy_engine::foreign_removal`): core checks the whole
//! stream, then hands each removal proof it carries to the store, which checks the proof against
//! the cited repo and seals one batch per proof that erases a held foreign copy. This module only
//! hands it the hub's file through the store's one writer: it checks the input's bound, clones
//! the file handle for the writer and passes the owned outcome back. Every error is core's,
//! unchanged.
//!
//! The file stays the hub's, on success and on failure: core reads it and never unlinks it, and
//! its clone is closed when the call returns. A clone may share the caller's cursor, so the take
//! seeks before each read; a caller must not read, seek or change the file during the call.
//! Which projects cite a repo, fan-out, scheduling and retries are the hub's (D-WDKWQ). No CLI or
//! MCP tool takes a foreign removal.
use crate::store::{Snapshot, StoreHandle};
use rdy_engine::engine::with_writer_result;
use rdy_engine::foreign_removal::{self, check_removal_bounds};
use rdy_engine::{E_IO, RdyError};
use rdy_model::RepoId;
use std::fs::File;

pub use rdy_engine::foreign_removal::{ForeignRemovalTaken, RemovalTaken};

impl StoreHandle {
    /// Takes the removal proofs in `stream`, a complete `tag_history` batch of the cited repo
    /// `repo`, into this project's foreign copy of `repo` (see the module docs). Each proof
    /// erases the versions it approves that a delivery of `repo` holds here, on a live or a
    /// revoked link, and never local content of the same version; the result lists the applied
    /// and the missing versions of each proof. A proof taken before writes nothing again. A
    /// holder is refused (`this store is a holder`), as are this project's own repo (`a project
    /// cannot cite itself`) and a stream of another repo (`E_REPO_MISMATCH`). The bound is
    /// checked before the handle is cloned. The call takes `&self`: writes through a shared
    /// handle are ordered by the store's one writer, and a snapshot captured before a removal
    /// refuses content reads after it (`E_REDACTED_STATE`).
    pub fn take_foreign_removal(
        &self,
        stream: &File,
        repo: RepoId,
    ) -> Result<ForeignRemovalTaken, RdyError> {
        let len = stream.metadata().map_err(io)?.len();
        check_removal_bounds(len)?;
        // the writer's closure must own what it reads
        let file = stream.try_clone().map_err(io)?;
        with_writer_result(self.local(), move |rdy| {
            foreign_removal::take_foreign_removal(rdy, &file, &repo)
        })
    }
}

impl Snapshot {
    /// The bytes this snapshot's store retains for the cited repo `repo`: every FOREIGN entry of
    /// it (deliveries, revocations and removals, on live and revoked links) and every BLOCK frame
    /// a delivery of it holds, an erased one included. Another repo's bytes are its own. A
    /// snapshot captured before a later removal is refused (`E_REDACTED_STATE`).
    pub fn foreign_bytes(&self, repo: &RepoId) -> Result<u64, RdyError> {
        foreign_removal::foreign_bytes(self.read_state(), repo)
    }
}

/// E_IO: the caller's stream file cannot be read or cloned.
fn io(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("stream input: {e}"),
        "check that the stream file can be read",
    )
}
