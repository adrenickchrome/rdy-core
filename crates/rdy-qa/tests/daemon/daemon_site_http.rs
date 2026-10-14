//! ls6: the site's HTTP server and its event streams (D-N5NSN, D-FV6G9; D-A6ZZA): loopback by
//! default, LAN by opt-in with a warning, the closed guard on every request, a live page in view
//! that keeps the daemon up (ruling task-6: a page in a background tab closes its stream), and a
//! site that stops with its daemon.
//! Bless the goldens with RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test daemon ls6_responses.
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::site::server::MAX_CONNECTIONS;
use rdy_daemon::site::{self, LanOverride, SiteInfo, addr};
use rdy_daemon::{Daemon, DaemonConfig, ExitReason, RunDir, ServeOptions, Shared, start};
use rdy_engine::engine::write_typed;
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::daemon::{EchoHandler, Hosted};
use rdy_qa::golden::{approver, land, tiny};
use rdy_qa::http::{self, Sse};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The deadline of every wait for an event.
const T: Duration = Duration::from_secs(10);

/// Two landed docs, Alpha and Beta, each with one section of two prose blocks (as ls5).
fn two_docs() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = tiny();
    for t in ["Alpha", "Beta"] {
        rdy_qa::golden::write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": t.to_lowercase(), "title": t, "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} first.")},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} second.")}
        ])).unwrap();
    }
    land(&mut r, "work").unwrap();
    (d, r)
}

/// The section of the doc titled `title`.
fn section_of(r: &Rdy, title: &str) -> BlockId {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap()
        .id
        .clone();
    m.children(Some(&doc))[0].id.clone()
}

/// Writes an edit of the second prose block of the doc titled `title` into `view`, and lands
/// the view when `landed`.
fn edit_second_prose(r: &mut Rdy, view: &str, title: &str, body: &str, landed: bool) {
    let sec = section_of(r, title);
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let block = m.children(Some(&sec))[1].id.clone();
    rdy_qa::golden::write(
        r,
        view,
        json!([
            {"op": "update", "id": block.to_string(), "body": body}
        ]),
    )
    .unwrap();
    if landed {
        land(r, view).unwrap();
    }
}

/// The two-doc project, closed, in its directory.
fn project() -> tempfile::TempDir {
    let (dir, r) = two_docs();
    drop(r);
    dir
}

/// Writes `text` into the project's `.rdy/local.yaml`.
fn local_config(dir: &Path, text: &str) {
    std::fs::write(dir.join(".rdy/local.yaml"), text).unwrap();
}

fn host() -> Hosted {
    Hosted::start_in(project())
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

fn info(h: &Hosted) -> SiteInfo {
    shared(h)
        .site_info
        .get()
        .cloned()
        .expect("the site info is set before ready")
}

/// The site's loopback URL, `http://127.0.0.1:<port>/`.
fn url(h: &Hosted) -> String {
    info(h).url.expect("the site listens")
}

fn port_of(url: &str) -> u16 {
    let rest = url.strip_prefix("http://127.0.0.1:").unwrap();
    rest.trim_end_matches('/').parse().unwrap()
}

fn sock(url: &str) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port_of(url)))
}

/// Runs `f` on the daemon's writer.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

fn body(r: &http::Response) -> String {
    String::from_utf8(r.body.clone()).unwrap()
}

/// An open stream of the page at `path` (under the site's root).
fn open_page(url: &str, path: &str) -> Sse {
    let page = http::get(&format!("{url}{path}"));
    assert_eq!(page.status, 200, "{path}");
    Sse::open(&http::events_url(url, &body(&page)))
}

/// Replaces the project's `.rdy` with a fresh directory, as a restore from a copy does.
fn replace_project(dir: &Path) {
    std::fs::rename(dir.join(".rdy"), dir.join(".rdy-old")).unwrap();
    std::fs::create_dir(dir.join(".rdy")).unwrap();
}

/// Starts a host on the project in `h.dir` again, after `h.stop()`.
fn restart(h: &mut Hosted) {
    let run = start(ServeOptions {
        project: h.dir.path().to_path_buf(),
        version: "t".into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: Some("human:qa".into()),
        site_lan_override: None,
    })
    .unwrap();
    h.run = Some(run);
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/site_http")
}

