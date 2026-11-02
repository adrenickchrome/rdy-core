//! ui8 (D-6M0AF, D-94BT2, D-FV6G9, D-N5NSN; TASK-982 O7, C5): the HTTP support of the local
//! approval pages. UI routes exist only on a full listener bound to loopback while `serve.ui` is
//! on; anywhere else the ordinary guard answers the path. An eligible route checks one loopback
//! Host, its method, its body framing and limit, the `POST` Origin and the form's type, in that
//! order. The answer form is read from the bytes already read with the head and the rest of the
//! body, up to 4 MiB, and parsed strictly. A UI response sends one CSP whose only change is
//! `form-action 'self'`. The tests drive the classifier, the readers and the parser directly; the
//! handler's pins are in `daemon_approver_ui` and `daemon_ui_events`.
use rdy_daemon::Shared;
use rdy_daemon::site::ensure_profile_site;
use rdy_daemon::site::guard::{Guard, Refusal, refusal};
use rdy_daemon::site::http::{
    CSP, Method, Response, UI_CSP, parse_head, reason, to_bytes, ui_response,
};
use rdy_daemon::site::server::{Head, read_head, read_ui_body};
use rdy_daemon::site::ui_request::{
    Form, MAX_UI_BODY, Route as UiRoute, UI_GET, UI_POST, classify, form_refusal, parse_form,
};
use rdy_engine::E_BAD_INPUT;
use rdy_engine::answer::AnswerRequest;
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{agent, approver, land, write, write_as};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use serde_json::json;
use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

const P: u16 = 51000;
const REV: &str = "00112233aabbccdd";
const HASH: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn v4() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, P))
}

fn v6() -> SocketAddr {
    SocketAddr::from((Ipv6Addr::LOCALHOST, P))
}

fn lan() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::new(192, 168, 1, 20), P))
}

/// A request head from lines joined by `\n`.
fn req(lines: &str) -> rdy_daemon::site::http::Request {
    let mut head = lines.replace('\n', "\r\n");
    head.push_str("\r\n\r\n");
    parse_head(head.as_bytes()).unwrap()
}

fn get(target: &str, host: &str) -> rdy_daemon::site::http::Request {
    req(&format!("GET {target} HTTP/1.1\nHost: {host}"))
}

/// A well-formed answer `POST` to `host`, with `extra` header lines.
fn post_lines(host: &str, extra: &str) -> String {
    format!(
        "POST /~ui/answer HTTP/1.1\nHost: {host}\nOrigin: http://{host}\nContent-Type: application/x-www-form-urlencoded\nContent-Length: 10{extra}"
    )
}

fn post(host: &str) -> rdy_daemon::site::http::Request {
    req(&post_lines(host, ""))
}

/// `classify` on the full loopback listener at `local`, `serve.ui` on.
fn on(local: SocketAddr, r: &rdy_daemon::site::http::Request) -> Result<Option<UiRoute>, Refusal> {
    classify(r, local, true, true)
}

/// An answer request whose headers are the given lines, with no Host/Origin defaults.
fn answer_with(lines: &[&str]) -> rdy_daemon::site::http::Request {
    let mut head = String::from("POST /~ui/answer HTTP/1.1");
    for l in lines {
        head.push('\n');
        head.push_str(l);
    }
    req(&head)
}

// --- the world ------------------------------------------------------------------------------------

/// A project named `Tiny` whose canon holds doc `intro` and an org profile `guide` that selects
/// it, served by a daemon.
fn world() -> Hosted {
    let (dir, mut r) = tiny();
    let ids = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$intro", "body": "INTRO"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap()
    .created;
    land(&mut r, "w1").unwrap();
    write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "parent": ids["$admin"].to_string(),
            "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                "audience": "org", "select": [ids["$intro"].to_string()]}}]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    drop(r);
    Hosted::start_with(dir, None)
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap()
}

fn send(port: u16, request: &str) -> http::Response {
    http::raw(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), request)
}

/// A themed 404 by status, content type and heading (the message markup may change); it
/// names no UI.
#[track_caller]
fn assert_themed_404(r: &http::Response, what: &str) {
    assert_eq!(r.status, 404, "{what}: {}", r.head);
    assert_eq!(
        r.header("content-type"),
        Some("text/html; charset=utf-8"),
        "{what}: {}",
        r.head
    );
    let body = String::from_utf8_lossy(&r.body);
    assert!(body.contains("<h1>Page not found</h1>"), "{what}: {body}");
    for needle in ["~ui", "approval", "answer"] {
        assert!(!body.contains(needle), "{what}: names {needle:?}");
    }
}

#[track_caller]
fn assert_plain(r: &http::Response, f: Refusal, what: &str) {
    let want = refusal(f);
    assert_eq!(r.status, want.status, "{what}: {}", r.head);
    assert_eq!(r.body, want.body, "{what}");
    assert_eq!(
        r.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "{what}"
    );
}

