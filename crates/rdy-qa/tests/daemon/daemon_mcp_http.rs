//! mht6 (D-6M0AF, D-44NVC, D-QG54Z, D-5GFFX, D-4PHYQ): MCP over HTTP on the site's loopback
//! port, through a real daemon and real TCP. The guard runs in its fixed order (the head, the
//! listener, the project, Host, Origin, the bearer, the method, the body framing and bound), and
//! only then does rmcp see the request. Every test reads the endpoint from the daemon's own site
//! info (W5) and the bearer from the project's token file; the client is `rdy_qa::mcp_http`, its
//! own HTTP framing.
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::site::server::MAX_CONNECTIONS;
use rdy_daemon::site::ui_request::UI_POST;
use rdy_daemon::site::{addr, ensure_profile_site, mcp};
use rdy_daemon::{ExitReason, Shared};
use rdy_qa::daemon::Hosted;
use rdy_qa::golden::tiny;
use rdy_qa::mcp_http::{
    Endpoint, LEGACY, MODERN, Reply, call, claim, initialize, legacy, listen, modern, modern_meta,
    pairs, request, send, send_parts,
};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// A hang guard for what a test waits on: a failure message, never a measure.
const T: Duration = Duration::from_secs(60);

/// The 401 body of the transport table.
const NO_TOKEN: &str = "this endpoint needs the project's MCP token; run rdy mcp --http-config";
/// The 403 body for any Origin.
const ORIGIN: &str = "MCP over HTTP does not accept an Origin header.";
/// The 413 body (D-QG54Z).
const OVER: &str = "the MCP request body is over 16777216 bytes";
/// The body bound, in bytes.
const BOUND: usize = 16 * 1024 * 1024;

const ON: &str = "serve:\n  mcp_http: true\n";
const OFF: &str = "serve:\n  mcp_http: false\n";

/// A fresh project in `dir` with `config` in its `.rdy/local.yaml`.
fn project(config: &str) -> tempfile::TempDir {
    let (dir, r) = tiny();
    drop(r);
    local(dir.path(), config);
    dir
}

fn local(dir: &Path, config: &str) {
    std::fs::write(dir.join(".rdy/local.yaml"), config).unwrap();
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().expect("running").shared())
}

/// The site's loopback URL as the daemon reports it.
fn url(h: &Hosted) -> String {
    shared(h)
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens")
}

/// The project's bearer, as its private token file holds it.
fn token(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join(".rdy/run/mcp-token")).expect("the token");
    text.strip_suffix('\n').expect("one LF").to_string()
}

fn endpoint(h: &Hosted) -> Endpoint {
    Endpoint::of(&url(h), &token(h.dir.path()))
}

/// A daemon with MCP over HTTP on, and its endpoint.
fn on() -> (Hosted, Endpoint) {
    let h = Hosted::start_in(project(ON));
    let ep = endpoint(&h);
    (h, ep)
}

/// Waits until `f` holds, under the hang guard.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + T;
    while !f() {
        assert!(std::time::Instant::now() < end, "{what}");
        std::thread::yield_now();
    }
}

/// The admitted calls and streams of the daemon.
fn activity(h: &Hosted) -> (usize, usize) {
    let a = shared(h).http.get().expect("admission").activity();
    (a.calls, a.streams)
}

fn assert_text(r: &Reply, status: u16, text: &str) {
    assert_eq!((r.status, r.text().as_str()), (status, text), "{}", r.head);
}

/// A raw request with `head_lines` (each ending CRLF) and `body`, on its own connection.
fn raw(ep: &Endpoint, head: &str, body: &[u8]) -> Reply {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    send(ep.addr, &bytes)
}

fn overview(id: u64, actor: &str, run: &str) -> Value {
    call(id, "rdy_overview", json!({}), claim(actor, run))
}

/// A modern call of `name` for the claim.
fn modern_call(ep: &Endpoint, id: u64, name: &str, args: Value, actor: &str, run: &str) -> Reply {
    let h = modern("tools/call", Some(name));
    ep.post(
        &pairs(&h),
        &call(id, name, args, modern_meta(claim(actor, run))),
    )
}

