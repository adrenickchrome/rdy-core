//! The client side: connecting with a hello, `ctl` requests, and connect-or-start under
//! `start.lock`.

use crate::error::{E_DAEMON_GONE, E_DAEMON_START, ErrorReply};
use crate::hello::{HELLO_TIMEOUT, Hello, HelloReply, PROTO_VERSION};
use crate::layout::{NotPrivateDir, RunDir};
use crate::lock::{FileLock, open_no_follow};
use crate::session::{Proto, SessionId};
use crate::spawn::Spawned;
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// How long `connect_or_start` waits for a daemon to become ready, by default.
pub const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `connect_or_start` waits for a store lock held by no daemon to be released before it
/// refuses to spawn one.
pub const HELD_GRACE: Duration = Duration::from_millis(250);
/// The pause between two looks at a store lock held by no daemon.
const HELD_POLL: Duration = Duration::from_millis(10);
/// The longest pause between two connects while a spawned daemon comes up.
const CONNECT_BACKOFF_MAX: Duration = Duration::from_millis(50);

/// The pause before connect try `attempt` (counting from 0) while a spawned daemon comes up:
/// 1 ms, doubling, at most 50 ms (1, 2, 4, 8, 16, 32, 50, 50, ...).
pub fn connect_backoff(attempt: u32) -> Duration {
    Duration::from_millis(1u64 << attempt.min(6)).min(CONNECT_BACKOFF_MAX)
}
/// The pause between two tries of `start.lock`.
pub const LOCK_RETRY: Duration = Duration::from_millis(50);
/// How many lines of `daemon.log` an `E_DAEMON_START` cites.
pub const LOG_TAIL_LINES: usize = 20;

/// The longest hello reply line read, newline included.
const MAX_HELLO_REPLY_BYTES: usize = 64 * 1024;
/// How much of the end of `daemon.log` `log_tail` reads.
const MAX_LOG_TAIL_BYTES: u64 = 64 * 1024;

const START_FIX: &str = "look at .rdy/run/daemon.log; run rdy serve in the foreground to see why";
const VERSION_FIX: &str =
    "the rdy that starts the daemon must be this client's version: check which rdy is on PATH";
const GONE_FIX: &str = "check rdy status; a write may or may not have committed; it is not retried";

fn start_error(message: String) -> ErrorReply {
    ErrorReply::new(E_DAEMON_START, message, START_FIX)
}

/// An `E_DAEMON_START` that cites the last lines of `daemon.log` in its message and in
/// `extra["log_tail"]`.
fn start_error_with_log(run: &RunDir, message: String) -> ErrorReply {
    start_error_citing(run, message, None)
}

/// `start_error_with_log`; with `since` (the log's length right before our own spawn) it cites
/// only the lines written after it.
fn start_error_citing(run: &RunDir, mut message: String, since: Option<u64>) -> ErrorReply {
    if let Some(from) = since {
        let tail = log_tail_since(run, from, LOG_TAIL_LINES);
        if tail.is_empty() {
            message.push_str("\n(the daemon wrote nothing to daemon.log)");
        } else {
            message.push_str("\nlast lines of .rdy/run/daemon.log:\n");
            message.push_str(&tail.join("\n"));
        }
        let mut e = start_error(message);
        e.extra.insert("log_tail".into(), json!(tail));
        return e;
    }
    let tail = log_tail(run, LOG_TAIL_LINES);
    // a symlinked log is never read, so it counts as missing
    if fs::symlink_metadata(run.log_file()).is_ok_and(|m| m.is_file()) {
        message.push_str("\nlast lines of .rdy/run/daemon.log:\n");
        message.push_str(&tail.join("\n"));
    } else {
        message.push_str("\n(no daemon.log)");
    }
    let mut e = start_error(message);
    e.extra.insert("log_tail".into(), json!(tail));
    e
}

fn not_ready(run: &RunDir, start_timeout: Duration, since: Option<u64>) -> ErrorReply {
    start_error_citing(
        run,
        format!(
            "the daemon did not become ready within {} s",
            start_timeout.as_secs()
        ),
        since,
    )
}

