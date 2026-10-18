//! Signing through the caller's own agent, over the caller's own daemon connection.
//!
//! The daemon never reads `SSH_AUTH_SOCK`. When a request needs a signature, the host asks the
//! process at the other end of that request's connection (the CLI's [`crate::Client`], or the
//! MCP proxy), which asks its own [`CallerAgent`](crate::signing_agent::CallerAgent). The ask
//! travels on the same connection, after the same-version hello, as an internal callback line:
//! byte `0x1E`, one JSON object, then LF. A legal CLI or MCP JSON line cannot begin with that
//! byte, so callback lines are taken out before the ctl and MCP parsers see the stream, and they
//! never reach MCP stdout, diagnostics or the request-order tracker.
//!
//! The host's side is a [`RemoteAgent`]: a [`SigningAgent`] that a request body calls on the
//! blocking pool. It writes one callback request and waits on an owned channel for the reply,
//! which the connection's pump hands over. The pump keeps reading while the body waits, so the
//! writer and every other connection go on. One callback is outstanding per connection.
//!
//! The callback schemas, with unknown fields refused:
//!
//! - `{"id":N,"request":R,"op":"identities"}` and
//!   `{"id":N,"request":R,"op":"sign","key":"<hex>","payload":"<hex>"}`, from the host;
//! - `{"id":N,"request":R,"ok":{"keys":["<hex>"]}}`,
//!   `{"id":N,"request":R,"ok":{"signature":"<hex>"}}` and
//!   `{"id":N,"request":R,"error":{"code":"E_NO_AGENT","cause":"<closed cause>"}}`, from the
//!   caller.
//!
//! `R` is the id of the request the host is serving, exactly as its client sent it: the MCP
//! request id of a tool call, or the ctl request's id (`null` for an ask outside any request).
//! The caller's reply echoes it, and the caller answers a callback for the request it names,
//! never for whichever request happens to be oldest: the MCP proxy refuses the callback of a
//! request it no longer awaits (cancelled, answered or unknown) as `request cancelled`, without
//! asking its agent. Hex is lowercase. `N` is a nonzero u64; it starts at 1 on each connection and increases
//! without wrapping. A callback line has at most [`MAX_CALLBACK_LINE_BYTES`] bytes, checked
//! before any JSON is read; decoded content has the agent bounds of
//! [`rdy_engine::signing_crypto`]. An invalid callback reply closes the connection's signing
//! channel: the waiting call, and every later one, is `E_NO_AGENT` `invalid reply`. Losing the
//! connection wakes the waiter as `connection failed`.
//!
//! The caller's side ([`CallerLink`]) signs only an SSHSIG signed-data payload of one of the
//! four rdy namespaces, with empty reserved data and sha512, by a key of an accepted type: it is
//! not a general signing service for the MCP client.

use crate::handler::McpStream;
use crate::signing_agent::CallerAgent;
use rdy_engine::signing_crypto::{
    AgentIdentity, CallCancel, IDENTITIES_MAX, KEY_TYPES, NAMESPACES, PUBLIC_KEY_MAX, SSHSIG_MAX,
    STATEMENT_MAX, SigningAgent, SigningCall,
};
use rdy_engine::{E_NO_AGENT, NoAgentCause, RdyError, no_agent};
use rdy_mcp::server::LineScan;
use serde_json::Value;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

/// The first byte of a callback line.
pub const CALLBACK_MARK: u8 = 0x1E;
/// The longest callback line, mark included, newline excluded.
pub const MAX_CALLBACK_LINE_BYTES: usize = 1_048_576;
/// How many bytes of ordinary input a pump reads ahead, past what its reader has taken, while it
/// waits for a callback reply. More than that, and the callback fails as `invalid reply`, so the
/// pump never buffers without bound and never waits on a reader that waits on it.
pub const MAX_READ_AHEAD_BYTES: usize = 2 * rdy_mcp::tools::MAX_REQUEST_BYTES;

/// The buffer of an in-memory pipe between a pump and the stream it serves.
const PIPE_BYTES: usize = 1 << 16;
/// The ordinary bytes a pump reads ahead when no callback is outstanding.
const SOFT_READ_AHEAD_BYTES: usize = 1 << 16;
/// The SSHSIG preamble of signed data.
const SSHSIG_MAGIC: &[u8] = b"SSHSIG";
/// The one hash rdy signs with.
const SHA512: &[u8] = b"sha512";
/// The length of a sha512 digest.
const SHA512_LEN: usize = 64;

/// The caller's signer: its own agent. Each call is bound to a [`CallCancel`], which ends it as
/// `E_NO_AGENT` `request cancelled` whenever it comes, before the call starts included (a worker
/// still waiting for a thread never reaches the agent).
pub trait CallerSigner: Send + Sync {
    fn identities_until(&self, cancel: &CallCancel) -> Result<Vec<AgentIdentity>, RdyError>;

    fn sign_until(
        &self,
        public_key: &[u8],
        payload: &[u8],
        cancel: &CallCancel,
    ) -> Result<Vec<u8>, RdyError>;
}

impl CallerSigner for CallerAgent {
    fn identities_until(&self, cancel: &CallCancel) -> Result<Vec<AgentIdentity>, RdyError> {
        CallerAgent::identities_until(self, cancel)
    }

