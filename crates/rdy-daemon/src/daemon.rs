//! The daemon: the accept loop, the per-connection hello and handshake, and stop.

use crate::ctl::{DaemonInfo, serve_ctl};
use crate::error::{E_DAEMON_START, E_DAEMON_VERSION, ErrorReply};
use crate::handler::Handler;
use crate::hello::{HelloReply, bad_build, read_hello, valid_build};
use crate::layout::{ProjectId, RunDir};
use crate::session::{Proto, SessionInfo, Sessions};
use crate::socket;
use crate::sys;
use rdy_store::fileid::FileId;
use rdy_store::layout::STORE_DIR;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

/// How long a detached daemon with no connections waits before exiting.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `run` waits for connection tasks after a stop.
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// The least time between two `starting` replies on one connection.
pub const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(200);

/// How often a daemon with no connections checks that its project's `.rdy` is still the one it
/// bound (final ruling: a daemon whose project is gone or replaced exits). The writer's idle tick.
pub const PROJECT_TICK: Duration = crate::writer::CHECKPOINT_TICK;

/// Read past a final reply at most this much before closing (see `close_after_reply`).
pub(crate) const DRAIN_AFTER_REPLY_BYTES: usize = 64 * 1024;

/// One daemon log line on stderr: `<unix ms> rdy serve: <text>`. A failed write (a full disk
/// under `daemon.log`, a closed stderr) loses the line, never the daemon.
pub(crate) fn log(text: &str) {
    if captured(text) {
        return;
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = writeln!(io::stderr().lock(), "{ms} rdy serve: {text}");
}

/// The live [`LogCapture`]s: each one's expected texts and the lines it took.
type Captures = Vec<(u64, Vec<String>, Vec<String>)>;

static CAPTURES: Mutex<Captures> = Mutex::new(Vec::new());
/// How many [`LogCapture`]s are live, so a daemon with none never takes the lock.
static CAPTURING: AtomicUsize = AtomicUsize::new(0);

/// Whether a live capture expects `text` (it holds one of the capture's expected texts); it
/// then takes the line instead of stderr.
fn captured(text: &str) -> bool {
    if CAPTURING.load(Ordering::SeqCst) == 0 {
        return false;
    }
    let Ok(mut all) = CAPTURES.lock() else {
        return false;
    };
    for (_, expected, lines) in all.iter_mut() {
        if expected.iter().any(|e| text.contains(e.as_str())) {
            lines.push(text.to_string());
            return true;
        }
    }
    false
}

/// Test hook: while the returned capture lives, a daemon log line (any thread) that holds one
/// of `expected` goes to it instead of stderr, so a test asserts the diagnostics it provokes
/// and every unexpected line stays visible.
#[doc(hidden)]
pub fn capture_log_for_tests(expected: &[&str]) -> LogCapture {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let expected = expected.iter().map(|e| e.to_string()).collect();
    CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, expected, vec![]));
    CAPTURING.fetch_add(1, Ordering::SeqCst);
    LogCapture { id }
}

/// The daemon log lines a test expects ([`capture_log_for_tests`]); dropping it ends the capture.
#[doc(hidden)]
pub struct LogCapture {
    id: u64,
}

impl LogCapture {
    /// The lines taken so far, in order, without their time prefix.
    pub fn lines(&self) -> Vec<String> {
        let all = CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
        all.iter()
            .find(|(id, _, _)| *id == self.id)
            .map(|(_, _, lines)| lines.clone())
            .unwrap_or_default()
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        let mut all = CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|(id, _, _)| *id != self.id);
        CAPTURING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The peer went away mid-exchange (a client that closed or was killed before its reply): a
/// normal end of a connection, not an error to log.
fn is_hang_up(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
}

pub struct DaemonConfig {
    pub run: RunDir,
    pub version: String,
    /// This daemon's build id; `new` takes this binary's. A test may set another valid id.
    pub build: String,
    /// `None` never idles out (a foreground `rdy serve`).
    pub idle_timeout: Option<Duration>,
    /// Test hook: the peer account to accept instead of our own.
    #[doc(hidden)]
    pub expected_account: Option<sys::Account>,
}

impl DaemonConfig {
    pub fn new(run: RunDir, version: String) -> Self {
        DaemonConfig {
            run,
            version,
            build: crate::BUILD.to_string(),
            idle_timeout: None,
            expected_account: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Starting(String),
    Ready,
}

/// The host's side of the `starting`/`ready` replies. Cheap to clone.
#[derive(Clone)]
pub struct Readiness(Arc<watch::Sender<Status>>);

impl Readiness {
    pub(crate) fn new() -> Self {
        Readiness(Arc::new(
            watch::channel(Status::Starting("starting".into())).0,
        ))
    }

    /// Sets the progress text sent in `starting` replies. Ignored once ready.
    pub fn progress(&self, text: &str) {
        self.0.send_if_modified(|s| match s {
            Status::Starting(p) if p != text => {
                *p = text.to_string();
                true
            }
            _ => false,
        });
    }

    /// From now on, hellos are answered `ready` and waiting ones are released.
    pub fn ready(&self) {
        self.0.send_replace(Status::Ready);
    }

    pub fn is_ready(&self) -> bool {
        matches!(*self.0.borrow(), Status::Ready)
    }

    fn subscribe(&self) -> watch::Receiver<Status> {
        self.0.subscribe()
    }
}

/// Why `run` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// `DaemonHandle::stop` or the `stop` op.
    Stopped,
    /// No connections for the idle timeout.
    Idle,
    /// A client of another version arrived while no session was open.
    VersionReplaced,
    /// A client of the same version and another build arrived with no other connection, or the
    /// last connection closed after one came.
    BuildReplaced,
    /// The project's `.rdy` is gone or replaced (deleted, moved away, restored from a copy), or its
    /// store is (`project_parts`): the daemon serves nothing there and exits
    /// without writing to its store.
    ProjectGone,
}

impl ExitReason {
    fn label(self) -> &'static str {
        match self {
            ExitReason::Stopped => "stopped",
            ExitReason::Idle => "idle",
            ExitReason::VersionReplaced => "version replaced",
            ExitReason::BuildReplaced => "build replaced",
            ExitReason::ProjectGone => "project gone",
        }
    }
}

/// Controls a daemon from outside its `run`. Cheap to clone.
#[derive(Clone)]
pub struct DaemonHandle {
    info: DaemonInfo,
    run: RunDir,
    files: RunFiles,
    tick: Arc<Notify>,
    keepers: Keepers,
    check: Arc<ProjectCheck>,
}

/// What keeps a daemon from idling out: its open socket connections plus its holds (a live page
/// in view, ls6). `count` carries the sum; every change of either part writes it under the
/// watch's lock, so concurrent changes publish their sums in order.
#[derive(Clone)]
struct Keepers {
    connections: Arc<AtomicUsize>,
    holds: Arc<AtomicUsize>,
    count: Arc<watch::Sender<usize>>,
    /// Whether a client of another build came (D-CCC6E): the daemon exits when its last
    /// connection closes. Read and written only inside `update`, under the watch's lock, as the
    /// connection count changes. Never cleared.
    replace: Arc<AtomicBool>,
    /// Whether the daemon is leaving to be replaced: its run files are gone or going. Under the
    /// same lock.
    leaving: Arc<AtomicBool>,
}

/// What a hello of another build, or any hello after one, finds under the keepers' lock.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BuildCheck {
    /// Serve this client.
    Serve,
    /// The daemon is leaving; this client hears `exiting`.
    Leaving,
    /// This client is alone with another build: the daemon leaves now.
    Replace,
}