/// A modern listen of `uris` for the claim, its acknowledgement read.
fn open_listen(ep: &Endpoint, uris: &[&str], actor: &str, run: &str) -> rdy_qa::mcp_http::Stream {
    let h = modern("subscriptions/listen", None);
    let mut s = ep.open(&pairs(&h), &listen(9, uris, modern_meta(claim(actor, run))));
    assert_eq!(s.status, 200, "{}", s.head);
    assert_eq!(s.header("content-type"), Some("text/event-stream"));
    let ack = s.next(T).expect("the acknowledgement");
    assert!(ack.to_string().contains("acknowledged"), "{ack}");
    s
}

#[test]
fn mht6_supported_revisions_use_stateless_http() {
    let (h, ep) = on();
    let stateless = |r: &Reply| {
        assert!(r.header("mcp-session-id").is_none(), "{}", r.head);
        assert!(r.header("set-cookie").is_none(), "{}", r.head);
    };
    for rev in LEGACY {
        // a legacy initialize needs no header and makes no session
        let init = ep.post(&[], &initialize(1, rev));
        assert_eq!(init.status, 200, "{} {}", init.head, init.text());
        assert_eq!(
            init.reply()["result"]["protocolVersion"],
            rev,
            "{}",
            init.text()
        );
        stateless(&init);
        // a later request states its revision and its own claim, on a new connection
        let list = ep.post(&pairs(&legacy(rev)), &request(2, "tools/list", json!({})));
        assert_eq!(list.status, 200, "{}", list.text());
        assert!(
            list.reply()["result"]["tools"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t["name"] == "rdy_session")),
            "{}",
            list.text()
        );
        stateless(&list);
        let ov = ep.post(&pairs(&legacy(rev)), &overview(3, "agent:r", "r1"));
        let (err, v) = ov.tool();
        assert!(!err, "{v}");
        assert_eq!(v["session"], json!({"actor": "agent:r", "view": "r1"}));
        stateless(&ov);
        // a claimless call is the tool's own refusal
        let none = ep.post(
            &pairs(&legacy(rev)),
            &call(4, "rdy_overview", json!({}), json!({})),
        );
        let (err, v) = none.tool();
        assert!(err);
        assert_eq!(v["code"], "E_NO_SESSION", "{v}");
    }
    // the modern revision: its headers and metadata on every request
    let h2 = modern("tools/list", None);
    let list = ep.post(
        &pairs(&h2),
        &request(5, "tools/list", json!({"_meta": modern_meta(json!({}))})),
    );
    assert_eq!(list.status, 200, "{}", list.text());
    stateless(&list);
    let ov = modern_call(&ep, 6, "rdy_overview", json!({}), "agent:m", "m1");
    let (err, v) = ov.tool();
    assert!(!err, "{v}");
    assert_eq!(v["session"]["actor"], "agent:m");
    stateless(&ov);
    // any other revision is rmcp's own refusal, the SDK's missing-header fallback too
    for rev in ["2025-03-26", "2024-11-05", "2099-01-01"] {
        let r = ep.post(&pairs(&legacy(rev)), &request(7, "tools/list", json!({})));
        assert_text(
            &r,
            400,
            &format!("Bad Request: Unsupported MCP-Protocol-Version: {rev}"),
        );
        let r = ep.post(&[], &initialize(8, rev));
        assert_text(
            &r,
            400,
            &format!("Bad Request: Unsupported MCP-Protocol-Version: {rev}"),
        );
    }
    let r = ep.post(&[], &request(9, "tools/list", json!({})));
    assert_text(
        &r,
        400,
        "Bad Request: Unsupported MCP-Protocol-Version: 2025-03-26",
    );
    let r = ep.post(
        &[],
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_text(
        &r,
        400,
        "Bad Request: Unsupported MCP-Protocol-Version: 2025-03-26",
    );
    // a header named twice is refused
    let r = ep.post(
        &[
            ("MCP-Protocol-Version", LEGACY[0]),
            ("MCP-Protocol-Version", LEGACY[1]),
        ],
        &request(10, "tools/list", json!({})),
    );
    assert_eq!(r.status, 400, "{}", r.head);
    // older headers and modern metadata separately: modern metadata under an older header, and
    // a modern header with no metadata, are rmcp's refusals before any claim
    let r = ep.post(
        &pairs(&legacy(LEGACY[1])),
        &call(
            11,
            "rdy_overview",
            json!({}),
            modern_meta(claim("agent:m", "m1")),
        ),
    );
    assert_eq!(r.status, 400, "{} {}", r.head, r.text());
    let r = ep.post(
        &pairs(&modern("tools/call", Some("rdy_overview"))),
        &overview(12, "agent:m", "m1"),
    );
    assert_eq!(r.status, 400, "{} {}", r.head, r.text());
    // an accepted notification is 202 with no body
    let r = ep.post(
        &pairs(&legacy(LEGACY[0])),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!((r.status, r.body.len()), (202, 0), "{}", r.head);
    drop(h);
}

#[test]
fn mht6_local_process_without_token_cannot_call() {
    let (h, ep) = on();
    let rev = legacy(LEGACY[1]);
    let body = overview(1, "agent:t", "t1").to_string();
    let send_with = |auth: &[&str], extra: &str, body: &str| {
        let mut head = format!(
            "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\n",
            ep.authority, rev[0].1
        );
        for a in auth {
            head.push_str(&format!("Authorization: {a}\r\n"));
        }
        head.push_str(extra);
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        raw(&ep, &head, body.as_bytes())
    };
    let refused = |r: &Reply| {
        assert_text(r, 401, NO_TOKEN);
        assert_eq!(r.header("www-authenticate"), Some("Bearer"), "{}", r.head);
        assert!(!r.has_header_prefix("access-control-"), "{}", r.head);
    };
    let good = format!("Bearer {}", ep.token);
    let mut other = ep.token.clone().into_bytes();
    other[0] = if other[0] == b'A' { b'B' } else { b'A' };
    let other = format!("Bearer {}", String::from_utf8(other).unwrap());
    for auth in [
        vec![],
        vec![other.as_str()],
        vec!["Bearer"],
        vec!["Bearer "],
        vec![format!("bearer {}", ep.token).leak() as &str],
        vec![format!("Bearer  {}", ep.token).leak() as &str],
        vec![format!("Bearer {}=", ep.token).leak() as &str],
        vec![format!("Bearer {}", ep.token.to_lowercase()).leak() as &str],
        vec![format!("Basic {}", ep.token).leak() as &str],
        // exactly one Authorization: a second one, even the right one twice, is refused
        vec![good.as_str(), good.as_str()],
        vec![other.as_str(), good.as_str()],
    ] {
        refused(&send_with(&auth, "", &body));
    }
    // a credential in a cookie or another header is no bearer, and spoils a good one
    refused(&send_with(
        &[],
        &format!("Cookie: rdy={}\r\n", ep.token),
        &body,
    ));
    refused(&send_with(
        &[good.as_str()],
        &format!("Cookie: rdy={}\r\n", ep.token),
        &body,
    ));
    refused(&send_with(
        &[],
        &format!("X-Rdy-Token: {}\r\n", ep.token),
        &body,
    ));
    // the bearer is checked before the body is parsed or any tool runs
    refused(&send_with(&[], "", "{not json"));
    assert_eq!(activity(&h), (0, 0), "no refused request was admitted");
    // a token in the URL reaches no MCP dispatch: the site's own method refusal
    let r = raw(
        &ep,
        &format!(
            "POST /~mcp?token={} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n\r\n",
            ep.token, ep.authority
        ),
        b"",
    );
    assert_eq!(r.body, refusal(Refusal::Method).body, "{}", r.head);
    // the right bearer alone is let in
    let ok = send_with(&[good.as_str()], "", &body);
    assert!(!ok.tool().0, "{}", ok.text());
}

#[test]
fn mht6_web_page_origin_and_preflight_cannot_act() {
    let (_h, ep) = on();
    let body = overview(1, "agent:w", "w1").to_string();
    let own = format!("http://{}", ep.authority);
    let lan = format!("http://localhost:{}", ep.port());
    for origin in [
        "http://evil.example",
        "null",
        "",
        own.as_str(),
        lan.as_str(),
    ] {
        for token in [true, false] {
            let mut head = format!(
                "POST /~mcp HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\n",
                ep.authority, LEGACY[0]
            );
            if token {
                head.push_str(&format!("Authorization: Bearer {}\r\n", ep.token));
            }
            head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            let r = raw(&ep, &head, body.as_bytes());
            assert_text(&r, 403, ORIGIN);
            assert!(!r.has_header_prefix("access-control-"), "{}", r.head);
        }
    }
    // a preflight: the Origin is refused first, with no grant
    let r = raw(
        &ep,
        &format!(
            "OPTIONS /~mcp HTTP/1.1\r\nHost: {}\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n",
            ep.authority
        ),
        b"",
    );
    assert_text(&r, 403, ORIGIN);
    assert!(!r.has_header_prefix("access-control-"), "{}", r.head);
    // without an Origin and with the bearer, OPTIONS is a method POST does not share
    let r = raw(
        &ep,
        &format!(
            "OPTIONS /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
            ep.authority, ep.token
        ),
        b"",
    );
    assert_eq!(r.status, 405, "{}", r.head);
    assert_eq!(r.header("allow"), Some(UI_POST));
    assert!(!r.has_header_prefix("access-control-"), "{}", r.head);
}

#[test]
fn mht6_rebinding_host_cannot_act() {
    let (_h, ep) = on();
    let body = overview(1, "agent:h", "h1").to_string();
    let port = ep.port();
    let other = if port == u16::MAX { port - 1 } else { port + 1 };
    let host_refusal = refusal(Refusal::Host);
    let attempt = |hosts: &[&str], origin: bool, token: bool| {
        let mut head = "POST /~mcp HTTP/1.1\r\n".to_string();
        for h in hosts {
            head.push_str(&format!("Host: {h}\r\n"));
        }
        if origin {
            head.push_str("Origin: http://attacker.example\r\n");
        }
        if token {
            head.push_str(&format!("Authorization: Bearer {}\r\n", ep.token));
        }
        head.push_str(&format!(
            "Accept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\nContent-Length: {}\r\n\r\n",
            LEGACY[0],
            body.len()
        ));
        raw(&ep, &head, body.as_bytes())
    };
    let rebound = format!("attacker.example:{port}");
    let lan = format!("192.168.1.20:{port}");
    let wrong_port = format!("127.0.0.1:{other}");
    let own = format!("127.0.0.1:{port}");
    for hosts in [
        vec![rebound.as_str()],
        vec!["attacker.example"],
        vec!["localhost"],
        vec!["127.0.0.1"],
        vec![wrong_port.as_str()],
        // a LAN name with the port: MCP takes only loopback names (C6)
        vec![lan.as_str()],
        vec![],
        vec![own.as_str(), own.as_str()],
        vec![own.as_str(), rebound.as_str()],
    ] {
        for (origin, token) in [(false, true), (true, false), (true, true), (false, false)] {
            let r = attempt(&hosts, origin, token);
            assert_eq!(
                (r.status, &r.body),
                (403, &host_refusal.body),
                "{hosts:?}: {}",
                r.head
            );
        }
    }
    // every loopback spelling with the port is the endpoint's own
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("LocalHost:{port}"),
        format!("[::1]:{port}"),
    ] {
        let r = attempt(&[host.as_str()], false, true);
        assert!(!r.tool().0, "{host}: {}", r.text());
    }
    // the listener's own LAN address is a page host, and still no MCP host (C6)
    let lan_ip = Ipv4Addr::new(192, 168, 1, 20);
    let parsed = rdy_daemon::site::http::parse_head(
        format!("POST /~mcp HTTP/1.1\r\nHost: {lan_ip}:{port}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    assert!(
        rdy_daemon::site::guard::Guard::full(port, Some(lan_ip))
            .hosts()
            .contains(&format!("{lan_ip}:{port}"))
    );
    assert_eq!(mcp::host(&parsed, port), Err(Refusal::Host));
}

#[test]
fn mht6_lan_and_profile_have_no_mcp_actions() {
    let (h, ep) = on();
    let post = |port: u16| {
        let body = overview(1, "agent:p", "p1").to_string();
        raw(
            &ep,
            &format!(
                "POST /~mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\nContent-Length: {}\r\n\r\n",
                ep.token,
                LEGACY[0],
                body.len()
            ),
            body.as_bytes(),
        )
    };
    // the profile-only listener of a profile: its normal refusal, no MCP answer
    let info = ensure_profile_site(shared(&h), "implicit:org").expect("a profile-only site");
    let purl = info.url.expect("it listens");
    let pport: u16 = purl
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches('/')
        .parse()
        .unwrap();
    assert_ne!(pport, ep.port());
    let pep = Endpoint::of(&purl, &ep.token);
    let body = overview(1, "agent:p", "p1").to_string();
    let r = raw(
        &pep,
        &pep.head(&[("MCP-Protocol-Version", LEGACY[0])], body.len()),
        body.as_bytes(),
    );
    assert_eq!(
        (r.status, &r.body),
        (405, &refusal(Refusal::Method).body),
        "{}",
        r.head
    );
    let get = send(
        pep.addr,
        format!(
            "GET /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
            pep.authority, ep.token
        )
        .as_bytes(),
    );
    assert_eq!(get.status, 404, "{}", get.head);
    assert!(get.header("www-authenticate").is_none());
    // the full listener answers the same request
    assert!(!post(ep.port()).tool().0);
    // a LAN or profile-only socket never classifies as the MCP route, whatever the Host
    let request = rdy_daemon::site::http::parse_head(
        format!(
            "POST /~mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            ep.port()
        )
        .as_bytes(),
    )
    .unwrap();
    let lan = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 20), ep.port()));
    let lo = SocketAddr::from((Ipv4Addr::LOCALHOST, ep.port()));
    assert!(!mcp::is_route(&request, lan, true));
    assert!(!mcp::is_route(&request, lo, false));
    assert!(mcp::is_route(&request, lo, true));
    // a query, an encoded alias or a trailing slash is no MCP route
    for target in [
        "/~mcp?x=1",
        "/~mcp/",
        "/%7Emcp",
        "/~mc%70",
        "//~mcp",
        "/~MCP",
    ] {
        let r = rdy_daemon::site::http::parse_head(
            format!("POST {target} HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n").as_bytes(),
        )
        .unwrap();
        assert!(!mcp::is_route(&r, lo, true), "{target}");
    }
}