    fn sign_until(
        &self,
        public_key: &[u8],
        payload: &[u8],
        cancel: &CallCancel,
    ) -> Result<Vec<u8>, RdyError> {
        CallerAgent::sign_until(self, public_key, payload, cancel)
    }
}

/// The signer of a run with no daemon (the CLI's `--no-daemon`, `mcp --no-daemon`): this
/// process's own agent, from its own environment.
pub fn direct_agent() -> Arc<CallerAgent> {
    Arc::new(CallerAgent::from_env())
}

// ---- the wire ----

/// The lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// The bytes of lowercase hex `s`, when it is that and decodes to at most `max` bytes. The
/// length is checked before anything is allocated.
fn unhex(s: &str, max: usize) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(2) || s.len() / 2 > max {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    s.as_chunks::<2>()
        .0
        .iter()
        .map(|&[hi, lo]| Some(digit(hi)? << 4 | digit(lo)?))
        .collect()
}

/// `{mark}{json}\n`.
fn callback_line(json: &str) -> Vec<u8> {
    let mut line = Vec::with_capacity(json.len() + 2);
    line.push(CALLBACK_MARK);
    line.extend_from_slice(json.as_bytes());
    line.push(b'\n');
    line
}

/// A callback id: a nonzero u64.
pub(crate) fn callback_id(v: Option<&Value>) -> Option<u64> {
    v?.as_u64().filter(|&id| id != 0)
}

/// What the host asks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    Identities,
    Sign { key: Vec<u8>, payload: Vec<u8> },
}

/// The JSON of a request id, for a callback line.
fn request_json(request: &Value) -> String {
    serde_json::to_string(request).unwrap_or_else(|_| "null".into())
}

impl Ask {
    fn line(&self, id: u64, request: &Value) -> Vec<u8> {
        let request = request_json(request);
        match self {
            Ask::Identities => callback_line(&format!(
                r#"{{"id":{id},"request":{request},"op":"identities"}}"#
            )),
            Ask::Sign { key, payload } => callback_line(&format!(
                r#"{{"id":{id},"request":{request},"op":"sign","key":"{}","payload":"{}"}}"#,
                hex(key),
                hex(payload)
            )),
        }
    }
}

/// A host's callback request: its id, the request it serves and what it asks.
struct Parsed {
    id: u64,
    request: Value,
    ask: Ask,
}

/// A host's callback request body. `Err` carries the id when one could be read (the caller then
/// answers `invalid reply`).
fn parse_ask(body: &[u8]) -> Result<Parsed, Option<u64>> {
    let Ok(Value::Object(mut m)) = serde_json::from_slice::<Value>(body) else {
        return Err(None);
    };
    let id = callback_id(m.get("id")).ok_or(None)?;
    let bad = Err(Some(id));
    let Some(request) = m.remove("request") else {
        return bad;
    };
    let Some(op) = m.get("op").and_then(Value::as_str) else {
        return bad;
    };
    let parsed = |ask| {
        Ok(Parsed {
            id,
            request: request.clone(),
            ask,
        })
    };
    match op {
        "identities" if m.len() == 2 => parsed(Ask::Identities),
        "sign" if m.len() == 4 => {
            let field =
                |k: &str, max: usize| m.get(k).and_then(Value::as_str).and_then(|s| unhex(s, max));
            match (
                field("key", PUBLIC_KEY_MAX),
                field("payload", STATEMENT_MAX),
            ) {
                (Some(key), Some(payload)) => parsed(Ask::Sign { key, payload }),
                _ => bad,
            }
        }
        _ => bad,
    }
}

/// A caller's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Keys(Vec<Vec<u8>>),
    Signature(Vec<u8>),
    Refused(NoAgentCause),
}

impl Answer {
    fn line(&self, id: u64, request: &Value) -> Vec<u8> {
        let request = request_json(request);
        match self {
            Answer::Keys(keys) => {
                let keys: Vec<String> = keys.iter().map(|k| hex(k)).collect();
                let keys = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into());
                callback_line(&format!(
                    r#"{{"id":{id},"request":{request},"ok":{{"keys":{keys}}}}}"#
                ))
            }
            Answer::Signature(sig) => callback_line(&format!(
                r#"{{"id":{id},"request":{request},"ok":{{"signature":"{}"}}}}"#,
                hex(sig)
            )),
            Answer::Refused(cause) => {
                let cause = Value::String(cause.as_str().into());
                callback_line(&format!(
                    r#"{{"id":{id},"request":{request},"error":{{"code":"{E_NO_AGENT}","cause":{cause}}}}}"#
                ))
            }
        }
    }
}