impl Keepers {
    /// Applies `change` under the watch's lock and publishes the new sum.
    fn update(&self, change: impl FnOnce(&Self)) {
        self.count.send_modify(|n| {
            change(self);
            *n = self.connections.load(Ordering::SeqCst) + self.holds.load(Ordering::SeqCst);
        });
    }
}

/// While one lives, the daemon's idle timer does not run (ls6: a live page in view keeps the
/// daemon up; a page in a background tab closes its stream, ruling task-6). The project check
/// still runs on the idle tick, which counts socket connections only (D-A6ZZA).
pub struct IdleHold(Keepers);

impl Drop for IdleHold {
    fn drop(&mut self) {
        self.0.update(|k| {
            k.holds.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

impl DaemonHandle {
    /// Asks `run` to stop and returns at once. The socket and the pid file are removed here, so
    /// no new client can reach the daemon once this returns, and `run` then leaves those paths
    /// alone (a successor may already have re-created them); `run` drains and returns. A second
    /// stop is a no-op.
    pub fn stop(&self) {
        // before the signal, so `run` cannot get to the files first and this return before
        // they are gone
        self.files.remove(&self.run);
        self.info
            .stop
            .send_if_modified(|s| !std::mem::replace(s, true));
    }

    pub fn sessions(&self) -> Sessions {
        self.info.sessions.clone()
    }

    /// Open connections, from accept to close, whether or not past their hello.
    pub fn connections(&self) -> usize {
        self.info.connections.load(Ordering::SeqCst)
    }

    /// The socket this daemon bound.
    pub fn socket(&self) -> PathBuf {
        self.files.socket(&self.run)
    }

    /// Test hook: runs the idle tick's project check now (it runs every `PROJECT_TICK` while no
    /// connection is open), so a test needs no sleep.
    #[doc(hidden)]
    pub fn tick(&self) {
        self.tick.notify_one();
    }

    /// Holds the daemon up: the idle timer does not run while the hold lives.
    pub fn hold(&self) -> IdleHold {
        self.keepers.update(|k| {
            k.holds.fetch_add(1, Ordering::SeqCst);
        });
        IdleHold(self.keepers.clone())
    }

    /// Open socket connections plus holds: while above 0, the daemon does not idle out.
    pub fn keepers(&self) -> usize {
        *self.keepers.count.borrow()
    }

    /// [`Self::keepers`] as a watch.
    pub fn keepers_watch(&self) -> watch::Receiver<usize> {
        self.keepers.count.subscribe()
    }

    /// Turns true when the daemon stops, for any reason.
    pub fn stopped(&self) -> watch::Receiver<bool> {
        self.info.stop.subscribe()
    }

    /// The project check (D-A6ZZA) that a hello runs: when the project's `.rdy` or another of its
    /// parts (`project_parts`) is gone or replaced, removes this daemon's socket record and socket
    /// that still name it and stops the daemon with `ProjectGone`; `daemon.pid` stays until
    /// [`Self::drained`]. True when the project is gone. False before the bind.
    pub fn check_project(&self) -> bool {
        self.check.check()
    }

    /// The host's word that the daemon's last write has ended: removes `daemon.pid` while it
    /// still names this process. A daemon that the project check stops leaves that file in place
    /// when `run` returns, because a host may still finish writes on the blocking pool, and a
    /// client keeps waiting while the file names a live process (p-3MJBQ). Every other stop has
    /// removed it already, and then this does nothing.
    pub fn drained(&self) {
        self.files.lock().remove_pid(&self.run);
    }
}

pub struct Daemon {
    config: DaemonConfig,
    handler: Arc<dyn Handler>,
    info: DaemonInfo,
    readiness: Readiness,
    /// Set by the path that stops the daemon for a reason other than `Stopped`.
    reason: Arc<OnceLock<ExitReason>>,
    /// `info.connections` plus the holds, so `run` can watch the sum reach 0 (the idle timer).
    keepers: Keepers,
    files: RunFiles,
    listener: Option<sys::Listener>,
    /// The project check; it holds the identity of the project's `.rdy` and of its other parts
    /// from the bind on, and hellos from another project are refused.
    check: Arc<ProjectCheck>,
    /// `DaemonHandle::tick`: an idle tick now.
    tick: Arc<Notify>,
}

impl Daemon {
    pub fn new(config: DaemonConfig, handler: Arc<dyn Handler>) -> Daemon {
        let info = DaemonInfo {
            pid: std::process::id(),
            version: config.version.clone(),
            build: config.build.clone(),
            started_at: Instant::now(),
            sessions: Sessions::default(),
            connections: Arc::new(AtomicUsize::new(0)),
            stop: Arc::new(watch::channel(false).0),
        };
        let reason = Arc::new(OnceLock::new());
        let files = RunFiles::default();
        let keepers = Keepers {
            connections: Arc::clone(&info.connections),
            holds: Arc::new(AtomicUsize::new(0)),
            count: Arc::new(watch::channel(0).0),
            replace: Arc::new(AtomicBool::new(false)),
            leaving: Arc::new(AtomicBool::new(false)),
        };
        let check = Arc::new(ProjectCheck {
            root: config.run.root(),
            project: OnceLock::new(),
            parts: OnceLock::new(),
            reason: Arc::clone(&reason),
            run: config.run.clone(),
            files: files.clone(),
            stop: Arc::clone(&info.stop),
        });
        Daemon {
            config,
            handler,
            info,
            readiness: Readiness::new(),
            reason,
            keepers,
            files,
            listener: None,
            check,
            tick: Arc::new(Notify::new()),
        }
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle {
            info: self.info.clone(),
            run: self.config.run.clone(),
            files: self.files.clone(),
            tick: Arc::clone(&self.tick),
            keepers: self.keepers.clone(),
            check: Arc::clone(&self.check),
        }
    }

    /// Binds the socket, writes the pid file and records the socket (`socket::bind`). Call only
    /// while holding the store lock.
    pub async fn bind(&mut self) -> io::Result<()> {
        let (listener, path, project) = socket::bind(&self.config.run).await?;
        self.files.bound(path, project);
        let _ = self.check.project.set(project);
        let _ = self
            .check
            .parts
            .set(project_parts(&self.check.root.join(".rdy")));
        self.listener = Some(listener);
        Ok(())
    }

    /// Serves connections until stopped, idle for `idle_timeout`, or replaced by another
    /// version. Then, in order: stops accepting and drops the listener, removes the socket and
    /// the pid file (unless `DaemonHandle::stop` already did), tells every connection to finish,
    /// and waits for them up to `STOP_GRACE`. After `ProjectGone` the pid file stays: the host
    /// removes it with `DaemonHandle::drained` once its last write has ended.
    pub async fn run(mut self) -> io::Result<ExitReason> {
        let (mut listener, project) = self
            .listener
            .take()
            .zip(self.check.project.get().copied())
            .ok_or_else(|| io::Error::other("run before bind"))?;
        log(&format!(
            "listening on {}",
            self.files.socket(&self.config.run).display()
        ));
        let ctx = Arc::new(Conn {
            version: self.config.version.clone(),
            build: self.config.build.clone(),
            expected_account: match self.config.expected_account {
                Some(account) => account,
                None => sys::own_account()?,
            },
            handler: Arc::clone(&self.handler),
            info: self.info.clone(),
            readiness: self.readiness.clone(),
            reason: Arc::clone(&self.reason),
            run: self.config.run.clone(),
            files: self.files.clone(),
            project,
            root: self.config.run.root(),
            check: Arc::clone(&self.check),
            keepers: self.keepers.clone(),
        });
        let idle = self.config.idle_timeout;
        let mut stop = self.info.stop.subscribe();
        let mut count = self.keepers.count.subscribe();
        // armed at the start when nothing keeps the daemon (a hold taken before this subscribe
        // is never seen as a change), and on each drop of the count to 0
        let mut idle_at = if *count.borrow_and_update() == 0 {
            idle.map(|t| tokio::time::Instant::now() + t)
        } else {
            None
        };
        // the project check while no connection is open (a connection checks at its hello)
        let mut project_tick = tokio::time::interval(PROJECT_TICK);
        project_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let tick = Arc::clone(&self.tick);
        let mut tasks = JoinSet::new();
        loop {
            if *stop.borrow_and_update() {
                break;
            }
            while tasks.try_join_next().is_some() {}
            let idle_now = self.info.connections.load(Ordering::SeqCst) == 0;
            tokio::select! {
                _ = stop.changed() => {}
                _ = project_tick.tick(), if idle_now => {
                    ctx.check_project();
                }
                _ = tick.notified() => {
                    if self.info.connections.load(Ordering::SeqCst) == 0 {
                        ctx.check_project();
                    }
                }
                // the sender lives in `self`, so this never errors
                _ = count.changed() => {
                    idle_at = if *count.borrow_and_update() == 0 {
                        idle.map(|t| tokio::time::Instant::now() + t)
                    } else {
                        None
                    };
                }
                _ = sleep_until_some(idle_at), if idle_at.is_some() => {
                    let secs = idle.unwrap_or_default().as_secs_f64();
                    log(&format!("idle for {secs} s, exiting"));
                    let _ = self.reason.set(ExitReason::Idle);
                    self.info.stop.send_replace(true);
                }
                accepted = listener.accept() => match accepted {
                    Ok(stream) => {
                        self.keepers.update(|k| {
                            k.connections.fetch_add(1, Ordering::SeqCst);
                        });
                        idle_at = None;
                        let end = ConnEnd {
                            sessions: self.info.sessions.clone(),
                            keepers: self.keepers.clone(),
                            handler: Arc::clone(&self.handler),
                            session: None,
                            ctx: Arc::clone(&ctx),
                        };
                        tasks.spawn(Arc::clone(&ctx).serve(stream, end));
                    }
                    Err(e) => {
                        log(&format!("accept failed: {e}"));
                        // e.g. out of fds: don't spin
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
            }
        }
        let reason = self.reason.get().copied().unwrap_or(ExitReason::Stopped);
        // (1) stop accepting
        drop(listener);
        // (2) the socket and the pid file, unless `DaemonHandle::stop` already removed them. A
        // daemon whose project is gone keeps its pid file for the host (`DaemonHandle::drained`)
        if reason == ExitReason::ProjectGone {
            self.files.remove_endpoint(&self.config.run);
        } else {
            self.files.remove(&self.config.run);
        }
        // (3) every connection task sees `info.stop` and finishes
        while tasks.try_join_next().is_some() {}
        if reason != ExitReason::Idle {
            log(&format!(
                "stop requested, draining {} connections",
                tasks.len()
            ));
        }
        // (4) the bounded wait
        let drained = tokio::time::timeout(STOP_GRACE, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            log(&format!(
                "{} connections still open after {} s; abandoning them",
                tasks.len(),
                STOP_GRACE.as_secs()
            ));
        }
        // dropping the set aborts what is left; each task's `ConnEnd` still runs. A handler
        // inside `spawn_blocking` finishes on its own thread and its reply goes nowhere.
        drop(tasks);
        log(&format!("exiting ({})", reason.label()));
        Ok(reason)
    }
}

/// Sleeps until `at`; never completes for `None` (the caller guards the branch anyway).
async fn sleep_until_some(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The removal of this daemon's socket record, endpoint and pid file, done exactly once by
/// whichever stop path gets there first. Each goes only while it is still this daemon's: the paths
/// may belong to a successor, or to the project that replaced this one (final ruling). The record
/// goes only while it names this daemon's endpoint and project, the pid file only while it names
/// this process, and the endpoint only while it is the one this daemon bound. The lock is held
/// across the removal, so every caller returns only once the files are gone. The endpoint is the
/// one `bind` bound, so a project renamed or deleted while the daemon runs still has its socket
/// removed. Before a bind nothing is this daemon's.
///
/// The record and the endpoint can go before the pid file (`remove_endpoint`): a daemon that the
/// project check stops takes no new client, and keeps `daemon.pid` until its writes in flight have
/// drained, since a client keeps waiting while that file names a live process (p-3MJBQ).
#[derive(Clone, Default)]
struct RunFiles(Arc<Mutex<Files>>);

#[derive(Default)]
struct Files {
    /// The record and the endpoint are removed, or were not this daemon's when the first
    /// removal came.
    endpoint_removed: bool,
    /// The same for the pid file.
    pid_removed: bool,
    bound: Option<Bound>,
}

impl Files {
    fn remove_endpoint(&mut self, run: &RunDir) {
        if std::mem::replace(&mut self.endpoint_removed, true) {
            return;
        }
        let Some(bound) = &self.bound else {
            return;
        };
        // the record first: no client follows it to a socket that is going away
        if run.record_names(&bound.socket, bound.project) {
            remove_if_present(&run.socket_record());
        }
        if bound.socket_id.is_some() && sys::endpoint_identity(&bound.socket) == bound.socket_id {
            remove_if_present(&bound.socket);
        }
    }

    fn remove_pid(&mut self, run: &RunDir) {
        if std::mem::replace(&mut self.pid_removed, true) || self.bound.is_none() {
            return;
        }
        let own = sys::own_pid_record().ok();
        let own = own.as_deref().and_then(sys::parse_pid_record);
        if own.is_some() && run.pid_record() == own {
            remove_if_present(&run.pid_file());
        }
    }
}

/// What a bind made.
struct Bound {
    socket: PathBuf,
    /// What tells the endpoint from a later one at its path (`sys::endpoint_identity`).
    socket_id: Option<FileId>,
    project: ProjectId,
}

impl RunFiles {
    fn lock(&self) -> std::sync::MutexGuard<'_, Files> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bound(&self, socket: PathBuf, project: ProjectId) {
        self.lock().bound = Some(Bound {
            socket_id: sys::endpoint_identity(&socket),
            socket,
            project,
        });
    }

    fn socket(&self, run: &RunDir) -> PathBuf {
        self.lock()
            .bound
            .as_ref()
            .map_or_else(|| run.socket(), |b| b.socket.clone())
    }

    /// The record, the endpoint and the pid file, whichever are still to go.
    fn remove(&self, run: &RunDir) {
        let mut files = self.lock();
        files.remove_endpoint(run);
        files.remove_pid(run);
    }

    /// The record and the endpoint only; the pid file stays (see the type).
    fn remove_endpoint(&self, run: &RunDir) {
        self.lock().remove_endpoint(run);
    }
}

fn remove_if_present(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        log(&format!("could not remove {}: {e}", path.display()));
    }
}

/// What every connection task shares.
struct Conn {
    version: String,
    build: String,
    expected_account: sys::Account,
    handler: Arc<dyn Handler>,
    info: DaemonInfo,
    readiness: Readiness,
    reason: Arc<OnceLock<ExitReason>>,
    run: RunDir,
    files: RunFiles,
    /// The project this daemon serves (its `.rdy` at the bind) and its canonical root.
    project: ProjectId,
    root: PathBuf,
    check: Arc<ProjectCheck>,
    keepers: Keepers,
}

/// The project check (final ruling, D-A6ZZA), shared by every socket connection, the idle tick
/// and every `DaemonHandle` (the site runs it at each accepted HTTP connection, ls6).
/// What the project check watches beside `.rdy`, with its identity now: the store's directory and
/// its lock file, each if it stands. On every platform a delete can stop part way or leave
/// directories behind (D-6JZ5V), so `.rdy` may stand with no store in it, and a daemon would run
/// on with nothing to serve. The daemon holds the store's lock, and only its own end changes these
/// two. It is read once, at the bind; the check adds no timer.
fn project_parts(rdy_dir: &Path) -> Vec<(PathBuf, FileId)> {
    let store = rdy_dir.join(STORE_DIR);
    [store.join("lock"), store]
        .into_iter()
        .filter_map(|path| FileId::lstat(&path).ok().map(|id| (path, id)))
        .collect()
}

struct ProjectCheck {
    /// The canonical project root.
    root: PathBuf,
    /// The identity of the project's `.rdy` at the bind; unset before it.
    project: OnceLock<ProjectId>,
    /// What else stands for the project, with its identity at the bind (`project_parts`).
    parts: OnceLock<Vec<(PathBuf, FileId)>>,
    reason: Arc<OnceLock<ExitReason>>,
    run: RunDir,
    files: RunFiles,
    stop: Arc<watch::Sender<bool>>,
}

impl ProjectCheck {
    /// Whether `<root>/.rdy` (from `lstat`) is no longer `project`, the `.rdy` this daemon bound
    /// with: gone, or another directory (a restore, a copy moved into place). The same for each of
    /// the project's other parts.
    fn gone(&self, project: ProjectId) -> bool {
        let changed = |path: &Path, bound: FileId| match FileId::lstat(path) {
            Ok(id) => id != bound,
            Err(_) => true,
        };
        changed(&self.root.join(".rdy"), project)
            || self
                .parts
                .get()
                .is_some_and(|parts| parts.iter().any(|(path, id)| changed(path, *id)))
    }

    /// When the project is gone, removes this daemon's socket record and endpoint while they
    /// still name it (`RunFiles::remove_endpoint`), then stops with `ProjectGone`. `daemon.pid`
    /// stays until the host's writes in flight have drained (`DaemonHandle::drained`). True when
    /// the project is gone; false before the bind.
    fn check(&self) -> bool {
        let Some(project) = self.project.get().copied() else {
            return false;
        };
        if !self.gone(project) {
            return false;
        }
        if self.reason.set(ExitReason::ProjectGone).is_ok() {
            log(&format!(
                "the project's .rdy at {} is gone or replaced; exiting",
                self.root.display()
            ));
        }
        // before any reply: a client's retry may start a successor at once
        self.files.remove_endpoint(&self.run);
        self.stop.send_replace(true);
        true
    }
}

/// The refusal of a hello from another project: the daemon on `socket` serves the project at
/// `root` as process `pid` (ruling task-3-round1; the final ruling names the process). A daemon
/// whose own project is gone or replaced is `stopping`: its error says so and carries
/// `stopping: true`, and the client retries once, which starts the project's own daemon.
/// Otherwise the client's own daemon is not running.
fn another_project(socket: &Path, root: &Path, pid: u32, stopping: bool) -> ErrorReply {
    // D-EDS38: the root as a person reads it; identity and I/O keep the canonical path
    let message = format!(
        "the daemon on {} serves another project ({}, process {pid})",
        socket.display(),
        rdy_store::pio::display_path(root)
    );
    if !stopping {
        return ErrorReply::new(
            E_DAEMON_START,
            message,
            "this project's daemon is not running; run rdy serve here, or remove a stale .rdy/run/daemon.sock copied from another project",
        );
    }
    let mut e = ErrorReply::new(
        E_DAEMON_START,
        message,
        format!(
            "that daemon is stopping; run the command again. If it keeps answering, end process {pid}"
        ),
    );
    e.extra.insert("stopping".into(), true.into());
    e
}

/// The end of a connection, run on every way out of its task (return, panic, abort): removes
/// the session if one was opened, calls `session_ended` once, and uncounts the connection.
struct ConnEnd {
    sessions: Sessions,
    keepers: Keepers,
    handler: Arc<dyn Handler>,
    session: Option<SessionInfo>,
    ctx: Arc<Conn>,
}

impl Drop for ConnEnd {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            self.sessions.remove(s.id);
            let handler = Arc::clone(&self.handler);
            // a panicking hook must not abort the process from inside a drop
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.session_ended(&s)))
                .is_err()
            {
                log(&format!("session_ended panicked for session {}", s.id));
            }
        }
        // under the watch's lock, so concurrent ends publish their counts in order; the sum
        // keeps the holds. The last connection of a latched daemon stops it: holds and site
        // pages are not connections (D-CCC6E)
        let mut last = false;
        self.keepers.update(|k| {
            let left = k.connections.fetch_sub(1, Ordering::SeqCst) - 1;
            if left == 0
                && k.replace.load(Ordering::SeqCst)
                && !k.leaving.swap(true, Ordering::SeqCst)
            {
                // before a new client can be told anything: the next one starts a successor
                self.ctx.files.remove(&self.ctx.run);
                last = true;
            }
        });
        if last {
            log(
                "the last connection closed after a client of another build; exiting to be replaced",
            );
            let _ = self.ctx.reason.set(ExitReason::BuildReplaced);
            self.ctx.info.stop.send_replace(true);
        }
    }
}

/// Aborts a spawned task when dropped, so an abandoned connection takes its MCP session along.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn write_reply<W: AsyncWrite + Unpin>(w: &mut W, reply: &HelloReply) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(reply).map_err(io::Error::other)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Writes a final reply and closes. Bytes the client already sent are read off first: closing a
/// Unix socket with unread input makes Linux report ECONNRESET to the peer, which could hide the
/// reply it is about to read.
async fn close_after_reply(mut stream: sys::Stream, reply: &HelloReply) {
    if write_reply(&mut stream, reply).await.is_err() {
        return;
    }
    let _ = stream.shutdown().await;
    let mut buf = [0u8; 4096];
    let mut drained = 0;
    while drained < DRAIN_AFTER_REPLY_BYTES {
        match stream.try_read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

impl Conn {
    /// The project check (final ruling, [`ProjectCheck::check`]). Run at each hello and on the
    /// idle tick.
    fn check_project(&self) -> bool {
        self.check.check()
    }

    /// The build step, under the lock that changes the connection count. Another build alone
    /// removes the run files, before its `exiting` reply, and stops the daemon; with another
    /// connection it latches the replacement and is served.
    fn check_build(&self, build: &str) -> BuildCheck {
        let mut check = BuildCheck::Serve;
        self.keepers.update(|k| {
            if k.leaving.load(Ordering::SeqCst) {
                check = BuildCheck::Leaving;
            } else if build != self.build {
                if k.connections.load(Ordering::SeqCst) == 1 {
                    k.leaving.store(true, Ordering::SeqCst);
                    self.files.remove(&self.run);
                    check = BuildCheck::Replace;
                } else {
                    k.replace.store(true, Ordering::SeqCst);
                }
            }
        });
        if check == BuildCheck::Replace {
            log(&format!(
                "a client of build {build} arrived with no other connection; exiting to be replaced"
            ));
            let _ = self.reason.set(ExitReason::BuildReplaced);
            self.info.stop.send_replace(true);
        }
        check
    }

    async fn serve(self: Arc<Self>, mut stream: sys::Stream, mut end: ConnEnd) {
        // (a) the peer check
        let peer = match sys::check_client(&stream, self.expected_account) {
            Ok(process) => process.account(),
            Err(refusal) => {
                log(&refusal);
                return;
            }
        };
        // (b) the hello; a stop closes a connection still in it
        let mut stop = self.info.stop.subscribe();
        if *stop.borrow_and_update() {
            return;
        }
        let read = tokio::select! {
            biased;
            _ = stop.changed() => return,
            read = read_hello(&mut stream) => read,
        };
        let hello = match read {
            Ok(h) => h,
            Err(error) => {
                close_after_reply(stream, &HelloReply::Refused { error }).await;
                return;
            }
        };
        // (c) the project. A daemon whose own `.rdy` is gone or replaced (the project deleted,
        // moved away, restored from a copy) stops: it refuses every hello as stopping, after its
        // socket is removed, so the client's retry starts the project's own daemon. A client of
        // another project (a copy's stale record) never gets a session, and never makes this
        // daemon exit to be replaced.
        if self.check_project() {
            let error = another_project(
                &self.files.socket(&self.run),
                &self.root,
                self.info.pid,
                true,
            );
            close_after_reply(stream, &HelloReply::Refused { error }).await;
            return;
        }
        if hello.project != self.project {
            log(&format!(
                "refused a client of another project ({})",
                hello.project
            ));
            let error = another_project(
                &self.files.socket(&self.run),
                &self.root,
                self.info.pid,
                false,
            );
            close_after_reply(stream, &HelloReply::Refused { error }).await;
            return;
        }
        // (d) the version handshake
        if hello.version != self.version {
            if self.info.sessions.is_empty() {
                let reply = HelloReply::Exiting {
                    version: self.version.clone(),
                };
                // before the reply: the client spawns a successor as soon as it reads it, and the
                // successor's run files must not be removed by `run`'s cleanup afterwards
                self.files.remove(&self.run);
                close_after_reply(stream, &reply).await;
                log(&format!(
                    "a client of version {} arrived with no sessions; exiting to be replaced",
                    hello.version
                ));
                let _ = self.reason.set(ExitReason::VersionReplaced);
                self.info.stop.send_replace(true);
            } else {
                let error = ErrorReply::new(
                    E_DAEMON_VERSION,
                    format!(
                        "the running daemon is rdy {}, this client is rdy {}",
                        self.version, hello.version
                    ),
                    "close the other sessions or run rdy serve --stop",
                );
                close_after_reply(stream, &HelloReply::Refused { error }).await;
            }
            return;
        }
        // (d2) the build: the same version, so its shape and then its value
        if !valid_build(&hello.build) {
            close_after_reply(stream, &HelloReply::Refused { error: bad_build() }).await;
            return;
        }
        if self.check_build(&hello.build) != BuildCheck::Serve {
            let reply = HelloReply::Exiting {
                version: self.version.clone(),
            };
            close_after_reply(stream, &reply).await;
            return;
        }
        // (e) starting, then ready
        if !self.wait_ready(&mut stream).await {
            return;
        }
        let session = self.info.sessions.insert(
            hello.proto,
            hello.version.clone(),
            hello.build.clone(),
            hello.cwd.clone(),
            peer,
        );
        end.session = Some(session.clone());
        // the host's start of the session (a pack reload) before `ready`; a stop closes the
        // connection, and one `starting` reply goes out if it takes a while
        if !self.start_session(&mut stream, &session, &mut stop).await {
            return;
        }
        let ready = HelloReply::Ready {
            version: self.version.clone(),
            build: self.build.clone(),
            session: session.id,
        };
        if write_reply(&mut stream, &ready).await.is_err() {
            return;
        }
        // (f) the protocol
        match hello.proto {
            Proto::Ctl => {
                let (r, w) = sys::split(stream);
                let handler = Arc::clone(&self.handler);
                if let Err(e) = serve_ctl(r, w, session, handler, self.info.clone()).await
                    && !is_hang_up(&e)
                {
                    log(&format!("ctl session ended with an error: {e}"));
                }
            }
            Proto::Mcp => {
                let id = session.id;
                // the signing channel comes out of the connection here; the pump joins the
                // service's stream and the channel to the connection
                let crate::signing_transport::HostMcp {
                    stream,
                    agent,
                    pump,
                } = crate::signing_transport::host_mcp(stream);
                let mut pump = AbortOnDrop(tokio::spawn(pump));
                // spawned so a panic in the handler's future surfaces as a JoinError here
                let mut task = AbortOnDrop(tokio::spawn(self.handler.mcp(session, stream, agent)));
                // on a stop the session's future and the pump are dropped (their drops abort
                // them), which closes the stream under them
                tokio::select! {
                    biased;
                    _ = stop.changed() => {}
                    joined = &mut task.0 => {
                        if let Err(e) = joined
                            && e.is_panic()
                        {
                            log(&format!("mcp session {id} panicked"));
                        }
                        // the service's last bytes reach the connection before it closes
                        tokio::select! {
                            biased;
                            _ = stop.changed() => {}
                            _ = &mut pump.0 => {}
                        }
                    }
                }
            }
        }
        // (f) runs in `end`'s drop
    }

    /// Awaits `Handler::session_started`. Past `PROGRESS_MIN_INTERVAL` it writes one `starting`
    /// reply (`reloading the pack`) and keeps waiting. False on a stop or a write error.
    async fn start_session(
        &self,
        stream: &mut sys::Stream,
        session: &SessionInfo,
        stop: &mut watch::Receiver<bool>,
    ) -> bool {
        if *stop.borrow_and_update() {
            return false;
        }
        let mut started = self.handler.session_started(session);
        let progress_at = tokio::time::sleep(PROGRESS_MIN_INTERVAL);
        tokio::pin!(progress_at);
        let mut told = false;
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => return false,
                () = &mut started => return true,
                () = &mut progress_at, if !told => {
                    told = true;
                    let reply = HelloReply::Starting {
                        progress: "reloading the pack".into(),
                    };
                    if write_reply(stream, &reply).await.is_err() {
                        return false;
                    }
                }
            }
        }
    }

    /// Writes `starting` replies until ready (true), or returns false on a stop or a write
    /// error. Ready is sent by the caller; `starting` replies are at least
    /// `PROGRESS_MIN_INTERVAL` apart and carry the latest progress.
    async fn wait_ready(&self, stream: &mut sys::Stream) -> bool {
        let mut status = self.readiness.subscribe();
        let mut stop = self.info.stop.subscribe();
        if *stop.borrow_and_update() {
            return false;
        }
        loop {
            let progress = match &*status.borrow_and_update() {
                Status::Ready => return true,
                Status::Starting(p) => p.clone(),
            };
            if write_reply(stream, &HelloReply::Starting { progress })
                .await
                .is_err()
            {
                return false;
            }
            let earliest_next = tokio::time::Instant::now() + PROGRESS_MIN_INTERVAL;
            // wait for a change
            tokio::select! {
                biased;
                _ = stop.changed() => return false,
                changed = status.changed() => if changed.is_err() { return false },
            }
            // coalesce progress until the interval has passed; ready goes out at once
            loop {
                if matches!(*status.borrow(), Status::Ready) {
                    return true;
                }
                tokio::select! {
                    biased;
                    _ = stop.changed() => return false,
                    _ = tokio::time::sleep_until(earliest_next) => break,
                    changed = status.changed() => if changed.is_err() { return false },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Attempt, connect};
    use crate::ctl::CtlRequest;
    use crate::hello::{Hello, PROTO_VERSION};
    use rdy_engine::signing_crypto::SigningAgent;
    use serde_json::Value;
    use std::fs;

    /// Answers nothing that matters: these tests are about the daemon around it.
    struct Quiet;

    impl Handler for Quiet {
        fn ctl(
            &self,
            _: &SessionInfo,
            _: CtlRequest,
            _: Arc<dyn SigningAgent>,
        ) -> Result<Value, ErrorReply> {
            Ok(Value::Null)
        }
    }

    const VERSION: &str = "1.0.0";

    /// A directory with a `.rdy`, and its run dir.
    fn project() -> (tempfile::TempDir, RunDir) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join(".rdy")).unwrap();
        let run = RunDir::of_project(d.path());
        (d, run)
    }

    async fn bound(run: &RunDir) -> Daemon {
        let config = DaemonConfig::new(run.clone(), VERSION.to_string());
        let mut daemon = Daemon::new(config, Arc::new(Quiet));
        daemon.bind().await.unwrap();
        daemon
    }

    fn hello(project: ProjectId) -> Hello {
        Hello {
            rdy: PROTO_VERSION,
            version: VERSION.to_string(),
            build: crate::BUILD.to_string(),
            proto: Proto::Ctl,
            cwd: std::env::temp_dir(),
            project,
        }
    }

    /// D-A6ZZA on every endpoint: a client of another project is refused and changes nothing; once
    /// the project's `.rdy` is replaced, the daemon says it is stopping and goes, and its endpoint
    /// with it, so the new project's daemon can take the same name.
    #[tokio::test]
    async fn wx17_project_replacement_stops_the_old_daemon() {
        let (d, run) = project();
        let (_other_dir, other) = project();
        let daemon = bound(&run).await;
        let handle = daemon.handle();
        let endpoint = handle.socket();
        daemon.readiness().ready();
        let serving = tokio::spawn(daemon.run());

        let Attempt::Refused(e) = connect(&run, &hello(other.project_id().unwrap())).await else {
            panic!("a client of another project is refused");
        };
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(
            e.message,
            format!(
                "the daemon on {} serves another project ({}, process {})",
                endpoint.display(),
                rdy_store::pio::display_path(&run.root()),
                std::process::id()
            )
        );
        assert_eq!(e.extra.get("stopping"), None);
        assert_eq!(
            sys::endpoint_liveness(&endpoint).await,
            sys::Liveness::Live,
            "that client changes nothing"
        );

        fs::rename(d.path().join(".rdy"), d.path().join(".rdy-old")).unwrap();
        fs::create_dir(d.path().join(".rdy")).unwrap();
        let new = run.project_id().unwrap();
        match connect(&run, &hello(new)).await {
            Attempt::Refused(e) => {
                assert_eq!(
                    e.fix,
                    format!(
                        "that daemon is stopping; run the command again. If it keeps answering, end process {}",
                        std::process::id()
                    )
                );
                assert_eq!(e.extra.get("stopping"), Some(&Value::Bool(true)));
            }
            // a socket in the run dir went away with the old `.rdy`: the idle check finds it
            Attempt::NoDaemon(_) => handle.tick(),
            _ => panic!("no session on a replaced project"),
        }
        assert_eq!(serving.await.unwrap().unwrap(), ExitReason::ProjectGone);
        assert_eq!(sys::endpoint_liveness(&endpoint).await, sys::Liveness::Gone);
        assert_eq!(bound(&run).await.handle().socket(), run.socket());
    }

    /// A daemon's cleanup takes only what is still its own, whichever way it stops. What a successor
    /// wrote stays; a daemon with no successor leaves nothing.
    #[tokio::test]
    async fn wx17_old_cleanup_keeps_the_new_run_files() {
        let (_d, run) = project();
        let (_other_dir, other) = project();
        let daemon = bound(&run).await;
        let handle = daemon.handle();
        let endpoint = handle.socket();
        let own = run.project_id().unwrap().to_string();
        let record = fs::read_to_string(run.socket_record()).unwrap();
        let next_record = record.replace(&own, &other.project_id().unwrap().to_string());
        assert_ne!(record, next_record);
        crate::socket::write_run_file(&run.socket_record(), next_record.as_bytes()).unwrap();
        let pid = std::process::id();
        let next_pid = fs::read_to_string(run.pid_file()).unwrap().replacen(
            &pid.to_string(),
            &(pid + 1).to_string(),
            1,
        );
        crate::socket::write_run_file(&run.pid_file(), next_pid.as_bytes()).unwrap();
        // a socket can be replaced under its daemon; a pipe's name stays with its first instance
        let next_endpoint = sys::clear_endpoint(&endpoint)
            .and_then(|()| sys::bind_endpoint(&endpoint))
            .ok();

        handle.stop();

        assert_eq!(
            fs::read_to_string(run.socket_record()).unwrap(),
            next_record
        );
        assert_eq!(fs::read_to_string(run.pid_file()).unwrap(), next_pid);
        if next_endpoint.is_some() {
            assert!(
                fs::symlink_metadata(&endpoint).is_ok(),
                "the successor's socket"
            );
        }
        drop(next_endpoint);
        drop(daemon);
        fs::remove_file(run.socket_record()).unwrap();
        fs::remove_file(run.pid_file()).unwrap();
        // the other half of the rule: a daemon removes its own, on each of its ways out
        let _ = sys::remove_endpoint(&endpoint);

        let alone = bound(&run).await;
        let handle = alone.handle();
        handle.stop();
        assert!(!run.socket_record().exists());
        assert!(!run.pid_file().exists());
        drop(alone);
        assert_eq!(sys::endpoint_liveness(&endpoint).await, sys::Liveness::Gone);
    }

    /// Blocks each request in `ctl` until the test lets it go: a write in flight.
    struct Holding {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl Handler for Holding {
        fn ctl(
            &self,
            _: &SessionInfo,
            _: CtlRequest,
            _: Arc<dyn SigningAgent>,
        ) -> Result<Value, ErrorReply> {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            if let Some(release) = self.release.lock().unwrap().take() {
                let _ = release.blocking_recv();
            }
            Ok(Value::Null)
        }
    }

    /// p-3MJBQ on a project check (D-A6ZZA, D-6JZ5V): a client keeps waiting while `daemon.pid`
    /// names a live process, so a daemon that the check stops keeps that file until its writes in
    /// flight have drained. The socket record goes at once, so no new client reaches it.
    #[tokio::test]
    async fn s2_run_files_stay_until_in_flight_writes_drain() {
        let (d, run) = project();
        let store = d.path().join(".rdy").join(STORE_DIR);
        fs::create_dir(&store).unwrap();
        fs::write(store.join("lock"), b"").unwrap();
        let (entered, in_flight) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let handler = Arc::new(Holding {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(Some(released)),
        });
        let mut daemon = Daemon::new(DaemonConfig::new(run.clone(), VERSION.to_string()), handler);
        daemon.bind().await.unwrap();
        let handle = daemon.handle();
        daemon.readiness().ready();
        let serving = tokio::spawn(daemon.run());
        let project = run.project_id().unwrap();

        let Attempt::Ready(mut writer) = connect(&run, &hello(project)).await else {
            panic!("the writer gets a session");
        };
        let write = tokio::spawn(async move { writer.ctl("write", Value::Null).await });
        in_flight.await.unwrap();

        // the store moved under the live writer; the next hello's project check sees it
        fs::rename(&store, d.path().join("store-aside")).unwrap();
        let Attempt::Refused(e) = connect(&run, &hello(project)).await else {
            panic!("a hello after the move is refused");
        };
        assert_eq!(e.extra.get("stopping"), Some(&Value::Bool(true)));
        assert!(!run.socket_record().exists(), "no new client reaches it");
        assert_eq!(
            run.pid_named(),
            Some(std::process::id()),
            "daemon.pid stays while the write is in flight"
        );

        release.send(()).unwrap();
        assert_eq!(write.await.unwrap().unwrap(), Value::Null);
        assert_eq!(serving.await.unwrap().unwrap(), ExitReason::ProjectGone);
        assert_eq!(
            run.pid_named(),
            Some(std::process::id()),
            "daemon.pid stays until the host's last write has ended"
        );
        handle.drained();
        assert!(!run.pid_file().exists());
    }

    /// D-EDS38: a message shows a plain drive path, never the `\\?\` prefix of a canonical root.
    #[test]
    fn s2_another_project_message_shows_a_plain_drive_path() {
        let (root, shown) = if cfg!(windows) {
            (Path::new(r"\\?\C:\w\orig"), r"C:\w\orig")
        } else {
            (Path::new("/w/orig"), "/w/orig")
        };
        let socket = Path::new("/tmp/rdy-501/abc.sock");
        for stopping in [false, true] {
            let e = another_project(socket, root, 4242, stopping);
            assert_eq!(
                e.message,
                format!(
                    "the daemon on {} serves another project ({shown}, process 4242)",
                    socket.display()
                )
            );
        }
    }

    /// df3 fix round 2 (ruling task-3-round1): the refusal of a hello from another project. The
    /// final ruling names the process; a daemon whose own project is gone says it is stopping.
    #[test]
    fn df3_the_refusal_of_another_project_names_the_socket_and_the_root() {
        let e = another_project(
            Path::new("/tmp/rdy-501/abc.sock"),
            Path::new("/w/orig"),
            4242,
            false,
        );
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(
            e.message,
            "the daemon on /tmp/rdy-501/abc.sock serves another project (/w/orig, process 4242)"
        );
        assert_eq!(
            e.fix,
            "this project's daemon is not running; run rdy serve here, or remove a stale .rdy/run/daemon.sock copied from another project"
        );
        assert_eq!(e.extra.get("stopping"), None);
        let e = another_project(
            Path::new("/tmp/rdy-501/abc.sock"),
            Path::new("/w/orig"),
            4242,
            true,
        );
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(
            e.message,
            "the daemon on /tmp/rdy-501/abc.sock serves another project (/w/orig, process 4242)"
        );
        assert_eq!(
            e.fix,
            "that daemon is stopping; run the command again. If it keeps answering, end process 4242"
        );
        assert_eq!(
            e.extra.get("stopping"),
            Some(&serde_json::Value::Bool(true))
        );
    }
}
