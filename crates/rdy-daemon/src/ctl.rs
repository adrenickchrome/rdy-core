//! The `ctl` protocol: one JSON request per line, one JSON reply per line, in request order.

use crate::daemon::DRAIN_AFTER_REPLY_BYTES;
use crate::error::{E_BAD_INPUT, E_LIMIT, ErrorReply};
use crate::handler::Handler;
use crate::session::{SessionInfo, Sessions};
use crate::signing_transport::{CALLBACK_MARK, HostInbound, MAX_CALLBACK_LINE_BYTES, host_link};
use rdy_engine::signing_crypto::SigningAgent;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;

/// The longest request line served, without its newline. Equal to `rdy_mcp::tools::MAX_REQUEST_BYTES`.
pub const MAX_CTL_LINE_BYTES: usize = rdy_mcp::tools::MAX_REQUEST_BYTES;

const E_INTERNAL: &str = "E_INTERNAL";

fn id_number_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    let v = Value::deserialize(d)?;
    if v.is_number() || v.is_string() {
        Ok(v)
    } else {
        Err(serde::de::Error::custom("id must be a number or a string"))
    }
}

/// The id of a request line that did not parse, read leniently: a number or a string, else null.
fn request_id(line: &[u8]) -> Value {
    match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(o)) => o
            .get("id")
            .filter(|v| v.is_number() || v.is_string())
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn empty_args() -> Value {
    json!({})
}

/// FW-HD1: a request refuses a key it does not take.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtlRequest {
    #[serde(deserialize_with = "id_number_or_string")]
    pub id: Value,
    pub op: String,
    #[serde(default = "empty_args")]
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CtlReply {
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReply>,
    /// What the session should know beside this reply (`pack_changed`, `pack_error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl CtlReply {
    pub fn ok(id: Value, value: Value) -> Self {
        CtlReply {
            id,
            ok: Some(value),
            error: None,
            notice: None,
        }
    }

    pub fn error(id: Value, error: ErrorReply) -> Self {
        CtlReply {
            id,
            ok: None,
            error: Some(error),
            notice: None,
        }
    }
}

/// What the built-in ops report, and the daemon's one stop signal.
#[derive(Clone)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub started_at: Instant,
    pub sessions: Sessions,
    pub connections: Arc<AtomicUsize>,
    pub stop: Arc<watch::Sender<bool>>,
}

/// One line read off the connection.
enum Got {
    Eof,
    Bytes(Vec<u8>),
    /// Longer than the limit; its bytes were drained, not kept.
    Over,
    /// A callback line (mark kept), for the connection's signing channel.
    Callback(Vec<u8>),
    /// A callback line longer than its limit; drained.
    CallbackOver,
}

#[derive(Default)]
struct LineState {
    buf: Vec<u8>,
    over: bool,
    /// Whether the line being read is a callback line; `None` before its first byte.
    callback: Option<bool>,
}

/// Reads one line, never holding more than the limit (plus a read chunk): `max` for an ordinary
/// line, [`MAX_CALLBACK_LINE_BYTES`] for a callback line, which its first byte names. The only
/// await is `fill_buf`, so dropping this future loses nothing: a caller may select it against a
/// signal.
async fn read_line<R: AsyncRead + Unpin>(
    r: &mut BufReader<R>,
    st: &mut LineState,
    max: usize,
) -> io::Result<Got> {
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            // EOF; a partial line is dropped
            return Ok(Got::Eof);
        }
        let callback = *st.callback.get_or_insert(buf[0] == CALLBACK_MARK);
        let max = if callback {
            MAX_CALLBACK_LINE_BYTES
        } else {
            max
        };
        let (chunk, done, consumed) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], true, i + 1),
            None => (buf, false, buf.len()),
        };
        if !st.over {
            st.buf.extend_from_slice(chunk);
            // room for a trailing \r, which is stripped before the limit applies
            if st.buf.len() > max + 1 {
                st.over = true;
                st.buf = Vec::new();
            }
        }
        r.consume(consumed);
        if done {
            let mut line = std::mem::take(&mut st.buf);
            st.callback = None;
            let over = std::mem::take(&mut st.over);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(match (callback, over || line.len() > max) {
                (false, true) => Got::Over,
                (false, false) => Got::Bytes(line),
                (true, true) => Got::CallbackOver,
                (true, false) => Got::Callback(line),
            });
        }
    }
}

