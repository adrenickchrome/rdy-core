//! What every connection of the daemon shares: the published state, the writer, the open report
//! and the session-end hooks.
use crate::session::SessionId;
use crate::writer::{Writer, WriterHandle};
use arc_swap::ArcSwap;
use rdy_engine::engine::{Engine, WriteFn};
use rdy_engine::{OpenReport, Rdy, RdyError, ReadState};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type SessionEndHook = Box<dyn Fn(SessionId) + Send + Sync>;

pub struct Shared {
    /// The published state: every read reads it, the writer stores into it before each reply.
    pub state: ArcSwap<ReadState>,
    /// What opening the store did.
    pub open_report: OpenReport,
    pub root: PathBuf,
    pub writer: WriterHandle,
    session_end: Mutex<Vec<SessionEndHook>>,
}

impl Shared {
    /// Publishes `rdy`'s state and starts the writer that owns it from now on.
    pub fn new(rdy: Rdy) -> Arc<Shared> {
        let open_report = rdy.open_report();
        let root = rdy.root().to_path_buf();
        let state = ArcSwap::new(rdy.read());
        Arc::new_cyclic(|weak| Shared {
            state,
            open_report,
            root,
            writer: Writer::spawn(rdy, weak.clone()),
            session_end: Mutex::new(Vec::new()),
        })
    }

    /// The engine over this daemon: reads from the published state, writes on the writer.
    pub fn engine(self: &Arc<Self>) -> Arc<dyn Engine> {
        Arc::new(DaemonEngine(Arc::clone(self)))
    }

    /// Registers `f` to run whenever a session ends.
    pub fn on_session_end(&self, f: impl Fn(SessionId) + Send + Sync + 'static) {
        self.hooks().push(Box::new(f));
    }

    /// Runs every session-end hook for `id`, in registration order. A hook must not register
    /// another hook.
    pub fn end_session(&self, id: SessionId) {
        for f in self.hooks().iter() {
            f(id);
        }
    }

    fn hooks(&self) -> std::sync::MutexGuard<'_, Vec<SessionEndHook>> {
        self.session_end.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stops the writer: the last state is published and the store is closed.
    pub fn shutdown(&self) -> Result<(), RdyError> {
        self.writer.shutdown()
    }
}

/// The daemon's [`Engine`]: reads load the published state, writes run on the writer.
pub struct DaemonEngine(Arc<Shared>);

impl Engine for DaemonEngine {
    fn read(&self) -> Arc<ReadState> {
        self.0.state.load_full()
    }

    fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
        self.0.writer.run(f)
    }

    fn open_report(&self) -> OpenReport {
        self.0.open_report.clone()
    }

    fn root(&self) -> PathBuf {
        self.0.root.clone()
    }
}
