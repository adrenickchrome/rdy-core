//! An MCP stdio client for tests. The server is untrusted: every request has a deadline, and a
//! missing or late reply is a Critical failure (the server is killed and the test panics).
//!
//! A bounded session ([`McpClient::try_launch_bounded`] and the `try_` calls) is the fallible
//! form for a release check: it never panics, caps everything the server writes and returns a
//! [`McpFailure`].
use crate::daemon::Served;
use crate::fixtures::rdy_bin;
use crate::platform;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a request waits for its reply before the server counts as hung: a deadline, not a
/// speed claim (the perf tier measures speed), so it is generous for a loaded machine.
pub const DEADLINE: Duration = Duration::from_secs(30);
/// The `protocolVersion` the client sends in its `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone)]
pub enum Reply {
    Ok(Value),
    ToolError(Value),
    RpcError(Value),
}

impl Reply {
    /// True for a result only: a tool error and a protocol (JSON-RPC) error are both failures.
    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok(_))
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            Reply::ToolError(v) => v["code"].as_str(),
            _ => None,
        }
    }
    pub fn ok(self) -> Value {
        match self {
            Reply::Ok(v) => v,
            other => panic!("expected a result, got {other:?}"),
        }
    }
}

/// Everything a server wrote, from [`McpClient::finish`].
#[derive(Debug, Clone)]
pub struct Transcript {
    /// Its exit code (`None` when killed by a signal).
    pub code: Option<i32>,
    /// Every stdout line, in arrival order, each ended by `\n`.
    pub stdout: String,
    /// Its stderr (empty when it was not piped).
    pub stderr: String,
}

/// Each of `hidden` that one of `channels` (a name and its text) holds, as `{name}: {hidden}`,
/// in channel order; empty when every channel is clean.
pub fn exposed(hidden: &[&str], channels: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, text) in channels {
        for s in hidden {
            if text.contains(s) {
                out.push(format!("{name}: {s}"));
            }
        }
    }
    out
}

/// Each of `hidden` in what a server wrote (`t`: its raw stdout and its stderr) or in
/// `project`'s daemon log (`.rdy/run/daemon.log`, which a detached daemon's stdout and stderr
/// go to), as `{channel}: {hidden}`. Read it after the server and the daemon have exited and
/// before the project is removed. A missing log reads as empty.
pub fn exposed_outputs(hidden: &[&str], t: &Transcript, project: &Path) -> Vec<String> {
    let log = rdy_daemon::RunDir::of_project(project).log_file();
    let log = match std::fs::read(&log) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => panic!("{}: {e}", log.display()),
    };
    exposed(
        hidden,
        &[
            ("mcp stdout", &t.stdout),
            ("mcp stderr", &t.stderr),
            ("daemon.log", &log),
        ],
    )
}

/// Why a bounded session failed. A bounded session never panics: each failure is one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpFailure {
    /// The server could not be started, or a pipe or a worker channel failed (the server
    /// closed its input or its output, for example).
    Io,
    /// A line that is not JSON, not UTF-8, not ended by LF, or not a JSON-RPC message of the
    /// expected shape.
    Protocol,
    /// The session's stdout and stderr together passed the byte cap.
    OutputLimit,
    /// An operation passed its hang guard.
    Deadline,
    /// The server or a worker could not be stopped, reaped or joined.
    Cleanup,
}

/// What the stdout drain hands the client. Only a bounded session's drains send anything but
/// lines.
enum Incoming {
    Line(String),
    /// A line that is not UTF-8, or bytes with no LF before the end of stdout.
    Invalid,
    /// The session's output passed its cap.
    Overflow,
    /// Stdout ended, or could not be read.
    Closed,
}

/// A bounded session's byte cap: stdout and stderr together, counted as they are read, before
/// any buffer grows.
struct Bound {
    max: usize,
    count: AtomicUsize,
    over: AtomicBool,
}