/// Reads off and drops, without waiting, input that has already arrived, up to about `max`
/// bytes. Closing a Unix socket with unread input makes Linux report ECONNRESET to the peer,
/// which could hide the reply it is about to read.
async fn drain_ready<R: AsyncRead + Unpin>(r: &mut BufReader<R>, max: usize) {
    let mut drained = 0;
    while drained < max {
        // one poll: what is buffered or ready now, never a wait for more
        let n = std::future::poll_fn(|cx| match Pin::new(&mut *r).poll_fill_buf(cx) {
            Poll::Ready(Ok(buf)) => Poll::Ready(buf.len()),
            Poll::Ready(Err(_)) | Poll::Pending => Poll::Ready(0),
        })
        .await;
        if n == 0 {
            break;
        }
        r.consume(n);
        drained += n;
    }
}

async fn write_reply<W: AsyncWrite + Unpin>(w: &mut W, reply: &CtlReply) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(reply).map_err(io::Error::other)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Writes a reply to a request of `session`, carrying the handler's notice for it.
async fn answer<W: AsyncWrite + Unpin>(
    w: &mut W,
    handler: &Arc<dyn Handler>,
    session: &SessionInfo,
    mut reply: CtlReply,
) -> io::Result<()> {
    reply.notice = handler.notice(session);
    write_reply(w, &reply).await
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Runs the handler on the blocking pool; a panic becomes an `E_INTERNAL` reply.
async fn call_handler(
    handler: &Arc<dyn Handler>,
    session: &SessionInfo,
    request: CtlRequest,
    agent: Arc<dyn SigningAgent>,
) -> CtlReply {
    let id = request.id.clone();
    let op = request.op.clone();
    let (handler, session) = (Arc::clone(handler), session.clone());
    let out = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler.ctl(&session, request, agent)
        }))
    })
    .await;
    let internal = |text: String| {
        ErrorReply::new(
            E_INTERNAL,
            format!("internal error in {op}: {text}"),
            "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
        )
    };
    match out {
        Ok(Ok(Ok(v))) => CtlReply::ok(id, v),
        Ok(Ok(Err(e))) => CtlReply::error(id, e),
        Ok(Err(panic)) => CtlReply::error(id, internal(panic_text(panic.as_ref()))),
        Err(join) => CtlReply::error(id, internal(join.to_string())),
    }
}

/// Lines read while a request ran, kept for after its reply.
#[derive(Default)]
struct Held {
    lines: VecDeque<Got>,
    /// The bytes of the kept request lines.
    bytes: usize,
}

impl Held {
    fn push(&mut self, got: Got) {
        if let Got::Bytes(l) = &got {
            self.bytes += l.len();
        }
        self.lines.push_back(got);
    }

    fn pop(&mut self) -> Option<Got> {
        let got = self.lines.pop_front()?;
        if let Got::Bytes(l) = &got {
            self.bytes -= l.len();
        }
        Some(got)
    }
}