/// The last `n` lines of the run dir's `daemon.log` (from at most its last 64 KiB); empty when
/// the file is missing, unreadable or a symlink.
pub fn log_tail(run: &RunDir, n: usize) -> Vec<String> {
    let Ok(mut f) = open_no_follow(fs::OpenOptions::new().read(true), &run.log_file()) else {
        return vec![];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(MAX_LOG_TAIL_BYTES);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // cut mid-line
        lines.remove(0);
    }
    let from = lines.len().saturating_sub(n);
    lines[from..].iter().map(|l| l.to_string()).collect()
}

/// The lines of the run dir's `daemon.log` written since byte `from` (all of it when the file is
/// now shorter: it was replaced), at most the last `n`, from at most its last 64 KiB; empty when
/// the file is missing, unreadable or a symlink.
pub fn log_tail_since(run: &RunDir, from: u64, n: usize) -> Vec<String> {
    let Ok(mut f) = open_no_follow(fs::OpenOptions::new().read(true), &run.log_file()) else {
        return vec![];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = if len < from { 0 } else { from };
    let start = from.max(len.saturating_sub(MAX_LOG_TAIL_BYTES));
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > from && !lines.is_empty() {
        // cut mid-line
        lines.remove(0);
    }
    let first = lines.len().saturating_sub(n);
    lines[first..].iter().map(|l| l.to_string()).collect()
}

/// The length of the run dir's `daemon.log` now (0 when missing or not a regular file).
fn log_len(run: &RunDir) -> u64 {
    fs::symlink_metadata(run.log_file())
        .ok()
        .filter(|m| m.is_file())
        .map_or(0, |m| m.len())
}

/// Whether `daemon.pid` names a live process: a daemon (starting, or draining its store) that a
/// held store lock may belong to.
fn daemon_pid_alive(run: &RunDir) -> bool {
    fs::read_to_string(run.pid_file())
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .is_some_and(crate::spawn::process_alive)
}

/// Whether the store's lock file is held by a process right now. A missing lock file or any other
/// probe error counts as not held.
fn store_lock_held(lock: &std::path::Path) -> bool {
    let Ok(f) = fs::File::open(lock) else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(fs::TryLockError::WouldBlock) => true,
        Err(fs::TryLockError::Error(_)) => false,
    }
}

/// Reads one hello reply, one byte at a time up to and including its newline, so nothing past
/// it is consumed (an MCP stream is handed on as it stands).
async fn read_hello_reply(stream: &mut UnixStream) -> io::Result<HelloReply> {
    let mut line = Vec::with_capacity(128);
    loop {
        let b = stream.read_u8().await?;
        if b == b'\n' {
            break;
        }
        line.push(b);
        if line.len() >= MAX_HELLO_REPLY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a hello reply over {MAX_HELLO_REPLY_BYTES} bytes"),
            ));
        }
    }
    serde_json::from_slice(&line).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("an unreadable hello reply: {e}"),
        )
    })
}

/// A connection past its hello: a session of the daemon.
#[derive(Debug)]
pub struct Client {
    stream: BufReader<UnixStream>,
    session: SessionId,
    version: String,
    next_id: u64,
    /// Set once the connection is closed or out of step; every later `ctl` is `E_DAEMON_GONE`.
    gone: bool,
    /// The `notice` of the last reply.
    notice: Option<String>,
}

impl Client {
    fn new(stream: UnixStream, session: SessionId, version: String) -> Client {
        Client {
            stream: BufReader::new(stream),
            session,
            version,
            next_id: 1,
            gone: false,
            notice: None,
        }
    }

