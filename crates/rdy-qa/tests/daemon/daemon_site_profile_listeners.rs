//! spv7 (D-DK03K, D-CWC91, D-N5NSN, D-FV6G9, D-1RXEA): the profile-only listeners a daemon owns.
//! The listener a request arrives on sets the mode, never a path, a Host header or a link. One
//! key has one listener set, reused while the daemon lives; a failed start leaves nothing
//! behind; the registry holds at most 32 keys and the HTTP limits are shared by every port. A
//! profile canon no longer holds serves no page, and its slot stays until the daemon stops. The
//! foreground entry point validates its profile before any HTTP port opens, and every failure
//! stops the daemon it created.
use rdy_daemon::site::addr::profile_candidates;
use rdy_daemon::site::guard::SiteTarget;
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::site::live::{LiveEvent, MAX_STREAMS, NoStream};
use rdy_daemon::site::profiles::{TestEvent, set_test_hooks};
use rdy_daemon::site::server::{LAN_BIND_FIX, MAX_CONNECTIONS, NO_LAN_ADDRESS_FIX};
use rdy_daemon::site::{LanOverride, SiteInfo, ensure_profile_site};
use rdy_daemon::{
    Attempt, ExitReason, Hello, PROTO_VERSION, ProfileSiteReady, Proto, RunDir, ServeError,
    ServeOptions, Shared, connect, request_stop, serve_with_profile,
};
use rdy_engine::engine::{SITE_LAN_FIX, write_typed};
use rdy_engine::publication::profile_at;
use rdy_engine::{E_NOT_FOUND, E_SITE, Rdy, RdyError, Scope};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, write};
use rdy_qa::golden::tiny;
use rdy_qa::http::{self, Sse};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

/// The deadline of every wait for an event.
const T: Duration = Duration::from_secs(10);
/// The caveat every profile-only site carries (its title in place of `{}`).
const CAVEAT_FIX: &str = "give the full site's link from rdy site when the person needs everything";
/// The fix of the removal warning.
const REMOVED_FIX: &str = "land the profile again, or run rdy serve --stop to end the site";

// --- the world ----------------------------------------------------------------------------------

/// The project of every pin. Canon holds a folder `manual` (doc `intro`, page
/// `manual/intro.html`, whose prose reads `INTRO-ONLY-1`), a top-level `secret` doc (prose
/// `SECRET-WORDS`) and an `admin` doc that holds the profiles: `guide` (org, `Guide book`, the
/// folder), `other` (org, `Other book`, the secret doc) and `k01`..`kNN` (org, the folder). Tag
/// `v1.0.0` is canon. View `work` edits intro to `INTRO-VIEW`; proposal `prop` edits it to
/// `INTRO-PROP`.
struct World {
    h: Hosted,
    /// The source site's root URL.
    url: String,
    ids: BTreeMap<String, String>,
    prop: String,
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

fn profile_op(key: &str, title: &str, admin: &str, select: &[&str]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": admin,
        "fields": {"key": key, "title": title, "description": format!("About {title}."),
            "audience": "org", "select": select}})
}

fn key_name(i: usize) -> String {
    format!("k{i:02}")
}

/// The project, closed, in its directory, with `extra` profiles `k01`..; `config` is written
/// to `.rdy/local.yaml` when given.
fn project(
    extra: usize,
    config: Option<&str>,
) -> (tempfile::TempDir, BTreeMap<String, String>, String) {
    let (dir, mut r) = tiny();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONLY-1"},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "SECRET-WORDS"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let admin = ids["$admin"].clone();
    let manual = ids["$manual"].clone();
    let mut ops = vec![
        profile_op("guide", "Guide book", &admin, &[&manual]),
        profile_op("other", "Other book", &admin, &[&ids["$secret"]]),
    ];
    for i in 1..=extra {
        let k = key_name(i);
        ops.push(profile_op(&k, &format!("Book {k}"), &admin, &[&manual]));
    }
    ids.extend(put(&mut r, "admin", Value::Array(ops)));
    land(&mut r, "admin").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let ip = ids["$ip"].clone();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": ip, "body": "INTRO-VIEW"}]),
    );
    put(
        &mut r,
        "prop",
        json!([{"op": "update", "id": ip, "body": "INTRO-PROP"}]),
    );
    let prop = r
        .propose(&rdy_qa::fixtures::writer(), "prop", "Prop", None)
        .unwrap()
        .r#ref;
    drop(r);
    if let Some(text) = config {
        std::fs::write(dir.path().join(".rdy/local.yaml"), text).unwrap();
    }
    (dir, ids, prop)
}

fn world_with(extra: usize, config: Option<&str>, lan: Option<LanOverride>) -> World {
    let (dir, ids, prop) = project(extra, config);
    let h = Hosted::start_with(dir, lan);
    let url = shared(&h)
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the source site listens");
    World { h, url, ids, prop }
}

fn world() -> World {
    world_with(0, None, None)
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

fn root_of(dir: &Path) -> PathBuf {
    RunDir::of_project(dir).root()
}

/// Runs `f` on the daemon's writer.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

/// Deletes profile block `id` and lands it.
fn remove_profile(sh: &Arc<Shared>, id: &str) {
    let id = id.to_string();
    on_writer(sh, move |r| {
        put(r, "rm", json!([{"op": "delete", "id": id}]));
        land(r, "rm").unwrap();
    });
}

/// Creates a new profile `key` (a new id) selecting the manual folder, and lands it.
fn restore_profile(sh: &Arc<Shared>, w: &World, key: &str, title: &str) -> String {
    let op = profile_op(key, title, &w.ids["$admin"], &[&w.ids["$manual"]]);
    let key = key.to_string();
    let v = on_writer(sh, move |r| {
        let ids = put(r, "restore", json!([op]));
        land(r, "restore").unwrap();
        ids[&format!("${key}")].clone()
    });
    v.as_str().unwrap().to_string()
}

/// Lands an edit that touches no profile.
fn unrelated_landing(sh: &Arc<Shared>, w: &World) {
    let secret = w.ids["$secret"].clone();
    on_writer(sh, move |r| {
        put(
            r,
            "noise",
            json!([{"op": "create", "type": "prose", "parent": secret, "body": "NOISE"}]),
        );
        land(r, "noise").unwrap();
    });
}

// --- hooks ----------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Bind(SocketAddr),
    Log(String),
    Stage(String),
}

/// What the daemon of `dir` reports through the test hooks from now on: every event, kept in
/// order, and a channel of the same events. Dropping the guard removes the hooks.
struct Recorder {
    root: PathBuf,
    all: Arc<Mutex<Vec<Ev>>>,
    rx: Receiver<Ev>,
}

impl Recorder {
    fn on(dir: &Path) -> Recorder {
        Recorder::probing(dir, &[])
    }

