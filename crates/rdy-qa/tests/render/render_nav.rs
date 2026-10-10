//! Left nav, breadcrumbs, toc, previous/next (nav1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn nav1_groups_follow_top_level_folders_and_mark_the_current_page() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
        nav.contains("href=\"../../index.html\">Home</a>")
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
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    assert!(
        !one.contains("<aside class=\"index\""),
        "no toc: no empty index column above the page on a phone (FF-F)"
    );
    assert!(three.contains("<aside class=\"index\">"));
}

/// FF-E: the sidebar's way home is "Home", never a doc's title such as "Overview".
#[test]
fn nav1_the_home_entry_does_not_collide_with_a_doc_title() {
    let (_d, _r, _ids, out) = site_canon();
    let h = html(out.path(), "specification/patterns.html");
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert!(nav.contains("href=\"../index.html\">Home</a>"), "{nav}");
    assert_eq!(nav.matches(">Overview</a>").count(), 1, "{nav}");
}

/// FF-D: the glossary's list carries the class its styles select.
#[test]
fn nav1_the_glossary_list_is_styled() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("glossary.html")).unwrap();
    assert!(h.contains("<dl class=\"glossary\">"), "{h}");
    let css = rdy_qa::site::parse_css(
        &std::fs::read_to_string(out.path().join("assets/site.css")).unwrap(),
    );
    assert!(
        css.blocks
            .iter()
            .any(|b| b.selector.trim() == ".glossary dt")
    );
}

#[test]
fn nav1_previous_and_next_follow_tree_order_on_doc_pages_only() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    let (_d, r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        !h.contains("glossary.html") && !h.contains("questions.html") && !h.contains(">Reference<")
    );
}

#[test]
fn nav1_the_sidebar_caps_each_list_at_50_with_a_more_link() {
    let (_d, r) = wide_folder(120); // folder "many" with 120 docs
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("many/doc-60.html")).unwrap();
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert_eq!(
        nav.matches("class=\"nav-item").count(),
        50 + 1,
        "50 siblings plus Home; the folder is the group label"
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
let observed = [], callback = null, options = null;
function IntersectionObserver(cb, opts) { callback = cb; options = opts; this.observe = (e) => observed.push(e); }
const listeners = {};
const location = { hash: '' };
const window = { IntersectionObserver, location, addEventListener: (t, f) => { (listeners[t] = listeners[t] || []).push(f); } };
new Function('window', 'document', 'IntersectionObserver', 'location', fs.readFileSync(process.argv[process.argv.length - 1], 'utf8'))(window, document, IntersectionObserver, location);
const report = { observed: observed.map((e) => e.tagName + ':' + (e.cls || e.className)), marked: () => all.map((l, i) => l.classList.contains('is-active') ? i : -1).filter((i) => i >= 0) };
const out = { observed: observed.map((e) => e.tagName + '.' + e.className) };
if (callback && observed.length) {
  // the reader is in the band of the 2nd heading, then of the 3rd
  const at = (i) => callback([{ target: observed[i], isIntersecting: true }]);
  at(1); out.afterSecond = report.marked();
  callback([{ target: observed[1], isIntersecting: false }]); at(2); out.afterThird = report.marked();
  // a jump to the 4th heading (a toc or in-page link) while the 3rd is still in the band:
  // the link the reader followed is marked, and stays marked while its heading is in the band
  location.hash = '#' + spec.links[3];
  (listeners.hashchange || []).forEach((f) => f());
  out.afterJump = report.marked();
  at(3); out.afterJumpBand = report.marked();
  // reading on: the 4th leaves the band, the 5th enters it
  callback([{ target: observed[2], isIntersecting: false }, { target: observed[3], isIntersecting: false }]); at(4);
  out.afterReadingOn = report.marked();
}
out.rootMargin = options && options.rootMargin;
console.log(JSON.stringify(out));
"##;

#[test]
fn nav1_the_scroll_spy_observes_the_heading_each_toc_link_points_at() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "nav1_the_scroll_spy_observes_the_heading_each_toc_link_points_at",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    // FF-B: a followed link is marked, though an earlier heading is still in the band
    assert_eq!(v["afterJump"], serde_json::json!([3]), "{v}");
    assert_eq!(v["afterJumpBand"], serde_json::json!([3]), "{v}");
    assert_eq!(v["afterReadingOn"], serde_json::json!([4]), "{v}");
    assert!(
        v["rootMargin"].as_str().unwrap().starts_with("0px "),
        "the band starts at the top of the viewport, under the header: {v}"
    );
}

