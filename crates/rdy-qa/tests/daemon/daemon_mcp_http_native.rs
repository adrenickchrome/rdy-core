//! mht9 (D-6M0AF, D-44NVC, D-QG54Z, D-5GFFX, D-FARN9): MCP over HTTP through the real `rdy`
//! binary and its real daemon child, on the platform that runs the test. The file is portable:
//! every platform runs the same endpoint, body, claim, stream, stop and replacement scenarios,
//! and the platform seams (`rdy_qa::platform`) judge the token's privacy, its links and the
//! second-account read. On Windows those are the protected ACL, the reparse rejection and a real
//! second account (the native gate; a hosted runner has none), and the daemon starts and stops
//! through its named-pipe control endpoint. Every URL is the one `rdy mcp --http-config` prints
//! (W5), never one derived from the port candidates. Every child runs in the fixture home.
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::{ExitReason, Proto, RunDir};
use rdy_qa::daemon::{Hosted, Served, connect_ready, wait_endpoint_gone};
use rdy_qa::golden::tiny;
use rdy_qa::mcp_http::{
    Endpoint, LEGACY, MODERN, Reply, call, claim, legacy, listen, modern, modern_meta, pairs,
    request, send,
};
use rdy_qa::platform;
use rdy_qa::skip::{Why, skip};
use serde_json::{Value, json};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// A hang guard for what a test waits on: a failure message, never a measure.
const T: Duration = Duration::from_secs(60);
const ON: &str = "serve:\n  mcp_http: true\n";
const NO_TOKEN: &str = "this endpoint needs the project's MCP token; run rdy mcp --http-config";
const ORIGIN: &str = "MCP over HTTP does not accept an Origin header.";
const OVER: &str = "the MCP request body is over 16777216 bytes";
const BOUND: usize = 16 * 1024 * 1024;
/// A build that is not this binary's: a fixed synthetic id, never the live one.
const OTHER_BUILD: &str = "0123456789abcdef";

fn local(dir: &Path, config: &str) {
    std::fs::write(dir.join(".rdy/local.yaml"), config).unwrap();
}

/// A served project (real `rdy`, real daemon child) with MCP over HTTP on.
fn on() -> Served {
    let s = Served::new();
    local(s.dir.path(), ON);
    s
}