    /// [`Recorder::on`]; at each stop stage it also records whether every port of `ports` has
    /// stopped accepting by then, as `<stage>: ports closed <bool>`.
    fn probing(dir: &Path, ports: &[u16]) -> Recorder {
        let root = root_of(dir);
        let ports = ports.to_vec();
        let all = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let (keep, tx) = (Arc::clone(&all), Mutex::new(tx));
        set_test_hooks(
            &root,
            Some(Box::new(move |e: TestEvent<'_>| {
                let ev = match e {
                    TestEvent::Bind(a) => Ev::Bind(a),
                    TestEvent::Log(l) => Ev::Log(l.to_string()),
                    TestEvent::Stage(s) => Ev::Stage(s.to_string()),
                };
                keep.lock().unwrap().push(ev.clone());
                if let Ev::Stage(stage) = &ev
                    && !ports.is_empty()
                {
                    let shut = ports.iter().all(|p| TcpStream::connect(sock(*p)).is_err());
                    keep.lock()
                        .unwrap()
                        .push(Ev::Stage(format!("{stage}: ports closed {shut}")));
                }
                let _ = tx.lock().unwrap().send(ev);
            })),
        );
        Recorder { root, all, rx }
    }

    fn events(&self) -> Vec<Ev> {
        self.all.lock().unwrap().clone()
    }

    fn binds(&self) -> Vec<SocketAddr> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Bind(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    fn logs(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Log(l) => Some(l),
                _ => None,
            })
            .collect()
    }

    fn stages(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Ev::Stage(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Waits for stop or start stage `stage`.
    fn await_stage(&self, stage: &str) {
        loop {
            match self.rx.recv_timeout(T) {
                Ok(Ev::Stage(s)) if s == stage => return,
                Ok(_) => {}
                Err(e) => panic!("no stage {stage:?} ({e:?}): {:?}", self.events()),
            }
        }
    }

    /// Waits for log line `line`.
    fn await_log(&self, line: &str) {
        loop {
            match self.rx.recv_timeout(T) {
                Ok(Ev::Log(l)) if l == line => return,
                Ok(_) => {}
                Err(e) => panic!("no log line {line:?} ({e:?}): {:?}", self.events()),
            }
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        set_test_hooks(&self.root, None);
    }
}

// --- HTTP -----------------------------------------------------------------------------------------

fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap()
}

fn sock(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn url_of(info: &SiteInfo) -> String {
    info.url.clone().expect("the profile site listens")
}

/// `GET /<path>` on `port`, with `Host: 127.0.0.1:<port>`.
fn get(port: u16, path: &str) -> http::Response {
    get_as(port, path, &format!("127.0.0.1:{port}"))
}

fn get_as(port: u16, path: &str, host: &str) -> http::Response {
    http::raw(
        sock(port),
        &format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    )
}

fn head(port: u16, path: &str) -> http::Response {
    http::raw(
        sock(port),
        &format!("HEAD /{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    )
}

fn text(r: &http::Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

/// Asserts `r` is exactly the base plain answer of `f`.
#[track_caller]
fn assert_refusal(r: &http::Response, f: Refusal, what: &str) {
    let want = refusal(f);
    assert_eq!(r.status, want.status, "{what}: {}", r.head);
    assert_eq!(text(r), String::from_utf8(want.body).unwrap(), "{what}");
    assert!(
        r.header("content-type")
            .is_some_and(|t| t.starts_with("text/plain")),
        "{what}: {}",
        r.head
    );
}

/// `GET /_rdy/events?<query>` on `port`, which must be refused: a stream that opens (200) never
/// ends, so it fails at once rather than waiting.
#[track_caller]
fn refused_events(port: u16, query: &str) -> http::Response {
    use std::io::{Read, Write};
    let mut s = TcpStream::connect(sock(port)).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "GET /_rdy/events?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut got = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut checked = false;
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) => panic!(
                "{query}: no refusal ({e}): {}",
                String::from_utf8_lossy(&got)
            ),
        }
        if !checked && let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            checked = true;
            let head = String::from_utf8_lossy(&got[..at]).into_owned();
            assert!(
                !head.starts_with("HTTP/1.1 200"),
                "{query}: a stream opened: {head}"
            );
        }
    }
    let at = got
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(got.len());
    let head = String::from_utf8_lossy(&got[..at]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let body = got.get(at + 4..).unwrap_or_default().to_vec();
    http::Response { status, head, body }
}

/// The page at `path` of the site on `port`, served 200.
#[track_caller]
fn page(port: u16, path: &str) -> String {
    let r = get(port, path);
    assert_eq!(r.status, 200, "{path}: {}", r.head);
    text(&r)
}

/// The events query (after `?`) of a served page.
fn events_query(html: &str) -> String {
    let url = http::events_url("http://x", html);
    url.split_once('?').unwrap().1.to_string()
}

/// No source content or hidden-scope hint in `body`.
#[track_caller]
fn selected_only(body: &str, what: &str) {
    for needle in [
        "SECRET-WORDS",
        "Secret page",
        "Other book",
        "INTRO-VIEW",
        "INTRO-PROP",
        "~versions",
        "~view",
        "~proposal",
        "~tag",
        "~profile",
    ] {
        assert!(!body.contains(needle), "{what} holds {needle:?}");
    }
}

#[track_caller]
fn assert_error(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

fn caveat_line(title: &str) -> String {
    format!(
        "site: warning: this site shows only the profile \"{title}\"; anyone with the project still reads everything; fix: {CAVEAT_FIX}"
    )
}

fn removed_line(key: &str) -> String {
    format!(
        "site: warning: canon no longer holds the profile \"{key}\"; its profile-only site serves no page; fix: {REMOVED_FIX}"
    )
}

fn stopping(e: &RdyError) {
    assert_error(
        e,
        E_SITE,
        "the daemon is stopping",
        "ask for the site's link again after the daemon stops",
    );
}

// --- the pins -------------------------------------------------------------------------------------

#[test]
fn spv7_listener_mode_wins_over_host_path_and_events() {
    let w = world();
    let sh = shared(&w.h);
    let source = port_of(&w.url);
    let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
    let only = port_of(&url_of(&info));
    assert_ne!(only, source, "a port of its own");

    // the selected pages and assets, at the root, on every valid Host form of the port
    let intro = page(only, "manual/intro.html");
    assert!(intro.contains("INTRO-ONLY-1"), "{intro}");
    selected_only(&intro, "intro");
    for host in [
        format!("localhost:{only}"),
        format!("LOCALHOST:{only}"),
        format!("[::1]:{only}"),
    ] {
        let r = get_as(only, "manual/intro.html", &host);
        assert_eq!(r.status, 200, "{host}: {}", r.head);
        assert_eq!(text(&r), intro, "{host}");
    }
    let r = http::raw(
        sock(only),
        &format!(
            "GET /manual/intro.html HTTP/1.1\r\nHost: localhost:{only}\r\nOrigin: http://localhost:{only}\r\n\r\n"
        ),
    );
    assert_eq!(r.status, 200, "own origin: {}", r.head);
    let home = page(only, "");
    selected_only(&home, "home");
    for asset in ["assets/site.css", "assets/app.js", "_rdy/live.js"] {
        let r = get(only, asset);
        assert_eq!(r.status, 200, "{asset}: {}", r.head);
        selected_only(&text(&r), asset);
    }
    // ordinary page queries are ignored
    for q in [
        "?scope=view:work",
        "?profile=other",
        "?menu=1&scope=versions",
        "?scope=proposal:P-zz",
    ] {
        assert_eq!(
            page(only, &format!("manual/intro.html{q}")),
            intro,
            "query {q}"
        );
    }
    // the source page of another doc is not selected
    assert_refusal(&get(only, "secret.html"), Refusal::NotFound, "secret.html");

    // a valid source Host cannot change the mode, on either port
    for (port, host) in [
        (only, format!("127.0.0.1:{source}")),
        (only, format!("localhost:{source}")),
        (source, format!("127.0.0.1:{only}")),
    ] {
        assert_refusal(
            &get_as(port, "manual/intro.html", &host),
            Refusal::Host,
            &host,
        );
    }

    // every decoded `/~` family is the plain 404 on the profile-only port
    let prop = &w.prop;
    let families = [
        "~profile/guide/manual/intro.html".to_string(),
        "~profile/other/secret.html".to_string(),
        "~profile/guide/".to_string(),
        "~view/work/manual/intro.html".to_string(),
        "~view/work/~profile/guide/manual/intro.html".to_string(),
        format!("~proposal/{prop}/manual/intro.html"),
        format!("~proposal/{prop}/~profile/guide/"),
        "~tag/v1.0.0/".to_string(),
        "~tag/v1.0.0/~profile/guide/manual/intro.html".to_string(),
        "~versions/".to_string(),
        "~versions/index.html".to_string(),
        "~versions/?scope=view:work&profile=guide".to_string(),
        "~elsewhere/x.html".to_string(),
        "~".to_string(),
        "%7Eview/work/manual/intro.html".to_string(),
        "%7eprofile/other/secret.html".to_string(),
        "%7Eversions/".to_string(),
        "manual/~profile/other/secret.html".to_string(),
        "_rdy/other".to_string(),
    ];
    for path in &families {
        assert_refusal(&get(only, path), Refusal::NotFound, path);
    }
    // the same profile routes on the full listener still serve
    let full = get(source, "~profile/guide/manual/intro.html");
    assert_eq!(full.status, 200, "{}", full.head);
    let other = get(source, "~profile/other/secret.html");
    assert_eq!(other.status, 200, "{}", other.head);
    assert!(text(&other).contains("SECRET-WORDS"));

    // events: the page's own query opens its stream
    let q = events_query(&intro);
    assert!(q.starts_with("scope=canon&page="), "{q}");
    let (rev_label, page_q) = {
        let rest = q.strip_prefix("scope=canon&").unwrap();
        let at = rest.find("&rev=").unwrap();
        (rest[at..].to_string(), rest[..at].to_string())
    };
    let ev = |query: &str| format!("http://127.0.0.1:{only}/_rdy/events?{query}");
    drop(Sse::open(&ev(&q)));
    // absent profile or its own folded key, with the first of each value
    for query in [
        format!("scope=canon&{page_q}{rev_label}&profile=guide"),
        format!("scope=canon&{page_q}{rev_label}&profile=GUIDE"),
        format!("scope=canon&scope=view:work&{page_q}{rev_label}"),
        format!("scope=canon&{page_q}{rev_label}&profile=guide&profile=other"),
        format!("scope=canon&{page_q}{rev_label}&unknown=1&x=%ZZ"),
    ] {
        drop(Sse::open(&ev(&query)));
    }
    // a recognized, well-formed context that selects another target: the plain 404
    for query in [
        format!("scope=canon&{page_q}{rev_label}&profile=other"),
        format!("scope=canon&{page_q}{rev_label}&profile=implicit:org"),
        format!("scope=canon&{page_q}{rev_label}&menu=1"),
        format!("scope=canon&{page_q}{rev_label}&profile=guide&menu=1"),
        format!("scope=view:work&{page_q}{rev_label}"),
        format!("scope=view:work&{page_q}{rev_label}&profile=guide"),
        format!("scope=proposal:{prop}&{page_q}{rev_label}"),
        format!("scope=tag:v1.0.0&{page_q}{rev_label}"),
        format!("scope=versions&{page_q}{rev_label}"),
        format!("scope=versions&{page_q}{rev_label}&profile=guide&menu=1"),
        format!("scope=view:work&scope=canon&{page_q}{rev_label}"),
        format!("scope=canon&{page_q}{rev_label}&profile=other&profile=guide"),
    ] {
        let r = refused_events(only, &query);
        assert_refusal(&r, Refusal::NotFound, &query);
    }
    // a malformed value stays 400
    for query in [
        format!("scope=canon&{page_q}{rev_label}&menu=2"),
        format!("scope=canon&{page_q}{rev_label}&profile=bad%20key"),
        format!("scope=canon&{page_q}{rev_label}&profile=%ZZ"),
        format!("scope=view:a:b&{page_q}{rev_label}"),
        format!("scope=%ZZ&{page_q}{rev_label}"),
        format!("scope=canon&{page_q}&rev=XYZ&label=00112233aabbccdd"),
        format!("scope=canon&{page_q}"),
        format!("{page_q}{rev_label}"),
        format!("scope=canon&page=../x{rev_label}"),
        // a malformed page, revision or label is 400 before the bound target is compared: a
        // valid context of another target does not turn it into the not found
        "scope=view:work&page=../secret&rev=XYZ&label=0011223344556677".to_string(),
        format!("scope=view:work&page=../secret{rev_label}"),
        format!("scope=view:work&{page_q}&rev=XYZ&label=0011223344556677"),
        format!("scope=view:work&{page_q}&rev=0011223344556677&label=XYZ"),
        format!("scope=canon&page=a//b{rev_label}&profile=other"),
        format!("scope=canon&{page_q}&rev=00112233AABBCCDD&label=0011223344556677&profile=other"),
        format!("scope=canon&page=%2E%2E/x{rev_label}&menu=1"),
        format!("scope=tag:v1.0.0&page=x%5Cy{rev_label}"),
        format!("scope=proposal:{prop}&{page_q}&rev=XYZ&label=0011223344556677"),
        format!("scope=versions&page=../x{rev_label}&profile=guide&menu=1"),
        format!("scope=versions&page=../x{rev_label}"),
        format!("scope=view:work&{page_q}"),
        format!("scope=view:work&page=%ZZ{rev_label}"),
    ] {
        let r = refused_events(only, &query);
        assert_refusal(&r, Refusal::Malformed, &query);
    }
    // HEAD of the event route keeps the method refusal
    let r = http::raw(
        sock(only),
        &format!("HEAD /_rdy/events?{q} HTTP/1.1\r\nHost: 127.0.0.1:{only}\r\n\r\n"),
    );
    assert_eq!(r.status, 405, "{}", r.head);
}

#[test]
fn spv7_same_key_concurrent_starts_return_one_port() {
    let w = world();
    let sh = shared(&w.h);
    let rec = Recorder::on(w.h.dir.path());
    let source = port_of(&w.url);
    const N: usize = 8;
    let barrier = Arc::new(Barrier::new(N));
    let starts: Vec<_> = (0..N)
        .map(|i| {
            let (sh, barrier) = (Arc::clone(&sh), Arc::clone(&barrier));
            // the key, its folded spelling and its id name one profile
            let name = match i % 3 {
                0 => "guide".to_string(),
                1 => "GUIDE".to_string(),
                _ => w.ids["$guide"].clone(),
            };
            std::thread::spawn(move || {
                barrier.wait();
                url_of(&ensure_profile_site(sh, &name).unwrap())
            })
        })
        .collect();
    let urls: Vec<String> = starts.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(urls.iter().all(|u| *u == urls[0]), "{urls:?}");
    assert_eq!(sh.profile_sites.keys(), vec!["guide".to_string()]);
    let guide = port_of(&urls[0]);
    assert!(
        profile_candidates(&root_of(w.h.dir.path()), "guide").contains(&guide),
        "{guide}"
    );
    let v4: Vec<SocketAddr> = rec
        .binds()
        .into_iter()
        .filter(|a| *a == sock(guide))
        .collect();
    assert_eq!(v4, vec![sock(guide)], "one listener set: {:?}", rec.binds());
    assert!(rec.binds().iter().all(|a| a.port() != source));
    assert_eq!(
        rec.logs()
            .iter()
            .filter(|l| l.starts_with("site: profile guide: "))
            .count(),
        1,
        "{:?}",
        rec.logs()
    );
    // distinct keys stay distinct
    let other = url_of(&ensure_profile_site(Arc::clone(&sh), "other").unwrap());
    assert_ne!(other, urls[0]);
    assert_eq!(
        sh.profile_sites.keys(),
        vec!["guide".to_string(), "other".to_string()]
    );
    // a later start reuses the listener
    assert_eq!(
        url_of(&ensure_profile_site(Arc::clone(&sh), "guide").unwrap()),
        urls[0]
    );
    assert!(page(guide, "manual/intro.html").contains("INTRO-ONLY-1"));
}

/// The report of a failed retry: the original error, then one fresh bind probe per candidate
/// port, in the order given. The probes run after the failure, so they show what the ports look
/// like now; they are not the daemon's own bind errors.
fn retry_failure(
    e: &RdyError,
    ports: &[u16],
    mut probe: impl FnMut(SocketAddr) -> std::io::Result<()>,
) -> String {
    let mut lines = vec![
        format!("profile site retry failed: {}: {}", e.code, e.message),
        format!("fix: {}", e.fix),
        "post-failure IPv4 bind probes (candidate order):".to_string(),
    ];
    for &port in ports {
        let at = sock(port);
        let seen = match probe(at) {
            Ok(()) => "bound".to_string(),
            Err(error) => format!("{:?}", error.kind()),
        };
        lines.push(format!("{at}: {seen}"));
    }
    lines.join("\n")
}

#[test]
fn plb4_retry_failure_reports_each_candidate() {
    use std::io::{Error, ErrorKind};
    let e = RdyError::new("E_SITE", "retry message", "retry fix");
    let ports: Vec<u16> = (49152..=49167).collect();
    let mut seen = Vec::new();
    let report = retry_failure(&e, &ports, |at| {
        let n = seen.len();
        seen.push(at);
        match n % 4 {
            0 => Err(Error::from(ErrorKind::AddrInUse)),
            1 => Err(Error::from(ErrorKind::PermissionDenied)),
            2 => Err(Error::other("probe failed")),
            _ => Ok(()),
        }
    });
    let want_probed: Vec<SocketAddr> = ports.iter().map(|p| sock(*p)).collect();
    assert_eq!(seen, want_probed, "every address is probed once, in order");
    let names = ["AddrInUse", "PermissionDenied", "Other", "bound"];
    let mut want = vec![
        "profile site retry failed: E_SITE: retry message".to_string(),
        "fix: retry fix".to_string(),
        "post-failure IPv4 bind probes (candidate order):".to_string(),
    ];
    for (i, port) in ports.iter().enumerate() {
        want.push(format!("127.0.0.1:{port}: {}", names[i % 4]));
    }
    assert_eq!(want.len(), 19);
    assert_eq!(report, want.join("\n"));
    assert!(!report.ends_with('\n'));
}

#[test]
fn plb4_recorder_clears_on_unwind() {
    let dir = tempfile::tempdir().unwrap();
    let weak = Mutex::new(None);
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rec = Recorder::on(dir.path());
        *weak.lock().unwrap() = Some(Arc::downgrade(&rec.all));
        // Outside every mutex guard, so no mutex is poisoned.
        panic!("unwind with the recorder installed");
    }));
    assert!(out.is_err(), "the closure panicked");
    let weak = weak
        .into_inner()
        .unwrap()
        .expect("the recorder was installed");
    assert!(
        weak.upgrade().is_none(),
        "the hook and the recorder held the only strong references"
    );
}