#[test]
fn mht6_post_streams_and_get_has_the_settled_contract() {
    let (h, ep) = on();
    // D-44NVC's order: Origin 403, then no token 401, then the method 405
    for method in ["GET", "HEAD", "DELETE", "PUT", "OPTIONS"] {
        let r = raw(
            &ep,
            &format!(
                "{method} /~mcp HTTP/1.1\r\nHost: {}\r\nOrigin: http://evil.example\r\nAccept: text/event-stream\r\n\r\n",
                ep.authority
            ),
            b"",
        );
        assert_eq!(r.status, 403, "{method}: {}", r.head);
        let r = raw(
            &ep,
            &format!(
                "{method} /~mcp HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
                ep.authority
            ),
            b"",
        );
        assert_eq!(r.status, 401, "{method}: {}", r.head);
        let r = raw(
            &ep,
            &format!(
                "{method} /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\nMCP-Protocol-Version: {MODERN}\r\n\r\n",
                ep.authority, ep.token
            ),
            b"",
        );
        assert_eq!(r.status, 405, "{method}: {}", r.head);
        assert_eq!(r.header("allow"), Some(UI_POST), "{method}");
        if method == "HEAD" {
            assert!(r.body.is_empty());
        }
        assert!(
            r.header("content-type")
                .is_none_or(|t| !t.starts_with("text/event-stream"))
        );
    }
    assert_eq!(activity(&h), (0, 0));
    // the modern listen is a long-lived POST response: acknowledged, then its notifications
    let mut s = open_listen(&ep, &["rdy://view/s1"], "agent:s", "s1");
    until("the listen counts as a stream", || activity(&h).1 == 1);
    let w = modern_call(
        &ep,
        20,
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": "one", "title": "one"}}]}),
        "agent:s",
        "s1",
    );
    assert!(!w.tool().0, "{}", w.text());
    let m = s.next(T).expect("a notification on the open POST");
    assert_eq!(m["method"], "notifications/resources/updated", "{m}");
    assert_eq!(m["params"]["uri"], "rdy://view/s1", "{m}");
    // closing the response ends the listen and gives its stream back
    drop(s);
    until("the closed listen ends", || activity(&h) == (0, 0));
}