    /// The session id the daemon gave this connection.
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// The daemon's version, from its `ready` reply.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The `notice` of the last reply (a reloaded or invalid pack), taken: a second call is
    /// `None` until another reply carries one.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Sends one `ctl` request and reads its reply. Ids count from 1. A closed connection, an
    /// unreadable reply or a reply to another id is `E_DAEMON_GONE`, and so is every later call.
    pub async fn ctl(&mut self, op: &str, args: Value) -> Result<Value, ErrorReply> {
        let gone = |message: String| ErrorReply::new(E_DAEMON_GONE, message, GONE_FIX);
        let closed = || gone(format!("the daemon closed the connection during {op}"));
        self.notice = None;
        if self.gone {
            return Err(closed());
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({"id": id, "op": op, "args": args}).to_string();
        line.push('\n');
        let w = self.stream.get_mut();
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            self.gone = true;
            return Err(closed());
        }
        let mut buf = Vec::new();
        match self.stream.read_until(b'\n', &mut buf).await {
            // EOF before the newline counts as no reply
            Ok(_) if buf.last() == Some(&b'\n') => {}
            Ok(_) | Err(_) => {
                self.gone = true;
                return Err(closed());
            }
        }
        let mut reply = match serde_json::from_slice::<Value>(&buf) {
            Ok(Value::Object(m)) => m,
            Ok(_) | Err(_) => {
                self.gone = true;
                return Err(gone(format!("the daemon sent an unreadable reply to {op}")));
            }
        };
        self.notice = match reply.remove("notice") {
            Some(Value::String(n)) => Some(n),
            _ => None,
        };
        let reply_id = reply.remove("id").unwrap_or(Value::Null);
        let error = match reply.remove("error") {
            None | Some(Value::Null) => None,
            Some(e) => match serde_json::from_value::<ErrorReply>(e) {
                Ok(e) => Some(e),
                Err(_) => {
                    self.gone = true;
                    return Err(gone(format!("the daemon sent an unreadable reply to {op}")));
                }
            },
        };
        if reply_id != json!(id) {
            // a request the daemon could not read at all is answered with a null id
            if let (Value::Null, Some(e)) = (&reply_id, error) {
                return Err(e);
            }
            self.gone = true;
            return Err(gone(format!(
                "the daemon answered id {reply_id} to request {id}"
            )));
        }
        match (error, reply.remove("ok")) {
            (Some(e), _) => Err(e),
            (None, Some(v)) => Ok(v),
            (None, None) => {
                self.gone = true;
                Err(gone(format!("the daemon sent an unreadable reply to {op}")))
            }
        }
    }

    /// The connection, for a `Proto::Mcp` client to hand to its MCP transport. Call it before any
    /// `ctl` (as an MCP client does) or after `ctl` replies only (`ctl` never pipelines): then
    /// nothing past the last reply has been read off the stream. Bytes read past it would be lost
    /// with the buffer; debug builds assert there are none.
    pub fn into_stream(self) -> UnixStream {
        debug_assert!(
            self.stream.buffer().is_empty(),
            "into_stream would drop {} bytes read past the last ctl reply",
            self.stream.buffer().len()
        );
        self.stream.into_inner()
    }

    /// Closes the connection; the daemon ends the session.
    pub fn close(self) {}
}

/// A connection the daemon answered `starting`: it is not ready yet.
#[derive(Debug)]
pub struct Pending {
    stream: UnixStream,
    run: RunDir,
    progress: String,
}

/// How waiting on a `Pending` failed.
enum Waited {
    Deadline,
    Failed(ErrorReply),
}

impl Pending {
    /// Waits for `ready`, passing each progress text (the first one included) to `progress`.
    /// Past `deadline`, or when the daemon closes the connection or refuses it while starting,
    /// it is an error (`E_DAEMON_START` with the log tail, or the daemon's refusal).
    pub async fn wait(
        self,
        deadline: Instant,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Client, ErrorReply> {
        let run = self.run.clone();
        self.wait_until(deadline, progress)
            .await
            .map_err(|w| match w {
                Waited::Deadline => start_error_with_log(
                    &run,
                    "the daemon did not become ready before the deadline".into(),
                ),
                Waited::Failed(e) => e,
            })
    }

    /// `wait`, generic over the callback so a `Send` one keeps the future `Send`.
    async fn wait_until<P: FnMut(&str) + ?Sized>(
        mut self,
        deadline: Instant,
        progress: &mut P,
    ) -> Result<Client, Waited> {
        progress(&self.progress);
        let deadline = tokio::time::Instant::from_std(deadline);
        loop {
            let reply =
                match tokio::time::timeout_at(deadline, read_hello_reply(&mut self.stream)).await {
                    Err(_) => return Err(Waited::Deadline),
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        return Err(Waited::Failed(start_error_with_log(
                            &self.run,
                            format!("the daemon closed the connection while starting: {e}"),
                        )));
                    }
                };
            match reply {
                HelloReply::Starting { progress: p } => progress(&p),
                HelloReply::Ready { version, session } => {
                    return Ok(Client::new(self.stream, session, version));
                }
                HelloReply::Refused { error } => return Err(Waited::Failed(error)),
                HelloReply::Exiting { version } => {
                    return Err(Waited::Failed(start_error_with_log(
                        &self.run,
                        format!("the daemon (rdy {version}) exited while starting"),
                    )));
                }
            }
        }
    }
}