/// A caller's reply body to the ask `ask` for the request `request`: its id and answer. `None`
/// for anything else: another shape, an unknown field, another request, a request line, bad hex
/// or content over its bound.
fn parse_answer(body: &[u8], ask: &Ask, request: &Value) -> Option<(u64, Answer)> {
    let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    if m.len() != 3 || m.get("request") != Some(request) {
        return None;
    }
    let id = callback_id(m.get("id"))?;
    if let Some(Value::Object(ok)) = m.get("ok") {
        let answer = match ask {
            Ask::Identities => {
                let keys = ok.get("keys").and_then(Value::as_array)?;
                if ok.len() != 1 || keys.len() > IDENTITIES_MAX {
                    return None;
                }
                let keys = keys
                    .iter()
                    .map(|k| unhex(k.as_str()?, PUBLIC_KEY_MAX))
                    .collect::<Option<Vec<_>>>()?;
                Answer::Keys(keys)
            }
            Ask::Sign { .. } => {
                let sig = ok.get("signature").and_then(Value::as_str)?;
                if ok.len() != 1 {
                    return None;
                }
                Answer::Signature(unhex(sig, SSHSIG_MAX)?)
            }
        };
        return Some((id, answer));
    }
    let Some(Value::Object(error)) = m.get("error") else {
        return None;
    };
    if error.len() != 2 || error.get("code").and_then(Value::as_str) != Some(E_NO_AGENT) {
        return None;
    }
    let cause = NoAgentCause::parse(error.get("cause").and_then(Value::as_str)?)?;
    Some((id, Answer::Refused(cause)))
}

/// Whether `payload` is SSHSIG signed data that rdy signs: `SSHSIG`, a namespace of the four,
/// empty reserved data, `sha512` and a sha512 digest, nothing after.
fn rdy_signed_data(payload: &[u8]) -> bool {
    let Some(mut rest) = payload.strip_prefix(SSHSIG_MAGIC) else {
        return false;
    };
    let mut string = |max: usize| -> Option<&[u8]> {
        let len = rest.get(..4)?;
        let n = usize::try_from(u32::from_be_bytes([len[0], len[1], len[2], len[3]])).ok()?;
        if n > max || n > rest.len() - 4 {
            return None;
        }
        let (s, tail) = rest[4..].split_at(n);
        rest = tail;
        Some(s)
    };
    let parts = (|| {
        let namespace = string(64)?;
        let reserved = string(0)?;
        let hash = string(SHA512.len())?;
        let digest = string(SHA512_LEN)?;
        Some((namespace, reserved, hash, digest))
    })();
    let Some((namespace, reserved, hash, digest)) = parts else {
        return false;
    };
    rest.is_empty()
        && reserved.is_empty()
        && hash == SHA512
        && digest.len() == SHA512_LEN
        && NAMESPACES.iter().any(|ns| ns.as_bytes() == namespace)
}

/// Whether `key` is a public-key blob whose type name is one rdy signs with.
fn signing_key_blob(key: &[u8]) -> bool {
    let Some(len) = key.get(..4) else {
        return false;
    };
    let Ok(n) = usize::try_from(u32::from_be_bytes([len[0], len[1], len[2], len[3]])) else {
        return false;
    };
    key.get(4..)
        .and_then(|rest| rest.get(..n))
        .is_some_and(|name| KEY_TYPES.iter().any(|t| t.as_bytes() == name))
}

// ---- the host ----

/// The host's signer for one connection: each call asks the process at the other end, naming
/// the request it serves ([`RemoteAgent::for_request`]; `null` outside any request).
pub struct RemoteAgent {
    link: Arc<Link>,
    request: Value,
}

impl RemoteAgent {
    /// This connection's signer serving the request `request` (its id, as the client sent it).
    pub fn for_request(&self, request: Value) -> Arc<RemoteAgent> {
        Arc::new(RemoteAgent {
            link: Arc::clone(&self.link),
            request,
        })
    }
}

struct Link {
    state: Mutex<LinkState>,
    /// Held for a whole call: one callback is outstanding per connection.
    turn: Mutex<()>,
    /// Callback request lines for the connection's writer.
    out: mpsc::UnboundedSender<Vec<u8>>,
    /// True while a callback waits for its reply.
    outstanding: watch::Sender<bool>,
}

struct LinkState {
    /// The next callback id.
    next: u64,
    waiting: Option<Waiting>,
    /// Set once the channel is closed: every later call fails with this cause.
    closed: Option<NoAgentCause>,
}

struct Waiting {
    id: u64,
    request: Value,
    ask: Ask,
    reply: std::sync::mpsc::SyncSender<Result<Answer, NoAgentCause>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Link {
    /// Closes the channel with `cause` (a first cause stays) and fails the waiting call.
    fn fail(&self, cause: NoAgentCause) {
        let mut st = lock(&self.state);
        st.closed.get_or_insert(cause);
        if let Some(w) = st.waiting.take() {
            let _ = w.reply.send(Err(cause));
        }
        self.outstanding.send_replace(false);
    }

    fn deliver(&self, body: &[u8]) {
        let mut st = lock(&self.state);
        let Some(w) = st.waiting.take() else {
            // a reply nobody waits for: a duplicate, or one sent unasked
            drop(st);
            return self.fail(NoAgentCause::InvalidReply);
        };
        match parse_answer(body, &w.ask, &w.request) {
            Some((id, answer)) if id == w.id => {
                let _ = w.reply.send(Ok(answer));
                self.outstanding.send_replace(false);
            }
            _ => {
                st.waiting = Some(w);
                drop(st);
                self.fail(NoAgentCause::InvalidReply);
            }
        }
    }

    fn call(&self, ask: Ask, request: &Value) -> Result<Answer, RdyError> {
        let _turn = lock(&self.turn);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut st = lock(&self.state);
            if let Some(cause) = st.closed {
                return Err(no_agent(cause));
            }
            let id = st.next;
            let Some(next) = id.checked_add(1) else {
                st.closed = Some(NoAgentCause::InvalidReply);
                return Err(no_agent(NoAgentCause::InvalidReply));
            };
            st.next = next;
            let line = ask.line(id, request);
            // outstanding before the line can be written: whoever writes it then reads for the
            // reply
            st.waiting = Some(Waiting {
                id,
                request: request.clone(),
                ask,
                reply: tx,
            });
            self.outstanding.send_replace(true);
            if self.out.send(line).is_err() {
                st.waiting = None;
                st.closed = Some(NoAgentCause::ConnectionFailed);
                self.outstanding.send_replace(false);
                return Err(no_agent(NoAgentCause::ConnectionFailed));
            }
        }
        match rx.recv() {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(cause)) => Err(no_agent(cause)),
            Err(_) => Err(no_agent(
                lock(&self.state)
                    .closed
                    .unwrap_or(NoAgentCause::ConnectionFailed),
            )),
        }
    }
}