#[test]
fn mht6_chunked_and_prefetched_bodies_reach_one_request() {
    let (h, ep) = on();
    let body = overview(1, "agent:c", "c1").to_string();
    let rev = [("MCP-Protocol-Version", LEGACY[1])];
    // the head and the whole body in one write: the body bytes read with the head reach rmcp
    let r = raw(&ep, &ep.head(&rev, body.len()), body.as_bytes());
    assert!(!r.tool().0, "{}", r.text());
    // the head, then the body byte by byte in separate writes
    let head = ep.head(&rev, body.len());
    let mut parts: Vec<&[u8]> = vec![head.as_bytes()];
    parts.extend(body.as_bytes().chunks(1));
    let r = send_parts(ep.addr, &parts);
    assert!(!r.tool().0, "{}", r.text());
    // a chunked body in uneven chunks, written apart
    let chunked_head = format!(
        "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        ep.authority, ep.token, LEGACY[1]
    );
    let (a, b) = body.split_at(7);
    let c1 = format!("{:x}\r\n{a}\r\n", a.len());
    let c2 = format!("{:X};ext=1\r\n{b}\r\n", b.len());
    let r = send_parts(
        ep.addr,
        &[
            chunked_head.as_bytes(),
            c1.as_bytes(),
            c2.as_bytes(),
            b"0\r\n\r\n",
        ],
    );
    assert!(!r.tool().0, "{} {}", r.head, r.text());
    // bytes past the body are never a second request: one response, then the close
    let second = format!("GET /index.html HTTP/1.1\r\nHost: {}\r\n\r\n", ep.authority);
    let mut pipelined = ep.head(&rev, body.len()).into_bytes();
    pipelined.extend_from_slice(body.as_bytes());
    pipelined.extend_from_slice(second.as_bytes());
    let r = send(ep.addr, &pipelined);
    assert!(!r.tool().0, "{}", r.text());
    let responses = r.raw.windows(9).filter(|w| *w == b"HTTP/1.1 ").count();
    assert_eq!(responses, 1, "{}", String::from_utf8_lossy(&r.raw));
    assert!(
        r.head.to_ascii_lowercase().contains("connection: close"),
        "{}",
        r.head
    );
    // a chunked body followed by a smuggled request in the same write
    let mut smuggled = chunked_head.clone().into_bytes();
    smuggled.extend_from_slice(format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).as_bytes());
    smuggled.extend_from_slice(second.as_bytes());
    let r = send(ep.addr, &smuggled);
    assert!(!r.tool().0, "{}", r.text());
    assert_eq!(r.raw.windows(9).filter(|w| *w == b"HTTP/1.1 ").count(), 1);
    until("every request ended", || activity(&h) == (0, 0));
}

