//! sod2 (D-F44G6, refined by D-WYZ97 and D-SMP5A): review targets on the live site render on
//! request. A view, a proposal version or a profile route under one keeps a detached
//! preparation of one captured state and the pages a request asked for, each with its own
//! recency, in one ledger with the eager builds of canon and tags. Under pressure the review
//! pages go first, least recently used across every target, then the shared parts of review
//! targets that keep no page; canon and tags are never dropped for them. A write drops a review
//! target's pages, and only pages with an open stream render again. A link runs one complete
//! warnings pass per kept state, and the warnings logged for a target go with the whole target.
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{ScopeRef, SiteTarget};
use rdy_daemon::site::live::*;
use rdy_engine::engine::write_typed;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{E_BAD_INPUT, E_REDACTED_STATE, Rdy, Scope};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;

/// The deadline of every wait for an event or a barrier.
const T: Duration = Duration::from_secs(10);
/// The scope-collision warning of a top-level doc named `glossary` beside generated terms.
const GLOSSARY: &str = "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it.";
/// What a removed block shows.
const STUB: &str = "Content removed.";

// --- the world ----------------------------------------------------------------------------------

/// Canon holds a folder `manual` (doc `intro`, one prose block), the docs `alpha` (two prose
/// blocks) and `beta` (one), and a doc `admin` that holds the org profile `guide`, which
/// selects the folder. Views and proposals are written by each test.
struct World {
    h: Hosted,
    url: String,
    ids: BTreeMap<String, String>,
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

fn world() -> World {
    let (dir, mut r) = tiny();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-CANON"},
            {"op": "create", "type": "doc", "id": "$alpha", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$a1", "parent": "$alpha", "body": "Alpha first."},
            {"op": "create", "type": "prose", "id": "$a2", "parent": "$alpha", "body": "Alpha second."},
            {"op": "create", "type": "doc", "id": "$beta", "fields": {"name": "beta", "title": "Beta", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$b1", "parent": "$beta", "body": "Beta first."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    let (admin, manual) = (ids["$admin"].clone(), ids["$manual"].clone());
    ids.extend(put(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "id": "$guide", "parent": admin,
            "fields": {"key": "guide", "title": "Guide book", "description": "About it.",
                "audience": "org", "select": [manual]}}]),
    ));
    land(&mut r, "admin").unwrap();
    drop(r);
    let h = Hosted::start_in(dir);
    let url = shared(&h)
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    World { h, url, ids }
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

/// Writes `ops` into view `view` on the daemon's writer; the created ids by `$name`.
fn commit(sh: &Arc<Shared>, view: &'static str, ops: Value) -> BTreeMap<String, String> {
    let v = on_writer(sh, move |r| put(r, view, ops));
    serde_json::from_value(v).unwrap()
}

/// Writes into view `view` a top-level doc `note-<view>` titled `Note <view>` of one prose
/// block `body`; the doc's id.
fn note(sh: &Arc<Shared>, view: &'static str, body: &str) -> String {
    commit(
        sh,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("note-{view}"), "title": format!("Note {view}"), "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": body}
        ]),
    )["$d"]
        .clone()
}

/// Writes into view `view` the two warnings of a review target: a top-level doc named
/// `glossary` beside a term (a preparation's warning), and a doc `broken-<view>` titled
/// `Broken <view>` whose diagram does not parse (a page's warning, its own words per view).
/// The diagram's id.
fn warnings_in(sh: &Arc<Shared>, view: &'static str) -> String {
    commit(
        sh,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Words"}},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Hidden"}, "body": "Kept apart."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": format!("broken-{view}"), "title": format!("Broken {view}"), "doc_type": "page"}},
            {"op": "create", "type": "diagram", "id": "$dg", "parent": "$b", "fields": {"lang": "mermaid", "alt": "broken"}, "body": "flowchart LR\n H[broken"}
        ]),
    )["$dg"]
        .clone()
}

/// The diagram warning of view `view`'s broken doc, whose diagram is `id`.
fn diagram_warning(sh: &Arc<Shared>, view: &str, id: &str) -> String {
    let t = src(view);
    let f = sh.site.review_facts(sh, &t).unwrap();
    let prefix = format!("Broken {view} ({id}): the Mermaid diagram does not parse");
    f.warnings
        .iter()
        .find(|w| w.starts_with(&prefix))
        .unwrap_or_else(|| panic!("{view}: no diagram warning in {:?}", f.warnings))
        .clone()
}

/// Proposes view `view` with a summary; the version's ref.
fn propose(sh: &Arc<Shared>, view: &'static str) -> String {
    let v = on_writer(sh, move |r| {
        r.propose(&writer(), view, view, Some("A test summary."))
            .unwrap()
            .r#ref
    });
    v.as_str().unwrap().to_string()
}

fn src(view: &str) -> SiteTarget {
    SiteTarget::Source(ScopeRef::View(view.into()))
}

fn prop(r: &str) -> SiteTarget {
    SiteTarget::Source(ScopeRef::Proposal(r.into()))
}

/// The `guide` profile route under view `view`.
fn guide(view: &str) -> SiteTarget {
    SiteTarget::Profile {
        scope: ScopeRef::View(view.into()),
        key: "guide".into(),
    }
}

/// The file `path` of review target `t` as the server sends it.
#[track_caller]
fn page(sh: &Arc<Shared>, t: &SiteTarget, path: &str) -> ReviewPage {
    sh.site
        .review_page(sh, t, path)
        .unwrap_or_else(|e| panic!("{t:?} {path}: {e}"))
        .unwrap_or_else(|| panic!("{t:?} has no {path}"))
}

/// A stream of `path` of `t` that has the page as `p` served it.
fn sub(sh: &Arc<Shared>, t: &SiteTarget, path: &str, p: &ReviewPage) -> Subscription {
    let rev = p.rev.as_deref().expect("a page has a revision");
    sh.site
        .subscribe(sh, t, path, rev, &p.label_rev)
        .unwrap_or_else(|_| panic!("the site is busy"))
}