impl SigningAgent for RemoteAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        match self.link.call(Ask::Identities, &self.request)? {
            Answer::Keys(keys) => Ok(keys
                .into_iter()
                .map(|public_key| AgentIdentity { public_key })
                .collect()),
            Answer::Refused(cause) => Err(no_agent(cause)),
            Answer::Signature(_) => Err(no_agent(NoAgentCause::InvalidReply)),
        }
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        if public_key.len() > PUBLIC_KEY_MAX {
            return Err(rdy_engine::signing_limit(&format!(
                "{PUBLIC_KEY_MAX} bytes"
            )));
        }
        if payload.len() > STATEMENT_MAX {
            return Err(rdy_engine::signing_limit(&format!("{STATEMENT_MAX} bytes")));
        }
        let ask = Ask::Sign {
            key: public_key.to_vec(),
            payload: payload.to_vec(),
        };
        match self.link.call(ask, &self.request)? {
            Answer::Signature(sig) => Ok(sig),
            Answer::Refused(cause) => Err(no_agent(cause)),
            Answer::Keys(_) => Err(no_agent(NoAgentCause::InvalidReply)),
        }
    }

    /// This connection's signer naming the call's request. The caller stops its own ask at the
    /// request's cancel and answers `request cancelled`, so the host waits for that reply.
    fn for_call(&self, call: &SigningCall) -> Option<Arc<dyn SigningAgent>> {
        Some(self.for_request(call.request.clone()))
    }
}

/// The reading half of a host connection's signing channel: callback lines read off the
/// connection go here. Dropping it closes the channel as `connection failed`.
pub struct HostInbound {
    link: Arc<Link>,
    outstanding: watch::Receiver<bool>,
}

impl HostInbound {
    /// A callback line read off the connection, without its newline (the mark may be kept).
    pub fn deliver(&self, line: &[u8]) {
        let body = line.strip_prefix(&[CALLBACK_MARK]).unwrap_or(line);
        self.link.deliver(body);
    }

    /// A callback line over [`MAX_CALLBACK_LINE_BYTES`] was read off and dropped.
    pub fn over(&self) {
        self.link.fail(NoAgentCause::InvalidReply);
    }

    /// The caller kept sending past [`MAX_READ_AHEAD_BYTES`] while a callback waited.
    pub fn flooded(&self) {
        self.link.fail(NoAgentCause::InvalidReply);
    }

    /// The connection is gone: the waiting call and every later one fail.
    pub fn close(&self) {
        self.link.fail(NoAgentCause::ConnectionFailed);
    }

    /// Whether a callback waits for its reply.
    pub fn outstanding(&self) -> bool {
        *self.outstanding.borrow()
    }

