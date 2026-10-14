//! A small HTTP/1.1 client for the live site's tests (ls6): one request per connection, read to
//! the end, and an event-stream reader with deadlines. Blocking std sockets, no new crate.
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// How long a request waits for its answer.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// An answer: its status (0 when nothing came back), its head (the status line and the header
/// lines, joined by `\r\n`, without the empty line) and its body.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

impl Response {
    /// The value of header `name` (any case), the first one sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.head.split("\r\n").skip(1).find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

/// `url`'s authority (`host:port`) and its path (`/` when it has none).
fn split_url(url: &str) -> (&str, &str) {
    let rest = url
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("not an http URL: {url}"));
    match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    }
}

fn resolve(authority: &str) -> SocketAddr {
    authority
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .unwrap_or_else(|| panic!("no address for {authority}"))
}

/// `GET url`, with the URL's authority as its `Host`.
pub fn get(url: &str) -> Response {
    let (authority, path) = split_url(url);
    raw(
        resolve(authority),
        &format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\n\r\n"),
    )
}

/// Sends `request` as it is to `addr` and reads to the end. A connection the server closes
/// with no answer (or resets) gives what was read: nothing, status 0.
pub fn raw(addr: SocketAddr, request: &str) -> Response {
    let mut s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    s.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    let mut got = Vec::new();
    // a server that closes at once may reset the write: what it sent still counts
    if s.write_all(request.as_bytes()).is_ok() {
        let mut chunk = [0u8; 8192];
        loop {
            match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    panic!("no answer from {addr} in {READ_TIMEOUT:?}")
                }
                Err(_) => break,
            }
        }
    }
    parse(&got)
}

fn parse(bytes: &[u8]) -> Response {
    let (head, body) = match bytes.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(at) => (&bytes[..at], &bytes[at + 4..]),
        None => (bytes, &[][..]),
    };
    let head = String::from_utf8_lossy(head).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Response {
        status,
        head,
        body: body.to_vec(),
    }
}

/// An open event stream: `GET` of an events URL whose head was read.
pub struct Sse {
    stream: TcpStream,
    buf: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
    ended: bool,
}

impl Sse {
    /// Opens the stream at `url` and reads its head; panics unless it is `200`. Once this
    /// returns, the server has the page's stream.
    pub fn open(url: &str) -> Sse {
        let (authority, path) = split_url(url);
        let mut stream = TcpStream::connect(resolve(authority)).unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: text/event-stream\r\n\r\n"
        )
        .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let at = loop {
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            let n = stream.read(&mut chunk).unwrap();
            assert!(
                n > 0,
                "the stream closed in its head: {}",
                String::from_utf8_lossy(&buf)
            );
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = parse(&buf[..at + 4]);
        assert_eq!(head.status, 200, "{}", head.head);
        Sse {
            stream,
            buf: buf[at + 4..].to_vec(),
            event: None,
            data: None,
            ended: false,
        }
    }

    /// The next event, as its name and data; comments and `retry` lines are skipped. `None` at
    /// the deadline or at the stream's end ([`Sse::ended`] tells which).
    pub fn next_event(&mut self, deadline: Duration) -> Option<(String, String)> {
        let until = Instant::now() + deadline;
        let mut chunk = [0u8; 4096];
        loop {
            while let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=at).collect();
                let line = String::from_utf8_lossy(&line[..at]);
                let line = line.strip_suffix('\r').unwrap_or(&line);
                if line.is_empty() {
                    if self.event.is_some() || self.data.is_some() {
                        let event = self.event.take().unwrap_or_else(|| "message".into());
                        return Some((event, self.data.take().unwrap_or_default()));
                    }
                    continue;
                }
                if line.starts_with(':') {
                    continue;
                }
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value).to_string();
                match field {
                    "event" => self.event = Some(value),
                    "data" => {
                        self.data = Some(match self.data.take() {
                            Some(d) => format!("{d}\n{value}"),
                            None => value,
                        })
                    }
                    _ => {}
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
            match self.stream.read(&mut chunk) {
                Ok(0) => self.ended = true,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return None;
                }
                Err(_) => self.ended = true,
            }
        }
    }

    /// The server ended the stream (or the connection failed).
    pub fn ended(&self) -> bool {
        self.ended
    }
}

/// The events URL of a served page: its `data-events` attribute, joined to `base` (the site's
/// root URL).
pub fn events_url(base: &str, page_html: &str) -> String {
    const ATTR: &str = "data-events=\"";
    let from = page_html
        .find(ATTR)
        .unwrap_or_else(|| panic!("the page has no data-events"))
        + ATTR.len();
    let len = page_html[from..].find('"').expect("the attribute ends");
    let path = page_html[from..from + len].replace("&amp;", "&");
    format!("{}{path}", base.trim_end_matches('/'))
}

/// `text` with the site's port as `<port>` (after a `:`) and each `Date` header's value as
/// `<date>`, so a response can be compared with a golden.
pub fn normalize(text: &str, port: u16) -> String {
    text.replace(&format!(":{port}"), ":<port>")
        .split('\n')
        .map(|line| match line.strip_prefix("Date: ") {
            Some(rest) if rest.ends_with('\r') => "Date: <date>\r".to_string(),
            Some(_) => "Date: <date>".to_string(),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls6_the_client_reads_heads_urls_and_dates() {
        let r = parse(b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nX: a:b\r\n\r\nno");
        assert_eq!(r.status, 404);
        assert_eq!(r.header("content-type"), Some("text/plain"));
        assert_eq!(r.header("X"), Some("a:b"));
        assert_eq!(r.body, b"no");
        assert_eq!(parse(b"").status, 0);
        assert_eq!(
            split_url("http://127.0.0.1:5/a/b.html"),
            ("127.0.0.1:5", "/a/b.html")
        );
        assert_eq!(split_url("http://127.0.0.1:5"), ("127.0.0.1:5", "/"));
        assert_eq!(
            events_url(
                "http://127.0.0.1:5/",
                r#"<script data-events="/_rdy/events?scope=canon&amp;page=a.html"></script>"#
            ),
            "http://127.0.0.1:5/_rdy/events?scope=canon&page=a.html"
        );
        assert_eq!(
            normalize(
                "HTTP/1.1 200 OK\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nHost: 127.0.0.1:51234\r\n",
                51234
            ),
            "HTTP/1.1 200 OK\r\nDate: <date>\r\nHost: 127.0.0.1:<port>\r\n"
        );
    }
}