fn empty(s: &mut Subscription) -> bool {
    s.rx.try_recv() == Err(TryRecvError::Empty)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The kept review pages, least recently used first.
fn kept(sh: &Shared) -> Vec<(SiteTarget, String)> {
    sh.site
        .ledger()
        .kept
        .into_iter()
        .map(|(t, p, _)| (t, p))
        .collect()
}

/// What the kept page `path` of `t` counts.
#[track_caller]
fn charge(sh: &Shared, t: &SiteTarget, path: &str) -> u64 {
    sh.site
        .ledger()
        .kept
        .into_iter()
        .find(|(kt, kp, _)| kt == t && kp == path)
        .unwrap_or_else(|| panic!("{t:?} {path} is not kept"))
        .2
}

/// Every line the site logs from now on.
fn collect(sh: &Shared) -> Arc<Mutex<Vec<String>>> {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let into = Arc::clone(&lines);
    sh.site.set_log_hook(Some(Box::new(move |l: &str| {
        into.lock().unwrap().push(l.to_string())
    })));
    lines
}

fn lines_of(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines.lock().unwrap().clone()
}

/// The log line of warning `w` of target `t`.
fn line(t: &SiteTarget, w: &str) -> String {
    format!("site: {}: warning: {w}", label(t))
}

/// Records a removal request of block `target` in view `view` and proposes it; the proposal id.
fn request_removal(sh: &Arc<Shared>, target: &str, view: &'static str) -> String {
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

/// A barrier at the publication check of every build or review admission of `target` on `site`.
fn hold(site: &LiveSite, target: SiteTarget) -> (Receiver<()>, Sender<()>) {
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

/// A barrier in each subscribe of `target` on `site`, after its render and before its stream
/// registers.
fn hold_after_refresh(site: &LiveSite, target: SiteTarget) -> (Receiver<()>, Sender<()>) {
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

/// Waits for the next event of `s`, up to [`T`].
fn next(s: &mut Subscription) -> Option<LiveEvent> {
    Hosted::rt()
        .block_on(async { tokio::time::timeout(T, s.rx.recv()).await })
        .ok()
        .flatten()
}

/// The bytes eager build `b` counts in the ledger: each file's bytes and path, and 64 per file.
fn counted(b: &SiteBuild) -> u64 {
    b.live
        .files
        .iter()
        .map(|(k, v)| k.len() as u64 + v.len() as u64 + 64)
        .sum()
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

/// The response headers of `r` but its date.
fn headers(r: &http::Response) -> Vec<String> {
    r.head
        .lines()
        .filter(|l| !l.to_ascii_lowercase().starts_with("date:"))
        .map(str::to_string)
        .collect()
}

// --- 1. the cap and global recency -------------------------------------------------------------

#[test]
fn sod2_pages_obey_the_cap_and_global_lru() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", "View a.");
    note(&sh, "b", "View b.");
    note(&sh, "c", "View c.");
    let pref = propose(&sh, "c");
    let (a, b, p, g) = (src("a"), src("b"), prop(&pref), guide("a"));
    // every page the test asks for, its charge measured under the default cap
    let asks: Vec<(SiteTarget, &str)> = vec![
        (a.clone(), "index.html"),
        (b.clone(), "index.html"),
        (p.clone(), "index.html"),
        (g.clone(), "index.html"),
        (a.clone(), "alpha.html"),
        (b.clone(), "beta.html"),
        (p.clone(), "alpha.html"),
        (g.clone(), "manual/intro.html"),
    ];
    let mut charges = BTreeMap::new();
    for (t, path) in &asks {
        page(&sh, t, path);
        charges.insert((t.clone(), path.to_string()), charge(&sh, t, path));
    }
    let l = sh.site.ledger();
    assert_eq!(l.eager, 0, "review targets only");
    assert_eq!(l.kept.len(), asks.len());
    assert_eq!(l.prepared.len(), 4, "{:?}", l.prepared);
    assert_eq!(
        l.total,
        l.shared + l.warnings + charges.values().sum::<u64>(),
        "the ledger counts the shared parts and each page"
    );
    let base = l.shared + l.warnings;
    // room for the shared parts and the three largest pages
    let mut sizes: Vec<u64> = charges.values().copied().collect();
    sizes.sort_unstable();
    let cap = base + sizes.iter().rev().take(3).sum::<u64>();
    // the model: the kept pages in recency order, dropped oldest first past the cap
    let mut model: Vec<(SiteTarget, String)> = kept(&sh);
    let fit = |model: &mut Vec<(SiteTarget, String)>| {
        let mut victims = vec![];
        while base + model.iter().map(|k| charges[k]).sum::<u64>() > cap {
            victims.push(model.remove(0));
        }
        victims
    };
    let check = |model: &Vec<(SiteTarget, String)>, what: &str| {
        let l = sh.site.ledger();
        assert_eq!(&kept(&sh), model, "{what}");
        assert_eq!(
            l.total,
            base + model.iter().map(|k| charges[k]).sum::<u64>(),
            "{what}: the bytes held"
        );
        assert!(l.total <= cap, "{what}");
        assert_eq!(
            (l.shared, l.prepared.len()),
            (base - l.warnings, 4),
            "{what}"
        );
    };
    sh.site.set_inactive_build_cap(Some(cap));
    let victims = fit(&mut model);
    assert!(!victims.is_empty(), "the cap drops pages");
    check(&model, "the new cap");
    // an open page is a candidate like any other
    let open_key = model[0].clone();
    let renders = sh.site.page_renders();
    let opened = page(&sh, &open_key.0, &open_key.1);
    let mut stream = sub(&sh, &open_key.0, &open_key.1, &opened);
    assert_eq!(sh.site.page_renders(), renders, "both were hits");
    let at = model.iter().position(|k| *k == open_key).unwrap();
    let k = model.remove(at);
    model.push(k);
    check(&model, "the open page touched");
    // touch the oldest page, then ask a page of another target than the victim's
    for round in 0..asks.len() {
        let oldest = model[0].clone();
        let renders = sh.site.page_renders();
        page(&sh, &oldest.0, &oldest.1);
        assert_eq!(sh.site.page_renders(), renders, "a hit renders nothing");
        let at = model.iter().position(|k| *k == oldest).unwrap();
        let k = model.remove(at);
        model.push(k);
        check(&model, &format!("round {round}: touched"));
        // the next page not kept, of another target than the next victim's
        let victim = model[0].clone();
        let (t, path) = asks
            .iter()
            .map(|(t, p)| (t.clone(), p.to_string()))
            .find(|k| !model.contains(k) && k.0 != victim.0)
            .expect("a page of another target");
        page(&sh, &t, &path);
        assert_eq!(sh.site.page_renders(), renders + 1, "a miss renders once");
        model.push((t.clone(), path.clone()));
        let victims = fit(&mut model);
        assert!(
            victims.contains(&victim),
            "round {round}: the least recently used page goes first: {victims:?}"
        );
        assert!(
            victims.iter().all(|v| v.0 != t || v.1 != path),
            "round {round}: the new page stays"
        );
        assert_ne!(victim.0, t, "round {round}: the eviction crosses targets");
        check(&model, &format!("round {round}: asked {t:?} {path}"));
    }
    assert!(
        !model.contains(&open_key) || empty(&mut stream),
        "an open page that stays hears nothing"
    );
    assert_eq!(sh.site.streams(), 1, "eviction closes no stream");
    assert_eq!(sh.site.review_builds(), 0);
    drop(stream);
}

// --- 2. a page larger than the cap -------------------------------------------------------------

#[test]
fn sod2_oversize_page_is_served_and_not_kept() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", &"Long words. ".repeat(20_000));
    let a = src("a");
    let first = page(&sh, &a, "note-a.html");
    let size = charge(&sh, &a, "note-a.html");
    let alpha = page(&sh, &a, "alpha.html");
    let mut stream = sub(&sh, &a, "alpha.html", &alpha);
    sh.site.set_inactive_build_cap(Some(size - 1));
    let renders = sh.site.page_renders();
    for n in 1..=2 {
        let again = page(&sh, &a, "note-a.html");
        assert_eq!(again.bytes, first.bytes, "request {n}: the same bytes");
        assert_eq!(sh.site.page_renders(), renders + n, "request {n} renders");
        assert!(
            !kept(&sh).iter().any(|(_, p)| p == "note-a.html"),
            "request {n}: a page past the cap is not kept"
        );
        assert!(sh.site.ledger().total < size);
        assert_eq!(sh.site.streams(), 1, "request {n}: the stream stays");
        assert!(empty(&mut stream));
    }
    // the stream still compares: an edit of alpha in the view reloads it
    let a2 = w.ids["$a2"].clone();
    commit(
        &sh,
        "a",
        json!([{"op": "update", "id": a2, "body": "Alpha edited."}]),
    );
    sh.site.step(&sh);
    assert_eq!(next(&mut stream), Some(LiveEvent::Reload));
    assert_eq!(sh.site.review_builds(), 0);
}

// --- 3. shared state and assets ----------------------------------------------------------------

#[test]
fn sod2_shared_state_and_assets_are_charged() {
    let w = world();
    let sh = shared(&w.h);
    // a view with large navigation, selected text and review data, and one tiny doc
    const WORDS: usize = 64 * 1024;
    let mut ops = vec![];
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{i}"), "fields": {"name": format!("doc{i}"), "title": format!("Document number {i}"), "doc_type": "page"}}));
        if i < 4 {
            ops.push(json!({"op": "create", "type": "prose", "parent": format!("$d{i}"), "body": format!("{i}").repeat(WORDS)}));
        }
    }
    ops.push(json!({"op": "create", "type": "doc", "id": "$t", "fields": {"name": "tiny", "title": "Tiny", "doc_type": "page"}}));
    ops.push(json!({"op": "create", "type": "prose", "parent": "$t", "body": "Small."}));
    let a1 = w.ids["$a1"].clone();
    ops.push(json!({"op": "update", "id": a1, "body": "Review data. ".repeat(4096)}));
    commit(&sh, "big", Value::Array(ops));
    let big = src("big");
    let small = page(&sh, &big, "tiny.html");
    let out = charge(&sh, &big, "tiny.html");
    let l = sh.site.ledger();
    assert!(
        l.shared >= 4 * WORDS as u64,
        "the estimate holds the selected text: {}",
        l.shared
    );
    assert!(
        l.shared >= 20 * out && l.shared >= 20 * small.bytes.len() as u64,
        "the preparation dominates the tiny output: {} against {out}",
        l.shared
    );
    let alone = sh.site.prepared_bytes(&big).unwrap();
    assert_eq!(l.shared, alone, "one target: its whole estimate");
    // unrelated views hold large state; a preparation of the same target state reads none of it
    for v in ["u1", "u2"] {
        commit(
            &sh,
            v,
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("{v}doc"), "title": "Unrelated", "doc_type": "page"}},
                {"op": "create", "type": "prose", "parent": "$d", "body": "U".repeat(4 * WORDS)}
            ]),
        );
    }
    let site = LiveSite::new();
    let weak_state = Arc::downgrade(&sh.state.load_full());
    site.review_page(&sh, &big, "tiny.html").unwrap().unwrap();
    assert_eq!(
        site.prepared_bytes(&big),
        Some(alone),
        "neither the unrelated views nor the whole read state is retained"
    );
    // a later publish replaces the state; no target keeps the one it read
    note(&sh, "u3", "Later.");
    sh.site.step(&sh);
    let until = std::time::Instant::now() + T;
    while weak_state.strong_count() > 0 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(weak_state.strong_count(), 0, "no target keeps a read state");
    drop(site);

    // two targets that own the same shared allocations: the latest alias and its version
    note(&sh, "pv", "Proposed.");
    let pref = propose(&sh, "pv");
    let stable = pref.split('@').next().unwrap().to_string();
    let (v1, latest) = (prop(&pref), prop(&stable));
    page(&sh, &v1, "index.html");
    page(&sh, &latest, "index.html");
    let (x, y) = (
        sh.site.prepared_bytes(&v1).unwrap(),
        sh.site.prepared_bytes(&latest).unwrap(),
    );
    let both = sh.site.ledger().shared - alone;
    assert!(
        both < x + y,
        "a shared allocation counts once: {both} < {x} + {y}"
    );
    assert!(
        both > x.max(y),
        "each target's own parts count apart: {both}"
    );
    // a second version moves only the alias: its own parts go, the shared charge stays
    let weak_latest = sh.site.prepared_handle(&latest).unwrap();
    let weak_v1 = sh.site.prepared_handle(&v1).unwrap();
    commit(
        &sh,
        "pv",
        json!([{"op": "update", "id": w.ids["$b1"].clone(), "body": "Beta proposed."}]),
    );
    let p2 = propose(&sh, "pv");
    assert!(p2.ends_with("@2"), "{p2}");
    sh.site.step(&sh);
    assert_eq!(
        weak_latest.strong_count(),
        0,
        "the alias's old preparation is released"
    );
    assert!(weak_v1.strong_count() > 0, "version 1 keeps its own");
    assert_eq!(
        sh.site.ledger().shared - alone,
        x,
        "only version 1's preparation counts"
    );
    // a landing moves version 1's inputs too: its state goes before anything new is admitted
    let ip = w.ids["$ip"].clone();
    on_writer(&sh, move |r| {
        put(
            r,
            "edit",
            json!([{"op": "update", "id": ip, "body": "INTRO-EDITED"}]),
        );
        land(r, "edit").unwrap();
    });
    sh.site.step(&sh);
    assert_eq!(
        weak_v1.strong_count(),
        0,
        "version 1's old preparation is released"
    );
    assert!(!sh.site.ledger().prepared.contains(&v1));
    // a full drop, with an open stream that keeps only its comparison facts
    let p = page(&sh, &v1, "index.html");
    let mut stream = sub(&sh, &v1, "index.html", &p);
    let weak = sh.site.prepared_handle(&v1).unwrap();
    sh.site.set_inactive_build_cap(Some(0));
    assert_eq!(
        weak.strong_count(),
        0,
        "a full drop releases the preparation"
    );
    let l = sh.site.ledger();
    assert_eq!((l.total, l.kept.len(), l.prepared.len()), (0, 0, 0));
    assert_eq!(sh.site.streams(), 1);
    sh.site.set_inactive_build_cap(None);
    page(&sh, &v1, "index.html");
    assert!(
        empty(&mut stream),
        "the stream compares with what it opened at"
    );

    // a figure file is a kept page of its own, counted at its length
    let png = {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend((0..256 * 1024u32).map(|n| n as u8));
        png
    };
    let blob = on_writer(&sh, {
        let png = png.clone();
        move |r| r.store_mut().put_blob(&png).unwrap().to_string()
    });
    let blob = blob.as_str().unwrap().to_string();
    commit(
        &sh,
        "fig",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figs", "title": "Figs", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": blob.clone(), "media_type": "image/png", "alt": "A figure"}}
        ]),
    );
    let fig = src("fig");
    let path = format!("assets/{blob}.png");
    let served = page(&sh, &fig, &path);
    assert_eq!(served.bytes, png, "a direct asset request");
    assert!(charge(&sh, &fig, &path) >= png.len() as u64);
    assert_eq!(sh.site.review_builds(), 0);
}

