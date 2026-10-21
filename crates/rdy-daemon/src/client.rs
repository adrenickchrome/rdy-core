//! The client side: connecting with a hello, `ctl` requests, and connect-or-start under
//! `start.lock`.

use crate::error::{E_DAEMON_GONE, E_DAEMON_START, E_DAEMON_VERSION, ErrorReply};
use crate::hello::{HELLO_TIMEOUT, Hello, HelloReply, PROTO_VERSION};
use crate::layout::{NotPrivateDir, RunDir, ShortDirError};
use crate::lock::{FileLock, open_no_follow};
use crate::session::{Proto, SessionId};
use crate::signing_transport::{CallerLink, CallerSigner, InterruptSource, read_reply, sigint};
use crate::socket::MAX_SOCKET_PATH;
use crate::spawn::Spawned;
use crate::sys;
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

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

/// `E_DAEMON_START` for a daemon that did not become ready within `start_timeout`: the
/// configured seconds, the log lines of this start (from `since`) and the fix.
pub fn not_ready(run: &RunDir, start_timeout: Duration, since: Option<u64>) -> ErrorReply {
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

/// The pid `daemon.pid` names when that process is alive: a daemon (starting, or draining its
/// store) that a held store lock may belong to.
fn live_daemon_pid(run: &RunDir) -> Option<u32> {
    fs::read_to_string(run.pid_file())
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .filter(|&pid| crate::spawn::process_alive(pid))
}

/// The project's store lock file, `.rdy/store/lock`, next to the run dir.
fn store_lock(run: &RunDir) -> Option<PathBuf> {
    run.path().parent().map(|p| p.join("store").join("lock"))
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
async fn read_hello_reply(stream: &mut sys::Stream) -> io::Result<HelloReply> {
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
    stream: BufReader<sys::Stream>,
    session: SessionId,
    version: String,
    next_id: u64,
    /// Set once the connection is closed or out of step; every later `ctl` is `E_DAEMON_GONE`.
    gone: bool,
    /// The `notice` of the last reply.
    notice: Option<String>,
    /// How this process answers the daemon's signing callbacks: with its own agent.
    link: CallerLink,
}

impl Client {
    fn new(stream: sys::Stream, session: SessionId, version: String) -> Client {
        Client {
            stream: BufReader::new(stream),
            session,
            version,
            next_id: 1,
            gone: false,
            notice: None,
            link: CallerLink::new(None, sigint()),
        }
    }

    /// This client with `agent`, this process's own SSH agent, as the signer the daemon asks
    /// for this connection's requests. Without one, each ask is `E_NO_AGENT`
    /// `SSH_AUTH_SOCK is not set`.
    pub fn with_agent(mut self, agent: Arc<dyn CallerSigner>) -> Client {
        self.link.set_agent(agent);
        self
    }

    /// This client with `interrupt` in place of `SIGINT` as what cancels an ask of its agent.
    pub fn with_interrupt(mut self, interrupt: InterruptSource) -> Client {
        self.link.set_interrupt(interrupt);
        self
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
    /// The daemon's signing callbacks before the reply are answered with this client's agent; a
    /// `SIGINT` (or the client's interrupt) while the agent is asked cancels that ask.
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
        let buf = match read_reply(&mut self.link, &mut self.stream).await {
            Ok(Some(buf)) => buf,
            // EOF before the newline counts as no reply
            Ok(None) | Err(_) => {
                self.gone = true;
                return Err(closed());
            }
        };
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
    pub fn into_stream(self) -> sys::Stream {
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
    stream: sys::Stream,
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

/// Connects to the project's daemon (the socket recorded in `daemon.sock`, else the run dir's
/// socket), sends `hello` and reads the first reply (within `HELLO_TIMEOUT`). A daemon that dies between accept and reply is no daemon; so is a socket
/// path that is a symlink, and a peer that is not of our own uid (neither hears the hello).
pub async fn connect(run: &RunDir, hello: &Hello) -> Attempt {
    connect_expecting_uid(run, hello, sys::own_uid()).await
}

/// Test hook: `connect`, taking the peer to be ours only when it runs as `uid`.
#[doc(hidden)]
pub async fn connect_expecting_uid(run: &RunDir, hello: &Hello, uid: u32) -> Attempt {
    match exchange_hello(run, hello, uid).await {
        Ok(found) => attempt(run, found.stream, found.reply),
        Err(e) => Attempt::NoDaemon(e),
    }
}

/// What one hello exchange found: the connection, the daemon's first reply, the socket path the
/// connection went to, and the pid of the process at its other end, when the system gives it.
struct Found {
    stream: sys::Stream,
    reply: HelloReply,
    socket: PathBuf,
    pid: Option<u32>,
}

/// The connect, the peer check, the hello and its first reply; any error is "no daemon". The
/// socket the daemon recorded (`daemon.sock`) comes first, so a client of any spelling of the
/// project reaches the one daemon. A record is followed only when it names this project's
/// `.rdy` (`RunDir::recorded_socket`): a copied project's record names the original. A record
/// that does not answer is stale; then the client's own socket (`RunDir::socket`) is tried. A
/// daemon that answers but serves another project refuses the hello, and that refusal is the
/// reply: it is never retried elsewhere (`connect_or_start` tries once more only when that
/// daemon says it is stopping).
async fn exchange_hello(run: &RunDir, hello: &Hello, uid: u32) -> io::Result<Found> {
    let own = run.socket();
    if let Some(recorded) = run.recorded_socket()
        && recorded != own
        && let Ok(found) = exchange_hello_at(&recorded, hello, uid).await
    {
        return Ok(found);
    }
    exchange_hello_at(&own, hello, uid).await
}

/// `exchange_hello` on the socket `path`.
async fn exchange_hello_at(path: &Path, hello: &Hello, uid: u32) -> io::Result<Found> {
    // a socket path that is a symlink may lead anywhere; our daemon binds a real socket there
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other(format!(
            "{} is a symbolic link, which rdy does not follow",
            path.display()
        )));
    }
    let mut stream = sys::connect(path).await?;
    // the mirror of the daemon's peer check: only a daemon of our own user hears the hello
    let peer = sys::peer_uid(&stream)?;
    if peer != uid {
        return Err(io::Error::other(format!(
            "the socket's peer is uid {peer}, not ours"
        )));
    }
    // read before the hello, from the connection that gets the reply
    let pid = sys::peer_pid(&stream);
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
        Ok(Ok(reply)) => Ok(Found {
            stream,
            reply,
            socket: path.to_path_buf(),
            pid,
        }),
    }
}

/// What a first hello reply means.
fn attempt(run: &RunDir, stream: sys::Stream, reply: HelloReply) -> Attempt {
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

/// What `request_stop` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopRequest {
    /// The daemon's process, when known.
    pub pid: Option<u32>,
    /// The socket to watch for the daemon's exit when `pid` is unknown ([`stop_socket`]), read
    /// before the stop.
    pub socket: PathBuf,
    /// Whether the stop went as a signal to the process (the fallback), not over the socket.
    pub signalled: bool,
}

/// The socket a stop watches when it has no pid: the one the daemon bound, which `daemon.sock`
/// names, else this client's own (`RunDir::socket`). A client of another spelling of the project
/// (a symlink, a macOS firmlink) computes another socket, which never exists. Read once, before
/// the stop: the daemon removes its record before its socket.
pub fn stop_socket(run: &RunDir) -> PathBuf {
    run.recorded_socket().unwrap_or_else(|| run.socket())
}

/// Refusal text of `request_stop` when it cannot tie the busy daemon's process to this project.
const UNTIED_MESSAGE: &str = "the running daemon is another rdy version and has other clients, and rdy cannot tie its process to this project";
const UNTIED_FIX: &str = "close the daemon's other sessions, then run rdy serve --stop again";

/// Asks the project's daemon to stop, never starting one. `Ok(None)` when no daemon answers.
///
/// A daemon of another version with other clients refuses every hello (`E_DAEMON_VERSION`,
/// D-ABRC8), so the stop cannot go over its socket, and daemons already released cannot change.
/// Then the client signals the daemon's process (`terminate`, `SIGTERM`, which `rdy serve` takes
/// as a stop), only when all of these hold: the socket record names this project's `.rdy`
/// (D-R7N9J); the refusal came on that recorded socket; the peer of that connection runs as our
/// uid (the peer check of the exchange) and the system gives its pid `p > 1`; and
/// `.rdy/run/daemon.pid` holds exactly `p`. Otherwise nothing is signalled.
pub async fn request_stop(
    run: &RunDir,
    hello: &Hello,
    start_timeout: Duration,
) -> Result<Option<StopRequest>, ErrorReply> {
    let Ok(found) = exchange_hello(run, hello, sys::own_uid()).await else {
        return Ok(None);
    };
    if let HelloReply::Refused { error } = &found.reply
        && error.code == E_DAEMON_VERSION
    {
        // `signal_stop` checks the four ties just before it signals. A narrow `kill(2)` race
        // remains, as with any pid-based signal: the daemon could exit after the pid file is
        // read and its pid be reused before the signal. Holding `found.stream` open does not
        // prevent this, because the daemon closes its end after the refusal.
        return signal_stop(run, &found).map(Some);
    }
    let client = match attempt(run, found.stream, found.reply) {
        Attempt::NoDaemon(_) => return Ok(None),
        Attempt::Ready(c) => Some(c),
        Attempt::Starting(p) => Some(p.wait(Instant::now() + start_timeout, &mut |_| {}).await?),
        // a daemon of another version: it is exiting already
        Attempt::Exiting => None,
        Attempt::Refused(e) => return Err(e),
    };
    let mut pid = None;
    // read while the daemon runs: it removes its record first when it stops
    let socket = stop_socket(run);
    if let Some(mut c) = client {
        pid = c
            .ctl("ping", json!({}))
            .await
            .ok()
            .and_then(|v| v["pid"].as_u64())
            .and_then(|p| u32::try_from(p).ok());
        c.ctl("stop", json!({})).await?;
        c.close();
    }
    Ok(Some(StopRequest {
        pid,
        socket,
        signalled: false,
    }))
}

/// The fallback of `request_stop`: signals the process of the daemon that refused `found`'s
/// hello, when the record, the connection and the pid file all tie it to this project.
fn signal_stop(run: &RunDir, found: &Found) -> Result<StopRequest, ErrorReply> {
    let untied = || ErrorReply::new(E_DAEMON_VERSION, UNTIED_MESSAGE, UNTIED_FIX);
    // 1. the record names this project's `.rdy`; 2. the refusal came on its socket
    let recorded = run.recorded_socket().ok_or_else(untied)?;
    if found.socket != recorded {
        return Err(untied());
    }
    // 3. the peer's pid, from the connection itself; never init or a process group
    let pid = found.pid.filter(|&p| p > 1).ok_or_else(untied)?;
    // 4. the pid file, opened without following a link, names the same process
    if read_pid_file(run) != Some(pid) {
        return Err(untied());
    }
    match crate::spawn::terminate(pid) {
        Ok(_) => Ok(StopRequest {
            pid: Some(pid),
            socket: recorded,
            signalled: true,
        }),
        Err(e) => Err(ErrorReply::new(
            E_DAEMON_VERSION,
            format!("could not send the stop signal to the daemon's process {pid}: {e}"),
            format!("end process {pid}, then run rdy serve --stop again"),
        )),
    }
}

/// The pid of a live daemon of the project at `project`, by the ties `request_stop` checks
/// before it signals (D-R7N9J): the socket record names this project's `.rdy`; a connection to
/// that recorded socket (never a link) has a peer of our uid whose pid the system gives,
/// `p > 1`; and `.rdy/run/daemon.pid`, opened without following a link, holds exactly `p`.
/// `None` when any tie fails. It sends nothing: the daemon reads an end of file where a hello
/// would be, and closes the connection.
///
/// `rdy load` asks it before it writes (final ruling 1): a daemon that still serves a store moved
/// aside would keep taking writes that the loaded project never sees.
pub fn live_daemon(project: &Path) -> Option<u32> {
    let run = RunDir::of_project(project);
    let recorded = run.recorded_socket()?;
    if fs::symlink_metadata(&recorded)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let pid = peer_pid(&recorded)?;
    (read_pid_file(&run) == Some(pid)).then_some(pid)
}

/// The pid of the process at the other end of a connection to `socket`, read as
/// `exchange_hello_at` reads it: the peer's uid with `sys::peer_uid` (it must be ours), then
/// the pid with `sys::peer_pid`, `p > 1`. The connection runs on a thread of its own with a
/// current-thread runtime: `live_daemon`'s caller may have no runtime (`rdy load`), or may be in
/// one (`rdy_load` on a blocking thread), where `block_on` would panic.
fn peer_pid(socket: &Path) -> Option<u32> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .ok()?;
            rt.block_on(async {
                let stream = sys::connect(socket).await.ok()?;
                if sys::peer_uid(&stream).ok()? != sys::own_uid() {
                    return None;
                }
                sys::peer_pid(&stream).filter(|&p| p > 1)
            })
        })
        .join()
        .ok()
        .flatten()
    })
}