/// The line loop for one ctl connection, from after the hello to the connection's end.
/// Returns on EOF, a read or write error, an oversized line, or once `info.stop` is set
/// (after the reply to the request in hand). The caller removes the session and calls
/// `Handler::session_ended`.
///
/// Callback lines go to the connection's signing channel, never to the request parser. While a
/// request runs, the loop writes the callback requests its agent makes and, while one waits for
/// its reply, reads on: callback replies are taken at once, and other lines are kept, in order,
/// for after the request's reply.
pub async fn serve_ctl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: R,
    mut w: W,
    session: SessionInfo,
    handler: Arc<dyn Handler>,
    info: DaemonInfo,
) -> io::Result<()> {
    let mut r = BufReader::new(r);
    let mut st = LineState::default();
    let mut stop = info.stop.subscribe();
    let (agent, inbound, mut outbound) = host_link();
    let agent: Arc<dyn SigningAgent> = agent;
    let mut held = Held::default();
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let line = match held.pop() {
            Some(line) => line,
            None => tokio::select! {
                biased;
                _ = stop.changed() => return Ok(()),
                line = read_line(&mut r, &mut st, MAX_CTL_LINE_BYTES) => line?,
            },
        };
        let line = match line {
            Got::Eof => return Ok(()),
            Got::Over => {
                let e = ErrorReply::new(
                    E_LIMIT,
                    "request line is over 16 MiB",
                    "send a smaller request",
                );
                write_reply(&mut w, &CtlReply::error(Value::Null, e)).await?;
                // the caller closes the connection: read off what the client already sent
                // first, as `close_after_reply` does for a refused hello
                let _ = w.shutdown().await;
                drain_ready(&mut r, DRAIN_AFTER_REPLY_BYTES).await;
                return Ok(());
            }
            Got::Callback(l) => {
                inbound.deliver(&l);
                continue;
            }
            Got::CallbackOver => {
                inbound.over();
                continue;
            }
            Got::Bytes(l) => l,
        };
        if line.is_empty() {
            continue;
        }
        let request: CtlRequest = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                let e = match rdy_engine::did_you_mean::unknown_field_error("ctl request", &msg) {
                    Some(e) => ErrorReply::new(e.code, e.message, e.fix),
                    None => ErrorReply::new(
                        E_BAD_INPUT,
                        format!(
                            "a ctl request is one JSON object per line with id, op and args: {msg}"
                        ),
                        r#"send {"id":1,"op":"ping","args":{}}"#,
                    ),
                };
                write_reply(&mut w, &CtlReply::error(request_id(&line), e)).await?;
                continue;
            }
        };
        let id = request.id.clone();
        match request.op.as_str() {
            "ping" => {
                let v = json!({
                    "pid": info.pid,
                    "version": info.version,
                    "sessions": info.sessions.len(),
                    "connections": info.connections.load(Ordering::SeqCst),
                    "uptime_ms": info.started_at.elapsed().as_millis() as u64,
                });
                answer(&mut w, &handler, &session, CtlReply::ok(id, v)).await?;
            }
            "sessions" => {
                let v = serde_json::to_value(info.sessions.list()).map_err(io::Error::other)?;
                answer(&mut w, &handler, &session, CtlReply::ok(id, v)).await?;
            }
            "stop" => {
                let reply = CtlReply::ok(id, json!({"stopping": true}));
                answer(&mut w, &handler, &session, reply).await?;
                info.stop.send_replace(true);
            }
            _ => {
                let call = call_handler(&handler, &session, request, Arc::clone(&agent));
                let reply = run_request(
                    call,
                    &mut r,
                    &mut st,
                    &mut w,
                    &inbound,
                    &mut outbound,
                    &mut held,
                )
                .await?;
                answer(&mut w, &handler, &session, reply).await?;
            }
        }
    }
}

/// Waits for the request `call`, writing the callback requests its agent makes and reading the
/// replies to them. Lines that are not callback replies are kept in `held`; past
/// [`crate::signing_transport::MAX_READ_AHEAD_BYTES`] of them the waiting callback fails, and
/// nothing more is read until the request ends. A write that fails closes the signing channel;
/// the error is returned once the request has ended.
async fn run_request<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    call: impl std::future::Future<Output = CtlReply>,
    r: &mut BufReader<R>,
    st: &mut LineState,
    w: &mut W,
    inbound: &HostInbound,
    outbound: &mut crate::signing_transport::HostOutbound,
    held: &mut Held,
) -> io::Result<CtlReply> {
    tokio::pin!(call);
    let mut reading = true;
    let mut failed: Option<io::Error> = None;
    loop {
        let read = reading && inbound.outstanding();
        tokio::select! {
            biased;
            reply = &mut call => return match failed {
                Some(e) => Err(e),
                None => Ok(reply),
            },
            line = outbound.next(), if failed.is_none() => {
                let wrote = async {
                    w.write_all(&line).await?;
                    w.flush().await
                };
                if let Err(e) = wrote.await {
                    inbound.close();
                    failed = Some(e);
                }
            }
            got = read_line(r, st, MAX_CTL_LINE_BYTES), if read => match got {
                Ok(Got::Callback(l)) => inbound.deliver(&l),
                Ok(Got::CallbackOver) => inbound.over(),
                Ok(Got::Eof) | Err(_) => {
                    inbound.close();
                    reading = false;
                    held.push(Got::Eof);
                }
                Ok(other) => {
                    held.push(other);
                    if held.bytes > crate::signing_transport::MAX_READ_AHEAD_BYTES {
                        inbound.flooded();
                        reading = false;
                    }
                }
            },
        }
    }
}
