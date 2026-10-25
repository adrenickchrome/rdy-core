//! Task 8 of the renderer plan (dgm8): the sidebar's folders disclose in place, hold one more
//! level than the path shows, indent per level, and keep their open state for the session; the
//! page works the same without the script.
use rdy_engine::Scope;
use rdy_qa::golden::*;
use serde_json::{Value, json};

/// Project `Deep`: a top-level doc, and the folder `area` three folders deep, with folders off
/// the path at each level, an empty folder and a folder that holds a folder.
///
/// ```text
/// Intro
/// Area
///   Area One
///   Mid
///     Mid One
///     Deep
///       Leaf
///       Deeper
///         Bottom
///     Side
///       Side One
///       Sidedeep
///         Sidedeep One
///   Empty
///   Other
///     Other One
///     Oo
///       Oo One
/// ```
fn deep_site() -> (tempfile::TempDir, rdy_engine::Rdy, tempfile::TempDir) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny();
    let folder = |id: &str, parent: Option<&str>, name: &str, title: &str| {
        let mut f = json!({"op": "create", "type": "folder", "id": id, "fields": {"name": name, "title": title}});
        if let Some(p) = parent {
            f["parent"] = p.into();
        }
        f
    };
    let doc = |id: &str, parent: Option<&str>, name: &str, title: &str| {
        let mut f = json!({"op": "create", "type": "doc", "id": id, "fields": {"name": name, "title": title, "doc_type": "overview"}});
        if let Some(p) = parent {
            f["parent"] = p.into();
        }
        f
    };
    let sec = |parent: &str| json!({"op": "create", "type": "section", "parent": parent, "fields": {"title": "Summary"}});
    write(
        &mut r,
        "work",
        json!([
            doc("$intro", None, "intro", "Intro"),
            sec("$intro"),
            folder("$area", None, "area", "Area"),
            doc("$a1", Some("$area"), "a-one", "Area One"),
            sec("$a1"),
            folder("$mid", Some("$area"), "mid", "Mid"),
            doc("$m1", Some("$mid"), "m-one", "Mid One"),
            sec("$m1"),
            folder("$deep", Some("$mid"), "deep", "Deep"),
            doc("$leaf", Some("$deep"), "leaf", "Leaf"),
            sec("$leaf"),
            folder("$deeper", Some("$deep"), "deeper", "Deeper"),
            doc("$bottom", Some("$deeper"), "bottom", "Bottom"),
            sec("$bottom"),
            folder("$side", Some("$mid"), "side", "Side"),
            doc("$s1", Some("$side"), "s-one", "Side One"),
            sec("$s1"),
            folder("$sd", Some("$side"), "sidedeep", "Sidedeep"),
            doc("$sd1", Some("$sd"), "sd-one", "Sidedeep One"),
            sec("$sd1"),
            folder("$empty", Some("$area"), "empty", "Empty"),
            folder("$other", Some("$area"), "other", "Other"),
            doc("$o1", Some("$other"), "o-one", "Other One"),
            sec("$o1"),
            folder("$oo", Some("$other"), "oo", "Oo"),
            doc("$oo1", Some("$oo"), "oo-one", "Oo One"),
            sec("$oo1"),
        ]),
    )
    .expect("the deep tree");
    land(&mut r, "work").expect("land the deep tree");
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    (d, r, out)
}

/// The sidebar's markup of a rendered page.
fn nav_of(h: &str) -> String {
    let open = "<nav aria-label=\"Pages\">";
    let start = h.find(open).expect("the sidebar");
    let end = start + h[start..].find("</nav>").expect("the sidebar ends") + "</nav>".len();
    h[start..end].to_string()
}

/// How many `<ul class="nav-sub">` lists enclose the first occurrence of `needle` in `nav`.
fn sub_depth(nav: &str, needle: &str) -> i32 {
    let at = nav
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {nav}"));
    let before = &nav[..at];
    // every list opens with `<ul` and closes with `</ul>`; only the nav-sub ones count
    let mut depth = 0;
    let mut stack: Vec<bool> = vec![];
    let mut rest = before;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if rest.starts_with("<ul class=\"nav-sub\"") {
            stack.push(true);
            depth += 1;
        } else if rest.starts_with("<ul") {
            stack.push(false);
        } else if rest.starts_with("</ul>") && stack.pop() == Some(true) {
            depth -= 1;
        }
        rest = &rest[1..];
    }
    depth
}

fn css(out: &std::path::Path) -> rdy_qa::site::Css {
    rdy_qa::site::parse_css(&std::fs::read_to_string(out.join("assets/site.css")).unwrap())
}

