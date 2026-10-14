//! HTTP/1.1 request heads and responses for the live site.
//!
//! A small, closed subset of HTTP/1.1 (ruling F1, D-FV6G9): a request is its head only, a
//! response always closes the connection, and the headers a response sends are a closed list.

/// The largest request head the site reads, in bytes, the empty line included.
pub const MAX_HEAD_BYTES: usize = 8192;
/// The most header lines a request head may hold.
pub const MAX_HEADERS: usize = 64;

/// The site's Content-Security-Policy: the page, its assets and its event stream, all from the
/// site itself, and no framing, no base change and no form posts.
pub const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// The headers every response sends, in this order, after `Date`. With `Content-Type`,
/// `Content-Length`, `Date` and the `Allow` of a 405, they are the only headers the site sends.
pub const BASE_HEADERS: &[(&str, &str)] = &[
    ("Cache-Control", "no-store"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("X-Frame-Options", "DENY"),
    ("Content-Security-Policy", CSP),
    ("Connection", "close"),
];

/// A request method. Only `GET` and `HEAD` have a variant: the guard refuses every other one.
#[derive(Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Other(String),
}

/// A parsed request head: header names in lower case, values trimmed, in the order sent.
#[derive(Debug)]
pub struct Request {
    pub method: Method,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// Every value of header `name` (any case), in order.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Why a request head does not parse.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Over `MAX_HEAD_BYTES` bytes or over `MAX_HEADERS` header lines.
    TooLarge,
    /// Not a request head this site reads.
    Malformed,
}

/// Parses `head`: the bytes up to and including the first empty line.
pub fn parse_head(head: &[u8]) -> Result<Request, ParseError> {
    if head.len() > MAX_HEAD_BYTES {
        return Err(ParseError::TooLarge);
    }
    let text = std::str::from_utf8(head).map_err(|_| ParseError::Malformed)?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let request_line = lines.next().ok_or(ParseError::Malformed)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ParseError::Malformed);
    };
    if method.is_empty() || target.is_empty() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(ParseError::Malformed);
    }
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        other => Method::Other(other.to_string()),
    };
    let mut headers = Vec::new();
    let mut ended = false;
    let mut lines = lines.peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            // The empty line counts only when its own line break follows it: a head cut after
            // the last header's line break is not one.
            ended = lines.peek().is_some();
            break;
        }
        if headers.len() == MAX_HEADERS {
            return Err(ParseError::TooLarge);
        }
        let (name, value) = line.split_once(':').ok_or(ParseError::Malformed)?;
        if name.is_empty() || name.contains([' ', '\t']) {
            return Err(ParseError::Malformed);
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    if !ended {
        return Err(ParseError::Malformed);
    }
    Ok(Request {
        method,
        target: target.to_string(),
        headers,
    })
}

/// A response: its status, the headers it adds to `BASE_HEADERS`, and its body.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// The bytes of `r`: the status line, `Date`, `BASE_HEADERS`, `r.headers`, `Content-Length`
/// (the body's length, also for `HEAD`), an empty line, then the body unless `head_only`.
pub fn to_bytes(r: &Response, head_only: bool, now_secs: i64) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nDate: {}\r\n",
        r.status,
        reason(r.status),
        http_date(now_secs)
    );
    for (name, value) in BASE_HEADERS {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    for (name, value) in &r.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", r.body.len()));
    let mut out = head.into_bytes();
    if !head_only {
        out.extend_from_slice(&r.body);
    }
    out
}