impl Bound {
    /// Counts `n` more bytes; false (and the cap marked as passed) once the total passes `max`.
    fn admit(&self, n: usize) -> bool {
        let before = self.count.fetch_add(n, Ordering::SeqCst);
        if before.saturating_add(n) > self.max {
            self.over.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }

    fn passed(&self) -> bool {
        self.over.load(Ordering::SeqCst)
    }
}

/// How long a bounded session's stop waits for each worker to end once the server's process
/// group is killed: a hang guard.
const JOIN_GUARD: Duration = DEADLINE;

pub struct McpClient {
    child: Child,
    writer: Sender<String>,
    written: Receiver<bool>,
    writer_thread: Option<JoinHandle<()>>,
    lines: Receiver<Incoming>,
    /// The child's stderr, drained on a thread (a child started with a piped stderr only;
    /// empty otherwise).
    stderr: Arc<Mutex<String>>,
    stderr_drain: Option<JoinHandle<()>>,
    /// Every line the child wrote to stdout, as it came: replies, notifications, and lines
    /// that are not JSON-RPC at all (which `recv` skips). `None` for a client that does not
    /// keep its output (a long run would grow it without bound).
    stdout: Option<Arc<Mutex<String>>>,
    stdout_drain: Option<JoinHandle<()>>,
    pending: Vec<Value>,
    /// The notifications (lines with a `method` and no `id`) received, in arrival order (ib10).
    notifications: Vec<Value>,
    /// The server's reply to `initialize`.
    init: Value,
    next: u64,
    calls: usize,
    latencies: Vec<(String, Duration)>,
    /// A bounded session's cap; `None` for the unlimited clients the tests and perf use.
    bound: Option<Arc<Bound>>,
    /// A bounded session's per-operation hang guard.
    budget: Duration,
    /// The server's PID, which also leads its process group in a bounded session.
    pid: u32,
    /// A bounded session that was stopped: its group killed, its child reaped, its workers
    /// joined.
    stopped: bool,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if self.bound.is_some() && !self.stopped {
            let _ = platform::kill_process_group(self.pid);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads the server's stdout in chunks and hands each LF-ended line to `tx`.
///
/// Unlimited (`bound` is `None`), as before: invalid UTF-8 becomes a line that fails to parse,
/// CR and LF are trimmed, and bytes left at the end count as a last line. Bounded: each chunk
/// is counted before it is kept, a line that is not UTF-8 or bytes with no LF at the end are
/// [`Incoming::Invalid`], passing the cap is [`Incoming::Overflow`], and the end of stdout is
/// [`Incoming::Closed`].
fn read_lines(
    mut out: impl Read,
    tx: &Sender<Incoming>,
    raw: Option<&Arc<Mutex<String>>>,
    bound: Option<&Bound>,
) {
    let deliver = |line: &[u8]| -> bool {
        let text = match bound {
            None => String::from_utf8_lossy(line)
                .trim_end_matches(['\r', '\n'])
                .to_string(),
            Some(_) => match std::str::from_utf8(line) {
                Ok(t) => t.strip_suffix('\n').unwrap_or(t).to_string(),
                Err(_) => {
                    let _ = tx.send(Incoming::Invalid);
                    return false;
                }
            },
        };
        if let Some(raw) = raw {
            let mut all = raw.lock().unwrap_or_else(|e| e.into_inner());
            all.push_str(&text);
            all.push('\n');
        }
        tx.send(Incoming::Line(text)).is_ok()
    };
    let mut partial: Vec<u8> = Vec::new();
    let mut scanned = 0;
    let mut buf = [0u8; 8192];
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Some(b) = bound
            && !b.admit(n)
        {
            let _ = tx.send(Incoming::Overflow);
            return;
        }
        partial.extend_from_slice(&buf[..n]);
        while let Some(at) = partial[scanned..].iter().position(|&c| c == b'\n') {
            let line: Vec<u8> = partial.drain(..=scanned + at).collect();
            scanned = 0;
            if !deliver(&line) {
                return;
            }
        }
        scanned = partial.len();
    }
    match bound {
        None => {
            if !partial.is_empty() {
                deliver(&partial);
            }
        }
        Some(_) => {
            if !partial.is_empty() {
                let _ = tx.send(Incoming::Invalid);
            }
            let _ = tx.send(Incoming::Closed);
        }
    }
}

/// Reads the server's stderr into `sink`. Unlimited, as before: bytes that are not UTF-8 become
/// U+FFFD. Bounded: each chunk is counted before it is kept, and passing the cap is sent to the
/// client as [`Incoming::Overflow`]. The stream must be UTF-8: a character may be split across
/// reads, but bytes that can never be UTF-8, or a character cut off at the end, are sent as
/// [`Incoming::Invalid`] (the rest is still read and counted, but not kept).
fn read_stderr(
    mut err: impl Read,
    sink: &Mutex<String>,
    bound: Option<(&Bound, &Sender<Incoming>)>,
) {
    let keep = |text: &str| {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_str(text)
    };
    let mut buf = [0u8; 4096];
    // the start of a character that the last read cut off: at most three bytes
    let mut carry: Vec<u8> = Vec::new();
    let mut invalid = false;
    loop {
        let n = match err.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let Some((b, tx)) = bound else {
            keep(&String::from_utf8_lossy(&buf[..n]));
            continue;
        };
        if !b.admit(n) {
            let _ = tx.send(Incoming::Overflow);
            return;
        }
        if invalid {
            continue;
        }
        carry.extend_from_slice(&buf[..n]);
        let (valid, rest) = match std::str::from_utf8(&carry) {
            Ok(text) => (text.len(), false),
            // a character that the next read may complete
            Err(e) if e.error_len().is_none() => (e.valid_up_to(), false),
            Err(_) => (0, true),
        };
        if rest {
            invalid = true;
            carry.clear();
            let _ = tx.send(Incoming::Invalid);
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&carry[..valid]) {
            keep(text);
        }
        carry.drain(..valid);
    }
    if let Some((_, tx)) = bound
        && !invalid
        && !carry.is_empty()
    {
        let _ = tx.send(Incoming::Invalid);
    }
}

/// Waits at most `guard` for the thread `h` to end, then joins it; false when it did not end
/// in time (it is left running) or it panicked.
fn join_within(h: JoinHandle<()>, guard: Duration) -> bool {
    let end = Instant::now() + guard;
    while !h.is_finished() {
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.join().is_ok()
}

/// The [`Reply`] in a `tools/call` response `v`. Lenient (the unlimited clients): a missing
/// text reads as `null`. Strict (a bounded session, whose `v` already passed [`message`]): an
/// `error` is a JSON-RPC error object, and a `result` is a tool result whose content blocks
/// each have their type's required fields, whose first block is text, whose `isError` (when
/// given) is a boolean and whose `structuredContent` (when given) is an object; `None`
/// otherwise.
fn decode_reply(v: &Value, strict: bool) -> Option<Reply> {
    if let Some(error) = v.get("error") {
        if strict && !rpc_error(error) {
            return None;
        }
        return Some(Reply::RpcError(error.clone()));
    }
    let result = &v["result"];
    if strict && !tool_result(result) {
        return None;
    }
    let text = result["content"][0]["text"].as_str().unwrap_or("null");
    let is_error = match result.get("isError") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(other) => other == &json!(true),
    };
    let body: Value = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
    Some(if is_error {
        Reply::ToolError(body)
    } else {
        Reply::Ok(body)
    })
}

/// Whether `v` holds the string field `key`.
fn has_str(v: &Value, key: &str) -> bool {
    v.get(key).is_some_and(Value::is_string)
}

/// Whether `v` holds the object field `key`.
fn has_object(v: &Value, key: &str) -> bool {
    v.get(key).is_some_and(Value::is_object)
}

/// Whether `e` is a JSON-RPC error object: an integer `code` and a string `message`.
fn rpc_error(e: &Value) -> bool {
    e.is_object() && (e["code"].is_i64() || e["code"].is_u64()) && has_str(e, "message")
}

/// Whether `r` is a `tools/call` result: a non-empty `content` array of well-formed blocks, the
/// first of them text, with an optional boolean `isError` and an optional object
/// `structuredContent`.
fn tool_result(r: &Value) -> bool {
    let Some(content) = r.get("content").and_then(Value::as_array) else {
        return false;
    };
    r.is_object()
        && content.first().is_some_and(|c| c["type"] == "text")
        && content.iter().all(content_block)
        && r.get("isError").is_none_or(Value::is_boolean)
        && r.get("structuredContent").is_none_or(Value::is_object)
}

/// Whether `c` is an MCP content block with its type's required fields: `text`, `image` and
/// `audio` (base64 `data` and `mimeType`), `resource_link` (`uri` and `name`) or `resource`
/// (a `resource` with a `uri` and its `text` or `blob`).
fn content_block(c: &Value) -> bool {
    match c.get("type").and_then(Value::as_str) {
        Some("text") => has_str(c, "text"),
        Some("image" | "audio") => has_str(c, "data") && has_str(c, "mimeType"),
        Some("resource_link") => has_str(c, "uri") && has_str(c, "name"),
        Some("resource") => {
            let r = &c["resource"];
            has_object(c, "resource")
                && has_str(r, "uri")
                && (has_str(r, "text") || has_str(r, "blob"))
        }
        _ => false,
    }
}

/// Whether `r` is an `initialize` result: a `protocolVersion`, a `capabilities` object and a
/// `serverInfo` with its `name` and `version`.
fn initialize_result(r: &Value) -> bool {
    let info = &r["serverInfo"];
    has_str(r, "protocolVersion")
        && has_object(r, "capabilities")
        && has_str(info, "name")
        && has_str(info, "version")
}

/// One line of a bounded session's stdout as a JSON-RPC 2.0 message the client accepts: a
/// response (a number or string `id` and exactly one of `result` and `error`) or a
/// notification (a string `method` and no `id`). A request from the server is refused: the
/// client answers none. Anything else is [`McpFailure::Protocol`].
fn message(line: &str) -> Result<Value, McpFailure> {
    let v: Value = serde_json::from_str(line).map_err(|_| McpFailure::Protocol)?;
    if !v.is_object() || v["jsonrpc"] != "2.0" {
        return Err(McpFailure::Protocol);
    }
    let response = match (v.get("id"), v.get("method")) {
        (Some(id), None) => {
            (id.is_number() || id.is_string())
                && v.get("result").is_some() != v.get("error").is_some()
        }
        (None, Some(method)) => {
            method.is_string()
                && v.get("params")
                    .is_none_or(|p| p.is_object() || p.is_array())
        }
        _ => false,
    };
    if response {
        Ok(v)
    } else {
        Err(McpFailure::Protocol)
    }
}

impl McpClient {
    pub fn start(project: &Path) -> McpClient {
        McpClient::start_with_env(project, &[])
    }