#[test]
fn spv7_failed_open_allocates_no_profile_site() {
    let w = world();
    let sh = shared(&w.h);
    let rec = Recorder::on(w.h.dir.path());
    let root = root_of(w.h.dir.path());
    let st = sh.state.load_full();

    // unknown and malformed names: the engine's own error, before any socket
    let long = "a".repeat(200);
    for name in ["nope", "implicit:public", "P-", "bad key", long.as_str()] {
        let e = ensure_profile_site(Arc::clone(&sh), name).unwrap_err();
        let want = profile_at(&st, &Scope::Canon, name).unwrap_err();
        assert_eq!(
            (e.code, &e.message, &e.fix),
            (want.code, &want.message, &want.fix),
            "{name}"
        );
    }
    let e = ensure_profile_site(Arc::clone(&sh), "nope").unwrap_err();
    assert_error(
        &e,
        E_NOT_FOUND,
        "Publication profile \"nope\" was not found.",
        "Use a publication profile id or key from this scope.",
    );
    assert!(sh.profile_sites.keys().is_empty());
    assert!(rec.binds().is_empty(), "{:?}", rec.binds());

    // sixteen occupied candidates
    let held: Vec<TcpListener> = profile_candidates(&root, "other")
        .into_iter()
        .filter_map(|p| TcpListener::bind(sock(p)).ok())
        .collect();
    for p in profile_candidates(&root, "other") {
        assert!(
            TcpListener::bind(sock(p)).is_err(),
            "candidate {p} is occupied"
        );
    }
    let e = ensure_profile_site(Arc::clone(&sh), "other").unwrap_err();
    assert_error(
        &e,
        E_SITE,
        "could not bind a loopback port for profile \"other\" after 16 attempts",
        "free a port in 49152..65535, then ask for the profile site again",
    );
    assert!(sh.profile_sites.keys().is_empty());
    let tried = rec.binds();
    assert_eq!(tried.len(), 16, "{tried:?}");
    assert!(tried.iter().all(|a| a.ip().is_loopback()), "{tried:?}");
    assert!(
        !rec.logs().iter().any(|l| l.starts_with("site: profile ")),
        "{:?}",
        rec.logs()
    );
    // the test releases its own sockets and retries; it cannot promise the ports are free
    drop(held);
    let other = match ensure_profile_site(Arc::clone(&sh), "other") {
        Ok(info) => info,
        Err(e) => {
            // Probes run only after the failure; each socket closes at once.
            let report = retry_failure(&e, &profile_candidates(&root, "other"), |at| {
                TcpListener::bind(at).map(drop)
            });
            panic!("{report}");
        }
    };
    assert_eq!(sh.profile_sites.keys(), vec!["other".to_string()]);
    assert!(page(port_of(&url_of(&other)), "secret.html").contains("SECRET-WORDS"));

    // the source site stays usable
    let r = http::get(&format!("{}secret.html", w.url));
    assert_eq!(r.status, 200, "{}", r.head);

    // a stopping daemon opens nothing
    let mut h = w.h;
    let handle = h.run.as_ref().unwrap().handle();
    handle.stop();
    let before = rec.binds().len();
    stopping(&ensure_profile_site(Arc::clone(&sh), "guide").unwrap_err());
    assert_eq!(rec.binds().len(), before, "no socket");
    let run = h.run.take().unwrap();
    assert_eq!(run.wait().unwrap(), ExitReason::Stopped);
    stopping(&ensure_profile_site(Arc::clone(&sh), "guide").unwrap_err());
    assert!(sh.profile_sites.keys().is_empty(), "stop released the slot");
}