/// Compares `text` (line ends as `\n`) with the golden `name`; `RDY_QA_BLESS=1` writes it.
fn check(name: &str, text: &str) {
    let text = text.replace("\r\n", "\n");
    let file = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{name}: no golden at {}; write it with RDY_QA_BLESS=1",
            file.display()
        )
    });
    assert_eq!(
        text, want,
        "{name}: the response changed from the golden (bless with RDY_QA_BLESS=1 if intended)"
    );
}

#[test]
fn ls6_the_site_listens_on_loopback_only() {
    let h = host();
    let i = info(&h);
    assert!(!i.addrs.is_empty(), "{i:?}");
    assert!(i.addrs.iter().all(|a| a.ip().is_loopback()), "{i:?}");
    let re = regex::Regex::new(r"^http://127\.0\.0\.1:\d+/$").unwrap();
    assert!(re.is_match(i.url.as_ref().unwrap()), "{i:?}");
    assert_eq!(i.lan_url, None);
    assert_eq!(i.warning, None);
}

#[test]
fn ls6_a_page_is_served_with_the_live_script() {
    let h = host();
    let url = url(&h);
    let r = http::get(&url);
    assert_eq!(r.status, 200, "{}", r.head);
    assert_eq!(r.header("Content-Type"), Some("text/html; charset=utf-8"));
    let page = body(&r);
    assert!(page.contains("/_rdy/live.js"), "the live script tag");
    assert!(page.contains("unversioned"), "the label");
    assert!(
        !r.head
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("access-control-")),
        "no CORS header: {}",
        r.head
    );
    let js = http::get(&format!("{url}_rdy/live.js"));
    assert_eq!(js.status, 200);
    assert_eq!(body(&js), rdy_render::live::LIVE_JS);
}

#[test]
fn ls6_a_view_is_served_in_review_mode() {
    let h = host();
    let sh = shared(&h);
    on_writer(&sh, |r| {
        edit_second_prose(r, "draft", "Alpha", "Draft one.", false)
    });
    let url = url(&h);
    let r = http::get(&format!("{url}~view/draft/alpha.html"));
    assert_eq!(r.status, 200, "{}", r.head);
    let page = body(&r);
    assert!(page.contains("Show changes"), "the review toggle");
    assert!(page.contains("Edited"), "the Edited mark");
    assert!(page.contains("scope=view:draft"), "the view's stream");
    let none = http::get(&format!("{url}~view/nosuch/index.html"));
    assert_eq!(none.status, 404, "{}", none.head);
}

#[test]
fn ls6_a_landing_reaches_the_open_page_and_no_other() {
    let h = host();
    let sh = shared(&h);
    let url = url(&h);
    let mut alpha = open_page(&url, "alpha.html");
    let mut beta = open_page(&url, "beta.html");
    on_writer(&sh, |r| {
        edit_second_prose(r, "work", "Alpha", "A new second paragraph.", true)
    });
    assert_eq!(alpha.next_event(T), Some(("reload".into(), "{}".into())));
    on_writer(&sh, |r| {
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
    });
    let label = Some(("label".to_string(), "{\"text\":\"v1.0.0\"}".to_string()));
    // had the landing sent Beta anything, that would come first: one stream keeps its order
    assert_eq!(beta.next_event(T), label);
    assert_eq!(alpha.next_event(T), label);
}

#[test]
fn ls6_a_foreign_host_gets_nothing() {
    let h = host();
    let url = url(&h);
    let port = port_of(&url);
    let r = http::raw(
        sock(&url),
        &format!("GET /index.html HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"),
    );
    assert_eq!(r.status, 403, "{}", r.head);
    assert!(!body(&r).contains("<html"), "no page text");
}