/// Phone rules use the 900px breakpoint: below it, previous/next stack in one column and the
/// next link reads from the left, as the prototype's phone layout does; above it they sit side by
/// side.
#[test]
fn nav1_previous_and_next_stack_below_900px() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let css = rdy_qa::site::parse_css(
        &std::fs::read_to_string(out.path().join("assets/site.css")).unwrap(),
    );
    for w in [320, 375, 600, 900] {
        assert_eq!(
            rdy_qa::site::cascaded(&css, ".pager", "grid-template-columns", w).as_deref(),
            Some("minmax(0, 1fr)"),
            "width {w}"
        );
        assert_eq!(
            rdy_qa::site::cascaded(&css, ".pager-next", "grid-column", w).as_deref(),
            Some("auto"),
            "width {w}"
        );
        assert_eq!(
            rdy_qa::site::cascaded(&css, ".pager-next", "text-align", w).as_deref(),
            Some("left"),
            "width {w}"
        );
    }
    for w in [901, 1024, 1440] {
        assert_eq!(
            rdy_qa::site::cascaded(&css, ".pager", "grid-template-columns", w).as_deref(),
            Some("1fr 1fr"),
            "width {w}"
        );
        assert_eq!(
            rdy_qa::site::cascaded(&css, ".pager-next", "text-align", w).as_deref(),
            Some("right"),
            "width {w}"
        );
    }
}