    /// `rdy mcp --no-daemon --project <project>`: the in-process stdio server, no daemon.
    pub fn start_with_env(project: &Path, env: &[(&str, &str)]) -> McpClient {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--no-daemon", "--project"])
            .arg(project)
            .envs(env.iter().copied())
            .stderr(Stdio::null());
        McpClient::launch_with(cmd, false)
    }

    /// `start_with_env` with the child's stderr kept, for a check of everything the server
    /// writes ([`McpClient::finish`]).
    pub fn start_capturing(project: &Path, env: &[(&str, &str)]) -> McpClient {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--no-daemon", "--project"])
            .arg(project)
            .envs(env.iter().copied())
            .stderr(Stdio::piped());
        McpClient::launch(cmd)
    }

    /// `start_proxy_at` that keeps every line the proxy writes, for [`McpClient::finish`].
    pub fn start_proxy_capturing(dir: &Path, env: &[(String, String)]) -> McpClient {
        McpClient::launch(McpClient::proxy_command(dir, env))
    }

    /// `rdy mcp --project <s.dir>` as `s`'s human: the proxy to the project's daemon, which it
    /// starts if none runs (`Served` stops it at drop). Its stderr is kept for `wait_exit`.
    pub fn start_proxy(s: &Served) -> McpClient {
        McpClient::start_proxy_at(s.dir.path(), &s.env_pairs())
    }

    /// `start_proxy` for a caller that cannot hold the `Served` (a worker thread that outlives
    /// its borrow): the project's directory and the human's environment (`Served::env_pairs`).
    pub fn start_proxy_at(dir: &Path, env: &[(String, String)]) -> McpClient {
        McpClient::launch_with(McpClient::proxy_command(dir, env), false)
    }

    fn proxy_command(dir: &Path, env: &[(String, String)]) -> Command {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--project"])
            .arg(dir)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stderr(Stdio::piped());
        cmd
    }

    /// Starts `cmd` as the server and initializes it, keeping every line it writes for
    /// [`McpClient::finish`]. `cmd` sets its own stderr: piped to keep it, null to drop it. A
    /// test may launch a stub server this way.
    pub fn launch(cmd: Command) -> McpClient {
        McpClient::launch_with(cmd, true)
    }

