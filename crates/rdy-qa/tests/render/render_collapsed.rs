//! rfr1 (D-3VVNC): a section whose `collapsed` field is true is a fold. On the site it is a
//! `<details>` whose summary is the section's heading and toolbar, and whose body is every block
//! under the section; the PDF and the single HTML file show it as a plain open section; the
//! Markdown tree writes `<details>`; Copy writes the plain form. Search, llms and link checks
//! read all of its text. A fragment opens the folds around its target.
use rdy_engine::{Rdy, Scope};
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::{html, tiny};
use rdy_render::print::fonts::Fonts;
use rdy_render::print::layout::LItem;
use rdy_render::print::md::{Layout, write_markdown};
use rdy_render::print::{self, Format, PBlock, Print};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// --- the fixture --------------------------------------------------------------------------------

/// Doc `Guide page`: the lede section `Intro`, then the collapsed `Alpha fold` (a prose, the
/// collapsed `Beta` with a title of code, an ampersand and an angle bracket holding a prose and
/// the open section `Gamma open`, and a claim after `Beta`), then the open section `Sibling`,
/// whose prose links into the folds.
struct Fixture {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
}

const GUIDE: &str = "guide.html";

fn guide_ops() -> Value {
    json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$intro", "parent": "$g", "fields": {"title": "Intro"}},
        {"op": "create", "type": "prose", "parent": "$intro", "body": "Lede words of the guide."},
        {"op": "create", "type": "section", "id": "$a", "parent": "$g", "fields": {"title": "Alpha fold", "collapsed": true}},
        {"op": "create", "type": "prose", "id": "$pa", "parent": "$a", "body": "Alpha body words."},
        {"op": "create", "type": "section", "id": "$b", "parent": "$a", "fields": {"title": "Beta `code` & <b>", "collapsed": true}},
        {"op": "create", "type": "prose", "id": "$pb", "parent": "$b", "body": "Beta body words."},
        {"op": "create", "type": "section", "id": "$c", "parent": "$b", "fields": {"title": "Gamma open"}},
        {"op": "create", "type": "prose", "id": "$pc", "parent": "$c", "body": "Gamma body words."},
        {"op": "create", "type": "claim", "id": "$ca", "parent": "$a", "body": "Alpha claim after beta."},
        {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Sibling"}},
        {"op": "create", "type": "prose", "id": "$ps", "parent": "$s", "body": "Sibling words, see {{$pc}}."}
    ])
}

fn fixture() -> Fixture {
    let (dir, mut r) = tiny();
    let w = write(&mut r, "work", guide_ops()).unwrap();
    land(&mut r, "work").unwrap();
    Fixture {
        _dir: dir,
        r,
        ids: w.created.into_iter().collect(),
    }
}

impl Fixture {
    fn site(&self, scope: &Scope) -> tempfile::TempDir {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&self.r.read(), scope, out.path()).unwrap();
        out
    }

    /// Lands an edit that clears `collapsed` on the two folds: the same blocks, plain sections.
    fn unfold(&mut self) {
        let ops = ["$a", "$b"]
            .map(|k| json!({"op": "update", "id": self.ids[k], "fields": {"collapsed": false}}));
        write(&mut self.r, "unfold", json!(ops)).unwrap();
        land(&mut self.r, "unfold").unwrap();
    }
}

// --- reading a page -----------------------------------------------------------------------------

/// One `<details class="section-fold">` of a page.
#[derive(Debug)]
struct Fold {
    open: bool,
    /// Where it starts and ends in the page.
    start: usize,
    end: usize,
    /// Its summary, its markup.
    summary: String,
    /// What follows the summary.
    body: String,
}

/// The folds of a page in document order, each with its extent: the matching `</details>` found
/// by counting every `details` element (a diagram's source is one too).
fn folds_of(page: &str) -> Vec<Fold> {
    let tag = Regex::new(r"<(/?)details\b([^>]*)>").unwrap();
    let marks: Vec<(usize, usize, bool, bool)> = tag
        .captures_iter(page)
        .map(|c| {
            let m = c.get(0).unwrap();
            (
                m.start(),
                m.end(),
                &c[1] == "/",
                c[2].contains("section-fold"),
            )
        })
        .collect();
    let mut out = vec![];
    for (i, (start, end, closing, fold)) in marks.iter().enumerate() {
        if *closing || !*fold {
            continue;
        }
        let mut depth = 0;
        let close = marks[i..]
            .iter()
            .find(|(_, _, closing, _)| {
                depth += if *closing { -1 } else { 1 };
                depth == 0
            })
            .expect("a fold closes");
        let inner = &page[*end..close.0];
        let summary_end = inner.find("</summary>").expect("a summary") + "</summary>".len();
        out.push(Fold {
            open: page[*start..*end].contains(" open"),
            start: *start,
            end: close.1,
            summary: inner[..summary_end].to_string(),
            body: inner[summary_end..].to_string(),
        });
    }
    out
}