    /// Waits until a callback starts or ends.
    pub async fn changed(&mut self) {
        if self.outstanding.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Drop for HostInbound {
    fn drop(&mut self) {
        self.close();
    }
}

/// The writing half of a host connection's signing channel: the callback request lines to write.
pub struct HostOutbound {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl HostOutbound {
    /// The next callback request line, mark and newline included. Never ends while the
    /// connection's [`RemoteAgent`] lives.
    pub async fn next(&mut self) -> Vec<u8> {
        match self.rx.recv().await {
            Some(line) => line,
            None => std::future::pending().await,
        }
    }
}

/// A new signing channel for one host connection.
pub fn host_link() -> (Arc<RemoteAgent>, HostInbound, HostOutbound) {
    let (out, rx) = mpsc::unbounded_channel();
    let (outstanding, watching) = watch::channel(false);
    let link = Arc::new(Link {
        state: Mutex::new(LinkState {
            next: 1,
            waiting: None,
            closed: None,
        }),
        turn: Mutex::new(()),
        out,
        outstanding,
    });
    (
        Arc::new(RemoteAgent {
            link: Arc::clone(&link),
            request: Value::Null,
        }),
        HostInbound {
            link,
            outstanding: watching,
        },
        HostOutbound { rx },
    )
}

/// An MCP connection with its signing channel taken out: the stream the MCP service runs on,
/// the connection's signer, and the pump that joins them to the connection.
pub struct HostMcp {
    pub stream: McpStream,
    pub agent: Arc<RemoteAgent>,
    /// Runs until the service's stream closes and its last bytes are written; it then closes the
    /// connection.
    pub pump: Pin<Box<dyn Future<Output = ()> + Send>>,
}

/// Splits the MCP connection `conn`: callback lines it reads go to the signer, everything else
/// to the service's stream, byte for byte; the service's output and the callback requests share
/// the connection's one writer, at whole lines.
pub fn host_mcp<S>(conn: S) -> HostMcp
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (agent, inbound, outbound) = host_link();
    let (service, pumped) = tokio::io::duplex(PIPE_BYTES);
    let pump = Box::pin(async move {
        let (sock_r, sock_w) = tokio::io::split(conn);
        let (app_r, app_w) = tokio::io::split(pumped);
        let into = demux(sock_r, app_w, inbound);
        let out = mux(app_r, sock_w, outbound);
        tokio::pin!(into, out);
        let mut into_done = false;
        loop {
            tokio::select! {
                () = &mut into, if !into_done => into_done = true,
                () = &mut out => break,
            }
        }
    });
    HostMcp {
        stream: Box::new(service),
        agent,
        pump,
    }
}

/// How a line read off a connection starts.
enum LineKind {
    /// No byte of the line read yet.
    Start,
    Ordinary,
    /// A callback line, assembled; `None` once it is over the bound.
    Callback(Option<Vec<u8>>),
}

/// Connection to service: ordinary bytes are forwarded as they come, callback lines are handed
/// to `inbound`. At the connection's end the service's input is shut down.
async fn demux<R, W>(mut sock: R, mut app: W, mut inbound: HostInbound)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; PIPE_BYTES];
    let mut pending: Vec<u8> = Vec::new();
    let mut kind = LineKind::Start;
    let mut app_open = true;
    loop {
        let waiting = inbound.outstanding();
        if waiting && pending.len() >= MAX_READ_AHEAD_BYTES {
            inbound.flooded();
            continue;
        }
        let can_read = pending.len() < SOFT_READ_AHEAD_BYTES || waiting;
        tokio::select! {
            biased;
            n = app.write(&pending), if app_open && !pending.is_empty() => match n {
                Ok(0) | Err(_) => {
                    app_open = false;
                    pending = Vec::new();
                }
                Ok(n) => {
                    pending.drain(..n);
                }
            },
            n = sock.read(&mut buf), if can_read => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    split_lines(&buf[..n], &mut kind, &mut pending, &inbound);
                    if !app_open {
                        pending.clear();
                    }
                }
            },
            () = inbound.changed(), if !can_read => {}
        }
    }
    inbound.close();
    if app_open {
        let _ = app.write_all(&pending).await;
        let _ = app.flush().await;
        let _ = app.shutdown().await;
    }
}

/// Sorts `bytes` read off a connection: ordinary bytes onto `pending`, whole callback lines to
/// `inbound`.
fn split_lines(
    mut bytes: &[u8],
    kind: &mut LineKind,
    pending: &mut Vec<u8>,
    inbound: &HostInbound,
) {
    while let Some(&first) = bytes.first() {
        if matches!(kind, LineKind::Start) {
            *kind = if first == CALLBACK_MARK {
                LineKind::Callback(Some(Vec::new()))
            } else {
                LineKind::Ordinary
            };
        }
        let end = bytes.iter().position(|&b| b == b'\n');
        let (segment, rest) = match end {
            Some(i) => (&bytes[..i], &bytes[i + 1..]),
            None => (bytes, &[][..]),
        };
        match kind {
            LineKind::Start => unreachable!("set above"),
            LineKind::Ordinary => {
                pending.extend_from_slice(&bytes[..end.map_or(bytes.len(), |i| i + 1)]);
            }
            LineKind::Callback(line) => {
                if let Some(l) = line {
                    if l.len() + segment.len() > MAX_CALLBACK_LINE_BYTES + 1 {
                        *line = None;
                    } else {
                        l.extend_from_slice(segment);
                    }
                }
                if end.is_some() {
                    match line.take() {
                        Some(mut l) => {
                            if l.last() == Some(&b'\r') {
                                l.pop();
                            }
                            if l.len() > MAX_CALLBACK_LINE_BYTES {
                                inbound.over();
                            } else {
                                inbound.deliver(&l);
                            }
                        }
                        None => inbound.over(),
                    }
                }
            }
        }
        if end.is_some() {
            *kind = LineKind::Start;
        }
        bytes = rest;
    }
}

/// A source of lines to put between whole lines of a stream.
trait LineSource {
    async fn next_line(&mut self) -> Vec<u8>;
}

impl LineSource for HostOutbound {
    async fn next_line(&mut self) -> Vec<u8> {
        self.next().await
    }
}

impl LineSource for mpsc::UnboundedReceiver<Vec<u8>> {
    async fn next_line(&mut self) -> Vec<u8> {
        match self.recv().await {
            Some(line) => line,
            None => std::future::pending().await,
        }
    }
}

/// What [`copy_lines`] ended on.
pub(crate) struct Copied {
    /// A write to the connection failed; later bytes were read and dropped.
    pub failed: bool,
    /// The last byte written from the stream is not a newline.
    pub midline: bool,
}

