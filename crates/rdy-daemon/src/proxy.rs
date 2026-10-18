//! `rdy mcp`: a stdio-to-socket proxy. It connects to the project's daemon as an `mcp` session
//! (starting the daemon if none runs) and pumps bytes both ways, one task each way.
//!
//! Nothing here can reorder replies: one task reads stdin and writes the socket (through
//! `pump_lines`, which refuses an over-long line before the daemon sees it), one task reads the
//! socket and writes stdout, and the socket is a byte stream. The daemon answers each connection
//! in request order (its tool calls are stamped at the transport: `rdy_mcp::order`).
//!
//! The proxy also keeps the ids of the requests it has sent and not yet seen answered
//! ([`InFlight`]). If the daemon goes away, it answers each of them itself with an
//! `E_DAEMON_GONE` error before it exits, so a host that waits on a request id is never left
//! waiting for a reply that cannot come.
//!
//! The proxy is the caller of its session's signing callbacks
//! ([`crate::signing_transport`]): it answers them with its own SSH agent and never forwards
//! them to stdout. The active request is the oldest tool call not yet answered or cancelled
//! (the daemon runs a session's tool calls one at a time, in arrival order): the host's
//! `notifications/cancelled` for it cancels the ask of the agent in flight, and one for any
//! other request cancels nothing. At stdin's end, the socket's writing half stays open, for the
//! callback replies only, until every tool call already sent is answered or cancelled: an act
//! already submitted is never left without its caller's agent.

use crate::client::{StartOptions, connect_or_start};
use crate::ctl::MAX_CTL_LINE_BYTES;
use crate::layout::RunDir;
use crate::session::Proto;
use crate::signing_agent::CallerAgent;
use crate::signing_transport::{
    CALLBACK_MARK, CallerLink, CallerSigner, MAX_CALLBACK_LINE_BYTES, copy_with_replies, watched,
    write_replies,
};
use crate::spawn::{serve_command, spawn_serve};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

/// How long, once the socket is gone, the proxy keeps reading what the host already wrote to its
/// stdin, so that those requests are answered too.
const STDIN_GRACE: Duration = Duration::from_millis(150);
/// How long, once a socket write has failed, the proxy waits for the replies the daemon wrote
/// before it went.
const REPLY_DRAIN: Duration = Duration::from_secs(2);
/// The longest daemon reply line the proxy buffers to see its id; a longer one is forwarded
/// unseen (and its request is then answered as gone if the daemon dies).
const MAX_TRACKED_REPLY_BYTES: usize = 16 * 1024 * 1024;

/// The ids of the JSON-RPC requests sent to the daemon and not yet answered, in send order.
#[derive(Debug, Default)]
pub struct InFlight(Vec<Value>);

fn object(line: &[u8]) -> Option<serde_json::Map<String, Value>> {
    match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// The id a response answers: a JSON object with an `id` and a `result` or an `error`.
fn answer_id(m: &Map<String, Value>) -> Option<&Value> {
    if !(m.contains_key("result") || m.contains_key("error")) {
        return None;
    }
    m.get("id")
}

impl InFlight {
    pub fn new() -> InFlight {
        InFlight::default()
    }

    /// Records the id of `line` when it is a request: a JSON object with a `method` and an `id`
    /// (a number or a string). Notifications, responses and anything unreadable are ignored.
    pub fn sent(&mut self, line: &[u8]) {
        if let Some(m) = object(line) {
            self.sent_object(&m);
        }
    }

    fn sent_object(&mut self, m: &Map<String, Value>) {
        if !m.contains_key("method") {
            return;
        }
        if let Some(id) = m.get("id")
            && (id.is_number() || id.is_string())
        {
            self.0.push(id.clone());
        }
    }

    /// Forgets the first recorded request that `line` answers: a JSON object with an `id` and a
    /// `result` or an `error`. Anything else is ignored.
    pub fn answered(&mut self, line: &[u8]) {
        if let Some(m) = object(line) {
            self.answered_object(&m);
        }
    }

    fn answered_object(&mut self, m: &Map<String, Value>) {
        if let Some(id) = answer_id(m)
            && let Some(i) = self.0.iter().position(|x| x == id)
        {
            self.0.remove(i);
        }
    }

    /// How many requests are unanswered.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One `E_DAEMON_GONE` error reply line (no newline) per unanswered request, in send order.
    pub fn gone_lines(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": "E_DAEMON_GONE: the daemon closed the connection before answering; a write may or may not have committed",
                        "data": {
                            "code": "E_DAEMON_GONE",
                            "fix": "check rdy status; a write may or may not have committed; it is not retried"
                        }
                    }
                })
                .to_string()
            })
            .collect()
    }
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the proxy tracks of the requests it forwarded.
#[derive(Default)]
struct Tracked {
    inflight: InFlight,
    /// The tool calls not yet answered or cancelled, in send order; the first is the active one.
    tools: VecDeque<Value>,
    /// The socket's reading half reached its end.
    socket_closed: bool,
}

