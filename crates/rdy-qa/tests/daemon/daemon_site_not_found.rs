//! dgm11 (D-94BT2, D-E8P8J, D-FV6G9, D-X61DT, D-0VCQ5, D-CWC91, D-DK03K; system/site p-F63VB,
//! tb-QCTS0, n-REYZ9): the live site's own not-found page. After the existing Host, Origin,
//! method and redaction checks, a missing page or file of a scope or profile is that target's
//! themed Page variant, rooted at its prefix; a scope or profile that no longer resolves is
//! canon's Scope variant, which never names it; a path the guard does not route is the
//! listener's own Page variant. A profile-only listener never falls back to the source, and
//! when its profile is gone, or a chosen layout cannot be built, the answer is the plain text.
//! A generated 404 has no live behaviour: no script, stream, slot or revision.
use rdy_daemon::Shared;
use rdy_daemon::site::ensure_profile_site;
use rdy_daemon::site::guard::{Refusal, ScopeRef, SiteTarget, refusal};
use rdy_engine::Rdy;
use rdy_engine::engine::write_typed;
use rdy_engine::redaction::RedactInput;
use rdy_engine::site_scope::proposal_ref;
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, raw, raw_commit, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

/// The deadline of a read that must end.
const T: Duration = Duration::from_secs(10);
/// The generated page's marker on its html element.
pub(crate) const MARKER: &str = "<html lang=\"en\" data-rdy-kind=\"not-found\">";
/// The Page variant's sentence.
pub(crate) const PAGE_SENTENCE: &str =
    "There is no page at this address. It may have moved, or the link may be wrong.";
/// The Scope variant's sentence.
pub(crate) const SCOPE_SENTENCE: &str = "This link names a version, view, proposal or profile that is not on the site now. A view or proposal leaves the site when its work lands or stops.";
/// The plain fallback, byte for byte (74 bytes, no line end).
pub(crate) const PLAIN: &str =
    "Not found. Ask your agent for the site's link (rdy_site), or run rdy site.";
/// The new text of a page that could not be built.
const BROKEN: &str =
    "This page could not be built. The person who runs this site can find the reason in rdy's log.";

/// Which themed page a 404 is.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Variant {
    /// A missing page of the target rooted at the given prefix.
    Page,
    /// Canon's page for a scope or profile that is not on the site now.
    Scope,
}