/// Copies `app` to `sock` until `app`'s end, writing each line `lines` yields whole, between two
/// lines of `app`. A line of `app` that starts with [`CALLBACK_MARK`] is dropped: only `lines`
/// writes callback lines. After a failed write, `app` is still read to its end, and dropped.
async fn copy_lines<R, W, L>(app: &mut R, sock: &mut W, lines: &mut L) -> Copied
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    L: LineSource,
{
    let mut buf = vec![0u8; PIPE_BYTES];
    let mut midline = false;
    let mut skipping = false;
    let mut failed = false;
    loop {
        tokio::select! {
            biased;
            line = lines.next_line(), if !midline => {
                if !failed && (sock.write_all(&line).await.is_err() || sock.flush().await.is_err()) {
                    failed = true;
                }
            }
            n = app.read(&mut buf) => {
                let n = match n {
                    Ok(0) | Err(_) => return Copied { failed, midline },
                    Ok(n) => n,
                };
                let mut out = Vec::with_capacity(n);
                for &b in &buf[..n] {
                    if !midline {
                        skipping = b == CALLBACK_MARK;
                    }
                    if !skipping {
                        out.push(b);
                    }
                    midline = b != b'\n';
                }
                if !failed
                    && !out.is_empty()
                    && (sock.write_all(&out).await.is_err() || sock.flush().await.is_err())
                {
                    failed = true;
                }
            }
        }
    }
}

/// Service to connection: the service's bytes, with callback requests between its lines. At the
/// service's end the connection's writing half is shut down.
async fn mux<R, W>(mut app: R, mut sock: W, mut outbound: HostOutbound)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let _ = copy_lines(&mut app, &mut sock, &mut outbound).await;
    let _ = sock.shutdown().await;
}

// ---- the caller ----

/// A future that ends when the caller is interrupted.
pub type Interrupted = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Makes the interrupt of one callback. It is called before the caller's agent is asked, and
/// whatever it watches is watched from then on, so an interrupt during the ask is never missed.
pub type InterruptSource = Arc<dyn Fn() -> Interrupted + Send + Sync>;

/// An interrupt that never comes.
pub fn never_interrupted() -> InterruptSource {
    Arc::new(|| Box::pin(std::future::pending()))
}

/// The caller's `SIGINT`, caught (through the sys seam) only while a callback asks the agent:
/// outside one, `SIGINT` does what it did before.
pub fn sigint() -> InterruptSource {
    Arc::new(|| {
        let guard = crate::sys::catch_interrupt();
        Box::pin(async move {
            match guard {
                Ok(mut g) => g.recv().await,
                Err(_) => std::future::pending().await,
            }
        })
    })
}

/// Why an ask of the caller's agent stopped before the agent answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The connection is gone: there is no one to reply to.
    Gone,
    /// The host broke the callback protocol: the link closes, and the reply is `invalid reply`.
    Invalid,
}

/// Cancels a call when dropped, unless disarmed: an ask dropped part way still ends its worker.
struct CancelOnDrop(Option<Arc<CallCancel>>);

impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
}

/// The caller's end of one connection's signing channel: it answers the host's callbacks with
/// the caller's own agent.
pub struct CallerLink {
    agent: Option<Arc<dyn CallerSigner>>,
    interrupt: InterruptSource,
    /// The last callback id answered.
    last: u64,
    /// Set once the host sent an invalid callback: every later one is refused.
    closed: bool,
}

impl std::fmt::Debug for CallerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallerLink")
            .field("agent", &self.agent.is_some())
            .field("last", &self.last)
            .field("closed", &self.closed)
            .finish()
    }
}

impl CallerLink {
    /// A caller with `agent` (none: every ask is `SSH_AUTH_SOCK is not set`), interrupted by
    /// `interrupt`.
    pub fn new(agent: Option<Arc<dyn CallerSigner>>, interrupt: InterruptSource) -> CallerLink {
        CallerLink {
            agent,
            interrupt,
            last: 0,
            closed: false,
        }
    }

    pub fn set_agent(&mut self, agent: Arc<dyn CallerSigner>) {
        self.agent = Some(agent);
    }

    pub fn set_interrupt(&mut self, interrupt: InterruptSource) {
        self.interrupt = interrupt;
    }

    /// The reply line (mark and newline included) to the callback line `line` (newline taken
    /// off, mark kept or not), asking the caller's agent on the blocking pool. `cancel_of` gives
    /// the cancel of the request the callback names (its `request`); `None` when that request is
    /// no longer awaited (cancelled, answered or unknown): the reply is then `request
    /// cancelled`, and the agent is not asked. A cancel, before the ask or during it, and an
    /// interrupt during it, give `request cancelled` too; the worker is always waited for, so no
    /// signing worker outlives its callback. When `stop` ends first the ask is cancelled too:
    /// [`Stop::Gone`] gives no reply, [`Stop::Invalid`] closes the link and gives `invalid
    /// reply`. A line that is not a valid callback request closes the link (see
    /// [`CallerLink::refuse`]). Every reply echoes the callback's `request`.
    pub async fn answer(
        &mut self,
        line: &[u8],
        cancel_of: impl FnOnce(&Value) -> Option<Arc<CallCancel>>,
        stop: impl Future<Output = Stop> + Send,
    ) -> Option<Vec<u8>> {
        let body = line.strip_prefix(&[CALLBACK_MARK]).unwrap_or(line);
        let Parsed { id, request, ask } = match parse_ask(body) {
            Ok(parsed) => parsed,
            Err(id) => return Some(self.refuse(id)),
        };
        if self.closed || id <= self.last {
            return Some(self.refuse_for(Some(id), &request));
        }
        self.last = id;
        let Some(cancel) = cancel_of(&request) else {
            return Some(Answer::Refused(NoAgentCause::Cancelled).line(id, &request));
        };
        match self.ask(ask, cancel, stop).await {
            Ok(answer) => Some(answer.line(id, &request)),
            Err(Stop::Gone) => None,
            Err(Stop::Invalid) => {
                self.closed = true;
                Some(Answer::Refused(NoAgentCause::InvalidReply).line(id, &request))
            }
        }
    }

