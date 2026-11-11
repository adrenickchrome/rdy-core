//! A small MCP-over-HTTP client for the daemon's tests (mht6): its own HTTP/1.1 framing over a
//! real TCP socket, independent of the daemon's dispatch. One request per connection; a reply is
//! read whole (by `Content-Length`, chunked, or to the close), and an open reply is read one SSE
//! message at a time with a deadline. Blocking std sockets, no new crate.
use serde_json::{Value, json};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// How long a whole reply may take to arrive.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The modern MCP revision.
pub const MODERN: &str = "2026-07-28";
/// The two older revisions MCP over HTTP serves.
pub const LEGACY: [&str; 2] = ["2025-06-18", "2025-11-25"];

/// Where one project's MCP endpoint is and the bearer it takes: the site's loopback address as
/// the daemon reports it, never derived from its port candidates.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub addr: SocketAddr,
    /// `127.0.0.1:<port>`, the request's `Host`.
    pub authority: String,
    pub token: String,
}

impl Endpoint {
    /// The endpoint of the site at `url` (`http://127.0.0.1:<port>/`) with `token`.
    pub fn of(url: &str, token: &str) -> Endpoint {
        let authority = url
            .strip_prefix("http://")
            .and_then(|r| r.strip_suffix('/'))
            .unwrap_or_else(|| panic!("not a site URL: {url}"))
            .to_string();
        let addr = authority
            .parse()
            .unwrap_or_else(|_| panic!("not a socket address: {authority}"));
        Endpoint {
            addr,
            authority,
            token: token.to_string(),
        }
    }

    /// The port the endpoint listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The head of a `POST /~mcp` with the bearer, JSON content and both accepted types,
    /// `extra` header lines, and `Content-Length` of `len`.
    pub fn head(&self, extra: &[(&str, &str)], len: usize) -> String {
        let mut head = format!(
            "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\n",
            self.authority, self.token
        );
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {len}\r\n\r\n"));
        head
    }

    /// `body` posted with `extra` headers; the whole reply.
    pub fn post(&self, extra: &[(&str, &str)], body: &Value) -> Reply {
        let body = body.to_string();
        let mut raw = self.head(extra, body.len()).into_bytes();
        raw.extend_from_slice(body.as_bytes());
        send(self.addr, &raw)
    }

    /// `body` posted with `extra` headers; the reply open, for its stream.
    pub fn open(&self, extra: &[(&str, &str)], body: &Value) -> Stream {
        let body = body.to_string();
        let mut raw = self.head(extra, body.len()).into_bytes();
        raw.extend_from_slice(body.as_bytes());
        Stream::open(self.addr, &raw)
    }
}

/// A reply: its status (0 when the server closed with nothing), its head lines, and its body,
/// decoded from its framing.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
    /// Every byte the server sent, for counting responses on one connection.
    pub raw: Vec<u8>,
}

impl Reply {
    /// The value of header `name` (any case), the first one sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.head, name)
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The JSON-RPC messages of the body: one JSON reply, or each SSE event's data.
    pub fn messages(&self) -> Vec<Value> {
        let text = self.text();
        if self
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/event-stream"))
        {
            return sse_messages(&text);
        }
        serde_json::from_str(&text).into_iter().collect()
    }

    /// The last JSON-RPC message: the reply to the request.
    pub fn reply(&self) -> Value {
        self.messages().pop().unwrap_or(Value::Null)
    }

    /// A tool reply's rdy body and whether it is an error.
    pub fn tool(&self) -> (bool, Value) {
        let r = self.reply();
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool reply: {} {}", self.head, self.text()));
        (
            r["result"]["isError"] == json!(true),
            serde_json::from_str(text).expect("a JSON tool body"),
        )
    }

    /// Whether any header of the reply starts with `prefix` (any case).
    pub fn has_header_prefix(&self, prefix: &str) -> bool {
        self.head.split("\r\n").skip(1).any(|l| {
            l.split_once(':')
                .is_some_and(|(n, _)| n.to_ascii_lowercase().starts_with(prefix))
        })
    }
}

fn header_in<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn sse_messages(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|event| {
            let data: String = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            serde_json::from_str(&data).ok()
        })
        .collect()
}

/// Sends `raw` as it is to `addr`, then reads to the end.
pub fn send(addr: SocketAddr, raw: &[u8]) -> Reply {
    send_parts(addr, &[raw])
}

/// Sends each of `parts` in its own write (a body in pieces), then reads to the end. A server
/// that closes at once may reset a write: what it sent still counts.
pub fn send_parts(addr: SocketAddr, parts: &[&[u8]]) -> Reply {
    let mut s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    s.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    s.set_nodelay(true).unwrap();
    for part in parts {
        if s.write_all(part).and_then(|()| s.flush()).is_err() {
            break;
        }
    }
    let mut got = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                panic!("no whole reply from {addr} in {READ_TIMEOUT:?}")
            }
            Err(_) => break,
        }
    }
    parse(got)
}

/// One reply off `bytes`: its head, and its body by its framing.
fn parse(bytes: Vec<u8>) -> Reply {
    let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Reply {
            status: 0,
            head: String::from_utf8_lossy(&bytes).into_owned(),
            body: Vec::new(),
            raw: bytes,
        };
    };
    let head = String::from_utf8_lossy(&bytes[..at]).into_owned();
    let rest = &bytes[at + 4..];
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let chunked =
        header_in(&head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        let mut d = Dechunk::default();
        d.feed(rest);
        d.out
    } else if let Some(n) = header_in(&head, "content-length").and_then(|v| v.parse().ok()) {
        rest[..usize::min(n, rest.len())].to_vec()
    } else {
        rest.to_vec()
    };
    Reply {
        status,
        head,
        body,
        raw: bytes,
    }
}

