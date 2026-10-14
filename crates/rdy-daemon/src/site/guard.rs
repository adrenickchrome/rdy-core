//! The site's closed guard: the checks every request passes, and the routes.
//!
//! The site is read-only and local (D-N5NSN, D-FV6G9): it answers `GET` and `HEAD` only, takes
//! no body, answers only to its own addresses and its own pages, and serves a closed set of
//! routes. Every check is pure. A new check or route needs a founder decision.

use crate::site::http::{Method, Request, Response, text};
use std::net::Ipv4Addr;

/// What the site answers to: its port, and the one LAN address it is bound to, if any.
pub struct Guard {
    pub port: u16,
    pub lan: Option<Ipv4Addr>,
}

/// A scope the site serves: canon at the root, a view under `~view/`, a proposal version under
/// `~proposal/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeRef {
    Canon,
    View(String),
    Proposal(String),
}

impl ScopeRef {
    /// The scope as rdy reads it: `canon`, `view:<name>` or `proposal:<ref>`.
    pub fn text(&self) -> String {
        match self {
            ScopeRef::Canon => "canon".to_string(),
            ScopeRef::View(name) => format!("view:{name}"),
            ScopeRef::Proposal(r) => format!("proposal:{r}"),
        }
    }

    /// Where the scope's pages start, under the site's root: empty for canon.
    pub fn path_prefix(&self) -> String {
        match self {
            ScopeRef::Canon => String::new(),
            ScopeRef::View(name) => format!("~view/{}/", percent_encode(name)),
            ScopeRef::Proposal(r) => format!("~proposal/{r}/"),
        }
    }
}

/// What a request that passes the guard asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// A page or asset of a scope, by its path inside the scope's site.
    Page { scope: ScopeRef, path: String },
    /// The live script.
    LiveJs,
    /// The event stream of an open page: its scope, its path, and the revision and label it has.
    Events {
        scope: ScopeRef,
        page: String,
        rev: String,
        label: String,
    },
}

/// Why the site refuses a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    TooLarge,
    Malformed,
    Body,
    Method,
    Host,
    Origin,
    NotFound,
    Busy,
    Broken,
}

impl Guard {
    /// The `Host` values the site answers to.
    pub fn hosts(&self) -> Vec<String> {
        let port = self.port;
        let mut hosts = vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ];
        if let Some(lan) = self.lan {
            hosts.push(format!("{lan}:{port}"));
        }
        hosts
    }

    /// Runs the closed list of checks on `r`, in order, and routes it.
    pub fn check(&self, r: &Request) -> Result<Route, Refusal> {
        // 1. GET and HEAD only.
        if let Method::Other(_) = r.method {
            return Err(Refusal::Method);
        }
        // 2. No body.
        if r.all("content-length").iter().any(|v| *v != "0")
            || !r.all("transfer-encoding").is_empty()
        {
            return Err(Refusal::Body);
        }
        // 3. Exactly one Host, one of the site's own.
        let host = match r.all("host").as_slice() {
            [one] if self.is_own_host(one) => *one,
            _ => return Err(Refusal::Host),
        };
        // 4. No Origin, or the site's own.
        for origin in r.all("origin") {
            let own = origin
                .strip_prefix("http://")
                .is_some_and(|h| h.eq_ignore_ascii_case(host));
            if !own {
                return Err(Refusal::Origin);
            }
        }
        // 5. A clean path.
        let (raw_path, query) = match r.target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (r.target.as_str(), None),
        };
        let path = clean_path(raw_path).ok_or(Refusal::NotFound)?;
        // 6. The routes.
        route(&r.method, &path, query)
    }

    fn is_own_host(&self, value: &str) -> bool {
        let port = self.port;
        value.eq_ignore_ascii_case(&format!("localhost:{port}"))
            || value == format!("127.0.0.1:{port}")
            || value == format!("[::1]:{port}")
            || self.lan.is_some_and(|lan| value == format!("{lan}:{port}"))
    }
}

/// Routes a clean path (no leading `/`).
fn route(method: &Method, path: &str, query: Option<&str>) -> Result<Route, Refusal> {
    if path == "_rdy/live.js" {
        return Ok(Route::LiveJs);
    }
    if path == "_rdy/events" {
        if *method == Method::Head {
            return Err(Refusal::Method);
        }
        return events(query.unwrap_or(""));
    }
    if path == "_rdy" || path.starts_with("_rdy/") {
        return Err(Refusal::NotFound);
    }
    if path.starts_with('~') {
        let mut parts = path.splitn(3, '/');
        let (Some(kind), Some(name), Some(rest)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(Refusal::NotFound);
        };
        let scope = match kind {
            "~view" if view_name_ok(name) => ScopeRef::View(name.to_string()),
            "~proposal" if proposal_ref_ok(name) => ScopeRef::Proposal(name.to_string()),
            _ => return Err(Refusal::NotFound),
        };
        return Ok(Route::Page {
            scope,
            path: with_index(rest),
        });
    }
    Ok(Route::Page {
        scope: ScopeRef::Canon,
        path: with_index(path),
    })
}

