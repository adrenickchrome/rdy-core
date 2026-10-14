//! The daemon: the accept loop, the per-connection hello and handshake, and stop.

use crate::ctl::{DaemonInfo, serve_ctl};
use crate::error::{E_DAEMON_START, E_DAEMON_VERSION, ErrorReply};
use crate::handler::Handler;
use crate::hello::{HelloReply, read_hello};
use crate::layout::{ProjectId, RunDir};
use crate::session::{Proto, SessionInfo, Sessions};
use crate::socket;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
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
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = writeln!(io::stderr().lock(), "{ms} rdy serve: {text}");
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
    /// `None` never idles out (a foreground `rdy serve`).
    pub idle_timeout: Option<Duration>,
    /// Test hook: the peer uid to accept instead of our own.
    #[doc(hidden)]
    pub expected_uid: Option<u32>,
}

impl DaemonConfig {
    pub fn new(run: RunDir, version: String) -> Self {
        DaemonConfig {
            run,
            version,
            idle_timeout: None,
            expected_uid: None,
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
    /// The project's `.rdy` is gone or replaced (deleted, moved away, restored from a copy): the
    /// daemon serves nothing there and exits without writing to its store.
    ProjectGone,
}

impl ExitReason {
    fn label(self) -> &'static str {
        match self {
            ExitReason::Stopped => "stopped",
            ExitReason::Idle => "idle",
            ExitReason::VersionReplaced => "version replaced",
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

    /// The project check (D-A6ZZA) that a hello runs: when the project's `.rdy` is gone or
    /// replaced, removes this daemon's run files that still name it and stops the daemon with
    /// `ProjectGone`. True when the project is gone. False before the bind.
    pub fn check_project(&self) -> bool {
        self.check.check()
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
    listener: Option<UnixListener>,
    /// The project check; it holds the identity of the project's `.rdy` from the bind on, and
    /// hellos from another project are refused.
    check: Arc<ProjectCheck>,
    /// `DaemonHandle::tick`: an idle tick now.
    tick: Arc<Notify>,
}

impl Daemon {
    pub fn new(config: DaemonConfig, handler: Arc<dyn Handler>) -> Daemon {
        let info = DaemonInfo {
            pid: std::process::id(),
            version: config.version.clone(),
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
        };
        let check = Arc::new(ProjectCheck {
            root: config.run.root(),
            project: OnceLock::new(),
            reason: Arc::clone(&reason),
            run: config.run.clone(),
            files: files.clone(),
            pid: info.pid,
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
        self.files.bound(path);
        let _ = self.check.project.set(project);
        self.listener = Some(listener);
        Ok(())
    }

    /// Serves connections until stopped, idle for `idle_timeout`, or replaced by another
    /// version. Then, in order: stops accepting and drops the listener, removes the socket and
    /// the pid file (unless `DaemonHandle::stop` already did), tells every connection to finish,
    /// and waits for them up to `STOP_GRACE`.
    pub async fn run(mut self) -> io::Result<ExitReason> {
        let (listener, project) = self
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
            expected_uid: self.config.expected_uid.unwrap_or_else(socket::own_uid),
            handler: Arc::clone(&self.handler),
            info: self.info.clone(),
            readiness: self.readiness.clone(),
            reason: Arc::clone(&self.reason),
            run: self.config.run.clone(),
            files: self.files.clone(),
            project,
            root: self.config.run.root(),
            check: Arc::clone(&self.check),
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
                    Ok((stream, _)) => {
                        self.keepers.update(|k| {
                            k.connections.fetch_add(1, Ordering::SeqCst);
                        });
                        idle_at = None;
                        let end = ConnEnd {
                            sessions: self.info.sessions.clone(),
                            keepers: self.keepers.clone(),
                            handler: Arc::clone(&self.handler),
                            session: None,
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
        // (2) the socket and the pid file, unless `DaemonHandle::stop` already removed them
        self.files.remove(&self.config.run);
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

/// The removal of this daemon's socket record, socket and pid file, done exactly once by
/// whichever stop path gets there first. Once removed, the paths may belong to a successor
/// daemon, so a second removal would delete its files. The lock is held across the removal, so
/// every caller returns only once the files are gone. The socket is the one `bind` bound
/// (`RunDir::socket` before a bind), so a project renamed or deleted while the daemon runs still
/// has its socket removed.
#[derive(Clone, Default)]
struct RunFiles(Arc<Mutex<Files>>);

#[derive(Default)]
struct Files {
    removed: bool,
    socket: Option<PathBuf>,
    /// The bound socket's file identity (`dev`, `ino`), from `lstat` right after the bind.
    socket_id: Option<(u64, u64)>,
}

impl RunFiles {
    fn lock(&self) -> std::sync::MutexGuard<'_, Files> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bound(&self, socket: PathBuf) {
        use std::os::unix::fs::MetadataExt;
        let mut files = self.lock();
        files.socket_id = std::fs::symlink_metadata(&socket)
            .ok()
            .map(|m| (m.dev(), m.ino()));
        files.socket = Some(socket);
    }

    /// The removal for a daemon whose project is gone (final ruling), once like `remove`: the
    /// paths may now belong to the project that replaced it. The record goes only while it still
    /// names this daemon's socket and `project`; the pid file only while it names `pid`; the
    /// socket only while it is the file this daemon bound.
    fn remove_if_ours(&self, run: &RunDir, project: ProjectId, pid: u32) {
        use std::os::unix::fs::MetadataExt;
        let mut files = self.lock();
        if files.removed {
            return;
        }
        files.removed = true;
        let socket = files.socket.clone().unwrap_or_else(|| run.socket());
        if run.record_names(&socket, project) {
            remove_if_present(&run.socket_record());
        }
        let bound = std::fs::symlink_metadata(&socket)
            .ok()
            .map(|m| (m.dev(), m.ino()));
        if bound.is_some() && bound == files.socket_id {
            remove_if_present(&socket);
        }
        let names_us = std::fs::read_to_string(run.pid_file())
            .is_ok_and(|t| t.trim().parse::<u32>().ok() == Some(pid));
        if names_us {
            remove_if_present(&run.pid_file());
        }
    }

    fn socket(&self, run: &RunDir) -> PathBuf {
        self.lock().socket.clone().unwrap_or_else(|| run.socket())
    }

    fn remove(&self, run: &RunDir) {
        let mut files = self.lock();
        if !files.removed {
            files.removed = true;
            // the record first: no client follows it to a socket that is going away
            remove_if_present(&run.socket_record());
            let socket = files.socket.clone().unwrap_or_else(|| run.socket());
            remove_if_present(&socket);
            remove_if_present(&run.pid_file());
        }
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
    expected_uid: u32,
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
}

/// The project check (final ruling, D-A6ZZA), shared by every socket connection, the idle tick
/// and every `DaemonHandle` (the site runs it at each accepted HTTP connection, ls6).
struct ProjectCheck {
    /// The canonical project root.
    root: PathBuf,
    /// The identity of the project's `.rdy` at the bind; unset before it.
    project: OnceLock<ProjectId>,
    reason: Arc<OnceLock<ExitReason>>,
    run: RunDir,
    files: RunFiles,
    pid: u32,
    stop: Arc<watch::Sender<bool>>,
}

impl ProjectCheck {
    /// Whether `<root>/.rdy` (from `lstat`) is no longer `project`, the `.rdy` this daemon bound
    /// with: gone, or another directory (a restore, a copy moved into place).
    fn gone(&self, project: ProjectId) -> bool {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(self.root.join(".rdy")) {
            Ok(m) => {
                ProjectId {
                    dev: m.dev(),
                    ino: m.ino(),
                } != project
            }
            Err(_) => true,
        }
    }

    /// When the project is gone, removes this daemon's run files that still name it
    /// (`RunFiles::remove_if_ours`), then stops with `ProjectGone`. True when the project is
    /// gone; false before the bind.
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
        self.files.remove_if_ours(&self.run, project, self.pid);
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
    let message = format!(
        "the daemon on {} serves another project ({}, process {pid})",
        socket.display(),
        root.display()
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
        // keeps the holds
        self.keepers.update(|k| {
            k.connections.fetch_sub(1, Ordering::SeqCst);
        });
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
async fn close_after_reply(mut stream: UnixStream, reply: &HelloReply) {
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

    async fn serve(self: Arc<Self>, mut stream: UnixStream, mut end: ConnEnd) {
        // (a) the peer check
        let peer_uid = match socket::peer_uid(&stream) {
            Ok(uid) => uid,
            Err(e) => {
                log(&format!("could not read a connection's peer uid: {e}"));
                return;
            }
        };
        if peer_uid != self.expected_uid {
            log(&format!("refused a connection from uid {peer_uid}"));
            return;
        }
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
        // (e) starting, then ready
        if !self.wait_ready(&mut stream).await {
            return;
        }
        let session = self.info.sessions.insert(
            hello.proto,
            hello.version.clone(),
            hello.cwd.clone(),
            peer_uid,
        );
        end.session = Some(session.clone());
        // the host's start of the session (a pack reload) before `ready`; a stop closes the
        // connection, and one `starting` reply goes out if it takes a while
        if !self.start_session(&mut stream, &session, &mut stop).await {
            return;
        }
        let ready = HelloReply::Ready {
            version: self.version.clone(),
            session: session.id,
        };
        if write_reply(&mut stream, &ready).await.is_err() {
            return;
        }
        // (f) the protocol
        match hello.proto {
            Proto::Ctl => {
                let (r, w) = stream.split();
                let handler = Arc::clone(&self.handler);
                if let Err(e) = serve_ctl(r, w, session, handler, self.info.clone()).await
                    && !is_hang_up(&e)
                {
                    log(&format!("ctl session ended with an error: {e}"));
                }
            }
            Proto::Mcp => {
                let id = session.id;
                // spawned so a panic in the handler's future surfaces as a JoinError here
                let mut task = AbortOnDrop(tokio::spawn(self.handler.mcp(session, stream)));
                // on a stop the session's future is dropped (`task`'s drop aborts it), which
                // closes the stream under it
                tokio::select! {
                    biased;
                    _ = stop.changed() => {}
                    joined = &mut task.0 => {
                        if let Err(e) = joined
                            && e.is_panic()
                        {
                            log(&format!("mcp session {id} panicked"));
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
        stream: &mut UnixStream,
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
    async fn wait_ready(&self, stream: &mut UnixStream) -> bool {
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