#[test]
fn dgm8_each_nested_level_indents() {
    let (_d, _r, out) = deep_site();
    let nav = nav_of(&html(out.path(), "area/mid/deep/leaf.html"));
    // the top-level group adds no indent: its list is a plain list
    assert!(
        nav.contains("<summary><span class=\"nav-group-label\">Area</span></summary><ul>"),
        "{nav}"
    );
    // each level below it is one nav-sub, and one is inside the other
    assert_eq!(sub_depth(&nav, ">Mid One</a>"), 1, "{nav}");
    assert_eq!(sub_depth(&nav, ">Leaf</a>"), 2, "{nav}");
    assert_eq!(sub_depth(&nav, ">Bottom</a>"), 3, "{nav}");
    let css = css(out.path());
    let at = |p: &str| rdy_qa::site::cascaded(&css, ".nav-root .nav-sub", p, 1200);
    assert_eq!(at("padding-inline-start").as_deref(), Some("0.875rem"));
    for gone in ["padding", "padding-left", "margin-left"] {
        assert_eq!(at(gone), None, "{gone}");
    }
    // nothing at the same or lower weight takes the indent back
    for sel in [".nav-sub", ".nav-root ul", ".nav-root, .nav-root ul"] {
        for p in ["padding-inline-start", "padding-left"] {
            let v = rdy_qa::site::cascaded(&css, sel, p, 1200);
            assert!(v.is_none(), "{sel} sets {p}: {v:?}");
        }
    }
    // the phone rules keep it
    assert_eq!(
        rdy_qa::site::cascaded(&css, ".nav-root .nav-sub", "padding-inline-start", 375).as_deref(),
        Some("0.875rem")
    );
}

#[test]
fn dgm8_folder_disclosure_does_not_navigate() {
    let (_d, _r, out) = deep_site();
    let nav = nav_of(&html(out.path(), "area/mid/deep/leaf.html"));
    // a link never sits inside a summary
    let mut rest = nav.as_str();
    let mut summaries = 0;
    while let Some(i) = rest.find("<summary") {
        let end = rest[i..].find("</summary>").expect("a summary ends");
        let summary = &rest[i..i + end];
        assert!(!summary.contains("<a "), "{summary}");
        summaries += 1;
        rest = &rest[i + end..];
    }
    // Area, Mid, Deep, Deeper, Side, Empty and Other
    assert_eq!(summaries, 7, "{nav}");
    // the ordinary link to the folder's page stands beside the summary
    for (title, href) in [
        ("Area", "../../index.html"),
        ("Mid", "../index.html"),
        ("Deep", "index.html"),
        ("Deeper", "deeper/index.html"),
        ("Side", "../side/index.html"),
        ("Empty", "../../empty/index.html"),
        ("Other", "../../other/index.html"),
    ] {
        // Deep is a folder on the path; the page is the doc Leaf, so no link carries the mark
        let link = format!(
            "<a class=\"nav-go\" href=\"{href}\" aria-label=\"Go to {title}\">\u{2197}</a><details"
        );
        assert!(nav.contains(&link), "{link} in {nav}");
    }
    assert_eq!(nav.matches('\u{2197}').count(), 7, "{nav}");
    // the current doc keeps its mark, and a current folder carries it on its link
    assert_eq!(nav.matches("aria-current=\"page\"").count(), 1, "{nav}");
    assert!(nav.contains("aria-current=\"page\">Leaf</a>"), "{nav}");
    let at_folder = nav_of(&html(out.path(), "area/mid/index.html"));
    assert!(
        at_folder.contains(
            "<a class=\"nav-go\" href=\"index.html\" aria-label=\"Go to Mid\" aria-current=\"page\">\u{2197}</a>"
        ),
        "{at_folder}"
    );
    assert_eq!(
        at_folder.matches("aria-current=\"page\"").count(),
        1,
        "{at_folder}"
    );
}

