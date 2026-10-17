//! ls2: the live layer: revisions, the input key, the diff, and the bytes a page is served as.
use rdy_engine::{Rdy, Scope};
use rdy_render::live::*;
use rdy_render::{SlotKind, escape};
use serde_json::json;

#[test]
fn ls2_a_page_is_served_filled_with_the_script() {
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let html = String::from_utf8(page(&b, "index.html", "canon").unwrap()).unwrap();
    // the header and the label sit in exactly the recorded ranges, moved on by the script tag put
    // in the head
    let base = &b.files["index.html"];
    let shift = html.len() - base.len();
    let kinds: Vec<SlotKind> = b.label_at["index.html"].iter().map(|(k, _)| *k).collect();
    assert_eq!(kinds, vec![SlotKind::Header, SlotKind::Label]);
    for (kind, r) in &b.label_at["index.html"] {
        let want = match kind {
            SlotKind::Label => escape(&b.label),
            SlotKind::Header => b.header.clone(),
        };
        assert_eq!(&html[r.start + shift..r.end + shift], want);
    }
    assert!(html.contains(&format!("<span data-rdy-label>{}</span>", escape(&b.label))));
    let tag = format!(
        "<script src=\"/_rdy/live.js\" data-events=\"/_rdy/events?scope=canon&amp;page=index.html&amp;rev={}&amp;label={}\" defer></script></head>",
        b.revs["index.html"], b.label_rev
    );
    assert!(html.contains(&tag), "{html}");
    let llms = String::from_utf8(page(&b, "llms.txt", "canon").unwrap()).unwrap();
    assert!(llms.contains(&b.label));
    assert_eq!(
        page(&b, "assets/site.css", "canon").unwrap(),
        b.files["assets/site.css"]
    );
    assert!(page(&b, "no/such.html", "canon").is_none());
    let v = build_live(&r.read(), &Scope::View("editing".into()), "/~view/editing/").unwrap();
    let vhtml = String::from_utf8(page(&v, "index.html", "view:editing").unwrap()).unwrap();
    assert!(
        vhtml.contains("scope=view:editing&amp;page=index.html"),
        "{vhtml}"
    );
    assert!(
        vhtml.contains("Show changes"),
        "a view renders in review mode"
    );
}

#[test]
fn ls2_a_proposal_version_renders_in_review_mode() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_d, mut r) = rdy_qa::golden::tiny_with_doc();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"name": "proposed-doc", "title": "Proposed doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    let p = r
        .propose(&rdy_qa::golden::writer(), "w", "Add a doc", None)
        .unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let b = build_live(&r.read(), &scope, &format!("/~proposal/{pref}/")).unwrap();
    // the new doc is the one changed block (O2, the task-5 ruling); "draft" is only the
    // lifecycle state (D-BMX8P, O9)
    assert!(
        b.label
            .ends_with("· proposal: Add a doc, 1 change, not landed"),
        "{}",
        b.label
    );
    assert!(
        !b.label.contains("P-"),
        "no proposal ref on the site: {}",
        b.label
    );
    assert!(
        b.header.contains(">+1</a>") && b.header.contains("1 change, not landed"),
        "{}",
        b.header
    );
    let home =
        String::from_utf8(page(&b, "index.html", &format!("proposal:{pref}")).unwrap()).unwrap();
    assert!(
        home.contains("Show changes") && home.contains("New"),
        "review mode"
    );
}

#[test]
fn ls2_a_body_edit_changes_only_its_page() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    edit_second_prose(&mut r, "work", "Alpha", "A new second paragraph.", true);
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert_eq!(
        d.reload.into_iter().collect::<Vec<_>>(),
        vec!["alpha.html".to_string()]
    );
    assert_eq!(d.label, None);
}

#[test]
fn ls2_a_tag_changes_the_label_and_the_home_page() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    r.tag(&rdy_qa::golden::approver(), "v1.0.0", None, None)
        .unwrap();
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert_eq!(d.label.as_deref(), Some("v1.0.0"));
    assert!(d.reload.contains("index.html"), "home lists the versions");
    assert!(!d.reload.contains("alpha.html") && !d.reload.contains("beta.html"));
}

type Step = Box<dyn Fn(&mut Rdy)>;