impl Tracked {
    /// Records a line the host sent. A `notifications/cancelled` for the active tool call is
    /// true: its ask of the agent is to be cancelled.
    fn sent(&mut self, line: &[u8]) -> bool {
        let Some(m) = object(line) else { return false };
        self.inflight.sent_object(&m);
        let id = m.get("id").filter(|id| id.is_number() || id.is_string());
        match (m.get("method").and_then(Value::as_str), id) {
            (Some("tools/call"), Some(id)) => {
                self.tools.push_back(id.clone());
                false
            }
            (Some("notifications/cancelled"), None) => {
                let Some(target) = m.get("params").and_then(|p| p.get("requestId")) else {
                    return false;
                };
                let active = self.tools.front() == Some(target);
                // a cancelled call is not answered: it is no longer awaited
                self.tools.retain(|id| id != target);
                active
            }
            _ => false,
        }
    }

    /// Records a reply line the daemon sent.
    fn answered(&mut self, line: &[u8]) {
        let Some(m) = object(line) else { return };
        self.inflight.answered_object(&m);
        if let Some(id) = answer_id(&m)
            && let Some(i) = self.tools.iter().position(|x| x == id)
        {
            self.tools.remove(i);
        }
    }

    /// Whether no callback can come for a request already sent.
    fn settled(&self) -> bool {
        self.socket_closed || self.tools.is_empty()
    }
}

/// The state the proxy's tasks share.
struct Shared {
    tracked: Mutex<Tracked>,
    /// Bumped at every change of `tracked` that can settle it.
    changed: watch::Sender<u64>,
    /// Bumped to cancel the ask of the agent in flight.
    cancel: watch::Sender<u64>,
    /// True while the last byte written to stdout is not a newline.
    midline: AtomicBool,
}