#[test]
fn dgm8_sidebar_works_without_script() {
    let (_d, _r, out) = deep_site();
    let page = html(out.path(), "area/mid/deep/leaf.html");
    let nav = nav_of(&page);
    // every folder is a native disclosure; the top-level group and the path are open, the rest
    // closed, and nothing in the markup needs the script
    let details: Vec<&str> = nav
        .match_indices("<details")
        .map(|(i, _)| &nav[i..i + nav[i..].find('>').unwrap()])
        .collect();
    let open: Vec<&&str> = details.iter().filter(|d| d.ends_with(" open")).collect();
    assert_eq!(details.len(), 7, "{nav}");
    assert_eq!(open.len(), 3, "Area, Mid and Deep are open: {nav}");
    for t in ["Area", "Mid", "Deep"] {
        assert!(
            nav.contains(&format!("<summary>{t}</summary>"))
                || nav.contains(&format!(
                    "<span class=\"nav-group-label\">{t}</span></summary>"
                )),
            "{t}: {nav}"
        );
    }
    for gone in ["onclick", "hidden", "style=", "<script", "javascript:"] {
        assert!(!nav.contains(gone), "{gone}: {nav}");
    }
    // the closed folders still hold their links, so a reader without script reaches them
    for label in [
        ">Bottom</a>",
        ">Sidedeep</a>",
        ">Side One</a>",
        ">Other One</a>",
        ">Oo</a>",
    ] {
        assert!(nav.contains(label), "{label}: {nav}");
    }
    // one extra level: what a folder off the path holds is a list of page links
    for gone in [">Sidedeep One</a>", ">Oo One</a>"] {
        assert!(!nav.contains(gone), "{gone}: {nav}");
    }
    // an empty folder is a disclosure too
    assert!(
        nav.contains("aria-label=\"Go to Empty\">\u{2197}</a><details class=\"nav-fold\"><summary>Empty</summary></details>"),
        "{nav}"
    );
    // the sheet hides nothing of the list, whatever the width
    let css = css(out.path());
    for w in [320, 900, 1200] {
        for sel in [
            ".nav-sub",
            ".nav-root .nav-sub",
            ".nav-fold",
            "details.nav-fold",
        ] {
            let v = rdy_qa::site::cascaded(&css, sel, "display", w);
            assert!(v.as_deref() != Some("none"), "{sel} at {w}: {v:?}");
        }
    }
    // the script is a plain, deferred-by-order part of app.js: after menu.js, nothing inline
    let app = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    let menu = app.find("The version menu, on the live site only").unwrap();
    let sidebar = app
        .find("The sidebar's folders")
        .expect("sidebar.js in app.js");
    assert!(menu < sidebar);
    assert!(!page.contains("rdy:sidebar"), "no inline state script");
}

/// Final review M4: a folder off the path (here `Big`, inside the top-level group `Area`) holds
/// its first level only, cut to 50 entries from its start, then a "+N more" row that leads to
/// the folder's page. A folder among them is a plain page link, and nothing it holds is listed.
#[test]
fn dgm8_an_off_path_folder_lists_at_most_fifty_children() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "parent": "$intro", "fields": {"title": "Summary"}}),
        json!({"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}}),
        json!({"op": "create", "type": "folder", "id": "$big", "parent": "$area", "fields": {"name": "big", "title": "Big"}}),
        json!({"op": "create", "type": "folder", "id": "$inner", "parent": "$big", "fields": {"name": "inner", "title": "Inner"}}),
        json!({"op": "create", "type": "doc", "id": "$inside", "parent": "$inner", "fields": {"name": "inside", "title": "Inside", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "parent": "$inside", "fields": {"title": "Summary"}}),
    ];
    for i in 0..52 {
        let id = format!("$d{i}");
        ops.push(json!({"op": "create", "type": "doc", "id": id, "parent": "$big", "fields": {"name": format!("d{i:02}"), "title": format!("Child {i:02}"), "doc_type": "overview"}}));
        ops.push(json!({"op": "create", "type": "section", "parent": id, "fields": {"title": "Summary"}}));
    }
    write(&mut r, "work", Value::Array(ops)).expect("the big folder");
    land(&mut r, "work").expect("land");
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let nav = nav_of(&html(out.path(), "intro.html"));
    // the folder is closed, and its list is its first 50 children, then the more row
    let fold = "<a class=\"nav-go\" href=\"area/big/index.html\" aria-label=\"Go to Big\">\u{2197}</a><details class=\"nav-fold\"><summary>Big</summary><ul class=\"nav-sub\">";
    let at = nav.find(fold).unwrap_or_else(|| panic!("{nav}"));
    let list = &nav[at + fold.len()..];
    let list = &list[..list.find("</ul>").expect("the list ends")];
    assert_eq!(list.matches("<li").count(), 51, "{list}");
    assert!(
        list.ends_with("<li class=\"more\"><a href=\"area/big/index.html\">+3 more</a></li>"),
        "{list}"
    );
    // the folder among them is a page link, not a disclosure, and what it holds is not listed
    assert!(
        list.starts_with(
            "<li class=\"nav-item\"><a href=\"area/big/inner/index.html\">Inner</a></li>"
        ),
        "{list}"
    );
    assert!(
        !list.contains("<details") && !list.contains("nav-go"),
        "{list}"
    );
    assert!(!nav.contains(">Inside</a>"), "{nav}");
    for i in 0..49 {
        assert!(list.contains(&format!(">Child {i:02}</a>")), "{i}: {list}");
    }
    for i in 49..52 {
        assert!(!nav.contains(&format!(">Child {i:02}</a>")), "{i}: {nav}");
    }
}

// ---- the script, in node ----------------------------------------------------------------

/// A just-enough DOM for `sidebar.js`: the sidebar's own markup parsed into elements, a
/// `sessionStorage` that can be denied, and a log of every listener, timer, request, cookie
/// write and navigation. A scenario runs as the body of a function over `h`:
/// `h.page(name, url, opts)` loads a page's sidebar and runs the script; `h.storage` is the
/// session's storage, shared by the pages loaded after one another.
const HARNESS: &str = r##"
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(0, 'utf8'));
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
const log = { listeners: [], timers: 0, requests: 0, cookies: 0, navigations: [] };

