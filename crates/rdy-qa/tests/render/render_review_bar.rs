//! sr7: the review bar under the header, the Show changes and Show diff toggles at its end, the
//! theme toggle in the drawer on phones (D-42YMF, D-W40KZ, D-BMX8P; RULING F2, F6).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_qa::site::{Css, cascaded, parse_css, visible_runs};
use std::path::Path;

/// The toggles as the bar writes them: both wait for script.
const TOGGLES: &str = "<div class=\"review-toggles\"><button id=\"show-changes\" type=\"button\" aria-pressed=\"true\" hidden>Show changes</button><button id=\"show-diff\" type=\"button\" aria-pressed=\"false\" hidden>Show diff</button></div>";

/// Every page of a site but the permalink pages under `go/`, which hold no layout.
fn layout_pages(site: &Path) -> Vec<(String, String)> {
    pages(site)
        .into_iter()
        .filter(|p| !p.strip_prefix(site).unwrap().starts_with("go"))
        .map(|p| {
            (
                p.display().to_string(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect()
}

/// The part of `h` from the first `from` to the next `to` after it.
fn between<'a>(h: &'a str, from: &str, to: &str) -> &'a str {
    let start = h.find(from).unwrap_or_else(|| panic!("no {from} in {h}"));
    let len = h[start..].find(to).unwrap_or_else(|| panic!("no {to}"));
    &h[start..start + len + to.len()]
}

/// The review project's proposal for the view `rewrite`, as a read scope.
fn proposal_scope(r: &mut rdy_engine::Rdy) -> Scope {
    let p = r
        .propose(
            &rdy_qa::fixtures::writer(),
            REVIEW_VIEW,
            "Rewrite the guide",
        )
        .unwrap();
    r.read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .unwrap()
}

#[test]
fn sr7_a_review_page_names_the_view_or_proposal_in_the_review_bar() {
    let scope = Scope::View(REVIEW_VIEW.into());
    let (_d, mut r, _ids, out) = review_site(&scope);
    // review_project writes the view as `human:qa`, so the bar names its owner by name (F1)
    let owner = r
        .read()
        .view_head(REVIEW_VIEW)
        .and_then(|h| h.owner.as_ref().map(|o| o.actor.clone()));
    assert_eq!(owner.as_deref(), Some("human:qa"));
    let view_bar = "Reviewing view · rewrite · by qa";
    let pages = layout_pages(out.path());
    assert!(pages.len() >= 4, "{}", pages.len());
    for (p, h) in &pages {
        let bar = between(h, "<div class=\"review-bar\">", "</p>");
        assert_eq!(
            bar,
            format!(
                "<div class=\"review-bar\"><p class=\"review-title\" title=\"{view_bar}\">{view_bar}</p>"
            ),
            "{p}"
        );
        // the bar follows the header and leads the frame
        let head_end = h.find("</header>").unwrap() + "</header>".len();
        let bar_at = h.find("<div class=\"review-bar\">").unwrap();
        assert!(h[head_end..bar_at].trim().is_empty(), "{p}");
        assert!(bar_at < h.find("<div class=\"app\">").unwrap(), "{p}");
        // one run, so the leak scan reads a sentence
        let runs: Vec<String> = visible_runs(h).into_iter().map(|r| r.text).collect();
        assert!(runs.iter().any(|t| t == view_bar), "{p}: {runs:?}");
        assert_eq!(h.matches("class=\"review-bar\"").count(), 1, "{p}");
    }

    // a proposal of the view: its title and its author, read by name (F1)
    let pscope = proposal_scope(&mut r);
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &pscope, out.path()).unwrap();
    let proposal_bar = "Reviewing proposal · Rewrite the guide · by qa";
    for (p, h) in layout_pages(out.path()) {
        assert!(
            h.contains(&format!(
                "<div class=\"review-bar\"><p class=\"review-title\" title=\"{proposal_bar}\">{proposal_bar}</p>"
            )),
            "{p}"
        );
        let runs: Vec<String> = visible_runs(&h).into_iter().map(|r| r.text).collect();
        assert!(runs.iter().any(|t| t == proposal_bar), "{p}: {runs:?}");
    }

    // D-BMX8P: the site never says "draft" for a view or a proposal
    for site in [&scope, &pscope] {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), site, out.path()).unwrap();
        for (p, h) in layout_pages(out.path()) {
            assert!(!h.contains("Reviewing draft"), "{p}");
            let header = between(&h, "<header class=\"head\">", "</header>");
            let bar = between(&h, "<div class=\"review-bar\">", "</div></div>");
            assert!(
                !header.to_lowercase().contains("draft"),
                "{p} header: {header}"
            );
            assert!(!bar.to_lowercase().contains("draft"), "{p} bar: {bar}");
            if p.ends_with("/index.html") {
                let list = between(&h, "<section class=\"chg\">", "</section>");
                assert!(!list.to_lowercase().contains("draft"), "{p} list: {list}");
            }
        }
    }
}