#[test]
fn spv7_registry_and_shared_http_limits_are_bounded() {
    let w = world_with(33, None, None);
    let sh = shared(&w.h);
    let rec = Recorder::on(w.h.dir.path());
    let source = port_of(&w.url);
    let mut ports = BTreeMap::new();
    for i in 1..=32 {
        let k = key_name(i);
        let info = ensure_profile_site(Arc::clone(&sh), &k).unwrap();
        ports.insert(k, port_of(&url_of(&info)));
    }
    let distinct: std::collections::BTreeSet<u16> = ports.values().copied().collect();
    assert_eq!(distinct.len(), 32);
    assert!(!distinct.contains(&source));
    let full = |name: &str| {
        let e = ensure_profile_site(Arc::clone(&sh), name).unwrap_err();
        assert_error(
            &e,
            E_SITE,
            "this daemon already serves 32 profile-only sites",
            "run rdy serve --stop, then start the profiles you need",
        );
    };
    let binds = rec.binds().len();
    full("k33");
    full("guide");
    assert_eq!(rec.binds().len(), binds, "a refused start binds nothing");
    // a missing key fails as missing, before the capacity check
    let e = ensure_profile_site(Arc::clone(&sh), "nope").unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    // a duplicate reuses its slot, at capacity too
    assert_eq!(
        port_of(&url_of(
            &ensure_profile_site(Arc::clone(&sh), "K01").unwrap()
        )),
        ports["k01"]
    );
    assert_eq!(sh.profile_sites.keys().len(), 32);

    // connections: one limit across every port
    let (k01, k02) = (ports["k01"], ports["k02"]);
    let half = MAX_CONNECTIONS / 2;
    let mut held: Vec<TcpStream> = (0..half)
        .map(|_| TcpStream::connect(sock(source)).unwrap())
        .collect();
    // the server accepts in order on each listener: an answer means every earlier connection on
    // it holds its permit
    assert_eq!(get(source, "index.html").status, 200);
    held.extend((0..half).map(|_| TcpStream::connect(sock(k01)).unwrap()));
    let busy = get(k01, "manual/intro.html");
    assert_refusal(&busy, Refusal::Busy, "the limit is shared, not per port");
    assert_refusal(
        &get(k02, "manual/intro.html"),
        Refusal::Busy,
        "another port",
    );
    assert_refusal(&get(source, "index.html"), Refusal::Busy, "the source port");
    drop(held);
    // the held connections close: the permits come back
    let mut tries = 0;
    let html = loop {
        let r = get(k01, "manual/intro.html");
        if r.status == 200 {
            break text(&r);
        }
        assert_refusal(&r, Refusal::Busy, "while the closes arrive");
        tries += 1;
        assert!(tries < 100_000, "the permits never came back");
        std::thread::yield_now();
    };
    let source_html = page(source, "index.html");

    // streams: one limit across every port
    let q = events_query(&html);
    let sq = events_query(&source_html);
    let mut streams = Vec::new();
    for i in 0..MAX_STREAMS {
        let url = if i % 2 == 0 {
            format!("http://127.0.0.1:{k01}/_rdy/events?{q}")
        } else {
            format!("http://127.0.0.1:{source}/_rdy/events?{sq}")
        };
        streams.push(Sse::open(&url));
    }
    let r = refused_events(k02, &q);
    assert_refusal(&r, Refusal::Busy, "the stream limit is shared");
    drop(streams);

    // a removed profile keeps its slot; its restoration reuses it
    remove_profile(&sh, &w.ids["$k01"]);
    rec.await_log(&removed_line("k01"));
    assert_eq!(sh.profile_sites.keys().len(), 32);
    full("k33");
    restore_profile(&sh, &w, "k01", "Book k01 again");
    assert_eq!(
        port_of(&url_of(
            &ensure_profile_site(Arc::clone(&sh), "k01").unwrap()
        )),
        k01,
        "the same URL"
    );
    full("k33");

    // only stop releases the slots
    let mut h = w.h;
    h.stop();
    assert!(sh.profile_sites.keys().is_empty());
}