    fn launch_with(cmd: Command, keep: bool) -> McpClient {
        let mut c = McpClient::spawn_session(cmd, keep, None).expect("spawn rdy mcp");
        let id = c.next_id();
        c.send(&McpClient::initialize_request(id));
        c.init = c.recv(id, DEADLINE);
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    /// The `initialize` request with `id`.
    fn initialize_request(id: u64) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "rdy-qa", "version": "0"}}})
    }

    /// Starts `cmd` with piped stdin and stdout and starts the workers: the stdout drain, the
    /// stderr drain (when `cmd` pipes stderr) and the stdin writer. A bounded session counts
    /// both streams against `bound` and sends its stderr overflow to the client.
    fn spawn_session(
        mut cmd: Command,
        keep: bool,
        bound: Option<Arc<Bound>>,
    ) -> std::io::Result<McpClient> {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        // the command holds no pipe end of the child's, but it is not needed any more
        drop(cmd);
        let pid = child.id();
        // a worker that cannot start: the server is stopped and reaped, and the error returned
        let abandon = |child: &mut Child, e: std::io::Error| {
            if bound.is_some() {
                let _ = platform::kill_process_group(pid);
            }
            let _ = child.kill();
            let _ = child.wait();
            e
        };
        let (Some(out), Some(mut stdin)) = (child.stdout.take(), child.stdin.take()) else {
            let e = std::io::Error::other("the server's stdin and stdout are not piped");
            return Err(abandon(&mut child, e));
        };
        let (tx, rx) = channel();
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_drain = child.stderr.take().map(|err| {
            // drained so a full pipe never blocks the child
            let sink = stderr.clone();
            let counted = bound.clone().map(|b| (b, tx.clone()));
            std::thread::Builder::new()
                .name("rdy-qa-mcp-stderr".into())
                .spawn(move || {
                    read_stderr(err, &sink, counted.as_ref().map(|(b, tx)| (&**b, tx)));
                })
        });
        let stderr_drain = match stderr_drain.transpose() {
            Ok(h) => h,
            Err(e) => return Err(abandon(&mut child, e)),
        };
        let stdout = keep.then(|| Arc::new(Mutex::new(String::new())));
        let stdout_drain = {
            let (raw, bound) = (stdout.clone(), bound.clone());
            std::thread::Builder::new()
                .name("rdy-qa-mcp-stdout".into())
                .spawn(move || read_lines(out, &tx, raw.as_ref(), bound.as_deref()))
        };
        let stdout_drain = match stdout_drain {
            Ok(h) => h,
            Err(e) => return Err(abandon(&mut child, e)),
        };
        // stdin writes happen on their own thread so a server that stops reading cannot block us
        let (wtx, wrx) = channel::<String>();
        let (dtx, drx) = channel::<bool>();
        let writer_thread = std::thread::Builder::new()
            .name("rdy-qa-mcp-stdin".into())
            .spawn(move || {
                for line in wrx {
                    let ok = writeln!(stdin, "{line}")
                        .and_then(|_| stdin.flush())
                        .is_ok();
                    if dtx.send(ok).is_err() || !ok {
                        break;
                    }
                }
            });
        let writer_thread = match writer_thread {
            Ok(h) => h,
            Err(e) => return Err(abandon(&mut child, e)),
        };
        Ok(McpClient {
            child,
            writer: wtx,
            written: drx,
            writer_thread: Some(writer_thread),
            lines: rx,
            stderr,
            stderr_drain,
            stdout,
            stdout_drain: Some(stdout_drain),
            pending: vec![],
            notifications: vec![],
            init: Value::Null,
            next: 0,
            calls: 0,
            latencies: vec![],
            bound,
            budget: DEADLINE,
            pid,
            stopped: false,
        })
    }

    /// Starts `command` as a bounded session and initializes it. The server runs in a process
    /// group of its own, and its stdout and stderr together may write at most `max_output`
    /// bytes in the whole session. Every operation has the [`DEADLINE`] hang guard. Nothing
    /// panics: a failure stops the session (its stdin closed, its group killed, its child
    /// reaped, its workers joined) and is returned.
    pub fn try_launch_bounded(
        command: Command,
        max_output: usize,
    ) -> Result<McpClient, McpFailure> {
        McpClient::try_launch_inner(command, max_output).map_err(|(failure, _)| failure)
    }

    /// [`McpClient::try_launch_bounded`], with the stopped session beside a failure (for tests).
    fn try_launch_inner(
        mut command: Command,
        max_output: usize,
    ) -> Result<McpClient, (McpFailure, Option<Box<McpClient>>)> {
        platform::own_process_group(&mut command);
        command.stderr(Stdio::piped());
        let bound = Arc::new(Bound {
            max: max_output,
            count: AtomicUsize::new(0),
            over: AtomicBool::new(false),
        });
        let mut c = McpClient::spawn_session(command, true, Some(bound))
            .map_err(|_| (McpFailure::Io, None))?;
        let initialized = (|| {
            let id = c.next_id();
            let v = c.try_request(&McpClient::initialize_request(id), id)?;
            if v.get("error").is_some() || !initialize_result(&v["result"]) {
                return Err(McpFailure::Protocol);
            }
            c.init = v;
            c.try_send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        })();
        match initialized {
            Ok(()) => Ok(c),
            Err(failure) => {
                let failure = c.fail(failure);
                Err((failure, Some(Box::new(c))))
            }
        }
    }

    /// Calls `tool` with `args` in a bounded session: the strict reply, or the failure, after
    /// which the session is stopped.
    pub fn try_call(&mut self, tool: &str, args: Value) -> Result<Reply, McpFailure> {
        let called = (|| {
            let id = self.next_id();
            self.calls += 1;
            let v = self.try_request(
                &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}),
                id,
            )?;
            decode_reply(&v, true).ok_or(McpFailure::Protocol)
        })();
        called.map_err(|failure| self.fail(failure))
    }

    /// Ends a bounded session: closes its stdin, waits at most `budget` for the server to
    /// exit, then kills its process group, reaps it and joins every worker. Everything it
    /// wrote, or the failure: a server still running at `budget` is [`McpFailure::Deadline`],
    /// output past the cap at any time [`McpFailure::OutputLimit`], and a stop that fails
    /// [`McpFailure::Cleanup`].
    pub fn try_finish(mut self, budget: Duration) -> Result<Transcript, McpFailure> {
        if self.stopped || self.bound.is_none() {
            return Err(McpFailure::Io);
        }
        self.close_stdin();
        let end = Instant::now() + budget;
        let mut exited = false;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    exited = true;
                    break;
                }
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) | Err(_) => break,
            }
        }
        let code = self.stop()?;
        if !exited {
            return Err(McpFailure::Deadline);
        }
        if self.bound.as_ref().is_some_and(|b| b.passed()) {
            return Err(McpFailure::OutputLimit);
        }
        // Both drains are joined, so everything the server wrote up to the end of its stdout
        // is queued: each line must be a JSON-RPC message, and stdout and stderr valid.
        for incoming in self.lines.try_iter() {
            match incoming {
                Incoming::Line(line) => {
                    message(&line)?;
                }
                Incoming::Invalid => return Err(McpFailure::Protocol),
                Incoming::Overflow => return Err(McpFailure::OutputLimit),
                Incoming::Closed => {}
            }
        }
        let take = |m: &Arc<Mutex<String>>| m.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Ok(Transcript {
            code,
            stdout: self.stdout.as_ref().map(take).unwrap_or_default(),
            stderr: take(&self.stderr),
        })
    }

    /// Stops a bounded session after `failure`: the failure, or [`McpFailure::Cleanup`] when
    /// the stop fails.
    fn fail(&mut self, failure: McpFailure) -> McpFailure {
        match self.stop() {
            Ok(_) => failure,
            Err(cleanup) => cleanup,
        }
    }

    /// Stops a bounded session: closes its stdin, kills its process group, reaps the server
    /// and joins the writer and both drains, each within [`JOIN_GUARD`]. The server's exit
    /// code (`None` when killed by a signal, or when the session was already stopped).
    fn stop(&mut self) -> Result<Option<i32>, McpFailure> {
        if self.stopped {
            return Ok(None);
        }
        self.stopped = true;
        self.close_stdin();
        let mut clean = platform::kill_process_group(self.pid).is_ok();
        let code = match self.child.wait() {
            Ok(status) => status.code(),
            Err(_) => {
                clean = false;
                None
            }
        };
        for h in [
            self.writer_thread.take(),
            self.stdout_drain.take(),
            self.stderr_drain.take(),
        ]
        .into_iter()
        .flatten()
        {
            clean &= join_within(h, JOIN_GUARD);
        }
        if clean {
            Ok(code)
        } else {
            Err(McpFailure::Cleanup)
        }
    }

    /// Sends `request` and waits for the response with `id`, within the session's budget.
    fn try_request(&mut self, request: &Value, id: u64) -> Result<Value, McpFailure> {
        self.try_send(request)?;
        self.try_recv(id)
    }

    /// Writes one message as a line, waiting at most the session's budget for it to be taken.
    fn try_send(&mut self, v: &Value) -> Result<(), McpFailure> {
        if self.writer.send(v.to_string()).is_err() {
            return Err(McpFailure::Io);
        }
        match self.written.recv_timeout(self.budget) {
            Ok(true) => Ok(()),
            Ok(false) | Err(RecvTimeoutError::Disconnected) => Err(McpFailure::Io),
            Err(RecvTimeoutError::Timeout) => Err(McpFailure::Deadline),
        }
    }

    /// The JSON-RPC response with `id`, waiting at most the session's budget. Every line must
    /// be a JSON-RPC 2.0 object: a response (set aside when it has another id) or a
    /// notification (kept).
    fn try_recv(&mut self, id: u64) -> Result<Value, McpFailure> {
        if let Some(i) = self.pending.iter().position(|v| v["id"] == json!(id)) {
            return Ok(self.pending.remove(i));
        }
        let end = Instant::now() + self.budget;
        loop {
            if self.bound.as_ref().is_some_and(|b| b.passed()) {
                return Err(McpFailure::OutputLimit);
            }
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(Incoming::Line(line)) => line,
                Ok(Incoming::Invalid) => return Err(McpFailure::Protocol),
                Ok(Incoming::Overflow) => return Err(McpFailure::OutputLimit),
                Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(McpFailure::Io);
                }
                Err(RecvTimeoutError::Timeout) => return Err(McpFailure::Deadline),
            };
            let v = message(&line)?;
            if let Some(got) = v.get("id") {
                if got == &json!(id) {
                    return Ok(v);
                }
                self.pending.push(v);
            } else {
                self.notifications.push(v);
            }
        }
    }

    /// Closes the child's stdin: the writer thread ends and drops the pipe.
    pub fn close_stdin(&mut self) {
        let (tx, _) = channel();
        self.writer = tx;
    }

    /// Waits at most `budget` for the child to exit: its exit code (`None` when killed by a
    /// signal) and the stderr captured so far. Past `budget` it is a Critical failure.
    pub fn wait_exit(&mut self, budget: Duration) -> (Option<i32>, String) {
        let end = Instant::now() + budget;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    // the drain thread ends at the pipe's EOF, which follows the exit; it is
                    // waited on briefly, in case a grandchild still holds the pipe
                    if let Some(h) = self.stderr_drain.take() {
                        let t = Instant::now();
                        while !h.is_finished() && t.elapsed() < Duration::from_secs(1) {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    let err = self
                        .stderr
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    return (status.code(), err);
                }
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => self.critical(&format!("the server did not exit within {budget:?}")),
                Err(e) => self.critical(&format!("waiting for the server failed: {e}")),
            }
        }
    }

    /// Closes the child's stdin, waits at most `budget` for it to exit, then for both of its
    /// pipes to reach their end, and returns everything it wrote. A pipe still open after
    /// `budget` (a grandchild holds it) is a Critical failure: its output cannot be checked, as
    /// is a client that does not keep its output.
    pub fn finish(mut self, budget: Duration) -> Transcript {
        let Some(stdout) = self.stdout.clone() else {
            self.critical(
                "this client does not keep its output; start it with a capturing constructor",
            );
        };
        self.close_stdin();
        let end = Instant::now() + budget;
        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => self.critical(&format!("the server did not exit within {budget:?}")),
                Err(e) => self.critical(&format!("waiting for the server failed: {e}")),
            }
        };
        for h in [self.stdout_drain.take(), self.stderr_drain.take()]
            .into_iter()
            .flatten()
        {
            while !h.is_finished() {
                if Instant::now() >= end {
                    self.critical(&format!("a pipe of the server stayed open past {budget:?}"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = h.join();
        }
        let take = |m: &Arc<Mutex<String>>| m.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Transcript {
            code,
            stdout: take(&stdout),
            stderr: take(&self.stderr),
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn send(&mut self, v: &Value) {
        self.send_raw(&v.to_string());
    }

    pub fn send_raw(&mut self, line: &str) {
        self.send_raw_within(line, DEADLINE);
    }

    /// Write one line to the server, waiting at most `budget` for it to be accepted.
    pub fn send_raw_within(&mut self, line: &str, budget: Duration) {
        if self.writer.send(line.to_string()).is_err() {
            self.critical("the server closed its input");
        }
        match self.written.recv_timeout(budget) {
            Ok(true) => {}
            Ok(false) => self.critical("the server closed its input"),
            Err(_) => self.critical(&format!(
                "the server did not accept a request within {budget:?}"
            )),
        }
    }

    fn critical(&mut self, what: &str) -> ! {
        let _ = self.child.kill();
        panic!("CRITICAL: {what}");
    }

    /// The raw JSON-RPC response with `id`, waiting at most `budget`.
    pub fn recv(&mut self, id: u64, budget: Duration) -> Value {
        if let Some(i) = self.pending.iter().position(|v| v["id"] == json!(id)) {
            return self.pending.remove(i);
        }
        let end = Instant::now() + budget;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(Incoming::Line(l)) => l,
                _ => self.critical(&format!("no reply to request {id} within {budget:?}")),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"] == json!(id) {
                return v;
            }
            if v.get("id").is_some() {
                self.pending.push(v);
            } else {
                self.keep_notification(v);
            }
        }
    }

    /// The next reply line in arrival order, whatever its id, waiting at most `budget`. Replies
    /// already set aside by [`McpClient::recv`] come first, in the order they arrived.
    pub fn recv_next(&mut self, budget: Duration) -> Value {
        if !self.pending.is_empty() {
            return self.pending.remove(0);
        }
        let end = Instant::now() + budget;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(Incoming::Line(l)) => l,
                _ => self.critical(&format!("no reply within {budget:?}")),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("id").is_some() {
                return v;
            }
            self.keep_notification(v);
        }
    }

    /// Keeps a line with a `method` and no `id`: a notification.
    fn keep_notification(&mut self, v: Value) {
        if v.get("method").is_some() {
            self.notifications.push(v);
        }
    }

    /// The notifications received so far, in arrival order (ib10).
    pub fn notifications(&self) -> &[Value] {
        &self.notifications
    }

    /// The server's reply to `initialize` (its `result` and `id`).
    pub fn initialize_result(&self) -> &Value {
        &self.init
    }

    /// Waits at most `budget` for a line, then takes every line already waiting: notifications go
    /// to [`McpClient::notifications`], replies are set aside for `recv`. Returns the lines taken.
    pub fn drain(&mut self, budget: Duration) -> usize {
        let mut taken = 0;
        let mut wait = budget;
        while let Ok(Incoming::Line(line)) = self.lines.recv_timeout(wait) {
            wait = Duration::ZERO;
            taken += 1;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("id").is_some() {
                self.pending.push(v);
            } else {
                self.keep_notification(v);
            }
        }
        taken
    }

    /// The `resources` of `resources/list`.
    pub fn list_resources(&mut self) -> Vec<Value> {
        let v = self.request("resources/list", json!({}));
        v["result"]["resources"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The `resourceTemplates` of `resources/templates/list`.
    pub fn list_resource_templates(&mut self) -> Vec<Value> {
        let v = self.request("resources/templates/list", json!({}));
        v["result"]["resourceTemplates"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The raw JSON-RPC reply to `resources/read`: `result.contents` or `error`.
    pub fn read_resource(&mut self, uri: &str) -> Value {
        self.request("resources/read", json!({"uri": uri}))
    }

    /// The raw JSON-RPC reply to (legacy) `resources/subscribe`.
    pub fn subscribe(&mut self, uri: &str) -> Value {
        self.request("resources/subscribe", json!({"uri": uri}))
    }

    /// True once a `notifications/resources/updated` for `uri` has arrived, polling for at most
    /// `budget`. It also finds one that arrived earlier.
    pub fn wait_notification(&mut self, uri: &str, budget: Duration) -> bool {
        let end = Instant::now() + budget;
        loop {
            if self.notifications.iter().any(|n| {
                n["method"] == "notifications/resources/updated" && n["params"]["uri"] == uri
            }) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            self.drain(left.min(Duration::from_millis(50)));
        }
    }

    /// Sends one request and returns its raw JSON-RPC reply.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.recv(id, DEADLINE)
    }

    pub fn call(&mut self, tool: &str, args: Value) -> Reply {
        self.call_within(tool, args, DEADLINE)
    }

    pub fn call_within(&mut self, tool: &str, args: Value, budget: Duration) -> Reply {
        let id = self.next_id();
        let t = Instant::now();
        self.calls += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
        let v = self.recv(id, budget);
        self.latencies.push((tool.to_string(), t.elapsed()));
        decode_reply(&v, false).unwrap_or(Reply::Ok(Value::Null))
    }

    pub fn tools(&mut self) -> Vec<Value> {
        let id = self.next_id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}}));
        self.recv(id, DEADLINE)["result"]["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The server still answers a normal read within the deadline.
    pub fn alive(&mut self) {
        match self.call("rdy_overview", json!({})) {
            Reply::Ok(_) | Reply::ToolError(_) => {}
            Reply::RpcError(e) => {
                self.critical(&format!("rdy_overview failed at the protocol level: {e}"))
            }
        }
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn latencies(&self) -> &[(String, Duration)] {
        &self.latencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake server: answers `initialize`, then sends one notification and one reply with an id
    /// the client is not waiting for.
    fn fake() -> McpClient {
        let script = r#"read line
echo '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"resources":{"subscribe":true}}}}'
echo '{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":"rdy://inbox"}}'
echo '{"jsonrpc":"2.0","id":99,"result":{}}'
sleep 20"#;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]).stderr(Stdio::null());
        McpClient::launch(cmd)
    }

    #[test]
    fn ib10_the_client_keeps_the_initialize_reply_and_the_notifications() {
        let mut c = fake();
        assert_eq!(
            c.initialize_result()["result"]["capabilities"]["resources"]["subscribe"],
            true
        );
        assert!(c.wait_notification("rdy://inbox", Duration::from_secs(10)));
        // the one notification the fake sent, and nothing taken for one
        let uris: Vec<&str> = c
            .notifications()
            .iter()
            .map(|n| n["params"]["uri"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(uris, ["rdy://inbox"]);
        // the reply that was not asked for is set aside, never taken for a notification
        assert_eq!(c.recv(99, Duration::from_secs(5))["id"], 99);
    }

    /// The bounded session's cap in these tests: the release checker's.
    const CAP: usize = platform::RELEASE_CHILD_OUTPUT_MAX;
    /// A peer's reply to `initialize`.
    const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"peer","version":"0"}}}"#;

    /// A peer script that answers `initialize`, reads the `initialized` notification, then runs
    /// `then`.
    fn after_init(then: &str) -> String {
        format!("read -r l; echo '{INIT}'; read -r n; {then}")
    }

    /// A started bounded session of the peer `script`.
    fn bounded(script: &str) -> McpClient {
        McpClient::try_launch_bounded(peer(script, &[]), CAP)
            .unwrap_or_else(|f| panic!("launch: {f:?}: {script}"))
    }

    #[test]
    fn pre3_bounded_mcp_finish_checks_the_rest_of_the_output() {
        use McpFailure::*;
        // Everything the server writes after the last reply, up to the end of its stdout, is
        // checked before a finish succeeds. The peer waits for its input to close first.
        // what, the peer's end, and the finish's exit code or failure
        type Case = (&'static str, String, Result<Option<i32>, McpFailure>);
        let cases: [Case; 8] = [
            (
                "a line that is not JSON",
                "cat > /dev/null; echo 'not json'".into(),
                Err(Protocol),
            ),
            (
                "a line that is not UTF-8",
                r"cat > /dev/null; printf '\377\n'".into(),
                Err(Protocol),
            ),
            (
                "a last line with no LF",
                r#"cat > /dev/null; printf '{"jsonrpc":"2.0","method":"notifications/message"}'"#
                    .into(),
                Err(Protocol),
            ),
            (
                "a JSON line that is not JSON-RPC",
                r#"cat > /dev/null; echo '{"id":9,"result":{}}'"#.into(),
                Err(Protocol),
            ),
            (
                "a response with neither a result nor an error",
                r#"cat > /dev/null; echo '{"jsonrpc":"2.0","id":9}'"#.into(),
                Err(Protocol),
            ),
            (
                "a notification",
                r#"cat > /dev/null; echo '{"jsonrpc":"2.0","method":"notifications/message","params":{}}'"#
                    .into(),
                Ok(Some(0)),
            ),
            ("nothing more", "cat > /dev/null".into(), Ok(Some(0))),
            (
                "a nonzero exit, for the caller to judge",
                "cat > /dev/null; exit 17".into(),
                Ok(Some(17)),
            ),
        ];
        for (what, then, want) in cases {
            let c = bounded(&after_init(&then));
            let pid = c.pid;
            assert_eq!(c.try_finish(DEADLINE).map(|t| t.code), want, "{what}");
            assert!(platform::process_exited(pid), "{what}: the server reaped");
        }
        // Output past the cap still wins over a malformed line.
        let c = bounded(&after_init(&format!(
            "cat > /dev/null; echo 'not json'; head -c {CAP} /dev/zero >&2"
        )));
        assert_eq!(c.try_finish(DEADLINE).map(|_| ()), Err(OutputLimit));
    }

    #[test]
    fn pre3_bounded_mcp_checks_reply_shapes() {
        use McpFailure::*;
        // One tools/call reply line (id 2) from a peer; the session stops after a failure.
        let call = |reply: &str| -> Result<Reply, McpFailure> {
            let mut c = bounded(&after_init(&format!(
                "read -r c; echo '{reply}'; cat > /dev/null"
            )));
            let got = c.try_call("rdy_overview", json!({}));
            if got.is_err() {
                assert_stopped(&c, reply);
            }
            got
        };
        let result = |r: &str| format!(r#"{{"jsonrpc":"2.0","id":2,"result":{r}}}"#);
        let content = |items: &str| result(&format!(r#"{{"content":[{items}]}}"#));
        let text = r#"{"type":"text","text":"{\"ok\":true}"}"#;
        let refused: Vec<(&str, String)> = vec![
            (
                "an image in place of the text, with a text string",
                content(r#"{"type":"image","text":"{}"}"#),
            ),
            (
                "an image in place of the text, with a text object",
                content(r#"{"type":"image","text":{}}"#),
            ),
            (
                "a text that is not a string",
                content(r#"{"type":"text","text":{}}"#),
            ),
            ("a text with no type", content(r#"{"text":"{}"}"#)),
            ("a content item that is not an object", content(r#""x""#)),
            (
                "an unknown content type",
                content(r#"{"type":"video","text":"{}"}"#),
            ),
            (
                "an image without its data",
                content(&format!(
                    r#"{text},{{"type":"image","mimeType":"image/png"}}"#
                )),
            ),
            (
                "an audio without its mime type",
                content(&format!(r#"{text},{{"type":"audio","data":"AA=="}}"#)),
            ),
            (
                "a resource link without its uri",
                content(&format!(r#"{text},{{"type":"resource_link","name":"n"}}"#)),
            ),
            (
                "an embedded resource with neither text nor blob",
                content(&format!(
                    r#"{text},{{"type":"resource","resource":{{"uri":"rdy://x"}}}}"#
                )),
            ),
            ("no content", result(r#"{"content":[]}"#)),
            ("content that is not an array", result(r#"{"content":"x"}"#)),
            ("a result with no content", result("{}")),
            (
                "isError that is not a boolean",
                result(&format!(r#"{{"content":[{text}],"isError":"yes"}}"#)),
            ),
            (
                "structuredContent that is not an object",
                result(&format!(r#"{{"content":[{text}],"structuredContent":5}}"#)),
            ),
            ("a result that is not an object", result("[1]")),
            (
                "a result and an error together",
                format!(
                    r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{text}]}},"error":{{"code":1,"message":"m"}}}}"#
                ),
            ),
            (
                "an error without a message",
                r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602}}"#.into(),
            ),
            (
                "an error code that is not an integer",
                r#"{"jsonrpc":"2.0","id":2,"error":{"code":"x","message":"m"}}"#.into(),
            ),
            (
                "an error that is not an object",
                r#"{"jsonrpc":"2.0","id":2,"error":"x"}"#.into(),
            ),
            (
                "neither a result nor an error",
                r#"{"jsonrpc":"2.0","id":2}"#.into(),
            ),
            (
                "another JSON-RPC version",
                format!(r#"{{"jsonrpc":"1.0","id":2,"result":{{"content":[{text}]}}}}"#),
            ),
            (
                "an id that is neither a number nor a string",
                format!(r#"{{"jsonrpc":"2.0","id":{{}},"result":{{"content":[{text}]}}}}"#),
            ),
            (
                "a request from the server",
                r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#.into(),
            ),
            (
                "a notification whose method is not a string",
                r#"{"jsonrpc":"2.0","method":5}"#.into(),
            ),
        ];
        for (what, reply) in refused {
            assert_eq!(call(&reply).map(|_| ()), Err(Protocol), "{what}: {reply}");
        }

        // Well-formed replies: a text result (with the optional blocks), a tool error and a
        // JSON-RPC error.
        let full = content(&format!(
            r#"{text},{{"type":"image","data":"AA==","mimeType":"image/png"}},{{"type":"audio","data":"AA==","mimeType":"audio/wav"}},{{"type":"resource_link","uri":"rdy://x","name":"x"}},{{"type":"resource","resource":{{"uri":"rdy://x","text":"t"}}}},{{"type":"resource","resource":{{"uri":"rdy://y","blob":"AA=="}}}}"#
        ));
        match call(&full) {
            Ok(Reply::Ok(v)) => assert_eq!(v, json!({"ok": true})),
            other => panic!("a full result: {other:?}"),
        }
        match call(&result(&format!(
            r#"{{"content":[{text}],"isError":true,"structuredContent":{{}}}}"#
        ))) {
            Ok(Reply::ToolError(v)) => assert_eq!(v, json!({"ok": true})),
            other => panic!("a tool error: {other:?}"),
        }
        match call(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"bad","data":1}}"#) {
            Ok(Reply::RpcError(v)) => assert_eq!(v["code"], -32602),
            other => panic!("a JSON-RPC error: {other:?}"),
        }

        // The initialize result has its required fields.
        for (what, init) in [
            (
                "no protocol version",
                r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{},"serverInfo":{"name":"p","version":"0"}}}"#,
            ),
            (
                "no capabilities",
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"p","version":"0"}}}"#,
            ),
            (
                "no server info",
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}"#,
            ),
        ] {
            let script = format!("read -r l; echo '{init}'; exec sleep 1000");
            match McpClient::try_launch_inner(peer(&script, &[]), CAP) {
                Ok(_) => panic!("{what}: the session started"),
                Err((got, c)) => {
                    assert_eq!(got, Protocol, "{what}");
                    assert_stopped(&c.expect("a started session"), what);
                }
            }
        }
    }

    #[test]
    fn pre3_bounded_mcp_stderr_must_be_utf8() {
        use McpFailure::*;
        // A character split across two reads of the stderr drain (each reads at most 4096
        // bytes): the first 4096 bytes end inside it.
        for (what, ch) in [
            ("two bytes", "é"),
            ("three bytes", "€"),
            ("four bytes", "𝄞"),
        ] {
            let lead = 4096 - (ch.len() - 1);
            let c = bounded(&after_init(&format!(
                "cat > /dev/null; {{ head -c {lead} /dev/zero | tr '\\0' a; printf '{ch}'; }} >&2"
            )));
            let t = c
                .try_finish(DEADLINE)
                .unwrap_or_else(|f| panic!("{what}: {f:?}"));
            assert_eq!(t.stderr, format!("{}{ch}", "a".repeat(lead)), "{what}");
        }
        for (what, bytes) in [
            ("a byte that is never UTF-8", r"\377"),
            ("a character cut off at the end", r"a\303"),
            ("an unpaired continuation byte", r"\200"),
        ] {
            let c = bounded(&after_init(&format!(
                "cat > /dev/null; printf '{bytes}' >&2"
            )));
            let pid = c.pid;
            assert_eq!(c.try_finish(DEADLINE).map(|_| ()), Err(Protocol), "{what}");
            assert!(platform::process_exited(pid), "{what}: the server reaped");
        }
        // During a call, too.
        let mut c = bounded(&after_init(
            r"read -r c; printf '\377' >&2; exec sleep 1000",
        ));
        assert_eq!(
            c.try_call("rdy_overview", json!({})).map(|_| ()),
            Err(Protocol)
        );
        assert_stopped(&c, "invalid stderr during a call");
        drop(c);

        // An unlimited client keeps its lossy stderr.
        let mut cmd = peer(
            &format!(r"read -r l; printf '\377' >&2; echo '{INIT}'; read -r n; cat > /dev/null"),
            &[],
        );
        cmd.stderr(Stdio::piped());
        let t = McpClient::launch(cmd).finish(DEADLINE);
        assert_eq!(t.stderr, "\u{fffd}");
    }

    /// A peer: `sh -c script`, with `args` as `$0`, `$1`, ...
    fn peer(script: &str, args: &[&Path]) -> Command {
        let mut cmd = platform::shell_script(script);
        cmd.args(args);
        cmd
    }

    /// A session that was stopped: its workers joined and its server reaped.
    fn assert_stopped(c: &McpClient, what: &str) {
        assert!(c.stopped, "{what}: stopped");
        assert!(c.writer_thread.is_none(), "{what}: the writer joined");
        assert!(c.stdout_drain.is_none(), "{what}: the stdout drain joined");
        assert!(c.stderr_drain.is_none(), "{what}: the stderr drain joined");
        assert!(platform::process_exited(c.pid), "{what}: the server reaped");
    }

    #[test]
    fn pre3_bounded_mcp_is_fallible_and_reaps() {
        use crate::platform::test_support::{eventually, fifo, fifo_line};
        use McpFailure::*;
        let deep = format!("read -r l; echo '{}'; exec sleep 1000", "[".repeat(200));
        let over_stdout = format!("read -r l; head -c {} /dev/zero; exec sleep 1000", CAP + 1);
        let over_stderr = format!("head -c {} /dev/zero >&2; exec sleep 1000", CAP + 1);
        let at_launch: Vec<(&str, String, McpFailure)> = vec![
            ("malformed JSON", "read -r l; echo 'not json'; exec sleep 1000".into(), Protocol),
            (
                "a line that is not JSON-RPC 2.0",
                r#"read -r l; echo '{"id":1,"result":{}}'; exec sleep 1000"#.into(),
                Protocol,
            ),
            (
                "a result that is not an object",
                r#"read -r l; echo '{"jsonrpc":"2.0","id":1,"result":5}'; exec sleep 1000"#.into(),
                Protocol,
            ),
            (
                "an error in place of the result",
                r#"read -r l; echo '{"jsonrpc":"2.0","id":1,"error":{"code":-1}}'; exec sleep 1000"#
                    .into(),
                Protocol,
            ),
            (
                "neither a response nor a notification",
                r#"read -r l; echo '{"jsonrpc":"2.0"}'; exec sleep 1000"#.into(),
                Protocol,
            ),
            ("nesting past the parser's limit", deep, Protocol),
            (
                "invalid UTF-8",
                "read -r l; printf '\\377\\n'; exec sleep 1000".into(),
                Protocol,
            ),
            (
                "unterminated JSON",
                r#"read -r l; printf '{"jsonrpc":"2.0","id":1'"#.into(),
                Protocol,
            ),
            ("closed output", "read -r l; exit 0".into(), Io),
            ("oversized JSON", over_stdout, OutputLimit),
            ("stderr overflow", over_stderr, OutputLimit),
        ];
        for (what, script, want) in at_launch {
            match McpClient::try_launch_inner(peer(&script, &[]), CAP) {
                Ok(_) => panic!("{what}: the session started"),
                Err((got, c)) => {
                    assert_eq!(got, want, "{what}");
                    assert_stopped(&c.expect("a started session"), what);
                }
            }
        }
        match McpClient::try_launch_inner(Command::new("/nonexistent/rdy-qa-mcp"), CAP) {
            Err((Io, None)) => {}
            Err((got, _)) => panic!("a server that cannot start: {got:?}"),
            Ok(_) => panic!("a server that cannot start started"),
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("ready");
        fifo(&ready);
        // The server closes its input: the call cannot be written. A child that another test
        // forks while this session's pipe is being made holds a copy of the pipe's read end
        // until that child's exec, and the write then succeeds and the call waits. So each try
        // has a short hang guard, and a try passes only with the write's failure (Io).
        let mut waited = Vec::new();
        loop {
            let mut c = McpClient::try_launch_bounded(
                peer(
                    &after_init("exec 0<&-; echo ready > \"$0\"; exec sleep 1000"),
                    &[&ready],
                ),
                CAP,
            )
            .expect("launch");
            let line = fifo_line(ready.clone());
            assert_eq!(
                line.recv_timeout(DEADLINE).as_deref(),
                Ok("ready"),
                "the peer closed its input"
            );
            c.budget = Duration::from_secs(5);
            let got = c.try_call("rdy_overview", json!({})).map(|_| ());
            assert_stopped(&c, "closed input");
            drop(c);
            if got == Err(Io) {
                break;
            }
            waited.push(got);
            assert!(waited.len() < 5, "closed input: {waited:?}");
        }

        // An injected expiry: no reply within the budget. The peer's own child is killed with it.
        let mut c = McpClient::try_launch_bounded(
            peer(
                &after_init("sleep 1000 & echo $! > \"$0\"; exec sleep 1000"),
                &[&ready],
            ),
            CAP,
        )
        .expect("launch");
        let line = fifo_line(ready.clone());
        let grandchild: u32 = line
            .recv_timeout(DEADLINE)
            .expect("the peer's child")
            .parse()
            .expect("a pid");
        c.budget = Duration::ZERO;
        assert_eq!(
            c.try_call("rdy_overview", json!({})).map(|_| ()),
            Err(Deadline)
        );
        assert_stopped(&c, "expiry");
        eventually("the peer's child was killed", || {
            platform::process_exited(grandchild)
        });
        drop(c);

        // A tool result of the wrong shape.
        let mut c = McpClient::try_launch_bounded(
            peer(
                &after_init(
                    r#"read -r c; echo '{"jsonrpc":"2.0","id":2,"result":{"content":"x"}}'; exec sleep 1000"#,
                ),
                &[],
            ),
            CAP,
        )
        .expect("launch");
        assert_eq!(
            c.try_call("rdy_overview", json!({})).map(|_| ()),
            Err(Protocol)
        );
        assert_stopped(&c, "a malformed tool result");
        drop(c);

        // A successful session: a notification is kept, the reply decoded, the server reaped.
        let reply = r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"ok\":true}"}]}}"#;
        let note = r#"{"jsonrpc":"2.0","method":"notifications/message","params":{}}"#;
        let mut c = McpClient::try_launch_bounded(
            peer(
                &after_init(&format!(
                    "read -r c; echo '{note}'; echo '{reply}'; cat > /dev/null"
                )),
                &[],
            ),
            CAP,
        )
        .expect("launch");
        let pid = c.pid;
        match c.try_call("rdy_overview", json!({})) {
            Ok(Reply::Ok(v)) => assert_eq!(v, json!({"ok": true})),
            other => panic!("the call: {other:?}"),
        }
        assert_eq!(c.notifications().len(), 1);
        assert_eq!(c.initialize_result()["id"], 1);
        let t = c.try_finish(DEADLINE).expect("finish");
        assert_eq!(t.code, Some(0));
        assert_eq!(t.stdout, format!("{INIT}\n{note}\n{reply}\n"));
        assert!(platform::process_exited(pid), "the server reaped");

        // The cap covers stdout and stderr together: exactly the cap passes, one byte more fails.
        let rest = CAP - (INIT.len() + 1);
        for (extra, want) in [(0, Ok(CAP)), (1, Err(OutputLimit))] {
            let c = McpClient::try_launch_bounded(
                peer(
                    &after_init(&format!(
                        "head -c {} /dev/zero >&2; cat > /dev/null",
                        rest + extra
                    )),
                    &[],
                ),
                CAP,
            )
            .expect("launch");
            let pid = c.pid;
            let got = c
                .try_finish(DEADLINE)
                .map(|t| t.stdout.len() + t.stderr.len());
            assert_eq!(got, want, "{extra} byte(s) past the cap");
            assert!(platform::process_exited(pid), "the server reaped");
        }

        // A server that does not exit when its input closes: the finish's own expiry.
        let c = McpClient::try_launch_bounded(peer(&after_init("exec sleep 1000"), &[]), CAP)
            .expect("launch");
        let pid = c.pid;
        assert_eq!(c.try_finish(Duration::ZERO).map(|_| ()), Err(Deadline));
        assert!(
            platform::process_exited(pid),
            "the server killed and reaped"
        );
    }

    #[test]
    fn ib10_drain_returns_at_once_when_nothing_is_waiting() {
        let mut c = fake();
        assert!(c.wait_notification("rdy://inbox", Duration::from_secs(10)));
        // the fake's one stray reply may still be on its way; once it is taken, nothing is left
        c.drain(Duration::from_millis(200));
        assert_eq!(c.drain(Duration::from_millis(50)), 0);
    }
}