/// The pid `.rdy/run/daemon.pid` holds: decimal digits only, surrounding whitespace trimmed.
/// `None` when it is missing, a link, too long or anything else.
fn read_pid_file(run: &RunDir) -> Option<u32> {
    let f = open_no_follow(fs::OpenOptions::new().read(true), &run.pid_file()).ok()?;
    let mut buf = Vec::new();
    f.take(64).read_to_end(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf).ok()?.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
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
/// private to this user, or that cannot be created or checked (D-YMWEM), has its own fix; any
/// other failure names the run dir.
fn ensure_error(run: &RunDir, e: io::Error) -> ErrorReply {
    let inner = e.get_ref();
    if let Some(dir) = inner.and_then(|i| i.downcast_ref::<NotPrivateDir>()) {
        return ErrorReply::new(
            E_DAEMON_START,
            format!("could not start the daemon: {dir}"),
            format!(
                "remove {}, or move the project to a shorter path",
                dir.0.display()
            ),
        );
    }
    if let Some(failed) = inner.and_then(|i| i.downcast_ref::<ShortDirError>()) {
        return ErrorReply::new(
            E_DAEMON_START,
            failed.to_string(),
            format!(
                "make {} a directory this user owns with mode 0700, or move the project to a path short enough that .rdy/run/rdy.sock fits (at most {MAX_SOCKET_PATH} bytes)",
                failed.dir.display()
            ),
        );
    }
    start_error(format!(
        "could not create the run directory {}: {e}",
        run.path().display()
    ))
}

/// The `E_DAEMON_START` for a spawned daemon that exited while the store is locked by daemon
/// `pid`, which answers on no socket this client tried: the recorded one, else its own.
fn locked_by_silent_daemon(run: &RunDir, pid: u32, since: u64) -> ErrorReply {
    let socket = run.recorded_socket().unwrap_or_else(|| run.socket());
    let mut e = ErrorReply::new(
        E_DAEMON_START,
        format!(
            "the store is locked by daemon {pid}, which does not answer on {}",
            socket.display()
        ),
        format!("stop it with rdy serve --stop from the project, or end process {pid}"),
    );
    e.extra.insert(
        "log_tail".into(),
        json!(log_tail_since(run, since, LOG_TAIL_LINES)),
    );
    e
}

/// Whether a refusal comes from a daemon that is stopping because its own project is gone or
/// replaced (`stopping: true`, final ruling): the client then tries once more.
fn stopping(e: &ErrorReply) -> bool {
    e.extra.get("stopping") == Some(&Value::Bool(true))
}

/// Connects to the project's daemon, starting one if none answers. At most one client spawns:
/// the spawn happens under `start.lock`, after a second connect made while holding it. A refusal
/// from a daemon that is stopping (its own project is gone or replaced: a project restored in
/// place) is retried once, within the same start timeout; that daemon removed its socket before
/// it answered, so the retry starts this project's own daemon.
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
    let progress: &mut (dyn FnMut(&str) + Send) = &mut *progress;
    let mut start = Start {
        version: &version,
        proto,
        cwd: &cwd,
        start_timeout,
        deadline,
        spawn: &mut spawn,
        progress,
    };
    match start.once(run).await {
        Err(e) if stopping(&e) => start.once(run).await,
        out => out,
    }
}

