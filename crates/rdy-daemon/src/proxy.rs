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
//! them to stdout. A callback belongs to the request it names (its `request`, the MCP request
//! id of the tool call the daemon serves), never to whichever request is oldest. Each tool call
//! not yet answered or cancelled has its own latched cancel, kept by its id: the host's
//! `notifications/cancelled` for it cancels its ask of the agent, whether the ask is in flight,
//! still waiting for a thread, or yet to come; one for another request id cancels nothing of
//! it. A callback whose request is cancelled, answered or unknown is refused at once as
//! `request cancelled`, and the agent is not asked. (The daemon refuses the callbacks of a
//! cancelled request on its side too.) Replies are followed as they
//! stream, holding no more than their id, so a reply of any length settles its request, and so
//! does a reply to a request with an id of any length the request line's bound allows.
//!
//! At stdin's end, the socket's writing half stays open, for the callback replies only, until
//! every tool call already sent is answered or cancelled: an act already submitted is never left
//! without its caller's agent. Whenever the proxy exits, it stops the ask in flight and waits
//! for its signing worker first: no worker outlives the proxy.

use crate::client::{StartOptions, connect_or_start};
use crate::ctl::MAX_CTL_LINE_BYTES;
use crate::layout::RunDir;
use crate::session::Proto;
use crate::signing_agent::CallerAgent;
use crate::signing_transport::{
    CALLBACK_MARK, CallerLink, CallerSigner, MAX_CALLBACK_LINE_BYTES, Stop, callback_id,
    copy_with_replies, never_interrupted, write_replies,
};
use crate::spawn::{serve_command, spawn_serve};
use rdy_engine::signing_crypto::CallCancel;
use rdy_mcp::server::LineScan;
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
        if let Some(id) = answer_id(m) {
            self.answered_id(id);
        }
    }

    /// Forgets the first recorded request with the id `id`.
    fn answered_id(&mut self, id: &Value) {
        if let Some(i) = self.0.iter().position(|x| x == id) {
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
    /// The tool calls not yet answered or cancelled, in send order, each with its cancel, found
    /// by its request id.
    tools: VecDeque<(Value, Arc<CallCancel>)>,
    /// The socket's reading half reached its end.
    socket_closed: bool,
    /// The proxy shuts the socket's writing half down at stdin's EOF, with no write failed, while
    /// the socket was still open: the socket's end that follows is the daemon's answer to it.
    stdin_closing: bool,
}

impl Tracked {
    /// Records a line the host sent. For a `notifications/cancelled` of a tool call still
    /// awaited, the call is forgotten and its cancel returned, to be cancelled.
    fn sent(&mut self, line: &[u8]) -> Option<Arc<CallCancel>> {
        let m = object(line)?;
        self.inflight.sent_object(&m);
        let id = m.get("id").filter(|id| id.is_number() || id.is_string());
        match (m.get("method").and_then(Value::as_str), id) {
            (Some("tools/call"), Some(id)) => {
                self.tools
                    .push_back((id.clone(), Arc::new(CallCancel::new())));
                None
            }
            (Some("notifications/cancelled"), None) => {
                let target = m.get("params").and_then(|p| p.get("requestId"))?;
                let i = self.tools.iter().position(|(id, _)| id == target)?;
                // a cancelled call is not answered: it is no longer awaited
                self.tools.remove(i).map(|(_, cancel)| cancel)
            }
            _ => None,
        }
    }

    /// Records that the daemon answered the request `id`.
    fn answered(&mut self, id: &Value) {
        self.inflight.answered_id(id);
        if let Some(i) = self.tools.iter().position(|(x, _)| x == id) {
            self.tools.remove(i);
        }
    }

    /// The cancel of the tool call `request`, while it is awaited: `None` once it is answered
    /// or cancelled, and for an id the proxy never sent as a tool call.
    fn cancel_of(&self, request: &Value) -> Option<Arc<CallCancel>> {
        self.tools
            .iter()
            .find(|(id, _)| id == request)
            .map(|(_, cancel)| Arc::clone(cancel))
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
    /// Set once a callback line broke the framing: the ask in flight stops, the link closes.
    invalid: watch::Sender<bool>,
    /// Set when the proxy exits: the ask in flight stops and the answerer ends.
    stop: watch::Sender<bool>,
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

    /// Ends when the ask of the agent in flight must stop: [`Stop::Gone`] at the socket's end
    /// or the proxy's exit, [`Stop::Invalid`] once a callback line broke the framing.
    async fn stopped(&self) -> Stop {
        let mut changes = self.changed.subscribe();
        let mut stop = self.stop.subscribe();
        let mut invalid = self.invalid.subscribe();
        loop {
            if *stop.borrow_and_update() || locked(&self.tracked).socket_closed {
                return Stop::Gone;
            }
            if *invalid.borrow_and_update() {
                return Stop::Invalid;
            }
            // the senders live in `self`: none of these ends with an error
            tokio::select! {
                _ = changes.changed() => {}
                _ = stop.changed() => {}
                _ = invalid.changed() => {}
            }
        }
    }
}

/// What the socket reader hands the callback answerer.
enum Inbound {
    /// A callback line.
    Line(Vec<u8>),
    /// A callback line over its bound, read off and dropped, with its id when one was read.
    Over(Option<u64>),
}

/// The line the socket reader is in.
enum Reading {
    /// An ordinary line: forwarded as it comes, its id followed as it streams.
    Ordinary(LineScan),
    /// A callback line, assembled up to its bound (`over` past it), its id followed too.
    Callback {
        bytes: Vec<u8>,
        scan: LineScan,
        over: bool,
    },
}

/// Copies the socket's read half to `out`, flushing after each chunk, until the socket's EOF
/// (or an error either side). Each reply it forwarded is recorded as answered, whatever its
/// length. A callback line is not forwarded: it goes, whole, to `callbacks`; one over its bound
/// fails the signing channel at once (the ask in flight stops) and goes as [`Inbound::Over`].
async fn socket_to_stdout<R, O>(
    mut sock_r: R,
    out: Arc<tokio::sync::Mutex<O>>,
    shared: Arc<Shared>,
    callbacks: mpsc::UnboundedSender<Inbound>,
) where
    R: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 64 * 1024];
    // `None` before a line's first byte
    let mut reading: Option<Reading> = None;
    loop {
        let n = match sock_r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut rest = &buf[..n];
        let mut forward: Vec<u8> = Vec::with_capacity(n);
        while let Some(&first) = rest.first() {
            let line = reading.get_or_insert_with(|| {
                if first == CALLBACK_MARK {
                    Reading::Callback {
                        bytes: Vec::new(),
                        scan: LineScan::new(),
                        over: false,
                    }
                } else {
                    // the reply's id is matched to a request the pump forwarded, of any id
                    // length: the scan keeps an id up to the request lines' own bound
                    Reading::Ordinary(LineScan::with_id_limit(MAX_CTL_LINE_BYTES))
                }
            });
            let (seg, done) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => (&rest[..i], true),
                None => (rest, false),
            };
            match line {
                Reading::Ordinary(scan) => {
                    forward.extend_from_slice(&rest[..seg.len() + usize::from(done)]);
                    scan.feed(seg);
                }
                Reading::Callback { bytes, scan, over } => {
                    scan.feed(seg);
                    // room for a trailing \r, which does not count
                    if !*over && bytes.len() + seg.len() > MAX_CALLBACK_LINE_BYTES + 1 {
                        *over = true;
                        *bytes = Vec::new();
                    } else if !*over {
                        bytes.extend_from_slice(seg);
                    }
                }
            }
            if !done {
                break;
            }
            match reading.take() {
                Some(Reading::Ordinary(scan)) => {
                    let line = scan.finish();
                    if line.response
                        && let Some(id) = line.id
                    {
                        locked(&shared.tracked).answered(&id);
                    }
                }
                Some(Reading::Callback {
                    mut bytes,
                    scan,
                    over,
                }) => {
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                    if over || bytes.len() > MAX_CALLBACK_LINE_BYTES {
                        shared.invalid.send_replace(true);
                        let _ =
                            callbacks.send(Inbound::Over(callback_id(scan.finish().id.as_ref())));
                    } else {
                        let _ = callbacks.send(Inbound::Line(bytes));
                    }
                }
                None => {}
            }
            rest = &rest[seg.len() + 1..];
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

/// Answers the session's callbacks with `agent`, one at a time, in the order the daemon asked,
/// until the proxy stops. Each is answered under the cancel of the tool call it names; one of a
/// request no longer awaited (cancelled, answered or unknown) is refused as `request cancelled`
/// without asking the agent; an oversized one closes the link.
async fn answer_callbacks(
    agent: Arc<dyn CallerSigner>,
    shared: Arc<Shared>,
    mut callbacks: mpsc::UnboundedReceiver<Inbound>,
    replies: mpsc::UnboundedSender<Vec<u8>>,
) {
    let mut link = CallerLink::new(Some(agent), never_interrupted());
    let mut stopping = shared.stop.subscribe();
    loop {
        let inbound = tokio::select! {
            biased;
            _ = stopping.wait_for(|stop| *stop) => return,
            inbound = callbacks.recv() => match inbound {
                Some(inbound) => inbound,
                None => return,
            },
        };
        let reply = match inbound {
            Inbound::Over(id) => Some(link.refuse(id)),
            Inbound::Line(line) => {
                let cancel_of = |request: &Value| locked(&shared.tracked).cancel_of(request);
                link.answer(&line, cancel_of, shared.stopped()).await
            }
        };
        if let Some(reply) = reply {
            let _ = replies.send(reply);
        }
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
        invalid: watch::channel(false).0,
        stop: watch::channel(false).0,
        midline: AtomicBool::new(false),
    });
    let output = Arc::new(tokio::sync::Mutex::new(output));
    let (replies_tx, mut replies) = mpsc::unbounded_channel::<Vec<u8>>();
    let (callbacks_tx, callbacks) = mpsc::unbounded_channel::<Inbound>();
    let answerer = tokio::spawn(answer_callbacks(
        agent,
        Arc::clone(&shared),
        callbacks,
        replies_tx,
    ));
    // dropped before its end, the proxy still stops the ask in flight
    let _stop = StopOnDrop(Arc::clone(&shared));
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
                            let cancel = locked(&shared.tracked).sent(line);
                            if let Some(cancel) = cancel {
                                cancel.cancel();
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
                if !failed {
                    // a drain that settled at the socket's end is no orderly close
                    let mut tracked = locked(&shared.tracked);
                    if !tracked.socket_closed {
                        tracked.stdin_closing = true;
                    }
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
    let gone = tokio::select! {
        biased;
        r = &mut a => match r {
            Ok(false) => {
                // the daemon's session ends at the EOF and closes the socket
                b.await;
                false
            }
            // a write to the socket failed: the daemon is gone; take what it wrote before it went
            Ok(true) | Err(_) => {
                let _ = tokio::time::timeout(REPLY_DRAIN, &mut b).await;
                true
            }
        },
        () = &mut b => {
            let stdin_closing = locked(&shared.tracked).stdin_closing;
            if stdin_closing {
                // the socket ended after the proxy's own half-close: the stdin task is
                // completing, and its value says whether a write failed
                !matches!(a.await, Ok(false))
            } else {
                // let the pump record the requests the host has already written
                if tokio::time::timeout(STDIN_GRACE, &mut a).await.is_err() {
                    a.abort();
                }
                true
            }
        }
    };
    if gone {
        answer_gone(&shared, &output).await;
    }
    // the output ends, then the ask in flight is stopped and its worker waited for: no signing
    // worker outlives the proxy
    let _ = output.lock().await.shutdown().await;
    shared.stop.send_replace(true);
    let _ = answerer.await;
    if gone {
        eprintln!("{GONE}");
        return 1;
    }
    0
}

/// Stops the proxy's answerer, and the ask in flight, when dropped.
struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.send_replace(true);
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