#[test]
fn mht6_ambiguous_headers_and_oversized_bodies_are_bounded() {
    let (h, ep) = on();
    let body = overview(1, "agent:b", "b1").to_string();
    let base = format!(
        "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\n",
        ep.authority, ep.token, LEGACY[1]
    );
    let bad = refusal(Refusal::Malformed);
    let n = body.len();
    for framing in [
        format!("Content-Length: {n}\r\nTransfer-Encoding: chunked\r\n"),
        format!("Transfer-Encoding: chunked\r\nContent-Length: {n}\r\n"),
        format!("Content-Length: {n}\r\nContent-Length: {n}\r\n"),
        format!("Content-Length: {n}, {n}\r\n"),
        format!("Content-Length: +{n}\r\n"),
        "Content-Length: x\r\n".to_string(),
        "Transfer-Encoding: gzip, chunked\r\n".to_string(),
        "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n".to_string(),
        "Transfer-Encoding: identity\r\n".to_string(),
        format!("Content-Length: {n}\r\nX-Bad: a\u{1}b\r\n"),
        format!("Content-Length: {n}\r\nX-Bad: a\u{7f}b\r\n"),
    ] {
        let r = raw(&ep, &format!("{base}{framing}\r\n"), body.as_bytes());
        assert_eq!(
            (r.status, &r.body),
            (400, &bad.body),
            "{framing:?}: {}",
            r.head
        );
    }
    // a Content-Length over the bound is refused before any body byte is read
    let r = raw(
        &ep,
        &format!("{base}Content-Length: {}\r\n\r\n", BOUND + 1),
        b"",
    );
    assert_text(&r, 413, OVER);
    let r = raw(
        &ep,
        &format!("{base}Content-Length: 99999999999999999999999\r\n\r\n"),
        b"",
    );
    assert_text(&r, 413, OVER);
    // at the bound the body reaches rmcp (all spaces: not a JSON message); one byte over is 413
    let at = vec![b' '; BOUND];
    let r = raw(&ep, &format!("{base}Content-Length: {BOUND}\r\n\r\n"), &at);
    assert!(r.status != 413 && r.status != 0, "{} {}", r.head, r.text());
    // chunked: at the bound, then one byte over
    let chunked = |len: usize| {
        let mut b = format!("{base}Transfer-Encoding: chunked\r\n\r\n").into_bytes();
        let mut left = len;
        while left > 0 {
            let c = left.min(1 << 20);
            b.extend_from_slice(format!("{c:x}\r\n").as_bytes());
            b.extend(std::iter::repeat_n(b' ', c));
            b.extend_from_slice(b"\r\n");
            left -= c;
        }
        b.extend_from_slice(b"0\r\n\r\n");
        send(ep.addr, &b)
    };
    let r = chunked(BOUND);
    assert!(r.status != 413 && r.status != 0, "{} {}", r.head, r.text());
    let r = chunked(BOUND + 1);
    assert_text(&r, 413, OVER);
    // the head bounds: 64 header lines pass, 65 do not
    let fill = |k: usize| (0..k).map(|i| format!("X-F{i}: 1\r\n")).collect::<String>();
    let used = 6; // Host, Authorization, Accept, Content-Type, MCP-Protocol-Version, Content-Length
    let r = raw(
        &ep,
        &format!("{base}{}Content-Length: {n}\r\n\r\n", fill(64 - used)),
        body.as_bytes(),
    );
    assert!(!r.tool().0, "{} {}", r.head, r.text());
    let r = raw(
        &ep,
        &format!("{base}{}Content-Length: {n}\r\n\r\n", fill(65 - used)),
        body.as_bytes(),
    );
    assert_eq!((r.status, &r.body), (431, &refusal(Refusal::TooLarge).body));
    until("every request ended", || activity(&h) == (0, 0));
}

