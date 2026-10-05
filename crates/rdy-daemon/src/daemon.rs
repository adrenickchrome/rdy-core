//! The daemon: the accept loop, the per-connection hello and handshake, and stop.

use crate::ctl::{DaemonInfo, serve_ctl};
use crate::error::{E_DAEMON_VERSION, ErrorReply};
use crate::handler::Handler;
use crate::hello::{HelloReply, read_hello};
use crate::layout::RunDir;
use crate::session::{Proto, SessionInfo, Sessions};
use crate::socket;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
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

/// Read past a refusal at most this much before closing (see `close_after_reply`).
const DRAIN_AFTER_REPLY_BYTES: usize = 64 * 1024;

/// One daemon log line on stderr: `<unix ms> rdy serve: <text>`.
pub(crate) fn log(text: &str) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    eprintln!("{ms} rdy serve: {text}");
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
    fn new() -> Self {
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

/// Controls a daemon from outside its `run`. Cheap to clone.
#[derive(Clone)]
pub struct DaemonHandle {
    info: DaemonInfo,
    socket: PathBuf,
}

impl DaemonHandle {
    /// Asks `run` to stop; returns at once.
    pub fn stop(&self) {
        self.info.stop.send_replace(true);
    }

    pub fn sessions(&self) -> Sessions {
        self.info.sessions.clone()
    }

    /// Open connections, from accept to close, whether or not past their hello.
    pub fn connections(&self) -> usize {
        self.info.connections.load(Ordering::SeqCst)
    }

    pub fn socket(&self) -> PathBuf {
        self.socket.clone()
    }
}

pub struct Daemon {
    config: DaemonConfig,
    handler: Arc<dyn Handler>,
    info: DaemonInfo,
    readiness: Readiness,
    /// Set by the path that stops the daemon for a reason other than `Stopped`.
    reason: Arc<OnceLock<ExitReason>>,
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
            listener: None,
        }
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle {
            info: self.info.clone(),
            socket: self.config.run.socket(),
        }
    }

    /// Binds the socket and writes the pid file (`socket::bind`). Call only while holding the
    /// store lock.
    pub async fn bind(&mut self) -> io::Result<()> {
        self.listener = Some(socket::bind(&self.config.run).await?);
        Ok(())
    }

    /// Serves connections until stopped, then removes the socket and the pid file.
    pub async fn run(mut self) -> io::Result<ExitReason> {
        let listener = self
            .listener
            .take()
            .ok_or_else(|| io::Error::other("run before bind"))?;
        let socket_path = self.config.run.socket();
        log(&format!("listening on {}", socket_path.display()));
        let ctx = Arc::new(Conn {
            version: self.config.version.clone(),
            expected_uid: self.config.expected_uid.unwrap_or_else(socket::own_uid),
            handler: Arc::clone(&self.handler),
            info: self.info.clone(),
            readiness: self.readiness.clone(),
            reason: Arc::clone(&self.reason),
        });
        let mut stop = self.info.stop.subscribe();
        let mut tasks = JoinSet::new();
        loop {
            if *stop.borrow_and_update() {
                break;
            }
            while tasks.try_join_next().is_some() {}
            tokio::select! {
                _ = stop.changed() => {}
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        self.info.connections.fetch_add(1, Ordering::SeqCst);
                        let end = ConnEnd {
                            sessions: self.info.sessions.clone(),
                            connections: Arc::clone(&self.info.connections),
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
        drop(listener);
        remove_if_present(&socket_path);
        remove_if_present(&self.config.run.pid_file());
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
        // dropping the set aborts what is left; each task's `ConnEnd` still runs
        drop(tasks);
        Ok(self.reason.get().copied().unwrap_or(ExitReason::Stopped))
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
}

/// The end of a connection, run on every way out of its task (return, panic, abort): removes
/// the session if one was opened, calls `session_ended` once, and uncounts the connection.
struct ConnEnd {
    sessions: Sessions,
    connections: Arc<AtomicUsize>,
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
        self.connections.fetch_sub(1, Ordering::SeqCst);
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
        // (b) the hello
        let hello = match read_hello(&mut stream).await {
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
                if let Err(e) = serve_ctl(r, w, session, handler, self.info.clone()).await {
                    log(&format!("ctl session ended with an error: {e}"));
                }
            }
            Proto::Mcp => {
                let id = session.id;
                // spawned so a panic in the handler's future surfaces as a JoinError here
                let mut task = AbortOnDrop(tokio::spawn(self.handler.mcp(session, stream)));
                if let Err(e) = (&mut task.0).await
                    && e.is_panic()
                {
                    log(&format!("mcp session {id} panicked"));
                }
            }
        }
        // (f) runs in `end`'s drop
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