#[test]
fn ls2_an_unchanged_key_means_unchanged_pages() {
    // the key is the only reason the site worker builds; a write it misses would leave a page stale
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "draft", "Alpha", "Draft text.", false);
    // final review M8: a proposal version too; a landing moves canon's head under it
    edit_second_prose(&mut r, "prop", "Beta", "Proposed text.", false);
    let p = r
        .propose(&rdy_qa::golden::writer(), "prop", "Proposed", None)
        .unwrap();
    let proposal = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.r#ref))
        .unwrap();
    let scopes = [Scope::Canon, Scope::View("draft".into()), proposal];
    let mut keys: Vec<_> = scopes
        .iter()
        .map(|s| inputs_key(&r.read(), s, 0).unwrap())
        .collect();
    let mut sites: Vec<_> = scopes
        .iter()
        .map(|s| build_live(&r.read(), s, "/").unwrap())
        .collect();
    let steps: Vec<(&str, Step)> = vec![
        (
            "a commit to draft",
            Box::new(|r| edit_second_prose(r, "draft", "Beta", "Draft two.", false)),
        ),
        (
            "a commit to another view",
            Box::new(|r| {
                rdy_qa::golden::write(
                    r,
                    "other",
                    json!([
                        {"op": "create", "type": "doc", "fields": {"name": "elsewhere", "title": "Elsewhere", "doc_type": "overview"}}
                    ]),
                )
                .unwrap();
            }),
        ),
        (
            "a landing",
            Box::new(|r| edit_second_prose(r, "work", "Beta", "Landed text.", true)),
        ),
        (
            "a tag",
            Box::new(|r| {
                r.tag(&rdy_qa::golden::approver(), "v2.0.0", None, None)
                    .unwrap();
            }),
        ),
    ];
    for (what, step) in steps {
        step(&mut r);
        for (i, s) in scopes.iter().enumerate() {
            let k = inputs_key(&r.read(), s, 0).unwrap();
            let site = build_live(&r.read(), s, "/").unwrap();
            if k == keys[i] {
                assert!(
                    site.files == sites[i].files,
                    "{what}: {s:?}'s key held but its pages moved"
                );
            }
            keys[i] = k;
            sites[i] = site;
        }
    }
    assert_ne!(
        inputs_key(&r.read(), &Scope::Canon, 0).unwrap(),
        inputs_key(&r.read(), &Scope::Canon, 1).unwrap(),
        "pack generation"
    );
}

/// Final review M11: a proposal version based on canon's head is read against canon itself, as
/// a view is (no as-of build of the same state); once canon moves on, against canon at its base.
#[test]
fn ls2_a_proposal_at_canons_head_is_reviewed_against_canon() {
    use rdy_render::review::base_of;
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "prop", "Alpha", "Proposed.", false);
    let p = r
        .propose(&rdy_qa::golden::writer(), "prop", "Proposed", None)
        .unwrap();
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.r#ref))
        .unwrap();
    assert_eq!(base_of(&r.read(), &scope), Some(Scope::Canon));
    edit_second_prose(&mut r, "work", "Beta", "Landed.", true);
    assert!(
        matches!(base_of(&r.read(), &scope), Some(Scope::At(_))),
        "{:?}",
        base_of(&r.read(), &scope)
    );
}

#[test]
fn ls2_a_commit_to_another_view_keeps_canons_key() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let canon = inputs_key(&r.read(), &Scope::Canon, 0).unwrap();
    edit_second_prose(&mut r, "draft", "Alpha", "Only in draft.", false);
    assert_eq!(inputs_key(&r.read(), &Scope::Canon, 0).unwrap(), canon);
}

#[test]
fn ls2_a_dismissal_moves_canons_key_and_its_pages() {
    // the disagreements list is an input of canon's key: a dismissal moves no head
    use rdy_engine::tensions::{TensionKind, tension_label};
    let (_d, mut r, _s) = rdy_qa::golden::retired_reference();
    let key = inputs_key(&r.read(), &Scope::Canon, 0).unwrap();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    assert!(
        before.files.contains_key("disagreements.html"),
        "guard: canon disagrees"
    );
    let t = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .find(|t| t.kind == TensionKind::DeprecatedReference)
        .cloned()
        .expect("guard: the canon holds the tension");
    r.dismiss_tension(
        &rdy_qa::golden::writer(),
        &tension_label(&t.id),
        "fine as it is",
    )
    .unwrap();
    assert_ne!(
        inputs_key(&r.read(), &Scope::Canon, 0).unwrap(),
        key,
        "a dismissal moves the key"
    );
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert!(
        d.reload.contains("disagreements.html") && d.reload.contains("index.html"),
        "{:?}",
        d.reload
    );
}

/// `Tiny` with two landed top-level docs, Alpha and Beta, each with a section holding two prose
/// blocks.
fn two_docs() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = rdy_qa::golden::tiny();
    for t in ["Alpha", "Beta"] {
        rdy_qa::golden::write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": t.to_lowercase(), "title": t, "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} first.")},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} second.")}
        ])).unwrap();
    }
    rdy_qa::golden::land(&mut r, "work").unwrap();
    (d, r)
}

