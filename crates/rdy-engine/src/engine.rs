//! The one door to a project: reads from the published state, writes through a closure that
//! runs against `&mut Rdy` on a single writer, in arrival order.
use crate::{OpenReport, Rdy, RdyError, ReadState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// A write: runs against the project on the writer and returns its result as JSON.
pub type WriteFn = Box<dyn FnOnce(&mut Rdy) -> Result<serde_json::Value, RdyError> + Send>;

pub trait Engine: Send + Sync {
    /// The current published state. After `write` returns, it includes that write.
    fn read(&self) -> Arc<ReadState>;
    /// Runs `f` serialized with every other write and returns after the state is published.
    fn write(&self, f: WriteFn) -> Result<serde_json::Value, RdyError>;
    /// What opening the store did.
    fn open_report(&self) -> OpenReport;
    fn root(&self) -> PathBuf;
    /// How many times the loaded pack changed (a reload, or a new or fixed pack error); `0` for
    /// an engine that never reloads.
    fn pack_generation(&self) -> u64 {
        0
    }
    /// Why the pack files on disk do not load, while they do not; the previous pack stays loaded.
    fn pack_error(&self) -> Option<String> {
        None
    }
}

/// The notice for a session that last saw pack generation `*seen`: `pack_changed` once per new
/// generation (unless the pack is now in error), then `pack_error` on every call while `error`
/// stands. Updates `*seen`.
pub fn notice(seen: &mut u64, generation: u64, error: Option<&str>) -> Option<String> {
    let mut parts = vec![];
    if *seen != generation {
        *seen = generation;
        if error.is_none() {
            parts.push(format!(
                "pack_changed: the pack was reloaded (generation {generation})"
            ));
        }
    }
    if let Some(e) = error {
        parts.push(format!(
            "pack_error: {e}; the previous pack stays loaded until the file is fixed"
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// `f` as a `WriteFn` whose `T` is serialized to JSON.
pub fn write_typed<T: serde::Serialize + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    e.write(Box::new(move |r| {
        let v = f(r)?;
        serde_json::to_value(v).map_err(|e| {
            RdyError::new(
                crate::E_INTERNAL,
                format!("a write result did not serialize: {e}"),
                "report this as a bug in rdy",
            )
        })
    }))
}

/// The in-process engine: a mutex around the project. Used by `--no-daemon`, `rdy mcp
/// --no-daemon` and tests.
pub struct Local(Mutex<Rdy>);

impl Local {
    pub fn new(rdy: Rdy) -> Local {
        Local(Mutex::new(rdy))
    }

    pub fn open(project: &Path) -> Result<Local, RdyError> {
        Ok(Local::new(Rdy::open(project)?))
    }

    /// A write that panicked may have left the caches half-updated: a poisoned lock is
    /// recovered with an `invalidate` (the state is republished from the store) and the poison
    /// is cleared, so the next lock pays nothing.
    fn lock(&self) -> MutexGuard<'_, Rdy> {
        match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                let mut g = poisoned.into_inner();
                g.invalidate();
                self.0.clear_poison();
                g
            }
        }
    }

    /// Closes the project's store cleanly.
    pub fn close(self) -> Result<(), RdyError> {
        let r = match self.0.into_inner() {
            Ok(r) => r,
            Err(poisoned) => {
                let mut r = poisoned.into_inner();
                r.invalidate();
                r
            }
        };
        r.close()
    }

    /// Checkpoints if writes are waiting and the store has been idle long enough.
    pub fn checkpoint_if_idle(&self) -> Result<bool, RdyError> {
        self.lock().checkpoint_if_idle()
    }
}

impl Engine for Local {
    fn read(&self) -> Arc<ReadState> {
        self.lock().read()
    }

    fn write(&self, f: WriteFn) -> Result<serde_json::Value, RdyError> {
        f(&mut self.lock())
    }

    fn open_report(&self) -> OpenReport {
        self.lock().open_report()
    }

    fn root(&self) -> PathBuf {
        self.lock().root().to_path_buf()
    }
}

/// The one JSON shape of an open report, for the MCP overview and the ctl `status` and `fsck`.
pub fn open_report_json(r: &OpenReport) -> serde_json::Value {
    serde_json::json!({
        "quarantined": r.quarantined.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "unclean_shutdown": r.unclean_shutdown,
        "redb_repaired": r.redb_repaired,
        "index_rebuilt": r.index_rebuilt,
        "caught_up_frames": r.caught_up_frames,
        "catch_up_commits": r.catch_up_commits,
    })
}