fn text(r: &http::Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

/// Asserts `r` is the themed not-found page of `variant`, rooted at `root`, for the site named
/// `name`: status 404, the HTML type, the generated page's marker, its title and sentences, its
/// links all at `root` (or a fragment), and no live behaviour.
#[track_caller]
pub(crate) fn assert_themed(
    r: &http::Response,
    variant: Variant,
    root: &str,
    name: &str,
    what: &str,
) {
    assert_eq!(r.status, 404, "{what}: {}", r.head);
    assert_eq!(
        r.header("content-type"),
        Some("text/html; charset=utf-8"),
        "{what}: {}",
        r.head
    );
    assert_eq!(
        r.header("content-length"),
        Some(r.body.len().to_string().as_str()),
        "{what}"
    );
    let body = text(r);
    assert!(body.contains(MARKER), "{what}: the marker: {body}");
    assert!(
        body.contains(&format!("<title>Page not found · {name}</title>")),
        "{what}: the title: {body}"
    );
    assert!(body.contains("<h1>Page not found</h1>"), "{what}: {body}");
    match variant {
        Variant::Page => {
            assert!(body.contains(&format!("<p>{PAGE_SENTENCE}</p>")), "{what}");
            assert!(!body.contains(SCOPE_SENTENCE), "{what}");
            assert!(
                body.contains(&format!(
                    "<p><a href=\"#search\">Search these pages</a>, or go to the <a href=\"{root}index.html\">home page</a>.</p>"
                )),
                "{what}: the home link at {root}: {body}"
            );
        }
        Variant::Scope => {
            assert_eq!(root, "/", "{what}: the Scope variant is canon's");
            assert!(body.contains(&format!("<p>{SCOPE_SENTENCE}</p>")), "{what}");
            assert!(!body.contains(PAGE_SENTENCE), "{what}");
            assert!(
                body.contains("<p>See the <a href=\"/index.html\">latest docs</a>, or open the <a href=\"/~versions/\">list of versions, proposals and views</a>.</p>"),
                "{what}: {body}"
            );
        }
    }
    for key in [" href=\"", " src=\"", " data-index=\""] {
        for v in body.split(key).skip(1) {
            let v = &v[..v.find('"').expect("closed")];
            assert!(
                v.starts_with(root) || v.starts_with('#'),
                "{what}: {key}{v} is not under {root}"
            );
        }
    }
    assert_inert(&body, what);
}

/// No live behaviour on a generated 404: no script, no stream URL, no version pill.
#[track_caller]
pub(crate) fn assert_inert(body: &str, what: &str) {
    for needle in [
        "/_rdy/live.js",
        "data-events",
        "_rdy/events",
        "class=\"pill\"",
    ] {
        assert!(!body.contains(needle), "{what}: holds {needle:?}");
    }
}

/// Asserts `r` is exactly the plain answer of `f`.
#[track_caller]
pub(crate) fn assert_plain(r: &http::Response, f: Refusal, what: &str) {
    let want = refusal(f);
    assert_eq!(r.status, want.status, "{what}: {}", r.head);
    assert_eq!(text(r), String::from_utf8(want.body).unwrap(), "{what}");
    assert_eq!(
        r.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "{what}: {}",
        r.head
    );
}

// --- the world ----------------------------------------------------------------------------------

/// The project of every pin, named `Tiny`. Canon holds a folder `manual` (doc `intro`, page
/// `manual/intro.html`, prose `INTRO-CANON`), a top-level `secret` doc (prose `SECRET-WORDS`),
/// and an `admin` doc that holds profile `guide` (org, `Guide book`, the folder). Tag `v1.0.0`
/// is canon. View `work` edits intro to `INTRO-VIEW`; proposal `prop` edits it to `INTRO-PROP`.
/// View `authored` adds a top-level doc named 404 (`AUTHORED-404-BODY`). View `bad` holds a
/// profile `zzzz` whose metadata is malformed.
struct World {
    h: Hosted,
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

fn world() -> World {
    let (dir, mut r) = tiny();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-CANON"},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$sp", "parent": "$secret", "body": "SECRET-WORDS"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let (admin, manual) = (ids["$admin"].clone(), ids["$manual"].clone());
    ids.extend(put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "id": "$guide", "parent": admin,
            "fields": {"key": "guide", "title": "Guide book", "description": "About Guide book.",
                "audience": "org", "select": [manual]}}]),
    ));
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
        .propose(&writer(), "prop", "Prop", Some("Edits the intro."))
        .unwrap()
        .r#ref;
    put(
        &mut r,
        "authored",
        json!([
            {"op": "create", "type": "doc", "id": "$own", "fields": {"name": "404", "title": "Authored missing page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$own", "body": "AUTHORED-404-BODY"}
        ]),
    );
    // malformed metadata, past every write rule
    raw_commit(
        &mut r,
        "bad",
        vec![raw(
            "p-BAD01",
            "publication_profile",
            Some(&admin),
            "zz",
            &[
                ("key", "zzzz"),
                ("title", "ZZZ"),
                ("audience", "org"),
                ("select", "not a list"),
            ],
            "",
        )],
    );
    drop(r);
    let h = Hosted::start_in(dir);
    let url = shared(&h)
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    World {
        h,
        url,
        ids,
        prop: proposal_ref(&prop).unwrap(),
    }
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// Runs `f` on the daemon's writer; the writer publishes once `f` returns.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

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

/// `<method> /<path>` on `port` with its own Host.
fn ask(port: u16, method: &str, path: &str) -> http::Response {
    http::raw(
        sock(port),
        &format!("{method} /{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    )
}

fn get(port: u16, path: &str) -> http::Response {
    ask(port, "GET", path)
}

/// The port of the profile-only site of `key`.
fn only_port(sh: &Arc<Shared>, key: &str) -> u16 {
    let info = ensure_profile_site(Arc::clone(sh), key).unwrap();
    port_of(&info.url.expect("the profile site listens"))
}

/// `GET /_rdy/events?<query>` on `port`, which must be refused (a stream never ends).
fn refused_events(port: u16, query: &str) -> http::Response {
    let mut s = TcpStream::connect(sock(port)).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "GET /_rdy/events?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut got = Vec::new();
    s.read_to_end(&mut got)
        .unwrap_or_else(|e| panic!("{query}: no refusal ({e})"));
    let at = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&got[..at]).into_owned();
    assert!(
        !head.starts_with("HTTP/1.1 200"),
        "{query}: a stream opened"
    );
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    http::Response {
        status,
        head,
        body: got[at + 4..].to_vec(),
    }
}

/// The header lines of `r` but `Date`, in order.
fn headers(r: &http::Response) -> Vec<String> {
    r.head
        .split("\r\n")
        .filter(|l| !l.starts_with("Date: "))
        .map(str::to_string)
        .collect()
}

/// Records a removal request of block `target` in view `view` and proposes it.
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
                    reason: "a leaked canary".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        assert!(res.redaction.is_some(), "the request is recorded");
        r.propose(&writer(), view, "Remove it", Some("Removes the canary."))
            .unwrap()
            .id
    });
    v.as_str().unwrap().to_string()
}