    /// The reply to a callback line that broke the protocol (oversized, malformed, a repeated
    /// id, or anything after the link closed): the link closes, and the reply is `invalid
    /// reply` for `id`, the line's id when one could be read. With none, it is the next id, so
    /// the host's waiter still wakes: a reply to another id fails the host's channel closed too.
    /// Its `request` is `null`: none could be read.
    pub fn refuse(&mut self, id: Option<u64>) -> Vec<u8> {
        self.refuse_for(id, &Value::Null)
    }

    /// [`CallerLink::refuse`], echoing `request`.
    fn refuse_for(&mut self, id: Option<u64>, request: &Value) -> Vec<u8> {
        self.closed = true;
        let id = id.unwrap_or_else(|| self.last.checked_add(1).unwrap_or(self.last));
        self.last = self.last.max(id);
        Answer::Refused(NoAgentCause::InvalidReply).line(id, request)
    }

    /// The answer to `ask`; `Err` when `stop` ended first.
    async fn ask(
        &self,
        ask: Ask,
        cancel: Arc<CallCancel>,
        stop: impl Future<Output = Stop> + Send,
    ) -> Result<Answer, Stop> {
        let Some(agent) = self.agent.clone() else {
            return Ok(Answer::Refused(NoAgentCause::Unset));
        };
        if let Ask::Sign { key, payload } = &ask
            && !(signing_key_blob(key) && rdy_signed_data(payload))
        {
            return Ok(Answer::Refused(NoAgentCause::Refused));
        }
        if cancel.is_cancelled() {
            return Ok(Answer::Refused(NoAgentCause::Cancelled));
        }
        // watched from before the agent is asked
        let interrupted = (self.interrupt)();
        let mut armed = CancelOnDrop(Some(Arc::clone(&cancel)));
        let worker = {
            let cancel = Arc::clone(&cancel);
            tokio::task::spawn_blocking(move || match ask {
                Ask::Identities => agent
                    .identities_until(&cancel)
                    .map(|ids| Answer::Keys(ids.into_iter().map(|i| i.public_key).collect())),
                Ask::Sign { key, payload } => agent
                    .sign_until(&key, &payload, &cancel)
                    .map(Answer::Signature),
            })
        };
        tokio::pin!(worker, stop);
        let outcome = tokio::select! {
            biased;
            out = &mut worker => {
                armed.disarm();
                return Ok(match out {
                    Ok(Ok(answer)) => answer,
                    Ok(Err(e)) => Answer::Refused(cause_of(&e)),
                    Err(_) => Answer::Refused(NoAgentCause::InvalidReply),
                });
            }
            () = interrupted => Ok(Answer::Refused(NoAgentCause::Cancelled)),
            why = &mut stop => Err(why),
        };
        // the cancel is kept even if the worker has not started: it ends at once
        cancel.cancel();
        let _ = (&mut worker).await;
        armed.disarm();
        outcome
    }
}

/// A line read with a cap.
enum Capped {
    Line(Vec<u8>),
    /// Over the cap: read off and dropped, with its top-level id when one could be read.
    Over(Option<u64>),
    Eof,
}

/// Reads one line off `r`, newline (and a `\r` before it) taken off, never holding more than
/// `max` bytes of it: a longer line is read off and dropped, its id read as it streams.
async fn read_capped<R: AsyncRead + Unpin>(r: &mut BufReader<R>, max: usize) -> io::Result<Capped> {
    let mut line = Vec::new();
    let mut over = false;
    let mut scan = LineScan::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return Ok(Capped::Eof);
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], true),
            None => (buf, false),
        };
        let used = chunk.len() + usize::from(done);
        scan.feed(chunk);
        if !over {
            // room for a trailing \r, which does not count
            if line.len() + chunk.len() > max + 1 {
                over = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        r.consume(used);
        if done {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(if over || line.len() > max {
                Capped::Over(callback_id(scan.finish().id.as_ref()))
            } else {
                Capped::Line(line)
            });
        }
    }
}

/// What the host does while the caller's agent is asked: nothing is allowed then (its one
/// callback is outstanding, and its reply comes after the callback's), so the connection's end
/// is [`Stop::Gone`] and any byte is [`Stop::Invalid`].
async fn watch_conn<S: AsyncRead + Unpin>(conn: &mut BufReader<S>) -> Stop {
    match conn.fill_buf().await {
        Ok(b) if !b.is_empty() => Stop::Invalid,
        _ => Stop::Gone,
    }
}

