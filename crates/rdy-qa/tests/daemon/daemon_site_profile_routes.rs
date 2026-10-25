//! spv6 (D-CWC91, D-N5NSN, D-FV6G9, D-42YMF, D-PX5B0): a publication profile on the full live
//! listener. `~profile/<key>/` follows zero or one scope prefix, and each scope reads its own
//! revision of the profile, so a profile that only a proposal holds serves under that proposal.
//! The versions page and the event stream take the profile and menu context from their query.
//! The cache keeps each scope, profile and menu context apart, keeps each profile's history
//! memo across builds, and keeps the redaction barriers. A blocked public profile serves with
//! its warning while every durable output still refuses it.
use crate::daemon_site_not_found::{Variant, assert_themed};
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{Refusal, ScopeRef, SiteTarget, refusal};
use rdy_daemon::site::live::{LiveEvent, LiveSite, SiteBuild, Subscription, normalize};
use rdy_engine::engine::{ExportRequest, RenderRequest, write_typed};
use rdy_engine::redaction::RedactInput;
use rdy_engine::site_scope::proposal_ref;
use rdy_engine::{E_PUBLICATION_REFERENCE, E_REDACTED_STATE, Rdy};
use rdy_model::Hash;
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, raw, raw_commit, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http::{self, Sse};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;

/// The deadline of every wait for an event.
const T: Duration = Duration::from_secs(10);
/// What a removed block shows.
const STUB: &str = "Content removed.";
/// The fix of a blocked public profile's live warning.
const PUBLIC_FIX: &str =
    "Include the target docs or remove these references. Run the publication check again.";
/// The history-limit warning of a profile build (Task 4), as one entry.
const HISTORY_WARNING: &str = "Profile history exceeds 100000 landings. The landings chip and since link are omitted. Open a tagged profile scope to read that version.";

// --- the world ----------------------------------------------------------------------------------

/// The project of every pin, on a daemon. Canon holds a folder `manual` (doc `intro`, page
/// `manual/intro.html`), a top-level `secret` doc whose three prose blocks are canaries and
/// whose first one references intro's prose, a top-level `figdoc` with a figure and a
/// reference to intro, and an `admin` doc that holds the profiles: `guide` (org, `Guide book`,
/// the folder), `other` (public, `Other book`, the secret doc) and `pubfig` (public, `Figure
/// book`, figdoc). Tag `v1.0.0` is canon with intro at `INTRO-CANON-1`; one landing since
/// moved it to `INTRO-CANON-2`. View `work` edits intro to `INTRO-VIEW`. Proposal `prop` has
/// version 1 (`INTRO-PROP-1`) and version 2 (`INTRO-PROP-2`). Proposal `newprof` adds profile
/// `fresh` (`Fresh book`, the intro doc), which canon does not hold. View `bad` holds a
/// profile `zzzz` whose metadata is malformed.
struct World {
    h: Hosted,
    url: String,
    ids: BTreeMap<String, String>,
    /// Proposal `prop`'s version 1, and its stable ref (its latest version).
    prop_v1: String,
    prop_latest: String,
    /// Proposal `newprof`'s version 1.
    newprof: String,
    fig_png: Vec<u8>,
}

/// Writes `ops` into `view`; the created ids by `$name`.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

fn profile_op(key: &str, title: &str, audience: &str, admin: &str, select: &[&str]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": admin,
        "fields": {"key": key, "title": title, "description": format!("About {title}."),
            "audience": audience, "select": select}})
}

fn propose(r: &mut Rdy, view: &str, title: &str) -> String {
    r.propose(&writer(), view, title, None)
        .unwrap_or_else(|e| panic!("propose {view}: {e}"))
        .r#ref
}