#[test]
fn mht6_unknown_tool_names_do_not_grow_cache() {
    let (h, ep) = on();
    let sh = shared(&h);
    // one claim, distinct unknown names, a probe name between them
    let probe = "rdy_probe_unknown";
    let mut names = Vec::new();
    for i in 0..12 {
        names.push(format!("rdy_unknown_{i}"));
        names.push(probe.to_string());
    }
    let mut lookups_of_probe = Vec::new();
    for (i, name) in names.iter().enumerate() {
        // none before the endpoint's first request
        let before = sh.mcp_http_transports().unwrap_or((0, 0));
        let r = modern_call(&ep, i as u64 + 1, name, json!({}), "agent:u", "u1");
        assert_eq!(r.status, 200, "{name}: {} {}", r.head, r.text());
        let reply = r.reply();
        assert!(
            reply.get("error").is_some() || reply["result"]["isError"] == json!(true),
            "{name}: {reply}"
        );
        // the request's transport service ended with its response
        let (_, made) = sh.mcp_http_transports().expect("the endpoint has served");
        until("the request's transport service is gone", || {
            sh.mcp_http_transports().unwrap().0 == 0
        });
        let looked = made - before.1;
        assert!(looked >= 1, "{name}: each request asks its own service");
        if name == probe {
            lookups_of_probe.push(looked);
        }
    }
    // a repeated name is looked up as often as the first time: no cache outlives its request
    assert!(
        lookups_of_probe.iter().all(|n| *n == lookups_of_probe[0]),
        "{lookups_of_probe:?}"
    );
    // the claim's saved state stays while no transport service does
    let r = modern_call(&ep, 99, "rdy_overview", json!({}), "agent:u", "u1");
    assert_eq!(r.tool().1["session"]["actor"], "agent:u");
    assert_eq!(sh.mcp_http_transports().unwrap().0, 0);
}

