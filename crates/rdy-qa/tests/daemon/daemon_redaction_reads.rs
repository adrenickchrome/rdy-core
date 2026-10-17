//! rdx14: the live site keeps its pages, menus and events within the current redaction epoch
//! (D-CC90E: "every view that holds the content shows the stub"; D-3M81W; D-X61DT). A build that
//! read a state from before a removal never publishes after it; while the published state is
//! older than the removal, every page, asset and event request gets the existing `Busy` answer;
//! an open page gets the existing `reload` event once the fresh build is published. A response
//! in progress never delays the removal (no lease, no drain). Barriers pause builds at the
//! publication check: nothing here sleeps.
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{Refusal, ScopeRef, refusal};
use rdy_daemon::site::live::{LiveEvent, LiveSite};
use rdy_daemon::site::sse::{self, EpochNow};
use rdy_engine::engine::write_typed;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{E_REDACTED_STATE, Rdy};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, writer};
use rdy_qa::golden::{land, tiny};
use rdy_qa::http::{self, Sse};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The deadline of every wait for an event.
const T: Duration = Duration::from_secs(10);
/// The content the first removal selects.
const SECRET: &str = "ZEBRA-7731 is the launch code.";
/// The content the second removal selects.
const OTHER: &str = "OKAPI-4410 is the backup code.";
/// What a removed block shows.
const STUB: &str = "Content removed.";

/// The ids of the two removable prose blocks.
struct Ids {
    secret: String,
    other: String,
}