#[test]
fn spv7_removal_evicts_pages_assets_and_streams() {
    let w = world();
    let sh = shared(&w.h);
    let rec = Recorder::on(w.h.dir.path());
    let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
    let only = port_of(&url_of(&info));
    let html = page(only, "manual/intro.html");
    for asset in ["assets/site.css", "_rdy/live.js"] {
        assert_eq!(get(only, asset).status, 200, "{asset}");
    }
    let q = events_query(&html);
    let events = format!("http://127.0.0.1:{only}/_rdy/events?{q}");
    let mut stream = Sse::open(&events);
    // cached: a second load builds nothing
    let builds = sh.site.builds();
    assert_eq!(page(only, "manual/intro.html"), html);
    assert_eq!(sh.site.builds(), builds, "served from the cache");

    remove_profile(&sh, &w.ids["$guide"]);
    assert_eq!(
        stream.next_event(T),
        Some(("reload".to_string(), "{}".to_string()))
    );
    assert_eq!(stream.next_event(T), None);
    assert!(stream.ended(), "the stream closed before its deadline");
    rec.await_log(&removed_line("guide"));

    // every guarded path is the plain 404: no stale page, asset, script or stream
    for path in [
        "",
        "index.html",
        "manual/",
        "manual/intro.html",
        "manual/intro.html?x=1",
        "assets/site.css",
        "assets/app.js",
        "_rdy/live.js",
    ] {
        assert_refusal(&get(only, path), Refusal::NotFound, path);
    }
    assert_refusal(&refused_events(only, &q), Refusal::NotFound, "events");
    assert_eq!(head(only, "manual/intro.html").status, 404);
    // the full listener's profile route is gone too
    assert_refusal(
        &get(port_of(&w.url), "~profile/guide/manual/intro.html"),
        Refusal::NotFound,
        "full listener",
    );
    // the slot stays, and its start is refused while canon lacks the key
    assert_eq!(sh.profile_sites.keys(), vec!["guide".to_string()]);
    assert_eq!(
        ensure_profile_site(Arc::clone(&sh), "guide")
            .unwrap_err()
            .code,
        E_NOT_FOUND
    );

    // one warning per disappearance: a later publish says nothing again
    unrelated_landing(&sh, &w);
    sh.site.step(&sh);
    assert_refusal(
        &get(only, "manual/intro.html"),
        Refusal::NotFound,
        "still gone",
    );
    let warned = rec
        .logs()
        .iter()
        .filter(|l| l.contains("no longer holds"))
        .count();
    assert_eq!(warned, 1, "{:?}", rec.logs());

    // restoration under a new id resumes the same URL
    let fresh = restore_profile(&sh, &w, "guide", "Guide book again");
    assert_ne!(fresh, w.ids["$guide"]);
    let again = page(only, "manual/intro.html");
    assert!(again.contains("INTRO-ONLY-1"), "{again}");
    assert_eq!(
        url_of(&ensure_profile_site(Arc::clone(&sh), &fresh).unwrap()),
        url_of(&info)
    );
    drop(Sse::open(&format!(
        "http://127.0.0.1:{only}/_rdy/events?{}",
        events_query(&again)
    )));

    // a second disappearance warns again
    remove_profile(&sh, &fresh);
    rec.await_log(&removed_line("guide"));
    assert_refusal(
        &get(only, "manual/intro.html"),
        Refusal::NotFound,
        "gone again",
    );
}

/// Holds every subscribe of a profile-only target after its refresh, before its stream
/// registers: each one says it arrived on the first channel, then waits for one release on the
/// second.
fn hold_profile_subscribes(sh: &Arc<Shared>) -> (Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (arrived, at) = channel();
    let (go, release) = channel::<()>();
    let gate = Mutex::new((arrived, release));
    sh.site
        .set_after_subscribe_refresh(Some(Box::new(move |t: &SiteTarget| {
            if !matches!(t, SiteTarget::ProfileOnly { .. }) {
                return;
            }
            let g = gate.lock().unwrap();
            g.0.send(()).unwrap();
            g.1.recv_timeout(T)
                .expect("the test releases the subscribe");
        })));
    (at, go)
}