impl Shared {
    fn bump(w: &watch::Sender<u64>) {
        w.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Waits until no callback can come for a request already sent.
    async fn settled(&self) {
        let mut changes = self.changed.subscribe();
        loop {
            if locked(&self.tracked).settled() {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Copies the socket's read half to `out`, flushing after each chunk, until the socket's EOF
/// (or an error either side). Each complete line it forwarded is recorded as answered. A
/// callback line is not forwarded: it goes, whole, to `callbacks` (one over its bound is
/// dropped).
async fn socket_to_stdout<R, O>(
    mut sock_r: R,
    out: Arc<tokio::sync::Mutex<O>>,
    shared: Arc<Shared>,
    callbacks: mpsc::UnboundedSender<Vec<u8>>,
) where
    R: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 64 * 1024];
    // the line being assembled; `None` while skipping an over-long one
    let mut partial: Option<Vec<u8>> = Some(Vec::new());
    // whether the line being read is a callback line; `None` before its first byte
    let mut callback: Option<bool> = None;
    loop {
        let n = match sock_r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut rest = &buf[..n];
        let mut forward: Vec<u8> = Vec::with_capacity(n);
        while let Some(&first) = rest.first() {
            let is_callback = *callback.get_or_insert(first == CALLBACK_MARK);
            let (seg, done) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => (&rest[..i], true),
                None => (rest, false),
            };
            let max = if is_callback {
                MAX_CALLBACK_LINE_BYTES + 1
            } else {
                MAX_TRACKED_REPLY_BYTES
            };
            if !is_callback {
                forward.extend_from_slice(&rest[..seg.len() + usize::from(done)]);
            }
            if let Some(p) = &mut partial {
                p.extend_from_slice(seg);
                if p.len() > max {
                    partial = None;
                }
            }
            if done {
                match (partial.take(), is_callback) {
                    (Some(mut p), true) => {
                        if p.last() == Some(&b'\r') {
                            p.pop();
                        }
                        if p.len() <= MAX_CALLBACK_LINE_BYTES {
                            let _ = callbacks.send(p);
                        }
                    }
                    (Some(p), false) => locked(&shared.tracked).answered(&p),
                    (None, _) => {}
                }
                partial = Some(Vec::new());
                callback = None;
                rest = &rest[seg.len() + 1..];
            } else {
                rest = &[];
            }
        }
        if !forward.is_empty() {
            let mut out = out.lock().await;
            if out.write_all(&forward).await.is_err() || out.flush().await.is_err() {
                return;
            }
            shared
                .midline
                .store(forward.last() != Some(&b'\n'), Ordering::SeqCst);
        }
        Shared::bump(&shared.changed);
    }
}

const GONE: &str = "rdy mcp: the daemon closed the connection (E_DAEMON_GONE); check rdy status; a write may or may not have committed";

/// Runs the proxy for `project` until stdin's EOF (then the daemon's session ends and the socket
/// drains: exit code 0) or until the daemon closes the connection first, or cannot be reached
/// (exit code 1, with the reason on stderr). The caller runs it on a current-thread runtime and
/// should not wait for that runtime's blocking pool at exit: a stdin read may still be pending.
/// The session's signing callbacks are answered with this process's own SSH agent.
pub async fn run_stdio(project: PathBuf, version: String, start_timeout: Duration) -> i32 {
    let spawn_project = project.clone();
    let opts = StartOptions {
        version,
        proto: Proto::Mcp,
        cwd: project.clone(),
        start_timeout,
        spawn: Box::new(move |run| {
            spawn_serve(&mut serve_command(
                &std::env::current_exe()?,
                &spawn_project,
                run,
            )?)
        }),
        progress: Box::new(|p| eprintln!("rdy mcp: starting: {p}")),
    };
    let client = match connect_or_start(&RunDir::of_project(&project), opts).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rdy mcp: {e}");
            return 1;
        }
    };
    run_proxy(
        tokio::io::stdin(),
        tokio::io::stdout(),
        client.into_stream(),
        Arc::new(CallerAgent::from_env()),
    )
    .await
}