/// What one connect found.
#[derive(Debug)]
pub enum Attempt {
    /// A ready daemon gave us a session.
    Ready(Client),
    /// The daemon is starting; wait on it.
    Starting(Pending),
    /// A daemon of another version is exiting to be replaced.
    Exiting,
    /// No daemon answered: no socket, a stale one, or a daemon that died before its reply.
    NoDaemon(io::Error),
    /// The daemon refused the hello.
    Refused(ErrorReply),
}

/// Connects to the run dir's socket, sends `hello` and reads the first reply (within
/// `HELLO_TIMEOUT`). A daemon that dies between accept and reply is no daemon; so is a socket
/// path that is a symlink, and a peer that is not of our own uid (neither hears the hello).
pub async fn connect(run: &RunDir, hello: &Hello) -> Attempt {
    connect_expecting_uid(run, hello, crate::socket::own_uid()).await
}

/// Test hook: `connect`, taking the peer to be ours only when it runs as `uid`.
#[doc(hidden)]
pub async fn connect_expecting_uid(run: &RunDir, hello: &Hello, uid: u32) -> Attempt {
    match exchange_hello(run, hello, uid).await {
        Ok((stream, reply)) => attempt(run, stream, reply),
        Err(e) => Attempt::NoDaemon(e),
    }
}

/// The connect, the peer check, the hello and its first reply; any error is "no daemon".
async fn exchange_hello(
    run: &RunDir,
    hello: &Hello,
    uid: u32,
) -> io::Result<(UnixStream, HelloReply)> {
    let path = run.socket();
    // a socket path that is a symlink may lead anywhere; our daemon binds a real socket there
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other(format!(
            "{} is a symbolic link, which rdy does not follow",
            path.display()
        )));
    }
    let mut stream = UnixStream::connect(&path).await?;
    // the mirror of the daemon's peer check: only a daemon of our own user hears the hello
    let peer = crate::socket::peer_uid(&stream)?;
    if peer != uid {
        return Err(io::Error::other(format!(
            "the socket's peer is uid {peer}, not ours"
        )));
    }
    let mut line = serde_json::to_vec(hello).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    match tokio::time::timeout(HELLO_TIMEOUT, read_hello_reply(&mut stream)).await {
        Err(_) => Err(io::Error::other(format!(
            "no hello reply within {} s",
            HELLO_TIMEOUT.as_secs()
        ))),
        Ok(Err(e)) => Err(io::Error::other(format!(
            "no hello reply from the daemon: {e}"
        ))),
        Ok(Ok(reply)) => Ok((stream, reply)),
    }
}

/// What a first hello reply means.
fn attempt(run: &RunDir, stream: UnixStream, reply: HelloReply) -> Attempt {
    match reply {
        HelloReply::Ready { version, session } => {
            Attempt::Ready(Client::new(stream, session, version))
        }
        HelloReply::Starting { progress } => Attempt::Starting(Pending {
            stream,
            run: run.clone(),
            progress,
        }),
        HelloReply::Refused { error } => Attempt::Refused(error),
        HelloReply::Exiting { .. } => Attempt::Exiting,
    }
}

/// Starts a daemon for a run dir and returns at once, with a way to tell whether it has exited.
pub type SpawnFn = Box<dyn FnMut(&RunDir) -> io::Result<Spawned> + Send>;

/// What `connect_or_start` needs.
pub struct StartOptions {
    /// This client's rdy version, sent in the hello.
    pub version: String,
    pub proto: Proto,
    pub cwd: PathBuf,
    pub start_timeout: Duration,
    /// Starts a daemon for the run dir and returns at once (`spawn::spawn_serve` in rdy).
    pub spawn: SpawnFn,
    /// Called with each `starting` progress text.
    pub progress: Box<dyn FnMut(&str) + Send>,
}