#[test]
fn spv7_removal_between_subscribe_refresh_and_registration_opens_no_stream() {
    let w = world();
    let sh = shared(&w.h);
    let rec = Recorder::on(w.h.dir.path());
    let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
    let only = port_of(&url_of(&info));
    let target = SiteTarget::ProfileOnly {
        key: "guide".to_string(),
    };
    let built = sh.site.refresh(&sh, &target).unwrap();
    let path = "manual/intro.html".to_string();
    let (rev, label) = (built.live.revs[&path].clone(), built.live.label_rev.clone());
    let (at, go) = hold_profile_subscribes(&sh);

    // through the site's API: the subscribe's refresh read canon with the profile; the profile
    // is removed and its removal inspected before the stream registers
    let opening = {
        let (sh, target, path) = (Arc::clone(&sh), target.clone(), path.clone());
        let (rev, label) = (rev.clone(), label.clone());
        std::thread::spawn(move || {
            sh.site
                .subscribe(&sh, &target, &path, &rev, &label)
                .map(|_| ())
        })
    };
    at.recv_timeout(T)
        .expect("the subscribe reaches its barrier");
    remove_profile(&sh, &w.ids["$guide"]);
    rec.await_log(&removed_line("guide"));
    go.send(()).unwrap();
    assert_eq!(
        opening.join().unwrap(),
        Err(NoStream::NotFound),
        "no stream after the removal"
    );
    assert_eq!(sh.site.streams(), 0);
    sh.site.step(&sh);
    assert_eq!(sh.site.streams(), 0, "after another step");

    // through the listener: the event request passed its availability check before the
    // removal, and gets the plain not found, with no stream and no hold
    let fresh = restore_profile(&sh, &w, "guide", "Guide book again");
    let html = page(only, "manual/intro.html");
    let q = events_query(&html);
    let opening = std::thread::spawn(move || refused_events(only, &q));
    at.recv_timeout(T)
        .expect("the event request reaches its barrier");
    remove_profile(&sh, &fresh);
    rec.await_log(&removed_line("guide"));
    go.send(()).unwrap();
    let r = opening.join().expect("no stream opened");
    assert_refusal(&r, Refusal::NotFound, "events after the removal");
    assert_eq!(sh.site.streams(), 0);
    sh.site.step(&sh);
    assert_eq!(sh.site.streams(), 0, "after another step");
    sh.site.set_after_subscribe_refresh(None);
}

#[test]
fn spv7_a_removal_after_a_restore_both_unseen_by_the_worker_still_clears_the_stream() {
    let w = world();
    let sh = shared(&w.h);
    ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
    let root = root_of(w.h.dir.path());
    // the worker is held after it logs the first removal, outside every lock, so the restore
    // and the second removal below both publish before its next inspection
    let (arrived, at) = channel();
    let (go, release) = channel::<()>();
    let gate = Mutex::new(Some((arrived, release)));
    let removed = removed_line("guide");
    set_test_hooks(
        &root,
        Some(Box::new(move |e: TestEvent<'_>| {
            if let TestEvent::Log(l) = e
                && l == removed
                && let Some((arrived, release)) = gate.lock().unwrap().take()
            {
                arrived.send(()).unwrap();
                release
                    .recv_timeout(T)
                    .expect("the test releases the worker");
            }
        })),
    );
    remove_profile(&sh, &w.ids["$guide"]);
    at.recv_timeout(T).expect("the worker inspects the removal");

    // restored: a stream of the profile-only site opens in that state
    let fresh = restore_profile(&sh, &w, "guide", "Guide book again");
    let target = SiteTarget::ProfileOnly {
        key: "guide".to_string(),
    };
    let built = sh.site.refresh(&sh, &target).unwrap();
    let path = "manual/intro.html";
    let mut sub = sh
        .site
        .subscribe(
            &sh,
            &target,
            path,
            &built.live.revs[path],
            &built.live.label_rev,
        )
        .expect("canon holds the profile again");
    assert_eq!(sh.site.streams(), 1);

    // removed again before the worker saw the restore: its next inspection still clears it
    remove_profile(&sh, &fresh);
    go.send(()).unwrap();
    sh.site.step(&sh);
    assert_eq!(sub.rx.try_recv().ok(), Some(LiveEvent::Reload));
    assert_eq!(
        sub.rx.try_recv().err(),
        Some(tokio::sync::mpsc::error::TryRecvError::Disconnected),
        "the stream closed"
    );
    assert_eq!(sh.site.streams(), 0);
    set_test_hooks(&root, None);
}

#[test]
fn spv7_lan_uses_the_profile_port_and_warning() {
    // an injected private address (the stand-in 127.0.0.1)
    {
        let w = world_with(
            0,
            Some("serve:\n  site_lan: true\n"),
            Some(LanOverride::Addr(Ipv4Addr::LOCALHOST)),
        );
        let sh = shared(&w.h);
        let rec = Recorder::on(w.h.dir.path());
        let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        let url = url_of(&info);
        let port = port_of(&url);
        let lan = format!("http://127.0.0.1:{port}/");
        assert_eq!(
            info.lan_url.as_deref(),
            Some(lan.as_str()),
            "the profile port"
        );
        let warning = info.warning.clone().unwrap();
        assert_eq!(
            (warning.message.as_str(), warning.fix.as_str()),
            (
                format!("anyone on your network can read these docs at {lan}").as_str(),
                SITE_LAN_FIX
            ),
            "the original diagnostic"
        );
        assert_eq!(
            rec.logs(),
            vec![
                format!("site: profile guide: {url}"),
                format!(
                    "site: warning: anyone on your network can read the profile \"Guide book\" at {lan}; fix: set serve.site_lan to false to keep the site on this machine"
                ),
                caveat_line("Guide book"),
            ]
        );
        no_wildcard(&rec, &root_of(w.h.dir.path()), port);
        assert!(info.addrs.iter().all(|a| a.port() == port), "{info:?}");
    }
    // no private address found
    {
        let w = world_with(
            0,
            Some("serve:\n  site_lan: true\n"),
            Some(LanOverride::NoneFound),
        );
        let sh = shared(&w.h);
        let rec = Recorder::on(w.h.dir.path());
        let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        let url = url_of(&info);
        assert_eq!(info.lan_url, None);
        let warning = info.warning.clone().unwrap();
        let message = "serve.site_lan is on, but this machine has no private network address; the site stays on this machine";
        assert_eq!(
            (warning.message.as_str(), warning.fix.as_str()),
            (message, NO_LAN_ADDRESS_FIX)
        );
        assert_eq!(
            rec.logs(),
            vec![
                format!("site: profile guide: {url}"),
                format!("site: profile \"Guide book\": {message}; fix: {NO_LAN_ADDRESS_FIX}"),
                caveat_line("Guide book"),
            ]
        );
        no_wildcard(&rec, &root_of(w.h.dir.path()), port_of(&url));
        assert!(info.addrs.iter().all(|a| a.ip().is_loopback()), "{info:?}");
    }
    // a LAN address this machine cannot bind
    {
        let w = world_with(
            0,
            Some("serve:\n  site_lan: true\n  site_lan_address: 10.255.255.254\n"),
            None,
        );
        let sh = shared(&w.h);
        let rec = Recorder::on(w.h.dir.path());
        let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        let url = url_of(&info);
        let port = port_of(&url);
        assert_eq!(info.lan_url, None);
        let warning = info.warning.clone().unwrap();
        assert!(
            warning
                .message
                .starts_with("could not listen on 10.255.255.254 (")
                && warning
                    .message
                    .ends_with("); the site stays on this machine"),
            "{warning:?}"
        );
        assert_eq!(warning.fix, LAN_BIND_FIX);
        assert_eq!(
            rec.logs(),
            vec![
                format!("site: profile guide: {url}"),
                format!(
                    "site: profile \"Guide book\": {}; fix: {LAN_BIND_FIX}",
                    warning.message
                ),
                caveat_line("Guide book"),
            ]
        );
        assert!(
            rec.binds()
                .contains(&SocketAddr::from((Ipv4Addr::new(10, 255, 255, 254), port))),
            "the configured address, on the profile port: {:?}",
            rec.binds()
        );
        no_wildcard(&rec, &root_of(w.h.dir.path()), port);
    }
}

