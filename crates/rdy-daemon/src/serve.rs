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
use crate::writer::panic_text;
use rdy_engine::{E_LOCKED, Rdy, RdyError, Scope, Session, SessionKind};
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
use std::sync::{Arc, Mutex};
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
        let ServeRun { shared, thread, .. } = self;
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
    // (1)
    if opts.detach {
        crate::spawn::detach().map_err(ServeError::Runtime)?;
    }
    // (2)
    log("opening store");
    let rdy = match open_store(&opts.project) {
        Ok(r) => r,
        Err(e) if e.code == E_LOCKED => {
            let err = ServeError::AlreadyRunning(store_dir(&opts.project));
            log(&err.to_string());
            return Err(err);
        }
        Err(e) => {
            let err = ServeError::Open(e);
            log(&err.to_string());
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
    // (5)
    let thread = {
        let shared = Arc::clone(&shared);
        let readiness = readiness.clone();
        std::thread::Builder::new()
            .name("rdy-serve".into())
            .spawn(move || serve_thread(rt, daemon, shared, readiness))
    };
    let thread = match thread {
        Ok(t) => t,
        Err(e) => {
            // the runtime and the daemon went down with the closure: take the run files too
            handle.stop();
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
            Err(e) if e.code == E_LOCKED && Instant::now() < deadline => {
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

/// SIGINT and SIGTERM, registered on a runtime of their own (the daemon's runtime moves to the
/// `rdy-serve` thread).
struct StopSignals {
    rt: tokio::runtime::Runtime,
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
}

impl StopSignals {
    /// `None` (logged) if the handlers cannot be installed: the daemon then runs without them,
    /// and `rdy serve --stop` still stops it cleanly.
    fn install() -> Option<StopSignals> {
        use tokio::signal::unix::{SignalKind, signal};
        let installed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .and_then(|rt| {
                let (int, term) = {
                    let _g = rt.enter();
                    (
                        signal(SignalKind::interrupt())?,
                        signal(SignalKind::terminate())?,
                    )
                };
                Ok(StopSignals { rt, int, term })
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
        let StopSignals {
            rt,
            mut int,
            mut term,
        } = self;
        let spawned = std::thread::Builder::new()
            .name("rdy-signals".into())
            .spawn(move || {
                rt.block_on(async move {
                    let name = tokio::select! {
                        _ = int.recv() => "SIGINT",
                        _ = term.recv() => "SIGTERM",
                    };
                    log(&format!("{name}: stopping"));
                    handle.stop();
                    let name = tokio::select! {
                        _ = int.recv() => "SIGINT",
                        _ = term.recv() => "SIGTERM",
                    };
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

/// Runs the warm-up, then answers ready. A panic in the warm-up is logged and the daemon serves
/// with a cold cache: readiness is always reached, so no client waits out its start timeout.
pub(crate) fn warm_then_ready(readiness: &Readiness, warm: impl FnOnce()) {
    if let Err(p) = catch_unwind(AssertUnwindSafe(warm)) {
        log(&format!(
            "warm-up panicked ({}); serving with a cold cache",
            panic_text(&*p)
        ));
    }
    readiness.ready();
    log("ready");
}

/// The body of the `rdy-serve` thread: warms canon beside the accept loop, then, once the
/// daemon exits, closes the store.
fn serve_thread(
    rt: tokio::runtime::Runtime,
    daemon: Daemon,
    shared: Arc<Shared>,
    readiness: Readiness,
) -> Result<ExitReason, ServeError> {
    readiness.progress("building canon");
    log("building canon");
    let warm = Arc::clone(&shared);
    rt.spawn_blocking(move || {
        warm_then_ready(&readiness, || {
            let st = warm.engine().read();
            for scope in WARM_SCOPES {
                if let Err(e) = st
                    .materialized(scope)
                    .and_then(|_| st.graph(scope).map(drop))
                {
                    log(&format!("warm-up failed: {e}"));
                }
            }
        });
        // plan M2e2 (D-Z0EMP): what moved while no daemon ran (the pack, the rdy build) is
        // logged once, after ready, in a batch of cause `reopened`
        warm.writer.reconcile_soon();
    });
    let ran = rt.block_on(daemon.run());
    // waits for the blocking pool (warm-up, in-flight handlers) to finish while the writer still
    // serves them; then the store closes last, so the process exits right after releasing it
    drop(rt);
    close(&shared);
    log("store closed");
    ran.map_err(ServeError::Runtime)
}

/// One ctl connection's session: what the hello said and who the daemon classified it as.
#[derive(Clone, Debug)]
pub struct CtlSession {
    pub info: SessionInfo,
    pub session: Session,
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
        let human = self
            .human
            .clone()
            .unwrap_or_else(|| human_actor_for_env(&self.shared.engine().root()));
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
            },
        );
        Ok(reply)
    }
}

impl Handler for ServeHandler {
    fn ctl(&self, session: &SessionInfo, request: CtlRequest) -> Result<Value, ErrorReply> {
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
            cwd: &cs.info.cwd,
        };
        dispatch(&cx, &request.op, request.args).map_err(ErrorReply::from)
    }

    fn mcp(
        &self,
        session: SessionInfo,
        stream: tokio::net::UnixStream,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let engine = self.shared.engine();
        Box::pin(async move {
            let (r, w) = stream.into_split();
            if let Err(e) = rdy_mcp::server::serve_on(engine, session.id.0, r, w).await {
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
        let r = Readiness::new();
        warm_then_ready(&r, || panic!("warm-up test panic"));
        assert!(r.is_ready());
    }
}
