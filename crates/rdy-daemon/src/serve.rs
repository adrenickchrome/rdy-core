//! The host: opens the store, binds the socket, warms canon, answers `ready`; classifies each
//! ctl session with the `session` op and serves MCP per connection over the socket.
use crate::ctl::CtlRequest;
use crate::daemon::{Daemon, DaemonConfig, DaemonHandle, ExitReason, Readiness, STOP_GRACE, log};
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
use rdy_mcp::server::DaemonBuildWarning;
use rdy_model::Role;
use rdy_store::start_lock::{ClaimOutcome, HandoffToken, StartClaim, StartGuard};
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
/// How long a daemon waits for a name a daemon that is leaving still holds, its pipe or its stop
/// event: that one's whole drain, then a store's release. Past this the name is someone else's, or
/// stuck, and the start fails.
const HELD_NAME_RETRY: Duration =
    Duration::from_secs(STOP_GRACE.as_secs() + LOCKED_RETRY.as_secs());
/// The pause between two tries at a start's claim or lock.
const START_RETRY_EVERY: Duration = Duration::from_millis(2);

/// A point of a daemon's start where a test hook runs ([`with_start_hook_for_tests`]).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartPoint {
    /// A launched daemon has its token and has not claimed its start.
    BeforeClaim,
    /// It has claimed its start and has not taken `start.lock`.
    Claimed,
    /// It owns the start and has not revalidated anything or opened the store.
    Guarded,
    /// The store is open, with the start still held.
    Opened,
    /// The project's stop event stands (a foreground or detached `serve`), and no endpoint does.
    StopEvent,
    /// The endpoint is bound and recorded, with the start still held.
    Bound,
}

type StartHook = Box<dyn FnMut(StartPoint) -> io::Result<()>>;

thread_local! {
    /// This thread's start hook ([`with_start_hook_for_tests`]).
    static START_HOOK: std::cell::RefCell<Option<StartHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: runs `f` with `hook` called at each [`StartPoint`] of a start on this thread. An
/// error fails the start there. No CLI, MCP, config or environment path reaches it.
#[doc(hidden)]
pub fn with_start_hook_for_tests<T>(
    hook: impl FnMut(StartPoint) -> io::Result<()> + 'static,
    f: impl FnOnce() -> T,
) -> T {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            START_HOOK.with(|h| *h.borrow_mut() = None);
        }
    }
    START_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    let _off = Off;
    f()
}

/// Calls this thread's start hook, if any, at `point`.
fn start_point(point: StartPoint) -> Result<(), ServeError> {
    let Some(mut hook) = START_HOOK.with(|h| h.borrow_mut().take()) else {
        return Ok(());
    };
    let r = hook(point);
    START_HOOK.with(|h| *h.borrow_mut() = Some(hook));
    r.map_err(ServeError::Runtime)
}

pub struct ServeOptions {
    pub project: PathBuf,
    pub version: String,
    /// Run in a session of its own (`setsid`), for a daemon spawned by a client. It reads its
    /// start's token from its stdin (`spawn::serve_command`).
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
    /// What a daemon must have before it answers could not be had: its stop event.
    Start(io::Error),
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
            ServeError::Bind(e) => write!(f, "{}: {e}", sys::BIND_REFUSED),
            ServeError::Start(e) => write!(f, "could not start the daemon: {e}"),
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
    start_checked(opts, None, None)
}

/// `make`, tried again while another process holds the name it makes (`sys::name_is_held`).
fn outlasting_a_held_name<T>(mut make: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let until = Instant::now() + HELD_NAME_RETRY;
    loop {
        match make() {
            Err(e) if sys::name_is_held(&e) && Instant::now() < until => {
                std::thread::sleep(LOCKED_RETRY_EVERY);
            }
            out => return out,
        }
    }
}