#[test]
fn sr7_canon_and_tag_pages_have_no_review_bar() {
    let (_d, r, _ids) = review_project();
    let landing: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    for scope in [Scope::Canon, Scope::At(landing)] {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), &scope, out.path()).unwrap();
        let pages = layout_pages(out.path());
        assert!(pages.len() >= 4, "{scope:?}");
        for (p, h) in pages {
            for gone in ["review-bar", "review-title", "Reviewing ", "show-diff"] {
                assert!(!h.contains(gone), "{scope:?} {p} holds {gone}");
            }
        }
    }
}

#[test]
fn sr7_the_toggles_sit_in_the_review_bar_and_wait_for_script() {
    let scope = Scope::View(REVIEW_VIEW.into());
    let (_d, mut r, _ids, out) = review_site(&scope);
    let mut sites = vec![(scope, out)];
    let pscope = proposal_scope(&mut r);
    let pout = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &pscope, pout.path()).unwrap();
    sites.push((pscope, pout));
    for (scope, out) in &sites {
        let pages = layout_pages(out.path());
        assert!(pages.len() >= 4, "{scope:?}");
        for (p, h) in pages {
            let bar = between(&h, "<div class=\"review-bar\">", "</div></div>");
            assert!(bar.ends_with(&format!("{TOGGLES}</div>")), "{p}: {bar}");
            assert_eq!(h.matches("id=\"show-changes\"").count(), 1, "{p}");
            assert_eq!(h.matches("id=\"show-diff\"").count(), 1, "{p}");
            // both buttons are hidden until the script shows them
            for id in ["show-changes", "show-diff"] {
                let button = between(&h, &format!("<button id=\"{id}\""), "</button>");
                assert!(button.contains(" hidden>"), "{p}: {button}");
            }
            let tools = between(&h, "<div class=\"head-tools\">", "</header>");
            assert!(!tools.contains("show-changes"), "{p}: {tools}");
            assert!(!tools.contains("show-diff"), "{p}: {tools}");
        }
    }
    let app = std::fs::read_to_string(sites[0].1.path().join("assets/app.js")).unwrap();
    assert!(app.contains("show-diff") && app.contains("rdy-diff"));
}