fn world() -> World {
    let (dir, mut r) = tiny();
    let fig_png = b"\x89PNG\r\n\x1a\nFIGBYTES-figdoc".to_vec();
    let fig_blob: Hash = r.store_mut().put_blob(&fig_png).unwrap();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-CANON-1"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    let ip = ids["$ip"].clone();
    ids.extend(put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$sa", "parent": "$secret", "body": format!("SECRET-A sees {{{{{ip}}}}}.")},
            {"op": "create", "type": "prose", "id": "$sb", "parent": "$secret", "body": "SECRET-B words."},
            {"op": "create", "type": "prose", "id": "$sc", "parent": "$secret", "body": "SECRET-C words."},
            {"op": "create", "type": "doc", "id": "$figdoc", "fields": {"name": "figdoc", "title": "Figure page", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$figdoc", "fields": {"blob": fig_blob.to_string(), "media_type": "image/png", "alt": "A figure"}},
            {"op": "create", "type": "prose", "parent": "$figdoc", "body": format!("Figure sees {{{{{ip}}}}}.")}
        ]),
    ));
    land(&mut r, "w1").unwrap();
    let admin = ids["$admin"].clone();
    ids.extend(put(
        &mut r,
        "admin",
        json!([
            profile_op("guide", "Guide book", "org", &admin, &[&ids["$guide"]]),
            profile_op("other", "Other book", "public", &admin, &[&ids["$secret"]]),
            profile_op(
                "pubfig",
                "Figure book",
                "public",
                &admin,
                &[&ids["$figdoc"]]
            )
        ]),
    ));
    land(&mut r, "admin").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    edit_land(&mut r, &ip, "INTRO-CANON-2");
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": ip, "body": "INTRO-VIEW"}]),
    );
    put(
        &mut r,
        "prop",
        json!([{"op": "update", "id": ip, "body": "INTRO-PROP-1"}]),
    );
    let prop_v1 = propose(&mut r, "prop", "Prop");
    put(
        &mut r,
        "prop",
        json!([{"op": "update", "id": ip, "body": "INTRO-PROP-2"}]),
    );
    let v2 = propose(&mut r, "prop", "Prop");
    assert!(
        prop_v1.ends_with("@1") && v2.ends_with("@2"),
        "{prop_v1} {v2}"
    );
    let prop_latest = prop_v1.split('@').next().unwrap().to_string();
    let intro = ids["$intro"].clone();
    put(
        &mut r,
        "newprof",
        json!([profile_op("fresh", "Fresh book", "org", &admin, &[&intro])]),
    );
    let newprof = propose(&mut r, "newprof", "New profile");
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
        prop_v1: proposal_ref(&prop_v1).unwrap(),
        prop_latest: proposal_ref(&prop_latest).unwrap(),
        newprof: proposal_ref(&newprof).unwrap(),
        fig_png,
    }
}

/// Lands an edit of prose `id` to `body`, through view `edit`.
fn edit_land(r: &mut Rdy, id: &str, body: &str) {
    put(r, "edit", json!([{"op": "update", "id": id, "body": body}]));
    land(r, "edit").unwrap();
}

fn shared(h: &Hosted) -> Arc<Shared> {
    Arc::clone(h.run.as_ref().unwrap().shared())
}

/// Runs `f` on the daemon's writer.
fn on_writer<T: serde::Serialize + 'static>(
    sh: &Arc<Shared>,
    f: impl FnOnce(&mut Rdy) -> T + Send + 'static,
) -> Value {
    write_typed(&*sh.engine(), move |r| Ok(f(r))).unwrap()
}

/// `GET <site><path>` (`path` without its leading `/`).
fn get(w: &World, path: &str) -> http::Response {
    http::get(&format!("{}{path}", w.url))
}

