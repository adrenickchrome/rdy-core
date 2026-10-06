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

use crate::client::{StartOptions, connect_or_start};
use crate::ctl::MAX_CTL_LINE_BYTES;
use crate::layout::RunDir;
use crate::session::Proto;
use crate::spawn::{serve_command, spawn_serve};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

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

impl InFlight {
    pub fn new() -> InFlight {
        InFlight::default()
    }

    /// Records the id of `line` when it is a request: a JSON object with a `method` and an `id`
    /// (a number or a string). Notifications, responses and anything unreadable are ignored.
    pub fn sent(&mut self, line: &[u8]) {
        let Some(mut m) = object(line) else { return };
        if !m.contains_key("method") {
            return;
        }
        if let Some(id) = m.remove("id")
            && (id.is_number() || id.is_string())
        {
            self.0.push(id);
        }
    }

    /// Forgets the first recorded request that `line` answers: a JSON object with an `id` and a
    /// `result` or an `error`. Anything else is ignored.
    pub fn answered(&mut self, line: &[u8]) {
        let Some(m) = object(line) else { return };
        if !(m.contains_key("result") || m.contains_key("error")) {
            return;
        }
        if let Some(id) = m.get("id")
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

fn locked(m: &Mutex<InFlight>) -> MutexGuard<'_, InFlight> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A socket write half that, once a write has failed, swallows every later write and flush: the
/// stdin pump then keeps reading (and recording) what the host sends, instead of stopping at the
/// first request the daemon cannot take.
struct Guarded<W> {
    inner: W,
    failed: bool,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for Guarded<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.failed {
            return Poll::Ready(Ok(buf.len()));
        }
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Err(_)) => {
                self.failed = true;
                Poll::Ready(Ok(buf.len()))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.failed {
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Err(_)) => {
                self.failed = true;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.failed {
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Ready(Err(_)) => {
                self.failed = true;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// Copies the socket's read half to stdout, flushing after each chunk, until the socket's EOF
/// (or an error either side). Each complete line it forwarded is shown to `inflight`; `midline`
/// is true while the last byte forwarded is not a newline.
async fn socket_to_stdout(
    mut sock_r: tokio::net::unix::OwnedReadHalf,
    inflight: Arc<Mutex<InFlight>>,
    midline: Arc<AtomicBool>,
) {
    let mut stdout = tokio::io::stdout();
    let mut buf = vec![0u8; 64 * 1024];
    // the line being assembled; `None` while skipping an over-long one
    let mut partial: Option<Vec<u8>> = Some(Vec::new());
    loop {
        let n = match sock_r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if stdout.write_all(&buf[..n]).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
        midline.store(buf[n - 1] != b'\n', Ordering::SeqCst);
        let mut rest = &buf[..n];
        while !rest.is_empty() {
            let (seg, done) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => (&rest[..i], true),
                None => (rest, false),
            };
            if let Some(p) = &mut partial {
                p.extend_from_slice(seg);
                if p.len() > MAX_TRACKED_REPLY_BYTES {
                    partial = None;
                }
            }
            if done {
                if let Some(p) = partial.take() {
                    locked(&inflight).answered(&p);
                }
                partial = Some(Vec::new());
                rest = &rest[seg.len() + 1..];
            } else {
                rest = &[];
            }
        }
    }
}

const GONE: &str = "rdy mcp: the daemon closed the connection (E_DAEMON_GONE); check rdy status; a write may or may not have committed";

/// Runs the proxy for `project` until stdin's EOF (then the daemon's session ends and the socket
/// drains: exit code 0) or until the daemon closes the connection first, or cannot be reached
/// (exit code 1, with the reason on stderr). The caller runs it on a current-thread runtime and
/// should not wait for that runtime's blocking pool at exit: a stdin read may still be pending.
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
    let (sock_r, sock_w) = client.into_stream().into_split();
    let inflight = Arc::new(Mutex::new(InFlight::new()));
    let midline = Arc::new(AtomicBool::new(false));
    // stdin to the socket; the task's value is true when a socket write failed (the daemon is
    // gone); it ends only at stdin's EOF, after the write half is shut down
    let mut a = {
        let inflight = inflight.clone();
        tokio::spawn(async move {
            let mut w = Guarded {
                inner: sock_w,
                failed: false,
            };
            let _ = rdy_mcp::server::pump_lines_with(
                tokio::io::stdin(),
                &mut w,
                MAX_CTL_LINE_BYTES,
                &mut |n, why| eprintln!("rdy mcp: dropped a {n}-byte request line: {why}"),
                &mut |line| locked(&inflight).sent(line),
            )
            .await;
            let _ = w.shutdown().await;
            w.failed
        })
    };
    let b = socket_to_stdout(sock_r, inflight.clone(), midline.clone());
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
    answer_gone(&inflight, &midline).await;
    eprintln!("{GONE}");
    1
}

/// Writes an `E_DAEMON_GONE` error reply to stdout for every request still unanswered, ending a
/// reply the daemon left half-written first.
async fn answer_gone(inflight: &Mutex<InFlight>, midline: &AtomicBool) {
    let mut out = String::new();
    if midline.load(Ordering::SeqCst) {
        out.push('\n');
    }
    for line in locked(inflight).gone_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    if out.is_empty() {
        return;
    }
    let mut stdout = tokio::io::stdout();
    let _ = stdout.write_all(out.as_bytes()).await;
    let _ = stdout.flush().await;
}