/// One `connect_or_start` try.
struct Start<'a> {
    version: &'a str,
    proto: Proto,
    cwd: &'a Path,
    start_timeout: Duration,
    deadline: Instant,
    spawn: &'a mut SpawnFn,
    progress: &'a mut (dyn FnMut(&str) + Send),
}

impl Start<'_> {
    async fn once(&mut self, run: &RunDir) -> Result<Client, ErrorReply> {
        let (version, proto, cwd, start_timeout, deadline) = (
            self.version,
            self.proto,
            self.cwd,
            self.start_timeout,
            self.deadline,
        );
        let spawn = &mut *self.spawn;
        let progress: &mut (dyn FnMut(&str) + Send) = &mut *self.progress;
        // the hello names this project's `.rdy` (`RunDir::project_id`)
        let hello_of = |project| Hello {
            rdy: PROTO_VERSION,
            version: version.to_string(),
            proto,
            cwd: cwd.to_path_buf(),
            project,
        };
        // whether a daemon told us it is exiting: it then still holds the store while it drains
        let mut saw_exiting = false;
        // 1. a daemon may be running; a project whose `.rdy` does not exist yet has none
        if let Ok(project) = run.project_id() {
            match connect(run, &hello_of(project)).await {
                Attempt::Ready(c) => return Ok(c),
                Attempt::Starting(p) => {
                    return finish(p, run, deadline, start_timeout, progress, None).await;
                }
                Attempt::Refused(e) => return Err(e),
                Attempt::Exiting => saw_exiting = true,
                Attempt::NoDaemon(_) => {}
            }
        }

        // the last guard: a daemon spawned for a socket path over `sun_path` would only die at its
        // bind. `run.socket()` is the short socket whenever `.rdy/run/rdy.sock` is too long
        // (D-YMWEM), so this passes for every project.
        crate::socket::check_socket_path(run)
            .map_err(|e| start_error(format!("could not start the daemon: {e}")))?;

        // 2. take start.lock; the guard unlocks on every return below. `ensure` also checks the
        // short socket's directory, before any spawn.
        run.ensure().map_err(|e| ensure_error(run, e))?;
        let hello = hello_of(run.project_id().map_err(|e| ensure_error(run, e))?);
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
        if let Some(lock) = store_lock(run) {
            let store = lock.parent().unwrap_or(&lock).to_path_buf();
            let grace = if saw_exiting {
                crate::serve::LOCKED_RETRY
            } else {
                HELD_GRACE
            };
            let give_up = (Instant::now() + grace).min(deadline);
            while store_lock_held(&lock) && live_daemon_pid(run).is_none() {
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
            let reached = match exchange_hello(run, &hello, sys::own_uid()).await {
                Ok(Found {
                    reply: HelloReply::Exiting { version: theirs },
                    ..
                }) => {
                    return Err(ErrorReply::new(
                        E_DAEMON_START,
                        format!(
                            "the daemon we started is rdy {theirs}, this client is rdy {}",
                            hello.version
                        ),
                        VERSION_FIX,
                    ));
                }
                Ok(found) => attempt(run, found.stream, found.reply),
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
            // a daemon that died (say, finding the store locked) will never answer: say so now. A
            // store held by a live daemon that answers on no socket we know is named as such.
            if let Some(code) = (spawned.exited)() {
                if let Some(pid) = live_daemon_pid(run)
                    && store_lock(run).is_some_and(|l| store_lock_held(&l))
                {
                    return Err(locked_by_silent_daemon(run, pid, log_from));
                }
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

        // fix round 1 (ruling 1): a short directory that cannot be prepared (a sandboxed /tmp,
        // simulated by a parent this user cannot write) names the directory and has its own fix
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let ro = d.path().join("read-only");
        fs::DirBuilder::new().mode(0o500).create(&ro).unwrap();
        let dir = ro.join("rdy-501");
        let e = ensure_error(&run, crate::layout::ensure_short_dir(&dir).unwrap_err());
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(
            e.message,
            format!(
                "could not prepare the run directory {} for the daemon socket: Permission denied (os error 13)",
                dir.display()
            )
        );
        assert_eq!(
            e.fix,
            format!(
                "make {} a directory this user owns with mode 0700, or move the project to a path short enough that .rdy/run/rdy.sock fits (at most 103 bytes)",
                dir.display()
            )
        );
    }
}