/// A host over a project whose canon holds doc Alpha (the first prose block holds `SECRET`, the
/// second `OTHER`, the third neither) and doc Beta, landed and tagged `v1.0.0`.
fn host() -> (Hosted, Ids) {
    let (dir, mut r) = tiny();
    let w = rdy_qa::golden::write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$a", "fields": {"title": "Part"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": SECRET},
        {"op": "create", "type": "prose", "id": "$q", "parent": "$s", "body": OTHER},
        {"op": "create", "type": "prose", "parent": "$s", "body": "An unrelated paragraph."},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$t", "parent": "$b", "fields": {"title": "Part"}},
        {"op": "create", "type": "prose", "parent": "$t", "body": "Beta stays."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let ids = Ids {
        secret: w.created["$p"].to_string(),
        other: w.created["$q"].to_string(),
    };
    drop(r);
    (Hosted::start_in(dir), ids)
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// The site's loopback URL, `http://127.0.0.1:<port>/`.
fn url(h: &Hosted) -> String {
    shared(h)
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens")
}

/// Runs `f` on the daemon's writer; the writer publishes once `f` returns.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

/// Records a removal request of `target` in view `view` and proposes it; returns the proposal.
fn request(sh: &Arc<Shared>, target: &str, view: &'static str) -> String {
    let target = target.to_string();
    let v = on_writer(sh, move |r| {
        let res = r
            .redact_in(
                &writer(),
                view,
                RedactInput {
                    targets: vec![target],
                    reason_class: "secret".into(),
                    reason: "a leaked code".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        assert!(res.redaction.is_some(), "the request is recorded");
        r.propose(
            &writer(),
            view,
            "Remove the code",
            Some("Removes the leaked code."),
        )
        .unwrap()
        .id
    });
    v.as_str().unwrap().to_string()
}

/// A person approves proposal `id`: the removal is authorized and its bytes are erased.
fn approve(r: &mut Rdy, id: &str) {
    r.land(&approver(), id).unwrap();
}

/// The store family's redaction epoch now.
fn epoch(sh: &Shared) -> u64 {
    sh.state.load().reader().redaction_epoch()
}

fn text(r: &http::Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

/// Asserts `r` is the existing `Busy` answer, with its unchanged text.
fn assert_busy(r: &http::Response, what: &str) {
    let busy = refusal(Refusal::Busy);
    assert_eq!(r.status, busy.status, "{what}: {}", r.head);
    assert_eq!(r.body, busy.body, "{what}: the Busy text");
}

/// `GET url`, read to the end, or only through the head of a `200` event stream (which never
/// ends): the status, the head and what came after it so far.
fn get_or_head(url: &str) -> http::Response {
    let rest = url.strip_prefix("http://").unwrap();
    let at = rest.find('/').unwrap();
    let (authority, path) = (&rest[..at], &rest[at..]);
    let mut s = TcpStream::connect(authority).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {authority}\r\n\r\n").unwrap();
    let mut got = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).into_owned();
            if head.starts_with("HTTP/1.1 200") && head.contains("text/event-stream") {
                return http::Response {
                    status: 200,
                    head,
                    body: got[end + 4..].to_vec(),
                };
            }
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    }
    let end = got
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(got.len());
    let head = String::from_utf8_lossy(&got[..end]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    http::Response {
        status,
        head,
        body: got.get(end + 4..).unwrap_or_default().to_vec(),
    }
}

/// A barrier at the publication check of every build of `scope`: the build reports it is there
/// on the returned receiver, then waits until the returned sender releases it. Builds of other
/// scopes pass.
fn hold(site: &LiveSite, scope: ScopeRef) -> (Receiver<()>, Sender<()>) {
    let (at_tx, at_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let at_tx = Mutex::new(at_tx);
    let go_rx = Mutex::new(go_rx);
    site.set_before_publish(Some(Box::new(move |s: &ScopeRef| {
        if *s == scope {
            let _ = at_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv();
        }
    })));
    (at_rx, go_tx)
}

/// Whether any file of the build holds `needle`.
fn build_holds(b: &rdy_render::live::LiveBuild, needle: &str) -> bool {
    b.files
        .values()
        .any(|f| f.windows(needle.len()).any(|w| w == needle.as_bytes()))
}

#[test]
fn rdx14_delayed_live_builds_cannot_publish_an_old_epoch() {
    let (h, ids) = host();
    let sh = shared(&h);
    let url = url(&h);

    // a page build of canon reads the state before the removal and pauses at the check
    let p = request(&sh, &ids.secret, "redact-1");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, ScopeRef::Canon);
    let held = {
        let sh = Arc::clone(&sh);
        std::thread::spawn(move || {
            sh.site
                .refresh(&sh, &ScopeRef::Canon)
                .map(|b| (b.epoch, build_holds(&b, SECRET)))
        })
    };
    at.recv().unwrap();
    on_writer(&sh, move |r| approve(r, &p));
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    go.send(()).unwrap();
    match held.join().unwrap() {
        Err(e) => assert_eq!(e.code, E_REDACTED_STATE, "{e}"),
        Ok((at, leaks)) => panic!(
            "a build of epoch {at} published after the removal (epoch {}); holds the content: {leaks}",
            epoch(&sh)
        ),
    }
    sh.site.set_before_publish(None);
    // later requests serve the new state only
    let b = sh.site.refresh(&sh, &ScopeRef::Canon).unwrap();
    assert_eq!(
        b.epoch,
        epoch(&sh),
        "the served build is of the current epoch"
    );
    assert!(
        !build_holds(&b, SECRET),
        "the served build holds no removed content"
    );
    let page = http::get(&format!("{url}alpha.html"));
    assert_eq!(page.status, 200, "{}", page.head);
    assert!(!text(&page).contains(SECRET) && text(&page).contains(STUB));

    // a menu build reads the state before the second removal and pauses at the check
    sh.site.refresh(&sh, &ScopeRef::Versions).unwrap();
    let p = request(&sh, &ids.other, "redact-2");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, ScopeRef::Versions);
    let held = {
        let sh = Arc::clone(&sh);
        std::thread::spawn(move || sh.site.refresh(&sh, &ScopeRef::Versions).map(|b| b.epoch))
    };
    at.recv().unwrap();
    on_writer(&sh, move |r| approve(r, &p));
    assert!(epoch(&sh) > before, "the second removal advanced the epoch");
    go.send(()).unwrap();
    match held.join().unwrap() {
        Err(e) => assert_eq!(e.code, E_REDACTED_STATE, "{e}"),
        Ok(at) => panic!(
            "a menu of epoch {at} published after the removal (epoch {})",
            epoch(&sh)
        ),
    }
    sh.site.set_before_publish(None);
    let menu = sh.site.refresh(&sh, &ScopeRef::Versions).unwrap();
    assert_eq!(
        menu.epoch,
        epoch(&sh),
        "the served menu is of the current epoch"
    );
    let page = http::get(&format!("{url}~versions/"));
    assert_eq!(page.status, 200, "{}", page.head);
    let page = http::get(&format!("{url}alpha.html"));
    assert_eq!(page.status, 200, "{}", page.head);
    let body = text(&page);
    assert!(!body.contains(SECRET) && !body.contains(OTHER), "{body}");
}

#[test]
fn rdx14_stale_pages_assets_and_events_refuse_during_rebuild() {
    let (h, ids) = host();
    let sh = shared(&h);
    let url = url(&h);
    // the old build: served, cached, and holding the content
    let page = http::get(&format!("{url}alpha.html"));
    assert_eq!(page.status, 200, "{}", page.head);
    assert!(text(&page).contains(SECRET));
    let events = http::events_url(&url, &text(&page));
    let full = http::get(&format!("{url}llms-full.txt"));
    assert!(
        text(&full).contains(SECRET),
        "llms-full.txt holds the content"
    );
    assert_eq!(http::get(&format!("{url}assets/site.css")).status, 200);

    // the removal is authorized and erased; the writer has not published its state yet
    let p = request(&sh, &ids.secret, "redact");
    let before = epoch(&sh);
    let (landed_tx, landed_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let writer = {
        let sh = Arc::clone(&sh);
        std::thread::spawn(move || {
            on_writer(&sh, move |r| {
                approve(r, &p);
                landed_tx.send(()).unwrap();
                go_rx.recv().unwrap();
            })
        })
    };
    landed_rx.recv().unwrap();
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    for (what, path) in [
        ("the page", "alpha.html".to_string()),
        ("another page", "beta.html".to_string()),
        ("an asset", "assets/site.css".to_string()),
        ("the llms file", "llms-full.txt".to_string()),
        ("the live script", "_rdy/live.js".to_string()),
    ] {
        let r = http::get(&format!("{url}{path}"));
        assert!(!text(&r).contains(SECRET), "{what}: no removed content");
        assert_busy(&r, what);
    }
    let r = get_or_head(&events);
    assert!(!text(&r).contains(SECRET), "the events: no removed content");
    assert_busy(&r, "the events");

    // the writer publishes the fresh state: the fresh build is served
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    let page = http::get(&format!("{url}alpha.html"));
    assert_eq!(page.status, 200, "{}", page.head);
    assert!(!text(&page).contains(SECRET) && text(&page).contains(STUB));
    let full = http::get(&format!("{url}llms-full.txt"));
    assert_eq!(full.status, 200, "{}", full.head);
    assert!(
        !text(&full).contains(SECRET),
        "llms-full.txt holds no removed content"
    );
    assert_eq!(http::get(&format!("{url}assets/site.css")).status, 200);
    drop(Sse::open(&http::events_url(&url, &text(&page))));
}

#[test]
fn rdx14_sse_refresh_has_no_removed_content() {
    let (h, ids) = host();
    let sh = shared(&h);
    let url = url(&h);
    // old subscribers: the page with the content, and a page without it
    let alpha = http::get(&format!("{url}alpha.html"));
    assert!(text(&alpha).contains(SECRET));
    let beta = http::get(&format!("{url}beta.html"));
    let mut a = Sse::open(&http::events_url(&url, &text(&alpha)));
    let mut b = Sse::open(&http::events_url(&url, &text(&beta)));
    let p = request(&sh, &ids.secret, "redact");
    on_writer(&sh, move |r| approve(r, &p));
    // the existing refresh shape, carrying nothing
    for (what, s) in [("alpha", &mut a), ("beta", &mut b)] {
        loop {
            let got = s.next_event(T);
            let Some((name, data)) = got else {
                panic!("{what}: no reload after the fresh state was published");
            };
            assert!(!data.contains(SECRET), "{what}: {data}");
            if name == "reload" {
                assert_eq!(data, "{}", "{what}: the existing refresh event");
                break;
            }
        }
    }
    // then current content only
    for path in ["alpha.html", "beta.html", "llms-full.txt", "index.html"] {
        let r = http::get(&format!("{url}{path}"));
        assert_eq!(r.status, 200, "{path}: {}", r.head);
        assert!(!text(&r).contains(SECRET), "{path}");
    }
    assert!(text(&http::get(&format!("{url}alpha.html"))).contains(STUB));
}

#[test]
fn rdx14_a_prior_response_does_not_delay_erasure() {
    let (h, ids) = host();
    let sh = shared(&h);
    let url = url(&h);
    let tag = format!("{url}~tag/v1.0.0/alpha.html");
    // a held response: an open event stream of the page with the content
    let alpha = http::get(&format!("{url}alpha.html"));
    assert!(text(&alpha).contains(SECRET));
    let stream = Sse::open(&http::events_url(&url, &text(&alpha)));
    // and a request whose build read the state before the removal, paused at the check
    let p = request(&sh, &ids.secret, "redact");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, ScopeRef::Tag("v1.0.0".into()));
    let first = {
        let tag = tag.clone();
        std::thread::spawn(move || http::get(&tag))
    };
    at.recv().unwrap();
    // neither held response blocks the authorization
    on_writer(&sh, move |r| approve(r, &p));
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    sh.site.set_before_publish(None);
    // a new request, behind the held build
    let second = {
        let tag = tag.clone();
        std::thread::spawn(move || http::get(&tag))
    };
    go.send(()).unwrap();
    let first = first.join().unwrap();
    assert!(
        !text(&first).contains(SECRET),
        "the held build was discarded: {}",
        first.head
    );
    assert_busy(&first, "the request whose build predates the removal");
    let second = second.join().unwrap();
    assert_eq!(second.status, 200, "{}", second.head);
    assert!(!text(&second).contains(SECRET) && text(&second).contains(STUB));
    let again = http::get(&tag);
    assert_eq!(again.status, 200, "{}", again.head);
    assert!(!text(&again).contains(SECRET));
    drop(stream);
}

/// Reads the stream on `client` into `got` through the first `\n\n` at or after `from`: the
/// next frame, or what came before the stream ended. Returns where it ends in `got`.
fn next_frame(client: &mut TcpStream, got: &mut Vec<u8>, from: usize) -> usize {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(at) = got[from..].windows(2).position(|w| w == b"\n\n") {
            return from + at + 2;
        }
        match client.read(&mut chunk) {
            Ok(0) | Err(_) => return got.len(),
            Ok(n) => got.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Reads the stream's head and its reconnect delay ([`sse::RETRY`]) into `got`; returns where
/// they end.
fn stream_head(client: &mut TcpStream, got: &mut Vec<u8>) -> usize {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(at) = got.windows(sse::RETRY.len()).position(|w| w == sse::RETRY) {
            let head = String::from_utf8_lossy(&got[..at]);
            assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
            return at + sse::RETRY.len();
        }
        match client.read(&mut chunk) {
            Ok(n) if n > 0 => got.extend_from_slice(&chunk[..n]),
            _ => panic!("the stream ended before its head"),
        }
    }
}

/// A served event stream of `sub` over a loopback connection: the reader's end, the sender that
/// stops it, and the receiver of each value the stream's epoch probe returned (the server's
/// probe, [`sse::epoch_now`]), one per event it considered.
fn serve(
    h: &Hosted,
    rt: &tokio::runtime::Runtime,
    sub: rdy_daemon::site::live::Subscription,
) -> (
    TcpStream,
    tokio::sync::watch::Sender<bool>,
    Receiver<Option<u64>>,
) {
    let sh = shared(h);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_read_timeout(Some(T)).unwrap();
    let (conn, _) = listener.accept().unwrap();
    conn.set_nonblocking(true).unwrap();
    let real = sse::epoch_now(sh);
    let (seen_tx, seen_rx) = channel();
    let seen_tx = Mutex::new(seen_tx);
    let probe: EpochNow = Box::new(move || {
        let now = real();
        let _ = seen_tx.lock().unwrap().send(now);
        now
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let hold = h.run.as_ref().unwrap().handle().hold();
    rt.spawn(async move {
        let conn = tokio::net::TcpStream::from_std(conn).unwrap();
        sse::serve(conn, sub, hold, stop_rx, probe, 0).await;
    });
    (client, stop_tx, seen_rx)
}

#[test]
fn rdx14_a_queued_event_waits_for_the_fresh_state() {
    let (h, ids) = host();
    let sh = shared(&h);
    let url = url(&h);
    let rt = Hosted::rt();
    // the request first: its publish is behind every build below, so no build after the
    // streams open has anything new for them
    let p = request(&sh, &ids.secret, "redact");
    // two open pages of alpha at its current revision with an older label: each stream's
    // queue holds a label event
    let old = sh.site.refresh(&sh, &ScopeRef::Canon).unwrap();
    assert!(build_holds(&old, SECRET));
    let rev = old.revs["alpha.html"].clone();
    let open = || {
        sh.site
            .subscribe(&sh, &ScopeRef::Canon, "alpha.html", &rev, "an-older-label")
            .expect("the stream opens")
    };
    let (a, b) = (open(), open());

    // the removal is authorized and erased; the writer has not published its state yet
    let (landed_tx, landed_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let writer = {
        let sh = Arc::clone(&sh);
        std::thread::spawn(move || {
            on_writer(&sh, move |r| {
                approve(r, &p);
                landed_tx.send(()).unwrap();
                go_rx.recv().unwrap();
            })
        })
    };
    landed_rx.recv().unwrap();
    assert!(
        sh.state.load().ensure_current_epoch().is_err(),
        "the published state predates the removal"
    );

    // stream a meets its queued label while the published state is stale, then stops: it
    // wrote nothing after its head (a reload now would load the Busy answer)
    let (mut ca, stop_a, seen_a) = serve(&h, &rt, a);
    let mut got = Vec::new();
    let retry = stream_head(&mut ca, &mut got);
    seen_a
        .recv_timeout(T)
        .expect("stream a considered its label");
    let _ = stop_a.send(true);
    let end = next_frame(&mut ca, &mut got, retry);
    assert_eq!(
        String::from_utf8_lossy(&got[retry..end]),
        "",
        "stream a: no event while the published state is stale"
    );

    // stream b meets its queued label while the published state is stale and stays open
    let (mut cb, stop_b, seen_b) = serve(&h, &rt, b);
    let mut got = Vec::new();
    let retry = stream_head(&mut cb, &mut got);
    seen_b
        .recv_timeout(T)
        .expect("stream b considered its label");

    // the writer publishes the fresh state: the stream gets the existing refresh event
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    let end = next_frame(&mut cb, &mut got, retry);
    assert_eq!(
        String::from_utf8_lossy(&got[retry..end]),
        String::from_utf8_lossy(&sse::frame(&LiveEvent::Reload)),
        "stream b: the refresh event after the fresh state was published"
    );
    assert_eq!(
        seen_b.recv_timeout(T).unwrap(),
        Some(epoch(&sh)),
        "the refresh event went out while the published state was current"
    );
    // the page the reload loads is the fresh one
    let page = http::get(&format!("{url}alpha.html"));
    assert_eq!(page.status, 200, "{}", page.head);
    assert!(!text(&page).contains(SECRET) && text(&page).contains(STUB));
    let _ = stop_b.send(true);
}

#[test]
fn rdx14_a_subscribe_across_removal_gets_the_refresh() {
    let (h, ids) = host();
    let sh = shared(&h);
    let tag = ScopeRef::Tag("v1.0.0".into());
    // a site with no worker: its publish count never moves, as the worker's does not without
    // another publish, so nothing refreshes the scope after the step below
    let site = LiveSite::new();
    let p = request(&sh, &ids.secret, "redact");
    let before = epoch(&sh);
    // an old page of the tag opens its stream: the scope has no build, and the subscribe's
    // build reads the state before the removal and pauses at the publication check
    let (at, go) = hold(&site, tag.clone());
    let opening = {
        let (sh, site, tag) = (Arc::clone(&sh), Arc::clone(&site), tag.clone());
        std::thread::spawn(move || {
            site.subscribe(&sh, &tag, "alpha.html", "an-older-rev", "an-older-label")
        })
    };
    at.recv().unwrap();
    // the removal is authorized and the fresh state published
    on_writer(&sh, move |r| approve(r, &p));
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    assert!(
        sh.state.load().ensure_current_epoch().is_ok(),
        "the fresh state is published"
    );
    // the worker's step for that publish runs before the stream is registered
    assert_eq!(site.step(&sh), 0, "no open stream: the step builds nothing");
    site.set_before_publish(None);
    go.send(()).unwrap();
    let mut sub = opening.join().unwrap().expect("the stream opens");
    // the open page gets the existing refresh event, carrying nothing
    assert_eq!(
        sub.rx.try_recv().ok(),
        Some(LiveEvent::Reload),
        "the page opened before the removal reloads"
    );
    let b = site.refresh(&sh, &tag).unwrap();
    assert_eq!(
        b.epoch,
        epoch(&sh),
        "the scope's build is of the current epoch"
    );
    assert!(!build_holds(&b, SECRET));
}