/// The reason phrase of `status`, for the closed set of statuses the site sends.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// `secs` seconds since the epoch as an IMF-fixdate, e.g. `Thu, 01 Jan 1970 00:00:00 GMT`.
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant), the step of `rdy_engine::rfc3339`
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    // 1970-01-01 was a Thursday.
    let weekday = DAYS[(days + 4).rem_euclid(7) as usize];
    format!(
        "{weekday}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        MONTHS[(m - 1) as usize],
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The content type of a served file, by the extension of `path` (a closed list).
pub fn content_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// A plain-text response.
pub fn text(status: u16, body: &str) -> Response {
    Response {
        status,
        headers: vec![("Content-Type", "text/plain; charset=utf-8".to_string())],
        body: body.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls3_a_head_parses_and_a_bad_one_does_not() {
        let r = parse_head(b"GET /a/b.html?x=1 HTTP/1.1\r\nHost: 127.0.0.1:5\r\nX-A:  v \r\n\r\n")
            .unwrap();
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.target, "/a/b.html?x=1");
        assert_eq!(r.all("host"), vec!["127.0.0.1:5"]);
        assert_eq!(r.all("x-a"), vec!["v"]);
        assert_eq!(
            parse_head(b"GET / HTTP/2\r\n\r\n").unwrap_err(),
            ParseError::Malformed
        );
        assert_eq!(
            parse_head(b"GET /\r\n\r\n").unwrap_err(),
            ParseError::Malformed
        );
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nNoColon\r\n\r\n").unwrap_err(),
            ParseError::Malformed
        );
        let big = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEAD_BYTES)
        );
        assert_eq!(
            parse_head(big.as_bytes()).unwrap_err(),
            ParseError::TooLarge
        );
        let many: String = (0..=MAX_HEADERS).map(|i| format!("X{i}: 1\r\n")).collect();
        assert_eq!(
            parse_head(format!("GET / HTTP/1.1\r\n{many}\r\n").as_bytes()).unwrap_err(),
            ParseError::TooLarge
        );
    }

    /// Fix round 1: each parse rule of the brief has an assertion that fails without it.
    #[test]
    fn ls3_each_head_rule_is_pinned() {
        let not_utf8 = b"GET /\xff HTTP/1.1\r\nHost: 127.0.0.1:5\r\n\r\n";
        assert_eq!(parse_head(not_utf8).unwrap_err(), ParseError::Malformed);
        let spaced = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:5\r\nBad Name: x\r\n\r\n";
        assert_eq!(parse_head(spaced).unwrap_err(), ParseError::Malformed);
        let old = parse_head(b"GET / HTTP/1.0\r\nHost: 127.0.0.1:5\r\n\r\n").unwrap();
        assert_eq!(old.method, Method::Get);
        assert_eq!(old.target, "/");
        assert_eq!(old.all("host"), vec!["127.0.0.1:5"]);
    }

    /// Fix round 1: a head is the bytes up to and including the first empty line, so a head cut
    /// before that line ends is malformed.
    #[test]
    fn ls3_a_head_cut_before_its_empty_line_is_malformed() {
        for head in [
            &b"GET / HTTP/1.1\r\nHost: 127.0.0.1:51000\r\n"[..],
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:51000\r\n\r",
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:51000",
            b"GET / HTTP/1.1\r\n",
        ] {
            assert_eq!(
                parse_head(head).unwrap_err(),
                ParseError::Malformed,
                "{}",
                String::from_utf8_lossy(head).escape_debug()
            );
        }
        assert!(parse_head(b"GET / HTTP/1.1\n\n").is_ok());
    }

    #[test]
    fn ls3_a_response_has_the_closed_header_set() {
        let r = Response {
            status: 200,
            headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
            body: b"hi".to_vec(),
        };
        let text = String::from_utf8(to_bytes(&r, false, 0)).unwrap();
        assert_eq!(
            text,
            format!(
                "HTTP/1.1 200 OK\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: {CSP}\r\nConnection: close\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 2\r\n\r\nhi"
            )
        );
        let head = String::from_utf8(to_bytes(&r, true, 0)).unwrap();
        assert!(
            head.ends_with("Content-Length: 2\r\n\r\n"),
            "HEAD keeps the length, drops the body"
        );
        assert!(!text.to_ascii_lowercase().contains("access-control"));
    }

    #[test]
    fn ls3_dates_and_types() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
        assert_eq!(http_date(1_760_227_200), "Sun, 12 Oct 2025 00:00:00 GMT");
        assert_eq!(content_type("a/b.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("assets/fonts/onest.woff2"), "font/woff2");
        assert_eq!(content_type("llms.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type("x.exe"), "application/octet-stream");
    }
}