/// Writes an edit of the second prose block of the doc titled `title` into `view`, and lands
/// the view when `land`.
fn edit_second_prose(r: &mut Rdy, view: &str, title: &str, body: &str, land: bool) {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap()
        .id
        .clone();
    let sec = m.children(Some(&doc))[0].id.clone();
    let second = m.children(Some(&sec))[1].id.clone();
    rdy_qa::golden::write(
        r,
        view,
        json!([
            {"op": "update", "id": second.to_string(), "body": body}
        ]),
    )
    .unwrap();
    if land {
        rdy_qa::golden::land(r, view).unwrap();
    }
}

#[test]
fn ls2_a_permalink_page_has_no_revision_and_no_script() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, r) = two_docs();
    let b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let links: Vec<_> = b.files.keys().filter(|p| p.starts_with("go/")).collect();
    assert!(!links.is_empty(), "guard: the site has permalink pages");
    for p in links {
        assert!(!b.revs.contains_key(p), "{p}");
        assert_eq!(page(&b, p, "canon").unwrap(), b.files[p], "{p}");
    }
    assert!(b.files.keys().filter(|p| p.ends_with(".html")).count() > b.revs.len());
}

#[test]
fn ls2_a_new_label_goes_only_into_the_recorded_ranges() {
    // content can hold the label's text; only the places the build wrote it are changed
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "work", "Alpha", "It reads unversioned, here.", true);
    let mut b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    assert!(b.label.contains("unversioned"), "guard: {}", b.label);
    let old = b.label.clone();
    b.label = "v9 <&>, 3 updates since".into();
    let html = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    // the colophon holds the label; the header holds the pill and its chips (sr5)
    assert_eq!(html.matches(&escape(&b.label)).count(), 1, "{html}");
    assert!(
        html.contains("It reads unversioned, here."),
        "the text in content is kept"
    );
    assert!(!html.contains(&format!("<span data-rdy-label>{old}</span>")));
    let llms = String::from_utf8(page(&b, "llms.txt", "canon").unwrap()).unwrap();
    assert!(llms.contains(&b.label), "the llms files take the label raw");
    // final review M4: the llms files take the label on one line, as the static build writes it
    // (a proposal title or a view name can hold a run of spaces or a line break)
    b.label = "Two  spaces\nand a newline".into();
    let html = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    assert_eq!(html.matches(&escape(&b.label)).count(), 1, "{html}");
    for f in ["llms.txt", "llms-full.txt"] {
        let text = String::from_utf8(page(&b, f, "canon").unwrap()).unwrap();
        assert!(text.contains("Two spaces and a newline"), "{f}: {text}");
        assert!(!text.contains("Two  spaces"), "{f}: {text}");
        assert!(!text.contains("\nand a newline"), "{f}: {text}");
    }
}

/// A page for `live.js`: its script tag, the document's visibility, the window's events, a fake
/// `EventSource` and fake timers that the cases run by hand (no real wait).
const LIVE_DOM: &str = r##"
const fs = require('fs');
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
function setup() {
  const log = { streams: [], timers: [], reloads: 0 };
  const on = (o) => { o.listeners = {}; o.addEventListener = (t, f) => { (o.listeners[t] = o.listeners[t] || []).push(f); }; return o; };
  const script = { getAttribute: (n) => n === 'data-events' ? '/_rdy/events?scope=canon&page=index.html&rev=1&label=2' : null };
  const document = on({ currentScript: script, visibilityState: 'visible', querySelectorAll: () => [] });
  const win = on({});
  function EventSource(url) { on(this); this.url = url; this.readyState = 0; this.closed = false; log.streams.push(this); }
  EventSource.prototype.close = function () { this.closed = true; this.readyState = 2; };
  EventSource.CONNECTING = 0; EventSource.OPEN = 1; EventSource.CLOSED = 2;
  win.EventSource = EventSource;
  const setTimeout = (f, ms) => { log.timers.push({ f, ms, done: false }); return log.timers.length; };
  const clearTimeout = (id) => { if (id && log.timers[id - 1]) log.timers[id - 1].done = true; };
  const location = { pathname: '/index.html', reload: () => { log.reloads += 1; } };
  const store = {}; const sessionStorage = { getItem: (k) => k in store ? store[k] : null, setItem: (k, v) => { store[k] = v; }, removeItem: (k) => { delete store[k]; } };
  new Function('window', 'document', 'EventSource', 'addEventListener', 'location', 'sessionStorage',
    'scrollTo', 'scrollY', 'setTimeout', 'clearTimeout', src)(
    win, document, EventSource, win.addEventListener, location, sessionStorage,
    () => {}, 0, setTimeout, clearTimeout);
  const fire = (o, t, e) => { (o.listeners[t] || []).forEach((f) => f(e || {})); if (o['on' + t]) o['on' + t](e || {}); };
  const last = () => log.streams[log.streams.length - 1];
  // the server answers the stream with a non-200 status (Busy): the browser fails the connection
  const refuse = (es) => { es = es || last(); es.readyState = 2; fire(es, 'error'); };
  // the timers still due, with their delays
  const due = () => log.timers.filter((t) => !t.done).map((t) => t.ms);
  // runs the timers due now, as the clock would
  const run = () => log.timers.filter((t) => !t.done).forEach((t) => { t.done = true; t.f(); });
  const open = () => log.streams.filter((s) => !s.closed).length;
  const visibility = (v) => { document.visibilityState = v; fire(document, 'visibilitychange'); };
  return { log, fire, last, refuse, due, run, open, visibility };
}
"##;