impl StartOptions {
    /// `DEFAULT_START_TIMEOUT` and no progress callback.
    pub fn new(version: String, proto: Proto, cwd: PathBuf, spawn: SpawnFn) -> StartOptions {
        StartOptions {
            version,
            proto,
            cwd,
            start_timeout: DEFAULT_START_TIMEOUT,
            spawn,
            progress: Box::new(|_| {}),
        }
    }
}

/// Waits on a `Pending` for `connect_or_start`; the deadline error names the start timeout.
async fn finish(
    p: Pending,
    run: &RunDir,
    deadline: Instant,
    start_timeout: Duration,
    progress: &mut (dyn FnMut(&str) + Send),
    since: Option<u64>,
) -> Result<Client, ErrorReply> {
    p.wait_until(deadline, progress).await.map_err(|w| match w {
        Waited::Deadline => not_ready(run, start_timeout, since),
        Waited::Failed(e) => e,
    })
}

/// The `E_DAEMON_START` for a failed `RunDir::ensure`: a short socket directory that is not
/// private to this user (D-YMWEM) has its own fix; any other failure names the run dir.
fn ensure_error(run: &RunDir, e: io::Error) -> ErrorReply {
    match e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NotPrivateDir>())
    {
        Some(dir) => ErrorReply::new(
            E_DAEMON_START,
            format!("could not start the daemon: {dir}"),
            format!(
                "remove {}, or move the project to a shorter path",
                dir.0.display()
            ),
        ),
        None => start_error(format!(
            "could not create the run directory {}: {e}",
            run.path().display()
        )),
    }
}