/// A just-enough DOM for `review.js` with a Show diff button: a root element, the buttons, the
/// `[data-diff]` blocks, and a session storage that can be made to throw.
const DIFF_HARNESS: &str = r##"
const fs = require('fs');
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
function El(hidden) { this.attrs = {}; this.hidden = hidden; this.listeners = {}; }
El.prototype.getAttribute = function (n) { return n in this.attrs ? this.attrs[n] : null; };
El.prototype.setAttribute = function (n, v) { this.attrs[n] = String(v); };
El.prototype.removeAttribute = function (n) { delete this.attrs[n]; };
El.prototype.hasAttribute = function (n) { return n in this.attrs; };
El.prototype.addEventListener = function (t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); };
El.prototype.click = function () {
  const self = this;
  (this.listeners.click || []).forEach((f) => f({
    type: 'click', target: self, currentTarget: self,
    preventDefault: () => { throw new Error('preventDefault was called'); },
  }));
};
function Store(broken) {
  const data = {};
  const fail = () => { throw new Error('storage is blocked'); };
  return {
    getItem: (k) => (broken ? fail() : (k in data ? data[k] : null)),
    setItem: (k, v) => { if (broken) fail(); data[k] = String(v); },
    removeItem: (k) => { if (broken) fail(); delete data[k]; },
  };
}
function load(blocks, storage, rooted) {
  const root = new El(false);
  if (rooted) root.attrs['data-diff'] = 'shown';
  const changes = new El(true); changes.attrs['aria-pressed'] = 'true';
  const diff = new El(true); diff.attrs['aria-pressed'] = 'false';
  const diffs = [];
  for (let i = 0; i < blocks; i++) { const d = new El(true); diffs.push(d); }
  const document = {
    documentElement: root,
    getElementById: (id) => (id === 'show-diff' ? diff : id === 'show-changes' ? changes : null),
    querySelectorAll: (sel) => (sel === '[data-diff]' ? (rooted ? [root].concat(diffs) : diffs) : []),
    querySelector: (sel) => (sel === '[data-diff]' ? diffs[0] || null : null),
    addEventListener: () => {},
  };
  new Function('window', 'document', 'sessionStorage', src)({}, document, storage);
  return { root, diff, diffs };
}
const state = (p, s) => ({
  hidden: p.diff.hidden, pressed: p.diff.getAttribute('aria-pressed'),
  root: p.root.getAttribute('data-diff'), stored: s.getItem('rdy-diff'),
  blocks: p.diffs.map((d) => d.hidden),
});
const out = {};
const s1 = Store(false);
const p1 = load(2, s1); out.initial = state(p1, s1);
p1.diff.click(); out.on = state(p1, s1);
p1.diff.click(); out.off = state(p1, s1);
p1.diff.click();
const p2 = load(2, s1); out.reload = state(p2, s1);
const s3 = Store(false);
const p3 = load(0, s3); out.none = state(p3, s3);
const p4 = load(1, Store(true)); p4.diff.click();
out.broken = { hidden: p4.diff.hidden, pressed: p4.diff.getAttribute('aria-pressed'), root: p4.root.getAttribute('data-diff'), blocks: p4.diffs.map((d) => d.hidden) };
// a root that already carries data-diff is not one of the diffs, so it is never hidden
const s5 = Store(false);
const p5 = load(1, s5, true); p5.diff.click(); p5.diff.click();
out.rooted = { root: p5.root.hidden, blocks: p5.diffs.map((d) => d.hidden) };
console.log(JSON.stringify(out));
"##;

#[test]
fn sr7_show_diff_reveals_the_inline_diffs_and_remembers() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "sr7_show_diff_reveals_the_inline_diffs_and_remembers",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/review.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", DIFF_HARNESS, "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "initial": {"hidden": false, "pressed": "false", "root": null, "stored": null, "blocks": [true, true]},
            "on": {"hidden": false, "pressed": "true", "root": "shown", "stored": "shown", "blocks": [false, false]},
            "off": {"hidden": false, "pressed": "false", "root": null, "stored": null, "blocks": [true, true]},
            "reload": {"hidden": false, "pressed": "true", "root": "shown", "stored": "shown", "blocks": [false, false]},
            "none": {"hidden": true, "pressed": "false", "root": null, "stored": null, "blocks": []},
            "broken": {"hidden": false, "pressed": "true", "root": "shown", "blocks": [false]},
            "rooted": {"root": false, "blocks": [true]}
        })
    );
}

