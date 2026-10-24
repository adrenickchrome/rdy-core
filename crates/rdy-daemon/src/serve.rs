//! The host: opens the store, binds the socket, warms canon, answers `ready`; classifies each
//! ctl session with the `session` op and serves MCP per connection over the socket.
use crate::ctl::CtlRequest;
use crate::daemon::{Daemon, DaemonConfig, DaemonHandle, ExitReason, Readiness, log};
use crate::error::{E_BAD_INPUT, E_NO_SESSION, ErrorReply};
use crate::handler::Handler;
use crate::layout::RunDir;
use crate::ops::{OpContext, dispatch};
use crate::session::{SessionId, SessionInfo};
use crate::shared::Shared;
use crate::site::guard::SiteTarget;
use crate::site::profiles::{self, TestEvent, say};
use crate::site::{self, LanOverride, SiteInfo, SiteServer};
use crate::sys;
use crate::writer::panic_text;
use rdy_engine::publication::{ResolvedProfile, profile_at};
use rdy_engine::signing_crypto::SigningAgent;
use rdy_engine::{E_HELD_BY_READER, E_LOCKED, Rdy, RdyError, Scope, Session, SessionKind};
use rdy_engine::{classify, human_actor_for_env};
use rdy_model::Role;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The scopes built before the daemon answers `ready`.
pub const WARM_SCOPES: &[Scope] = &[Scope::Canon];

/// How long `start` retries opening a locked store before it is `AlreadyRunning` (a predecessor
/// releases the store within about 60 ms of removing its socket).
pub const LOCKED_RETRY: Duration = Duration::from_secs(2);
/// The interval of those retries.
const LOCKED_RETRY_EVERY: Duration = Duration::from_millis(25);

pub struct ServeOptions {
    pub project: PathBuf,
    pub version: String,
    /// Run in a session of its own (`setsid`), for a daemon spawned by a client.
    pub detach: bool,
    /// `None` never idles out (a foreground `rdy serve`).
    pub idle_timeout: Option<Duration>,
    /// Test hook: `Some` replaces `human_actor_for_env(<root>)` for session classification.
    #[doc(hidden)]
    pub human_actor: Option<String>,
    /// Test hook: `Some` stands in for the LAN address of the default route while
    /// `serve.site_lan` is on.
    #[doc(hidden)]
    pub site_lan_override: Option<LanOverride>,
}

