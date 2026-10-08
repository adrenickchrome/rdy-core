//! FW-V3: the per-block toolbar holds Link and Copy ref, shows on hover, focus or tap at every
//! width, and is never a permanent row (tbar1).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_qa::site::{cascaded, parse_css};

#[test]
fn tbar1_the_toolbar_shows_on_hover_focus_or_tap_at_every_width_and_never_as_a_row() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let css = parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap());
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w).unwrap_or_default();
    for w in [360, 600, 900, 901, 1280] {
        assert_eq!(at(".tools", "opacity", w), "0", "hidden at rest, width {w}");
        assert_eq!(
            at(".tools", "pointer-events", w),
            "none",
            "takes no taps at rest, width {w}"
        );
        assert_eq!(at(".tools", "height", w), "", "no fixed row, width {w}");
        assert_ne!(at(".tools", "display", w), "none", "width {w}");
        assert_ne!(
            at(".tools", "visibility", w),
            "hidden",
            "Tab reaches the buttons, width {w}"
        );
        for shown in [
            ".block:hover .tools",
            ".block:focus-within .tools",
            ".block.tools-open .tools",
            ".tools:focus-within",
            ".tools:has(.cite)",
        ] {
            assert_eq!(at(shown, "opacity", w), "1", "{shown}, width {w}");
            assert_eq!(at(shown, "pointer-events", w), "auto", "{shown}, width {w}");
        }
        assert!(
            !at(":focus-visible", "outline", w).is_empty(),
            "a visible focus ring, width {w}"
        );
    }
    for w in [360, 600, 900] {
        assert_eq!(
            at(".tools", "position", w),
            "absolute",
            "over the block, width {w}"
        );
        assert_eq!(
            at(".tools", "float", w),
            "none",
            "never squeezes the block, width {w}"
        );
        assert_eq!(at(".block", "position", w), "relative", "width {w}");
    }
}

/// Orchestrator addition (the table that changed width when text was selected): the toolbar takes
/// no layout space at any width, so showing it or the Cite button joining it moves nothing. No
/// layout engine runs here, so the contract is read from the stylesheet: the toolbar is out of
/// flow, is not a flex or grid item of the content row, and no revealing rule changes how it is
/// placed or sized. The visual check is the orchestrator's re-capture.
#[test]
fn tbar1_the_toolbar_takes_no_layout_space_at_any_width() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let css = parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap());
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w).unwrap_or_default();
    for w in [360, 600, 900, 901, 1280, 1920] {
        assert_eq!(
            at(".tools", "position", w),
            "absolute",
            "out of flow, width {w}"
        );
        assert_ne!(at(".tools", "float", w), "right", "not a float, width {w}");
        assert_eq!(
            at(".tools", "top", w),
            "0",
            "anchored to the block's corner, width {w}"
        );
        assert_eq!(
            at(".tools", "right", w),
            "0",
            "anchored to the block's corner, width {w}"
        );
        // the bar sits above the block's top edge, in the gap between blocks, so it covers no
        // glyph of the block it belongs to
        assert_eq!(
            at(".tools", "transform", w),
            "translateY(-100%)",
            "lifted into the gap above the block, width {w}"
        );
        assert_eq!(at(".tools", "margin", w), "", "no margin row, width {w}");
        assert_eq!(
            at(".block", "position", w),
            "relative",
            "the anchor, width {w}"
        );
        for d in ["flex", "inline-flex", "grid", "inline-grid"] {
            assert_ne!(
                at(".block", "display", w),
                d,
                "the toolbar is no flex or grid item, width {w}"
            );
        }
        // what reveals the toolbar, or adds the Cite button to it, never places or sizes it
        for rule in [
            ".block:hover .tools",
            ".block:focus-within .tools",
            ".block.tools-open .tools",
            ".tools:hover",
            ".tools:focus-within",
            ".tools:has(.cite)",
            ".tool.cite",
        ] {
            for prop in [
                "position",
                "float",
                "display",
                "margin",
                "width",
                "height",
                "top",
                "right",
                "padding",
                "border",
                "transform",
            ] {
                assert_eq!(at(rule, prop, w), "", "{rule} sets {prop}, width {w}");
            }
        }
    }
}