/// `HEAD <site><path>`.
fn head(w: &World, path: &str) -> http::Response {
    let authority = w.url.strip_prefix("http://").unwrap().trim_end_matches('/');
    let addr = authority.parse().unwrap();
    http::raw(
        addr,
        &format!("HEAD /{path} HTTP/1.1\r\nHost: {authority}\r\n\r\n"),
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

fn profile(scope: ScopeRef, key: &str) -> SiteTarget {
    SiteTarget::Profile {
        scope,
        key: key.into(),
    }
}

fn menu(scope: ScopeRef, key: Option<&str>) -> SiteTarget {
    SiteTarget::Menu {
        scope,
        profile: key.map(str::to_string),
    }
}

fn view(name: &str) -> ScopeRef {
    ScopeRef::View(name.into())
}

fn file(b: &SiteBuild, path: &str) -> String {
    String::from_utf8_lossy(
        b.live
            .files
            .get(path)
            .unwrap_or_else(|| panic!("no {path}: {:?}", b.live.files.keys())),
    )
    .into_owned()
}

fn holds(b: &SiteBuild, needle: &str) -> bool {
    b.live
        .files
        .values()
        .any(|f| f.windows(needle.len()).any(|w| w == needle.as_bytes()))
}

fn sub(sh: &Arc<Shared>, target: &SiteTarget, page: &str, b: &SiteBuild) -> Subscription {
    sh.site
        .subscribe(sh, target, page, &b.live.revs[page], &b.live.label_rev)
        .unwrap_or_else(|_| panic!("the site is busy"))
}

fn empty(s: &mut Subscription) -> bool {
    s.rx.try_recv() == Err(TryRecvError::Empty)
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

fn epoch(sh: &Shared) -> u64 {
    sh.state.load().reader().redaction_epoch()
}

/// A barrier at the publication check of every build of `target`.
fn hold(site: &LiveSite, target: SiteTarget) -> (Receiver<()>, Sender<()>) {
    let target = normalize(&target);
    let (at_tx, at_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let (at_tx, go_rx) = (Mutex::new(at_tx), Mutex::new(go_rx));
    site.set_before_publish(Some(Box::new(move |t: &SiteTarget| {
        if *t == target {
            let _ = at_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv();
        }
    })));
    (at_rx, go_tx)
}

/// A barrier in each subscribe of `target`, after its refresh and before its stream registers.
fn hold_after_refresh(site: &LiveSite, target: SiteTarget) -> (Receiver<()>, Sender<()>) {
    let target = normalize(&target);
    let (at_tx, at_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let (at_tx, go_rx) = (Mutex::new(at_tx), Mutex::new(go_rx));
    site.set_after_subscribe_refresh(Some(Box::new(move |t: &SiteTarget| {
        if *t == target {
            let _ = at_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv();
        }
    })));
    (at_rx, go_tx)
}

// --- 1. routes ----------------------------------------------------------------------------------

#[test]
fn spv6_profile_routes_serve_each_scopes_revision() {
    let w = world();
    let (v1, latest, newprof) = (&w.prop_v1, &w.prop_latest, &w.newprof);
    for (path, want) in [
        (
            "~profile/guide/manual/intro.html".to_string(),
            "INTRO-CANON-2",
        ),
        (
            "~tag/v1.0.0/~profile/guide/manual/intro.html".into(),
            "INTRO-CANON-1",
        ),
        (
            "~view/work/~profile/guide/manual/intro.html".into(),
            "INTRO-VIEW",
        ),
        (
            format!("~proposal/{v1}/~profile/guide/manual/intro.html"),
            "INTRO-PROP-1",
        ),
        (
            format!("~proposal/{latest}/~profile/guide/manual/intro.html"),
            "INTRO-PROP-2",
        ),
        // only the proposal holds this profile: it serves under the proposal
        (
            format!("~proposal/{newprof}/~profile/fresh/manual/intro.html"),
            "INTRO-CANON-2",
        ),
        // a folded spelling of the key is the same profile
        ("~profile/GUIDE/manual/intro.html".into(), "INTRO-CANON-2"),
    ] {
        let page = get(&w, &path);
        assert_eq!(page.status, 200, "{path}: {}", page.head);
        let body = text(&page);
        assert!(body.contains(want), "{path}: {want}");
        for other in [
            "INTRO-CANON-1",
            "INTRO-CANON-2",
            "INTRO-VIEW",
            "INTRO-PROP-1",
            "INTRO-PROP-2",
        ] {
            assert!(other == want || !body.contains(other), "{path}: {other}");
        }
        assert!(!body.contains("SECRET-"), "{path}: an excluded doc");
        let h = head(&w, &path);
        assert_eq!(h.status, 200, "HEAD {path}: {}", h.head);
        assert!(h.body.is_empty(), "HEAD {path}: no body");
        assert_eq!(h.header("content-type"), page.header("content-type"));
        // the profile's home page under the same root
        let root = &path[..path.len() - "manual/intro.html".len()];
        let home = get(&w, root);
        assert_eq!(home.status, 200, "{root}: {}", home.head);
        assert!(text(&home).contains("Intro page"), "{root}");
    }
    // a proposal-only profile: canon, the tag and the view hold no such profile. A target the
    // site does not hold now is canon's Scope variant (dgm11)
    for path in [
        "~profile/fresh/",
        "~profile/fresh/manual/intro.html",
        "~tag/v1.0.0/~profile/fresh/",
        "~view/work/~profile/fresh/",
        "~profile/nope/",
        "~view/work/~profile/nope/",
        "~view/nowhere/~profile/guide/",
    ] {
        assert_themed(&get(&w, path), Variant::Scope, "/", "Tiny", path);
        assert_eq!(head(&w, path).status, 404, "HEAD {path}");
    }
    // a page the selection does not have: the profile route's Page variant (dgm11)
    for path in ["~profile/guide/secret.html", "~profile/guide/nope.html"] {
        let r = get(&w, path);
        assert_themed(&r, Variant::Page, "/~profile/guide/", "Guide book", path);
        assert!(!text(&r).contains("SECRET-"), "{path}: an excluded doc");
        assert_eq!(head(&w, path).status, 404, "HEAD {path}");
    }
    // malformed profile metadata is a build error, not a missing page
    assert_refusal(
        &get(&w, "~view/bad/~profile/zzzz/"),
        Refusal::Broken,
        "malformed",
    );
    // the source site stays as it was
    let source = get(&w, "manual/intro.html");
    assert_eq!(source.status, 200);
    assert!(text(&source).contains("INTRO-CANON-2") && text(&source).contains("Secret page"));
    assert_themed(&get(&w, "nope.html"), Variant::Page, "/", "Tiny", "source");
}

// --- 2. queries ---------------------------------------------------------------------------------

#[test]
fn spv6_queries_preserve_first_values_and_statuses() {
    let w = world();
    // an ordinary page, of a profile or not, ignores its query
    for (path, query) in [
        (
            "~profile/guide/manual/intro.html",
            "?scope=view:work&profile=other&menu=1",
        ),
        ("~profile/guide/manual/intro.html", "?scope=%ZZ"),
        ("manual/intro.html", "?profile=guide&scope=%ZZ"),
        ("~view/work/~profile/guide/", "?profile=nope"),
    ] {
        let plain = get(&w, path);
        let asked = get(&w, &format!("{path}{query}"));
        assert_eq!(plain.status, 200, "{path}");
        assert_eq!(asked.status, 200, "{path}{query}");
        assert_eq!(asked.body, plain.body, "{path}{query}");
    }

    // the versions page reads the first scope and profile, decodes only those, ignores the rest
    let ctx = get(&w, "~versions/?scope=view%3Awork&profile=guide");
    assert_eq!(ctx.status, 200, "{}", ctx.head);
    let body = text(&ctx);
    assert!(
        body.contains("aria-current=\"page\"") && body.contains("Guide book"),
        "the current profile row: {body}"
    );
    assert!(
        body.contains("menu=1"),
        "the page follows its context's stream"
    );
    for query in [
        "?x=%ZZ&scope=view:work&scope=canon&profile=guide&profile=other",
        "?scope=view%3Awork&scope=%ZZ&profile=GUIDE&profile=%ZZ&menu=7",
        "/index.html?profile=guide&scope=view:work",
    ] {
        let path = match query.strip_prefix('/') {
            Some(rest) => format!("~versions/{rest}"),
            None => format!("~versions/{query}"),
        };
        let got = get(&w, &path);
        assert_eq!(got.status, 200, "{path}: {}", got.head);
        assert_eq!(got.body, ctx.body, "{path}");
    }
    // no context is canon's menu, and `/~versions/` stays bare in its own links
    let bare = get(&w, "~versions/");
    assert_eq!(bare.status, 200);
    assert_eq!(get(&w, "~versions/?scope=canon").body, bare.body);
    assert_eq!(get(&w, "~versions/?unknown=1").body, bare.body);
    assert!(text(&bare).contains("data-events=\"/_rdy/events?scope=versions&amp;page=index.html"));
    // a first recognized value that does not decode, or names nothing valid, is malformed
    for query in [
        "scope=%ZZ&scope=canon",
        "scope=bogus",
        "scope=versions",
        "scope=view:a:b",
        "scope=",
        "profile=%ZZ&profile=guide",
        "profile=P-12",
        "profile=a%2Fb",
        "profile=",
        "profile=..",
    ] {
        assert_refusal(
            &get(&w, &format!("~versions/?{query}")),
            Refusal::Malformed,
            query,
        );
    }
    // a profile the scope does not hold: canon's Scope variant (dgm11)
    assert_themed(
        &get(&w, "~versions/?profile=fresh"),
        Variant::Scope,
        "/",
        "Tiny",
        "an absent profile",
    );

    // the events route: first of each, unknown keys ignored, malformed otherwise
    let page = get(&w, "~view/work/~profile/guide/manual/intro.html");
    let events = http::events_url(&w.url, &text(&page));
    assert!(
        events.contains("/_rdy/events?scope=view:work&profile=guide&page=manual/intro.html&rev="),
        "{events}"
    );
    drop(Sse::open(&events));
    let menu_events = http::events_url(&w.url, &text(&ctx));
    assert!(
        menu_events.contains("scope=view:work&profile=guide&menu=1&page=index.html"),
        "{menu_events}"
    );
    drop(Sse::open(&menu_events));
    let q = events.split_once('?').unwrap().1.to_string();
    let rest = q.split_once("&page=").unwrap().1.to_string();
    let base = format!("{}_rdy/events?", w.url);
    // duplicates and unknown keys, with values that would not decode
    drop(Sse::open(&format!(
        "{base}x=%ZZ&scope=view:work&scope=%ZZ&profile=guide&profile=%ZZ&page={rest}&page=%ZZ"
    )));
    for bad in [
        format!("scope=%ZZ&scope=canon&profile=guide&page={rest}"),
        format!("scope=canon&profile=%ZZ&profile=guide&page={rest}"),
        format!("scope=canon&profile=P-12&page={rest}"),
        format!("scope=canon&menu=2&page={rest}"),
        format!("scope=canon&menu=&page={rest}"),
        format!("scope=versions&menu=1&page={rest}"),
        format!("scope=versions&profile=guide&page={rest}"),
        format!("scope=bogus&profile=guide&page={rest}"),
        format!(
            "scope=canon&profile=guide&page=../x&{}",
            rest.split_once('&').unwrap().1
        ),
        format!("profile=guide&page={rest}"),
    ] {
        assert_refusal(
            &http::get(&format!("{base}{bad}")),
            Refusal::Malformed,
            &bad,
        );
    }
    // the base menu spelling still opens canon's menu stream
    let bare_events = http::events_url(&w.url, &text(&bare));
    assert!(
        bare_events.contains("scope=versions&page=index.html"),
        "{bare_events}"
    );
    drop(Sse::open(&bare_events));

    // malformed paths stay not found
    for path in [
        "~profile/guide/~profile/other/",
        "~profile/guide/guide/~profile/other/",
        "guide/~profile/guide/",
        "~view/work/guide/~profile/guide/",
        "~view/work/~profile/guide/~profile/other/",
        "~view/work/~nope/index.html",
        "~profile/guide/~view/work/",
        "~profile/guide",
        "~profile/",
        "~profile/%ZZ/",
        "~profile/P-1234/",
        "~profile/p-ABCDE/",
        "~profile/a%2Fb/",
        "~profile/a%5Cb/",
        "~profile/a%00b/",
        "~profile/./",
        "~profile/../",
        "~profile/two--dashes/",
        &format!("~profile/{}/", "k".repeat(33)),
        "~profile/guide/../secret.html",
        "~tag/v1.0.0/~profile/guide",
    ] {
        // a path the guard does not route: canon's Page variant (dgm11)
        assert_themed(&get(&w, path), Variant::Page, "/", "Tiny", path);
    }
}

// --- 3. cache -----------------------------------------------------------------------------------

#[test]
fn spv6_profile_cache_separates_scope_and_menu() {
    let w = world();
    let sh = shared(&w.h);
    let targets = [
        SiteTarget::Source(ScopeRef::Canon),
        profile(ScopeRef::Canon, "guide"),
        profile(ScopeRef::Canon, "other"),
        profile(view("work"), "guide"),
        profile(view("work"), "other"),
        menu(ScopeRef::Canon, None),
        menu(ScopeRef::Canon, Some("guide")),
        menu(view("work"), None),
        menu(view("work"), Some("guide")),
    ];
    let builds: Vec<Arc<SiteBuild>> = targets
        .iter()
        .map(|t| {
            sh.site
                .refresh(&sh, t)
                .unwrap_or_else(|e| panic!("{t:?}: {e}"))
        })
        .collect();
    for (i, a) in builds.iter().enumerate() {
        for (j, b) in builds.iter().enumerate().skip(i + 1) {
            assert!(!Arc::ptr_eq(a, b), "{:?} {:?}", targets[i], targets[j]);
            assert_ne!(
                file(a, "index.html"),
                file(b, "index.html"),
                "{:?} and {:?} share bytes",
                targets[i],
                targets[j]
            );
        }
    }
    // each build's own warnings: only the blocked public profile has one
    let warning = format!("Public profile has 1 references outside its selection. {PUBLIC_FIX}");
    for (t, b) in targets.iter().zip(&builds) {
        let want: Vec<String> = match t {
            SiteTarget::Profile { key, .. } if key == "other" => vec![warning.clone()],
            _ => vec![],
        };
        assert_eq!(b.live.warnings, want, "{t:?}");
        let profile = b.profile.as_ref().map(|p| p.title.as_str());
        match t {
            SiteTarget::Profile { key, .. } if key == "guide" => {
                assert_eq!(profile, Some("Guide book"))
            }
            SiteTarget::Profile { .. } => assert_eq!(profile, Some("Other book")),
            _ => assert_eq!(profile, None, "{t:?}"),
        }
    }
    // the source of the versions page is canon's menu: one entry
    let again = sh
        .site
        .refresh(&sh, &SiteTarget::Source(ScopeRef::Versions))
        .unwrap();
    assert!(
        Arc::ptr_eq(&again, &builds[5]),
        "one build for canon's menu"
    );
    // a profile page links its menu in its scope and profile
    assert!(
        file(&builds[3], "manual/intro.html")
            .contains("href=\"/~versions/?scope=view%3Awork&amp;profile=guide\""),
        "the pill opens the context's menu"
    );

    // an excluded edit sends a profile's open pages nothing; a selected edit reloads its page
    let guide = profile(ScopeRef::Canon, "guide");
    let b = &builds[1];
    let mut intro = sub(&sh, &guide, "manual/intro.html", b);
    let mut home = sub(&sh, &guide, "index.html", b);
    let mut other = sub(&sh, &targets[2], "secret.html", &builds[2]);
    for s in [&mut intro, &mut home, &mut other] {
        assert!(empty(s), "current pages get nothing on subscribe");
    }
    let sb = w.ids["$sb"].clone();
    on_writer(&sh, move |r| edit_land(r, &sb, "SECRET-B edited."));
    let after = sh.site.refresh(&sh, &guide).unwrap();
    assert!(
        empty(&mut intro) && empty(&mut home),
        "an edit outside the selection sends nothing"
    );
    assert_eq!(
        file(&after, "manual/intro.html"),
        file(b, "manual/intro.html")
    );
    sh.site.refresh(&sh, &targets[2]).unwrap();
    assert_eq!(
        other.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "the other profile shows it"
    );
    let ip = w.ids["$ip"].clone();
    on_writer(&sh, move |r| edit_land(r, &ip, "INTRO-CANON-3"));
    let after = sh.site.refresh(&sh, &guide).unwrap();
    assert!(file(&after, "manual/intro.html").contains("INTRO-CANON-3"));
    assert_eq!(
        intro.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "a selected edit reloads"
    );

    // the menu's view and proposal counts come from the source-count cache, in every context
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = Arc::clone(&calls);
        sh.site.set_docs_changed(Box::new(move |st, scope| {
            calls.fetch_add(1, Ordering::SeqCst);
            rdy_render::menu::docs_changed(st, scope)
        }));
    }
    let cx = menu(view("work"), Some("other"));
    sh.site.refresh(&sh, &cx).unwrap();
    let first = calls.load(Ordering::SeqCst);
    assert!(first > 0, "a landing moved what the counts read");
    for t in [
        menu(ScopeRef::Canon, None),
        menu(ScopeRef::Canon, Some("guide")),
        menu(view("work"), Some("guide")),
        menu(ScopeRef::Proposal(w.newprof.clone()), Some("fresh")),
    ] {
        sh.site.refresh(&sh, &t).unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        first,
        "each count is read once, for every context"
    );
}

// --- 4. history memo and its warning ------------------------------------------------------------

/// The landings of canon after the tag `v1.0.0`.
fn landings_since_tag(sh: &Shared) -> u64 {
    let st = sh.state.load_full();
    let tag: Hash = st.versions().unwrap()[0].landing.parse().unwrap();
    let mut n = 0;
    let mut cur = st.canon_head();
    while let Some(h) = cur
        && h != tag
    {
        n += 1;
        cur = st.reader().landing(&h).unwrap().prev;
    }
    n
}

#[test]
fn spv6_profile_history_reuses_memo_and_warns_once() {
    let w = world();
    let sh = shared(&w.h);
    let guide = profile(ScopeRef::Canon, "guide");
    let evals = || sh.site.history_evaluations(&guide).expect("a slot");
    // cold: one landing since the tag, which changed what the profile shows
    let b = sh.site.refresh(&sh, &guide).unwrap();
    assert!(file(&b, "index.html").contains("1 landing since v1.0.0"));
    let cold = evals();
    assert_eq!(
        cold,
        landings_since_tag(&sh),
        "a cold walk evaluates each landing"
    );
    // an unchanged tip evaluates nothing, even when another target builds
    sh.site.refresh(&sh, &guide).unwrap();
    sh.site
        .refresh(&sh, &profile(ScopeRef::Canon, "other"))
        .unwrap();
    assert_eq!(evals(), cold);
    // a warm rebuild visits only the new landings: a selected one, then an excluded one
    let ip = w.ids["$ip"].clone();
    on_writer(&sh, move |r| edit_land(r, &ip, "INTRO-CANON-3"));
    let b = sh.site.refresh(&sh, &guide).unwrap();
    assert!(
        file(&b, "manual/intro.html").contains("INTRO-CANON-3"),
        "the bytes changed"
    );
    assert!(file(&b, "index.html").contains("2 landings since v1.0.0"));
    assert_eq!(evals(), cold + 1, "one new landing, one pair");
    let sb = w.ids["$sb"].clone();
    on_writer(&sh, move |r| edit_land(r, &sb, "SECRET-B edited."));
    let b = sh.site.refresh(&sh, &guide).unwrap();
    assert!(file(&b, "index.html").contains("2 landings since v1.0.0"));
    assert_eq!(evals(), cold + 2);
    // a removal moves the epoch: the memo is cleared, and every landing is evaluated again
    let p = request(&sh, &w.ids["$sc"], "redact");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    let before = evals();
    let b = sh.site.refresh(&sh, &guide).unwrap();
    assert_eq!(b.live.epoch, epoch(&sh));
    assert_eq!(
        evals() - before,
        landings_since_tag(&sh),
        "the new epoch walks the interval again"
    );

    // past the ceiling: the page serves, without the chip or the since link; the warning is
    // logged once while the condition holds
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let lines = Arc::clone(&lines);
        sh.site.set_log_hook(Some(Box::new(move |l: &str| {
            lines.lock().unwrap().push(l.to_string())
        })));
    }
    let logged = || {
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| {
                **l == format!("site: /~view/work/~profile/guide/: warning: {HISTORY_WARNING}")
            })
            .count()
    };
    let work = profile(view("work"), "guide");
    sh.site.set_history_ceiling(Some(2));
    for _ in 0..3 {
        let page = get(&w, "~view/work/~profile/guide/");
        assert_eq!(page.status, 200, "{}", page.head);
        let body = text(&page);
        assert!(!body.contains("since v1.0.0"), "no neutral chip: {body}");
        assert!(!body.contains("changes-since.html"), "no since link");
    }
    assert_themed(
        &get(&w, "~view/work/~profile/guide/changes-since.html"),
        Variant::Page,
        "/~view/work/~profile/guide/",
        "Guide book",
        "no since page",
    );
    assert_eq!(
        sh.site.refresh(&sh, &work).unwrap().live.warnings,
        vec![HISTORY_WARNING.to_string()]
    );
    assert_eq!(logged(), 1, "logged once across requests");
    // an extending chain stays over the limit: built again, logged no more
    let sb = w.ids["$sb"].clone();
    on_writer(&sh, move |r| edit_land(r, &sb, "SECRET-B again."));
    let page = get(&w, "~view/work/~profile/guide/");
    assert_eq!(page.status, 200);
    assert!(!text(&page).contains("since v1.0.0"));
    assert_eq!(logged(), 1, "the condition persists: no second line");
    // a shorter interval (a new tag) clears the condition: the chip comes back with the next
    // selected landing, and a new over-limit run is logged again
    on_writer(&sh, |r| {
        r.tag(&approver(), "v2.0.0", None, None).unwrap();
    });
    assert!(
        sh.site
            .refresh(&sh, &work)
            .unwrap()
            .live
            .warnings
            .is_empty()
    );
    let ip = w.ids["$ip"].clone();
    on_writer(&sh, move |r| edit_land(r, &ip, "INTRO-CANON-4"));
    let page = get(&w, "~view/work/~profile/guide/");
    assert!(
        text(&page).contains("1 landing since v2.0.0"),
        "the chip is back"
    );
    assert!(
        sh.site
            .refresh(&sh, &work)
            .unwrap()
            .live
            .warnings
            .is_empty()
    );
    assert_eq!(logged(), 1);
    for body in ["INTRO-CANON-5", "INTRO-CANON-6"] {
        let ip = w.ids["$ip"].clone();
        on_writer(&sh, move |r| edit_land(r, &ip, body));
    }
    let page = get(&w, "~view/work/~profile/guide/");
    assert_eq!(page.status, 200);
    assert!(
        !text(&page).contains("since v2.0.0"),
        "over the limit again"
    );
    assert_eq!(logged(), 2, "a new over-limit run is logged");
    sh.site.set_log_hook(None);
}

/// Fix round 1: eight targets with open pages hold their builds outside the eight inactive
/// slots, so a ninth profile target keeps its build, its memo and its warning state.
#[test]
fn spv6_open_targets_leave_inactive_slots_for_profile_memo() {
    let w = world();
    let sh = shared(&w.h);
    let active = [
        SiteTarget::Source(ScopeRef::Canon),
        SiteTarget::Source(view("work")),
        SiteTarget::Source(ScopeRef::Proposal(w.prop_v1.clone())),
        SiteTarget::Source(ScopeRef::Tag("v1.0.0".into())),
        profile(ScopeRef::Canon, "other"),
        profile(ScopeRef::Canon, "pubfig"),
        menu(ScopeRef::Canon, None),
        menu(view("work"), Some("guide")),
    ];
    let mut subs = Vec::new();
    for t in &active {
        let b = sh.site.refresh(&sh, t).unwrap();
        let page = b.live.revs.keys().next().expect("a page").clone();
        subs.push(sub(&sh, t, &page, &b));
    }
    assert_eq!(sh.site.streams(), active.len());
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let lines = Arc::clone(&lines);
        sh.site.set_log_hook(Some(Box::new(move |l: &str| {
            lines.lock().unwrap().push(l.to_string())
        })));
    }
    let logged = || {
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| **l == format!("site: /~profile/guide/: warning: {HISTORY_WARNING}"))
            .count()
    };
    sh.site.set_history_ceiling(Some(0));
    let guide = profile(ScopeRef::Canon, "guide");
    let first = sh.site.refresh(&sh, &guide).unwrap();
    assert_eq!(first.live.warnings, vec![HISTORY_WARNING.to_string()]);
    assert!(
        sh.site.history_evaluations(&guide).is_some(),
        "the ninth target keeps its slot and memo"
    );
    let builds = sh.site.builds();
    for _ in 0..3 {
        let again = sh.site.refresh(&sh, &guide).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "the build is reused");
        let page = get(&w, "~profile/guide/");
        assert_eq!(page.status, 200, "{}", page.head);
    }
    assert_eq!(sh.site.builds(), builds, "no cold rebuild");
    assert!(sh.site.history_evaluations(&guide).is_some());
    assert_eq!(logged(), 1, "logged once while the condition persists");
    // the open targets keep theirs too
    for t in &active {
        let b = sh.site.refresh(&sh, t).unwrap();
        assert!(!b.live.files.is_empty());
    }
    assert_eq!(sh.site.builds(), builds, "no open target rebuilt");
    sh.site.set_log_hook(None);
    drop(subs);
}