#[test]
fn mht6_pages_and_mcp_share_connection_capacity() {
    let (_h, ep) = on();
    let page = format!("GET /index.html HTTP/1.1\r\nHost: {}\r\n\r\n", ep.authority);
    assert_ne!(send(ep.addr, page.as_bytes()).status, 503);
    // one listen and idle sockets fill the 64 permits every listener shares
    let s = open_listen(&ep, &["rdy://project"], "agent:k", "k1");
    let held: Vec<TcpStream> = (0..MAX_CONNECTIONS - 1)
        .map(|_| TcpStream::connect(ep.addr).unwrap())
        .collect();
    let busy = refusal(Refusal::Busy);
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:k", "k1"));
    assert_eq!((r.status, &r.body), (503, &busy.body), "{}", r.head);
    let r = send(ep.addr, page.as_bytes());
    assert_eq!((r.status, &r.body), (503, &busy.body));
    // the listen's close gives its permit back: a call gets in again
    drop(s);
    until("a call is served once the listen's permit is back", || {
        let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(2, "agent:k", "k1"));
        r.status == 200 && !r.tool().0
    });
    // and every permit comes back after the overload
    drop(held);
    until("pages are served again", || {
        send(ep.addr, page.as_bytes()).status != 503
    });
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(3, "agent:k", "k1"));
    assert!(!r.tool().0);
}