/// Deletes the manual folder that profile `guide` selects, with its doc and prose, and lands it:
/// the profile's metadata still reads, but its selection no longer resolves, so nothing of it
/// builds.
fn drop_selection(sh: &Arc<Shared>, w: &World) {
    let ids: Vec<String> = ["$ip", "$intro", "$manual"]
        .iter()
        .map(|k| w.ids[*k].clone())
        .collect();
    on_writer(sh, move |r| {
        let ops: Vec<Value> = ids
            .iter()
            .map(|id| json!({"op": "delete", "id": id}))
            .collect();
        put(r, "dropit", Value::Array(ops));
        land(r, "dropit").unwrap();
    });
}

// --- the pins -----------------------------------------------------------------------------------

#[test]
fn dgm11_every_live_scope_gets_its_own_404() {
    let w = world();
    let port = port_of(&w.url);
    let prop = &w.prop;
    for (root, name) in [
        ("/".to_string(), "Tiny"),
        ("/~tag/v1.0.0/".into(), "Tiny"),
        ("/~view/work/".into(), "Tiny"),
        (format!("/~proposal/{prop}/"), "Tiny"),
        ("/~profile/guide/".into(), "Guide book"),
        ("/~tag/v1.0.0/~profile/guide/".into(), "Guide book"),
        ("/~view/work/~profile/guide/".into(), "Guide book"),
        (format!("/~proposal/{prop}/~profile/guide/"), "Guide book"),
    ] {
        // the scope serves its pages
        let home = get(port, &root[1..]);
        assert_eq!(home.status, 200, "{root}: {}", home.head);
        let profiled = root.contains("~profile");
        // a missing page, a missing file deep under the root, and on a profile an excluded page
        let mut missing = vec![
            "nosuch.html",
            "manual/nosuch/deep.html",
            "assets/nosuch.css",
        ];
        if profiled {
            missing.push("secret.html");
        }
        let first = get(port, &format!("{}nosuch.html", &root[1..]));
        for path in missing {
            let r = get(port, &format!("{}{path}", &root[1..]));
            let what = format!("{root}{path}");
            assert_themed(&r, Variant::Page, &root, name, &what);
            // the page does not depend on the path, and never echoes it
            assert_eq!(r.body, first.body, "{what}");
            assert!(!text(&r).contains("nosuch"), "{what}");
            if profiled {
                for needle in ["SECRET-WORDS", "Secret page", "Tiny"] {
                    assert!(!text(&r).contains(needle), "{what}: holds {needle:?}");
                }
                assert!(
                    text(&r).contains("Intro page"),
                    "{what}: the selected navigation"
                );
            } else {
                assert!(
                    text(&r).contains("Secret page"),
                    "{what}: the scope's navigation"
                );
            }
        }
    }
}

#[test]
fn dgm11_missing_scope_uses_canon_without_its_name() {
    let w = world();
    let port = port_of(&w.url);
    let stable = w.prop.split('@').next().unwrap();
    // validly parsed targets the site does not hold now: canon's Scope variant
    for path in [
        "~view/gone-view/".to_string(),
        "~view/gone-view/manual/intro.html".into(),
        "~tag/v9.9.9/".into(),
        "~tag/v9.9.9/index.html".into(),
        format!("~proposal/{stable}@9/"),
        "~profile/absentkey/".into(),
        "~profile/absentkey/manual/intro.html".into(),
        "~view/work/~profile/absentkey/".into(),
        "~view/gone-view/~profile/guide/".into(),
        "~tag/v9.9.9/~profile/guide/".into(),
        "~versions/?profile=absentkey".into(),
        "~versions/?scope=view:gone-view".into(),
    ] {
        let r = get(port, &path);
        assert_themed(&r, Variant::Scope, "/", "Tiny", &path);
        for name in ["gone-view", "v9.9.9", "absentkey", "@9"] {
            assert!(!text(&r).contains(name), "{path}: names {name:?}");
        }
    }
    // a path the guard does not route: canon's Page variant, which never reads the path
    let plain_page = get(port, "nosuch.html");
    for path in [
        "~nope/x.html",
        "~view/work",
        "~profile/guide",
        "~profile/guide/~profile/other/",
        "~view/work/~nope/index.html",
        "%ZZ.html",
        "_rdy/other",
        "~versions/elsewhere.html",
        "a/../b.html",
    ] {
        let r = get(port, path);
        assert_themed(&r, Variant::Page, "/", "Tiny", path);
        assert_eq!(r.body, plain_page.body, "{path}");
        for piece in ["~nope", "%ZZ", "_rdy/other", "elsewhere"] {
            assert!(!text(&r).contains(piece), "{path}: echoes {piece:?}");
        }
    }
}