// --- 4. writes ---------------------------------------------------------------------------------

#[test]
fn sod2_write_drops_all_pages_and_renders_only_open_pages() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", "View a.");
    let a = src("a");
    for path in ["index.html", "beta.html"] {
        page(&sh, &a, path);
    }
    let alpha = page(&sh, &a, "alpha.html");
    let mut stream = sub(&sh, &a, "alpha.html", &alpha);
    assert_eq!(kept(&sh).len(), 3);
    let renders = sh.site.page_renders();
    let a2 = w.ids["$a2"].clone();
    commit(
        &sh,
        "a",
        json!([{"op": "update", "id": a2, "body": "Alpha edited."}]),
    );
    sh.site.step(&sh);
    assert_eq!(next(&mut stream), Some(LiveEvent::Reload));
    assert_eq!(
        sh.site.page_renders(),
        renders + 1,
        "only the open page renders"
    );
    assert_eq!(
        kept(&sh),
        vec![(a.clone(), "alpha.html".to_string())],
        "the write dropped every page; the open one came back"
    );
    // the others render when asked, from the new state
    let beta = page(&sh, &a, "beta.html");
    assert_eq!(sh.site.page_renders(), renders + 2);
    assert!(text(&beta.bytes).contains("Beta first."));
    let again = page(&sh, &a, "alpha.html");
    assert!(text(&again.bytes).contains("Alpha edited."));
    assert_eq!(sh.site.page_renders(), renders + 2, "the open page is kept");
    // a page of a target with no stream: dropped by the worker, rendered on its next request
    note(&sh, "b", "View b.");
    let b = src("b");
    page(&sh, &b, "beta.html");
    let renders = sh.site.page_renders();
    note(&sh, "b2", "unrelated");
    commit(
        &sh,
        "b",
        json!([{"op": "update", "id": w.ids["$b1"].clone(), "body": "Beta in b."}]),
    );
    sh.site.step(&sh);
    assert!(
        !kept(&sh).iter().any(|(t, _)| *t == b),
        "a closed page's cache is dropped too"
    );
    assert_eq!(
        sh.site.page_renders(),
        renders,
        "and nothing renders for it"
    );
    assert!(text(&page(&sh, &b, "beta.html").bytes).contains("Beta in b."));
    assert_eq!(sh.site.review_builds(), 0);
}

