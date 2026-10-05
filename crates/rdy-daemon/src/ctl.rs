//! The `ctl` protocol: one JSON request per line, one JSON reply per line, in request order.

use crate::error::{E_BAD_INPUT, E_LIMIT, ErrorReply};
use crate::handler::Handler;
use crate::session::{SessionInfo, Sessions};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;

/// The longest request line served, without its newline. Equal to `rdy_mcp::tools::MAX_REQUEST_BYTES`.
pub const MAX_CTL_LINE_BYTES: usize = 16 * 1024 * 1024;

const E_INTERNAL: &str = "E_INTERNAL";

fn id_number_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    let v = Value::deserialize(d)?;
    if v.is_number() || v.is_string() {
        Ok(v)
    } else {
        Err(serde::de::Error::custom("id must be a number or a string"))
    }
}

fn empty_args() -> Value {
    json!({})
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
}

impl CtlReply {
    pub fn ok(id: Value, value: Value) -> Self {
        CtlReply {
            id,
            ok: Some(value),
            error: None,
        }
    }

    pub fn error(id: Value, error: ErrorReply) -> Self {
        CtlReply {
            id,
            ok: None,
            error: Some(error),
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
}

#[derive(Default)]
struct LineState {
    buf: Vec<u8>,
    over: bool,
}

/// Reads one line, never holding more than the limit (plus a read chunk). The only await is
/// `fill_buf`, so dropping this future loses nothing: a caller may select it against a signal.
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
            if std::mem::take(&mut st.over) {
                return Ok(Got::Over);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > max {
                return Ok(Got::Over);
            }
            return Ok(Got::Bytes(line));
        }
    }
}

async fn write_reply<W: AsyncWrite + Unpin>(w: &mut W, reply: &CtlReply) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(reply).map_err(io::Error::other)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
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
) -> CtlReply {
    let id = request.id.clone();
    let op = request.op.clone();
    let (handler, session) = (Arc::clone(handler), session.clone());
    let out = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler.ctl(&session, request)
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

/// The line loop for one ctl connection, from after the hello to the connection's end.
/// Returns on EOF, a read or write error, an oversized line, or once `info.stop` is set
/// (after the reply to the request in hand). The caller removes the session and calls
/// `Handler::session_ended`.
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
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let line = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            line = read_line(&mut r, &mut st, MAX_CTL_LINE_BYTES) => line?,
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
                return Ok(());
            }
            Got::Bytes(l) => l,
        };
        if line.is_empty() {
            continue;
        }
        let request: CtlRequest = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                let e = ErrorReply::new(
                    E_BAD_INPUT,
                    format!("a ctl request is one JSON object per line with id, op and args: {e}"),
                    r#"send {"id":1,"op":"ping","args":{}}"#,
                );
                write_reply(&mut w, &CtlReply::error(Value::Null, e)).await?;
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
                write_reply(&mut w, &CtlReply::ok(id, v)).await?;
            }
            "sessions" => {
                let v = serde_json::to_value(info.sessions.list()).map_err(io::Error::other)?;
                write_reply(&mut w, &CtlReply::ok(id, v)).await?;
            }
            "stop" => {
                write_reply(&mut w, &CtlReply::ok(id, json!({"stopping": true}))).await?;
                info.stop.send_replace(true);
            }
            _ => {
                let reply = call_handler(&handler, &session, request).await;
                write_reply(&mut w, &reply).await?;
            }
        }
    }
}
