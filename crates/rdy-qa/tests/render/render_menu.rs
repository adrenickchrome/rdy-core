//! sr8: the version menu of the live site (D-2F29G; RULING F4, F5; D-BMX8P, O9): the versions,
//! then the open proposals, then the open views, each with a reader's words and the number of
//! docs it changes; nothing on it acts or waits on a person (D-X61DT, k-BA85X, k-N0EJF).
use rdy_engine::{ProposalInfo, Rdy};
use rdy_qa::golden::{FIXTURE_DIR, Ids, approver, fixture_clock, land, write, writer};
use rdy_render::menu::{MenuRow, build_versions, docs_changed, menu, menu_key};
use serde_json::json;
use std::path::Path;

/// The golden corpus, landed and tagged `v1.0.0` with a message; the view `editing` edits the
/// tutorial's prose, is owned by an agent and is proposed once.
fn proposed() -> (tempfile::TempDir, Rdy, ProposalInfo) {
    let _clock = fixture_clock();
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let mut r = Rdy::open(dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(
        &approver(),
        "v1.0.0",
        None,
        Some("Accepted by the Steering Council".into()),
    )
    .unwrap();
    edit_tutorial(&mut r, "editing", "So far we have used three patterns.");
    r.view_own(&writer(), "editing", Some("agent:design_thread"))
        .unwrap();
    let p = r
        .propose(&writer(), "editing", "Tighten the tutorial")
        .unwrap();
    (dir, r, p)
}

fn edit_tutorial(r: &mut Rdy, view: &str, body: &str) {
    let ids = Ids::new();
    write(
        r,
        view,
        json!([{"op": "update", "id": ids.tutorial_prose.to_string(), "body": body}]),
    )
    .unwrap();
}

fn labels(rows: &[MenuRow]) -> Vec<&str> {
    rows.iter().map(|r| r.label.as_str()).collect()
}

/// The page the live site serves at `/~versions/`.
fn page_of(r: &Rdy) -> String {
    let st = r.read();
    let b = build_versions(&st, &mut |s| docs_changed(&st, s)).unwrap();
    assert_eq!(
        b.files.keys().collect::<Vec<_>>(),
        ["index.html"],
        "one page"
    );
    assert!(b.label_at.is_empty(), "no header slot and no label");
    assert!(b.label.is_empty() && b.header.is_empty());
    String::from_utf8(b.files["index.html"].clone()).unwrap()
}

#[test]
fn sr8_the_menu_lists_versions_proposals_and_views() {
    let (_d, r, p) = proposed();
    let st = r.read();
    let m = menu(&st, &mut |s| docs_changed(&st, s)).unwrap();

    assert_eq!(labels(&m.versions), ["Latest", "v1.0.0"]);
    assert_eq!(m.versions[0].href, "/");
    assert_eq!(m.versions[0].meta, ["v1.0.0"], "canon's text label");
    assert_eq!(m.versions[1].href, "/~tag/v1.0.0/");
    assert_eq!(
        m.versions[1].meta,
        ["5 October 2026", "Accepted by the Steering Council"]
    );

    assert_eq!(labels(&m.proposals), ["Tighten the tutorial"]);
    assert_eq!(m.proposals[0].href, format!("/~proposal/{}/", p.r#ref));
    assert!(p.r#ref.starts_with("P-") && p.r#ref.ends_with("@1"));
    assert_eq!(m.proposals[0].meta, ["by qa", "1 doc changed"]);

    assert_eq!(labels(&m.views), ["editing"]);
    assert_eq!(m.views[0].href, "/~view/editing/");
    assert_eq!(
        m.views[0].meta,
        ["by design thread (agent)", "1 doc changed"]
    );

    // the page holds the same rows, in three sections inside one navigation landmark
    let html = page_of(&r);
    assert!(
        html.contains("<h1>Versions, proposals and views</h1>"),
        "{html}"
    );
    assert!(
        html.contains("<nav class=\"vmenu\" aria-label=\"Versions, proposals and views\">"),
        "{html}"
    );
    let at = |needle: &str| {
        html.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {html}"))
    };
    assert!(at("<h2>Versions</h2>") < at("<h2>Proposals</h2>"));
    assert!(at("<h2>Proposals</h2>") < at("<h2>Views</h2>"));
    assert_eq!(html.matches("<ul class=\"vmenu-list\">").count(), 3);
    for row in m.versions.iter().chain(&m.proposals).chain(&m.views) {
        let want = format!(
            "<a href=\"{}\">{}</a> <span class=\"vmenu-meta\">{}</span>",
            row.href,
            row.label,
            row.meta.join(" · ")
        );
        assert!(html.contains(&want), "{want} in {html}");
    }
    assert!(html.contains("<link rel=\"stylesheet\" href=\"/assets/site.css\">"));
    assert!(
        html.contains("localStorage.getItem(\"rdy-theme\")"),
        "the pre-paint script"
    );
    assert!(
        html.contains("<a class=\"brand\" href=\"/\">"),
        "the site name links home"
    );
}

#[test]
fn sr8_an_empty_list_says_none_open() {
    let _clock = fixture_clock();
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let st = r.read();
    let m = menu(&st, &mut |s| docs_changed(&st, s)).unwrap();
    assert_eq!(labels(&m.versions), ["Latest"]);
    assert_eq!(m.versions[0].meta, ["unversioned"]);
    assert!(m.proposals.is_empty() && m.views.is_empty());
    let html = page_of(&r);
    assert_eq!(html.matches("None open.").count(), 2, "{html}");
    assert_eq!(html.matches("<ul class=\"vmenu-list\">").count(), 1);
}

#[test]
fn sr8_the_menu_has_no_call_to_action() {
    let (_d, r, _p) = proposed();
    let html = page_of(&r);
    for word in [
        "Approve",
        "approve",
        "ready",
        "needs you",
        "waiting",
        "<form",
        "<button",
    ] {
        assert!(!html.contains(word), "{word:?} in {html}");
    }
    // D-BMX8P: "draft" is only the lifecycle state, never a view or a proposal
    for run in rdy_qa::site::visible_runs(&html) {
        assert!(
            !run.text.to_lowercase().contains("draft"),
            "{:?} in {html}",
            run.text
        );
    }
    assert!(!html.to_lowercase().contains("draft"), "{html}");
}

#[test]
fn sr8_the_key_moves_with_what_the_menu_shows() {
    let (d, mut r, _p) = proposed();
    let key = |r: &Rdy| menu_key(&r.read(), 0);
    let k0 = key(&r);
    assert_eq!(k0, key(&r), "the same state gives the same key");
    assert_ne!(
        k0,
        menu_key(&r.read(), 1),
        "the pack generation is in the key"
    );

    // a commit to a listed view
    edit_tutorial(
        &mut r,
        "editing",
        "So far we have used three simple patterns.",
    );
    let k1 = key(&r);
    assert_ne!(k1, k0, "a view commit moves the key");

    // a new tag, on the landing canon is at
    r.tag(&approver(), "v1.0.1", None, None).unwrap();
    let k2 = key(&r);
    assert_ne!(k2, k1, "a new tag moves the key");

    // a new view, then a proposal of it
    let ids = Ids::new();
    write(
        &mut r,
        "other",
        json!([{"op": "update", "id": ids.guards_prose.to_string(), "body": "Guards say more."}]),
    )
    .unwrap();
    let k3 = key(&r);
    assert_ne!(k3, k2, "a new view with a change moves the key");
    r.propose(&writer(), "other", "Guards").unwrap();
    let k4 = key(&r);
    assert_ne!(k4, k3, "a new proposal moves the key");

    // a change of owner (the menu names the owner)
    r.view_own(&writer(), "editing", Some("agent:claude"))
        .unwrap();
    let k5 = key(&r);
    assert_ne!(k5, k4, "an ownership change moves the key");

    // a body edit landed in canon: the view `fix` holds no change once it lands, so it is not
    // listed, and only canon's head (and the views based on it) move
    write(
        &mut r,
        "fix",
        json!([{"op": "update", "id": ids.motivation_prose.to_string(), "body": "Three idioms."}]),
    )
    .unwrap();
    let k6 = key(&r);
    land(&mut r, "fix").unwrap();
    let st = r.read();
    assert_ne!(key(&r), k6, "a landing moves the key through canon's head");
    let m = menu(&st, &mut |s| docs_changed(&st, s)).unwrap();
    assert_eq!(labels(&m.views), ["editing", "other"]);
    drop(st);

    // the project's name is the page's title and brand
    let k7 = key(&r);
    r.close().unwrap();
    std::fs::write(
        d.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: Renamed\n",
    )
    .unwrap();
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.read().project_name(), "Renamed");
    assert_ne!(key(&r), k7, "a new project name moves the key");
}

/// A just-enough DOM for `menu.js`: a header with the pill, stub `fetch`, `DOMParser` and
/// `EventSource`, and a log of what the script fetched, streamed and followed.
const MENU_HARNESS: &str = r##"
const fs = require('fs');
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
let doc = null;
function El(tag, attrs) { this.tagName = tag; this.attrs = Object.assign({}, attrs || {}); this.children = []; this.parentNode = null; }
Object.defineProperty(El.prototype, 'className', { get() { return this.attrs.class || ''; }, set(v) { this.attrs.class = String(v); } });
Object.defineProperty(El.prototype, 'nextSibling', { get() { if (!this.parentNode) return null; const s = this.parentNode.children; return s[s.indexOf(this) + 1] || null; } });
Object.defineProperty(El.prototype, 'firstChild', { get() { return this.children[0] || null; } });
El.prototype.getAttribute = function (n) { return n in this.attrs ? this.attrs[n] : null; };
El.prototype.setAttribute = function (n, v) { this.attrs[n] = String(v); };
El.prototype.removeAttribute = function (n) { delete this.attrs[n]; };
El.prototype.hasAttribute = function (n) { return n in this.attrs; };
El.prototype.appendChild = function (c) { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = this; this.children.push(c); return c; };
El.prototype.insertBefore = function (c, ref) { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = this; const i = ref ? this.children.indexOf(ref) : -1; if (i < 0) this.children.push(c); else this.children.splice(i, 0, c); return c; };
El.prototype.removeChild = function (c) { this.children.splice(this.children.indexOf(c), 1); c.parentNode = null; return c; };
El.prototype.replaceChild = function (c, old) { this.insertBefore(c, old); return this.removeChild(old); };
El.prototype.contains = function (n) { for (let x = n; x; x = x.parentNode) if (x === this) return true; return false; };
El.prototype.focus = function () { doc.activeElement = this; };
function matches(el, sel) {
  const m = /^([a-z]+)?(?:\.([\w-]+))?(?:\[([\w-]+)\])?$/.exec(sel);
  if (!m) throw new Error('the harness does not read the selector ' + sel);
  if (m[1] && el.tagName !== m[1]) return false;
  if (m[2] && !el.className.split(' ').includes(m[2])) return false;
  if (m[3] && !(m[3] in el.attrs)) return false;
  return true;
}
El.prototype.closest = function (sel) { for (let x = this; x; x = x.parentNode) if (matches(x, sel)) return x; return null; };
El.prototype.querySelector = function (sel) { for (const c of this.children) { if (matches(c, sel)) return c; const d = c.querySelector(sel); if (d) return d; } return null; };
El.prototype.querySelectorAll = function (sel) { const out = []; for (const c of this.children) { if (matches(c, sel)) out.push(c); out.push(...c.querySelectorAll(sel)); } return out; };
function h(tag, attrs, ...kids) { const e = new El(tag, attrs); kids.forEach((k) => e.appendChild(k)); return e; }
// the fetched page: its live script and its menu (revision n)
function menuPage(n) {
  return h('html', {},
    h('head', {}, h('script', { src: '/_rdy/live.js', 'data-events': '/_rdy/events?scope=versions&page=index.html&rev=' + n })),
    h('body', {}, h('main', {}, h('h1', {}),
      h('nav', { class: 'vmenu', 'aria-label': 'Versions, proposals and views' },
        h('a', { href: '/' }), h('a', { href: '/~tag/v1.0.0/' }), h('a', { href: '/~view/editing/?' + n })))));
}
function setup(opts) {
  const log = { fetched: [], streams: [], followed: null };
  const pillAttrs = { class: 'pill', href: '/~versions/' };
  if (opts.menu) Object.assign(pillAttrs, { 'data-menu': '', 'aria-haspopup': 'dialog', 'aria-expanded': 'false' });
  const pill = h('a', pillAttrs, h('span', { class: 'pill-text' }));
  const version = h('span', { class: 'version', 'data-rdy-version': '' }, pill);
  const other = h('a', { href: '/elsewhere.html' });
  const header = h('header', { class: 'head' }, h('a', { class: 'brand' }), version, other);
  const body = h('body', {}, header);
  const document = {
    activeElement: null, visibilityState: 'visible', listeners: {},
    addEventListener(t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); },
    querySelector: (s) => body.querySelector(s),
    createElement: (t) => new El(t),
    importNode: (n) => n,
  };
  doc = document;
  let served = 0;
  const window = {
    listeners: {},
    addEventListener(t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); },
    fetch: (url) => { log.fetched.push(url); served += 1; const n = served; return opts.fail ? Promise.reject(new Error('down')) : Promise.resolve({ ok: true, text: () => Promise.resolve('page ' + n) }); },
    DOMParser: function () { this.parseFromString = (text) => menuPage(text.split(' ')[1]); },
    EventSource: function (url) { const es = this; es.url = url; es.closed = false; es.listeners = {}; es.addEventListener = (t, f) => { (es.listeners[t] = es.listeners[t] || []).push(f); }; es.close = () => { es.closed = true; }; log.streams.push(es); },
    location: { set href(v) { log.followed = v; }, get href() { return 'http://127.0.0.1/'; } },
  };
  new Function('window', 'document', src)(window, document);
  const fire = (target, t, e) => (document.listeners[t] || []).forEach((f) => f(Object.assign({ type: t, target }, e)));
  const click = (target) => { let prevented = false; fire(target, 'click', { button: 0, preventDefault() { prevented = true; } }); return prevented; };
  const key = (k) => fire(body, 'keydown', { key: k, preventDefault() {} });
  const pop = () => header.querySelector('.vmenu-pop');
  const state = () => {
    const p = pop();
    return {
      expanded: pill.getAttribute('aria-expanded'),
      open: !!p,
      role: p && p.getAttribute('role'),
      label: p && p.getAttribute('aria-label'),
      links: p ? p.querySelectorAll('a').map((a) => a.getAttribute('href')) : [],
      menus: p ? p.querySelectorAll('nav').length : 0,
      after_version: !!p && version.nextSibling === p,
      focus: document.activeElement === pill ? 'pill' : (document.activeElement ? document.activeElement.getAttribute('href') : null),
      fetched: log.fetched.slice(),
      streams: log.streams.map((s) => ({ url: s.url, closed: s.closed })),
      followed: log.followed,
    };
  };
  return { pill, other, header, document, log, click, key, state };
}
const tick = () => new Promise((r) => setTimeout(r, 0));
(async () => {
  const out = {};
  // a click opens the dialog with the fetched links; a reload event fetches again
  const a = setup({ menu: true });
  out.prevented = a.click(a.pill.firstChild);
  out.pending = a.pill.getAttribute('aria-expanded');
  await tick();
  out.opened = a.state();
  a.log.streams[0].listeners.reload.forEach((f) => f({}));
  await tick();
  out.reloaded = a.state();
  a.key('Escape');
  out.escaped = a.state();
  // a second click on the pill closes it
  const b = setup({ menu: true });
  b.click(b.pill); await tick();
  out.second = b.click(b.pill);
  out.toggled = b.state();
  // a click outside closes it
  const c = setup({ menu: true });
  c.click(c.pill); await tick();
  out.outside_prevented = c.click(c.other);
  out.outside = c.state();
  // a hidden tab closes the stream; the popover stays
  const d = setup({ menu: true });
  d.click(d.pill); await tick();
  d.document.visibilityState = 'hidden';
  (d.document.listeners.visibilitychange || []).forEach((f) => f({}));
  out.hidden = d.state();
  // a failed fetch follows the link
  const e = setup({ menu: true, fail: true });
  out.fail_prevented = e.click(e.pill); await tick();
  out.failed = e.state();
  // a pill with no data-menu is a plain link
  const f = setup({ menu: false });
  out.plain_prevented = f.click(f.pill); await tick();
  out.plain = f.state();
  console.log(JSON.stringify(out));
})().catch((e) => { console.error(e); process.exit(1); });
"##;

#[test]
fn sr8_the_menu_script_opens_and_closes_the_popover() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "sr8_the_menu_script_opens_and_closes_the_popover",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/menu.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", MENU_HARNESS, "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let page = "/~versions/index.html";
    let s1 = "/_rdy/events?scope=versions&page=index.html&rev=1";
    let s2 = "/_rdy/events?scope=versions&page=index.html&rev=2";
    let label = "Versions, proposals and views";
    assert_eq!(v["prevented"], true, "{v}");
    assert_eq!(v["pending"], "true", "{v}");
    assert_eq!(
        v["opened"],
        json!({
            "expanded": "true", "open": true, "role": "dialog", "label": label,
            "links": ["/", "/~tag/v1.0.0/", "/~view/editing/?1"], "menus": 1,
            "after_version": true, "focus": "/", "fetched": [page],
            "streams": [{"url": s1, "closed": false}], "followed": null
        }),
        "a click opens the dialog with the fetched links and focuses the first"
    );
    assert_eq!(
        v["reloaded"]["links"],
        json!(["/", "/~tag/v1.0.0/", "/~view/editing/?2"]),
        "{v}"
    );
    assert_eq!(v["reloaded"]["menus"], 1, "{v}");
    assert_eq!(v["reloaded"]["fetched"], json!([page, page]), "{v}");
    assert_eq!(
        v["reloaded"]["streams"],
        json!([{"url": s1, "closed": true}, {"url": s2, "closed": false}]),
        "{v}"
    );
    assert_eq!(
        v["escaped"]["streams"],
        json!([{"url": s1, "closed": true}, {"url": s2, "closed": true}]),
        "{v}"
    );
    for (case, closed) in [
        ("escaped", &v["escaped"]),
        ("toggled", &v["toggled"]),
        ("outside", &v["outside"]),
    ] {
        assert_eq!(closed["open"], false, "{case}: {v}");
        assert_eq!(closed["expanded"], "false", "{case}: {v}");
        assert_eq!(
            closed["focus"], "pill",
            "{case}: focus returns to the pill: {v}"
        );
        assert_eq!(closed["followed"], json!(null), "{case}: {v}");
    }
    assert_eq!(v["second"], true, "{v}");
    assert_eq!(v["toggled"]["streams"][0]["closed"], true, "{v}");
    assert_eq!(v["outside_prevented"], false, "{v}");
    assert_eq!(v["outside"]["streams"][0]["closed"], true, "{v}");
    assert_eq!(v["hidden"]["open"], true, "{v}");
    assert_eq!(v["hidden"]["streams"][0]["closed"], true, "{v}");
    assert_eq!(v["fail_prevented"], true, "{v}");
    assert_eq!(v["failed"]["open"], false, "{v}");
    assert_eq!(v["failed"]["expanded"], "false", "{v}");
    assert_eq!(
        v["failed"]["followed"], "/~versions/",
        "a failed fetch follows the link: {v}"
    );
    assert_eq!(v["plain_prevented"], false, "{v}");
    assert_eq!(v["plain"]["fetched"], json!([]), "{v}");
    assert_eq!(v["plain"]["open"], false, "{v}");
}