/// The head of the events stream of `path`, up to and including its `retry: 2000` line.
fn events_head(url: &str, path: &str) -> String {
    let page = http::get(&format!("{url}{path}"));
    let events = http::events_url(url, &body(&page));
    let target = events
        .strip_prefix(&format!("http://127.0.0.1:{}", port_of(url)))
        .unwrap();
    let mut s = TcpStream::connect(sock(url)).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        port_of(url)
    )
    .unwrap();
    let mut got = Vec::new();
    let mut byte = [0u8; 1];
    while !got.ends_with(b"retry: 2000\n\n") {
        let n = s.read(&mut byte).unwrap();
        assert_eq!(
            n,
            1,
            "the stream ended early: {}",
            String::from_utf8_lossy(&got)
        );
        got.push(byte[0]);
    }
    String::from_utf8(got).unwrap()
}

#[test]
fn ls6_responses_match_their_goldens() {
    let h = host();
    let sh = shared(&h);
    on_writer(&sh, |r| {
        edit_second_prose(r, "draft", "Alpha", "Draft one.", false)
    });
    let url = url(&h);
    let port = port_of(&url);
    let at = sock(&url);
    let own = format!("Host: 127.0.0.1:{port}");
    let head_of = |request: String| http::normalize(&http::raw(at, &request).head, port);
    let whole = |request: String| {
        let r = http::raw(at, &request);
        http::normalize(&format!("{}\r\n\r\n{}", r.head, body(&r)), port)
    };
    check(
        "page_head",
        &head_of(format!("HEAD /index.html HTTP/1.1\r\n{own}\r\n\r\n")),
    );
    check(
        "view_page_head",
        &head_of(format!(
            "HEAD /~view/draft/index.html HTTP/1.1\r\n{own}\r\n\r\n"
        )),
    );
    check(
        "not_found",
        &whole(format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\n\r\n")),
    );
    check(
        "bad_host",
        &whole(format!(
            "GET /index.html HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"
        )),
    );
    check(
        "foreign_origin",
        &whole(format!(
            "GET /index.html HTTP/1.1\r\n{own}\r\nOrigin: http://rebound.invalid\r\n\r\n"
        )),
    );
    check(
        "method",
        &whole(format!(
            "POST /index.html HTTP/1.1\r\n{own}\r\nContent-Length: 0\r\n\r\n"
        )),
    );
    check(
        "events_head",
        &http::normalize(&events_head(&url, "alpha.html"), port),
    );
}

#[test]
fn ls6_lan_mode_binds_one_private_address_and_warns() {
    let dir = project();
    local_config(dir.path(), "serve:\n  site_lan: true\n");
    let h = Hosted::start_with(dir, Some(LanOverride::Addr(Ipv4Addr::LOCALHOST)));
    let i = info(&h);
    let port = port_of(i.url.as_ref().unwrap());
    let lan_url = format!("http://127.0.0.1:{port}/");
    assert_eq!(i.lan_url.as_deref(), Some(lan_url.as_str()));
    let warning = format!(
        "anyone on your network can read these docs at {lan_url}. Set serve.site_lan to false to keep the site on this machine."
    );
    assert_eq!(i.warning.as_deref(), Some(warning.as_str()));
    assert!(
        site::log_lines(&i).contains(&format!("site: warning: {warning}")),
        "{:?}",
        site::log_lines(&i)
    );
}

#[test]
fn ls6_lan_mode_without_a_private_address_stays_local() {
    let dir = project();
    local_config(dir.path(), "serve:\n  site_lan: true\n");
    let h = Hosted::start_with(dir, Some(LanOverride::NoneFound));
    let i = info(&h);
    assert_eq!(i.lan_url, None);
    assert_eq!(
        i.warning.as_deref(),
        Some(
            "serve.site_lan is on, but this machine has no private network address; the site stays on this machine"
        )
    );
    assert!(i.addrs.iter().all(|a| a.ip().is_loopback()), "{i:?}");
}

#[test]
fn ls6_a_lan_address_not_on_this_machine_stays_local() {
    let dir = project();
    local_config(
        dir.path(),
        "serve:\n  site_lan: true\n  site_lan_address: 10.255.255.254\n",
    );
    let h = Hosted::start_with(dir, None);
    let i = info(&h);
    assert_eq!(i.lan_url, None);
    let w = i.warning.clone().unwrap();
    assert!(w.starts_with("could not listen on 10.255.255.254 ("), "{w}");
    assert!(w.ends_with("); the site stays on this machine"), "{w}");
    assert!(i.addrs.iter().all(|a| a.ip().is_loopback()), "{i:?}");
}

/// A live page in view holds the daemon through its stream (F3 as narrowed by ruling task-6: a
/// page in a background tab closes its stream, and so lets the daemon go).
#[test]
fn ls6_an_open_live_page_keeps_the_daemon() {
    let h = host();
    let handle = h.run.as_ref().unwrap().handle();
    assert_eq!(handle.keepers(), 0, "no client");
    let url = url(&h);
    let page = open_page(&url, "alpha.html");
    assert_eq!(handle.keepers(), 1, "the page in view");
    drop(page);
    let mut keepers = handle.keepers_watch();
    let rt = Hosted::rt();
    let back = rt.block_on(async {
        tokio::time::timeout(T, keepers.wait_for(|n| *n == 0))
            .await
            .map(|r| r.is_ok())
    });
    assert_eq!(back, Ok(true), "the closed stream lets the daemon go");
}

/// Fix round 1: a hold taken before the daemon's run starts still keeps it up. `run` arms the
/// idle timer at its start only when nothing keeps the daemon.
#[test]
fn ls6_a_hold_taken_before_the_run_keeps_the_daemon() {
    let dir = project();
    // one thread runs the daemon and the test, and timers fire in deadline order
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut config = DaemonConfig::new(RunDir::of_project(dir.path()), "t".into());
        config.idle_timeout = Some(Duration::ZERO);
        let mut d = Daemon::new(config, Arc::new(EchoHandler::default()));
        d.bind().await.unwrap();
        let handle = d.handle();
        let hold = handle.hold();
        let run = tokio::spawn(d.run());
        // an ordering fence, not a wait for time: this deadline comes after the run's idle
        // deadline (its start), so a timer armed then has fired and been handled by now
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!run.is_finished(), "the held daemon idled out");
        drop(hold);
        let reason = tokio::time::timeout(T, run).await;
        assert_eq!(
            reason.unwrap().unwrap().unwrap(),
            ExitReason::Idle,
            "with no keeper the idle timer runs"
        );
    });
}