/// Final review, fix round 1: the shown toolbar sits above its block, so a permalink that lands
/// a block just below the sticky header must leave room for the bar too, or its top is under the
/// header. At a desktop width the room is the header (its min-height and 1px rule) plus the bar
/// (a `.tool`'s line box, its vertical padding and the bar's 1px borders). On a phone the
/// header's three rows dwarf the bar, and `theme1_anchors_land_below_the_sticky_header` holds.
#[test]
fn tbar1_a_permalink_lands_with_the_shown_toolbar_below_the_sticky_header() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let css = parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap());
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w).unwrap_or_default();
    let rem = |v: &str| -> f64 {
        v.split_whitespace()
            .next()
            .and_then(|n| n.strip_suffix("rem"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{v:?} is not in rem"))
    };
    let px = 1.0 / 16.0;
    for w in [901, 1280, 1920] {
        let header = rem(&at(".head", "min-height", w)) + px;
        let line: f64 = at(".tool", "line-height", w).parse().unwrap();
        let bar = rem(&at(".tool", "font-size", w)) * line
            + 2.0 * rem(&at(".tool", "padding", w))
            + 2.0 * px;
        let lands = rem(&at("[id]", "scroll-margin-top", w));
        assert!(
            lands >= header + bar,
            "width {w}: a permalink lands {lands}rem down, but the header and the shown bar take \
             {:.3}rem",
            header + bar
        );
    }
}

#[test]
fn tbar1_the_toolbar_holds_only_link_and_copy_ref_and_tab_reaches_it() {
    let (_d, r, _ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = html(out.path(), "ledgers.html");
    let bars: Vec<&str> = h
        .split("<span class=\"tools\">")
        .skip(1)
        .map(|s| &s[..s.find("</span>").unwrap()])
        .collect();
    assert!(bars.len() > 3, "{h}");
    for b in &bars {
        let buttons: Vec<&str> = b.split("<button ").skip(1).collect();
        assert!(!buttons.is_empty() && buttons.len() <= 2, "{b}");
        assert!(
            buttons[0].starts_with("type=\"button\" class=\"tool copy-link\""),
            "{b}"
        );
        if let Some(second) = buttons.get(1) {
            assert!(
                second.starts_with("type=\"button\" class=\"tool copy-ref\""),
                "{b}"
            );
        }
        for never in ["tabindex", "hidden", "aria-hidden", "disabled"] {
            assert!(!b.contains(never), "{never}: {b}");
        }
    }
}

/// A just-enough DOM for `copy.js`: elements with classes, attributes, `closest`, `matches` and
/// `querySelectorAll` over simple selectors (tag, `.class`…, `[attr]`…, comma lists), and a
/// `document` that records listeners. It fires `pointerup` events and reports which blocks are
/// open after each.
const TAP_HARNESS: &str = r##"
const fs = require('fs');
function El(tag, cls, attrs, parent) {
  this.nodeType = 1; this.tagName = tag.toUpperCase(); this.attrs = attrs || {};
  this._cls = new Set((cls || '').split(' ').filter(Boolean)); this.children = [];
  this.parentElement = parent || null; this.nextElementSibling = null; this.previousElementSibling = null;
  if (parent) {
    const sib = parent.children[parent.children.length - 1];
    if (sib) { sib.nextElementSibling = this; this.previousElementSibling = sib; }
    parent.children.push(this);
  }
  const self = this;
  this.classList = {
    add: (c) => self._cls.add(c), remove: (c) => self._cls.delete(c), contains: (c) => self._cls.has(c),
    toggle: (c, force) => { const on = force === undefined ? !self._cls.has(c) : !!force; if (on) self._cls.add(c); else self._cls.delete(c); return on; },
  };
}
El.prototype.getAttribute = function (n) { return n in this.attrs ? this.attrs[n] : null; };
El.prototype.hasAttribute = function (n) { return n in this.attrs; };
El.prototype.setAttribute = function (n, v) { this.attrs[n] = String(v); };
function matchOne(e, sel) {
  const m = /^([a-z]*)((?:\.[\w-]+)*)((?:\[[\w-]+\])*)$/.exec(sel.trim());
  if (!m) throw new Error('the harness does not know the selector ' + sel);
  if (m[1] && e.tagName !== m[1].toUpperCase()) return false;
  for (const c of m[2].split('.').filter(Boolean)) if (!e._cls.has(c)) return false;
  for (const a of (m[3].match(/[\w-]+/g) || [])) if (!(a in e.attrs)) return false;
  return true;
}
El.prototype.matches = function (sel) { return sel.split(',').some((s) => matchOne(this, s)); };
El.prototype.closest = function (sel) { for (let e = this; e; e = e.parentElement) if (e.matches(sel)) return e; return null; };
El.prototype.querySelectorAll = function (sel) {
  const out = []; const walk = (e) => { for (const c of e.children) { if (c.matches(sel)) out.push(c); walk(c); } };
  walk(this); return out;
};
El.prototype.querySelector = function (sel) { return this.querySelectorAll(sel)[0] || null; };
const body = new El('body');
const article = new El('article', '', { 'data-rdy': 'content' }, body);
const blockA = new El('div', 'block block-prose', { 'data-rdy-block': 'p-AAAAA', id: 'a' }, article);
const toolsA = new El('span', 'tools', {}, blockA);
const linkBtn = new El('button', 'tool copy-link', { 'data-href': '#a' }, toolsA);
const paraA = new El('p', '', {}, blockA);
const anchorA = new El('a', '', { href: 'x.html' }, paraA);
const blockB = new El('div', 'block block-prose', { 'data-rdy-block': 'p-BBBBB', id: 'b' }, article);
new El('span', 'tools', {}, blockB);
const paraB = new El('p', '', {}, blockB);
const listeners = {};
const document = {
  body, activeElement: body,
  addEventListener: (t, f) => { (listeners[t] = listeners[t] || []).push(f); },
  querySelectorAll: (s) => body.querySelectorAll(s),
  querySelector: (s) => body.querySelector(s),
  createElement: (t) => new El(t),
};
const window = { getSelection: () => ({ rangeCount: 0, isCollapsed: true, toString: () => '' }) };
const navigator = {};
const location = { href: 'https://example.invalid/p.html' };
new Function('window', 'document', 'navigator', 'location', fs.readFileSync(process.argv[process.argv.length - 1], 'utf8'))(window, document, navigator, location);
const fire = (target, pointerType) => (listeners.pointerup || []).forEach((f) => f({
  type: 'pointerup', target, pointerType, preventDefault: () => { throw new Error('preventDefault was called'); },
}));
const open = () => body.querySelectorAll('[data-rdy-block]').filter((b) => b.classList.contains('tools-open')).map((b) => b.attrs.id);
const out = {};
fire(paraA, 'touch'); out.tapA = open();
fire(paraB, 'touch'); out.tapB = open();
fire(paraB, 'touch'); out.tapBAgain = open();
fire(paraA, 'mouse'); out.mouse = open();
fire(anchorA, 'touch'); out.link = open();
fire(paraA, 'touch'); fire(linkBtn, 'touch'); out.button = open();
fire(body, 'touch'); out.outside = open();
fire(paraA, 'pen'); out.pen = open();
console.log(JSON.stringify(out));
"##;

#[test]
fn tbar1_a_tap_shows_one_blocks_toolbar_and_a_tap_elsewhere_hides_it() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!(
            "node is not installed: the tap harness is skipped (the visual review taps it in a browser)"
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/copy.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", TAP_HARNESS, "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "tapA": ["a"], "tapB": ["b"], "tapBAgain": [], "mouse": [], "link": [],
            "button": ["a"], "outside": [], "pen": ["a"]
        })
    );
}