// --- 5. a landing ------------------------------------------------------------------------------

#[test]
fn sod2_landing_never_builds_a_whole_review_target() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", "View a.");
    note(&sh, "b", "View b.");
    note(&sh, "c", "View c.");
    let pref = propose(&sh, "c");
    let open = [
        (src("a"), "alpha.html"),
        (src("a"), "index.html"),
        (src("b"), "alpha.html"),
        (prop(&pref), "alpha.html"),
        (guide("a"), "manual/intro.html"),
    ];
    let mut streams = vec![];
    for (t, path) in &open {
        let p = page(&sh, t, path);
        streams.push(sub(&sh, t, path, &p));
    }
    // a second stream on one page shares its render
    let p = page(&sh, &src("a"), "alpha.html");
    streams.push(sub(&sh, &src("a"), "alpha.html", &p));
    let (renders, builds) = (sh.site.page_renders(), sh.site.builds());
    // a real landing, proposed with its summary, on the daemon's writer; the worker runs
    let a1 = w.ids["$a1"].clone();
    on_writer(&sh, move |r| {
        put(
            r,
            "edit",
            json!([{"op": "update", "id": a1, "body": "Alpha landed."}]),
        );
        land(r, "edit").unwrap();
    });
    // the views' alpha pages show canon's alpha: the worker reloads them
    for i in [0, 2, 5] {
        assert_eq!(next(&mut streams[i]), Some(LiveEvent::Reload), "stream {i}");
    }
    // the worker's pass is done once this one finds nothing left
    sh.site.step(&sh);
    assert_eq!(sh.site.review_builds(), 0, "no whole review build");
    assert_eq!(sh.site.builds(), builds, "no eager build either");
    assert_eq!(
        sh.site.page_renders(),
        renders + open.len() as u64,
        "each open page rendered once"
    );
    assert!(empty(&mut streams[3]), "a proposal version holds still");
    drop(streams);
}

// --- 6. streams after eviction -----------------------------------------------------------------

#[test]
fn sod2_streams_compare_opened_bytes_after_eviction() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", "View a.");
    // alpha is already changed in the view: a second change of it moves no other page
    let a1 = w.ids["$a1"].clone();
    commit(
        &sh,
        "a",
        json!([{"op": "update", "id": a1, "body": "Alpha first in a."}]),
    );
    let a = src("a");
    let beta = page(&sh, &a, "beta.html");
    let alpha = page(&sh, &a, "alpha.html");
    let mut b1 = sub(&sh, &a, "beta.html", &beta);
    let mut b2 = sub(&sh, &a, "beta.html", &beta);
    let mut al = sub(&sh, &a, "alpha.html", &alpha);
    // nothing is kept from here on: every comparison reads a fresh render
    sh.site.set_inactive_build_cap(Some(1));
    assert_eq!(sh.site.ledger().total, 0);
    // another block of alpha: alpha reloads, beta's label and header move
    let renders = sh.site.page_renders();
    let a2 = w.ids["$a2"].clone();
    commit(
        &sh,
        "a",
        json!([{"op": "update", "id": a2, "body": "Alpha edited."}]),
    );
    sh.site.step(&sh);
    assert_eq!(next(&mut al), Some(LiveEvent::Reload));
    let now = sh.site.review_page(&sh, &a, "beta.html").unwrap().unwrap();
    let label = LiveEvent::Label {
        text: now.label.clone(),
        html: now.header.clone(),
    };
    assert!(
        now.label.ends_with("4 changes, not landed"),
        "{}",
        now.label
    );
    assert_eq!(next(&mut b1), Some(label.clone()));
    assert_eq!(next(&mut b2), Some(label.clone()));
    assert_eq!(
        sh.site.page_renders(),
        renders + 3,
        "beta's two streams shared one render (and the test's own request)"
    );
    assert!(empty(&mut b1) && empty(&mut b2) && empty(&mut al));
    // a reconnect with the old label hears the new one; with the current one it hears nothing
    let mut old = sub(&sh, &a, "beta.html", &beta);
    assert_eq!(old.rx.try_recv(), Ok(label));
    let mut stale = sub(&sh, &a, "alpha.html", &alpha);
    assert_eq!(
        stale.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "an old revision reloads"
    );
    drop(stale);
    let mut current = sub(&sh, &a, "beta.html", &now);
    assert!(empty(&mut current));
    drop((old, current));
    // the page is removed: its streams reload, once
    let (beta_doc, b1_id) = (w.ids["$beta"].clone(), w.ids["$b1"].clone());
    commit(
        &sh,
        "a",
        json!([{"op": "delete", "id": b1_id}, {"op": "delete", "id": beta_doc}]),
    );
    sh.site.step(&sh);
    assert_eq!(next(&mut b1), Some(LiveEvent::Reload));
    assert_eq!(next(&mut b2), Some(LiveEvent::Reload));
    assert!(
        sh.site.review_page(&sh, &a, "beta.html").unwrap().is_none(),
        "the page is gone"
    );
    // a write that changes neither page's bytes nor the label sends nothing
    sh.site.set_inactive_build_cap(None);
    let al_now = page(&sh, &a, "alpha.html");
    let mut al2 = sub(&sh, &a, "alpha.html", &al_now);
    note(&sh, "elsewhere", "Unrelated.");
    sh.site.step(&sh);
    assert!(empty(&mut al2), "an unrelated view's write");
    assert_eq!(sh.site.review_builds(), 0);
}

// --- 7. epochs and races -----------------------------------------------------------------------