/// A chunked body decoder fed in pieces.
#[derive(Default)]
struct Dechunk {
    pending: Vec<u8>,
    out: Vec<u8>,
    done: bool,
}

impl Dechunk {
    /// Feeds `more`; the decoded bytes so far are in `out`.
    fn feed(&mut self, more: &[u8]) {
        self.pending.extend_from_slice(more);
        loop {
            if self.done {
                return;
            }
            let Some(eol) = self.pending.windows(2).position(|w| w == b"\r\n") else {
                return;
            };
            let line = String::from_utf8_lossy(&self.pending[..eol]).into_owned();
            let size = line.split(';').next().unwrap_or("").trim();
            let Ok(size) = usize::from_str_radix(size, 16) else {
                self.done = true;
                return;
            };
            if size == 0 {
                self.done = true;
                return;
            }
            let need = eol + 2 + size + 2;
            if self.pending.len() < need {
                return;
            }
            self.out
                .extend_from_slice(&self.pending[eol + 2..eol + 2 + size]);
            self.pending.drain(..need);
        }
    }
}

/// An open reply, read one message at a time: a JSON reply, or an SSE stream.
pub struct Stream {
    stream: TcpStream,
    pub status: u16,
    pub head: String,
    chunked: bool,
    dechunk: Dechunk,
    /// Decoded body bytes not yet split into events.
    text: Vec<u8>,
    ended: bool,
}

impl Stream {
    fn open(addr: SocketAddr, raw: &[u8]) -> Stream {
        let mut stream = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
        stream.set_nodelay(true).unwrap();
        stream.write_all(raw).unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        let mut got = Vec::new();
        let mut chunk = [0u8; 4096];
        let at = loop {
            if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => panic!("no reply head: {}", String::from_utf8_lossy(&got)),
                Ok(n) => got.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&got[..at]).into_owned();
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let chunked = header_in(&head, "transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
        let mut s = Stream {
            stream,
            status,
            head,
            chunked,
            dechunk: Dechunk::default(),
            text: Vec::new(),
            ended: false,
        };
        s.take(&got[at + 4..]);
        s
    }

    /// The value of header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.head, name)
    }

    fn take(&mut self, bytes: &[u8]) {
        if self.chunked {
            self.dechunk.feed(bytes);
            self.text.append(&mut self.dechunk.out);
            if self.dechunk.done {
                self.ended = true;
            }
        } else {
            self.text.extend_from_slice(bytes);
        }
    }

    /// The next JSON-RPC message, waiting up to `deadline`; `None` when the reply ended (or the
    /// deadline passed, see [`Stream::ended`]). An event with no JSON data is skipped.
    pub fn next(&mut self, deadline: Duration) -> Option<Value> {
        let until = Instant::now() + deadline;
        loop {
            while let Some(end) = self.text.windows(2).position(|w| w == b"\n\n") {
                let event: Vec<u8> = self.text.drain(..end + 2).collect();
                if let Some(v) = sse_messages(&String::from_utf8_lossy(&event)).pop() {
                    return Some(v);
                }
            }
            if self.ended {
                return None;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            self.stream.set_read_timeout(Some(left)).unwrap();
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => self.ended = true,
                Ok(n) => self.take(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return None;
                }
                Err(_) => self.ended = true,
            }
        }
    }

    /// Whether the server ended the reply (its last chunk, or the close).
    pub fn ended(&self) -> bool {
        self.ended
    }
}

/// The headers of a request on an older `revision`.
pub fn legacy(revision: &str) -> Vec<(&'static str, String)> {
    vec![("MCP-Protocol-Version", revision.to_string())]
}

/// The headers of a modern request of `method`, naming `name` (a tool) when given.
pub fn modern(method: &str, name: Option<&str>) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("MCP-Protocol-Version", MODERN.to_string()),
        ("Mcp-Method", method.to_string()),
    ];
    if let Some(n) = name {
        h.push(("Mcp-Name", n.to_string()));
    }
    h
}

/// Borrowed header pairs of `h`.
pub fn pairs<'a>(h: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    h.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The modern per-request metadata, with `extra` keys.
pub fn modern_meta(extra: Value) -> Value {
    let mut m = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "rdy-qa", "version": "0"},
    });
    for (k, v) in extra.as_object().expect("an object") {
        m[k] = v.clone();
    }
    m
}

/// An rdy claim for `_meta`.
pub fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

/// A `tools/call` of `name` with `args` and `_meta`.
pub fn call(id: u64, name: &str, args: Value, meta: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args, "_meta": meta}})
}

/// A request of `method` with `params`.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// A legacy `initialize` asking for `revision`.
pub fn initialize(id: u64, revision: &str) -> Value {
    request(
        id,
        "initialize",
        json!({"protocolVersion": revision, "capabilities": {},
            "clientInfo": {"name": "rdy-qa", "version": "0"}}),
    )
}

/// A modern `subscriptions/listen` of `uris`, for `meta`'s claim.
pub fn listen(id: u64, uris: &[&str], meta: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "subscriptions/listen",
        "params": {"_meta": meta, "notifications": {"resourceSubscriptions": uris}}})
}