/// The proxy over `socket`, a session past its hello: `input` to the socket, the socket to
/// `output`, and the session's signing callbacks answered with `agent`. Exit codes and stderr as
/// [`run_stdio`].
pub async fn run_proxy<I, O, S>(input: I, output: O, socket: S, agent: Arc<dyn CallerSigner>) -> i32
where
    I: AsyncRead + Send + Unpin + 'static,
    O: AsyncWrite + Send + Unpin + 'static,
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (sock_r, mut sock_w) = tokio::io::split(socket);
    let shared = Arc::new(Shared {
        tracked: Mutex::new(Tracked::default()),
        changed: watch::channel(0).0,
        cancel: watch::channel(0).0,
        midline: AtomicBool::new(false),
    });
    let output = Arc::new(tokio::sync::Mutex::new(output));
    let (replies_tx, mut replies) = mpsc::unbounded_channel::<Vec<u8>>();
    let (callbacks_tx, mut callbacks) = mpsc::unbounded_channel::<Vec<u8>>();
    // the callback answerer: one ask of the agent at a time, in the order the daemon asked
    let answerer = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut link = CallerLink::new(Some(agent), watched(&shared.cancel));
            while let Some(line) = callbacks.recv().await {
                let gone = {
                    let shared = Arc::clone(&shared);
                    async move {
                        let mut changes = shared.changed.subscribe();
                        while !locked(&shared.tracked).socket_closed {
                            if changes.changed().await.is_err() {
                                return;
                            }
                        }
                    }
                };
                if let Some(reply) = link.answer(&line, gone).await {
                    let _ = replies_tx.send(reply);
                }
            }
        })
    };
    let _answerer = AbortOnDrop(answerer);
    // stdin to the socket; the task's value is true when a socket write failed (the daemon is
    // gone); it ends only at stdin's EOF, after the write half is shut down
    let mut a = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let (mut to_writer, mut from_pump) = tokio::io::duplex(64 * 1024);
            let pump = {
                let shared = Arc::clone(&shared);
                async move {
                    let _ = rdy_mcp::server::pump_lines_with(
                        input,
                        &mut to_writer,
                        MAX_CTL_LINE_BYTES,
                        &mut |n, why| eprintln!("rdy mcp: dropped a {n}-byte request line: {why}"),
                        &mut |line| {
                            if locked(&shared.tracked).sent(line) {
                                Shared::bump(&shared.cancel);
                            }
                        },
                    )
                    .await;
                    let _ = to_writer.shutdown().await;
                }
            };
            let writer = async {
                let copied = copy_with_replies(&mut from_pump, &mut sock_w, &mut replies).await;
                let mut failed = copied.failed;
                if !failed && !locked(&shared.tracked).settled() {
                    // a request already sent may still need the caller's agent: the writing
                    // half stays open, for its callback replies, until none can come
                    if copied.midline {
                        failed = sock_w.write_all(b"\n").await.is_err();
                    }
                    failed =
                        write_replies(&mut sock_w, &mut replies, failed, shared.settled()).await;
                }
                let _ = sock_w.shutdown().await;
                failed
            };
            let ((), failed) = tokio::join!(pump, writer);
            failed
        })
    };
    let b = {
        let (output, shared) = (Arc::clone(&output), Arc::clone(&shared));
        async move {
            socket_to_stdout(sock_r, output, Arc::clone(&shared), callbacks_tx).await;
            locked(&shared.tracked).socket_closed = true;
            Shared::bump(&shared.changed);
        }
    };
    tokio::pin!(b);
    tokio::select! {
        biased;
        r = &mut a => match r {
            Ok(false) => {
                // the daemon's session ends at the EOF and closes the socket
                b.await;
                return 0;
            }
            // a write to the socket failed: the daemon is gone; take what it wrote before it went
            Ok(true) | Err(_) => {
                let _ = tokio::time::timeout(REPLY_DRAIN, &mut b).await;
            }
        },
        () = &mut b => {
            // let the pump record the requests the host has already written
            if tokio::time::timeout(STDIN_GRACE, &mut a).await.is_err() {
                a.abort();
            }
        }
    }
    answer_gone(&shared, &output).await;
    eprintln!("{GONE}");
    1
}

/// Aborts a spawned task when dropped.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Writes an `E_DAEMON_GONE` error reply to `output` for every request still unanswered, ending
/// a reply the daemon left half-written first.
async fn answer_gone<O: AsyncWrite + Unpin>(shared: &Shared, output: &tokio::sync::Mutex<O>) {
    let mut out = String::new();
    if shared.midline.load(Ordering::SeqCst) {
        out.push('\n');
    }
    for line in locked(&shared.tracked).inflight.gone_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    if out.is_empty() {
        return;
    }
    let mut stdout = output.lock().await;
    let _ = stdout.write_all(out.as_bytes()).await;
    let _ = stdout.flush().await;
}
