//! The daemon: the accept loop, the per-connection hello and handshake, and stop.

use crate::ctl::{DaemonInfo, serve_ctl};
use crate::error::{E_DAEMON_VERSION, ErrorReply};
use crate::handler::Handler;
use crate::hello::{HelloReply, read_hello};
use crate::layout::RunDir;
use crate::session::{Proto, SessionInfo, Sessions};
use crate::socket;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

/// How long a detached daemon with no connections waits before exiting.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// How long `run` waits for connection tasks after a stop.
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// The least time between two `starting` replies on one connection.
pub const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(200);

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
}

impl ExitReason {
    fn label(self) -> &'static str {
        match self {
            ExitReason::Stopped => "stopped",
            ExitReason::Idle => "idle",
            ExitReason::VersionReplaced => "version replaced",
        }
    }
}

/// Controls a daemon from outside its `run`. Cheap to clone.
#[derive(Clone)]
pub struct DaemonHandle {
    info: DaemonInfo,
    run: RunDir,
    files: RunFiles,
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

    pub fn socket(&self) -> PathBuf {
        self.run.socket()
    }
}

pub struct Daemon {
    config: DaemonConfig,
    handler: Arc<dyn Handler>,
    info: DaemonInfo,
    readiness: Readiness,
    /// Set by the path that stops the daemon for a reason other than `Stopped`.
    reason: Arc<OnceLock<ExitReason>>,
    /// Mirrors `info.connections` so `run` can watch it reach 0 (the idle timer).
    count: Arc<watch::Sender<usize>>,
    files: RunFiles,
    listener: Option<UnixListener>,
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
        Daemon {
            config,
            handler,
            info,
            readiness: Readiness::new(),
            reason: Arc::new(OnceLock::new()),
            count: Arc::new(watch::channel(0).0),
            files: RunFiles::default(),
            listener: None,
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
        }
    }

    /// Binds the socket and writes the pid file (`socket::bind`). Call only while holding the
    /// store lock.
    pub async fn bind(&mut self) -> io::Result<()> {
        self.listener = Some(socket::bind(&self.config.run).await?);
        Ok(())
    }

    /// Serves connections until stopped, idle for `idle_timeout`, or replaced by another
    /// version. Then, in order: stops accepting and drops the listener, removes the socket and
    /// the pid file (unless `DaemonHandle::stop` already did), tells every connection to finish,
    /// and waits for them up to `STOP_GRACE`.
    pub async fn run(mut self) -> io::Result<ExitReason> {
        let listener = self
            .listener
            .take()
            .ok_or_else(|| io::Error::other("run before bind"))?;
        log(&format!(
            "listening on {}",
            self.config.run.socket().display()
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
        });
        let idle = self.config.idle_timeout;
        let mut stop = self.info.stop.subscribe();
        let mut count = self.count.subscribe();
        // armed at the start (no connections yet) and on each drop of the count to 0
        let mut idle_at = idle.map(|t| tokio::time::Instant::now() + t);
        let mut tasks = JoinSet::new();
        loop {
            if *stop.borrow_and_update() {
                break;
            }
            while tasks.try_join_next().is_some() {}
            tokio::select! {
                _ = stop.changed() => {}
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
                        let connections = &self.info.connections;
                        self.count.send_modify(|n| {
                            *n = connections.fetch_add(1, Ordering::SeqCst) + 1;
                        });
                        idle_at = None;
                        let end = ConnEnd {
                            sessions: self.info.sessions.clone(),
                            connections: Arc::clone(&self.info.connections),
                            count: Arc::clone(&self.count),
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

/// The removal of this daemon's socket and pid file, done exactly once by whichever stop path
/// gets there first. Once removed, the paths may belong to a successor daemon, so a second
/// removal would delete its files. The lock is held across the removal, so every caller returns
/// only once the files are gone.
#[derive(Clone, Default)]
struct RunFiles(Arc<Mutex<bool>>);

impl RunFiles {
    fn remove(&self, run: &RunDir) {
        let mut removed = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !*removed {
            *removed = true;
            remove_if_present(&run.socket());
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
}

/// The end of a connection, run on every way out of its task (return, panic, abort): removes
/// the session if one was opened, calls `session_ended` once, and uncounts the connection.
struct ConnEnd {
    sessions: Sessions,
    connections: Arc<AtomicUsize>,
    count: Arc<watch::Sender<usize>>,
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
        // under the watch's lock, so concurrent ends publish their counts in order
        let connections = &self.connections;
        self.count.send_modify(|n| {
            *n = connections.fetch_sub(1, Ordering::SeqCst) - 1;
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
        // (c) the version handshake
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
        // (d) starting, then ready
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
        // (e) the protocol
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