fn out(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The endpoint `rdy mcp --http-config` prints: its URL and bearer, nothing derived (W5).
fn endpoint(s: &Served) -> Endpoint {
    let v: Value = serde_json::from_str(out(&s.rdy(&["--json", "mcp", "--http-config"])).trim())
        .expect("the config JSON");
    let url = v["url"].as_str().expect("a url");
    let site = url
        .strip_suffix("~mcp")
        .unwrap_or_else(|| panic!("not an MCP url: {url}"));
    let token = v["headers"]["Authorization"]
        .as_str()
        .and_then(|a| a.strip_prefix("Bearer "))
        .expect("a bearer");
    Endpoint::of(site, token)
}

fn token_path(dir: &Path) -> std::path::PathBuf {
    dir.join(".rdy/run/mcp-token")
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + T;
    while !f() {
        assert!(Instant::now() < end, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_text(r: &Reply, status: u16, text: &str) {
    assert_eq!((r.status, r.text().as_str()), (status, text), "{}", r.head);
}

fn overview(id: u64, actor: &str, run: &str) -> Value {
    call(id, "rdy_overview", json!({}), claim(actor, run))
}

fn modern_call(ep: &Endpoint, id: u64, name: &str, args: Value, actor: &str, run: &str) -> Reply {
    let h = modern("tools/call", Some(name));
    ep.post(
        &pairs(&h),
        &call(id, name, args, modern_meta(claim(actor, run))),
    )
}

fn open_listen(ep: &Endpoint, uris: &[&str], actor: &str, run: &str) -> rdy_qa::mcp_http::Stream {
    let h = modern("subscriptions/listen", None);
    let mut s = ep.open(&pairs(&h), &listen(9, uris, modern_meta(claim(actor, run))));
    assert_eq!(s.status, 200, "{}", s.head);
    assert_eq!(s.header("content-type"), Some("text/event-stream"));
    let ack = s.next(T).expect("the acknowledgement");
    assert!(ack.to_string().contains("acknowledged"), "{ack}");
    s
}

/// A raw request: `head` and `body`, on a connection of its own.
fn raw(ep: &Endpoint, head: &str, body: &[u8]) -> Reply {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    send(ep.addr, &bytes)
}

/// The head of a POST with the bearer, a legacy revision and `framing` lines.
fn post_head(ep: &Endpoint, framing: &str) -> String {
    format!(
        "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\n{framing}\r\n",
        ep.authority, ep.token, LEGACY[1]
    )
}

/// The scenarios every endpoint of the project must give: the same on each loopback address.
fn scenarios(ep: &Endpoint, tag: &str) {
    // the endpoint: no bearer, a wrong bearer, any Origin, a rebinding Host, a method POST lacks
    let body = overview(1, "agent:n", "n1").to_string();
    let bare = |auth: &str, extra: &str| {
        raw(
            ep,
            &format!(
                "POST /~mcp HTTP/1.1\r\nHost: {}\r\n{auth}{extra}Accept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {}\r\nContent-Length: {}\r\n\r\n",
                ep.authority,
                LEGACY[0],
                body.len()
            ),
            body.as_bytes(),
        )
    };
    let refused = bare("", "");
    assert_text(&refused, 401, NO_TOKEN);
    assert_eq!(refused.header("www-authenticate"), Some("Bearer"));
    assert!(
        !refused.has_header_prefix("access-control-"),
        "{}",
        refused.head
    );
    let mut other = ep.token.clone().into_bytes();
    other[0] = if other[0] == b'A' { b'B' } else { b'A' };
    let wrong = format!(
        "Authorization: Bearer {}\r\n",
        String::from_utf8(other).unwrap()
    );
    assert_text(&bare(&wrong, ""), 401, NO_TOKEN);
    let good = format!("Authorization: Bearer {}\r\n", ep.token);
    for origin in ["http://evil.example", "null", ""] {
        let r = bare(&good, &format!("Origin: {origin}\r\n"));
        assert_text(&r, 403, ORIGIN);
        assert!(!r.has_header_prefix("access-control-"), "{}", r.head);
    }
    let rebound = raw(
        ep,
        &format!(
            "POST /~mcp HTTP/1.1\r\nHost: attacker.example:{}\r\n{good}Content-Length: {}\r\n\r\n",
            ep.port(),
            body.len()
        ),
        body.as_bytes(),
    );
    assert_eq!(
        (rebound.status, &rebound.body),
        (403, &refusal(Refusal::Host).body),
        "{}",
        rebound.head
    );
    // D-44NVC: with the bearer, GET is a 405 and `Allow: POST`; never an event stream
    for method in ["GET", "HEAD"] {
        let r = raw(
            ep,
            &format!(
                "{method} /~mcp HTTP/1.1\r\nHost: {}\r\n{good}Accept: text/event-stream\r\n\r\n",
                ep.authority
            ),
            b"",
        );
        assert_eq!(r.status, 405, "{method}: {}", r.head);
        assert_eq!(r.header("allow"), Some("POST"), "{method}");
    }

    // claims: a claimed call per revision, a claimless one refused by the tool, and the claim
    // held across connections (each request its own connection)
    for rev in LEGACY {
        let r = ep.post(&pairs(&legacy(rev)), &overview(2, "agent:n", "n1"));
        let (err, v) = r.tool();
        assert!(!err, "{v}");
        assert_eq!(v["session"], json!({"actor": "agent:n", "view": "n1"}));
        assert!(r.header("mcp-session-id").is_none(), "{}", r.head);
        let none = ep.post(
            &pairs(&legacy(rev)),
            &call(3, "rdy_overview", json!({}), json!({})),
        );
        let (err, v) = none.tool();
        assert!(err);
        assert_eq!(v["code"], "E_NO_SESSION", "{v}");
    }
    let (err, v) = modern_call(ep, 4, "rdy_overview", json!({}), "agent:n", "n1").tool();
    assert!(!err, "{v}");
    assert_eq!(v["session"]["actor"], "agent:n");

    // bodies: one request in chunks, ambiguous framing, and the bound
    let n = body.len();
    let (a, b) = body.as_bytes().split_at(n / 2);
    let chunked = [
        format!("{:x}\r\n", a.len()).into_bytes(),
        a.to_vec(),
        b"\r\n".to_vec(),
        format!("{:x}\r\n", b.len()).into_bytes(),
        b.to_vec(),
        b"\r\n0\r\n\r\n".to_vec(),
    ]
    .concat();
    let r = raw(
        ep,
        &post_head(ep, "Transfer-Encoding: chunked\r\n"),
        &chunked,
    );
    assert!(!r.tool().0, "{} {}", r.head, r.text());
    let bad = refusal(Refusal::Malformed);
    for framing in [
        format!("Content-Length: {n}\r\nTransfer-Encoding: chunked\r\n"),
        format!("Content-Length: {n}\r\nContent-Length: {n}\r\n"),
        "Content-Length: x\r\n".to_string(),
    ] {
        let r = raw(ep, &post_head(ep, &framing), body.as_bytes());
        assert_eq!(
            (r.status, &r.body),
            (400, &bad.body),
            "{framing:?}: {}",
            r.head
        );
    }
    let r = raw(
        ep,
        &post_head(ep, &format!("Content-Length: {}\r\n", BOUND + 1)),
        b"",
    );
    assert_text(&r, 413, OVER);

    // streams: a modern listen is acknowledged, carries a write's notification, and ends when
    // the client closes it
    let mut s = open_listen(ep, &["rdy://view/n1"], "agent:n", "n1");
    let w = modern_call(
        ep,
        20,
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": tag, "title": tag}}]}),
        "agent:n",
        "n1",
    );
    assert!(!w.tool().0, "{}", w.text());
    let m = s.next(T).expect("a notification on the open POST");
    assert_eq!(m["method"], "notifications/resources/updated", "{m}");
    assert_eq!(m["params"]["uri"], "rdy://view/n1", "{m}");
    drop(s);
    // a tools/list needs no claim, and the server speaks the modern revision too
    let h = modern("tools/list", None);
    let list = ep.post(
        &pairs(&h),
        &request(5, "tools/list", json!({"_meta": modern_meta(json!({}))})),
    );
    assert_eq!(list.status, 200, "{}", list.text());
    assert_eq!(MODERN, "2026-07-28");
}

/// The fixture home of a served project's commands.
fn home_of(s: &Served) -> std::path::PathBuf {
    let env = s.env_pairs();
    let value = env
        .iter()
        .find(|(k, _)| k == "HOME" || k == "USERPROFILE")
        .map(|(_, v)| v.clone())
        .expect("the fixture sets a home");
    value.into()
}

/// No command of the served project looked up or made the operator's meta project.
fn no_meta_read(s: &Served) {
    let home = home_of(s);
    assert!(
        !home.join(".rdy/meta").exists(),
        "a process made or started a meta project under {}",
        home.display()
    );
}

#[test]
fn mht9_native_http_auth_and_listen_match_on_both_platforms() {
    let s = on();
    let ep = endpoint(&s);
    let token = token_path(s.dir.path());
    // the bearer's files are closed to another account: the ACL on Windows, the mode elsewhere
    eprintln!(
        "mht9 second account proves: {}",
        platform::ANOTHER_ACCOUNT_PROOF
    );
    for object in [token.as_path(), s.run_dir().path()] {
        assert!(
            !platform::another_account_can_read(object).expect("check"),
            "another account reads {}",
            object.display()
        );
    }
    assert_eq!(ep.addr.ip().to_string(), "127.0.0.1", "{ep:?}");
    scenarios(&ep, "v4");
    no_meta_read(&s);

    // IPv6 loopback, where this host has it: the site binds [::1] on the same port, best effort
    let name = "mht9_native_http_auth_and_listen_match_on_both_platforms";
    if TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_err() {
        skip(name, Why::Prerequisite, "this host cannot bind [::1]");
        return;
    }
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, ep.port()));
    if TcpStream::connect_timeout(&v6, T).is_err() {
        skip(
            name,
            Why::Prerequisite,
            "the daemon did not bind [::1] on the site port",
        );
        return;
    }
    let v6_ep = Endpoint {
        addr: v6,
        authority: format!("[::1]:{}", ep.port()),
        token: ep.token.clone(),
    };
    scenarios(&v6_ep, "v6");
}

#[test]
fn mht9_native_stop_and_build_replacement_close_http() {
    // `rdy serve --stop` (the daemon's own control endpoint: a named pipe on Windows) ends an
    // open listen and closes the port
    let s = on();
    let ep = endpoint(&s);
    let pid = s.daemon_pid().expect("the daemon runs");
    let mut stream = open_listen(&ep, &["rdy://project"], "agent:p", "p1");
    s.stop();
    assert_eq!(stream.next(T), None);
    assert!(stream.ended(), "the stop ended the stream");
    wait_endpoint_gone(&s.run_dir());
    until("the daemon process exits", || platform::process_exited(pid));
    until("the site port closes", || {
        TcpStream::connect_timeout(&ep.addr, Duration::from_secs(2)).is_err()
    });
    no_meta_read(&s);

    // a client of another build waits for the open listen, and replaces the daemon when it
    // ends: the port closes with the daemon
    let (dir, r) = tiny();
    drop(r);
    local(dir.path(), ON);
    let mut h = Hosted::start_in(dir);
    let site = h
        .run
        .as_ref()
        .and_then(|r| r.shared().site_info.get().and_then(|i| i.url.clone().ok()))
        .expect("the site listens");
    let token = std::fs::read_to_string(token_path(h.dir.path())).unwrap();
    let ep = Endpoint::of(&site, token.trim_end());
    let mut stream = open_listen(&ep, &["rdy://project"], "agent:b", "b1");
    let run = h.run.take().expect("the host runs");
    let run_dir = RunDir::of_project(h.dir.path());
    let mut newer = h.hello(Proto::Ctl);
    newer.build = OTHER_BUILD.into();
    let rt = Hosted::rt();
    let other = rt.block_on(connect_ready(&run_dir, &newer));
    assert_eq!(
        stream.next(Duration::from_millis(200)),
        None,
        "the open listen keeps the current build serving"
    );
    assert!(!stream.ended(), "the replacement waits for the listen");
    let ok = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:b", "b1"));
    assert!(!ok.tool().0, "{}", ok.text());
    other.close();
    drop(stream);
    assert_eq!(run.wait().unwrap(), ExitReason::BuildReplaced);
    until("the replaced daemon's port closes", || {
        TcpStream::connect_timeout(&ep.addr, Duration::from_secs(2)).is_err()
    });
}

#[test]
fn mht9_native_private_token_survives_restart() {
    let s = on();
    let first = endpoint(&s);
    let token = token_path(s.dir.path());
    let bytes = std::fs::read(&token).expect("the token file");
    assert!(
        !platform::another_account_can_read(&token).expect("check"),
        "the token is closed to another account"
    );
    s.stop();
    wait_endpoint_gone(&s.run_dir());

    // the next daemon prepares the same token, and the old bearer still opens its endpoint
    let second = endpoint(&s);
    assert_eq!(second.token, first.token, "the token survives a restart");
    assert_eq!(
        std::fs::read(&token).unwrap(),
        bytes,
        "the file is not rewritten"
    );
    assert!(
        !platform::another_account_can_read(&token).expect("check"),
        "the token stays closed after the restart"
    );
    let r = second.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:r", "r1"));
    assert!(!r.tool().0, "{}", r.text());
    s.stop();
    wait_endpoint_gone(&s.run_dir());

    // a second name of the token's directory is refused, never followed (a standard Windows
    // account may be refused the follow itself, W2), and nothing reaches its target
    let outside = tempfile::tempdir().expect("a directory outside the project");
    let target = outside.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::remove_file(&token).unwrap();
    platform::dir_alias(&target, &token).expect("make the directory alias");
    assert!(
        platform::follow_refused(&token) || std::fs::metadata(&token).is_ok_and(|m| m.is_dir()),
        "the alias resolves or the system refuses to follow it"
    );
    let o = s.rdy(&["--json", "mcp", "--http-config"]);
    assert!(
        !o.status.success(),
        "a linked token is no token: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&o.stderr).trim()).expect("JSON");
    assert_eq!(v["code"], "E_SITE", "{v}");
    assert_eq!(
        std::fs::read_dir(&target).unwrap().count(),
        0,
        "nothing reached the target"
    );
    platform::remove_dir_link(&token).expect("remove the alias");
    no_meta_read(&s);
}

/// The child of `mht9_native_site_port_cannot_be_taken_by_second_process`: another process of
/// this binary that tries to listen on the daemon's port, and says what the system answered.
#[test]
fn mht9_bind_probe_child() {
    let Ok(spec) = std::env::var("RDY_QA_MHT9_BIND") else {
        return;
    };
    let mut taken = Vec::new();
    for addr in spec.split(',') {
        let addr: SocketAddr = addr.parse().expect("an address");
        match TcpListener::bind(addr) {
            Ok(l) => {
                taken.push(addr);
                drop(l);
            }
            Err(e) => eprintln!("mht9 bind {addr}: refused as expected: {e}"),
        }
    }
    // do not return into the harness: the answer is the exit code
    std::process::exit(if taken.is_empty() {
        0
    } else {
        eprintln!("mht9 bind: took {taken:?}");
        3
    });
}

#[test]
fn mht9_native_site_port_cannot_be_taken_by_second_process() {
    let s = on();
    let ep = endpoint(&s);
    let pid = s.daemon_pid().expect("the daemon runs");
    let port = ep.port();
    assert_eq!(
        platform::tcp_listener_pids(port),
        std::collections::BTreeSet::from([pid]),
        "only the daemon listens on {port}"
    );
    // a second process, on every address the daemon holds
    let mut addrs = vec![SocketAddr::from(([127, 0, 0, 1], port))];
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&v6, T).is_ok() {
        addrs.push(v6);
    }
    let spec: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &rdy_qa::test_name!("mht9_bind_probe_child"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RDY_QA_MHT9_BIND", spec.join(","))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a second process took the site's port: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // the daemon still serves, and still alone on the port
    let r = ep.post(&pairs(&legacy(LEGACY[0])), &overview(1, "agent:x", "x1"));
    assert!(!r.tool().0, "{}", r.text());
    assert_eq!(
        platform::tcp_listener_pids(port),
        std::collections::BTreeSet::from([pid])
    );
    no_meta_read(&s);
}