/// A just-enough DOM for `theme.js` with two theme groups (the header's and the drawer's).
const THEME_HARNESS: &str = r##"
const fs = require('fs');
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
function El() { this.attrs = {}; this.listeners = {}; this.children = []; }
El.prototype.getAttribute = function (n) { return n in this.attrs ? this.attrs[n] : null; };
El.prototype.setAttribute = function (n, v) { this.attrs[n] = String(v); };
El.prototype.removeAttribute = function (n) { delete this.attrs[n]; };
El.prototype.addEventListener = function (t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); };
El.prototype.closest = function (sel) { return sel === '[data-theme-choice]' && 'data-theme-choice' in this.attrs ? this : null; };
El.prototype.querySelectorAll = function (sel) { return sel === '[data-theme-choice]' ? this.children : []; };
function group() {
  const g = new El();
  for (const c of ['system', 'light', 'dark']) { const b = new El(); b.attrs['data-theme-choice'] = c; b.attrs['aria-checked'] = 'false'; g.children.push(b); }
  return g;
}
const groups = [group(), group()];
const all = groups.flatMap((g) => g.children);
const root = new El();
const data = {};
const localStorage = {
  getItem: (k) => (k in data ? data[k] : null),
  setItem: (k, v) => { data[k] = String(v); },
  removeItem: (k) => { delete data[k]; },
};
const document = {
  documentElement: root,
  querySelector: (sel) => (sel === '.theme' ? groups[0] : null),
  querySelectorAll: (sel) => (sel === '.theme' ? groups : sel === '[data-theme-choice]' ? all : []),
  getElementById: () => null,
  addEventListener: () => {},
  dispatchEvent: () => {},
};
new Function('window', 'document', 'localStorage', 'CustomEvent', src)({}, document, localStorage, function () {});
const checked = () => groups.map((g) => g.children.map((b) => b.getAttribute('aria-checked')));
const out = { initial: checked() };
// a click in the second group, on its Dark button
const dark = groups[1].children[2];
(groups[1].listeners.click || []).forEach((f) => f({ target: dark }));
out.afterDark = checked();
out.theme = root.getAttribute('data-theme');
out.stored = data['rdy-theme'] || null;
// a click in the first group, on its Auto button
const auto = groups[0].children[0];
(groups[0].listeners.click || []).forEach((f) => f({ target: auto }));
out.afterAuto = checked();
out.themeAfterAuto = root.getAttribute('data-theme');
console.log(JSON.stringify(out));
"##;

#[test]
fn sr7_every_theme_group_works() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "sr7_every_theme_group_works",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/theme.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", THEME_HARNESS, "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let sys = serde_json::json!(["true", "false", "false"]);
    let dark = serde_json::json!(["false", "false", "true"]);
    assert_eq!(
        v,
        serde_json::json!({
            "initial": [sys, sys],
            "afterDark": [dark, dark],
            "theme": "dark",
            "stored": "dark",
            "afterAuto": [sys, sys],
            "themeAfterAuto": null
        })
    );
}

/// The review site's stylesheet.
fn review_css() -> Css {
    let (_d, _r, _ids, out) = review_site(&Scope::View(REVIEW_VIEW.into()));
    parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap())
}