/// The event route from its query: `scope`, `page`, `rev` and `label` (first of each; other keys
/// are ignored).
fn events(query: &str) -> Result<Route, Refusal> {
    let (mut scope, mut page, mut rev, mut label) = (None, None, None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let slot = match key {
            "scope" => &mut scope,
            "page" => &mut page,
            "rev" => &mut rev,
            "label" => &mut label,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(query_decode(value).ok_or(Refusal::Malformed)?);
        }
    }
    let (Some(scope), Some(page), Some(rev), Some(label)) = (scope, page, rev, label) else {
        return Err(Refusal::Malformed);
    };
    let scope = parse_scope(&scope).ok_or(Refusal::Malformed)?;
    if !is_rev(&rev) || !is_rev(&label) {
        return Err(Refusal::Malformed);
    }
    let page = clean_path(&format!("/{page}")).ok_or(Refusal::Malformed)?;
    Ok(Route::Events {
        scope,
        page: with_index(&page),
        rev,
        label,
    })
}

/// `canon`, `view:<name>` or `proposal:<ref>`.
fn parse_scope(text: &str) -> Option<ScopeRef> {
    if text == "canon" {
        return Some(ScopeRef::Canon);
    }
    if let Some(name) = text.strip_prefix("view:") {
        return view_name_ok(name).then(|| ScopeRef::View(name.to_string()));
    }
    let r = text.strip_prefix("proposal:")?;
    proposal_ref_ok(r).then(|| ScopeRef::Proposal(r.to_string()))
}

/// 16 lowercase hex digits: a page revision or a label hash.
fn is_rev(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A view name: one clean, non-empty path segment.
fn view_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// `P-` and 4 to 64 hex digits (either case), then optionally `@` and a whole number from 1: the
/// forms `resolve_in` reads.
fn proposal_ref_ok(r: &str) -> bool {
    let Some(rest) = r.strip_prefix("P-") else {
        return false;
    };
    let (prefix, n) = match rest.split_once('@') {
        Some((prefix, n)) => (prefix, Some(n)),
        None => (rest, None),
    };
    let prefix_ok =
        (4..=64).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_hexdigit());
    let n_ok = n.is_none_or(|n| {
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.parse::<u32>().is_ok_and(|n| n >= 1)
    });
    prefix_ok && n_ok
}

/// A path with `index.html` appended when it is empty or ends in `/`.
fn with_index(path: &str) -> String {
    if path.is_empty() || path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path.to_string()
    }
}

/// Check 5: `raw` percent-decoded, without its leading `/`. `None` when it does not start with
/// `/`, does not decode to UTF-8, holds a NUL or a `\`, or has a `.` or `..` segment or an empty
/// segment other than a trailing one.
fn clean_path(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix('/')?;
    let decoded = String::from_utf8(percent_decode(rest, false)?).ok()?;
    if decoded.contains(['\0', '\\']) {
        return None;
    }
    let segments: Vec<&str> = decoded.split('/').collect();
    let last = segments.len() - 1;
    for (i, seg) in segments.iter().enumerate() {
        if *seg == "." || *seg == ".." || (seg.is_empty() && i != last) {
            return None;
        }
    }
    Some(decoded)
}

/// A query value, percent-decoded with `+` as a space (form encoding).
fn query_decode(raw: &str) -> Option<String> {
    String::from_utf8(percent_decode(raw, true)?).ok()
}

/// `%XX` escapes decoded; `None` for a `%` without two hex digits.
fn percent_decode(raw: &str, plus_is_space: bool) -> Option<Vec<u8>> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = hex_value(*bytes.get(i + 1)?)?;
                let lo = hex_value(*bytes.get(i + 2)?)?;
                out.push(hi << 4 | lo);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(out)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `s` with every byte outside `A-Z a-z 0-9 - . _ ~` percent-encoded.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The response for a refusal: plain text, with `Allow` on a 405.