#[test]
fn sod2_epochs_and_subscribe_races_discard_stale_pages() {
    let w = world();
    let sh = shared(&w.h);
    let ids = commit(
        &sh,
        "a",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "secret", "title": "Secret", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$s1", "parent": "$d", "body": "SECRET-ONE words."}
        ]),
    );
    on_writer(&sh, |r| land(r, "a").unwrap());
    note(&sh, "v", "View v.");
    let v = src("v");

    // a page render of the state before a removal pauses at its publication: it is discarded
    let p = request_removal(&sh, &ids["$s1"], "redact-1");
    let before = epoch(&sh);
    let (at, go) = hold(&sh.site, v.clone());
    let held = {
        let (sh, v) = (Arc::clone(&sh), v.clone());
        std::thread::spawn(move || {
            sh.site
                .review_page(&sh, &v, "secret.html")
                .map(|p| p.map(|p| p.bytes))
        })
    };
    at.recv_timeout(T).expect("the render reaches its barrier");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    assert!(epoch(&sh) > before, "the removal advanced the epoch");
    go.send(()).unwrap();
    match held.join().unwrap() {
        Err(e) => assert_eq!(e.code, E_REDACTED_STATE, "{e}"),
        Ok(got) => panic!("an old-epoch page was kept: {got:?}"),
    }
    sh.site.set_before_publish(None);
    assert!(kept(&sh).is_empty(), "nothing of the old epoch is kept");
    let now = page(&sh, &v, "secret.html");
    assert_eq!(now.epoch, epoch(&sh));
    assert!(!text(&now.bytes).contains("SECRET-ONE") && text(&now.bytes).contains(STUB));

    // an ordinary write while a render pauses: a new request never gets the paused state's page
    let (at, go) = hold(&sh.site, v.clone());
    let held = {
        let (sh, v) = (Arc::clone(&sh), v.clone());
        std::thread::spawn(move || {
            sh.site
                .review_page(&sh, &v, "note-v.html")
                .map(|p| p.map(|p| p.bytes))
        })
    };
    at.recv_timeout(T).expect("the render reaches its barrier");
    let n = commit(
        &sh,
        "v",
        json!([{"op": "create", "type": "doc", "id": "$x", "fields": {"name": "late", "title": "Late", "doc_type": "page"}}]),
    );
    assert!(n.contains_key("$x"));
    go.send(()).unwrap();
    sh.site.set_before_publish(None);
    let old = held.join().unwrap().unwrap().unwrap();
    assert!(
        !text(&old).contains("late.html"),
        "the paused request read the old state"
    );
    let fresh = page(&sh, &v, "note-v.html");
    assert!(
        text(&fresh.bytes).contains("late.html"),
        "a new request reads the new state"
    );

    // a subscribe whose render read the old state is held before it registers while a write
    // lands; the worker's step runs without it; once registered it is compared again
    let site = LiveSite::new();
    let opened = site.review_page(&sh, &v, "note-v.html").unwrap().unwrap();
    let (at, go) = hold_after_refresh(&site, v.clone());
    let opening = {
        let (sh, site, v) = (Arc::clone(&sh), Arc::clone(&site), v.clone());
        let (rev, label) = (opened.rev.clone().unwrap(), opened.label_rev.clone());
        std::thread::spawn(move || site.subscribe(&sh, &v, "note-v.html", &rev, &label))
    };
    at.recv_timeout(T)
        .expect("the subscribe reaches its barrier");
    commit(
        &sh,
        "v",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "page"}}]),
    );
    // the daemon's writer tells its own site; this one is told here
    site.published();
    assert_eq!(site.step(&sh), 0, "no stream yet: the step renders nothing");
    site.set_after_subscribe_refresh(None);
    go.send(()).unwrap();
    let mut s = opening.join().unwrap().expect("the stream opens");
    assert_eq!(
        s.rx.try_recv(),
        Ok(LiveEvent::Reload),
        "the new state is compared"
    );
    drop(s);

    // the same race across a removal: the old-epoch render is never compared as current
    let ids = commit(
        &sh,
        "v",
        json!([{"op": "create", "type": "prose", "id": "$s2", "parent": ids["$d"].clone(), "body": "SECRET-TWO words."}]),
    );
    let site = LiveSite::new();
    let opened = site.review_page(&sh, &v, "secret.html").unwrap().unwrap();
    assert!(text(&opened.bytes).contains("SECRET-TWO"));
    let p = request_removal(&sh, &ids["$s2"], "redact-2");
    let (at, go) = hold_after_refresh(&site, v.clone());
    let opening = {
        let (sh, site, v) = (Arc::clone(&sh), Arc::clone(&site), v.clone());
        let (rev, label) = (opened.rev.clone().unwrap(), opened.label_rev.clone());
        std::thread::spawn(move || site.subscribe(&sh, &v, "secret.html", &rev, &label))
    };
    at.recv_timeout(T)
        .expect("the subscribe reaches its barrier");
    on_writer(&sh, move |r| r.land(&approver(), &p).unwrap());
    site.published();
    site.set_after_subscribe_refresh(None);
    go.send(()).unwrap();
    let mut s = opening.join().unwrap().expect("the stream opens");
    assert_eq!(s.rx.try_recv(), Ok(LiveEvent::Reload));
    let after = site.review_page(&sh, &v, "secret.html").unwrap().unwrap();
    assert!(!text(&after.bytes).contains("SECRET-TWO"));
    assert_eq!(after.epoch, epoch(&sh));
    assert_eq!(sh.site.review_builds() + site.review_builds(), 0);
}

// --- 8. links ----------------------------------------------------------------------------------

/// `rdy_site` through an MCP session on the daemon, and the `site_url` op `rdy site` sends.
fn links(h: &Hosted, scope: &str) -> (Value, Value) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = h.client(None, None).await;
        let cli = c
            .ctl("site_url", json!({"scope": scope}))
            .await
            .unwrap_or_else(|e| panic!("site_url: {e:?}"));
        let (r, mut w) = rdy_daemon::sys::split(h.mcp_stream().await);
        let mut r = BufReader::new(r);
        let mut line = String::new();
        let send = |v: Value| format!("{v}\n");
        w.write_all(send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": rdy_qa::mcp::PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes()).await.unwrap();
        r.read_line(&mut line).await.unwrap();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        w.write_all(send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "rdy_session", "arguments": {"actor": "agent:m", "role": "reader"}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        w.write_all(send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "rdy_site", "arguments": {"scope": scope}}})).as_bytes()).await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        let body = v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{v}"));
        (cli, serde_json::from_str(body).unwrap())
    })
}