function El(tag, attrs) {
  this.tagName = tag.toUpperCase(); this.attrs = attrs || {}; this.nodes = []; this.parentNode = null; this.listeners = {};
  this.scrolled = [];
}
El.prototype = {
  get children() { return this.nodes.filter((n) => n instanceof El); },
  get textContent() { return this.nodes.map((n) => (n instanceof El ? n.textContent : n)).join(''); },
  get previousElementSibling() {
    const sibs = this.parentNode ? this.parentNode.children : [];
    const i = sibs.indexOf(this);
    return i > 0 ? sibs[i - 1] : null;
  },
  get open() { return 'open' in this.attrs; },
  set open(v) { if (v) this.attrs.open = ''; else delete this.attrs.open; },
  getAttribute(n) { return n in this.attrs ? this.attrs[n] : null; },
  hasAttribute(n) { return n in this.attrs; },
  setAttribute(n, v) { this.attrs[n] = String(v); },
  removeAttribute(n) { delete this.attrs[n]; },
  contains(o) { for (let e = o; e; e = e.parentNode) if (e === this) return true; return false; },
  matches(sel) {
    const m = /^([a-zA-Z]*)((?:\.[\w-]+|\[[^\]]+\])*)$/.exec(sel.trim());
    if (!m) throw new Error('unsupported selector ' + sel);
    if (m[1] && this.tagName !== m[1].toUpperCase()) return false;
    const parts = m[2].match(/\.[\w-]+|\[[^\]]+\]/g) || [];
    return parts.every((p) => {
      if (p[0] === '.') return (this.attrs.class || '').split(/\s+/).includes(p.slice(1));
      const a = /^\[([\w-]+)(?:="([^"]*)")?\]$/.exec(p);
      if (!(a[1] in this.attrs)) return false;
      return a[2] === undefined || this.attrs[a[1]] === a[2];
    });
  },
  querySelectorAll(sel) {
    const out = [];
    const walk = (e) => { for (const c of e.children) { if (c.matches(sel)) out.push(c); walk(c); } };
    walk(this); return out;
  },
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; },
  addEventListener(type, fn) {
    (this.listeners[type] = this.listeners[type] || []).push(fn);
    log.listeners.push(this.tagName + ':' + type);
  },
  scrollIntoView(arg) { this.scrolled.push(arg); scrollLog.push({ text: this.textContent, arg }); },
};
let scrollLog = [];