#[derive(Debug)]
pub enum ServeError {
    /// The store (this directory) is locked by another rdy process.
    AlreadyRunning(PathBuf),
    Open(RdyError),
    Bind(io::Error),
    Runtime(io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::AlreadyRunning(p) => write!(
                f,
                "already running: the store at {} is locked by another rdy process",
                p.display()
            ),
            ServeError::Open(e) => write!(f, "could not open the store: {e}"),
            ServeError::Bind(e) => write!(f, "could not bind the socket: {e}"),
            ServeError::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// The store directory of a project (`rdy_store::layout`'s `.rdy/store`).
fn store_dir(project: &Path) -> PathBuf {
    project.join(".rdy").join("store")
}

/// A started host: the socket is bound and a thread (`rdy-serve`) serves it.
pub struct ServeRun {
    handle: DaemonHandle,
    readiness: Readiness,
    shared: Arc<Shared>,
    thread: JoinHandle<Result<ExitReason, ServeError>>,
    /// A foreground profile-only start's hold on readiness: `ready` is answered only once the
    /// warm-up is done and this sends (see [`serve_with_profile`]). Dropped unsent, the daemon
    /// never answers `ready`.
    gate: Option<mpsc::Sender<()>>,
}

impl ServeRun {
    pub fn handle(&self) -> DaemonHandle {
        self.handle.clone()
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    /// Test hook: what every connection shares.
    #[doc(hidden)]
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Waits for the daemon to exit. The store is closed before this returns.
    pub fn wait(self) -> Result<ExitReason, ServeError> {
        let ServeRun {
            shared,
            thread,
            gate,
            ..
        } = self;
        // a start not released by now never is: the warm-up stops waiting, answers no `ready`,
        // and the runtime's blocking pool can drain
        drop(gate);
        match thread.join() {
            Ok(out) => out,
            Err(_) => {
                // the serving thread did not get to close the store
                if let Err(e) = shared.shutdown() {
                    log(&format!(
                        "the store did not close cleanly ({e}); the next open catches its index up"
                    ));
                }
                Err(ServeError::Runtime(io::Error::other(
                    "the rdy-serve thread panicked",
                )))
            }
        }
    }
}

/// Opens the store, binds the socket and starts serving on a thread of its own. Returns once
/// the socket is bound; clients are answered `starting` until canon is built.
pub fn start(opts: ServeOptions) -> Result<ServeRun, ServeError> {
    start_checked(opts, None)
}

/// [`start`], refusing before either HTTP site binds when canon does not hold `profile` (the
/// foreground profile-only start).
fn start_checked(opts: ServeOptions, profile: Option<&str>) -> Result<ServeRun, ServeError> {
    // (1)
    if opts.detach {
        crate::spawn::detach().map_err(ServeError::Runtime)?;
    }
    let root = RunDir::of_project(&opts.project).root();
    // (2)
    log("opening store");
    let rdy = match open_store(&opts.project) {
        Ok(r) => r,
        Err(e) if e.code == E_LOCKED || e.code == E_HELD_BY_READER => {
            let err = ServeError::AlreadyRunning(store_dir(&opts.project));
            say(&root, &err.to_string());
            return Err(err);
        }
        Err(e) => {
            let err = ServeError::Open(e);
            say(&root, &err.to_string());
            return Err(err);
        }
    };
    // (3)
    let shared = Shared::new(rdy);
    let r = &shared.open_report;
    log(&format!(
        "store open: caught up {} frames, {} quarantined, index rebuilt: {}, unclean shutdown: {}",
        r.caught_up_frames,
        r.quarantined.len(),
        r.index_rebuilt,
        r.unclean_shutdown
    ));
    // (3b) the initial profile of a foreground profile-only start, in canon, before any port
    if let Some(name) = profile
        && let Err(e) = profile_at(&shared.state.load_full(), &Scope::Canon, name)
    {
        close(&shared);
        let err = ServeError::Open(e);
        say(&root, &err.to_string());
        return Err(err);
    }
    // (4)
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            close(&shared);
            return Err(ServeError::Runtime(e));
        }
    };
    let config = DaemonConfig {
        run: RunDir::of_project(&opts.project),
        version: opts.version.clone(),
        idle_timeout: opts.idle_timeout,
        expected_uid: None,
    };
    let handler = ServeHandler::new(Arc::clone(&shared), opts.human_actor.clone());
    let mut daemon = Daemon::new(config, Arc::new(handler));
    if let Err(e) = rt.block_on(daemon.bind()) {
        close(&shared);
        return Err(ServeError::Bind(e));
    }
    let handle = daemon.handle();
    let readiness = daemon.readiness();
    // (4b) the site, once the daemon owns the project: the store lock is held and the socket is
    // bound and recorded (D-WZ4ZP, D-R7N9J). No loopback port leaves the daemon without a site.
    // The profile-only listeners start later, on request, on this runtime.
    let (site, info) = rt.block_on(SiteServer::bind(&root, opts.site_lan_override));
    for line in site::log_lines(&info) {
        log(&line);
    }
    let _ = shared.site_info.set(info);
    shared.profile_sites.init(
        rt.handle().clone(),
        handle.clone(),
        root.clone(),
        opts.site_lan_override,
    );
    // (5) a foreground profile-only start answers `ready` only once its caller released it
    let (gate, released) = match profile {
        Some(_) => {
            let (tx, rx) = mpsc::channel();
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    let thread = {
        let shared = Arc::clone(&shared);
        let readiness = readiness.clone();
        let handle = handle.clone();
        std::thread::Builder::new()
            .name("rdy-serve".into())
            .spawn(move || {
                serve_thread(rt, daemon, shared, readiness, site, handle, root, released)
            })
    };
    let thread = match thread {
        Ok(t) => t,
        Err(e) => {
            // the runtime and the daemon went down with the closure (and every listener with
            // the runtime): take the run files too
            handle.stop();
            drop(shared.profile_sites.close());
            close(&shared);
            return Err(ServeError::Runtime(e));
        }
    };
    // (6)
    Ok(ServeRun {
        handle,
        readiness,
        shared,
        thread,
        gate,
    })
}

/// `Rdy::open`, retried while the store is `E_LOCKED` for up to [`LOCKED_RETRY`]: a predecessor
/// daemon removes its socket before it drains and closes its store (and its process keeps the
/// index open until it exits), so a successor spawned the moment the socket is gone, by a
/// version replace or after `rdy serve --stop`, finds the store still held for a few tens of ms.
/// A holder that keeps it (a `--no-daemon` command, a live daemon) is still `E_LOCKED` after the
/// bound.
fn open_store(project: &Path) -> Result<Rdy, RdyError> {
    let deadline = Instant::now() + LOCKED_RETRY;
    loop {
        match Rdy::open(project) {
            Err(e)
                if (e.code == E_LOCKED || e.code == E_HELD_BY_READER)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(LOCKED_RETRY_EVERY);
            }
            out => return out,
        }
    }
}

/// `start(opts)?.wait()`, with SIGINT (Ctrl-C) and SIGTERM taken as `rdy serve --stop`: the
/// first one stops the daemon (it drains, the store closes cleanly and this returns `Stopped`);
/// a second one while it drains exits the process at once, which loses nothing (the log is
/// durable) and leaves the next open to catch up. The handlers are installed before the store
/// opens, so no signal finds the store open without them.
pub fn serve(opts: ServeOptions) -> Result<ExitReason, ServeError> {
    let signals = StopSignals::install();
    let run = start(opts)?;
    if let Some(s) = signals {
        s.stop_on_signal(run.handle());
    }
    run.wait()
}

/// What a foreground profile-only start has once its listener is open: the folded key, the
/// listener's info (its `warning` the original LAN diagnostic), and the profile and render
/// warnings of the build it captured.
#[derive(Clone, Debug)]
pub struct ProfileSiteReady {
    pub key: String,
    pub info: SiteInfo,
    pub profile: ResolvedProfile,
    pub warnings: Vec<String>,
}

/// [`serve`] with the profile-only listener of `key` (D-DK03K): the same signals and start;
/// canon must hold `key` before either HTTP site binds. Once the daemon owns the store and its
/// socket, the listener of `key` opens and its pages build; `on_ready` then runs once, on this
/// thread, and the call serves in the foreground until the daemon stops. A failed profile, build
/// or `on_ready` stops and joins the daemon this call created, releasing its sockets, logs the
/// error as a failed start does, and is returned as `ServeError::Open`.
pub fn serve_with_profile(
    opts: ServeOptions,
    key: &str,
    on_ready: impl FnOnce(ProfileSiteReady) -> Result<(), RdyError>,
) -> Result<ExitReason, ServeError> {
    let signals = StopSignals::install();
    let root = RunDir::of_project(&opts.project).root();
    let run = start_checked(opts, Some(key))?;
    if let Some(s) = signals {
        s.stop_on_signal(run.handle());
    }
    let ready = profile_ready(run.shared(), key).and_then(on_ready);
    if let Err(e) = ready {
        let err = ServeError::Open(e);
        say(&root, &err.to_string());
        run.handle().stop();
        // the gate goes unsent: no client hears `ready` from this daemon
        if let Err(w) = run.wait() {
            log(&format!("the daemon did not stop cleanly: {w}"));
        }
        return Err(err);
    }
    // the start succeeded: `ready` goes out once the warm-up is done too
    if let Some(gate) = &run.gate {
        let _ = gate.send(());
    }
    run.wait()
}

/// Opens (or reuses) the listener of `key` and builds its pages once, for the facts of that
/// build.
fn profile_ready(sh: &Arc<Shared>, key: &str) -> Result<ProfileSiteReady, RdyError> {
    let (key, info) = sh.profile_sites.ensure(sh, key)?;
    let target = SiteTarget::ProfileOnly { key: key.clone() };
    let build = sh.site.refresh(sh, &target)?;
    let profile = build.profile.clone().ok_or_else(|| {
        RdyError::new(
            rdy_engine::E_SITE,
            format!("the profile-only site of \"{key}\" built no profile"),
            "run rdy serve --stop, then start it again",
        )
    })?;
    Ok(ProfileSiteReady {
        key,
        info,
        profile,
        warnings: build.live.warnings.clone(),
    })
}

/// SIGINT and SIGTERM, registered on a runtime of their own (the daemon's runtime moves to the
/// `rdy-serve` thread).
struct StopSignals {
    rt: tokio::runtime::Runtime,
    signals: sys::StopSignals,
}

impl StopSignals {
    /// `None` (logged) if the handlers cannot be installed: the daemon then runs without them,
    /// and `rdy serve --stop` still stops it cleanly.
    fn install() -> Option<StopSignals> {
        let installed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .and_then(|rt| {
                let signals = {
                    let _g = rt.enter();
                    sys::StopSignals::install()?
                };
                Ok(StopSignals { rt, signals })
            });
        match installed {
            Ok(s) => Some(s),
            Err(e) => {
                log(&format!(
                    "could not install the SIGINT/SIGTERM handlers: {e}"
                ));
                None
            }
        }
    }

    /// Waits on a thread of its own (`rdy-signals`, alive until the process exits).
    fn stop_on_signal(self, handle: DaemonHandle) {
        let StopSignals { rt, mut signals } = self;
        let spawned = std::thread::Builder::new()
            .name("rdy-signals".into())
            .spawn(move || {
                rt.block_on(async move {
                    let name = signals.recv().await;
                    log(&format!("{name}: stopping"));
                    handle.stop();
                    let name = signals.recv().await;
                    log(&format!("{name} again: exiting without draining"));
                    std::process::exit(130);
                })
            });
        if let Err(e) = spawned {
            log(&format!("could not start the signal thread: {e}"));
        }
    }
}

/// Closes the store, logging a failed close.
fn close(shared: &Shared) {
    if let Err(e) = shared.shutdown() {
        log(&format!(
            "the store did not close cleanly ({e}); the next open catches its index up"
        ));
    }
}

/// Runs the warm-up, then answers ready once `released` (called after the warm-up) is true. A
/// panic in the warm-up is logged and the daemon serves with a cold cache: a released start
/// always reaches readiness, so no client waits out its start timeout. A foreground
/// profile-only start waits in `released` for its caller's callback; false leaves the daemon
/// `starting` (it is stopping) and logs no `ready`. Returns whether ready was answered.
pub(crate) fn warm_then_ready(
    readiness: &Readiness,
    warm: impl FnOnce(),
    released: impl FnOnce() -> bool,
) -> bool {
    if let Err(p) = catch_unwind(AssertUnwindSafe(warm)) {
        log(&format!(
            "warm-up panicked ({}); serving with a cold cache",
            panic_text(&*p)
        ));
    }
    if !released() {
        return false;
    }
    readiness.ready();
    log("ready");
    true
}

/// The body of the `rdy-serve` thread: warms canon beside the accept loop and serves the site,
/// then, once the daemon exits, ends the site and closes the store.
#[allow(clippy::too_many_arguments)]
fn serve_thread(
    rt: tokio::runtime::Runtime,
    daemon: Daemon,
    shared: Arc<Shared>,
    readiness: Readiness,
    site: Option<SiteServer>,
    handle: DaemonHandle,
    root: PathBuf,
    released: Option<mpsc::Receiver<()>>,
) -> Result<ExitReason, ServeError> {
    // the site accepts until the daemon stops; the runtime's drop below ends what is left of
    // it before the store closes, so a successor finds the port free
    if let Some(site) = site {
        rt.spawn(site.run(Arc::clone(&shared), handle));
    }
    readiness.progress("building canon");
    log("building canon");
    let warm = Arc::clone(&shared);
    let warmed = root.clone();
    rt.spawn_blocking(move || {
        let ready = warm_then_ready(
            &readiness,
            || {
                let st = warm.engine().read();
                for scope in WARM_SCOPES {
                    if let Err(e) = st
                        .materialized(scope)
                        .and_then(|_| st.graph(scope).map(drop))
                    {
                        log(&format!("warm-up failed: {e}"));
                    }
                }
            },
            || {
                profiles::report(&warmed, TestEvent::Stage("warmed"));
                // a foreground profile-only start: its callback's success releases it, and
                // a failure drops the gate unsent
                released.is_none_or(|rx| rx.recv().is_ok())
            },
        );
        if !ready {
            return;
        }
        profiles::report(&warmed, TestEvent::Stage("ready"));
        // plan M2e2 (D-Z0EMP): what moved while no daemon ran (the pack, the rdy build) is
        // logged once, after ready, in a batch of cause `reopened`
        warm.writer.reconcile_soon();
    });
    let ran = rt.block_on(daemon.run());
    // the profile-only listeners: no start from now on, and each listener's task, ended by the
    // stop, is joined on this runtime before the store closes or is abandoned
    let listeners = shared.profile_sites.close();
    rt.block_on(async move {
        for task in listeners {
            let _ = task.await;
        }
    });
    profiles::report(&root, TestEvent::Stage("profiles-joined"));
    // waits for the blocking pool (warm-up, in-flight handlers) to finish while the writer still
    // serves them; then the store closes last, so the process exits right after releasing it
    drop(rt);
    if matches!(ran, Ok(ExitReason::ProjectGone)) {
        // final ruling: no store write; the store's path may hold another project now
        shared.writer.abandon();
        log("store left as it is (the project is gone)");
        profiles::report(&root, TestEvent::Stage("store-abandoned"));
    } else {
        close(&shared);
        log("store closed");
        profiles::report(&root, TestEvent::Stage("store-closed"));
    }
    ran.map_err(ServeError::Runtime)
}

/// One ctl connection's session: what the hello said and who the daemon classified it as.
#[derive(Clone, Debug)]
pub struct CtlSession {
    pub info: SessionInfo,
    pub session: Session,
    /// The human actor the session was classified against (a relayed approval names it).
    pub human: String,
}

/// The host's [`Handler`]: the `session` op, the ctl ops over the shared engine, and MCP.
pub struct ServeHandler {
    shared: Arc<Shared>,
    /// Replaces `human_actor_for_env(<root>)` when set (a test hook).
    human: Option<String>,
    ctl: Mutex<HashMap<SessionId, CtlSession>>,
    /// The pack generation each session has heard of (for its `pack_changed` notice).
    seen: Arc<Mutex<HashMap<SessionId, u64>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionArgs {
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    run: Option<String>,
}

impl ServeHandler {
    pub fn new(shared: Arc<Shared>, human_actor: Option<String>) -> ServeHandler {
        ServeHandler {
            shared,
            human: human_actor,
            ctl: Mutex::new(HashMap::new()),
            seen: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, CtlSession>> {
        self.ctl.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open_session(&self, info: &SessionInfo, args: Value) -> Result<Value, ErrorReply> {
        let args = if args.is_null() { json!({}) } else { args };
        let a: SessionArgs = serde_json::from_value(args).map_err(|e| {
            let msg = e.to_string();
            match rdy_engine::did_you_mean::unknown_field_error("session", &msg) {
                Some(err) => ErrorReply::from(err),
                None => ErrorReply::new(
                    E_BAD_INPUT,
                    format!("session: {msg}"),
                    "args for session: {actor?, run?}",
                ),
            }
        })?;
        // D-ME2C0: the configured (or test-hook) human and the client's actor and run are checked
        // before a session is recorded; a refusal leaves the session table as it was
        let human = match self.human.clone() {
            Some(h) => h,
            None => human_actor_for_env(&self.shared.engine().root())?,
        };
        let s = classify(&human, a.actor.as_deref(), a.run.as_deref(), Role::Writer)?;
        let reply = json!({
            "actor": s.actor,
            "run": s.run,
            "kind": match s.kind {
                SessionKind::Human => "human",
                SessionKind::Agent => "agent",
            },
            "default_view": s.default_view(),
        });
        self.sessions().insert(
            info.id,
            CtlSession {
                info: info.clone(),
                session: s,
                human,
            },
        );
        Ok(reply)
    }
}

impl Handler for ServeHandler {
    fn ctl(
        &self,
        session: &SessionInfo,
        request: CtlRequest,
        agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        if request.op == "session" {
            return self.open_session(session, request.args);
        }
        let Some(cs) = self.sessions().get(&session.id).cloned() else {
            return Err(ErrorReply::new(
                E_NO_SESSION,
                "no session on this connection",
                r#"send {"id":1,"op":"session","args":{"actor":null,"run":null}} first"#,
            ));
        };
        let engine = self.shared.engine();
        let cx = OpContext {
            engine: &*engine,
            session: &cs.session,
            human: &cs.human,
            cwd: &cs.info.cwd,
            agent: &*agent,
        };
        dispatch(&cx, &request.op, request.args).map_err(ErrorReply::from)
    }

    fn mcp(
        &self,
        session: SessionInfo,
        stream: crate::handler::McpStream,
        agent: Arc<dyn SigningAgent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let engine = self.shared.engine();
        let hub = Arc::clone(&self.shared.hub);
        Box::pin(async move {
            let (r, w) = tokio::io::split(stream);
            if let Err(e) = rdy_mcp::server::serve_on(engine, session.id.0, hub, agent, r, w).await
            {
                log(&format!(
                    "mcp session {} ended with an error: {e}",
                    session.id
                ));
            }
        })
    }

    /// Reloads the config and packs on the writer if the watch saw them change, so the
    /// session's first reply already reads the reloaded pack (the check runs on the blocking
    /// pool, and the writer is asked only on a change, so a new connection does not queue behind
    /// a long write otherwise). A failed reload is the writer's to log; it never fails the
    /// connection. The session then counts as having heard of the current generation.
    fn session_started(&self, session: &SessionInfo) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let shared = Arc::clone(&self.shared);
        let seen = Arc::clone(&self.seen);
        let id = session.id;
        Box::pin(async move {
            let sh = Arc::clone(&shared);
            let _ = tokio::task::spawn_blocking(move || {
                if sh.watch.changed() {
                    let _ = sh.writer.reload();
                }
            })
            .await;
            // after the reload: a session that connected across it has nothing to hear
            let generation = shared.pack_status().0;
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, generation);
        })
    }

    fn notice(&self, session: &SessionInfo) -> Option<String> {
        let (generation, error) = self.shared.pack_status();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let seen = seen.entry(session.id).or_insert(generation);
        rdy_engine::engine::notice(seen, generation, error.as_deref())
    }

    fn session_ended(&self, session: &SessionInfo) {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session.id);
        self.sessions().remove(&session.id);
        self.shared.end_session(session.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn msw9_a_warm_up_that_panics_still_answers_ready() {
        let log = crate::daemon::capture_log_for_tests(&["warm-up test panic"]);
        let r = Readiness::new();
        assert!(warm_then_ready(
            &r,
            || panic!("warm-up test panic"),
            || true
        ));
        assert!(r.is_ready());
        assert_eq!(
            log.lines(),
            ["warm-up panicked (warm-up test panic); serving with a cold cache"]
        );
    }
}
