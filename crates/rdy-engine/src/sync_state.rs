//! The engine side of the store's sync state (D-3RSCW): one remote's `upload` and `pending`
//! refs, read from the published state, and sync state steps written on the writer as a batch
//! of their own. It also holds the per-remote guard that serializes the pushes and pulls of one
//! remote in this process (D-47HVF `SYNC_LOCKED`). One process holds a store (the daemon, or a
//! `--no-daemon` run), so the guard needs no file on disk; it holds no store lock, and the
//! network steps it covers run without the writer.
use crate::engine::{Engine, with_writer_result};
use crate::error::{E_LOCKED, RdyError};
use rdy_store::sync_state::{
    SYNC_LOCKED_FIX, SyncPending, SyncStep, SyncUpload, sync_locked_message,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// One remote's resumable sync refs, as the published state reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteRefs {
    pub upload: Option<SyncUpload>,
    pub pending: Option<SyncPending>,
}

/// The `upload` and `pending` refs of `remote`.
pub fn remote_refs(engine: &dyn Engine, remote: &str) -> Result<RemoteRefs, RdyError> {
    let read = engine.read();
    let reader = read.reader();
    Ok(RemoteRefs {
        upload: reader.sync_upload(remote)?,
        pending: reader.sync_pending(remote)?,
    })
}

/// Writes `steps` for `remote` on the writer, as one sealed batch of their own, and publishes
/// the state again. A failed compare is `E_LOCKED` and writes nothing.
pub fn write_steps(
    engine: &dyn Engine,
    remote: &str,
    steps: Vec<SyncStep>,
) -> Result<(), RdyError> {
    let remote = remote.to_string();
    with_writer_result(engine, move |r| {
        r.seal_open()?;
        let epoch = r.store.redaction_epoch();
        r.store_mut().write_sync(&remote, steps)?;
        crate::stream_take::republish(r, epoch)
    })
}

/// The remotes with a push or pull running in this process, by project.
fn running() -> &'static Mutex<BTreeSet<(PathBuf, String)>> {
    static RUNNING: OnceLock<Mutex<BTreeSet<(PathBuf, String)>>> = OnceLock::new();
    RUNNING.get_or_init(Mutex::default)
}

/// A running push or pull of one remote of one project. It ends when dropped.
#[derive(Debug)]
pub struct RemoteGuard {
    key: (PathBuf, String),
}

impl RemoteGuard {
    /// Marks `remote` of the project at `project` as running. `E_LOCKED`, with D-47HVF's words,
    /// when a push or pull of it already runs.
    pub fn take(project: &Path, remote: &str) -> Result<RemoteGuard, RdyError> {
        let project = project
            .canonicalize()
            .unwrap_or_else(|_| project.to_path_buf());
        let key = (project, remote.to_string());
        let mut set = running().lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(key.clone()) {
            return Err(RdyError::new(
                E_LOCKED,
                sync_locked_message(remote),
                SYNC_LOCKED_FIX,
            ));
        }
        Ok(RemoteGuard { key })
    }
}

impl Drop for RemoteGuard {
    fn drop(&mut self) {
        let mut set = running().lock().unwrap_or_else(|e| e.into_inner());
        set.remove(&self.key);
    }
}