/// Reads the next ordinary line off the caller's connection `conn`, without its newline,
/// answering each callback line before it with the caller's agent. `Ok(None)` at the
/// connection's end, a cut last line included, and when the connection ends while the agent is
/// asked. Bytes that arrive while the agent is asked break the protocol: the ask is cancelled,
/// the signing channel closes and the callback's reply is `invalid reply`; reading then goes on.
/// So does a callback line over [`MAX_CALLBACK_LINE_BYTES`] or a malformed one.
pub async fn read_reply<S>(
    link: &mut CallerLink,
    conn: &mut BufReader<S>,
) -> io::Result<Option<Vec<u8>>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    loop {
        let first = match conn.fill_buf().await?.first() {
            None => return Ok(None),
            Some(&b) => b,
        };
        if first != CALLBACK_MARK {
            let mut line = Vec::new();
            conn.read_until(b'\n', &mut line).await?;
            if line.pop() != Some(b'\n') {
                return Ok(None);
            }
            return Ok(Some(line));
        }
        let reply = match read_capped(conn, MAX_CALLBACK_LINE_BYTES).await? {
            Capped::Line(line) => {
                // a ctl client has one request in flight: every callback is that request's
                let fresh = |_: &Value| Some(Arc::new(CallCancel::new()));
                let Some(reply) = link.answer(&line, fresh, watch_conn(conn)).await else {
                    return Ok(None);
                };
                reply
            }
            Capped::Eof => return Ok(None),
            Capped::Over(id) => link.refuse(id),
        };
        let w = conn.get_mut();
        w.write_all(&reply).await?;
        w.flush().await?;
    }
}

/// The closed cause of an agent error.
fn cause_of(e: &RdyError) -> NoAgentCause {
    let prefix = "the SSH agent could not sign: ";
    if e.code == E_NO_AGENT
        && let Some(cause) = e.message.strip_prefix(prefix).and_then(NoAgentCause::parse)
    {
        return cause;
    }
    NoAgentCause::InvalidReply
}

/// The proxy's writer: copies the host's lines (`app`, from stdin) to the connection with the
/// callback replies (`replies`) between them.
pub(crate) async fn copy_with_replies<R, W>(
    app: &mut R,
    sock: &mut W,
    replies: &mut mpsc::UnboundedReceiver<Vec<u8>>,
) -> Copied
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    copy_lines(app, sock, replies).await
}

/// Writes the replies that come on `replies` until `done` ends; `failed` once a write failed.
pub(crate) async fn write_replies<W: AsyncWrite + Unpin>(
    sock: &mut W,
    replies: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    mut failed: bool,
    done: impl Future<Output = ()>,
) -> bool {
    tokio::pin!(done);
    loop {
        tokio::select! {
            biased;
            () = &mut done => return failed,
            line = replies.next_line() => {
                if !failed && (sock.write_all(&line).await.is_err() || sock.flush().await.is_err()) {
                    failed = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sig5_hex_is_lowercase_and_bounded() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(unhex("00abff", 3), Some(vec![0x00, 0xab, 0xff]));
        assert_eq!(unhex("00ABff", 3), None);
        assert_eq!(unhex("0ab", 3), None);
        assert_eq!(unhex("00abff", 2), None);
    }

    #[test]
    fn sig5_only_rdy_signed_data_is_signed() {
        let string = |b: &[u8]| {
            let mut v = u32::try_from(b.len()).unwrap().to_be_bytes().to_vec();
            v.extend_from_slice(b);
            v
        };
        let data = |ns: &[u8], reserved: &[u8], hash: &[u8], digest: &[u8]| {
            let mut v = SSHSIG_MAGIC.to_vec();
            for part in [ns, reserved, hash, digest] {
                v.extend(string(part));
            }
            v
        };
        let digest = [7u8; 64];
        assert!(rdy_signed_data(&data(
            b"rdy-approval-v1",
            b"",
            b"sha512",
            &digest
        )));
        assert!(rdy_signed_data(&data(
            b"rdy-keybind-v1",
            b"",
            b"sha512",
            &digest
        )));
        assert!(!rdy_signed_data(&data(b"git", b"", b"sha512", &digest)));
        assert!(!rdy_signed_data(&data(
            b"rdy-tag-v1",
            b"x",
            b"sha512",
            &digest
        )));
        assert!(!rdy_signed_data(&data(
            b"rdy-tag-v1",
            b"",
            b"sha256",
            &digest[..32]
        )));
        assert!(!rdy_signed_data(&data(
            b"rdy-tag-v1",
            b"",
            b"sha512",
            &digest[..63]
        )));
        let mut trailing = data(b"rdy-tag-v1", b"", b"sha512", &digest);
        trailing.push(0);
        assert!(!rdy_signed_data(&trailing));
        assert!(!rdy_signed_data(b"raw bytes"));
        assert!(signing_key_blob(
            &[string(b"ssh-ed25519"), string(&[1; 32])].concat()
        ));
        assert!(!signing_key_blob(&string(b"ssh-rsa")));
        assert!(!signing_key_blob(&[0, 0, 0, 9, b's']));
    }
}