const LIVE_CASES: &str = r##"
const out = {};
// a refused stream is closed and opened again after a delay, with the page's revisions
const a = setup();
out.first = { streams: a.log.streams.length, url: a.last().url };
a.refuse();
out.refused = { open: a.open(), closed: a.log.streams[0].closed, due: a.due() };
a.run();
out.reopened = { streams: a.log.streams.length, open: a.open(), url: a.last().url, due: a.due() };
// each refusal in a row waits longer, up to a cap
const delays = [];
for (let i = 0; i < 7; i++) { a.refuse(); delays.push(a.due()); a.run(); }
out.delays = delays;
// a stream that opens resets the delay
a.fire(a.last(), 'open');
a.refuse();
out.after_open = a.due();
a.run();
// a dropped stream the browser reconnects itself (still CONNECTING) is left to the browser
a.last().readyState = 0; a.fire(a.last(), 'error');
out.connecting = { open: a.open(), due: a.due(), streams: a.log.streams.length };
// a late error of a stream already replaced does nothing
const before = a.log.streams.length;
a.refuse(a.log.streams[0]);
out.stale = { due: a.due(), streams: a.log.streams.length - before };
// a hidden page does not reopen; it opens once when shown again
const b = setup();
b.refuse();
b.visibility('hidden');
b.run();
out.hidden = { streams: b.log.streams.length, open: b.open(), due: b.due() };
b.visibility('visible');
b.run();
out.shown = { streams: b.log.streams.length, open: b.open(), due: b.due() };
// a refusal while hidden schedules nothing
const c = setup();
const s = c.last();
c.visibility('hidden');
c.refuse(s);
out.hidden_refusal = { due: c.due(), streams: c.log.streams.length };
// the reload event still reloads the page
const d = setup();
d.fire(d.last(), 'reload');
out.reload = { reloads: d.log.reloads, open: d.open(), due: d.due() };
console.log(JSON.stringify(out));
"##;

#[test]
fn rdx14_the_live_script_reopens_a_refused_stream() {
    // fix round (14.promoted): while the published state predates a removal the events route
    // answers Busy; EventSource fails such a connection for good, so live.js opens it again
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "rdx14_the_live_script_reopens_a_refused_stream",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/live.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", &format!("{LIVE_DOM}{LIVE_CASES}"), "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let url = "/_rdy/events?scope=canon&page=index.html&rev=1&label=2";
    assert_eq!(v["first"], json!({"streams": 1, "url": url}), "{v}");
    assert_eq!(
        v["refused"],
        json!({"open": 0, "closed": true, "due": [1000]}),
        "a refused stream is closed and a reopen is due in a second: {v}"
    );
    assert_eq!(
        v["reopened"],
        json!({"streams": 2, "open": 1, "url": url, "due": []}),
        "the reopen asks with the page's revisions: {v}"
    );
    assert_eq!(
        v["delays"],
        json!([[2000], [4000], [8000], [16000], [30000], [30000], [30000]]),
        "refusals in a row back off to a 30 s cap: {v}"
    );
    assert_eq!(
        v["after_open"],
        json!([1000]),
        "an open stream resets the delay: {v}"
    );
    assert_eq!(
        v["connecting"],
        json!({"open": 1, "due": [], "streams": 10}),
        "the browser's own reconnect is left alone: {v}"
    );
    assert_eq!(
        v["stale"],
        json!({"due": [], "streams": 0}),
        "a replaced stream's error does nothing: {v}"
    );
    assert_eq!(
        v["hidden"],
        json!({"streams": 1, "open": 0, "due": []}),
        "a hidden page does not reopen: {v}"
    );
    assert_eq!(
        v["shown"],
        json!({"streams": 2, "open": 1, "due": []}),
        "a page shown again opens one stream: {v}"
    );
    assert_eq!(
        v["hidden_refusal"],
        json!({"due": [], "streams": 1}),
        "a refusal of a hidden page schedules nothing: {v}"
    );
    assert_eq!(
        v["reload"],
        json!({"reloads": 1, "open": 0, "due": []}),
        "{v}"
    );
}