#[test]
fn ls6_a_replaced_project_stops_the_site() {
    let mut h = host();
    let url = url(&h);
    let port = port_of(&url);
    // an open socket connection keeps the idle tick's project check off, so only the HTTP
    // connection's own check can stop the daemon
    let rt = Hosted::rt();
    let client = rt.block_on(h.client(None, None));
    replace_project(h.dir.path());
    let r = http::raw(
        sock(&url),
        &format!("GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(
        r.head.is_empty() && r.body.is_empty(),
        "no reply: {}",
        r.head
    );
    drop(client);
    let run = h.run.take().unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::ProjectGone);
}

/// Fix round 1 (D-A6ZZA): a connection accepted before the project was replaced, whose request
/// comes after, gets no reply. The project check also runs when the request arrives.
#[test]
fn ls6_a_request_after_the_project_is_replaced_gets_no_reply() {
    let mut h = host();
    let url = url(&h);
    let port = port_of(&url);
    // as ls6_a_replaced_project_stops_the_site: only the HTTP side can stop the daemon
    let rt = Hosted::rt();
    let client = rt.block_on(h.client(None, None));
    let mut early = TcpStream::connect(sock(&url)).unwrap();
    early.set_read_timeout(Some(T)).unwrap();
    // fence: one listener accepts in order, so once a later connection has its answer,
    // `early` was accepted, and passed the accept's check
    assert_eq!(http::get(&url).status, 200);
    replace_project(h.dir.path());
    write!(
        early,
        "GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut got = Vec::new();
    let _ = early.read_to_end(&mut got);
    assert!(
        got.is_empty(),
        "no reply: {}",
        String::from_utf8_lossy(&got[..got.len().min(200)])
    );
    drop(client);
    let run = h.run.take().unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::ProjectGone);
}