#[test]
fn dgm11_profile_only_never_falls_back_to_source() {
    let w = world();
    let sh = shared(&w.h);
    let only = only_port(&sh, "guide");
    let intro = get(only, "manual/intro.html");
    assert_eq!(intro.status, 200, "{}", intro.head);
    let first = get(only, "nosuch.html");
    for path in [
        "nosuch.html",
        "manual/nosuch.html",
        "secret.html",
        "assets/nosuch.css",
        "~view/work/manual/intro.html",
        "~profile/guide/",
        "~versions/",
        "~tag/v1.0.0/",
        "%7Eview/work/",
        "_rdy/other",
        "404.html",
    ] {
        let r = get(only, path);
        assert_themed(&r, Variant::Page, "/", "Guide book", path);
        assert_eq!(r.body, first.body, "{path}");
        let body = text(&r);
        for needle in [
            "SECRET-WORDS",
            "Secret page",
            "Tiny",
            "INTRO-VIEW",
            "~versions",
            "~view",
            "~profile",
            "~tag",
            "~proposal",
            SCOPE_SENTENCE,
        ] {
            assert!(!body.contains(needle), "{path}: holds {needle:?}");
        }
    }
    // an event context of another target is the listener's page too, before any stream
    let q = "scope=view:work&page=index.html&rev=0011223344556677&label=0011223344556677";
    assert_themed(
        &refused_events(only, q),
        Variant::Page,
        "/",
        "Guide book",
        q,
    );

    // the profile goes: the plain answer on every path, never the source's page
    let guide = w.ids["$guide"].clone();
    on_writer(&sh, move |r| {
        put(r, "rm", json!([{"op": "delete", "id": guide}]));
        land(r, "rm").unwrap();
    });
    for path in ["nosuch.html", "index.html", "~view/work/", "_rdy/other"] {
        assert_plain(&get(only, path), Refusal::NotFound, path);
    }
    assert_plain(&refused_events(only, q), Refusal::NotFound, "events");
    // on the full listener its route is a scope the site does not hold now
    let full = port_of(&w.url);
    let r = get(full, "~profile/guide/manual/intro.html");
    assert_themed(&r, Variant::Scope, "/", "Tiny", "the full listener");
}

#[test]
fn dgm11_head_and_get_share_404_headers() {
    let w = world();
    let sh = shared(&w.h);
    let full = port_of(&w.url);
    let only = only_port(&sh, "guide");
    for (port, path) in [
        (full, "nosuch.html"),
        (full, "404.html"),
        (full, "~view/work/nosuch.html"),
        (full, "~profile/guide/secret.html"),
        (full, "~view/gone-view/"),
        (full, "~nope/x.html"),
        (only, "nosuch.html"),
        (only, "~view/work/"),
    ] {
        let g = get(port, path);
        let h = ask(port, "HEAD", path);
        assert_eq!(g.status, 404, "{path}: {}", g.head);
        assert_eq!(h.status, g.status, "HEAD {path}");
        assert_eq!(headers(&h), headers(&g), "HEAD {path}");
        assert!(h.body.is_empty(), "HEAD {path}: no body");
        assert_eq!(
            h.header("content-length"),
            Some(g.body.len().to_string().as_str()),
            "HEAD {path}"
        );
        // the common response and security headers stay
        for (name, value) in rdy_daemon::site::http::BASE_HEADERS {
            assert_eq!(h.header(name), Some(*value), "HEAD {path}: {name}");
        }
        assert_eq!(h.header("content-type"), Some("text/html; charset=utf-8"));
    }
}