/// Connects to the project's daemon, starting one if none answers. At most one client spawns:
/// the spawn happens under `start.lock`, after a second connect made while holding it.
pub async fn connect_or_start(run: &RunDir, opts: StartOptions) -> Result<Client, ErrorReply> {
    let StartOptions {
        version,
        proto,
        cwd,
        start_timeout,
        mut spawn,
        mut progress,
    } = opts;
    let deadline = Instant::now() + start_timeout;
    let hello = Hello {
        rdy: PROTO_VERSION,
        version,
        proto,
        cwd,
    };
    let progress: &mut (dyn FnMut(&str) + Send) = &mut *progress;
    // whether a daemon told us it is exiting: it then still holds the store while it drains
    let mut saw_exiting = false;
    // 1. a daemon may be running
    match connect(run, &hello).await {
        Attempt::Ready(c) => return Ok(c),
        Attempt::Starting(p) => {
            return finish(p, run, deadline, start_timeout, progress, None).await;
        }
        Attempt::Refused(e) => return Err(e),
        Attempt::Exiting => saw_exiting = true,
        Attempt::NoDaemon(_) => {}
    }

    // the last guard: a daemon spawned for a socket path over `sun_path` would only die at its
    // bind. `run.socket()` is the short socket whenever `.rdy/run/rdy.sock` is too long
    // (D-YMWEM), so this passes for every project.
    crate::socket::check_socket_path(run)
        .map_err(|e| start_error(format!("could not start the daemon: {e}")))?;

    // 2. take start.lock; the guard unlocks on every return below. `ensure` also checks the
    // short socket's directory, before any spawn.
    run.ensure().map_err(|e| ensure_error(run, e))?;
    let lock_path = run.start_lock();
    let _lock = loop {
        match FileLock::try_take(&lock_path) {
            Ok(Some(lock)) => break lock,
            Ok(None) => {}
            Err(e) => {
                return Err(start_error(format!(
                    "could not lock {}: {e}",
                    lock_path.display()
                )));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(start_error(format!(
                "another client has been starting the daemon for {} s",
                start_timeout.as_secs()
            )));
        }
        tokio::time::sleep(LOCK_RETRY.min(deadline - now)).await;
    };

    // 3. another client may have started one while we waited for the lock
    match connect(run, &hello).await {
        Attempt::Ready(c) => return Ok(c),
        Attempt::Starting(p) => {
            return finish(p, run, deadline, start_timeout, progress, None).await;
        }
        Attempt::Refused(e) => return Err(e),
        Attempt::Exiting => saw_exiting = true,
        Attempt::NoDaemon(_) => {}
    }

    // a store held by a process that is not a daemon (a --no-daemon rdy) would only make the
    // daemon we spawn die at its open: refuse that here, without a spawn, once it has had a
    // moment to let go. A daemon that is draining holds the store too (it removes its socket and
    // pid file first), and the successor's own open retries for LOCKED_RETRY to outlast it: so
    // wait that long when a daemon just told us it is exiting, and do not refuse at all while
    // the pid file names a live process.
    if let Some(store) = run.path().parent().map(|p| p.join("store")) {
        let lock = store.join("lock");
        let grace = if saw_exiting {
            crate::serve::LOCKED_RETRY
        } else {
            HELD_GRACE
        };
        let give_up = (Instant::now() + grace).min(deadline);
        while store_lock_held(&lock) && !daemon_pid_alive(run) {
            match connect(run, &hello).await {
                Attempt::Ready(c) => return Ok(c),
                Attempt::Starting(p) => {
                    return finish(p, run, deadline, start_timeout, progress, None).await;
                }
                Attempt::Refused(e) => return Err(e),
                Attempt::Exiting | Attempt::NoDaemon(_) => {}
            }
            let now = Instant::now();
            if now >= give_up {
                return Err(ErrorReply::new(
                    E_DAEMON_START,
                    format!(
                        "the store at {} is held by another rdy process that is not a daemon (a command run with --no-daemon, or rdy mcp --no-daemon)",
                        store.display()
                    ),
                    "a --no-daemon rdy holds the store: close it, or drop --no-daemon",
                ));
            }
            tokio::time::sleep(HELD_POLL).await;
        }
    }

    // 4. spawn
    let log_from = log_len(run);
    let mut spawned = match spawn(run) {
        Ok(s) => s,
        Err(e) => return Err(start_error(format!("could not start the daemon: {e}"))),
    };

    // 5. wait for it to answer. An old daemon removes its socket before it answers `exiting`,
    // so an `exiting` now comes from the daemon we spawned: it is not of our version, and
    // waiting cannot help. Neither can waiting on a daemon that has exited.
    let mut attempt_no = 0u32;
    loop {
        let reached = match exchange_hello(run, &hello, crate::socket::own_uid()).await {
            Ok((_, HelloReply::Exiting { version: theirs })) => {
                return Err(ErrorReply::new(
                    E_DAEMON_START,
                    format!(
                        "the daemon we started is rdy {theirs}, this client is rdy {}",
                        hello.version
                    ),
                    VERSION_FIX,
                ));
            }
            Ok((stream, reply)) => attempt(run, stream, reply),
            Err(e) => Attempt::NoDaemon(e),
        };
        match reached {
            Attempt::Ready(c) => return Ok(c),
            Attempt::Starting(p) => {
                return finish(p, run, deadline, start_timeout, progress, Some(log_from)).await;
            }
            Attempt::Refused(e) => return Err(e),
            Attempt::Exiting | Attempt::NoDaemon(_) => {}
        }
        // a daemon that died (say, finding the store locked) will never answer: say so now
        if let Some(code) = (spawned.exited)() {
            return Err(start_error_citing(
                run,
                format!("the daemon exited with code {code} before it was ready"),
                Some(log_from),
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(not_ready(run, start_timeout, Some(log_from)));
        }
        tokio::time::sleep(connect_backoff(attempt_no).min(deadline - now)).await;
        attempt_no += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn df3_a_refused_short_socket_dir_is_e_daemon_start_with_its_own_fix() {
        let run = RunDir::of_project(Path::new("/p"));
        let dir = PathBuf::from("/tmp/rdy-501");
        let e = ensure_error(&run, io::Error::other(NotPrivateDir(dir)));
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(
            e.message,
            "could not start the daemon: /tmp/rdy-501 is not a private directory of this user, so rdy cannot put the socket there"
        );
        assert_eq!(
            e.fix,
            "remove /tmp/rdy-501, or move the project to a shorter path"
        );
        let other = ensure_error(&run, io::Error::other("no space"));
        assert_eq!(
            other.message,
            "could not create the run directory /p/.rdy/run: no space"
        );
        assert_eq!(other.fix, START_FIX);
    }
}
