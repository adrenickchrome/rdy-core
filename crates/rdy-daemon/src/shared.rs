//! What every connection of the daemon shares: the published state, the writer, the open report,
//! the pack watch and its generation, and the session-end hooks.
use crate::session::SessionId;
use crate::watch::PackWatch;
use crate::writer::{Writer, WriterHandle};
use arc_swap::ArcSwap;
use rdy_engine::engine::{Engine, WriteFn};
use rdy_engine::{BriefRegistry, OpenReport, Rdy, RdyError, ReadState};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

type SessionEndHook = Box<dyn Fn(SessionId) + Send + Sync>;

pub struct Shared {
    /// The published state: every read reads it, the writer stores into it before each reply.
    pub state: ArcSwap<ReadState>,
    /// What opening the store did.
    pub open_report: OpenReport,
    pub root: PathBuf,
    pub writer: WriterHandle,
    /// The pack files as last seen by the writer.
    pub watch: PackWatch,
    /// Bumped by the writer on each reload and on each new or fixed pack error.
    pub pack_gen: AtomicU64,
    /// Why the pack files on disk do not load, while they do not. The writer sets it before it
    /// bumps `pack_gen`, under this lock.
    pub pack_error: Mutex<Option<String>>,
    /// The project's briefing registry, taken from the `Rdy` before the writer owns it.
    pub briefs: Arc<BriefRegistry>,
    session_end: Mutex<Vec<SessionEndHook>>,
    /// Test hook: run once by the writer right after `reload_pack` returns.
    #[cfg(test)]
    pub(crate) after_reload: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Test count: the log lines the writer wrote for a failed clean republish.
    #[cfg(test)]
    pub(crate) republish_failure_logs: std::sync::atomic::AtomicUsize,
}

impl Shared {
    /// Publishes `rdy`'s state and starts the writer that owns it from now on.
    pub fn new(rdy: Rdy) -> Arc<Shared> {
        let open_report = rdy.open_report();
        let root = rdy.root().to_path_buf();
        let state = ArcSwap::new(rdy.read());
        let watch = PackWatch::new(&root, rdy.pack_fingerprint());
        let briefs = rdy.briefs();
        Arc::new_cyclic(|weak| Shared {
            state,
            open_report,
            root,
            writer: Writer::spawn(rdy, weak.clone()),
            watch,
            pack_gen: AtomicU64::new(0),
            pack_error: Mutex::new(None),
            briefs,
            session_end: Mutex::new(Vec::new()),
            #[cfg(test)]
            after_reload: Mutex::new(None),
            #[cfg(test)]
            republish_failure_logs: std::sync::atomic::AtomicUsize::new(0),
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

    /// The pack's generation and its standing error. The generation is read first: the writer
    /// sets the error before it bumps the generation, so a newer generation never pairs with an
    /// older error.
    pub fn pack_status(&self) -> (u64, Option<String>) {
        let generation = self.pack_gen.load(Ordering::SeqCst);
        (generation, self.pack_error_lock().clone())
    }

    pub(crate) fn pack_error_lock(&self) -> MutexGuard<'_, Option<String>> {
        self.pack_error.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hooks(&self) -> MutexGuard<'_, Vec<SessionEndHook>> {
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

    fn briefs(&self) -> Arc<BriefRegistry> {
        Arc::clone(&self.0.briefs)
    }

    fn pack_generation(&self) -> u64 {
        self.0.pack_gen.load(Ordering::SeqCst)
    }

    fn pack_error(&self) -> Option<String> {
        self.0.pack_error_lock().clone()
    }
}
