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
    try_raw(addr, request).unwrap_or_else(|e| panic!("connect {addr}: {e}"))
}

/// [`raw`], with a failed connect as its error: nothing listens at `addr`.
pub fn try_raw(addr: SocketAddr, request: &str) -> std::io::Result<Response> {
    let mut s = TcpStream::connect(addr)?;
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
    Ok(parse(&got))
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

/// One answer form of a local approval page, as its markup gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiForm {
    pub reference: String,
    pub card_hash: String,
    /// The hidden briefing token, when the page sends one.
    pub brief: Option<String>,
    /// The value of each submit button, in order.
    pub options: Vec<String>,
    /// The text of each submit button: the label the person chooses.
    pub labels: Vec<String>,
    /// The value of the button marked as recommended, when one is.
    pub recommended: Option<String>,
}

/// `text` with the entities a template's escape writes turned back.
pub fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2f;", "/")
        .replace("&#x2F;", "/")
        .replace("&amp;", "&")
}

/// The value of attribute `name` in the tag text `tag`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let from = tag.find(&key)? + key.len();
    let len = tag[from..].find('"')?;
    Some(unescape(&tag[from..from + len]))
}

/// The answer forms of `page`, in order, read from its markup: what a browser would submit.
pub fn ui_forms(page: &str) -> Vec<UiForm> {
    page.split("<form ")
        .skip(1)
        .map(|rest| {
            let form = rest.split("</form>").next().unwrap_or(rest);
            let mut out = UiForm::default();
            for tag in form.split('<').filter(|t| t.starts_with("input ")) {
                match (attr(tag, "name").as_deref(), attr(tag, "value")) {
                    (Some("ref"), Some(v)) => out.reference = v,
                    (Some("card_hash"), Some(v)) => out.card_hash = v,
                    (Some("brief"), Some(v)) => out.brief = Some(v),
                    _ => {}
                }
            }
            for row in form.split("<li ").skip(1) {
                let Some(tag) = row.split('<').find(|t| t.starts_with("button ")) else {
                    continue;
                };
                let Some(value) = attr(tag, "value") else {
                    continue;
                };
                if row.contains("class=\"ui-rec\"") && out.recommended.is_none() {
                    out.recommended = Some(value.clone());
                }
                out.options.push(value);
                out.labels
                    .push(unescape(tag.split_once('>').map_or("", |(_, text)| text)));
            }
            out
        })
        .collect()
}

/// `value` as a form value: every byte but the unreserved ones as `%XX`.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `POST /~ui/answer` request of `fields` to the site at `authority` (`host:port`), with the
/// exact `Host`, `Origin`, content type and length a browser's form sends.
pub fn ui_answer_request(authority: &str, fields: &[(&str, &str)]) -> String {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!(
        "POST /~ui/answer HTTP/1.1\r\nHost: {authority}\r\nOrigin: http://{authority}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The request that answers `form` with `option`, as a browser submits it.
pub fn ui_answer(authority: &str, form: &UiForm, option: &str) -> String {
    let mut fields = vec![
        ("ref", form.reference.as_str()),
        ("card_hash", form.card_hash.as_str()),
        ("option", option),
    ];
    if let Some(b) = &form.brief {
        fields.push(("brief", b));
    }
    ui_answer_request(authority, &fields)
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

    /// ui9: the forms a page holds are read back as a browser submits them.
    #[test]
    fn ui9_forms_read_their_fields_from_the_markup() {
        let page = r#"<form class="ui-form" method="post"><input type="hidden" name="ref" value="P-a@1"><input type="hidden" name="card_hash" value="ab"><input type="hidden" name="brief" value="b&#x2f;1"><textarea name="note"></textarea><ol><li class="o"><button type="submit" name="option" value="approve">Yes &amp; go</button> <span class="ui-rec">Recommended</span></li><li class="o"><button type="submit" name="option" value="later">No</button></li></ol></form>"#;
        assert_eq!(
            ui_forms(page),
            vec![UiForm {
                reference: "P-a@1".into(),
                card_hash: "ab".into(),
                brief: Some("b/1".into()),
                options: vec!["approve".into(), "later".into()],
                labels: vec!["Yes & go".into(), "No".into()],
                recommended: Some("approve".into()),
            }]
        );
        assert_eq!(
            ui_answer("h:1", &ui_forms(page)[0], "approve"),
            "POST /~ui/answer HTTP/1.1\r\nHost: h:1\r\nOrigin: http://h:1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 51\r\n\r\nref=P-a%401&card_hash=ab&option=approve&brief=b%2F1"
        );
    }

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