#[test]
fn sr7_at_phone_width_the_review_bar_is_one_line_and_the_toggles_sit_under_it() {
    let css = review_css();
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w);
    for w in [360, 480, 900] {
        // the title is one line and truncates
        assert_eq!(
            at(".review-title", "white-space", w).as_deref(),
            Some("nowrap"),
            "width {w}"
        );
        assert_eq!(
            at(".review-title", "overflow", w).as_deref(),
            Some("hidden"),
            "width {w}"
        );
        assert_eq!(
            at(".review-title", "text-overflow", w).as_deref(),
            Some("ellipsis"),
            "width {w}"
        );
        // the toggles are on the next row, at the start
        assert_eq!(
            at(".review-bar", "flex-direction", w).as_deref(),
            Some("column"),
            "width {w}"
        );
        assert_eq!(
            at(".review-toggles", "justify-content", w).as_deref(),
            Some("flex-start"),
            "width {w}"
        );
        // the header keeps the Menu button; the theme toggle moves into the drawer
        assert_eq!(
            at(".head .theme", "display", w).as_deref(),
            Some("none"),
            "width {w}"
        );
        let drawer = at(".theme-drawer", "display", w);
        assert!(drawer.is_some_and(|d| d != "none"), "width {w}");
    }
    for w in [901, 1280] {
        // above 900 px the two swap
        assert_ne!(
            at(".head .theme", "display", w).as_deref(),
            Some("none"),
            "width {w}"
        );
        assert_eq!(
            at(".theme-drawer", "display", w).as_deref(),
            Some("none"),
            "width {w}"
        );
        // the bar is one row: the title on the left, the toggles on the right, sticky under the header
        assert_ne!(
            at(".review-bar", "flex-direction", w).as_deref(),
            Some("column"),
            "width {w}"
        );
        assert_eq!(
            at(".review-bar", "justify-content", w).as_deref(),
            Some("space-between"),
            "width {w}"
        );
        assert_eq!(
            at(".review-bar", "position", w).as_deref(),
            Some("sticky"),
            "width {w}"
        );
        assert_eq!(
            at(".review-bar", "top", w).as_deref(),
            Some("var(--head-h)"),
            "width {w}"
        );
        assert_ne!(
            at(".review-title", "white-space", w).as_deref(),
            Some("nowrap"),
            "width {w}"
        );
    }
    // the old phone rules for Show changes in the header are gone
    for w in [360, 900] {
        assert_eq!(
            at(".head:has(#show-changes) .head-tools", "display", w),
            None,
            "width {w}"
        );
        assert_eq!(at("#show-changes", "grid-row", w), None, "width {w}");
    }
}

/// RULING F6 / D-42YMF: on a narrow screen the header's first row holds the brand, the version
/// pill with its chips and the Menu button; the search box has the second row. The version
/// menu opens under that first row, spanning the header with its 16 px gutters.
#[test]
fn sr7_the_narrow_header_holds_the_brand_the_pill_and_the_menu_on_row_one() {
    let css = review_css();
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w);
    let norm = |v: Option<String>| v.map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "));
    for w in [360, 480, 900] {
        // one grid row of brand, version and tools; the search box on the row below
        assert_eq!(
            at(".brand", "grid-row", w).as_deref(),
            Some("1"),
            "brand {w}"
        );
        assert_eq!(
            at(".version", "grid-row", w).as_deref(),
            Some("1"),
            "version {w}"
        );
        assert_eq!(
            at(".head-tools", "grid-row", w).as_deref(),
            Some("1"),
            "tools {w}"
        );
        assert_eq!(
            at(".head .search-wrap", "grid-row", w).as_deref(),
            Some("2"),
            "search {w}"
        );
        // three distinct columns on row one, in the order brand, pill, Menu
        let col = |sel: &str| at(sel, "grid-column", w).unwrap_or_default();
        assert_eq!(col(".brand"), "1", "{w}");
        assert_eq!(col(".version"), "2", "{w}");
        assert_eq!(col(".head-tools"), "3", "{w}");
        assert_eq!(
            norm(at(".head .search-wrap", "grid-column", w)).as_deref(),
            Some("1 / -1"),
            "{w}"
        );
        // the brand gives way first; the pill and the Menu button keep their size
        let cols = at(".head", "grid-template-columns", w).unwrap_or_default();
        assert_eq!(
            cols.split_whitespace().count(),
            4,
            "{w}: {cols}: minmax(0, 1fr) auto auto"
        );
        assert!(cols.starts_with("minmax(0, 1fr) auto auto"), "{w}: {cols}");
        // the popover opens under row one, across the header
        assert_eq!(
            norm(at(".vmenu-pop", "grid-row", w)).as_deref(),
            Some("1 / 2"),
            "pop {w}"
        );
        assert_eq!(
            norm(at(".vmenu-pop", "grid-column", w)).as_deref(),
            Some("1 / -1"),
            "pop {w}"
        );
        assert_eq!(at(".vmenu-pop", "left", w).as_deref(), Some("0"), "{w}");
        assert_eq!(at(".vmenu-pop", "right", w).as_deref(), Some("0"), "{w}");
    }
    // above 900 px: the brand, the pill, the search box and the tools share row one
    for w in [901, 1280] {
        assert_eq!(at(".version", "grid-row", w).as_deref(), Some("1"), "{w}");
        assert_eq!(
            at(".version", "grid-column", w).as_deref(),
            Some("2"),
            "{w}"
        );
        assert_eq!(
            at(".head .search-wrap", "grid-row", w).as_deref(),
            Some("1")
        );
    }
}