/// The heading of a fold's summary: level and inner markup.
fn heading_of(fold: &Fold) -> (u8, String) {
    let re = Regex::new(r#"<h([1-6]) class="section-title">(.*?)</h[1-6]>"#).unwrap();
    let c = re
        .captures(&fold.summary)
        .expect("a heading in the summary");
    (c[1].parse().unwrap(), c[2].to_string())
}

/// A page without what the folds add and without the copy payload's ranges: its blocks, in order.
fn without_wrapping(page: &str) -> String {
    let ranges = Regex::new(r#" data-copy-start="\d+" data-copy-end="\d+""#).unwrap();
    let template = Regex::new(r"(?s)<template data-copy-page hidden>.*?</template>").unwrap();
    let page = ranges.replace_all(page, "");
    let page = template.replace_all(&page, "");
    page.replace(
        "<details class=\"section-fold\"><summary class=\"section-fold-head\">",
        "",
    )
    .replace(
        "<details class=\"section-fold\" open><summary class=\"section-fold-head\">",
        "",
    )
    .replace("</summary>\n", "\n")
    .replace("</details>\n", "")
}

fn attr_values(page: &str, attr: &str) -> BTreeSet<String> {
    let re = Regex::new(&format!(r#"\b{attr}="([^"]*)""#)).unwrap();
    re.captures_iter(page).map(|c| c[1].to_string()).collect()
}

fn titles(folds: &[Fold]) -> Vec<(String, bool)> {
    folds
        .iter()
        .map(|f| {
            let (_, h) = heading_of(f);
            let plain = Regex::new(r"<[^>]*>")
                .unwrap()
                .replace_all(&h, "")
                .to_string();
            (plain, f.open)
        })
        .collect()
}

// --- the tests ----------------------------------------------------------------------------------

#[test]
fn rfr1_nested_folds_keep_headings_anchors_and_tools() {
    let mut f = fixture();
    let page = html(f.site(&Scope::Canon).path(), GUIDE);
    let folds = folds_of(&page);
    assert_eq!(folds.len(), 2, "one fold per collapsed section");
    assert!(folds.iter().all(|f| !f.open), "a fold starts closed");
    let (alpha, beta) = (&folds[0], &folds[1]);
    assert!(
        alpha.start < beta.start && beta.end < alpha.end,
        "Beta folds inside Alpha"
    );

    // the summary keeps the heading level, the heading id and one toolbar
    for (fold, level, id, title) in [
        (alpha, 2, "alpha-fold", "Alpha fold"),
        (
            beta,
            3,
            "beta-code-b",
            "Beta <code>code</code> &amp; &lt;b&gt;",
        ),
    ] {
        assert_eq!(heading_of(fold), (level, title.to_string()));
        assert!(fold.summary.contains(&format!(" id=\"{id}\"")), "{id}");
        assert!(fold.summary.contains("data-rdy-block=\"s-"), "{id}");
        assert_eq!(fold.summary.matches("class=\"tools\"").count(), 1, "{id}");
        for tool in ["tool copy-link", "tool copy-ref", "tool copy-text"] {
            assert_eq!(fold.summary.matches(tool).count(), 1, "{id}: {tool}");
        }
        assert!(
            fold.summary.starts_with(
                "<summary class=\"section-fold-head\"><div class=\"block block-section\""
            ),
            "{}",
            fold.summary
        );
    }

    // each fold holds the full extent of its section, and no sibling's block
    for (fold, within, without) in [
        (
            alpha,
            vec![
                "Alpha body words.",
                "Beta body words.",
                "Gamma body words.",
                "Alpha claim after beta.",
                "Gamma open",
            ],
            vec!["Sibling words", "Lede words", ">Intro<", "Sibling</h2>"],
        ),
        (
            beta,
            vec!["Beta body words.", "Gamma body words.", "Gamma open"],
            vec![
                "Alpha body words.",
                "Alpha claim after beta.",
                "Sibling words",
            ],
        ),
    ] {
        for w in within {
            assert!(fold.body.contains(w), "{w}");
        }
        for w in without {
            assert!(!fold.body.contains(w), "{w}");
        }
    }
    assert!(
        page.find(" id=\"sibling\"").unwrap() > alpha.end,
        "the next section is outside the fold"
    );
    assert!(
        page.find(" id=\"intro\"").unwrap() < alpha.start,
        "the section before it is outside the fold"
    );

    // the same page without the folds: the same ids, the same blocks, the same one toolbar each
    let wrapped = page.clone();
    f.unfold();
    let plain = html(f.site(&Scope::Canon).path(), GUIDE);
    assert!(folds_of(&plain).is_empty());
    assert_eq!(attr_values(&wrapped, "id"), attr_values(&plain, "id"));
    assert_eq!(
        attr_values(&wrapped, "data-rdy-block"),
        attr_values(&plain, "data-rdy-block")
    );
    assert_eq!(
        wrapped.matches("class=\"tools\"").count(),
        plain.matches("class=\"tools\"").count()
    );
    // what the folds add is the wrapper alone: take it away and every byte of a block is the same
    assert_eq!(without_wrapping(&wrapped), without_wrapping(&plain));
}

/// A just-enough DOM for `theme.js`: the served markup parsed into elements (with `closest`,
/// `matches`, `getElementById` and `querySelectorAll` over tag, class and attribute selectors), a
/// `location` whose address and hash a scenario sets, and the listeners the script adds. A
/// scenario is the body of a function over `h`.
const FOLD_HARNESS: &str = r##"
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(0, 'utf8'));
function decode(s) {
  return s.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);/g, (m, e) => {
    if (e[0] === '#') return String.fromCodePoint(e[1] === 'x' || e[1] === 'X' ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10));
    return { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" }[e] || m;
  });
}
let scrolled = [];
function El(tag, attrs) { this.tagName = tag.toUpperCase(); this.attrs = attrs || {}; this.nodes = []; this.parentNode = null; }
function matchOne(e, sel) {
  const m = /^([a-zA-Z][a-zA-Z0-9]*)?((?:\.[\w-]+|\[[^\]]+\])*)$/.exec(sel.trim());
  if (!m) throw new Error('the harness does not know the selector ' + sel);
  if (m[1] && e.tagName !== m[1].toUpperCase()) return false;
  return (m[2].match(/\.[\w-]+|\[[^\]]+\]/g) || []).every((p) => {
    if (p[0] === '.') return (e.attrs.class || '').split(/\s+/).includes(p.slice(1));
    const a = /^\[([\w-]+)(?:="([^"]*)")?\]$/.exec(p);
    if (!(a[1] in e.attrs)) return false;
    return a[2] === undefined || e.attrs[a[1]] === a[2];
  });
}
El.prototype = {
  get parentElement() { return this.parentNode instanceof El ? this.parentNode : null; },
  get children() { return this.nodes.filter((n) => n instanceof El); },
  get href() { const h = this.attrs.href; return h === undefined ? '' : new URL(h, spec.page).href; },
  get classList() {
    const self = this;
    return { contains: (c) => (self.attrs.class || '').split(/\s+/).includes(c) };
  },
  getAttribute(n) { return n in this.attrs ? this.attrs[n] : null; },
  hasAttribute(n) { return n in this.attrs; },
  setAttribute(n, v) { this.attrs[n] = String(v); },
  removeAttribute(n) { delete this.attrs[n]; },
  matches(sel) { return sel.split(',').some((s) => matchOne(this, s)); },
  closest(sel) { for (let e = this; e instanceof El; e = e.parentNode) if (e.matches(sel)) return e; return null; },
  querySelectorAll(sel) {
    const out = [];
    const walk = (e) => { for (const c of e.children) { if (c.matches(sel)) out.push(c); walk(c); } };
    walk(this); return out;
  },
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; },
  addEventListener() {},
  scrollIntoView() { scrolled.push(this.attrs.id || this.tagName); },
};
function parse(html, root) {
  let cur = root;
  const re = /<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:\s+[\w-]+(?:="[^"]*")?)*)\s*>|([^<]+)/g;
  const VOID = ['br', 'img', 'input', 'hr', 'meta', 'link'];
  let m;
  while ((m = re.exec(html))) {
    if (m[4] !== undefined) continue;
    if (m[1]) { if (cur.parentNode) cur = cur.parentNode; continue; }
    const attrs = {};
    for (const a of m[3].matchAll(/([\w-]+)(?:="([^"]*)")?/g)) attrs[a[1]] = a[2] === undefined ? '' : decode(a[2]);
    const e = new El(m[2], attrs); e.parentNode = cur; cur.nodes.push(e);
    if (!VOID.includes(m[2].toLowerCase())) cur = e;
  }
}
const html = new El('html');
const body = new El('body'); body.parentNode = html; html.nodes.push(body);
parse(spec.html.slice(spec.html.indexOf('<body')), body);
const docL = {}, winL = {};
const find = (id) => { const w = (e) => { for (const c of e.children) { if (c.attrs.id === id) return c; const r = w(c); if (r) return r; } return null; }; return w(body); };
const document = {
  documentElement: html, body, activeElement: body,
  addEventListener: (t, f) => { (docL[t] = docL[t] || []).push(f); },
  getElementById: find,
  querySelector: (s) => body.querySelector(s),
  querySelectorAll: (s) => body.querySelectorAll(s),
};
const location = { href: spec.page + (spec.hash || ''), hash: spec.hash || '' };
const window = {
  document, location,
  addEventListener: (t, f) => { (winL[t] = winL[t] || []).push(f); },
};
new Function('window', 'document', 'location', spec.script)(window, document, location);
const openFolds = () => body.querySelectorAll('details').filter((d) => d.hasAttribute('open') && (d.attrs.class || '').includes('section-fold'))
  .map((d) => d.querySelector('.block').attrs.id);
const h = {
  out: {}, spec,
  open: openFolds,
  closeAll() { for (const d of body.querySelectorAll('details')) if ((d.attrs.class || '').includes('section-fold')) d.removeAttribute('open'); },
  scrolled: () => scrolled.splice(0),
  // the address moves to `hash` and the page hears of it, as on a link, a back or a forward
  go(hash) { location.hash = hash; location.href = spec.page + hash; (winL.hashchange || []).forEach((f) => f({ type: 'hashchange' })); },
  click(el) { (docL.click || []).forEach((f) => f({ type: 'click', target: el })); },
  link(sel) { const e = body.querySelector(sel); if (!e) throw new Error('no ' + sel); return e; },
  anchor(href) { return new El('a', { href }); },
};
new Function('h', spec.scenario)(h);
console.log(JSON.stringify(h.out));
"##;

fn run_fold_script(page: &str, hash: &str, scenario: &str) -> Value {
    let spec = json!({
        "html": page,
        "page": "https://example.invalid/guide.html",
        "hash": hash,
        "script": std::fs::read_to_string(rdy_qa::site::theme_js("theme.js")).unwrap(),
        "scenario": scenario,
    });
    let mut child = std::process::Command::new("node")
        .args(["-e", FOLD_HARNESS])
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
        .write_all(spec.to_string().as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

#[test]
fn rfr1_fragments_open_every_ancestor() {
    if !crate::render_copy_text::node_or_skip("rfr1_fragments_open_every_ancestor") {
        return;
    }
    let f = fixture();
    let page = html(f.site(&Scope::Canon).path(), GUIDE);
    let alpha = "alpha-fold";
    let beta = "beta-code-b";

    // the address, at load: the folds around a block inside both open, and the page scrolls to it
    let v = run_fold_script(
        &page,
        "#gamma-body-words",
        "h.out.open = h.open(); h.out.scrolled = h.scrolled();",
    );
    assert_eq!(v["open"], json!([alpha, beta]));
    assert_eq!(v["scrolled"], json!(["gamma-body-words"]));

    // no fragment, a fragment outside every fold, and an id the page does not have: nothing opens
    for hash in ["", "#sibling", "#intro", "#nope"] {
        let v = run_fold_script(
            &page,
            hash,
            "h.out.open = h.open(); h.out.scrolled = h.scrolled();",
        );
        assert_eq!(v["open"], json!([]), "{hash:?}");
        assert_eq!(v["scrolled"], json!([]), "{hash:?}");
    }

    // a fold is its own target: its heading is in the summary, and it opens with its outer folds
    let v = run_fold_script(&page, "#beta-code-b", "h.out.open = h.open();");
    assert_eq!(v["open"], json!([alpha, beta]));
    let v = run_fold_script(&page, "#alpha-fold", "h.out.open = h.open();");
    assert_eq!(v["open"], json!([alpha]));
    // an address that percent-encodes a letter
    let v = run_fold_script(&page, "#%67amma-open", "h.out.open = h.open();");
    assert_eq!(v["open"], json!([alpha, beta]));

    // an ordinary link in the text, a link of the page index, a search result's link (a path
    // from the page, the form the search script writes), and a link to a place outside a fold
    let v = run_fold_script(
        &page,
        "",
        r##"
        const step = (el) => { h.closeAll(); h.click(el); return [h.open(), h.scrolled()]; };
        h.out.text = step(h.link('a[href="#gamma-body-words"]'));
        h.out.index = step(h.link('a.toc-link[href="#gamma-open"]'));
        h.out.index_alpha = step(h.link('a.toc-link[href="#alpha-fold"]'));
        h.out.search = step(h.anchor('guide.html#beta-body-words'));
        h.out.search_far = step(h.anchor('other/page.html#beta-body-words'));
        h.out.outside = step(h.link('a.toc-link[href="#sibling"]'));
        // the address already ends at the target: no hashchange follows the click
        h.go('#gamma-body-words'); h.closeAll(); h.scrolled();
        h.click(h.link('a[href="#gamma-body-words"]'));
        h.out.same_address = [h.open(), h.scrolled()];
        "##,
    );
    assert_eq!(v["text"], json!([[alpha, beta], ["gamma-body-words"]]));
    assert_eq!(v["index"], json!([[alpha, beta], ["gamma-open"]]));
    assert_eq!(v["index_alpha"], json!([[alpha], ["alpha-fold"]]));
    assert_eq!(v["search"], json!([[alpha, beta], ["beta-body-words"]]));
    assert_eq!(
        v["search_far"],
        json!([[], []]),
        "another page opens by its own load"
    );
    assert_eq!(v["outside"], json!([[], []]));
    assert_eq!(
        v["same_address"],
        json!([[alpha, beta], ["gamma-body-words"]])
    );

    // the history: a new fragment, and back and forward through the ones before it
    let v = run_fold_script(
        &page,
        "",
        r##"
        h.go('#alpha-body-words'); h.out.first = h.open();
        h.go('#gamma-body-words'); h.out.second = h.open();
        h.closeAll();
        h.go('#alpha-body-words'); h.out.back = h.open();
        h.closeAll();
        h.go('#gamma-body-words'); h.out.forward = h.open();
        "##,
    );
    assert_eq!(v["first"], json!(["alpha-fold"]));
    assert_eq!(v["second"], json!(["alpha-fold", "beta-code-b"]));
    assert_eq!(
        v["back"],
        json!(["alpha-fold"]),
        "back reopens what it needs only"
    );
    assert_eq!(v["forward"], json!(["alpha-fold", "beta-code-b"]));
}

/// Doc `Review guide`: a lede section, then one collapsed section per case. The draft view
/// edits a prose of `Edited`, adds one to `New`, removes one from the middle of `Middle`, removes
/// the last prose of `Last`, edits a prose of `Inner changed` (inside `Outer`), and leaves
/// `Inner same` and `Unchanged` alone.
fn review_fixture() -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = tiny();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "review-guide", "title": "Review guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$i", "parent": "$g", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "parent": "$i", "body": "Lede words."},
            {"op": "create", "type": "section", "id": "$e", "parent": "$g", "fields": {"title": "Edited", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$ee", "parent": "$e", "body": "Old wording."},
            {"op": "create", "type": "section", "id": "$n", "parent": "$g", "fields": {"title": "New", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$n", "body": "Kept in new."},
            {"op": "create", "type": "section", "id": "$m", "parent": "$g", "fields": {"title": "Middle", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$mg", "parent": "$m", "body": "Gone soon."},
            {"op": "create", "type": "prose", "parent": "$m", "body": "Stays in middle."},
            {"op": "create", "type": "section", "id": "$l", "parent": "$g", "fields": {"title": "Last", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$l", "body": "Kept in last."},
            {"op": "create", "type": "prose", "id": "$lg", "parent": "$l", "body": "Last one gone."},
            {"op": "create", "type": "section", "id": "$o", "parent": "$g", "fields": {"title": "Outer", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Outer kept."},
            {"op": "create", "type": "section", "id": "$ic", "parent": "$o", "fields": {"title": "Inner changed", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$ie", "parent": "$ic", "body": "Deep old."},
            {"op": "create", "type": "section", "id": "$is", "parent": "$o", "fields": {"title": "Inner same", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$is", "body": "Deep same."},
            {"op": "create", "type": "section", "id": "$u", "parent": "$g", "fields": {"title": "Unchanged", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$u", "body": "Same."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "draft",
        json!([
            {"op": "update", "id": id("$ee"), "body": "New wording."},
            {"op": "create", "type": "prose", "parent": id("$n"), "body": "Brand new words."},
            {"op": "delete", "id": id("$mg")},
            {"op": "delete", "id": id("$lg")},
            {"op": "update", "id": id("$ie"), "body": "Deep new."}
        ]),
    )
    .unwrap();
    (dir, r)
}

#[test]
fn rfr1_review_opens_only_folds_with_changes() {
    let (_dir, r) = review_fixture();
    let render = |scope: &Scope| {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), scope, out.path()).unwrap();
        html(out.path(), "review-guide.html")
    };

    // the review page: a fold that holds an edited, new or removed block starts open, a nested
    // fold opens with the change it holds, and every other fold stays closed
    let page = render(&Scope::View("draft".into()));
    let got = titles(&folds_of(&page));
    let want = [
        ("Edited", true),
        ("New", true),
        ("Middle", true),
        ("Last", true),
        ("Outer", true),
        ("Inner changed", true),
        ("Inner same", false),
        ("Unchanged", false),
    ];
    assert_eq!(
        got,
        want.iter()
            .map(|(t, o)| (t.to_string(), *o))
            .collect::<Vec<_>>(),
        "{page}"
    );
    // without scripting the markup alone says which are open: the `open` attribute is the state
    assert_eq!(
        page.matches("<details class=\"section-fold\" open>")
            .count(),
        6
    );
    assert_eq!(page.matches("<details class=\"section-fold\">").count(), 2);
    // the changes are on the page whatever the folds hold
    for w in ["New wording.", "Brand new words.", "Deep new."] {
        assert!(page.contains(w), "{w}");
    }

    // canon, which the draft does not touch: every fold starts closed
    let canon = titles(&folds_of(&render(&Scope::Canon)));
    assert_eq!(canon.len(), 8);
    assert!(canon.iter().all(|(_, open)| !open), "{canon:?}");
}

/// One fold with planned content: a profile that hides planned content leaves a hidden child and
/// a hidden section out of the fold; the words around them stay.
fn planned_fixture() -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = tiny();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "plan-guide", "title": "Plan guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$i", "parent": "$g", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "parent": "$i", "body": "Lede words."},
            {"op": "create", "type": "section", "id": "$f", "parent": "$g", "fields": {"title": "Fold with plans", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$f", "body": "Shown first words."},
            {"op": "create", "type": "prose", "parent": "$f", "fields": {"planned": true}, "body": "Hidden planned words."},
            {"op": "create", "type": "section", "id": "$h", "parent": "$f", "fields": {"title": "Hidden planned section", "collapsed": true, "planned": true}},
            {"op": "create", "type": "prose", "parent": "$h", "body": "Hidden child words."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "Shown last words."},
            {"op": "create", "type": "section", "id": "$p", "parent": "$g", "fields": {"title": "Planned fold", "collapsed": true, "planned": true}},
            {"op": "create", "type": "prose", "parent": "$p", "body": "Hidden fold words."},
            {"op": "create", "type": "section", "id": "$t", "parent": "$g", "fields": {"title": "Tail"}},
            {"op": "create", "type": "prose", "parent": "$t", "body": "Tail words."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let doc = w.created["$g"].clone();
    write(
        &mut r,
        "profiles",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$hide", "parent": w.created["$admin"],
             "fields": {"key": "hide", "title": "Hide site", "audience": "org", "planned": "hide", "select": [doc]}},
            {"op": "create", "type": "publication_profile", "id": "$mark", "parent": w.created["$admin"],
             "fields": {"key": "mark", "title": "Mark site", "audience": "org", "planned": "mark", "select": [doc]}}
        ]),
    )
    .unwrap();
    land(&mut r, "profiles").unwrap();
    (dir, r)
}

fn doc_blocks(p: &Print, page: &str) -> Vec<PBlock> {
    p.docs
        .iter()
        .find(|d| d.page == page)
        .unwrap_or_else(|| panic!("{page}"))
        .blocks
        .clone()
}

/// Every word of a print doc's runs, folds included.
fn words_of(blocks: &[PBlock], out: &mut String) {
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => {
                runs.iter().for_each(|r| out.push_str(&r.text))
            }
            PBlock::Fold { runs, blocks, .. } => {
                runs.iter().for_each(|r| out.push_str(&r.text));
                out.push(' ');
                words_of(blocks, out);
            }
            _ => {}
        }
        out.push(' ');
    }
}

fn pdf_words(p: &Print) -> String {
    print::pdf::lay_out(p, &Fonts::load())
        .pages
        .iter()
        .flat_map(|page| page.items.iter())
        .filter_map(|it| match it {
            LItem::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn rfr1_closed_text_remains_searchable_and_printable() {
    let mut f = fixture();
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);

    // the site: a closed fold's words are in the page, the search index and every llms file, which
    // carry no fold marker of their own; a link into the fold has its anchor
    let index = std::fs::read_to_string(out.path().join("search-index.json")).unwrap();
    let mut llms = vec![];
    for e in std::fs::read_dir(out.path()).unwrap() {
        let p = e.unwrap().path();
        if p.file_name().unwrap().to_string_lossy().starts_with("llms") {
            llms.push((
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
    assert!(!llms.is_empty(), "the site has llms files");
    for w in [
        "Alpha body words.",
        "Beta body words.",
        "Gamma body words.",
        "Alpha claim after beta.",
    ] {
        assert!(page.contains(w), "page: {w}");
        assert!(index.contains(w), "search index: {w}");
    }
    let full: String = llms.iter().map(|(_, t)| t.as_str()).collect();
    for w in [
        "Beta body words.",
        "Gamma body words.",
        "Alpha claim after beta.",
    ] {
        assert!(full.contains(w), "llms: {w}");
    }
    for (name, text) in &llms {
        for marker in ["<details", "<summary", "collapsed", "section-fold"] {
            assert!(!text.to_lowercase().contains(marker), "{name}: {marker}");
        }
    }
    assert!(page.contains("<a href=\"#gamma-body-words\""), "the link");
    assert!(
        folds_of(&page)[1].body.contains(" id=\"gamma-body-words\""),
        "its target is in the fold"
    );

    // the print model keeps the fold's boundary; the PDF and the single HTML file show plain
    // open sections, the same as a project whose sections were never folds
    let p = print::build(&f.r.read(), &Scope::Canon).unwrap();
    let blocks = doc_blocks(&p, GUIDE);
    let alpha = blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Fold { anchor, blocks, .. } if anchor.as_deref() == Some("alpha-fold") => {
                Some(blocks)
            }
            _ => None,
        })
        .expect("the Alpha fold");
    assert!(
        alpha.iter().any(
            |b| matches!(b, PBlock::Fold { anchor, .. } if anchor.as_deref() == Some("beta-code-b"))
        ),
        "Beta folds inside Alpha"
    );
    let tmp = tempfile::tempdir().unwrap();
    let single = tmp.path().join("one.html");
    print::export(&f.r.read(), &Scope::Canon, &single, Format::Html, None).unwrap();
    let single_folded = std::fs::read_to_string(&single).unwrap();
    assert!(
        !single_folded.contains("<details"),
        "no fold in the single file"
    );
    for w in [
        "Alpha fold",
        "Beta",
        "Gamma open",
        "Beta body words.",
        "Alpha claim after beta.",
    ] {
        assert!(single_folded.contains(w), "single html: {w}");
    }
    let pdf_folded = pdf_words(&p);
    for w in [
        "Alpha fold",
        "Beta body words.",
        "Gamma open",
        "Alpha claim after beta.",
    ] {
        assert!(pdf_folded.contains(w), "pdf: {w}");
    }
    let pdf_file = tmp.path().join("one.pdf");
    print::export(&f.r.read(), &Scope::Canon, &pdf_file, Format::Pdf, None).unwrap();
    assert!(std::fs::read(&pdf_file).unwrap().starts_with(b"%PDF"));
    f.unfold();
    let single_plain = tmp.path().join("plain.html");
    print::export(
        &f.r.read(),
        &Scope::Canon,
        &single_plain,
        Format::Html,
        None,
    )
    .unwrap();
    assert_eq!(
        single_folded,
        std::fs::read_to_string(&single_plain).unwrap(),
        "the single HTML file is the same with or without folds"
    );
    let p2 = print::build(&f.r.read(), &Scope::Canon).unwrap();
    assert_eq!(
        pdf_folded,
        pdf_words(&p2),
        "the PDF shows plain open sections"
    );

    // planned content: a profile that hides it leaves it out of the fold and the fold keeps the
    // rest, in every output; one that marks it keeps the fold whole
    let (_d, r) = planned_fixture();
    let read = r.read();
    let site = rdy_render::build_profile_site(&read, "canon", "hide").unwrap();
    let all: String = site
        .files
        .iter()
        .filter(|(k, _)| k.ends_with(".html") || k.ends_with(".json") || k.starts_with("llms"))
        .map(|(_, v)| String::from_utf8_lossy(v).to_string())
        .collect();
    for w in [
        "Shown first words.",
        "Shown last words.",
        "Fold with plans",
        "Tail words.",
    ] {
        assert!(all.contains(w), "hide site: {w}");
    }
    for w in [
        "Hidden planned words.",
        "Hidden child words.",
        "Hidden planned section",
        "Hidden fold words.",
        "Planned fold",
    ] {
        assert!(!all.contains(w), "hide site: {w}");
    }
    let hp = print::build_profile(&read, "canon", "hide").unwrap();
    let hide_blocks = doc_blocks(&hp, "plan-guide.html");
    let mut words = String::new();
    words_of(&hide_blocks, &mut words);
    for w in [
        "Fold with plans",
        "Shown first words.",
        "Shown last words.",
        "Tail words.",
    ] {
        assert!(words.contains(w), "hide print: {w}");
    }
    for w in [
        "Hidden planned words.",
        "Hidden child words.",
        "Hidden planned section",
        "Hidden fold words.",
    ] {
        assert!(!words.contains(w), "hide print: {w}");
    }
    let hide_pdf = pdf_words(&hp);
    assert!(hide_pdf.contains("Shown last words."));
    assert!(!hide_pdf.contains("Hidden"), "{hide_pdf}");
    // the fold ends where its last shown block ends: the tail section is outside it
    let inside = hide_blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Fold { runs, blocks, .. } if runs[0].text == "Fold with plans" => {
                let mut s = String::new();
                words_of(blocks, &mut s);
                Some(s)
            }
            _ => None,
        })
        .expect("the fold");
    assert!(inside.contains("Shown last words."));
    assert!(!inside.contains("Tail words."), "{inside}");
    let mp = print::build_profile(&read, "canon", "mark").unwrap();
    let mut marked = String::new();
    words_of(&doc_blocks(&mp, "plan-guide.html"), &mut marked);
    for w in [
        "Hidden planned words.",
        "Hidden child words.",
        "Hidden fold words.",
    ] {
        assert!(marked.contains(w), "mark print: {w}");
    }
}

fn md_tree(p: &Print, layout: Layout) -> (BTreeMap<String, String>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(p, &out, layout).unwrap();
    let mut files = BTreeMap::new();
    let mut stack = vec![out.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(&out)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(rel, t);
            }
        }
    }
    (files, dir)
}

/// The doc body after its front matter and its title line.
fn body_of(file: &str) -> &str {
    file.split_once("\n# Guide page\n\n").expect("the title").1
}

#[test]
fn rfr1_markdown_and_copy_preserve_fold_boundaries() {
    let f = fixture();
    let p = print::build(&f.r.read(), &Scope::Canon).unwrap();
    let want = |details: &str| {
        format!(
            "<a id=\"lede-words-of-the-guide\"></a>\n\
Lede words of the guide.\n\n\
<a id=\"intro\"></a>\n\
## Intro\n\n\
<a id=\"alpha-fold\"></a>\n\n\
{details}\n\
<summary><b>Alpha fold</b></summary>\n\n\
<a id=\"alpha-body-words\"></a>\n\
Alpha body words.\n\n\
<a id=\"beta-code-b\"></a>\n\n\
{details}\n\
<summary><b>Beta <code>code</code> &amp; &lt;b&gt;</b></summary>\n\n\
<a id=\"beta-body-words\"></a>\n\
Beta body words.\n\n\
<a id=\"gamma-open\"></a>\n\
#### Gamma open\n\n\
<a id=\"gamma-body-words\"></a>\n\
Gamma body words.\n\n\
</details>\n\n\
<a id=\"alpha-claim-after-beta\"></a>\n\
**Claim** Alpha claim after beta.\n\n\
</details>\n\n\
<a id=\"sibling\"></a>\n\
## Sibling\n\n\
<a id=\"sibling-words-see-gamma-body-words\"></a>\n\
Sibling words, see [Gamma body words](guide.md#gamma-body-words).\n"
        )
    };
    for (layout, dir, details, extension) in [
        (Layout::Plain, "", "<details>", false),
        (Layout::Mkdocs, "docs/", "<details markdown=\"1\">", true),
        (Layout::Techdocs, "docs/", "<details markdown=\"1\">", true),
    ] {
        let (files, _keep) = md_tree(&p, layout);
        let doc = &files[&format!("{dir}guide.md")];
        assert_eq!(body_of(doc), want(details), "{layout:?}");
        // every opened fold is closed, in order
        assert_eq!(doc.matches("<details").count(), 2);
        assert_eq!(doc.matches("</details>").count(), 2);
        // the title is not repeated as a Markdown heading, and the subsection keeps its level
        assert!(!doc.contains("## Alpha fold"));
        assert!(!doc.contains("### Beta"));
        assert!(doc.contains("\n#### Gamma open\n"));
        // the extension a MkDocs tree needs
        match files.get("mkdocs.yml") {
            Some(yml) => assert_eq!(
                yml.contains("markdown_extensions:\n  - md_in_html\n"),
                extension,
                "{yml}"
            ),
            None => assert!(!extension),
        }
    }

    // a tree with no fold writes no extension and no details
    let mut plain = fixture();
    plain.unfold();
    let p2 = print::build(&plain.r.read(), &Scope::Canon).unwrap();
    for layout in [Layout::Mkdocs, Layout::Techdocs] {
        let (files, _keep) = md_tree(&p2, layout);
        assert!(!files["mkdocs.yml"].contains("md_in_html"), "{layout:?}");
        assert!(!files["docs/guide.md"].contains("<details"), "{layout:?}");
    }

    // Copy: the plain form, with absolute links, no anchor line and no front matter
    if !crate::render_copy_text::node_or_skip("rfr1_markdown_and_copy_preserve_fold_boundaries") {
        return;
    }
    let out = f.site(&Scope::Canon);
    let page = html(out.path(), GUIDE);
    let v = crate::render_copy_text::run(crate::render_copy_text::page_spec(
        &page,
        "http://h/guide.html",
        crate::render_copy_text::COPY_ALL,
    ));
    let copies = crate::render_copy_text::copies(&page, &v);
    let by_start = |text: &str| {
        copies
            .iter()
            .find(|c| c.text.starts_with(text))
            .unwrap_or_else(|| panic!("a copy that starts {text:?}"))
    };
    let beta = "<details>\n<summary><b>Beta <code>code</code> &amp; &lt;b&gt;</b></summary>\n\nBeta body words.\n\n#### Gamma open\n\nGamma body words.\n\n</details>\n";
    let alpha = format!(
        "<details>\n<summary><b>Alpha fold</b></summary>\n\nAlpha body words.\n\n{beta}\n**Claim** Alpha claim after beta.\n\n</details>\n"
    );
    assert_eq!(by_start("<details>\n<summary><b>Alpha fold").text, alpha);
    assert_eq!(by_start("<details>\n<summary><b>Beta").text, beta);
    // a block inside a fold, and the section inside both: no stray opening or closing line
    assert_eq!(by_start("Beta body words.").text, "Beta body words.\n");
    assert_eq!(
        by_start("#### Gamma open").text,
        "#### Gamma open\n\nGamma body words.\n"
    );
    assert_eq!(
        by_start("**Claim**").text,
        "**Claim** Alpha claim after beta.\n"
    );
    // the page: every fold opened is closed
    let page_copy = by_start("# Guide page");
    assert!(page_copy.text.contains(&alpha), "{}", page_copy.text);
    assert_eq!(page_copy.text.matches("<details>").count(), 2);
    assert_eq!(page_copy.text.matches("</details>").count(), 2);
    assert!(!page_copy.text.contains("<a id"), "no anchor in Copy");
    assert!(!page_copy.text.starts_with("---"), "no front matter");
    assert!(
        page_copy
            .text
            .contains("[Gamma body words](http://h/guide.html#gamma-body-words)"),
        "{}",
        page_copy.text
    );
}

/// A collapsed first section keeps its first prose: the prose is not the page lede, it sits in the
/// fold, section Copy holds it, and an edit to it opens the fold in a review render.
#[test]
fn rfr1_collapsed_first_section_keeps_its_first_prose() {
    let (dir, mut r) = tiny();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "first-fold", "title": "First fold", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$a", "parent": "$g", "fields": {"title": "Closed first", "collapsed": true}},
            {"op": "create", "type": "prose", "id": "$pa", "parent": "$a", "body": "Closed first words."},
            {"op": "create", "type": "section", "id": "$b", "parent": "$g", "fields": {"title": "Second"}},
            {"op": "create", "type": "prose", "parent": "$b", "body": "Second words."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    fn render(r: &Rdy, scope: &Scope) -> String {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), scope, out.path()).unwrap();
        html(out.path(), "first-fold.html")
    }
    let page = render(&r, &Scope::Canon);
    let folds = folds_of(&page);
    assert_eq!(folds.len(), 1, "{page}");
    assert!(!folds[0].open);
    assert!(folds[0].body.contains("Closed first words."), "{page}");
    // nothing of it outside the fold: it is not the lede
    let template = Regex::new(r"(?s)<template data-copy-page hidden>.*?</template>").unwrap();
    let outside = template
        .replace_all(
            &format!("{}{}", &page[..folds[0].start], &page[folds[0].end..]),
            "",
        )
        .into_owned();
    assert!(!outside.contains("Closed first words."), "{outside}");

    // an edit to that prose opens its fold in a review
    write(
        &mut r,
        "draft",
        json!([{"op": "update", "id": w.created["$pa"], "body": "Closed first edited."}]),
    )
    .unwrap();
    let review = folds_of(&render(&r, &Scope::View("draft".into())));
    assert_eq!(review.len(), 1);
    assert!(review[0].open, "{review:?}");
    assert!(review[0].body.contains("Closed first edited."));

    // section Copy holds the prose
    if !crate::render_copy_text::node_or_skip("rfr1_collapsed_first_section_keeps_its_first_prose")
    {
        drop(dir);
        return;
    }
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = html(out.path(), "first-fold.html");
    let v = crate::render_copy_text::run(crate::render_copy_text::page_spec(
        &page,
        "http://h/first-fold.html",
        crate::render_copy_text::COPY_ALL,
    ));
    let copies = crate::render_copy_text::copies(&page, &v);
    let sec = copies
        .iter()
        .find(|c| c.text.starts_with("<details>\n<summary><b>Closed first"))
        .expect("a section copy");
    assert!(sec.text.contains("Closed first words."), "{}", sec.text);
    drop(dir);
}