// --- 5. redaction races -------------------------------------------------------------------------

#[test]
fn spv6_profile_redaction_wins_publish_and_subscribe_races() {
    let w = world();
    let sh = shared(&w.h);
    let other = profile(ScopeRef::Canon, "other");

    // a page build of the profile reads the state before a removal and pauses at its check
    let p = request(&sh, &w.ids["$sb"], "redact-1");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, other.clone());
    let held = {
        let (sh, other) = (Arc::clone(&sh), other.clone());
        std::thread::spawn(move || {
            sh.site
                .refresh(&sh, &other)
                .map(|b| (b.live.epoch, holds(&b, "SECRET-B")))
        })
    };
    at.recv_timeout(T).expect("the build reaches its barrier");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    go.send(()).unwrap();
    match held.join().unwrap() {
        Err(e) => assert_eq!(e.code, E_REDACTED_STATE, "{e}"),
        Ok(got) => panic!("an old-epoch profile build published: {got:?}"),
    }
    sh.site.set_before_publish(None);
    let b = sh.site.refresh(&sh, &other).unwrap();
    assert_eq!(b.live.epoch, epoch(&sh));
    assert!(!holds(&b, "SECRET-B") && holds(&b, STUB));
    let page = get(&w, "~profile/other/secret.html");
    assert_eq!(page.status, 200, "{}", page.head);
    assert!(!text(&page).contains("SECRET-B") && text(&page).contains(STUB));

    // a contextual menu build reads the state before a removal and pauses at its check
    let cx = menu(ScopeRef::Canon, Some("other"));
    sh.site.refresh(&sh, &cx).unwrap();
    let p = request(&sh, &w.ids["$sa"], "redact-2");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, cx.clone());
    let held = {
        let (sh, cx) = (Arc::clone(&sh), cx.clone());
        std::thread::spawn(move || sh.site.refresh(&sh, &cx).map(|b| b.live.epoch))
    };
    at.recv_timeout(T).expect("the build reaches its barrier");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    assert!(epoch(&sh) > before);
    go.send(()).unwrap();
    match held.join().unwrap() {
        Err(e) => assert_eq!(e.code, E_REDACTED_STATE, "{e}"),
        Ok(at) => panic!("a menu of epoch {at} published after the removal"),
    }
    sh.site.set_before_publish(None);
    assert_eq!(sh.site.refresh(&sh, &cx).unwrap().live.epoch, epoch(&sh));
    assert_eq!(get(&w, "~versions/?profile=other").status, 200);

    // a subscribe whose refresh built the old state, held before its stream registers while a
    // removal lands, still gets the refresh: the old build is never compared as current
    let site = LiveSite::new();
    let old = site.refresh(&sh, &other).unwrap();
    assert!(holds(&old, "SECRET-C"));
    let page = "secret.html".to_string();
    let (rev, label_rev) = (old.live.revs[&page].clone(), old.live.label_rev.clone());
    let p = request(&sh, &w.ids["$sc"], "redact-3");
    let (at, go) = hold_after_refresh(&site, other.clone());
    let opening = {
        let (sh, site, other) = (Arc::clone(&sh), Arc::clone(&site), other.clone());
        std::thread::spawn(move || site.subscribe(&sh, &other, &page, &rev, &label_rev))
    };
    at.recv_timeout(T).expect("the build reaches its barrier");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    assert!(sh.state.load().ensure_current_epoch().is_ok());
    assert_eq!(site.step(&sh), 0, "no open stream: the step builds nothing");
    site.set_after_subscribe_refresh(None);
    go.send(()).unwrap();
    let mut s = opening.join().unwrap().expect("the stream opens");
    assert_eq!(s.rx.try_recv().ok(), Some(LiveEvent::Reload));
    let b = site.refresh(&sh, &other).unwrap();
    assert_eq!(b.live.epoch, epoch(&sh));
    assert!(!holds(&b, "SECRET-C"));

    // and an open page of the profile hears of a removal through the existing refresh event
    let page = get(&w, "~profile/other/secret.html");
    let mut stream = Sse::open(&http::events_url(&w.url, &text(&page)));
    let ip = w.ids["$ip"].clone();
    let p = request(&sh, &ip, "redact-4");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    loop {
        let Some((name, data)) = stream.next_event(T) else {
            panic!("no reload after the removal");
        };
        assert!(!data.contains("INTRO-CANON"), "{data}");
        if name == "reload" {
            assert_eq!(data, "{}");
            break;
        }
    }
}