#[test]
fn sr7_touch_targets_are_40px_at_phone_width() {
    let css = review_css();
    for sel in [
        ".pill",
        ".chip",
        "#show-changes",
        "#show-diff",
        ".theme-drawer button",
        ".vmenu a",
    ] {
        for prop in ["min-height", "min-width"] {
            assert_eq!(
                cascaded(&css, sel, prop, 360).as_deref(),
                Some("40px"),
                "{sel} {prop}"
            );
        }
    }
}

#[test]
fn sr7_show_diff_swaps_an_edited_blocks_text_for_its_diff() {
    let css = review_css();
    let sel = ":root[data-diff=\"shown\"] .has-chg.chg-edited > .block-text";
    for w in [360, 900, 901, 1280] {
        assert_eq!(
            cascaded(&css, sel, "display", w).as_deref(),
            Some("none"),
            "width {w}"
        );
        // shown without script when the markers are: the diff itself is not hidden by a rule
        assert_ne!(
            cascaded(&css, ".chg-diff-inline", "display", w).as_deref(),
            Some("none"),
            "width {w}"
        );
    }
}

/// With Show changes off the inline diff is hidden (it is a `.chg`), so Show diff must not also
/// hide the block's own text: an edited block would show nothing.
#[test]
fn sr7_with_changes_hidden_show_diff_never_empties_a_block() {
    let (_d, _r, _ids, out) = review_site(&Scope::View(REVIEW_VIEW.into()));
    let text = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let css = parse_css(&text);
    let swap = ":root[data-diff=\"shown\"] .has-chg.chg-edited > .block-text";
    let keep = ":root[data-changes=\"hidden\"] .has-chg.chg-edited > .block-text";
    // the two rules have the same weight, so the later one wins
    assert!(text.find(swap).unwrap() < text.find(keep).unwrap());
    for w in [360, 900, 901, 1280] {
        assert_eq!(
            cascaded(&css, keep, "display", w).as_deref(),
            Some("block"),
            "width {w}"
        );
    }
    assert_eq!(
        cascaded(&css, ":root[data-changes=\"hidden\"] .chg", "display", 360).as_deref(),
        Some("none")
    );
}

#[test]
fn sr7_the_version_menu_is_a_panel_under_the_pill() {
    let css = review_css();
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w);
    for w in [360, 900, 901, 1280] {
        assert_eq!(
            at(".vmenu-pop", "position", w).as_deref(),
            Some("absolute"),
            "width {w}"
        );
        assert_eq!(
            at(".vmenu-pop", "max-height", w).as_deref(),
            Some("70vh"),
            "width {w}"
        );
        assert_eq!(
            at(".vmenu-pop", "overflow-y", w).as_deref(),
            Some("auto"),
            "width {w}"
        );
        assert!(
            at(".vmenu a:focus-visible", "outline", w).is_some(),
            "width {w}"
        );
    }
    for w in [901, 1280] {
        assert_eq!(
            at(".vmenu-pop", "max-width", w).as_deref(),
            Some("min(28rem, calc(100vw - 32px))"),
            "width {w}"
        );
    }
    // at 900 px and below it spans the width, 16 px gutters coming from the header's padding
    for w in [360, 900] {
        assert_eq!(
            at(".vmenu-pop", "left", w).as_deref(),
            Some("0"),
            "width {w}"
        );
        assert_eq!(
            at(".vmenu-pop", "right", w).as_deref(),
            Some("0"),
            "width {w}"
        );
        assert_eq!(
            at(".vmenu-pop", "max-width", w).as_deref(),
            Some("none"),
            "width {w}"
        );
    }
}