#[test]
fn dgm11_404_never_subscribes_or_enters_indexes() {
    let w = world();
    let sh = shared(&w.h);
    let port = port_of(&w.url);
    for (root, target) in [
        ("", SiteTarget::Source(ScopeRef::Canon)),
        (
            "~view/work/",
            SiteTarget::Source(ScopeRef::View("work".into())),
        ),
        (
            "~profile/guide/",
            SiteTarget::Profile {
                scope: ScopeRef::Canon,
                key: "guide".into(),
            },
        ),
    ] {
        for path in ["nosuch.html", "404.html"] {
            let r = get(port, &format!("{root}{path}"));
            assert_eq!(r.status, 404, "{root}{path}");
            // the generated page itself is inert, not only a plain answer
            assert!(text(&r).contains(MARKER), "{root}{path}: {}", r.head);
            assert_inert(&text(&r), &format!("{root}{path}"));
        }
        let b = sh.site.refresh(&sh, &target).unwrap();
        for path in ["nosuch.html", "404.html"] {
            assert!(
                !b.live.revs.contains_key(path),
                "{root}: a revision of {path}"
            );
            assert!(!b.live.files.contains_key(path), "{root}: a file {path}");
            assert!(
                !b.live.label_at.contains_key(path),
                "{root}: slots of {path}"
            );
        }
        for index in ["search-index.json", "llms.txt", "llms-full.txt"] {
            if let Some(bytes) = b.live.files.get(index) {
                let t = String::from_utf8_lossy(bytes);
                assert!(!t.contains("Page not found"), "{root}{index}");
                assert!(!t.contains("404.html"), "{root}{index}");
            }
        }
        assert!(
            !b.live
                .files
                .values()
                .any(|f| f.windows(MARKER.len()).any(|x| x == MARKER.as_bytes())),
            "{root}: a generated page in the build"
        );
    }
    assert_eq!(sh.site.streams(), 0, "no 404 opened a stream");
}