// --- 6. blocked public profiles -----------------------------------------------------------------

#[test]
fn spv6_blocked_public_live_serves_while_output_refuses() {
    let w = world();
    let sh = shared(&w.h);
    let e = sh.engine();
    for (key, page, figure) in [
        ("other", "secret.html", false),
        ("pubfig", "figdoc.html", true),
    ] {
        let got = get(&w, &format!("~profile/{key}/{page}"));
        assert_eq!(got.status, 200, "{key}: {}", got.head);
        let b = sh
            .site
            .refresh(&sh, &profile(ScopeRef::Canon, key))
            .unwrap();
        assert_eq!(
            b.live.warnings,
            vec![format!(
                "Public profile has 1 references outside its selection. {PUBLIC_FIX}"
            )],
            "{key}"
        );
        let served: Vec<&String> = b
            .live
            .files
            .iter()
            .filter(|(_, bytes)| bytes.as_slice() == w.fig_png.as_slice())
            .map(|(p, _)| p)
            .collect();
        assert_eq!(!served.is_empty(), figure, "{key}: {served:?}");
        for path in served {
            let f = get(&w, &format!("~profile/{key}/{path}"));
            assert_eq!(f.status, 200, "{key}: {path}");
            assert_eq!(f.body, w.fig_png);
        }
        // every durable output still refuses, before it creates anything
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(format!("{key}-site"));
        let err = e
            .render(RenderRequest {
                out: out.clone(),
                scope: None,
                profile: Some(key.into()),
                view: None,
            })
            .unwrap_err();
        assert_eq!(err.code, E_PUBLICATION_REFERENCE, "{key}: {err}");
        assert!(!out.exists(), "{key}: render");
        for (format, name) in [("pdf", "out.pdf"), ("html", "out.html"), ("md", "out-md")] {
            let out = dir.path().join(format!("{key}-{name}"));
            let err = e
                .export(ExportRequest {
                    out: out.clone(),
                    format: format.into(),
                    layout: None,
                    scope: None,
                    profile: Some(key.into()),
                    view: None,
                })
                .unwrap_err();
            assert_eq!(err.code, E_PUBLICATION_REFERENCE, "{key} {format}: {err}");
            assert_eq!(err.fix, PUBLIC_FIX, "{key} {format}");
            assert!(!out.exists(), "{key} {format}");
        }
    }
}
