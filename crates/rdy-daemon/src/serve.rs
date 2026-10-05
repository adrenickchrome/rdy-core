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
use rdy_engine::{E_LOCKED, Rdy, RdyError, Scope, Session, SessionKind};
use rdy_engine::{classify, human_actor_from_env};
use rdy_model::Role;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The scopes built before the daemon answers `ready`.
pub const WARM_SCOPES: &[Scope] = &[Scope::Canon];

pub struct ServeOptions {
    pub project: PathBuf,
    pub version: String,
    /// Run in a session of its own (`setsid`), for a daemon spawned by a client.
    pub detach: bool,
    /// `None` never idles out (a foreground `rdy serve`).
    pub idle_timeout: Option<Duration>,
    /// Test hook: `Some` replaces `human_actor_from_env()` for session classification.
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
    let rdy = match Rdy::open(&opts.project) {
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

/// `start(opts)?.wait()`.
pub fn serve(opts: ServeOptions) -> Result<ExitReason, ServeError> {
    start(opts)?.wait()
}

/// Closes the store, logging a failed close.
fn close(shared: &Shared) {
    if let Err(e) = shared.shutdown() {
        log(&format!(
            "the store did not close cleanly ({e}); the next open catches its index up"
        ));
    }
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
        let st = warm.engine().read();
        for scope in WARM_SCOPES {
            if let Err(e) = st
                .materialized(scope)
                .and_then(|_| st.graph(scope).map(drop))
            {
                log(&format!("warm-up failed: {e}"));
            }
        }
        drop(st);
        readiness.ready();
        log("ready");
    });
    let ran = rt.block_on(daemon.run());
    close(&shared);
    log("store closed");
    // waits for the blocking pool (warm-up, in-flight handlers) to finish
    drop(rt);
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
    /// Replaces `human_actor_from_env()` when set (a test hook).
    human: Option<String>,
    ctl: Mutex<HashMap<SessionId, CtlSession>>,
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
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, CtlSession>> {
        self.ctl.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open_session(&self, info: &SessionInfo, args: Value) -> Result<Value, ErrorReply> {
        let args = if args.is_null() { json!({}) } else { args };
        let a: SessionArgs = serde_json::from_value(args).map_err(|e| {
            ErrorReply::new(
                E_BAD_INPUT,
                format!("session: {e}"),
                "args for session: {actor?, run?}",
            )
        })?;
        let human = self.human.clone().unwrap_or_else(human_actor_from_env);
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

    fn session_ended(&self, session: &SessionInfo) {
        self.sessions().remove(&session.id);
        self.shared.end_session(session.id);
    }
}