#[test]
fn sod2_link_checks_warn_once_without_caching_pages() {
    let w = world();
    let sh = shared(&w.h);
    let lines = collect(&sh);
    let dg = warnings_in(&sh, "g");
    let g = src("g");
    let (cli, mcp) = links(&w.h, "view:g");
    let warning = format!("Broken g ({dg}): the Mermaid diagram does not parse");
    let warnings = mcp["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("{mcp}"));
    assert_eq!(warnings.len(), 2, "{mcp}");
    assert_eq!(warnings[0], GLOSSARY, "the preparation's warning first");
    assert!(
        warnings[1].as_str().unwrap().starts_with(&warning),
        "then the diagram of a page nobody asked for: {mcp}"
    );
    assert_eq!(cli["warnings"], mcp["warnings"], "CLI and MCP agree");
    assert_eq!(cli["changes"], mcp["changes"]);
    assert_eq!(sh.site.checks(), 1, "the two links share one pass");
    assert_eq!(sh.site.page_renders(), 0, "a link renders no page");
    assert!(kept(&sh).is_empty(), "and keeps none");
    let found: Vec<String> = warnings
        .iter()
        .map(|w| line(&g, w.as_str().unwrap()))
        .collect();
    assert_eq!(lines_of(&lines), found, "each warning logged once");
    // repeated links share the pass while the target is kept
    for _ in 0..3 {
        let f = sh.site.review_facts(&sh, &g).unwrap();
        assert_eq!(json!(f.warnings), mcp["warnings"]);
    }
    assert_eq!((sh.site.checks(), sh.site.preparations()), (1, 1));
    // a state change needs a new pass, and logs nothing the target logged
    note(&sh, "g", "More words.");
    let (cli2, mcp2) = links(&w.h, "view:g");
    assert_eq!(sh.site.checks(), 2, "the new state's pass");
    assert_eq!(
        (cli2["warnings"].clone(), mcp2["warnings"].clone()),
        (mcp["warnings"].clone(), mcp["warnings"].clone())
    );
    assert_eq!(lines_of(&lines), found, "nothing logged again");
    assert_eq!(sh.site.page_renders(), 0);
    sh.site.set_log_hook(None);
}

// --- 9. one warning log ------------------------------------------------------------------------

#[test]
fn sod2_page_and_check_warnings_share_one_log() {
    let w = world();
    let sh = shared(&w.h);
    let lines = collect(&sh);
    let da = warnings_in(&sh, "a");
    let db = warnings_in(&sh, "b");
    let (a, b) = (src("a"), src("b"));
    // page first: the page logs its preparation's warning and its diagram's
    let broken = page(&sh, &a, "broken-a.html");
    assert_eq!(broken.epoch, epoch(&sh));
    assert_eq!(lines_of(&lines).len(), 2, "{:?}", lines_of(&lines));
    let wa = diagram_warning(&sh, "a", &da);
    let la = vec![line(&a, GLOSSARY), line(&a, &wa)];
    assert_eq!(
        lines_of(&lines),
        la,
        "the pass logs nothing the page logged"
    );
    // check first: the pass logs both; the page then logs nothing
    let wb = diagram_warning(&sh, "b", &db);
    page(&sh, &b, "broken-b.html");
    let lb = vec![line(&b, GLOSSARY), line(&b, &wb)];
    assert_eq!(lines_of(&lines), [la.clone(), lb.clone()].concat());
    // page eviction keeps the record: the page renders again and logs nothing
    let l = sh.site.ledger();
    sh.site.set_inactive_build_cap(Some(l.shared + l.warnings));
    assert!(kept(&sh).is_empty() && sh.site.ledger().prepared.len() == 2);
    let renders = sh.site.page_renders();
    page(&sh, &a, "broken-a.html");
    assert_eq!(sh.site.page_renders(), renders + 1);
    assert_eq!(lines_of(&lines), [la.clone(), lb.clone()].concat());
    sh.site.set_inactive_build_cap(None);
    // the diagram is fixed: a page render alone proves nothing gone
    let fix = |sh: &Arc<Shared>, body: &str| {
        commit(
            sh,
            "a",
            json!([{"op": "update", "id": da.clone(), "body": body}]),
        );
    };
    fix(&sh, "graph TD\n  A --> B");
    page(&sh, &a, "broken-a.html");
    assert!(
        sh.site.logged_warnings(&a).unwrap().contains(&wa),
        "a partial render cannot prove the warning gone"
    );
    fix(&sh, "flowchart LR\n H[broken");
    page(&sh, &a, "broken-a.html");
    assert_eq!(lines_of(&lines).len(), 4, "still logged: no new line");
    // a completed pass proves it gone; its return is logged again, once
    fix(&sh, "graph TD\n  A --> B");
    let f = sh.site.review_facts(&sh, &a).unwrap();
    assert_eq!(f.warnings, vec![GLOSSARY.to_string()]);
    assert_eq!(
        sh.site.logged_warnings(&a).unwrap(),
        vec![GLOSSARY.to_string()]
    );
    fix(&sh, "flowchart LR\n H[broken");
    page(&sh, &a, "broken-a.html");
    page(&sh, &a, "broken-a.html");
    sh.site.review_facts(&sh, &a).unwrap();
    assert_eq!(
        lines_of(&lines),
        [la.clone(), lb, vec![line(&a, &wa)]].concat(),
        "the return is logged once"
    );
    sh.site.set_log_hook(None);
}

// --- 10. HTTP ----------------------------------------------------------------------------------

#[test]
fn sod2_review_http_preserves_files_and_refusals() {
    let w = world();
    let sh = shared(&w.h);
    let png = b"\x89PNG\r\n\x1a\nFIGBYTES-review".to_vec();
    let blob = on_writer(&sh, {
        let png = png.clone();
        move |r| r.store_mut().put_blob(&png).unwrap().to_string()
    });
    let blob = blob.as_str().unwrap().to_string();
    commit(
        &sh,
        "a",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figs", "title": "Figs", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": blob.clone(), "media_type": "image/png", "alt": "A figure"}},
            {"op": "create", "type": "diagram", "parent": "$d", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "changes", "title": "Authored changes", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$c", "body": "AUTHORED-CHANGES"}
        ]),
    );
    let a = src("a");
    // the whole reference build belongs to the test alone
    let st = sh.state.load_full();
    let whole = rdy_render::live::build_live(&st, &Scope::View("a".into()), &a).unwrap();
    drop(st);
    // a direct asset first, before any page that refers to it
    let fig = format!("assets/{blob}.png");
    let r = get(&w, &format!("~view/a/{fig}"));
    assert_eq!((r.status, &r.body), (200, &png), "{}", r.head);
    let mut paths: Vec<&String> = whole.files.keys().collect();
    paths.sort_by_key(|p| *p != &fig);
    for path in paths {
        let want = rdy_render::live::page(&whole, path, &a).unwrap();
        let g = get(&w, &format!("~view/a/{path}"));
        assert_eq!(g.status, 200, "{path}: {}", g.head);
        assert!(g.body == want, "{path}: the whole build's bytes");
        let h = head(&w, &format!("~view/a/{path}"));
        assert_eq!((h.status, headers(&h)), (200, headers(&g)), "HEAD {path}");
        assert!(h.body.is_empty(), "HEAD {path}");
    }
    let changes = get(&w, "~view/a/changes.html");
    assert!(
        text(&changes.body).contains("AUTHORED-CHANGES"),
        "the authored page wins"
    );
    // themed misses, rooted at the target; a scope not on the site is canon's Scope variant
    for (path, sentence) in [
        ("~view/a/nosuch.html", "There is no page at this address."),
        (
            "~view/a/assets/nosuch.png",
            "There is no page at this address.",
        ),
        ("~view/nope/", "is not on the site now"),
        ("~proposal/P-0000000000000/", "is not on the site now"),
    ] {
        let g = get(&w, path);
        assert_eq!(g.status, 404, "{path}: {}", g.head);
        assert!(text(&g.body).contains(sentence), "{path}");
        assert!(
            !text(&g.body).contains("/_rdy/live.js"),
            "{path}: no live script"
        );
        let h = head(&w, path);
        assert_eq!((h.status, headers(&h)), (404, headers(&g)), "HEAD {path}");
    }
    let miss = get(&w, "~view/a/nosuch.html");
    assert!(text(&miss.body).contains("href=\"/~view/a/index.html\""));
    assert_eq!(sh.site.streams(), 0, "no miss opened a stream");
    assert!(
        !kept(&sh).iter().any(|(_, p)| p.contains("nosuch")),
        "no missing path is kept"
    );
    assert_eq!(sh.site.review_builds(), 0, "no whole review build");
    assert_eq!(
        sh.site.preparations(),
        1,
        "one preparation served every request"
    );
}

// --- 11. eager targets -------------------------------------------------------------------------

#[test]
fn sod2_eager_targets_keep_their_existing_path() {
    let w = world();
    let sh = shared(&w.h);
    on_writer(&sh, |r| {
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
    });
    let eager = [
        SiteTarget::Source(ScopeRef::Canon),
        SiteTarget::Profile {
            scope: ScopeRef::Canon,
            key: "guide".into(),
        },
        SiteTarget::Source(ScopeRef::Tag("v1.0.0".into())),
        SiteTarget::Profile {
            scope: ScopeRef::Tag("v1.0.0".into()),
            key: "guide".into(),
        },
        SiteTarget::Menu {
            scope: ScopeRef::Canon,
            profile: None,
        },
        SiteTarget::Menu {
            scope: ScopeRef::View("work".into()),
            profile: Some("guide".into()),
        },
        SiteTarget::ProfileOnly {
            key: "guide".into(),
        },
    ];
    note(&sh, "work", "Work.");
    for (i, t) in eager.iter().enumerate() {
        assert!(!is_review(t), "{t:?}");
        let builds = sh.site.builds();
        let b = sh
            .site
            .refresh(&sh, t)
            .unwrap_or_else(|e| panic!("{t:?}: {e}"));
        assert_eq!(sh.site.builds(), builds + 1, "{t:?}: one whole build");
        assert!(!b.live.files.is_empty());
        let again = sh.site.refresh(&sh, t).unwrap();
        assert!(Arc::ptr_eq(&b, &again), "{t:?}: the build is kept");
        assert_eq!(sh.site.held_builds(), i + 1);
        let err = sh.site.review_page(&sh, t, "index.html").unwrap_err();
        assert_eq!(err.code, E_BAD_INPUT, "{t:?}: no review entry");
    }
    // the eager routes serve their builds
    for (path, t) in [
        ("", &eager[0]),
        ("~profile/guide/", &eager[1]),
        ("~tag/v1.0.0/", &eager[2]),
    ] {
        let b = sh.site.refresh(&sh, t).unwrap();
        let g = get(&w, &format!("{path}index.html"));
        assert_eq!(g.status, 200);
        assert!(
            g.body == rdy_render::live::page(&b.live, "index.html", t).unwrap(),
            "{path}"
        );
    }
    assert_eq!(get(&w, "~versions/").status, 200);
    // a review target has no whole build
    let err = sh
        .site
        .refresh(&sh, &src("work"))
        .err()
        .expect("no whole review build");
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(
        (
            sh.site.preparations(),
            sh.site.page_renders(),
            sh.site.review_builds()
        ),
        (0, 0, 0),
        "no review work"
    );
}

// --- 12. one ledger ----------------------------------------------------------------------------