/// [`start`], refusing before either HTTP site binds when canon does not hold `profile` (the
/// foreground profile-only start). `signals` claim the project between the store's open and the
/// bind: a client that can reach this daemon can always stop it.
fn start_checked(
    opts: ServeOptions,
    profile: Option<&str>,
    signals: Option<&mut StopSignals>,
) -> Result<ServeRun, ServeError> {
    // (1)
    if opts.detach {
        crate::spawn::detach().map_err(ServeError::Runtime)?;
    }
    let root = RunDir::of_project(&opts.project).root();
    // (2) the start first, then the store: start.lock before the store lock. The line comes
    // first too: a rival start that waits at the lock is a start that opens the store
    log("opening store");
    let start = match take_start(&opts) {
        Ok(s) => s,
        Err(err) => {
            say(&root, &err.to_string());
            return Err(err);
        }
    };
    let rdy = match open_store(&opts.project, start.as_ref()) {
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
    start_point(StartPoint::Opened)?;
    // (3)
    let shared = Shared::new(rdy);
    shared.set_daemon_identity(&opts.version, crate::BUILD);
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
    // (3c) the project's stop event, once the store is this process's. A daemon that is leaving may
    // hold it until its process has ended
    if let Some(signals) = signals {
        let claimed = outlasting_a_held_name(|| signals.signals.claim_project(&root))
            .map_err(ServeError::Start)
            .and_then(|()| start_point(StartPoint::StopEvent));
        if let Err(err) = claimed {
            close(&shared);
            say(&root, &err.to_string());
            return Err(err);
        }
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
        build: crate::BUILD.to_string(),
        idle_timeout: opts.idle_timeout,
        expected_account: None,
    };
    let handler = ServeHandler::new(Arc::clone(&shared), opts.human_actor.clone());
    let mut daemon = Daemon::new(config, Arc::new(handler));
    let daemon_bind = outlasting_a_held_name(|| rt.block_on(daemon.bind()));
    if let Err(e) = daemon_bind {
        close(&shared);
        return Err(ServeError::Bind(e));
    }
    let handle = daemon.handle();
    let readiness = daemon.readiness();
    if let Err(err) = start_point(StartPoint::Bound) {
        handle.stop();
        drop(daemon);
        close(&shared);
        return Err(err);
    }
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
    // the daemon owns the project now: a client that connects finds its socket and waits there
    drop(start);
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

/// The start of this process, held: what lets it open the store and bind its socket. A foreground
/// start takes its own guard. A daemon a client launched (`detach`) reads its token and claims the
/// start its client recorded for it; with no token (a stdin at its end) it is a start of its own
/// like any other. A directory that is no project gets no guard: the open refuses it in its own
/// words, with nothing made in it.
fn take_start(opts: &ServeOptions) -> Result<Option<StartGuard>, ServeError> {
    if rdy_engine::config_levels::require_project(&opts.project).is_err() {
        return Ok(None);
    }
    let run = RunDir::of_project(&opts.project);
    run.ensure().map_err(ServeError::Bind)?;
    if !opts.detach {
        return own_start(&opts.project, &run, Instant::now() + LOCKED_RETRY).map(Some);
    }
    let until = Instant::now() + rdy_engine::serve_config(&opts.project).start_timeout;
    match crate::spawn::inherited_token(until) {
        Some(token) => claim_start(&opts.project, &run, &token, until).map(Some),
        None => own_start(&opts.project, &run, until).map(Some),
    }
}

/// `start.lock` could not be taken: the file's or the system's own reason.
fn lock_refused(run: &RunDir, e: io::Error) -> ServeError {
    ServeError::Runtime(io::Error::new(
        e.kind(),
        format!("could not lock {}: {e}", run.start_lock().display()),
    ))
}

/// Takes `start.lock` with its reservation checked, retrying until `until`. Another owner at that
/// point is `AlreadyRunning`: it starts a daemon, or changes the store under this one.
fn own_start(project: &Path, run: &RunDir, until: Instant) -> Result<StartGuard, ServeError> {
    loop {
        match StartGuard::try_take(run.rdy_dir()) {
            Ok(Some(start)) => return Ok(start),
            Ok(None) if Instant::now() >= until => {
                return Err(ServeError::AlreadyRunning(store_dir(project)));
            }
            Ok(None) => std::thread::sleep(LOCKED_RETRY_EVERY),
            Err(e) => return Err(lock_refused(run, e)),
        }
    }
}

/// Takes over the start its client recorded for this process: claims it, takes `start.lock` once
/// the client lets go, and revalidates what may have moved while no one held the lock: the project
/// (the claim binds its identity), the endpoint and the upgrade plan. A start that is no longer
/// this process's, or a daemon that answers now, is a concurrent winner: `AlreadyRunning`, with
/// the store unopened.
fn claim_start(
    project: &Path,
    run: &RunDir,
    token: &HandoffToken,
    until: Instant,
) -> Result<StartGuard, ServeError> {
    let lost = || ServeError::AlreadyRunning(store_dir(project));
    start_point(StartPoint::BeforeClaim)?;
    let mut claim = loop {
        match StartClaim::try_claim(run.rdy_dir(), token, std::process::id()) {
            Ok(ClaimOutcome::Claimed(c)) => break c,
            Ok(ClaimOutcome::NotOurs) => {
                log("this start is no longer this daemon's: its client gave up, or another won");
                return Err(lost());
            }
            Ok(ClaimOutcome::Busy) if Instant::now() >= until => return Err(lost()),
            Ok(ClaimOutcome::Busy) => std::thread::sleep(START_RETRY_EVERY),
            Err(e) => return Err(lock_refused(run, e)),
        }
    };
    start_point(StartPoint::Claimed)?;
    let start = loop {
        match claim.try_into_guard() {
            Ok(Ok(start)) => break start,
            Ok(Err(_)) if Instant::now() >= until => return Err(lost()),
            Ok(Err(c)) => {
                claim = c;
                std::thread::sleep(START_RETRY_EVERY);
            }
            Err(e) => return Err(lock_refused(run, e)),
        }
    };
    start_point(StartPoint::Guarded)?;
    if crate::client::live_daemon(project).is_some() {
        log("another daemon serves this project: this one does not open the store");
        return Err(lost());
    }
    crate::maintenance::revalidate_plan(project, &start).map_err(ServeError::Open)?;
    Ok(start)
}

/// `Rdy::open`, retried while the store is `E_LOCKED` for up to [`LOCKED_RETRY`]: a predecessor
/// daemon removes its socket before it drains and closes its store (and its process keeps the
/// index open until it exits), so a successor spawned the moment the socket is gone, by a
/// version replace or after `rdy serve --stop`, finds the store still held for a few tens of ms.
/// A holder that keeps it (a `--no-daemon` command, a live daemon) is still `E_LOCKED` after the
/// bound. `start` is the start lock this process holds; `None` only for what is no project, which
/// the open refuses.
fn open_store(project: &Path, start: Option<&StartGuard>) -> Result<Rdy, RdyError> {
    let deadline = Instant::now() + LOCKED_RETRY;
    loop {
        let opened = match start {
            Some(start) => Rdy::open_under_start(project, start),
            None => Rdy::open(project),
        };
        match opened {
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
/// opens, so no signal finds the store open without them. The project's stop event comes once the
/// store is open and before the endpoint is bound (Windows, D-FPKPS), and goes when this returns.
pub fn serve(opts: ServeOptions) -> Result<ExitReason, ServeError> {
    let mut signals = StopSignals::install(&opts.project)?;
    let run = start_checked(opts, None, signals.as_mut())?;
    let watch = signals.and_then(|s| s.stop_on_signal(run.handle()));
    let out = run.wait();
    drop(watch);
    out
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
    let mut signals = StopSignals::install(&opts.project)?;
    let root = RunDir::of_project(&opts.project).root();
    let run = start_checked(opts, Some(key), signals.as_mut())?;
    let _watch = signals.and_then(|s| s.stop_on_signal(run.handle()));
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

/// SIGINT and SIGTERM, or the stop event, Ctrl-C and Ctrl-Break, registered on a runtime of their
/// own (the daemon's runtime moves to the `rdy-serve` thread).
struct StopSignals {
    rt: tokio::runtime::Runtime,
    signals: sys::StopSignals,
}

/// The `rdy-signals` thread. Dropping this ends it, once the daemon has stopped, and with it the
/// project's stop event.
struct SignalWatch {
    done: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        drop(self.done.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl StopSignals {
    /// Installs what can go before the store opens. A failure that the daemon can live with is
    /// logged and is `None`: it then runs without them, and `rdy serve --stop` still stops it
    /// cleanly. `project` names what they stop.
    fn install(project: &Path) -> Result<Option<StopSignals>, ServeError> {
        let installed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .and_then(|rt| {
                let signals = {
                    let _g = rt.enter();
                    sys::StopSignals::early()?
                };
                Ok(StopSignals { rt, signals })
            });
        match installed {
            Ok(s) => Ok(Some(s)),
            Err(e) if sys::StopSignals::REQUIRED => {
                let err = ServeError::Start(e);
                say(&RunDir::of_project(project).root(), &err.to_string());
                Err(err)
            }
            Err(e) => {
                log(&format!(
                    "could not install the SIGINT/SIGTERM handlers: {e}"
                ));
                Ok(None)
            }
        }
    }

    /// Waits on a thread of its own (`rdy-signals`), until the watch this returns is dropped.
    fn stop_on_signal(self, handle: DaemonHandle) -> Option<SignalWatch> {
        let StopSignals { rt, mut signals } = self;
        let (done, mut finished) = tokio::sync::oneshot::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("rdy-signals".into())
            .spawn(move || {
                rt.block_on(async move {
                    let name = tokio::select! {
                        name = signals.recv() => name,
                        _ = &mut finished => return,
                    };
                    log(&format!("{name}: stopping"));
                    handle.stop();
                    let name = tokio::select! {
                        name = signals.recv() => name,
                        _ = &mut finished => return,
                    };
                    log(&format!("{name} again: exiting without draining"));
                    std::process::exit(130);
                })
            });
        match spawned {
            Ok(thread) => Some(SignalWatch {
                done: Some(done),
                thread: Some(thread),
            }),
            Err(e) => {
                log(&format!("could not start the signal thread: {e}"));
                None
            }
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
        // ain4: a complete public instructions reply clears the reminder for the session's actor
        // and run, in the claims registry this daemon's MCP sessions share; the private form a
        // host's reader sends marks nothing
        let public_instructions =
            request.op == "instructions" && request.args.get("part") != Some(&Value::Bool(true));
        let mut out = dispatch(&cx, &request.op, request.args).map_err(ErrorReply::from);
        let key = (cs.session.actor.clone(), cs.session.run.clone());
        if public_instructions && out.is_ok() {
            self.shared.hub.claims().mark_instructions_read(&key);
        }
        // ain5: the CLI's overview and briefings carry the reminder while this actor and run
        // owe it, as the MCP sessions of this daemon do
        if let Ok(v) = &mut out {
            let owed = !self.shared.hub.claims().instructions_read(&key);
            crate::ops::with_reminder(&request.op, v, owed);
        }
        out
    }

    fn mcp(
        &self,
        session: SessionInfo,
        stream: crate::handler::McpStream,
        agent: Arc<dyn SigningAgent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let engine = self.shared.engine();
        let hub = Arc::clone(&self.shared.hub);
        // the version already matched at the hello; the same version and another build warns
        let warning = engine.daemon_identity().and_then(|(version, build)| {
            (session.version == version && session.build != build).then(|| DaemonBuildWarning {
                version,
                daemon_build: build,
                client_build: session.build.clone(),
            })
        });
        Box::pin(async move {
            let (r, w) = tokio::io::split(stream);
            if let Err(e) =
                rdy_mcp::server::serve_on(engine, session.id.0, hub, agent, warning, r, w).await
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
    use crate::client::{Attempt, connect};
    use crate::hello::{Hello, PROTO_VERSION};
    use crate::session::Proto;

    /// A client that hears `ready` can reach the pipe, and the stop event already stands: the
    /// event comes before the pipe, so what a client can reach can always be stopped. The event
    /// goes when the daemon has stopped. Unix has no such object: it stops over the socket.
    #[test]
    fn wx17_ready_means_pipe_and_stop_event_exist() {
        let d = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(d.path()).unwrap();
        Rdy::init(&project, "rdy-standard")
            .unwrap()
            .close()
            .unwrap();
        let run = RunDir::of_project(&project);
        let root = run.root();
        let (points, seen) = mpsc::channel();
        let serving = {
            let project = project.clone();
            std::thread::spawn(move || {
                with_start_hook_for_tests(
                    move |point| {
                        points.send(point).unwrap();
                        Ok(())
                    },
                    || {
                        serve(ServeOptions {
                            project,
                            version: "1.0.0".into(),
                            detach: false,
                            idle_timeout: None,
                            human_actor: None,
                            site_lan_override: None,
                        })
                    },
                )
            })
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let hello = |project| Hello {
            rdy: PROTO_VERSION,
            version: "1.0.0".into(),
            build: crate::BUILD.into(),
            proto: Proto::Ctl,
            cwd: root.clone(),
            project,
        };
        let long = Instant::now() + Duration::from_secs(120);
        let mut client = loop {
            assert!(Instant::now() < long, "the daemon never answered ready");
            let Ok(id) = run.project_id() else { continue };
            match rt.block_on(connect(&run, &hello(id))) {
                Attempt::Ready(c) => break c,
                Attempt::Starting(p) => {
                    break rt.block_on(p.wait(long, &mut |_: &str| {})).unwrap();
                }
                Attempt::NoDaemon(_) => std::thread::sleep(LOCKED_RETRY_EVERY),
                Attempt::Exiting => panic!("this daemon has no reason to leave"),
                Attempt::Refused(e) => panic!("{e:?}"),
            }
        };
        assert_eq!(
            seen.try_iter().collect::<Vec<_>>(),
            [StartPoint::Opened, StartPoint::StopEvent, StartPoint::Bound]
        );
        if !sys::signal_project_stop(&root).unwrap() {
            rt.block_on(client.ctl("stop", json!({}))).unwrap();
        }
        assert!(matches!(serving.join().unwrap(), Ok(ExitReason::Stopped)));
        client.close();
        match sys::signal_project_stop(&root) {
            Ok(signalled) => assert!(!signalled),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}"),
        }
    }

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
