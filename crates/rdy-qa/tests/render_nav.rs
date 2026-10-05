//! Left nav, breadcrumbs, toc, previous/next (nav1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn nav1_groups_follow_top_level_folders_and_mark_the_current_page() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert!(
        nav.contains("<span class=\"nav-group-label\">Beta</span>")
            && nav.contains("<span class=\"nav-group-label\">Alpha</span>")
            && nav.contains("<span class=\"nav-group-label\">Reference</span>"),
        "{nav}"
    );
    assert!(nav.contains("aria-current=\"page\"") && nav.contains(">Three</a>"));
    assert!(
        nav.contains("href=\"../../index.html\">Overview</a>")
            && nav.contains("href=\"../../intro.html\">Intro</a>")
    );
    assert!(
        nav.contains("href=\"../../glossary.html\">Glossary</a>")
            && nav.contains("href=\"../../questions.html\">Open questions</a>")
    );
    assert!(
        nav.contains(">One</a>") && nav.contains(">Two</a>"),
        "a top-level folder always lists its direct children: {nav}"
    );
    assert_eq!(
        nav.matches("class=\"nav-sub\"").count(),
        1,
        "only Gamma, on the path, is expanded: {nav}"
    );
    let one = std::fs::read_to_string(out.path().join("a/one.html")).unwrap();
    let nav1 = &one[one.find("<nav aria-label=\"Pages\">").unwrap()..one.find("</aside>").unwrap()];
    assert!(
        nav1.contains(">Gamma</a>") && !nav1.contains(">Three</a>"),
        "Gamma is a bare entry off the path: {nav1}"
    );
}

#[test]
fn nav1_crumbs_start_with_the_project_name_and_end_with_the_page() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    let s = h.find("<nav class=\"crumbs\"").unwrap();
    let crumbs = &h[s..s + h[s..].find("</nav>").unwrap()];
    let texts: Vec<&str> = crumbs
        .split('>')
        .filter_map(|s| s.split('<').next())
        .filter(|s| !s.trim().is_empty())
        .collect();
    assert_eq!(texts, ["Tree", "Beta", "Gamma", "Three"]);
    assert!(crumbs.contains("<span aria-current=\"page\">Three</span>"));
}

#[test]
fn nav1_the_toc_appears_at_three_sections_and_follows_depth() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let three = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    assert!(
        three.contains("<nav aria-label=\"On this page\">")
            && three.contains("<p class=\"toc-label\">On this page</p>")
    );
    assert_eq!(three.matches("class=\"toc-link\"").count(), 5);
    let one = std::fs::read_to_string(out.path().join("a/one.html")).unwrap();
    assert!(
        !one.contains("aria-label=\"On this page\""),
        "one section: no toc"
    );
}

#[test]
fn nav1_previous_and_next_follow_tree_order_on_doc_pages_only() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let two = std::fs::read_to_string(out.path().join("a/two.html")).unwrap();
    assert!(
        two.contains("class=\"pager-prev\" href=\"one.html\"")
            && two.contains("<span class=\"pager-title\">One</span>")
    );
    assert!(two.contains("class=\"pager-next\" href=\"../b/c/three.html\""));
    let intro = std::fs::read_to_string(out.path().join("intro.html")).unwrap();
    assert!(
        intro.contains("class=\"pager-next\" href=\"a/one.html\"") && !intro.contains("pager-prev")
    );
    let folder = std::fs::read_to_string(out.path().join("a/index.html")).unwrap();
    assert!(!folder.contains("aria-label=\"Previous and next\""));
}

/// Review Focus 4.
#[test]
fn nav1_absent_collected_pages_are_not_linked() {
    let (_d, mut r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        !h.contains("glossary.html") && !h.contains("questions.html") && !h.contains(">Reference<")
    );
}

#[test]
fn nav1_the_sidebar_caps_each_list_at_50_with_a_more_link() {
    let (_d, mut r) = wide_folder(120); // folder "many" with 120 docs
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("many/doc-60.html")).unwrap();
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert_eq!(
        nav.matches("class=\"nav-item").count(),
        50 + 1,
        "50 siblings plus Overview; the folder is the group label"
    );
    assert!(
        nav.contains("class=\"more\"")
            && nav.contains(">+70 more</a>")
            && nav.contains("href=\"index.html\"")
    );
    assert!(nav.contains("aria-current=\"page\"") && nav.contains(">Doc 60</a>"));
}