/// Every address the profile listener of `guide` tried is a loopback or a private one: on its
/// own port, or (an occupied earlier candidate) on `127.0.0.1` alone.
#[track_caller]
fn no_wildcard(rec: &Recorder, root: &Path, port: u16) {
    let binds = rec.binds();
    let candidates = profile_candidates(root, "guide");
    assert!(binds.contains(&sock(port)), "{binds:?}");
    for a in &binds {
        assert!(candidates.contains(&a.port()), "{binds:?}");
        if a.port() != port {
            assert_eq!(
                a.ip(),
                std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                "{binds:?}"
            );
        }
        assert!(!a.ip().is_unspecified(), "a wildcard bind: {binds:?}");
        let ok = match a.ip() {
            std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
            std::net::IpAddr::V6(ip) => ip.is_loopback(),
        };
        assert!(ok, "a public bind: {binds:?}");
    }
}

/// The hello of a foreground daemon of version `t`.
fn hello(dir: &Path) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: "t".into(),
        proto: Proto::Ctl,
        cwd: dir.into(),
        project: RunDir::of_project(dir).project_id().unwrap(),
    }
}

fn opts(dir: &Path) -> ServeOptions {
    ServeOptions {
        project: dir.to_path_buf(),
        version: "t".into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: Some("human:qa".into()),
        site_lan_override: None,
    }
}

/// Asserts no listener answers on `port` any more.
#[track_caller]
fn closed(port: u16) {
    assert!(
        TcpStream::connect(sock(port)).is_err(),
        "port {port} still accepts"
    );
}

/// Waits until the registry of `sh` is closed (its slots released), which the stop does right
/// before it joins the listeners.
fn stopping_now(sh: &Arc<Shared>) {
    let mut tries = 0u64;
    while !sh.profile_sites.keys().is_empty() {
        tries += 1;
        assert!(tries < 100_000_000, "the registry never closed");
        std::thread::yield_now();
    }
}

/// The index of `stage` in `stages`.
fn at(stages: &[String], stage: &str) -> usize {
    stages
        .iter()
        .position(|s| s == stage)
        .unwrap_or_else(|| panic!("no {stage}: {stages:?}"))
}

#[test]
fn spv7_stop_and_foreground_failure_release_every_listener() {
    // a stop joins every profile listener before the store closes
    {
        let mut w = world();
        let sh = shared(&w.h);
        let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        let other = ensure_profile_site(Arc::clone(&sh), "other").unwrap();
        let (guide, other) = (port_of(&url_of(&info)), port_of(&url_of(&other)));
        let html = page(guide, "manual/intro.html");
        let mut stream = Sse::open(&format!(
            "http://127.0.0.1:{guide}/_rdy/events?{}",
            events_query(&html)
        ));
        let rec = Recorder::probing(w.h.dir.path(), &[guide, other]);
        assert_eq!(w.h.stop(), ExitReason::Stopped);
        let stages = rec.stages();
        assert!(
            at(&stages, "profiles-joined") < at(&stages, "store-closed"),
            "{stages:?}"
        );
        // joined: the listeners are closed before the runtime goes and the store closes
        assert!(
            at(&stages, "profiles-joined: ports closed true") < at(&stages, "store-closed"),
            "{stages:?}"
        );
        assert_eq!(stream.next_event(T), None);
        assert!(stream.ended());
        closed(guide);
        closed(other);
        assert!(sh.profile_sites.keys().is_empty());
        stopping(&ensure_profile_site(Arc::clone(&sh), "guide").unwrap_err());
    }
    // the join outlasts a listener held at its end: each listener's task is held after it
    // ends, and its completion is recorded only once the test releases it; the stop's join is
    // reported only after both completions
    {
        let mut w = world();
        let sh = shared(&w.h);
        ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        ensure_profile_site(Arc::clone(&sh), "other").unwrap();
        let root = root_of(w.h.dir.path());
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let keep = Arc::clone(&order);
        let (arrived, at_end) = channel();
        let (release, held) = channel::<()>();
        let (arrived, held) = (Mutex::new(arrived), Mutex::new(held));
        set_test_hooks(
            &root,
            Some(Box::new(move |e: TestEvent<'_>| {
                let TestEvent::Stage(stage) = e else { return };
                match stage {
                    "profile-listener-ended" => {
                        keep.lock().unwrap().push("listener held".to_string());
                        arrived.lock().unwrap().send(()).unwrap();
                        held.lock()
                            .unwrap()
                            .recv_timeout(T)
                            .expect("the test releases the listener");
                        // the held work is done: the task completes as the hook returns
                        keep.lock().unwrap().push("listener done".to_string());
                    }
                    "profiles-joined" => {
                        let mut o = keep.lock().unwrap();
                        let done = o.iter().filter(|s| *s == "listener done").count();
                        o.push(format!("joined after {done} done"));
                        o.push(stage.to_string());
                    }
                    _ => keep.lock().unwrap().push(stage.to_string()),
                }
            })),
        );
        let run = w.h.run.take().unwrap();
        run.handle().stop();
        let stopping = std::thread::spawn(move || run.wait());
        // both listeners have ended and are held; the registry is closed (the step right
        // before the join): the join has nothing to report while they are held
        for _ in 0..2 {
            at_end.recv_timeout(T).expect("a listener ends");
        }
        stopping_now(&sh);
        assert!(
            !order.lock().unwrap().iter().any(|s| s == "profiles-joined"),
            "joined while both listeners are held: {:?}",
            order.lock().unwrap()
        );
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert_eq!(stopping.join().unwrap().unwrap(), ExitReason::Stopped);
        set_test_hooks(&root, None);
        let order = order.lock().unwrap().clone();
        assert!(
            order.contains(&"joined after 2 done".to_string()),
            "{order:?}"
        );
        assert!(
            at(&order, "profiles-joined") < at(&order, "store-closed"),
            "{order:?}"
        );
    }
    // a lost project joins them before the store is abandoned
    {
        let mut w = world();
        let sh = shared(&w.h);
        let info = ensure_profile_site(Arc::clone(&sh), "guide").unwrap();
        let guide = port_of(&url_of(&info));
        let rec = Recorder::probing(w.h.dir.path(), &[guide]);
        let dir = w.h.dir.path().to_path_buf();
        std::fs::rename(dir.join(".rdy"), dir.join(".rdy-old")).unwrap();
        std::fs::create_dir(dir.join(".rdy")).unwrap();
        let run = w.h.run.take().unwrap();
        run.handle().tick();
        assert_eq!(run.wait().unwrap(), ExitReason::ProjectGone);
        let stages = rec.stages();
        assert!(
            at(&stages, "profiles-joined") < at(&stages, "store-abandoned"),
            "{stages:?}"
        );
        assert!(
            at(&stages, "profiles-joined: ports closed true") < at(&stages, "store-abandoned"),
            "{stages:?}"
        );
        assert!(!stages.iter().any(|s| s == "store-closed"), "{stages:?}");
        closed(guide);
        assert!(sh.profile_sites.keys().is_empty());
    }
    // an unknown initial key opens no HTTP port and logs its error
    {
        let (dir, _, _) = project(0, None);
        let rec = Recorder::on(dir.path());
        let called = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&called);
        let out = serve_with_profile(opts(dir.path()), "nope", move |_| {
            *flag.lock().unwrap() = true;
            Ok(())
        });
        let Err(ServeError::Open(e)) = out else {
            panic!("{out:?}")
        };
        assert_error(
            &e,
            E_NOT_FOUND,
            "Publication profile \"nope\" was not found.",
            "Use a publication profile id or key from this scope.",
        );
        assert!(!*called.lock().unwrap(), "no callback");
        assert!(rec.binds().is_empty(), "no HTTP bind: {:?}", rec.binds());
        assert!(
            rec.logs()
                .contains(&format!("could not open the store: {e}")),
            "{:?}",
            rec.logs()
        );
        drop(Rdy::open(dir.path()).expect("the store was released"));
    }
    // a callback error stops the daemon this call created and returns that error
    {
        let (dir, _, _) = project(0, None);
        let rec = Recorder::on(dir.path());
        let seen = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&seen);
        let caller = std::thread::current().id();
        let out = serve_with_profile(opts(dir.path()), "GUIDE", move |ready: ProfileSiteReady| {
            assert_eq!(std::thread::current().id(), caller, "on the caller thread");
            let port = port_of(&url_of(&ready.info));
            let body = page(port, "manual/intro.html");
            assert!(body.contains("INTRO-ONLY-1"));
            *keep.lock().unwrap() = Some((ready.key.clone(), ready.profile.title.clone(), port));
            Err(RdyError::new(
                "E_TEST",
                "the callback failed",
                "fix the callback",
            ))
        });
        let Err(ServeError::Open(e)) = out else {
            panic!("{out:?}")
        };
        assert_error(&e, "E_TEST", "the callback failed", "fix the callback");
        let (key, title, port) = seen.lock().unwrap().clone().expect("the callback ran");
        assert_eq!((key.as_str(), title.as_str()), ("guide", "Guide book"));
        closed(port);
        let source = rec
            .binds()
            .into_iter()
            .find(|a| a.port() != port)
            .expect("the source site bound");
        closed(source.port());
        assert!(
            rec.logs().contains(
                &"could not open the store: E_TEST: the callback failed (fix: fix the callback)"
                    .to_string()
            ),
            "{:?}",
            rec.logs()
        );
        let stages = rec.stages();
        assert!(
            at(&stages, "profiles-joined") < at(&stages, "store-closed"),
            "{stages:?}"
        );
        drop(Rdy::open(dir.path()).expect("the store was released"));
    }
    // a build error does too
    {
        let (dir, ids, _) = project(0, None);
        let mut r = Rdy::open(dir.path()).unwrap();
        let admin = ids["$admin"].clone();
        let ip = ids["$ip"].clone();
        // a selector that names a prose block: the metadata reads, the selection refuses
        put(
            &mut r,
            "badsel",
            json!([profile_op("badsel", "Bad book", &admin, &[&ip])]),
        );
        land(&mut r, "badsel").unwrap();
        drop(r);
        let rec = Recorder::on(dir.path());
        let out = serve_with_profile(opts(dir.path()), "badsel", |_| {
            panic!("no callback after a failed build")
        });
        let Err(ServeError::Open(e)) = out else {
            panic!("{out:?}")
        };
        assert_eq!(e.code, rdy_engine::E_BAD_INPUT, "{e:?}");
        assert!(
            rec.logs()
                .contains(&format!("could not open the store: {e}")),
            "{:?}",
            rec.logs()
        );
        for a in rec.binds() {
            closed(a.port());
        }
        drop(Rdy::open(dir.path()).expect("the store was released"));
    }
    // success: the callback runs once, then the call serves in the foreground until a stop
    {
        let (dir, _, _) = project(0, None);
        let path = dir.path().to_path_buf();
        let (tx, rx) = channel();
        let serving = std::thread::spawn(move || {
            serve_with_profile(opts(&path), "guide", move |ready| {
                tx.send(ready).unwrap();
                Ok(())
            })
        });
        let ready = rx.recv_timeout(T).expect("the callback ran");
        assert_eq!(ready.key, "guide");
        assert_eq!(ready.profile.title, "Guide book");
        assert!(ready.warnings.is_empty(), "{:?}", ready.warnings);
        let port = port_of(&url_of(&ready.info));
        assert!(page(port, "manual/intro.html").contains("INTRO-ONLY-1"));
        let rt = Hosted::rt();
        rt.block_on(request_stop(
            &RunDir::of_project(dir.path()),
            &hello(dir.path()),
            T,
        ))
        .unwrap()
        .expect("a daemon answered");
        assert_eq!(serving.join().unwrap().unwrap(), ExitReason::Stopped);
        closed(port);
        assert!(rx.try_recv().is_err(), "one callback");
    }
}

