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

    fn lock(&self) -> MutexGuard<'_, Rdy> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Closes the project's store cleanly.
    pub fn close(self) -> Result<(), RdyError> {
        self.0
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .close()
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