pub fn refusal(f: Refusal) -> Response {
    let (status, body) = match f {
        Refusal::TooLarge => (431, "The request head is too large."),
        Refusal::Malformed => (400, "Bad request."),
        Refusal::Body => (400, "This site takes no request body."),
        Refusal::Method => (405, "This site is read-only: it answers GET and HEAD only."),
        Refusal::Host => (403, "This site answers only to its own addresses."),
        Refusal::Origin => (403, "This site answers only requests from its own pages."),
        Refusal::NotFound => (
            404,
            "Not found. Ask your agent for the site's link (rdy_site), or run rdy site.",
        ),
        Refusal::Busy => (
            503,
            "Too many live pages are open. Close some tabs, then reload this one.",
        ),
        Refusal::Broken => (500, "The site could not be built. The daemon log says why."),
    };
    let mut r = text(status, body);
    if status == 405 {
        r.headers.push(("Allow", "GET, HEAD".to_string()));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::http::{Request, parse_head};
    use std::net::Ipv4Addr;

    fn g() -> Guard {
        Guard {
            port: 51000,
            lan: None,
        }
    }

    fn lan() -> Guard {
        Guard {
            port: 51000,
            lan: Some(Ipv4Addr::new(192, 168, 1, 20)),
        }
    }

    /// A request head from lines joined by `\n`.
    fn req(lines: &str) -> Request {
        let mut head = lines.replace('\n', "\r\n");
        head.push_str("\r\n\r\n");
        parse_head(head.as_bytes()).unwrap()
    }

    fn get(target: &str, host: &str) -> Request {
        req(&format!("GET {target} HTTP/1.1\nHost: {host}"))
    }

    fn canon(path: &str) -> Route {
        Route::Page {
            scope: ScopeRef::Canon,
            path: path.into(),
        }
    }

    #[test]
    fn ls3_a_page_route_needs_a_loopback_host() {
        for host in [
            "127.0.0.1:51000",
            "localhost:51000",
            "LOCALHOST:51000",
            "[::1]:51000",
        ] {
            assert_eq!(
                g().check(&get("/system/site.html", host)),
                Ok(canon("system/site.html"))
            );
        }
        assert_eq!(
            g().check(&get("/", "127.0.0.1:51000")),
            Ok(canon("index.html"))
        );
        assert_eq!(
            g().check(&get("/system/", "127.0.0.1:51000")),
            Ok(canon("system/index.html"))
        );
    }

    #[test]
    fn ls3_a_foreign_host_is_refused() {
        for host in [
            "rebound.invalid:51000",
            "127.0.0.1:51001",
            "127.0.0.1",
            "0.0.0.0:51000",
            "192.168.1.20:51000",
        ] {
            assert_eq!(
                g().check(&get("/index.html", host)),
                Err(Refusal::Host),
                "{host}"
            );
        }
        let two = req("GET /index.html HTTP/1.1\nHost: 127.0.0.1:51000\nHost: 127.0.0.1:51000");
        assert_eq!(g().check(&two), Err(Refusal::Host));
        assert_eq!(
            g().check(&req("GET /index.html HTTP/1.1")),
            Err(Refusal::Host)
        );
    }

    #[test]
    fn ls3_lan_mode_accepts_its_one_address_only() {
        assert_eq!(
            lan().check(&get("/index.html", "192.168.1.20:51000")),
            Ok(canon("index.html"))
        );
        assert_eq!(
            lan().check(&get("/index.html", "127.0.0.1:51000")),
            Ok(canon("index.html"))
        );
        for host in [
            "192.168.1.21:51000",
            "mymac.local:51000",
            "rebound.invalid:51000",
            "192.168.1.20:51001",
        ] {
            assert_eq!(
                lan().check(&get("/index.html", host)),
                Err(Refusal::Host),
                "{host}"
            );
        }
    }

    #[test]
    fn ls3_a_foreign_origin_is_refused() {
        let own =
            req("GET /index.html HTTP/1.1\nHost: localhost:51000\nOrigin: http://localhost:51000");
        assert!(g().check(&own).is_ok());
        for origin in [
            "http://rebound.invalid",
            "http://127.0.0.1:51000",
            "null",
            "https://localhost:51000",
        ] {
            let r = req(&format!(
                "GET /index.html HTTP/1.1\nHost: localhost:51000\nOrigin: {origin}"
            ));
            assert_eq!(g().check(&r), Err(Refusal::Origin), "{origin}");
        }
    }

    #[test]
    fn ls3_only_get_and_head_without_a_body() {
        for m in [
            "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "TRACE", "CONNECT",
        ] {
            let r = req(&format!("{m} /index.html HTTP/1.1\nHost: 127.0.0.1:51000"));
            assert_eq!(g().check(&r), Err(Refusal::Method), "{m}");
        }
        assert!(
            g().check(&req("HEAD /index.html HTTP/1.1\nHost: 127.0.0.1:51000"))
                .is_ok()
        );
        let body = req("GET /index.html HTTP/1.1\nHost: 127.0.0.1:51000\nContent-Length: 3");
        assert_eq!(g().check(&body), Err(Refusal::Body));
        let chunked =
            req("GET /index.html HTTP/1.1\nHost: 127.0.0.1:51000\nTransfer-Encoding: chunked");
        assert_eq!(g().check(&chunked), Err(Refusal::Body));
        let zero = req("GET /index.html HTTP/1.1\nHost: 127.0.0.1:51000\nContent-Length: 0");
        assert!(g().check(&zero).is_ok());
    }

    #[test]
    fn ls3_a_bad_path_is_not_found() {
        for t in [
            "/../index.html",
            "/a/%2e%2e/index.html",
            "/a//b.html",
            "/a%5cb.html",
            "/a%00b.html",
            "/_rdy/other",
            "/~elsewhere/x/index.html",
            "/~view//index.html",
            "/~proposal/X-1/index.html",
            "/~proposal/P-ab@x/index.html",
            "/~proposal/P-7QX2M/index.html",
            "/~proposal/P-7a2c9e@0/index.html",
            "http://rebound.invalid/index.html",
        ] {
            assert_eq!(
                g().check(&get(t, "127.0.0.1:51000")),
                Err(Refusal::NotFound),
                "{t}"
            );
        }
    }

    #[test]
    fn ls3_the_scope_routes() {
        let h = "127.0.0.1:51000";
        assert_eq!(
            g().check(&get("/~view/work-agent-a/system/site.html", h)),
            Ok(Route::Page {
                scope: ScopeRef::View("work-agent-a".into()),
                path: "system/site.html".into()
            })
        );
        assert_eq!(
            g().check(&get("/~proposal/P-7a2c9e@3/", h)),
            Ok(Route::Page {
                scope: ScopeRef::Proposal("P-7a2c9e@3".into()),
                path: "index.html".into()
            })
        );
        assert_eq!(
            g().check(&get("/~proposal/P-7a2c9e/index.html", h)),
            Ok(Route::Page {
                scope: ScopeRef::Proposal("P-7a2c9e".into()),
                path: "index.html".into()
            })
        );
        assert_eq!(ScopeRef::View("a b".into()).path_prefix(), "~view/a%20b/");
        assert_eq!(
            ScopeRef::Proposal("P-7a2c9e@3".into()).text(),
            "proposal:P-7a2c9e@3"
        );
    }

    #[test]
    fn ls3_the_live_routes() {
        let h = "127.0.0.1:51000";
        assert_eq!(g().check(&get("/_rdy/live.js", h)), Ok(Route::LiveJs));
        let ev = "/_rdy/events?scope=view:draft&page=system/site.html&rev=00112233aabbccdd&label=ffeeddccbbaa9988&x=1";
        assert_eq!(
            g().check(&get(ev, h)),
            Ok(Route::Events {
                scope: ScopeRef::View("draft".into()),
                page: "system/site.html".into(),
                rev: "00112233aabbccdd".into(),
                label: "ffeeddccbbaa9988".into()
            })
        );
        let bad = "/_rdy/events?scope=canon&page=index.html&rev=XYZ&label=ffeeddccbbaa9988";
        assert_eq!(g().check(&get(bad, h)), Err(Refusal::Malformed));
        let bad_scope =
            "/_rdy/events?scope=tag:v1&page=index.html&rev=00112233aabbccdd&label=00112233aabbccdd";
        assert_eq!(g().check(&get(bad_scope, h)), Err(Refusal::Malformed));
        let head = req(&format!(
            "HEAD /_rdy/events?scope=canon&page=index.html&rev=00112233aabbccdd&label=00112233aabbccdd HTTP/1.1\nHost: {h}"
        ));
        assert_eq!(g().check(&head), Err(Refusal::Method));
    }

    #[test]
    fn ls3_every_refusal_has_its_text() {
        let r = refusal(Refusal::Method);
        assert_eq!(r.status, 405);
        assert!(r.headers.contains(&("Allow", "GET, HEAD".into())));
        assert_eq!(
            r.body,
            b"This site is read-only: it answers GET and HEAD only."
        );
        assert_eq!(
            refusal(Refusal::NotFound).body,
            b"Not found. Ask your agent for the site's link (rdy_site), or run rdy site.".to_vec()
        );
        assert_eq!(
            refusal(Refusal::Host).body,
            b"This site answers only to its own addresses.".to_vec()
        );
        assert_eq!(refusal(Refusal::TooLarge).status, 431);
        assert_eq!(refusal(Refusal::Busy).status, 503);
        assert_eq!(refusal(Refusal::Broken).status, 500);
    }
}