/// What a hello to the daemon of `dir` finds.
fn hello_attempt(rt: &tokio::runtime::Runtime, dir: &Path) -> Attempt {
    rt.block_on(connect(&RunDir::of_project(dir), &hello(dir)))
}

#[test]
fn spv7_foreground_readiness_waits_for_the_callback() {
    // a callback error: the warm-up is done, yet a client hears only `starting` while the
    // callback runs, and never `ready` after it fails
    {
        let (dir, _, _) = project(0, None);
        let rec = Recorder::on(dir.path());
        let rt = Hosted::rt();
        let mut waiting = None;
        let out = serve_with_profile(opts(dir.path()), "guide", |_ready| {
            rec.await_stage("warmed");
            match hello_attempt(&rt, dir.path()) {
                Attempt::Starting(p) => waiting = Some(p),
                other => panic!("ready before the callback returned: {other:?}"),
            }
            Err(RdyError::new(
                "E_TEST",
                "the callback failed",
                "fix the callback",
            ))
        });
        let Err(ServeError::Open(e)) = out else {
            panic!("{out:?}")
        };
        assert_eq!(e.code, "E_TEST");
        let pending = waiting.expect("the callback connected");
        let after = rt.block_on(pending.wait(std::time::Instant::now() + T, &mut |_| {}));
        assert!(after.is_err(), "no ready after the failed callback");
        let stages = rec.stages();
        assert!(!stages.iter().any(|s| s == "ready"), "{stages:?}");
        assert!(!rec.logs().iter().any(|l| l == "ready"), "{:?}", rec.logs());
        drop(Rdy::open(dir.path()).expect("the store was released"));
    }
    // a build error: no readiness either
    {
        let (dir, ids, _) = project(0, None);
        let mut r = Rdy::open(dir.path()).unwrap();
        put(
            &mut r,
            "badsel",
            json!([profile_op(
                "badsel",
                "Bad book",
                &ids["$admin"],
                &[&ids["$ip"]]
            )]),
        );
        land(&mut r, "badsel").unwrap();
        drop(r);
        let rec = Recorder::on(dir.path());
        let out = serve_with_profile(opts(dir.path()), "badsel", |_| {
            panic!("no callback after a failed build")
        });
        assert!(matches!(out, Err(ServeError::Open(_))), "{out:?}");
        let stages = rec.stages();
        assert!(!stages.iter().any(|s| s == "ready"), "{stages:?}");
        drop(Rdy::open(dir.path()).expect("the store was released"));
    }
    // success: `starting` while the callback runs, `ready` once it returned
    {
        let (dir, _, _) = project(0, None);
        let rec = Recorder::on(dir.path());
        let path = dir.path().to_path_buf();
        let (tx, rx) = channel();
        let (go, release) = channel::<()>();
        let serving = std::thread::spawn(move || {
            serve_with_profile(opts(&path), "guide", move |_ready| {
                tx.send(()).unwrap();
                release
                    .recv_timeout(T)
                    .expect("the test releases the callback");
                Ok(())
            })
        });
        rx.recv_timeout(T).expect("the callback ran");
        rec.await_stage("warmed");
        let rt = Hosted::rt();
        let Attempt::Starting(pending) = hello_attempt(&rt, dir.path()) else {
            panic!("ready while the callback runs")
        };
        assert!(
            !rec.stages().iter().any(|s| s == "ready"),
            "{:?}",
            rec.stages()
        );
        go.send(()).unwrap();
        let client = rt
            .block_on(pending.wait(std::time::Instant::now() + T, &mut |_| {}))
            .expect("ready once the callback returned");
        drop(client);
        rt.block_on(request_stop(
            &RunDir::of_project(dir.path()),
            &hello(dir.path()),
            T,
        ))
        .unwrap()
        .expect("a daemon answered");
        assert_eq!(serving.join().unwrap().unwrap(), ExitReason::Stopped);
    }
}