#[test]
fn sod2_eager_builds_and_review_entries_share_one_ledger() {
    let w = world();
    let sh = shared(&w.h);
    on_writer(&sh, |r| {
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
    });
    note(&sh, "a", "View a.");
    let canon = SiteTarget::Source(ScopeRef::Canon);
    let tag = SiteTarget::Source(ScopeRef::Tag("v1.0.0".into()));
    let bc = sh.site.refresh(&sh, &canon).unwrap();
    let bt = sh.site.refresh(&sh, &tag).unwrap();
    let eager = counted(&bc) + counted(&bt);
    drop((bc, bt));
    let a = src("a");
    page(&sh, &a, "index.html");
    page(&sh, &guide("a"), "index.html");
    sh.site.review_facts(&sh, &a).unwrap();
    let l = sh.site.ledger();
    assert_eq!(l.eager, eager, "the eager builds count as before");
    assert!(l.shared > 0 && l.pages > 0);
    assert_eq!(l.total, l.eager + l.shared + l.pages + l.warnings);
    assert_eq!(sh.site.held_bytes(), l.total, "one ledger");
    // under a cap below canon's build, the review parts go and the most recent eager build
    // stays past the cap: the eager canon exception, a count and not a memory bound
    sh.site.refresh(&sh, &canon).unwrap();
    let builds = sh.site.builds();
    sh.site.set_inactive_build_cap(Some(1));
    let l = sh.site.ledger();
    assert_eq!((l.pages, l.shared, l.warnings), (0, 0, 0));
    assert_eq!(sh.site.held_builds(), 1, "one eager build stays");
    assert!(l.total > 1, "past the cap");
    sh.site.refresh(&sh, &canon).unwrap();
    assert_eq!(sh.site.builds(), builds, "canon is the one kept");
    // a review request is still served, and keeps nothing
    let served = page(&sh, &a, "index.html");
    assert!(!served.bytes.is_empty());
    assert_eq!(
        sh.site.ledger().total,
        l.total,
        "the exact estimate after the request"
    );
}

// --- 13. pages before shared parts -------------------------------------------------------------

#[test]
fn sod2_review_pages_drop_before_shared_parts() {
    let w = world();
    let sh = shared(&w.h);
    note(&sh, "a", "View a.");
    note(&sh, "b", "View b.");
    let (a, b) = (src("a"), src("b"));
    page(&sh, &a, "index.html");
    page(&sh, &b, "index.html");
    page(&sh, &a, "alpha.html");
    page(&sh, &b, "beta.html");
    let l = sh.site.ledger();
    let (pa, pb) = (
        sh.site.prepared_bytes(&a).unwrap(),
        sh.site.prepared_bytes(&b).unwrap(),
    );
    // one byte short of every page: the oldest page goes, every preparation stays
    sh.site.set_inactive_build_cap(Some(l.total - 1));
    assert_eq!(
        kept(&sh),
        vec![
            (b.clone(), "index.html".to_string()),
            (a.clone(), "alpha.html".to_string()),
            (b.clone(), "beta.html".to_string())
        ]
    );
    assert_eq!(sh.site.ledger().prepared.len(), 2);
    // room for the shared parts alone: every page goes, from both targets, and no preparation
    sh.site.set_inactive_build_cap(Some(l.shared + l.warnings));
    let now = sh.site.ledger();
    assert!(now.kept.is_empty());
    assert_eq!((now.prepared.len(), now.shared), (2, l.shared));
    // room for one target: the least recently used target goes whole
    sh.site
        .set_inactive_build_cap(Some(l.shared + l.warnings - 1));
    let now = sh.site.ledger();
    assert_eq!(now.prepared, vec![b.clone()], "a was used before b");
    assert_eq!(now.shared, pb);
    assert!(pa > 0);
    assert_eq!(sh.site.logged_warnings(&a), None, "the whole target went");
}

// --- 14. shared parts that cannot fit ----------------------------------------------------------

#[test]
fn sod2_shared_parts_that_cannot_fit_serve_and_are_not_kept() {
    let w = world();
    let sh = shared(&w.h);
    let doc = note(&sh, "a", "View a.");
    let a = src("a");
    let first = page(&sh, &a, "alpha.html");
    let size = sh.site.prepared_bytes(&a).unwrap();
    sh.site.set_inactive_build_cap(Some(size / 2));
    assert_eq!(sh.site.ledger().total, 0, "the preparation cannot fit");
    let preps = sh.site.preparations();
    for n in 1..=2 {
        let again = page(&sh, &a, "alpha.html");
        assert_eq!(again.bytes, first.bytes, "request {n}");
        assert_eq!(
            sh.site.preparations(),
            preps + n,
            "request {n} prepares again"
        );
        let l = sh.site.ledger();
        assert_eq!(
            (l.total, l.prepared.len(), l.kept.len()),
            (0, 0, 0),
            "request {n}"
        );
    }
    // the stream keeps only its comparison facts, and compares
    let mut stream = sub(&sh, &a, "alpha.html", &first);
    assert!(empty(&mut stream));
    assert_eq!(sh.site.ledger().total, 0);
    let a2 = w.ids["$a2"].clone();
    commit(
        &sh,
        "a",
        json!([{"op": "update", "id": a2, "body": "Alpha edited."}]),
    );
    sh.site.step(&sh);
    assert_eq!(next(&mut stream), Some(LiveEvent::Reload));
    // one more change in a doc the view already changed: the label moves, alpha does not
    commit(
        &sh,
        "a",
        json!([{"op": "create", "type": "prose", "parent": doc, "body": "More."}]),
    );
    sh.site.step(&sh);
    match next(&mut stream) {
        Some(LiveEvent::Label { text, .. }) => assert!(text.contains("changes"), "{text}"),
        other => panic!("a label-only change: {other:?}"),
    }
    assert_eq!(sh.site.ledger().total, 0, "nothing kept after the requests");
    assert_eq!(sh.site.review_builds(), 0);
}

// --- 15. full target eviction ------------------------------------------------------------------

#[test]
fn sod2_full_target_eviction_allows_same_state_preparation_and_checks() {
    let w = world();
    let sh = shared(&w.h);
    warnings_in(&sh, "a");
    let a = src("a");
    let count = || {
        (
            sh.site.preparations(),
            sh.site.checks(),
            sh.site.page_renders(),
        )
    };
    sh.site.review_facts(&sh, &a).unwrap();
    page(&sh, &a, "alpha.html");
    assert_eq!(count(), (1, 1, 1));
    // page eviction: the preparation and the pass stay
    let l = sh.site.ledger();
    sh.site.set_inactive_build_cap(Some(l.shared + l.warnings));
    let f = sh.site.review_facts(&sh, &a).unwrap();
    assert_eq!(count(), (1, 1, 1), "the kept pass");
    page(&sh, &a, "alpha.html");
    assert_eq!(
        count(),
        (1, 1, 2),
        "the page renders again, from the kept preparation"
    );
    // full target eviction: the same state prepares and checks again
    sh.site.set_inactive_build_cap(Some(0));
    assert_eq!(sh.site.ledger().targets, 0);
    sh.site.set_inactive_build_cap(None);
    let again = sh.site.review_facts(&sh, &a).unwrap();
    assert_eq!(count(), (2, 2, 2));
    assert_eq!(again.warnings, f.warnings, "the same complete warnings");
    page(&sh, &a, "alpha.html");
    assert_eq!(count(), (2, 2, 3));
}

// --- 16. eager builds are protected ------------------------------------------------------------