/// A just-enough DOM and `IntersectionObserver` for `toc.js`, driven by a JSON description of the
/// page: `{ links: [anchor…], blocks: [{ id, children: [{ tag, id, cls }…] }…] }`. The scroll-spy
/// observes the heading each toc link points at and reports which link ended up marked.
const SPY_HARNESS: &str = r##"
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(0, 'utf8'));
function El(tag, id, cls) {
  this.tagName = tag.toUpperCase(); this.id = id || ''; this.className = cls || '';
  this.children = []; this.attrs = {}; this._cls = new Set();
  const self = this;
  this.classList = { add: (c) => self._cls.add(c), remove: (c) => self._cls.delete(c), contains: (c) => self._cls.has(c) };
}
El.prototype.getAttribute = function (n) { return this.attrs[n] === undefined ? null : this.attrs[n]; };
El.prototype.querySelectorAll = function (sel) {
  const out = [];
  const walk = (e) => { for (const c of e.children) { if (match(c, sel)) out.push(c); walk(c); } };
  walk(this); return out;
};
El.prototype.querySelector = function (sel) { return this.querySelectorAll(sel)[0] || null; };
function match(e, sel) {
  return sel.split(',').some((s) => {
    s = s.trim();
    if (s === 'a[href^="#"]') return e.tagName === 'A' && (e.attrs.href || '').startsWith('#');
    const m = /^h([1-6])\[id\]$/.exec(s);
    return !!m && e.tagName === 'H' + m[1] && !!e.id;
  });
}
const index = new El('nav'), content = new El('article');
const byId = {};
const all = [];
for (const a of spec.links) { const l = new El('a'); l.attrs.href = '#' + a; index.children.push(l); all.push(l); }
for (const b of spec.blocks) {
  const d = new El('div', b.id, 'block block-section'); content.children.push(d); byId[b.id] = d;
  for (const c of b.children) { const e = new El(c.tag, c.id, c.cls); d.children.push(e); }
}
const document = {
  readyState: 'complete',
  querySelector: (s) => s === 'nav[aria-label="On this page"]' ? index : s === '[data-rdy="content"]' ? content : null,
  getElementById: (id) => byId[id] || null,
  addEventListener: () => {},
};
let observed = [], callback = null;
function IntersectionObserver(cb) { callback = cb; this.observe = (e) => observed.push(e); }
const window = { IntersectionObserver };
new Function('window', 'document', 'IntersectionObserver', fs.readFileSync(process.argv[process.argv.length - 1], 'utf8'))(window, document, IntersectionObserver);
const report = { observed: observed.map((e) => e.tagName + ':' + (e.cls || e.className)), marked: () => all.map((l, i) => l.classList.contains('is-active') ? i : -1).filter((i) => i >= 0) };
const out = { observed: observed.map((e) => e.tagName + '.' + e.className) };
if (callback && observed.length) {
  // the reader is in the band of the 2nd heading, then of the 3rd
  const at = (i) => callback([{ target: observed[i], isIntersecting: true }]);
  at(1); out.afterSecond = report.marked();
  callback([{ target: observed[1], isIntersecting: false }]); at(2); out.afterThird = report.marked();
}
console.log(JSON.stringify(out));
"##;

#[test]
fn nav1_the_scroll_spy_observes_the_heading_each_toc_link_points_at() {
    let node = std::process::Command::new("node").arg("--version").output();
    if node.is_err() {
        eprintln!(
            "node is not installed: the scroll-spy harness is skipped (Task 16 reviews it in a browser)"
        );
        return;
    }
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    // The page as rendered: every toc link's target and the first heading that follows it.
    let toc = &h[h.find("<nav aria-label=\"On this page\">").unwrap()..];
    let toc = &toc[..toc.find("</nav>").unwrap()];
    let links: Vec<String> = toc
        .split("class=\"toc-link\" href=\"#")
        .skip(1)
        .map(|s| s[..s.find('"').unwrap()].to_string())
        .collect();
    assert_eq!(links.len(), 5);
    let mut blocks = Vec::new();
    for a in &links {
        let at = h
            .find(&format!("id=\"{a}\""))
            .unwrap_or_else(|| panic!("no element has id {a}"));
        let after = &h[at..];
        let open = after
            .find("<h")
            .expect("a heading follows the section anchor");
        let tag = &after[open..open + after[open..].find('>').unwrap()];
        let name = &tag[1..3];
        let cls = tag
            .split("class=\"")
            .nth(1)
            .map(|s| s[..s.find('"').unwrap()].to_string())
            .unwrap_or_default();
        let hid = tag
            .split(" id=\"")
            .nth(1)
            .map(|s| s[..s.find('"').unwrap()].to_string())
            .unwrap_or_default();
        // a non-heading sibling first, like the block's id-anchor span, must not confuse it
        blocks.push(serde_json::json!({"id": a, "children": [
            {"tag": "span", "id": "", "cls": "id-anchor"}, {"tag": name, "id": hid, "cls": cls}]}));
    }
    let spec = serde_json::json!({"links": links, "blocks": blocks}).to_string();
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/toc.js"
    );
    let mut child = std::process::Command::new("node")
        .args(["-e", SPY_HARNESS, "--", js])
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
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let observed = v["observed"].as_array().unwrap();
    assert_eq!(
        observed.len(),
        5,
        "one heading per toc link, never the section div: {v}"
    );
    assert!(
        observed
            .iter()
            .all(|e| e.as_str().unwrap().starts_with('H')),
        "{v}"
    );
    assert_eq!(v["afterSecond"], serde_json::json!([1]), "{v}");
    assert_eq!(v["afterThird"], serde_json::json!([2]), "{v}");
}