function parse(html) {
  const root = new El('root');
  let cur = root;
  const re = /<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:\s+[\w-]+(?:="[^"]*")?)*)\s*>|([^<]+)/g;
  let m;
  while ((m = re.exec(html))) {
    if (m[4] !== undefined) { cur.nodes.push(m[4].replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#x27;/g, "'")); continue; }
    if (m[1]) { cur = cur.parentNode; continue; }
    const attrs = {};
    for (const a of m[3].matchAll(/([\w-]+)(?:="([^"]*)")?/g)) attrs[a[1]] = a[2] === undefined ? '' : a[2];
    const e = new El(m[2], attrs); e.parentNode = cur; cur.nodes.push(e); cur = e;
  }
  return root;
}

function Storage(deny) {
  const data = new Map();
  this.data = data; this.sets = 0;
  this.getItem = (k) => { if (deny === 'get') throw new Error('denied'); return data.has(k) ? data.get(k) : null; };
  this.setItem = (k, v) => { if (deny === 'set') throw new Error('denied'); this.sets++; data.set(k, String(v)); };
  this.removeItem = (k) => { data.delete(k); };
}

const h = { storage: new Storage(), out: {}, log };
h.newStorage = (deny) => new Storage(deny);
h.page = function (name, url, opts) {
  opts = opts || {};
  scrollLog = [];
  const root = parse(spec.pages[name]);
  const docListeners = {}, winListeners = {};
  const location = { get href() { return url; }, set href(v) { log.navigations.push(v); }, assign: (v) => log.navigations.push(v), replace: (v) => log.navigations.push(v) };
  const fail = () => { log.requests++; throw new Error('no request'); };
  const document = {
    readyState: 'loading',
    querySelector: (s) => root.querySelector(s),
    querySelectorAll: (s) => root.querySelectorAll(s),
    addEventListener: (t, f) => { (docListeners[t] = docListeners[t] || []).push(f); log.listeners.push('document:' + t); },
    get cookie() { log.cookies++; return ''; }, set cookie(v) { log.cookies++; },
  };
  const storage = opts.storage || h.storage;
  const window = {
    location, URL, document,
    addEventListener: (t, f) => { (winListeners[t] = winListeners[t] || []).push(f); log.listeners.push('window:' + t); },
    fetch: fail, XMLHttpRequest: function () { fail(); },
    setTimeout: () => { log.timers++; return 0; }, setInterval: () => { log.timers++; return 0; },
    requestAnimationFrame: () => { log.timers++; return 0; },
    navigator: { sendBeacon: fail },
  };
  if (opts.deny === 'access') Object.defineProperty(window, 'sessionStorage', { get() { throw new Error('SecurityError'); } });
  else window.sessionStorage = storage;
  new Function('window', 'document', src)(window, document);
  const page = {
    root, document, window,
    ready() { document.readyState = 'interactive'; (docListeners.DOMContentLoaded || []).forEach((f) => f()); },
    hide() { (winListeners.pagehide || []).forEach((f) => f()); },
    details(label) {
      const d = root.querySelectorAll('details').find((d) => d.children[0].textContent === label);
      if (!d) throw new Error('no folder ' + label);
      return d;
    },
    labels() { return root.querySelectorAll('details').map((d) => d.children[0].textContent); },
    openLabels() { return root.querySelectorAll('details').filter((d) => d.open).map((d) => d.children[0].textContent); },
    // what the browser does: the open state changes, then the details fires toggle
    toggle(label, open) {
      const d = page.details(label);
      d.open = open === undefined ? !d.open : open;
      (d.listeners.toggle || []).forEach((f) => f());
    },
    click(label) {
      const s = page.details(label).children[0];
      (s.listeners.click || []).forEach((f) => f({ target: s, preventDefault() { log.prevented = true; } }));
      (docListeners.click || []).forEach((f) => f({ target: s, preventDefault() { log.prevented = true; } }));
      page.toggle(label);
    },
    scrolled() { return scrollLog.slice(); },
    stored() {
      const keys = [...storage.data.keys()];
      return keys.map((k) => [k, storage.data.get(k)]);
    },
  };
  return page;
};
new Function('h', spec.scenario)(h);
h.out.log = log;
console.log(JSON.stringify(h.out));
"##;

/// Runs `scenario` (the body of a function over `h`, see [`HARNESS`]) against the sidebars of
/// `pages` with `sidebar.js`, and returns what it filled into `h.out`.
fn run(pages: Value, scenario: &str) -> Value {
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/sidebar.js"
    );
    let spec = json!({"pages": pages, "scenario": scenario}).to_string();
    let mut child = std::process::Command::new("node")
        .args(["-e", HARNESS, "--", js])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(spec.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// The node tests run only where node is installed.
fn node_or_skip(test: &str) -> bool {
    if rdy_qa::skip::node_available() {
        return true;
    }
    rdy_qa::skip::skip(
        test,
        rdy_qa::skip::Why::Prerequisite,
        "node is not installed",
    );
    false
}

/// The sidebars of the deep site's pages, by page path.
fn sidebars(out: &std::path::Path, paths: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for p in paths {
        m.insert((*p).into(), nav_of(&html(out, p)).into());
    }
    m.into()
}

const SIDE_ONE: &str = "area/mid/side/s-one.html";
const LEAF: &str = "area/mid/deep/leaf.html";
const HOME_KEY: &str = "rdy:sidebar:1:http://h/index.html";

#[test]
fn dgm8_sidebar_state_restores_open_paths_and_current_entry() {
    if !node_or_skip("dgm8_sidebar_state_restores_open_paths_and_current_entry") {
        return;
    }
    let (_d, _r, out) = deep_site();
    let pages = sidebars(out.path(), &[SIDE_ONE, LEAF]);
    let v = run(
        pages,
        &format!(
            r#"
        const A = 'http://h/{SIDE_ONE}', B = 'http://h/{LEAF}';
        // page A: Sidedeep is a folder here; the reader opens it and Other, and leaves
        let a = h.page('{SIDE_ONE}', A); a.ready();
        h.out.aDefault = a.openLabels();
        h.out.aFirstScroll = a.scrolled();
        a.toggle('Sidedeep', true); a.toggle('Other', true); a.hide();
        h.out.afterA = a.stored();
        // page B: Sidedeep is only a link in its neighbourhood, so it has no folder to restore
        let b = h.page('{LEAF}', B); b.ready();
        h.out.bLabels = b.labels();
        h.out.bOpen = b.openLabels();
        b.toggle('Deeper', true); b.toggle('Other', false); b.hide();
        h.out.afterB = b.stored();
        // back on A
        let a2 = h.page('{SIDE_ONE}', A); a2.ready();
        h.out.a2Open = a2.openLabels();
        h.out.a2Scroll = a2.scrolled();
        // a reader who closed the page's own folder finds the path open again
        a2.toggle('Side', false); a2.toggle('Mid', false); a2.hide();
        let a3 = h.page('{SIDE_ONE}', A); a3.ready();
        h.out.a3Open = a3.openLabels();
        h.out.a3Stored = a3.stored();
    "#
        ),
    );
    // by default: the top-level group and the path
    assert_eq!(v["aDefault"], json!(["Area", "Mid", "Side"]), "{v}");
    // the page scrolls its current entry into view, nearest, on the first load as well
    assert_eq!(
        v["aFirstScroll"],
        json!([{"text": "Side One", "arg": {"block": "nearest"}}]),
        "{v}"
    );
    let urls = |names: &[&str]| -> Vec<String> {
        let mut u: Vec<String> = names.iter().map(|n| format!("http://h/{n}")).collect();
        u.sort();
        u
    };
    // A saved what it showed open: its group, its path, and the two the reader opened
    let after_a = &v["afterA"];
    assert_eq!(after_a.as_array().unwrap().len(), 1, "one key only: {v}");
    assert_eq!(after_a[0][0], HOME_KEY);
    let saved_a: Value = serde_json::from_str(after_a[0][1].as_str().unwrap()).unwrap();
    assert_eq!(
        saved_a,
        json!({"open": urls(&[
            "area/index.html",
            "area/mid/index.html",
            "area/mid/side/index.html",
            "area/mid/side/sidedeep/index.html",
            "area/other/index.html",
        ])}),
        "sorted, unique, absolute: {v}"
    );
    // B has no Sidedeep folder; it keeps the group and its path open, and not Other (closed
    // by the reader here), and it never forgets the paths of folders it does not show
    assert!(
        !v["bLabels"]
            .as_array()
            .unwrap()
            .contains(&json!("Sidedeep")),
        "{v}"
    );
    assert_eq!(
        v["bOpen"],
        json!(["Area", "Mid", "Deep", "Side", "Other"]),
        "{v}"
    );
    let saved_b: Value = serde_json::from_str(v["afterB"][0][1].as_str().unwrap()).unwrap();
    assert_eq!(
        saved_b,
        json!({"open": urls(&[
            "area/index.html",
            "area/mid/deep/deeper/index.html",
            "area/mid/deep/index.html",
            "area/mid/index.html",
            "area/mid/side/sidedeep/index.html",
            "area/mid/side/index.html",
        ])}),
        "{v}"
    );
    // back on A: Sidedeep is open again from B's untouched memory; Other is closed
    assert_eq!(
        v["a2Open"],
        json!(["Area", "Mid", "Deep", "Side", "Sidedeep"]),
        "{v}"
    );
    assert_eq!(
        v["a2Scroll"],
        json!([{"text": "Side One", "arg": {"block": "nearest"}}]),
        "{v}"
    );
    // the path is open whatever was stored
    assert_eq!(
        v["a3Open"],
        json!(["Area", "Mid", "Deep", "Side", "Sidedeep"]),
        "{v}"
    );
    assert_eq!(v["log"]["requests"], 0);
    assert_eq!(v["log"]["cookies"], 0);
    assert_eq!(v["log"]["timers"], 0);
}

#[test]
fn dgm8_sidebar_state_is_bounded_and_scoped() {
    if !node_or_skip("dgm8_sidebar_state_is_bounded_and_scoped") {
        return;
    }
    let (_d, _r, out) = deep_site();
    let pages = sidebars(out.path(), &[SIDE_ONE]);
    let v = run(
        pages,
        &format!(
            r#"
        const P = '{SIDE_ONE}';
        const key = (u) => 'rdy:sidebar:1:' + u;
        const state = (s, u) => s.data.get(key(u));
        const OTHER = 'http://h/area/other/index.html';
        // scoped: another origin, a live scope prefix and a file site each have a key
        const sites = ['http://h/', 'http://h/~scopes/draft/', 'https://other.example/docs/', 'file:///tmp/site/'];
        for (const base of sites) {{
          const s = h.newStorage();
          const p = h.page(P, base + P, {{ storage: s }}); p.ready(); p.toggle('Other', true); p.hide();
          h.out['keys ' + base] = [...s.data.keys()];
        }}
        // a state kept under one home is not read under another
        const shared = h.newStorage();
        shared.data.set(key('http://h/index.html'), JSON.stringify({{ open: ['http://h/area/other/index.html'] }}));
        const elsewhere = h.page(P, 'http://h/~scopes/draft/' + P, {{ storage: shared }}); elsewhere.ready();
        h.out.elsewhereOpen = elsewhere.openLabels();
        const here = h.page(P, 'http://h/' + P, {{ storage: shared }}); here.ready();
        h.out.hereOpen = here.openLabels();

        // state that is not ours to read is ignored: the native defaults stand
        const bad = {{
          malformed: '{{"open": [',
          wrongType: '[1,2]',
          notAnArray: JSON.stringify({{ open: 'http://h/area/other/index.html' }}),
          notStrings: JSON.stringify({{ open: [1] }}),
          tooMany: JSON.stringify({{ open: Array.from({{ length: 257 }}, (_, i) => 'http://h/f' + String(i).padStart(4, '0')) }}),
          longEntry: JSON.stringify({{ open: ['http://h/' + 'x'.repeat(2048)] }}),
          oversized: JSON.stringify({{ open: [], pad: 'x'.repeat(65536) }}),
          nonAscii: JSON.stringify({{ open: ['http://h/é'] }}),
          // each of these would open Other if the state were taken: the whole state is refused
          multibyte: JSON.stringify({{ open: [OTHER], pad: 'é'.repeat(40000) }}),
          notUrl: JSON.stringify({{ open: [OTHER, 'abcdef'] }}),
          relative: JSON.stringify({{ open: [OTHER, '/area/other/index.html'] }}),
          otherScheme: JSON.stringify({{ open: [OTHER, 'javascript:alert(1)'] }}),
          quote: JSON.stringify({{ open: [OTHER, 'http://h/a"b'] }}),
          backslash: JSON.stringify({{ open: [OTHER, 'http://h/a\\b'] }}),
          notCanonical: JSON.stringify({{ open: [OTHER, 'HTTP://H/area/mid/../x'] }}),
        }};
        h.out.ignored = {{}};
        for (const [name, raw] of Object.entries(bad)) {{
          const s = h.newStorage(); s.data.set(key('http://h/index.html'), raw);
          const p = h.page(P, 'http://h/' + P, {{ storage: s }}); p.ready();
          h.out.ignored[name] = p.openLabels();
        }}
        // the limits themselves are accepted: 256 entries of 2048 bytes would pass the entry
        // limits but not the size limit, so the largest accepted state is what fits in 65536
        {{
          const s = h.newStorage();
          const ok = Array.from({{ length: 256 }}, (_, i) => 'http://h/' + String(i).padStart(4, '0'));
          s.data.set(key('http://h/index.html'), JSON.stringify({{ open: ok }}));
          const p = h.page(P, 'http://h/' + P, {{ storage: s }}); p.ready();
          h.out.at256 = p.openLabels();
        }}

        // the size limit counts bytes of the stored text: exactly 65536 is read, one more is not
        {{
          const mk = (extra) => {{
            const open = Array.from({{ length: 31 }}, (_, i) => 'http://h/' + String(i).padStart(4, '0') + 'z'.repeat(2048 - 13));
            open.push(OTHER);
            open.push('http://h/last');
            const fixed = Buffer.byteLength(JSON.stringify({{ open }}));
            open[open.length - 1] += 'q'.repeat(65536 - fixed + extra);
            return JSON.stringify({{ open }});
          }};
          for (const [name, extra] of [['atLimit', 0], ['overLimit', 1]]) {{
            const raw = mk(extra);
            h.out[name + 'Bytes'] = Buffer.byteLength(raw);
            const s = h.newStorage(); s.data.set(key('http://h/index.html'), raw);
            const p = h.page(P, 'http://h/' + P, {{ storage: s }}); p.ready();
            h.out[name] = p.openLabels();
          }}
        }}

        // more than 256 once merged: the path first, then the lowest of the rest (Other, opened
        // just now, sorts after the stored ones and is the one that does not fit)
        {{
          const s = h.newStorage();
          const stored = Array.from({{ length: 256 }}, (_, i) => 'http://h/' + String(i).padStart(4, '0'));
          s.data.set(key('http://h/index.html'), JSON.stringify({{ open: stored }}));
          const p = h.page(P, 'http://h/' + P, {{ storage: s }}); p.ready();
          p.toggle('Other', true); p.hide();
          const kept = JSON.parse(state(s, 'http://h/index.html')).open;
          h.out.cap = {{
            count: kept.length,
            sorted: kept.join() === [...kept].sort().join(),
            unique: new Set(kept).size === kept.length,
            path: ['area/index.html', 'area/mid/index.html', 'area/mid/side/index.html'].map((u) => kept.includes('http://h/' + u)),
            other: kept.includes('http://h/area/other/index.html'),
            lowestKept: kept.includes('http://h/0000') && kept.includes('http://h/0252'),
            highestDropped: !kept.includes('http://h/0253') && !kept.includes('http://h/0255'),
          }};
        }}

        // the size limit holds on a write: long paths keep the saved text under 65536 bytes
        {{
          const s = h.newStorage();
          const base = 'http://h/' + 'p'.repeat(900) + '/';
          const stored = Array.from({{ length: 31 }}, (_, i) => 'http://h/' + String(i).padStart(4, '0') + 'z'.repeat(2030));
          s.data.set(key(base + 'index.html'), JSON.stringify({{ open: stored }}));
          const p = h.page(P, base + P, {{ storage: s }}); p.ready();
          for (const l of ['Other', 'Deep', 'Empty', 'Sidedeep', 'Oo']) if (p.labels().includes(l)) p.toggle(l, true);
          p.hide();
          const raw = s.data.get(key(base + 'index.html'));
          const kept = JSON.parse(raw).open;
          h.out.size = {{
            length: Buffer.byteLength(raw),
            path: ['area/index.html', 'area/mid/index.html', 'area/mid/side/index.html'].map((u) => kept.includes(base + u)),
            entries: kept.length,
            parses: Object.keys(JSON.parse(raw)).join(),
          }};
        }}
    "#
        ),
    );
    for base in [
        "http://h/",
        "http://h/~scopes/draft/",
        "https://other.example/docs/",
        "file:///tmp/site/",
    ] {
        // the page's Home link is relative; the key is the absolute address of the home page
        assert_eq!(
            v[format!("keys {base}")],
            json!([format!("rdy:sidebar:1:{base}index.html")]),
            "{v}"
        );
    }
    assert_eq!(v["elsewhereOpen"], json!(["Area", "Mid", "Side"]), "{v}");
    assert_eq!(
        v["hereOpen"],
        json!(["Area", "Mid", "Side", "Other"]),
        "{v}"
    );
    for name in [
        "malformed",
        "wrongType",
        "notAnArray",
        "notStrings",
        "tooMany",
        "longEntry",
        "oversized",
        "nonAscii",
        "multibyte",
        "notUrl",
        "relative",
        "otherScheme",
        "quote",
        "backslash",
        "notCanonical",
    ] {
        assert_eq!(
            v["ignored"][name],
            json!(["Area", "Mid", "Side"]),
            "{name} must be ignored: {v}"
        );
    }
    // an accepted state is applied; here none of its paths is a folder of the page, so the page
    // keeps the native defaults of its path
    assert_eq!(v["atLimitBytes"], 65536, "{v}");
    assert_eq!(v["overLimitBytes"], 65537, "{v}");
    assert_eq!(v["atLimit"], json!(["Area", "Mid", "Side", "Other"]), "{v}");
    assert_eq!(v["overLimit"], json!(["Area", "Mid", "Side"]), "{v}");
    assert_eq!(v["at256"], json!(["Area", "Mid", "Side"]), "{v}");
    assert_eq!(
        v["cap"],
        json!({
            "count": 256, "sorted": true, "unique": true,
            "path": [true, true, true], "other": false,
            "lowestKept": true, "highestDropped": true,
        }),
        "{v}"
    );
    assert!(v["size"]["length"].as_u64().unwrap() <= 65536, "{v}");
    assert_eq!(v["size"]["path"], json!([true, true, true]), "{v}");
    assert_eq!(v["size"]["parses"], "open", "{v}");
}

#[test]
fn dgm8_storage_denial_keeps_native_navigation() {
    if !node_or_skip("dgm8_storage_denial_keeps_native_navigation") {
        return;
    }
    let (_d, _r, out) = deep_site();
    let pages = sidebars(out.path(), &[SIDE_ONE]);
    let v = run(
        pages,
        &format!(
            r#"
        const P = '{SIDE_ONE}', U = 'http://h/' + P;
        for (const deny of ['access', 'get', 'set']) {{
          const p = h.page(P, U, {{ deny, storage: h.newStorage(deny === 'access' ? undefined : deny) }});
          p.ready();
          const before = p.openLabels();
          // a reader's toggles and leaving the page do not throw, and the folders stay as set
          p.toggle('Other', true); p.click('Empty'); p.hide();
          h.out[deny] = {{ before, after: p.openLabels(), scrolled: p.scrolled() }};
        }}
        // a click on a summary toggles and nothing else: no handler of ours, no navigation
        const q = h.page(P, U); q.ready();
        q.click('Other');
        h.out.click = q.openLabels();
    "#
        ),
    );
    for deny in ["access", "get", "set"] {
        let r = &v[deny];
        assert_eq!(r["before"], json!(["Area", "Mid", "Side"]), "{deny}: {v}");
        assert_eq!(
            r["after"],
            json!(["Area", "Mid", "Side", "Empty", "Other"]),
            "{deny}: {v}"
        );
        // the current entry is still scrolled into view
        assert_eq!(
            r["scrolled"],
            json!([{"text": "Side One", "arg": {"block": "nearest"}}]),
            "{deny}: {v}"
        );
    }
    assert_eq!(v["click"], json!(["Area", "Mid", "Side", "Other"]), "{v}");
    let log = &v["log"];
    assert_eq!(log["navigations"], json!([]), "{v}");
    assert!(log.get("prevented").is_none(), "{v}");
    assert_eq!(log["requests"], 0, "no fetch or XHR: {v}");
    assert_eq!(log["timers"], 0, "no timer: {v}");
    assert_eq!(log["cookies"], 0, "no cookie: {v}");
    // the only listeners are the page's start, each folder's toggle and the page's leaving: no
    // click, no key, no hash change
    let kinds: std::collections::BTreeSet<String> = log["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "DETAILS:toggle",
            "document:DOMContentLoaded",
            "window:pagehide"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        "{v}"
    );
}
