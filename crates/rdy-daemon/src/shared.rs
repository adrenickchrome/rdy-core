//! What every connection of the daemon shares: the published state, the writer, the open report,
//! the pack watch and its generation, the hub of connected MCP sessions and its notifier, and
//! the session-end hooks.
use crate::notify::{NotifyMsg, spawn_notifier};
use crate::session::SessionId;
use crate::site::SiteInfo;
use crate::site::live::{LiveSite, spawn_site_worker};
use crate::watch::PackWatch;
use crate::writer::{Writer, WriterHandle};
use arc_swap::ArcSwap;
use rdy_engine::engine::{Engine, SiteLink, WriteFn};
use rdy_engine::semantic::lease::LeaseRegistry;
use rdy_engine::{BriefRegistry, E_SITE, OpenReport, Rdy, RdyError, ReadState};
use rdy_mcp::notify::NotifyHub;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

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
    /// Bumped by the writer on each reload and on each new or fixed pack error, after it stores
    /// the state that holds the change: read it before `state`, and a state is never older than
    /// its generation.
    pub pack_gen: AtomicU64,
    /// Why the pack files on disk do not load, while they do not. The writer sets it before it
    /// bumps `pack_gen`.
    pub pack_error: Mutex<Option<String>>,
    /// The project's briefing registry, taken from the `Rdy` before the writer owns it.
    pub briefs: Arc<BriefRegistry>,
    /// The project's lease registry for review requests, taken from the `Rdy` the same way.
    pub leases: Arc<LeaseRegistry>,
    /// The MCP sessions connected to this daemon, who they are and what they subscribed to
    /// (ib10). A session leaves it when it ends (a session-end hook).
    pub hub: Arc<NotifyHub>,
    /// The notifier thread's queue (ib11): the writer forwards each batch's events here, and a
    /// pack reload, after it publishes. Unbounded, so a forward never blocks the writer.
    pub(crate) notify: Sender<NotifyMsg>,
    /// The live site's state: the last build of each served scope and its open pages. The
    /// writer tells it of each publish; its `rdy-site` worker builds off the writer.
    pub site: Arc<LiveSite>,
    /// Where the site listens: set once by the daemon's start, after the bind and before
    /// `ready` (ls6). Unset for a `Shared` that no daemon serves.
    pub site_info: OnceLock<SiteInfo>,
    session_end: Mutex<Vec<SessionEndHook>>,
    /// Test hook: run once by the writer right after `reload_pack` returns.
    #[cfg(test)]
    pub(crate) after_reload: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Test hook: run once by the writer in a pack reload, right before it stores the state.
    #[cfg(test)]
    pub(crate) before_reload_store: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Test count: the log lines the writer wrote for a failed clean republish.
    #[cfg(test)]
    pub(crate) republish_failure_logs: std::sync::atomic::AtomicUsize,
    /// Test record: each (session, URI) the notifier told about a batch's events, in order.
    #[cfg(test)]
    pub(crate) delivered: Mutex<Vec<(u64, String)>>,
}

impl Shared {
    /// Publishes `rdy`'s state and starts the writer that owns it from now on, the notifier
    /// that tells subscribed MCP sessions what the writer's batches changed (ib11), and the site
    /// worker that builds the scopes with an open page after each publish (ls5). The writer
    /// receives every event `rdy` emits from now on (`Rdy::subscribe`, before the writer owns it).
    pub fn new(rdy: Rdy) -> Arc<Shared> {
        let open_report = rdy.open_report();
        let root = rdy.root().to_path_buf();
        let state = ArcSwap::new(rdy.read());
        let watch = PackWatch::new(&root, rdy.pack_fingerprint());
        let briefs = rdy.briefs();
        let leases = rdy.leases();
        let events = rdy.subscribe();
        let owned = Arc::clone(&leases);
        let hub = NotifyHub::new();
        let leaving = Arc::clone(&hub);
        let (notify, queue) = channel();
        let site = LiveSite::new();
        let sh = Arc::new_cyclic(|weak| {
            // the notifier ends when its queue's sender, held here, is dropped with the last
            // `Shared`
            drop(spawn_notifier(weak.clone(), queue));
            Shared {
                state,
                open_report,
                root,
                writer: Writer::spawn(rdy, events, weak.clone()),
                watch,
                pack_gen: AtomicU64::new(0),
                pack_error: Mutex::new(None),
                briefs,
                leases,
                hub,
                notify,
                site,
                site_info: OnceLock::new(),
                session_end: Mutex::new(vec![
                    Box::new(move |id: SessionId| {
                        owned.release_owner(id.0);
                    }),
                    Box::new(move |id: SessionId| leaving.disconnect(id.0)),
                ]),
                #[cfg(test)]
                after_reload: Mutex::new(None),
                #[cfg(test)]
                before_reload_store: Mutex::new(None),
                #[cfg(test)]
                republish_failure_logs: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                delivered: Mutex::new(Vec::new()),
            }
        });
        // the site worker ends on `LiveSite::close` (the shutdown) or with the last `Shared`.
        // It starts once `sh` lives: inside `new_cyclic` it could see no `Shared` yet and end.
        drop(spawn_site_worker(Arc::downgrade(&sh), Arc::clone(&sh.site)));
        sh
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

    /// Stops the site worker and ends every open page's stream, then stops the writer: the last
    /// state is published and the store is closed.
    pub fn shutdown(&self) -> Result<(), RdyError> {
        self.site.close();
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

    fn leases(&self) -> Arc<LeaseRegistry> {
        Arc::clone(&self.0.leases)
    }

    fn pack_generation(&self) -> u64 {
        self.0.pack_gen.load(Ordering::SeqCst)
    }

    fn pack_error(&self) -> Option<String> {
        self.0.pack_error_lock().clone()
    }

    fn site(&self) -> Result<SiteLink, RdyError> {
        let off = |reason: &str| {
            RdyError::new(
                E_SITE,
                format!("the live site is off: {reason}"),
                "set serve.site_port to a free port from 1024 to 65535 in .rdy/local.yaml, then run rdy serve --stop",
            )
        };
        let info = self
            .0
            .site_info
            .get()
            .ok_or_else(|| off("the daemon has not started it"))?;
        match &info.url {
            Ok(url) => Ok(SiteLink {
                url: url.clone(),
                lan_url: info.lan_url.clone(),
                warning: info.warning.clone(),
            }),
            Err(reason) => Err(off(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_mcp::notify::Sink;

    #[test]
    fn ib11_a_session_end_removes_it_from_the_hub() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        sh.hub.connect(5, Sink::None);
        sh.hub.identify(5, "agent:a", Some("a"));
        sh.hub.subscribe(5, "rdy://inbox");
        sh.hub.connect(6, Sink::None);
        assert_eq!(sh.hub.targets().len(), 2);
        sh.end_session(SessionId(5));
        let sids: Vec<u64> = sh.hub.targets().iter().map(|t| t.sid).collect();
        assert_eq!(
            sids,
            vec![6],
            "the ended session left the hub, the other stays"
        );
        sh.shutdown().unwrap();
    }
}