#[test]
fn sod2_canon_and_tags_are_not_dropped_for_a_review_target() {
    let w = world();
    let sh = shared(&w.h);
    on_writer(&sh, |r| {
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
    });
    note(&sh, "a", "View a.");
    let a = src("a");
    page(&sh, &a, "index.html");
    let prep = sh.site.prepared_bytes(&a).unwrap();
    sh.site.set_inactive_build_cap(Some(0));
    sh.site.set_inactive_build_cap(None);
    let eager = [
        SiteTarget::Source(ScopeRef::Canon),
        SiteTarget::Source(ScopeRef::Tag("v1.0.0".into())),
        SiteTarget::Profile {
            scope: ScopeRef::Canon,
            key: "guide".into(),
        },
        SiteTarget::Profile {
            scope: ScopeRef::Tag("v1.0.0".into()),
            key: "guide".into(),
        },
    ];
    let mut bytes = 0;
    for t in &eager {
        bytes += counted(&sh.site.refresh(&sh, t).unwrap());
    }
    // the older eager builds fill the cap; a review preparation cannot fit beside them
    sh.site.set_inactive_build_cap(Some(bytes + prep / 2));
    assert_eq!(sh.site.held_builds(), 4);
    let builds = sh.site.builds();
    let served = page(&sh, &a, "index.html");
    assert!(!served.bytes.is_empty());
    let f = sh.site.review_facts(&sh, &guide("a")).unwrap();
    assert_eq!(f.profile.unwrap().title, "Guide book");
    page(&sh, &guide("a"), "manual/intro.html");
    let l = sh.site.ledger();
    assert_eq!(l.total, bytes, "the exact estimate: the eager builds alone");
    assert_eq!((l.shared, l.pages, l.warnings), (0, 0, 0));
    assert_eq!(sh.site.held_builds(), 4, "no eager build dropped");
    for t in &eager {
        sh.site.refresh(&sh, t).unwrap();
    }
    assert_eq!(sh.site.builds(), builds, "no eager rebuild");
    // with room beside them, the review parts are kept, and still yield first
    sh.site.set_inactive_build_cap(Some(bytes + 4 * prep));
    page(&sh, &a, "index.html");
    assert_eq!(sh.site.ledger().prepared, vec![a.clone()]);
    sh.site.set_inactive_build_cap(Some(bytes));
    assert_eq!(sh.site.ledger().total, bytes);
    assert_eq!(sh.site.held_builds(), 4);
}

// --- 17. warnings after a full drop ------------------------------------------------------------

#[test]
fn sod2_full_drop_allows_warnings_once_per_pass() {
    let w = world();
    let sh = shared(&w.h);
    let lines = collect(&sh);
    let dg = warnings_in(&sh, "a");
    let a = src("a");
    let first = sh.site.review_facts(&sh, &a).unwrap();
    let wa = diagram_warning(&sh, "a", &dg);
    let pass = vec![line(&a, GLOSSARY), line(&a, &wa)];
    assert_eq!(first.warnings, vec![GLOSSARY.to_string(), wa.clone()]);
    assert_eq!(lines_of(&lines), pass);
    // page eviction keeps the record
    page(&sh, &a, "broken-a.html");
    let l = sh.site.ledger();
    sh.site.set_inactive_build_cap(Some(l.shared + l.warnings));
    page(&sh, &a, "broken-a.html");
    sh.site.review_facts(&sh, &a).unwrap();
    assert_eq!(lines_of(&lines), pass, "nothing logged again");
    // each full drop permits one line per warning in the next pass, with the complete warnings
    for round in 1..=2 {
        sh.site.set_inactive_build_cap(Some(0));
        sh.site.set_inactive_build_cap(None);
        let f = sh.site.review_facts(&sh, &a).unwrap();
        assert_eq!(f.warnings, first.warnings, "round {round}");
        page(&sh, &a, "broken-a.html");
        sh.site.review_facts(&sh, &a).unwrap();
        let want: Vec<String> = (0..=round).flat_map(|_| pass.clone()).collect();
        assert_eq!(lines_of(&lines), want, "round {round}");
    }
    sh.site.set_log_hook(None);
}

// --- 18. warning history under churn -----------------------------------------------------------

#[test]
fn sod2_warning_target_churn_bounds_history_and_relogs_after_drop() {
    const VIEWS: [&str; 12] = [
        "w0", "w1x", "w2", "w3", "w4", "w5", "w6", "w7", "w8", "w9", "w10", "w11",
    ];
    let w = world();
    let sh = shared(&w.h);
    let lines = collect(&sh);
    let mut warned = BTreeMap::new();
    for v in VIEWS {
        let dg = warnings_in(&sh, v);
        warned.insert(v, dg);
    }
    let words: BTreeMap<&str, String> = VIEWS
        .iter()
        .map(|v| (*v, diagram_warning(&sh, v, &warned[v])))
        .collect();
    // measured: one target's preparation and warning state; the cap holds about two
    let one = sh.site.prepared_bytes(&src(VIEWS[11])).unwrap();
    sh.site.set_inactive_build_cap(Some(0));
    lines.lock().unwrap().clear();
    let cap = one * 5 / 2;
    sh.site.set_inactive_build_cap(Some(cap));
    let expect = |v: &str| vec![line(&src(v), GLOSSARY), line(&src(v), &words[v])];
    let mut want: Vec<String> = vec![];
    let assert_bounds = |what: &str| {
        let l = sh.site.ledger();
        assert!(l.total <= cap, "{what}: {} > {cap}", l.total);
        assert!(l.targets <= MAX_SCOPES, "{what}: {} targets", l.targets);
        assert!(l.logged <= 2 * l.targets, "{what}: {} records", l.logged);
        let strings: u64 = VIEWS
            .iter()
            .filter_map(|v| sh.site.logged_warnings(&src(v)))
            .flatten()
            .map(|w| w.len() as u64)
            .sum();
        assert!(l.warnings >= strings, "{what}: the warning strings count");
        assert_eq!(l.logged == 0, l.warnings == 0, "{what}");
    };
    for round in 0..2 {
        for v in VIEWS {
            let t = src(v);
            // repeated occurrences within one pass: a page, its stream, the pass, the page
            let p = page(&sh, &t, &format!("broken-{v}.html"));
            assert_bounds(&format!("round {round} {v}: page"));
            let s = sub(&sh, &t, &format!("broken-{v}.html"), &p);
            let f = sh.site.review_facts(&sh, &t).unwrap();
            assert_eq!(f.warnings, vec![GLOSSARY.to_string(), words[v].clone()]);
            page(&sh, &t, &format!("broken-{v}.html"));
            drop(s);
            assert_bounds(&format!("round {round} {v}: closed"));
            // a target dropped since its last pass logs each warning once in this one
            want.extend(expect(v));
            assert_eq!(lines_of(&lines), want, "round {round} {v}");
        }
    }
    // dropped targets keep no history anywhere; a weak probe of a kept one goes with it
    let l = sh.site.ledger();
    let gone: Vec<&str> = VIEWS
        .iter()
        .copied()
        .filter(|v| sh.site.logged_warnings(&src(v)).is_none())
        .collect();
    assert!(gone.len() >= VIEWS.len() - MAX_SCOPES, "{gone:?}");
    assert!(l.targets < VIEWS.len());
    let last = src(VIEWS[11]);
    let weak = sh
        .site
        .prepared_handle(&last)
        .expect("the newest target is kept");
    // a retained target with its pages evicted logs nothing
    sh.site.set_inactive_build_cap(Some(l.shared + l.warnings));
    page(&sh, &last, "broken-w11.html");
    sh.site.review_facts(&sh, &last).unwrap();
    assert_eq!(lines_of(&lines), want, "page eviction keeps the record");
    // its full drop releases everything; a revisit logs one line per warning
    sh.site.set_inactive_build_cap(Some(0));
    assert_eq!(weak.strong_count(), 0);
    assert_eq!(sh.site.ledger(), Ledger::default());
    sh.site.set_inactive_build_cap(Some(cap));
    page(&sh, &last, "broken-w11.html");
    page(&sh, &last, "broken-w11.html");
    sh.site.review_facts(&sh, &last).unwrap();
    want.extend(expect(VIEWS[11]));
    assert_eq!(lines_of(&lines), want);
    sh.site.set_log_hook(None);
}