#[test]
fn dgm11_guards_and_redaction_precede_themed_output() {
    let w = world();
    let sh = shared(&w.h);
    let port = port_of(&w.url);
    let own = format!("Host: 127.0.0.1:{port}");
    let raw = |req: String| http::raw(sock(port), &req);
    // the existing refusals keep their status and text: no page goes to a foreign Host or Origin
    for (req, f) in [
        (
            format!("GET /nosuch.html HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"),
            Refusal::Host,
        ),
        (
            format!("GET /~view/gone/ HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"),
            Refusal::Host,
        ),
        (
            format!("GET /~nope/ HTTP/1.1\r\n{own}\r\nOrigin: http://rebound.invalid\r\n\r\n"),
            Refusal::Origin,
        ),
        (
            format!("POST /nosuch.html HTTP/1.1\r\n{own}\r\nContent-Length: 0\r\n\r\n"),
            Refusal::Method,
        ),
        (
            format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\nContent-Length: 4\r\n\r\nbody"),
            Refusal::Body,
        ),
        (
            format!(
                "GET /nosuch.html HTTP/1.1\r\n{own}\r\nX-Pad: {}\r\n\r\n",
                "a".repeat(9000)
            ),
            Refusal::TooLarge,
        ),
        (
            format!("GET /nosuch.html HTTP/9\r\n{own}\r\n\r\n"),
            Refusal::Malformed,
        ),
    ] {
        let r = raw(req.clone());
        assert_plain(&r, f, &req[..req.find('\r').unwrap()]);
        assert!(!text(&r).contains("<html"), "{req}");
    }
    // HEAD of the event route keeps the method refusal, with the plain type and length
    let r = raw(format!(
        "HEAD /_rdy/events?scope=canon HTTP/1.1\r\n{own}\r\n\r\n"
    ));
    let want = refusal(Refusal::Method);
    assert_eq!(r.status, 405, "{}", r.head);
    assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(
        r.header("content-length"),
        Some(want.body.len().to_string().as_str())
    );

    // a removal is authorized and erased; the writer has not published its state yet
    let p = request(&sh, &w.ids["$sp"], "redact");
    let (landed_tx, landed_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let writer = {
        let sh = Arc::clone(&sh);
        std::thread::spawn(move || {
            on_writer(&sh, move |r| {
                r.land(&approver(), &p).unwrap();
                landed_tx.send(()).unwrap();
                go_rx.recv().unwrap();
            })
        })
    };
    landed_rx.recv().unwrap();
    // every not-found route waits for the fresh state: the existing Busy, no page
    for path in [
        "nosuch.html",
        "404.html",
        "~view/work/nosuch.html",
        "~view/gone-view/",
        "~profile/absentkey/",
        "~nope/x.html",
        "secret.html",
    ] {
        let r = get(port, path);
        assert_plain(&r, Refusal::Busy, path);
        assert!(!text(&r).contains("SECRET-WORDS"), "{path}");
    }
    go_tx.send(()).unwrap();
    writer.join().unwrap();
    // then the themed pages again, of the fresh state
    assert_themed(
        &get(port, "nosuch.html"),
        Variant::Page,
        "/",
        "Tiny",
        "after",
    );
    assert_themed(
        &get(port, "~view/gone-view/"),
        Variant::Scope,
        "/",
        "Tiny",
        "after",
    );
}

#[test]
fn dgm11_broken_layout_has_the_exact_plain_fallback() {
    // the texts: Broken's new body; the plain not found unchanged, 74 bytes with no line end
    assert_eq!(refusal(Refusal::Broken).status, 500);
    assert_eq!(refusal(Refusal::Broken).body, BROKEN.as_bytes());
    assert_eq!(refusal(Refusal::NotFound).body, PLAIN.as_bytes());
    assert_eq!(PLAIN.len(), 74);

    let w = world();
    let sh = shared(&w.h);
    let full = port_of(&w.url);
    // a target that does not build is broken, with the new text, and its pages never 404
    for path in [
        "~view/bad/~profile/zzzz/",
        "~view/bad/~profile/zzzz/nosuch.html",
    ] {
        assert_plain(&get(full, path), Refusal::Broken, path);
    }
    // a profile-only listener whose profile metadata stops reading: its own layout cannot be
    // built, so a path it does not route gets the plain answer, never the source's page
    let only = only_port(&sh, "guide");
    assert_themed(
        &get(only, "~view/work/"),
        Variant::Page,
        "/",
        "Guide book",
        "before",
    );
    drop_selection(&sh, &w);
    for path in ["~view/work/", "~versions/", "_rdy/other"] {
        let r = get(only, path);
        assert_plain(&r, Refusal::NotFound, path);
        assert_eq!(r.body.len(), 74, "{path}");
    }
    assert_plain(
        &get(only, "nosuch.html"),
        Refusal::Broken,
        "a page of the listener",
    );
}

#[test]
fn dgm11_explicit_404_and_authored_collision_follow_the_matrix() {
    let w = world();
    let sh = shared(&w.h);
    let port = port_of(&w.url);
    // no authored page: /404.html is the generated page, status 404, with no live behaviour
    for root in ["", "~view/work/", "~profile/guide/"] {
        let path = format!("{root}404.html");
        let g = get(port, &path);
        let name = if root.contains("profile") {
            "Guide book"
        } else {
            "Tiny"
        };
        assert_themed(&g, Variant::Page, &format!("/{root}"), name, &path);
        let h = ask(port, "HEAD", &path);
        assert_eq!((h.status, headers(&h)), (404, headers(&g)), "HEAD {path}");
        assert!(h.body.is_empty(), "HEAD {path}");
    }
    // an authored 404 doc is an ordinary page, with normal live behaviour
    let g = get(port, "~view/authored/404.html");
    assert_eq!(g.status, 200, "{}", g.head);
    let body = text(&g);
    assert!(body.contains("AUTHORED-404-BODY"), "{body}");
    assert!(
        !body.contains(MARKER),
        "an authored page is not the generated one"
    );
    assert!(
        body.contains("/_rdy/live.js") && body.contains("data-events"),
        "live"
    );
    let h = ask(port, "HEAD", "~view/authored/404.html");
    assert_eq!((h.status, headers(&h)), (200, headers(&g)));
    assert!(h.body.is_empty());
    // a missing path beside it is the themed page, never the authored body
    for path in [
        "~view/authored/nosuch.html",
        "~view/authored/manual/gone.html",
    ] {
        let r = get(port, path);
        assert_themed(&r, Variant::Page, "/~view/authored/", "Tiny", path);
        assert!(!text(&r).contains("AUTHORED-404-BODY"), "{path}");
        let h = ask(port, "HEAD", path);
        assert_eq!((h.status, headers(&h)), (404, headers(&r)), "HEAD {path}");
        assert!(h.body.is_empty(), "HEAD {path}");
    }
    // and a live build never gives the local collision warning
    let b = sh
        .site
        .refresh(&sh, &SiteTarget::Source(ScopeRef::View("authored".into())))
        .unwrap();
    assert!(
        !b.live.warnings.iter().any(|w| w.contains("404")),
        "{:?}",
        b.live.warnings
    );
}