#[test]
fn mht6_disable_and_stop_cancel_open_streams() {
    let (mut h, ep) = on();
    let sh = shared(&h);
    let mut s = open_listen(&ep, &["rdy://project"], "agent:d", "d1");
    until("the listen counts", || activity(&h).1 == 1);
    // disabled: the next keepalive check ends the stream, and new work finds no endpoint
    local(h.dir.path(), OFF);
    sh.recheck_mcp_http();
    assert_eq!(s.next(T), None);
    assert!(
        s.ended(),
        "the stream ended at the check, not at its deadline"
    );
    until("the stream gave its place back", || activity(&h) == (0, 0));
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:d", "d1"));
    assert_eq!(
        (r.status, &r.body),
        (405, &refusal(Refusal::Method).body),
        "{}",
        r.head
    );
    // a check while it is on keeps a stream open
    local(h.dir.path(), ON);
    let mut s = open_listen(&ep, &["rdy://project"], "agent:d", "d1");
    sh.recheck_mcp_http();
    assert_eq!(s.next(Duration::from_millis(200)), None);
    assert!(!s.ended(), "an enabled check keeps the stream");
    // a stop ends it too
    assert_eq!(h.stop(), ExitReason::Stopped);
    assert_eq!(s.next(T), None);
    assert!(s.ended(), "the stop ended the stream");
}

/// KNOWN-ISSUES 1216: on Windows, a start can miss the store deletion this check reads.
#[test]
fn mht6_project_replacement_refuses_new_http_work() {
    let (mut h, ep) = on();
    let ok = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:x", "x1"));
    assert!(!ok.tool().0);
    rdy_qa::daemon::replace_rdy_dir(h.dir.path());
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(2, "agent:x", "x1"));
    assert_eq!(r.status, 0, "no reply for a replaced project: {}", r.head);
    let run = h.run.take().unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::ProjectGone);
}

#[test]
fn mht6_os_assigned_port_serves_mcp() {
    // a project whose 16 derived candidates this test holds: the daemon's site takes an
    // OS-assigned port (D-5GFFX), and MCP is served there
    let (dir, _held) = loop {
        let dir = project(ON);
        let root = dir.path().canonicalize().unwrap();
        let held: Result<Vec<_>, _> = addr::candidates(None, &root)
            .iter()
            .map(|p| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, *p)))
            .collect();
        if let Ok(held) = held {
            break (dir, held);
        }
    };
    let root = dir.path().canonicalize().unwrap();
    let candidates = addr::candidates(None, &root);
    let h = Hosted::start_in(dir);
    let ep = endpoint(&h);
    assert!(
        !candidates.contains(&ep.port()),
        "{} is OS-assigned",
        ep.port()
    );
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:o", "o1"));
    let (err, v) = r.tool();
    assert!(!err, "{v}");
    assert_eq!(v["session"]["actor"], "agent:o");
    let r = modern_call(&ep, 2, "rdy_overview", json!({}), "agent:o", "o1");
    assert!(!r.tool().0);
}

#[test]
fn mht6_disabled_endpoint_is_the_missing_route() {
    let h = Hosted::start_in(project(OFF));
    let ep = endpoint(&h);
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:z", "z1"));
    assert_eq!(
        (r.status, &r.body),
        (405, &refusal(Refusal::Method).body),
        "{}",
        r.head
    );
    let get = send(
        ep.addr,
        format!("GET /~mcp HTTP/1.1\r\nHost: {}\r\n\r\n", ep.authority).as_bytes(),
    );
    assert_eq!(get.status, 404, "{}", get.head);
    assert!(get.header("www-authenticate").is_none());
}