/// The ids of every element of `h` that carries one, in document order.
fn ids_of(h: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = h;
    while let Some(at) = rest.find(" id=\"") {
        rest = &rest[at + 5..];
        let end = rest.find('"').unwrap();
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// The review controls' ids belong to the chrome on every scope: a section named like a control
/// gets its own anchor, so an id is never twice on a page and a link to the section lands on the
/// section, not on the button (and no button rule styles a document block).
#[test]
fn sr7_a_section_named_like_a_review_control_gets_its_own_anchor() {
    let (_d, mut r, ids) = review_project();
    let intro = ids.intro.to_string();
    write(
        &mut r,
        REVIEW_VIEW,
        serde_json::json!([
            {"op": "create", "type": "section", "id": "$sd", "parent": intro, "fields": {"title": "Show diff"}},
            {"op": "create", "type": "prose", "parent": "$sd", "body": "About the diff toggle."},
            {"op": "create", "type": "section", "id": "$sc", "parent": intro, "fields": {"title": "Show changes"}},
            {"op": "create", "type": "prose", "parent": "$sc", "body": "About the changes toggle."}
        ]),
    )
    .unwrap();
    let view = Scope::View(REVIEW_VIEW.into());
    let pscope = proposal_scope(&mut r);
    // canon after the view lands: no button on a canon page, but the same anchors
    let mut sites = vec![view, pscope];
    let landed = {
        land(&mut r, REVIEW_VIEW).unwrap();
        Scope::Canon
    };
    sites.push(landed);
    for scope in &sites {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), scope, out.path()).unwrap();
        let review = !matches!(scope, Scope::Canon);
        let mut seen = 0;
        for (p, h) in layout_pages(out.path()) {
            let all = ids_of(&h);
            let mut sorted = all.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                all.len(),
                "{scope:?} {p}: a repeated id in {all:?}"
            );
            // a button id is on a review page's bar and nowhere else
            for id in ["show-changes", "show-diff"] {
                let n = all.iter().filter(|i| *i == id).count();
                assert_eq!(n, usize::from(review), "{scope:?} {p}: {id}");
                if review {
                    let tag = between(&h, &format!("<button id=\"{id}\""), ">");
                    assert!(tag.contains("type=\"button\""), "{p}: {tag}");
                }
            }
            for (title, anchor) in [
                ("Show diff", "show-diff-2"),
                ("Show changes", "show-changes-2"),
            ] {
                let heading = format!(">{title}</h2>");
                let Some(at) = h.find(&heading) else { continue };
                seen += 1;
                // the anchor is on the section's own block, which holds the heading
                let block = &h[h[..at].rfind("<div class=\"block ").unwrap()..at];
                assert!(
                    block.starts_with("<div class=\"block block-section")
                        && block.contains(&format!(" id=\"{anchor}\"")),
                    "{scope:?} {p}: the heading's block is {block}"
                );
                // every link to the section resolves to its heading, not to a button
                assert!(h.contains(&format!("href=\"#{anchor}\"")), "{scope:?} {p}");
                for bad in ["#show-diff\"", "#show-changes\""] {
                    assert!(
                        !h.contains(&format!("href=\"{bad}")),
                        "{scope:?} {p}: {bad}"
                    );
                }
            }
        }
        assert!(seen >= 2, "{scope:?}: the sections were not rendered");
    }
}