/// What the ordinary listener at `port` answers to the UI paths when no UI route applies: the
/// themed 404 for `GET` and `HEAD`, the existing 405 for `POST`, and `Bad request.` for a UI
/// event request.
fn ordinary_answers(port: u16, what: &str) {
    let host = format!("127.0.0.1:{port}");
    for path in ["/~ui/", "/~ui/item/P-abc@1", "/~ui/answer", "/~ui"] {
        let r = send(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert_themed_404(&r, &format!("{what}: GET {path}"));
        let head = send(
            port,
            &format!("HEAD {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert_eq!(head.status, 404, "{what}: HEAD {path}");
        assert!(head.body.is_empty(), "{what}: HEAD {path} sends no body");
        assert_eq!(
            head.header("content-length"),
            r.header("content-length"),
            "{what}: HEAD {path} reports the GET length"
        );
    }
    let post = send(
        port,
        &format!(
            "POST /~ui/answer HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 5\r\n\r\nref=r"
        ),
    );
    assert_plain(&post, Refusal::Method, &format!("{what}: POST"));
    assert_eq!(post.header("allow"), Some("GET, HEAD"), "{what}");
    let events = send(
        port,
        &format!("GET /_rdy/events?ui=1&rev={REV} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_plain(&events, Refusal::Malformed, &format!("{what}: ui events"));
}

#[test]
fn ui8_only_full_loopback_listeners_classify_ui_routes() {
    // every loopback spelling of the listener's port, on each loopback socket
    for (local, host) in [
        (v4(), "127.0.0.1:51000"),
        (v4(), "localhost:51000"),
        (v4(), "LOCALHOST:51000"),
        (v6(), "[::1]:51000"),
        (v6(), "localhost:51000"),
    ] {
        assert_eq!(
            on(local, &get("/~ui/", host)),
            Ok(Some(UiRoute::Pending)),
            "{host}"
        );
        assert_eq!(
            on(local, &get("/~ui/item/P-abc@1", host)),
            Ok(Some(UiRoute::Item("P-abc@1".into()))),
            "{host}"
        );
        assert_eq!(
            on(local, &get("/~ui/item/advice%3AP-abc%401", host)),
            Ok(Some(UiRoute::Item("advice:P-abc@1".into()))),
            "{host}: one segment, decoded once"
        );
        assert_eq!(
            on(local, &req(&format!("HEAD /~ui/ HTTP/1.1\nHost: {host}"))),
            Ok(Some(UiRoute::Pending)),
            "{host}"
        );
        assert_eq!(on(local, &post(host)), Ok(Some(UiRoute::Answer)), "{host}");
        assert_eq!(
            on(local, &get(&format!("/_rdy/events?ui=1&rev={REV}"), host)),
            Ok(Some(UiRoute::Events { rev: REV.into() })),
            "{host}"
        );
        assert_eq!(
            on(local, &get(&format!("/_rdy/events?rev={REV}&ui=1"), host)),
            Ok(Some(UiRoute::Events { rev: REV.into() })),
            "{host}"
        );
    }
    // a LAN socket, a profile-only listener or serve.ui off: no UI route, whatever the Host
    let paths = [
        get("/~ui/", "127.0.0.1:51000"),
        get("/~ui/item/P-abc@1", "127.0.0.1:51000"),
        post("127.0.0.1:51000"),
        get(&format!("/_rdy/events?ui=1&rev={REV}"), "127.0.0.1:51000"),
        get("/~ui/", "192.168.1.20:51000"),
        get(
            &format!("/_rdy/events?ui=1&rev={REV}&scope=canon"),
            "127.0.0.1:51000",
        ),
    ];
    for r in &paths {
        assert_eq!(
            classify(r, lan(), true, true),
            Ok(None),
            "LAN: {}",
            r.target
        );
        assert_eq!(
            classify(r, v4(), false, true),
            Ok(None),
            "profile-only: {}",
            r.target
        );
        assert_eq!(
            classify(r, v4(), true, false),
            Ok(None),
            "ui off: {}",
            r.target
        );
        assert_eq!(classify(r, v6(), false, false), Ok(None), "{}", r.target);
    }
    // `/~ui` and every malformed or extra UI segment is an ordinary missing path
    for t in [
        "/~ui",
        "/~ui/index.html",
        "/~ui/x",
        "/~ui//",
        "/~ui/item",
        "/~ui/item/",
        "/~ui/item/a/b",
        "/~ui/item/a/",
        "/~ui/item/%2e%2e",
        "/~ui/item/..",
        "/~ui/item/a%2Fb",
        "/~ui/item/a%5Cb",
        "/~ui/item/a%00b",
        "/~ui/item/%FF",
        "/~ui/item/a%2",
        "/~ui/answer/",
        "/~ui/answer/x",
        "/x/~ui/",
        "/~UI/",
    ] {
        assert_eq!(on(v4(), &get(t, "127.0.0.1:51000")), Ok(None), "{t}");
    }
    // the guard's one percent decode: an encoded `~` is the same path
    assert_eq!(
        on(v4(), &get("/%7Eui/", "127.0.0.1:51000")),
        Ok(Some(UiRoute::Pending))
    );
    // the event route: an event request that names ui has exactly ui=1 and rev, once each
    let ev = |q: &str| on(v4(), &get(&format!("/_rdy/events?{q}"), "127.0.0.1:51000"));
    assert_eq!(
        ev(&format!(
            "scope=canon&page=index.html&rev={REV}&label={REV}"
        )),
        Ok(None)
    );
    assert_eq!(ev(&format!("uix=1&rev={REV}")), Ok(None));
    for q in [
        format!("ui=1&rev={REV}&scope=canon"),
        format!("ui=1&rev={REV}&profile=guide"),
        format!("ui=1&rev={REV}&menu=1"),
        format!("ui=1&rev={REV}&page=index.html"),
        format!("ui=1&rev={REV}&label={REV}"),
        format!("ui=1&rev={REV}&rev={REV}"),
        format!("ui=1&ui=1&rev={REV}"),
        format!("ui=2&rev={REV}"),
        format!("ui=&rev={REV}"),
        format!("ui&rev={REV}"),
        "ui=1".to_string(),
        "ui=1&rev=".to_string(),
        "ui=1&rev=00112233AABBCCDD".to_string(),
        "ui=1&rev=00112233aabbccd".to_string(),
        "ui=1&rev=00112233aabbccdde".to_string(),
        format!("ui=1&rev={REV}&"),
        format!("ui=1&&rev={REV}"),
    ] {
        assert_eq!(ev(&q), Err(Refusal::Malformed), "{q}");
    }
    // an eligible UI route takes exactly one loopback Host of this port
    for host in [
        "192.168.1.20:51000",
        "127.0.0.1:51001",
        "127.0.0.1",
        "rebound.invalid:51000",
        "0.0.0.0:51000",
    ] {
        assert_eq!(on(v4(), &get("/~ui/", host)), Err(Refusal::Host), "{host}");
        assert_eq!(on(v4(), &post(host)), Err(Refusal::Host), "{host}");
    }
    let two = req("GET /~ui/ HTTP/1.1\nHost: 127.0.0.1:51000\nHost: 127.0.0.1:51000");
    assert_eq!(on(v4(), &two), Err(Refusal::Host));
    assert_eq!(on(v4(), &req("GET /~ui/ HTTP/1.1")), Err(Refusal::Host));

    // the real daemon: with serve.ui off the full listener, and a profile-only listener always,
    // answer the UI paths as ordinary paths
    let h = world();
    let sh = shared(&h);
    std::fs::write(
        h.dir.path().join(".rdy/local.yaml"),
        "serve:\n  ui: false\n",
    )
    .unwrap();
    let full = port_of(&sh.site_info.get().unwrap().url.clone().unwrap());
    ordinary_answers(full, "full listener, serve.ui off");
    let only = port_of(
        &ensure_profile_site(Arc::clone(&sh), "guide")
            .unwrap()
            .url
            .unwrap(),
    );
    ordinary_answers(only, "profile-only listener");
}

#[test]
fn ui8_post_origin_and_framing_are_closed() {
    let h4 = "127.0.0.1:51000";
    assert_eq!(on(v4(), &post(h4)), Ok(Some(UiRoute::Answer)));
    assert_eq!(on(v6(), &post("[::1]:51000")), Ok(Some(UiRoute::Answer)));
    assert_eq!(
        on(v4(), &post("localhost:51000")),
        Ok(Some(UiRoute::Answer))
    );

    // the Origin: exactly one, the accepted Host's own http origin
    let base = [
        "Host: 127.0.0.1:51000",
        "Content-Type: application/x-www-form-urlencoded",
        "Content-Length: 10",
    ];
    let with_origins = |origins: &[&str]| {
        let mut lines = base.to_vec();
        lines.extend_from_slice(origins);
        on(v4(), &answer_with(&lines))
    };
    assert_eq!(
        with_origins(&["Origin: http://127.0.0.1:51000"]),
        Ok(Some(UiRoute::Answer))
    );
    for origins in [
        &[][..],
        &["Origin: null"],
        &[
            "Origin: http://127.0.0.1:51000",
            "Origin: http://127.0.0.1:51000",
        ],
        &[
            "Origin: http://127.0.0.1:51000",
            "Origin: http://rebound.invalid",
        ],
        &["Origin: http://localhost:51000"],
        &["Origin: http://[::1]:51000"],
        &["Origin: http://127.0.0.1:51001"],
        &["Origin: http://127.0.0.1"],
        &["Origin: https://127.0.0.1:51000"],
        &["Origin: http://127.0.0.1:51000/"],
        &["Origin: http://rebound.invalid"],
        &["Origin: "],
    ] {
        assert_eq!(with_origins(origins), Err(Refusal::Origin), "{origins:?}");
    }
    // the same spelling on each side: localhost is not 127.0.0.1 or [::1]
    let lh = |origin: &str| {
        on(
            v6(),
            &answer_with(&[
                "Host: [::1]:51000",
                origin,
                "Content-Type: application/x-www-form-urlencoded",
                "Content-Length: 1",
            ]),
        )
    };
    assert_eq!(lh("Origin: http://[::1]:51000"), Ok(Some(UiRoute::Answer)));
    assert_eq!(lh("Origin: http://localhost:51000"), Err(Refusal::Origin));
    assert_eq!(lh("Origin: http://127.0.0.1:51000"), Err(Refusal::Origin));

    // the body's framing: one decimal Content-Length up to 4 MiB, no Transfer-Encoding
    let framed = |extra: &[&str]| {
        let mut lines = vec![
            "Host: 127.0.0.1:51000",
            "Origin: http://127.0.0.1:51000",
            "Content-Type: application/x-www-form-urlencoded",
        ];
        lines.extend_from_slice(extra);
        on(v4(), &answer_with(&lines))
    };
    assert_eq!(framed(&["Content-Length: 0"]), Ok(Some(UiRoute::Answer)));
    assert_eq!(
        framed(&["Content-Length: 4194304"]),
        Ok(Some(UiRoute::Answer))
    );
    assert_eq!(
        framed(&["Content-Length: 4194305"]),
        Err(Refusal::UiBodyTooLarge)
    );
    assert_eq!(
        framed(&["Content-Length: 18446744073709551616"]),
        Err(Refusal::UiBodyTooLarge)
    );
    for extra in [
        &[][..],
        &["Content-Length: "],
        &["Content-Length: x"],
        &["Content-Length: -1"],
        &["Content-Length: +10"],
        &["Content-Length: 10", "Content-Length: 10"],
        &["Content-Length: 10", "Content-Length: 11"],
        &["Content-Length: 10", "Transfer-Encoding: chunked"],
        &["Transfer-Encoding: chunked"],
    ] {
        assert_eq!(framed(extra), Err(Refusal::Malformed), "{extra:?}");
    }

    // the media type
    let typed = |types: &[&str]| {
        let mut lines = vec![
            "Host: 127.0.0.1:51000",
            "Origin: http://127.0.0.1:51000",
            "Content-Length: 3",
        ];
        lines.extend_from_slice(types);
        on(v4(), &answer_with(&lines))
    };
    for ok in [
        "Content-Type: application/x-www-form-urlencoded",
        "Content-Type: application/x-www-form-urlencoded; charset=UTF-8",
        "Content-Type: APPLICATION/X-WWW-FORM-URLENCODED; CHARSET=utf-8",
    ] {
        assert_eq!(typed(&[ok]), Ok(Some(UiRoute::Answer)), "{ok}");
    }
    for types in [
        &[][..],
        &["Content-Type: text/plain"],
        &["Content-Type: application/json"],
        &["Content-Type: multipart/form-data; boundary=x"],
        &["Content-Type: application/x-www-form-urlencoded; charset=latin1"],
        &["Content-Type: application/x-www-form-urlencoded;charset=UTF-8"],
        &["Content-Type: application/x-www-form-urlencoded; charset=UTF-8; x=1"],
        &[
            "Content-Type: application/x-www-form-urlencoded",
            "Content-Type: application/x-www-form-urlencoded",
        ],
    ] {
        assert_eq!(typed(types), Err(Refusal::UiContentType), "{types:?}");
    }

    // the method of each route, with its Allow
    for m in ["GET", "HEAD", "PUT", "DELETE", "OPTIONS"] {
        let r = req(&format!("{m} /~ui/answer HTTP/1.1\nHost: {h4}"));
        assert_eq!(
            on(v4(), &r),
            Err(Refusal::UiMethod { allow: UI_POST }),
            "{m} /~ui/answer"
        );
    }
    for path in [
        "/~ui/",
        "/~ui/item/P-abc@1",
        &format!("/_rdy/events?ui=1&rev={REV}"),
    ] {
        for m in ["POST", "PUT", "DELETE", "OPTIONS"] {
            let r = req(&format!(
                "{m} {path} HTTP/1.1\nHost: {h4}\nOrigin: http://{h4}\nContent-Length: 0"
            ));
            assert_eq!(
                on(v4(), &r),
                Err(Refusal::UiMethod { allow: UI_GET }),
                "{m} {path}"
            );
        }
    }
    let get_refusal = refusal(Refusal::UiMethod { allow: UI_GET });
    assert_eq!(get_refusal.status, 405);
    assert_eq!(get_refusal.body, b"This page answers GET and HEAD only.");
    let post_refusal = refusal(Refusal::UiMethod { allow: UI_POST });
    assert_eq!(post_refusal.status, 405);
    assert_eq!(post_refusal.body, b"This address answers POST only.");
    for (r, allow) in [(&get_refusal, "GET, HEAD"), (&post_refusal, "POST")] {
        let allows: Vec<&String> = r
            .headers
            .iter()
            .filter(|(n, _)| *n == "Allow")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(allows, vec![allow]);
        assert!(
            r.headers
                .contains(&("Content-Type", "text/plain; charset=utf-8".into()))
        );
    }
    let big = refusal(Refusal::UiBodyTooLarge);
    assert_eq!(big.status, 413);
    assert_eq!(big.body, b"The answer is too large. Send at most 4 MiB.");
    let ty = refusal(Refusal::UiContentType);
    assert_eq!(ty.status, 415);
    assert_eq!(ty.body, b"Use a form to answer this card.");
    for (status, phrase) in [
        (303, "See Other"),
        (409, "Conflict"),
        (413, "Content Too Large"),
        (415, "Unsupported Media Type"),
        (422, "Unprocessable Content"),
    ] {
        assert_eq!(reason(status), phrase);
    }

    // the order: Host, method, framing, Origin, type
    let order = |lines: &[&str]| on(v4(), &answer_with(lines));
    assert_eq!(
        order(&["Host: rebound.invalid:51000", "Content-Length: 99999999"]),
        Err(Refusal::Host)
    );
    assert_eq!(
        on(
            v4(),
            &req("GET /~ui/answer HTTP/1.1\nHost: 127.0.0.1:51000\nContent-Length: 99999999")
        ),
        Err(Refusal::UiMethod { allow: UI_POST })
    );
    assert_eq!(
        order(&[
            "Host: 127.0.0.1:51000",
            "Origin: null",
            "Content-Length: 99999999"
        ]),
        Err(Refusal::UiBodyTooLarge)
    );
    assert_eq!(
        order(&["Host: 127.0.0.1:51000", "Origin: null", "Content-Length: 1"]),
        Err(Refusal::Origin)
    );
    // a UI page route keeps the no-body rule and the existing Origin rule
    let page = |extra: &str| {
        on(
            v4(),
            &req(&format!("GET /~ui/ HTTP/1.1\nHost: {h4}{extra}")),
        )
    };
    assert_eq!(page("\nContent-Length: 3"), Err(Refusal::Body));
    assert_eq!(page("\nTransfer-Encoding: chunked"), Err(Refusal::Body));
    assert_eq!(page("\nContent-Length: 0"), Ok(Some(UiRoute::Pending)));
    assert_eq!(
        page("\nOrigin: http://rebound.invalid"),
        Err(Refusal::Origin)
    );
    assert_eq!(
        page(&format!("\nOrigin: http://{h4}")),
        Ok(Some(UiRoute::Pending))
    );

    // the ordinary routes are unchanged: the classifier leaves them, and the guard refuses as
    // before (a UI path is no route of its own)
    let g = Guard::full(P, None);
    let ordinary_post = req(&format!("POST /index.html HTTP/1.1\nHost: {h4}"));
    assert_eq!(on(v4(), &ordinary_post), Ok(None));
    assert_eq!(g.check(&ordinary_post), Err(Refusal::Method));
    assert_eq!(
        g.check(&req(&format!("PUT /~ui/ HTTP/1.1\nHost: {h4}"))),
        Err(Refusal::Method)
    );
    assert_eq!(g.check(&get("/~ui/", h4)), Err(Refusal::NotFound));
    assert_eq!(
        g.check(&req(&format!(
            "GET /index.html HTTP/1.1\nHost: {h4}\nContent-Length: 3"
        ))),
        Err(Refusal::Body)
    );
    let ordinary = refusal(Refusal::Method);
    assert_eq!(
        ordinary.body,
        b"This site is read-only: it answers GET and HEAD only."
    );
    assert!(ordinary.headers.contains(&("Allow", "GET, HEAD".into())));
    assert!(matches!(post(h4).method, Method::Other(ref m) if m == "POST"));
}

// --- byte-stream adapters --------------------------------------------------------------------------

/// A reader that yields its chunks one read at a time, then either the end of the stream or a
/// read that never becomes ready. It counts every byte it hands out.
struct Chunks {
    chunks: VecDeque<Vec<u8>>,
    eof: bool,
    read: Arc<AtomicUsize>,
}

impl Chunks {
    fn new(chunks: Vec<Vec<u8>>, eof: bool) -> Chunks {
        Chunks {
            chunks: chunks.into_iter().filter(|c| !c.is_empty()).collect(),
            eof,
            read: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl AsyncRead for Chunks {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(mut chunk) = self.chunks.pop_front() else {
            // the end of the stream, or a peer that sends nothing more
            return if self.eof {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            };
        };
        let n = chunk.len().min(buf.remaining());
        buf.put_slice(&chunk[..n]);
        self.read.fetch_add(n, Ordering::SeqCst);
        if n < chunk.len() {
            chunk.drain(..n);
            self.chunks.push_front(chunk);
        }
        Poll::Ready(Ok(()))
    }
}

fn far() -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(3600)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// The whole answer path a connection takes before its handler: the head and the bytes read
/// with it, the head's parse, the classifier, the body read and the form's parse. `handled`
/// counts the forms that reach the handler; a refusal never does.
async fn answer(reader: &mut Chunks, handled: &AtomicUsize) -> Result<Form, Response> {
    let (head, rest) = match read_head(reader).await {
        Head::Bytes { head, rest } => (head, rest),
        Head::TooLarge => return Err(refusal(Refusal::TooLarge)),
        Head::Closed => panic!("the reader closed with nothing sent"),
    };
    let request = parse_head(&head).map_err(|_| refusal(Refusal::Malformed))?;
    match classify(&request, v4(), true, true).map_err(refusal)? {
        Some(UiRoute::Answer) => {}
        other => panic!("not the answer route: {other:?}"),
    }
    let body = read_ui_body(reader, &request, rest, far())
        .await
        .expect("within the budget")
        .map_err(refusal)?;
    let form = parse_form(&body).map_err(|e| form_refusal(&e))?;
    handled.fetch_add(1, Ordering::SeqCst);
    Ok(form)
}

fn request_bytes(body: &[u8], length: usize) -> Vec<u8> {
    let mut bytes = format!(
        "POST /~ui/answer HTTP/1.1\r\nHost: 127.0.0.1:51000\r\nOrigin: http://127.0.0.1:51000\r\nContent-Type: application/x-www-form-urlencoded; charset=UTF-8\r\nContent-Length: {length}\r\n\r\n"
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// The form of an answer that passed, or a panic that shows the refusal.
#[track_caller]
fn form_of(r: Result<Form, Response>) -> Form {
    r.unwrap_or_else(|r| panic!("refused: {} {}", r.status, String::from_utf8_lossy(&r.body)))
}

#[track_caller]
fn assert_text(r: &Response, status: u16, body: &str) {
    assert_eq!(r.status, status);
    assert_eq!(String::from_utf8_lossy(&r.body), body);
}

#[test]
fn ui8_prefetched_and_split_bodies_parse_identically() {
    let body = format!(
        "ref=P-abc%401&card_hash={HASH}&option=approve&note=caf%C3%A9+%E2%9C%93+ok&brief=brief%3Ax1"
    );
    let want = Form {
        reference: "P-abc@1".into(),
        card_hash: HASH.into(),
        option: "approve".into(),
        note: Some("café ✓ ok".into()),
        brief: Some("brief:x1".into()),
    };
    let bytes = request_bytes(body.as_bytes(), body.len());
    let handled = AtomicUsize::new(0);
    let rt = rt();
    let run = |chunks: Vec<Vec<u8>>, eof: bool| {
        rt.block_on(answer(&mut Chunks::new(chunks, eof), &handled))
    };

    // one packet, then every split between head and body bytes, then one byte a read
    assert_eq!(form_of(run(vec![bytes.clone()], true)), want.clone());
    for k in 0..=bytes.len() {
        let chunks = vec![bytes[..k].to_vec(), bytes[k..].to_vec()];
        assert_eq!(
            form_of(run(chunks.clone(), true)),
            want.clone(),
            "split at {k}"
        );
        assert_eq!(
            form_of(run(chunks, false)),
            want.clone(),
            "split at {k}, open"
        );
    }
    let bytewise: Vec<Vec<u8>> = bytes.iter().map(|b| vec![*b]).collect();
    assert_eq!(form_of(run(bytewise, false)), want.clone());
    // extra pipelined bytes, in the same packet or later: never read as a request
    let mut pipelined = bytes.clone();
    pipelined.extend_from_slice(b"GET /secret HTTP/1.1\r\nHost: 127.0.0.1:51000\r\n\r\n");
    assert_eq!(form_of(run(vec![pipelined.clone()], true)), want.clone());
    assert_eq!(
        form_of(run(vec![bytes.clone(), b"ref=other&x=1".to_vec()], true)),
        want.clone()
    );
    let handled_ok = handled.load(Ordering::SeqCst);

    // a premature end of the stream: 400, no handler
    for cut in [1, body.len() / 2, body.len()] {
        let r = run(vec![bytes[..bytes.len() - cut].to_vec()], true).unwrap_err();
        assert_text(&r, 400, "Bad request.");
    }
    // the form's own refusals, from every split too: no handler runs
    let keys = "the parameters here are brief, card_hash, note, option, ref";
    for (body, text) in [
        (
            "ref=r&ref=r&card_hash=h&option=o".to_string(),
            "E_BAD_INPUT: the answer form is not valid\nfix: reload the card and choose an option"
                .to_string(),
        ),
        (
            "ref=r&card_hash=h&option=o&refs=x".to_string(),
            format!(
                "E_BAD_INPUT: answer form: unknown parameter \"refs\"\nfix: did you mean \"ref\"? {keys}"
            ),
        ),
        (
            "ref=r&card_hash=h&option=o&zzzzzz=x".to_string(),
            format!(
                "E_BAD_INPUT: answer form: unknown parameter \"zzzzzz\"\nfix: remove \"zzzzzz\"; {keys}"
            ),
        ),
        (
            "ref=r&card_hash=h&option=o&note=n&brief=b&note=m".to_string(),
            "E_BAD_INPUT: the answer form is not valid\nfix: reload the card and choose an option"
                .to_string(),
        ),
        (
            "ref=r&card_hash=h&option=%FF".to_string(),
            "E_BAD_INPUT: the answer form is not valid\nfix: reload the card and choose an option"
                .to_string(),
        ),
        (
            "card_hash=h&option=o".to_string(),
            "E_BAD_INPUT: the answer form needs ref\nfix: reload the card and choose an option"
                .to_string(),
        ),
    ] {
        let bytes = request_bytes(body.as_bytes(), body.len());
        for k in [0, bytes.len() - body.len(), bytes.len() - 1, bytes.len()] {
            let r = run(vec![bytes[..k].to_vec(), bytes[k..].to_vec()], true).unwrap_err();
            assert_text(&r, 400, &text);
            assert_eq!(
                r.headers,
                vec![("Content-Type", "text/plain; charset=utf-8".to_string())]
            );
        }
    }
    // a sixth entry is refused, whatever it names
    let six = format!("ref=r&card_hash={HASH}&option=o&note=n&brief=b&x=1");
    let r = run(vec![request_bytes(six.as_bytes(), six.len())], true).unwrap_err();
    assert_text(
        &r,
        400,
        "E_BAD_INPUT: the answer form is not valid\nfix: reload the card and choose an option",
    );
    // empty note and brief are absent
    let empty = "ref=r&card_hash=h&option=o&note=&brief=";
    assert_eq!(
        form_of(run(
            vec![request_bytes(empty.as_bytes(), empty.len())],
            true
        )),
        Form {
            reference: "r".into(),
            card_hash: "h".into(),
            option: "o".into(),
            note: None,
            brief: None,
        }
    );
    assert_eq!(
        handled.load(Ordering::SeqCst),
        handled_ok + 1,
        "no handler runs on a refusal"
    );

    // the read budget: a body that does not arrive by the request's deadline ends the read
    // (the connection closes with no reply); no wall-clock wait is asserted
    let head_only = request_bytes(b"", body.len());
    let mut reader = Chunks::new(vec![head_only], false);
    let ended = rt.block_on(async {
        let Head::Bytes { head, rest } = read_head(&mut reader).await else {
            panic!("a head")
        };
        let request = parse_head(&head).unwrap();
        read_ui_body(&mut reader, &request, rest, tokio::time::Instant::now()).await
    });
    assert!(ended.is_none(), "the budget ended the read");

    // a malformed card hash is not the parser's to refuse: the engine's validator refuses it
    let form = parse_form(b"ref=REF&card_hash=nothex&option=approve").unwrap();
    assert_eq!(form.card_hash, "nothex");
    let (_dir, mut r) = tiny();
    let bot = agent("agent:bot", Some("r1"));
    write_as(
        &mut r,
        &bot,
        "r1",
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    )
    .unwrap();
    let p = r.propose(&bot, "r1", "Adds A", Some("Adds A.")).unwrap();
    let e = r
        .answer(
            &approver(),
            "human:qa",
            AnswerRequest {
                r#ref: p.r#ref.clone(),
                option: form.option,
                card_hash: Some(form.card_hash),
                note: form.note,
                brief: form.brief,
                ..AnswerRequest::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "card_hash nothex is not a hash");
    assert_eq!(e.fix, "pass the card_hash rdy_pending gave with the card");
}

#[test]
fn ui8_four_mib_passes_and_one_byte_more_is_refused() {
    let rt = rt();
    let prefix = format!("ref=P-abc%401&card_hash={HASH}&option=approve&note=");
    let mut body = prefix.clone().into_bytes();
    body.resize(MAX_UI_BODY, b'x');
    assert_eq!(body.len(), 4_194_304);
    let bytes = request_bytes(&body, body.len());
    // the head and the first bytes in one packet, the rest in 64 KiB reads
    let first = bytes.len() - body.len() + 100;
    let mut chunks = vec![bytes[..first].to_vec()];
    chunks.extend(bytes[first..].chunks(65_536).map(<[u8]>::to_vec));
    let mut reader = Chunks::new(chunks, true);
    let counted = Arc::clone(&reader.read);
    let (got, prefetched) = rt.block_on(async {
        let Head::Bytes { head, rest } = read_head(&mut reader).await else {
            panic!("a head")
        };
        let prefetched = rest.len();
        let request = parse_head(&head).unwrap();
        assert_eq!(
            classify(&request, v4(), true, true),
            Ok(Some(UiRoute::Answer))
        );
        let got = read_ui_body(&mut reader, &request, rest, far())
            .await
            .expect("in budget")
            .expect("an exactly-sized body");
        (got, prefetched)
    });
    assert_eq!(prefetched, 100);
    assert_eq!(got.len(), MAX_UI_BODY);
    assert_eq!(
        got.capacity(),
        MAX_UI_BODY,
        "one allocation of the framed length"
    );
    assert_eq!(got, body);
    assert_eq!(
        counted.load(Ordering::SeqCst),
        bytes.len(),
        "every byte read once"
    );
    let form = parse_form(&got).unwrap();
    assert_eq!(form.note.unwrap().len(), MAX_UI_BODY - prefix.len());

    // one byte more: 413 from the head, before any body byte is read or allocated
    let over = request_bytes(b"ref=r", MAX_UI_BODY + 1);
    let request = parse_head(&over[..over.len() - 5]).unwrap();
    assert_eq!(
        classify(&request, v4(), true, true),
        Err(Refusal::UiBodyTooLarge)
    );
    let mut body_reader = Chunks::new(vec![vec![b'x'; 1 << 20]], true);
    let counted = Arc::clone(&body_reader.read);
    let out = rt.block_on(read_ui_body(
        &mut body_reader,
        &request,
        b"ref=r".to_vec(),
        far(),
    ));
    assert_eq!(out, Some(Err(Refusal::UiBodyTooLarge)));
    assert_eq!(counted.load(Ordering::SeqCst), 0, "no body byte is read");
    let r = refusal(Refusal::UiBodyTooLarge);
    let text = String::from_utf8(to_bytes(&r, false, 0)).unwrap();
    assert!(
        text.starts_with("HTTP/1.1 413 Content Too Large\r\n"),
        "{text}"
    );
    assert!(text.ends_with("\r\n\r\nThe answer is too large. Send at most 4 MiB."));
}

#[test]
fn ui8_ui_csp_replaces_only_form_action() {
    let ui_csp = CSP.replace("form-action 'none'", "form-action 'self'");
    assert_eq!(UI_CSP, ui_csp);
    assert_ne!(UI_CSP, CSP);
    let directives = |csp: &str| csp.split("; ").map(str::to_string).collect::<Vec<_>>();
    let (base, ui) = (directives(CSP), directives(UI_CSP));
    assert_eq!(base.len(), ui.len());
    let changed: Vec<(&String, &String)> = base.iter().zip(&ui).filter(|(a, b)| a != b).collect();
    assert_eq!(
        changed,
        vec![(
            &"form-action 'none'".to_string(),
            &"form-action 'self'".to_string()
        )]
    );

    let page = ui_response(200, "text/html; charset=utf-8", b"hi".to_vec());
    let text = String::from_utf8(to_bytes(&page, false, 0)).unwrap();
    assert_eq!(
        text,
        format!(
            "HTTP/1.1 200 OK\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: {UI_CSP}\r\nConnection: close\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 2\r\n\r\nhi"
        )
    );
    assert_eq!(
        text.matches("Content-Security-Policy:").count(),
        1,
        "one CSP header"
    );
    assert!(
        !text.to_ascii_lowercase().contains("access-control"),
        "no CORS header"
    );
    // HEAD reports the GET length and sends no body
    let head = String::from_utf8(to_bytes(&page, true, 0)).unwrap();
    assert_eq!(head, text.strip_suffix("hi").unwrap());
    assert!(head.ends_with("Content-Length: 2\r\n\r\n"));

    // an ordinary response's bytes are unchanged
    let ordinary = Response {
        status: 200,
        headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
        body: b"hi".to_vec(),
    };
    assert_eq!(
        String::from_utf8(to_bytes(&ordinary, false, 0)).unwrap(),
        text.replace(UI_CSP, CSP)
    );
    for f in [
        Refusal::NotFound,
        Refusal::UiBodyTooLarge,
        Refusal::UiContentType,
    ] {
        let t = String::from_utf8(to_bytes(&refusal(f), false, 0)).unwrap();
        assert!(t.contains(&format!("Content-Security-Policy: {CSP}\r\n")));
        assert_eq!(t.matches("Content-Security-Policy:").count(), 1);
    }
}