/// FW-B15: a doc type with `nav: false` is rendered (the Why? trail and permalinks link into it)
/// but left out of the sidebar, its folder's index page, the home page's contents cards, the
/// previous/next pager and the search index.
#[test]
fn rec1_a_doc_type_with_nav_false_is_rendered_but_left_out_of_nav_folder_index_home_pager_and_search()
 {
    let (dir, r) = tiny();
    // the store is locked while a handle is open
    drop(r);
    let pack = "name: p\nversion: '1'\nextends: rdy-standard\ndoc_types:\n  record: { nav: false, skeleton: [] }\n";
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: p\nname: Records\n",
    )
    .unwrap();
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let doc = |id: &str, parent: Option<&str>, name: &str, title: &str, ty: &str| {
        let mut d = serde_json::json!({"op": "create", "type": "doc", "id": id,
            "fields": {"name": name, "title": title, "doc_type": ty}});
        if let Some(p) = parent {
            d["parent"] = p.into();
        }
        d
    };
    let sec = |parent: &str, title: &str| serde_json::json!({"op": "create", "type": "section", "parent": parent, "fields": {"title": title}});
    let prose = |parent: &str, body: &str| serde_json::json!({"op": "create", "type": "prose", "parent": parent, "body": body});
    let res = write(&mut r, "work", serde_json::json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
        doc("$a", Some("$f"), "alpha", "Alpha Page", "overview"),
        {"op": "create", "type": "section", "id": "$as", "parent": "$a", "fields": {"title": "Alpha Part"}},
        doc("$rec", Some("$f"), "rec-in", "Hidden Record", "record"),
        {"op": "create", "type": "section", "id": "$rs", "parent": "$rec", "fields": {"title": "Zebra Facts"}},
        prose("$rs", "Zebracontent lives here."),
        doc("$b", Some("$f"), "beta", "Beta Page", "overview"),
        sec("$b", "Beta Part"),
        doc("$top", None, "rec-top", "Top Record", "record"),
        sec("$top", "Top Facts"),
        doc("$vis", None, "visible", "Visible Top", "overview"),
        sec("$vis", "Visible Part")
    ]))
    .unwrap();
    let rec = res.created["$rec"].clone();
    write(
        &mut r,
        "work",
        serde_json::json!([prose(
            res.created["$as"].as_str(),
            &format!("See {{{{{rec}}}}} for the record.")
        )]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let read = |p: &str| std::fs::read_to_string(out.path().join(p)).unwrap();

    // rendered: both records have their pages, and a link into one leads there
    assert!(read("area/rec-in.html").contains("Zebracontent lives here."));
    assert!(read("rec-top.html").contains("Top Facts"));
    let alpha = read("area/alpha.html");
    assert!(
        alpha.contains(">Hidden Record</a> for the record"),
        "a reference to a record still links\n{alpha}"
    );
    assert!(
        alpha.contains("rec-in.html\" title=\"Hidden Record\""),
        "{alpha}"
    );

    // the sidebar, from every page
    for page in [
        "index.html",
        "area/alpha.html",
        "area/rec-in.html",
        "visible.html",
    ] {
        let h = read(page);
        let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
        assert!(
            !nav.contains("Hidden Record") && !nav.contains("Top Record"),
            "{page}: {nav}"
        );
        assert!(nav.contains(">Visible Top</a>"), "{page}: {nav}");
    }
    // the folder's index page
    let idx = read("area/index.html");
    assert!(
        idx.contains(">Alpha Page</a>") && idx.contains(">Beta Page</a>"),
        "{idx}"
    );
    assert!(
        !idx.contains("Hidden Record") && !idx.contains("rec-in.html"),
        "{idx}"
    );
    // the home page's contents cards
    let home = read("index.html");
    let cards = &home[home.find("<div class=\"cards\">").unwrap()..];
    let cards = &cards[..cards.find("</section>").unwrap()];
    assert!(
        cards.contains("Area") && cards.contains("Visible Top"),
        "{cards}"
    );
    assert!(
        !cards.contains("Top Record") && !cards.contains("rec-top.html"),
        "{cards}"
    );
    // the pager skips the record: Alpha's next is Beta, Beta's previous is Alpha
    let alpha_next = &alpha[alpha.find("class=\"pager-next\"").unwrap()..];
    assert!(
        alpha_next[..alpha_next.find("</a>").unwrap()].contains("Beta Page"),
        "{alpha}"
    );
    let beta = read("area/beta.html");
    let beta_prev = &beta[beta.find("class=\"pager-prev\"").unwrap()..];
    assert!(
        beta_prev[..beta_prev.find("</a>").unwrap()].contains("Alpha Page"),
        "{beta}"
    );
    // a record's own page is out of the reading order, and carries no empty pager landmark
    let recp = read("area/rec-in.html");
    assert!(
        !recp.contains("class=\"pager-prev\"") && !recp.contains("class=\"pager-next\""),
        "{recp}"
    );
    assert!(
        !recp.contains("class=\"pager\"") && !recp.contains("Previous and next"),
        "{recp}"
    );
    // the search index
    // (Alpha's own text says "Hidden Record": a reference reads as its target's title)
    let search = read("search-index.json");
    for gone in ["Top Record", "Zebra", "rec-in.html", "rec-top.html"] {
        assert!(!search.contains(gone), "{gone}: {search}");
    }
    assert!(!search.contains("\"title\":\"Hidden Record\""), "{search}");
    assert!(
        search.contains("Alpha Page") && search.contains("Beta Part"),
        "{search}"
    );
}

/// FW-B15: a folder whose every doc is of a `nav: false` type (recursively: a folder holding only
/// such folders too) is left out of navigation where its docs are: the sidebar, its parent's
/// index page, the home page's contents and the search index. Its own index page still renders.
/// A folder with one listed doc stays.
#[test]
fn rec1_a_folder_whose_docs_are_all_nav_false_is_left_out_of_nav_home_folder_index_and_search_but_still_renders()
 {
    let (dir, r) = tiny();
    // the store is locked while a handle is open
    drop(r);
    let pack = "name: p\nversion: '1'\nextends: rdy-standard\ndoc_types:\n  record: { nav: false, skeleton: [] }\n";
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: p\nname: Records\n",
    )
    .unwrap();
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let folder = |id: &str, parent: Option<&str>, name: &str, title: &str| {
        let mut f = serde_json::json!({"op": "create", "type": "folder", "id": id,
            "fields": {"name": name, "title": title}});
        if let Some(p) = parent {
            f["parent"] = p.into();
        }
        f
    };
    let doc = |id: &str, parent: &str, name: &str, title: &str, ty: &str| {
        serde_json::json!({"op": "create", "type": "doc", "id": id, "parent": parent,
            "fields": {"name": name, "title": title, "doc_type": ty}})
    };
    let sec = |parent: &str, title: &str| serde_json::json!({"op": "create", "type": "section", "parent": parent, "fields": {"title": title}});
    write(
        &mut r,
        "work",
        serde_json::json!([
            // only records, and a folder holding only records
            folder("$dec", None, "decisions", "Decisionfolder"),
            doc("$d1", "$dec", "d1", "Record One", "record"),
            sec("$d1", "One Facts"),
            folder("$old", Some("$dec"), "old", "Oldfolder"),
            doc("$d2", "$old", "d2", "Record Two", "record"),
            sec("$d2", "Two Facts"),
            // one listed doc beside a record, and a subfolder of only records
            folder("$area", None, "area", "Areafolder"),
            doc("$a", "$area", "alpha", "Alpha Page", "overview"),
            sec("$a", "Alpha Part"),
            doc("$d3", "$area", "d3", "Record Three", "record"),
            sec("$d3", "Three Facts"),
            folder("$sub", Some("$area"), "sub", "Subfolder"),
            doc("$d4", "$sub", "d4", "Record Four", "record"),
            sec("$d4", "Four Facts")
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let read = |p: &str| std::fs::read_to_string(out.path().join(p)).unwrap();

    // rendered: the hidden folders' index pages still exist
    for page in [
        "decisions/index.html",
        "decisions/old/index.html",
        "area/sub/index.html",
    ] {
        assert!(out.path().join(page).exists(), "{page}");
    }
    assert!(read("decisions/old/index.html").contains("Oldfolder"));

    // the sidebar, from every page
    for page in [
        "index.html",
        "area/index.html",
        "area/alpha.html",
        "decisions/index.html",
        "area/sub/index.html",
    ] {
        let h = read(page);
        let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
        for gone in ["Decisionfolder", "Oldfolder", "Subfolder"] {
            assert!(!nav.contains(gone), "{page}: {gone}: {nav}");
        }
        assert!(nav.contains("Areafolder"), "{page}: {nav}");
    }
    // the parent folder's index pages
    let idx = read("area/index.html");
    assert!(idx.contains(">Alpha Page</a>"), "{idx}");
    assert!(
        !idx.contains("Subfolder") && !idx.contains("sub/index.html"),
        "{idx}"
    );
    let dec = read("decisions/index.html");
    assert!(
        !dec.contains("Oldfolder") && !dec.contains("old/index.html"),
        "{dec}"
    );
    // the home page's contents cards
    let home = read("index.html");
    let cards = &home[home.find("<div class=\"cards\">").unwrap()..];
    let cards = &cards[..cards.find("</section>").unwrap()];
    assert!(cards.contains("Areafolder"), "{cards}");
    assert!(
        !cards.contains("Decisionfolder") && !cards.contains("decisions/index.html"),
        "{cards}"
    );
    // the search index
    let search = read("search-index.json");
    for gone in [
        "Decisionfolder",
        "Oldfolder",
        "Subfolder",
        "decisions/index.html",
        "decisions/old/index.html",
        "area/sub/index.html",
    ] {
        assert!(!search.contains(gone), "{gone}: {search}");
    }
    assert!(
        search.contains("Areafolder") && search.contains("Alpha Part"),
        "{search}"
    );
}