/// Fix round 1: a connection past [`MAX_CONNECTIONS`] gets the `Busy` answer, whole.
#[test]
fn ls6_a_connection_past_the_limit_gets_busy() {
    let h = host();
    let url = url(&h);
    let port = port_of(&url);
    let held: Vec<TcpStream> = (0..MAX_CONNECTIONS)
        .map(|_| TcpStream::connect(sock(&url)).unwrap())
        .collect();
    let r = http::raw(
        sock(&url),
        &format!("GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(r.status, 503, "{}", r.head);
    assert_eq!(r.body, refusal(Refusal::Busy).body);
    drop(held);
}

#[test]
fn ls6_an_open_page_does_not_stop_the_project_check() {
    let mut h = host();
    let url = url(&h);
    let mut page = open_page(&url, "alpha.html");
    let handle = h.run.as_ref().unwrap().handle();
    assert_eq!(handle.connections(), 0, "no socket connection");
    replace_project(h.dir.path());
    handle.tick();
    assert_eq!(page.next_event(T), None);
    assert!(page.ended(), "the stream ended before its deadline");
    let run = h.run.take().unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::ProjectGone);
}

#[test]
fn ls6_a_stopped_daemon_ends_the_streams() {
    let mut h = host();
    let url = url(&h);
    let mut page = open_page(&url, "alpha.html");
    assert_eq!(h.stop(), ExitReason::Stopped);
    assert_eq!(page.next_event(T), None);
    assert!(page.ended(), "the stream ended before its deadline");
}

#[test]
fn ls6_a_restarted_daemon_keeps_the_address() {
    let mut h = host();
    let before = url(&h);
    // served answers and a stream leave the site's side of each closed connection in TIME_WAIT
    // on the port: the successor binds it all the same
    assert_eq!(http::get(&before).status, 200);
    drop(open_page(&before, "alpha.html"));
    h.stop();
    restart(&mut h);
    let after = url(&h);
    if after != before {
        // a fallback is right only when another process holds the port now
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, port_of(&before))).is_ok();
        assert!(!free, "{before} was free, but the restart moved to {after}");
        let root = rdy_daemon::RunDir::of_project(h.dir.path()).root();
        eprintln!("the derived port was taken meanwhile: {before} became {after}");
        assert!(
            addr::candidates(None, &root).contains(&port_of(&after)),
            "{after}"
        );
    }
}

#[test]
fn ls6_a_config_edit_shows_on_the_next_load() {
    let h = host();
    let url = url(&h);
    assert!(!body(&http::get(&url)).contains("Renamed"));
    std::fs::write(
        h.dir.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: \"Renamed\"\n",
    )
    .unwrap();
    let r = http::get(&url);
    assert_eq!(r.status, 200, "{}", r.head);
    assert!(
        body(&r).contains("Renamed"),
        "the page load reloads the config"
    );
}

#[test]
fn ls6_a_port_clash_falls_back() {
    let dir = project();
    let root = rdy_daemon::RunDir::of_project(dir.path()).root();
    let c = addr::candidates(None, &root);
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, c[0])).ok();
    // the next candidate that is free right now (in practice the second)
    let next = c[1..]
        .iter()
        .copied()
        .find(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, *p)).is_ok())
        .unwrap();
    let h = Hosted::start_in(dir);
    assert_eq!(port_of(&url(&h)), next);
}

#[test]
fn ls6_a_set_port_that_is_taken_turns_the_site_off() {
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(port >= 1024, "guard: an unprivileged port");
    let dir = project();
    local_config(dir.path(), &format!("serve:\n  site_port: {port}\n"));
    let h = Hosted::start_in(dir);
    let i = info(&h);
    assert!(i.url.is_err(), "{i:?}");
    assert!(i.addrs.is_empty(), "{i:?}");
    let rt = Hosted::rt();
    let pid = rt.block_on(async {
        let mut c = h.client(None, None).await;
        c.ctl("ping", json!({})).await.unwrap()["pid"].clone()
    });
    assert_eq!(pid, json!(std::process::id()), "the daemon runs on");
}
