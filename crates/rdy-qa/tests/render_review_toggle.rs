//! DX4: the Show changes toggle, the stylesheet's review rules, and canon and tags showing no
//! change (rv1; FW-RV1, FW-RV2).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_qa::site::{cascaded, parse_css};

const BUTTON: &str = "<button id=\"show-changes\" type=\"button\" aria-pressed=\"true\" hidden>Show changes</button>";

/// Every page of a site but the permalink pages under `go/`.
fn layout_pages(site: &std::path::Path) -> Vec<(String, String)> {
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

#[test]
fn rv1_a_draft_page_offers_show_changes_hidden_until_script_and_canon_and_tags_do_not() {
    let (_d, _r, _ids, out) = review_site(&Scope::View(REVIEW_VIEW.into()));
    let draft = layout_pages(out.path());
    assert!(draft.len() >= 4, "{}", draft.len());
    for (p, h) in &draft {
        assert_eq!(h.matches(BUTTON).count(), 1, "{p}");
    }
    let app = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    assert!(app.contains("show-changes") && app.contains("rdy-changes"));

    let (_d, r, _ids) = review_project();
    let landing: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    for scope in [Scope::Canon, Scope::At(landing)] {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), &scope, out.path()).unwrap();
        for (p, h) in layout_pages(out.path()) {
            for gone in [
                "show-changes",
                "has-chg",
                "chg-label",
                "chg-removed",
                "nav-removed",
                "Changes in this draft",
            ] {
                assert!(!h.contains(gone), "{scope:?} {p} holds {gone}");
            }
        }
    }
}

#[test]
fn rv1_the_stylesheet_hides_markers_only_when_show_changes_is_off() {
    let (_d, _r, _ids, out) = review_site(&Scope::View(REVIEW_VIEW.into()));
    let css = parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap());
    let at = |sel: &str, prop: &str, w: u32| cascaded(&css, sel, prop, w).unwrap_or_default();
    for w in [360, 900, 901, 1280] {
        assert_eq!(
            at(":root[data-changes=\"hidden\"] .chg", "display", w),
            "none",
            "width {w}"
        );
        assert_eq!(
            at(
                ":root[data-changes=\"hidden\"] .has-chg",
                "border-inline-start-color",
                w
            ),
            "transparent",
            "width {w}"
        );
        assert_ne!(
            at(".chg", "display", w),
            "none",
            "shown without script, width {w}"
        );
        assert_ne!(at(".chg-label", "display", w), "none", "width {w}");
        assert!(
            !at(".has-chg", "border-inline-start", w).is_empty(),
            "width {w}"
        );
        assert_eq!(
            at(".has-chg.chg-edited", "border-inline-start-color", w),
            "var(--accent)",
            "width {w}"
        );
        assert_eq!(
            at(".has-chg.chg-new", "border-inline-start-color", w),
            "var(--success)",
            "width {w}"
        );
        assert_eq!(at(".chg-label", "user-select", w), "none", "width {w}");
    }
}

/// A just-enough DOM for `review.js`: a root element, the button, and a session storage that can
/// be made to throw. It loads the script, clicks, reloads, and reports the state after each.
const TOGGLE_HARNESS: &str = r##"
const fs = require('fs');
const src = fs.readFileSync(process.argv[process.argv.length - 1], 'utf8');
function El() { this.attrs = {}; this.hidden = true; this.listeners = {}; }
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
function load(withButton, storage) {
  const root = new El();
  const button = withButton ? new El() : null;
  if (button) button.attrs['aria-pressed'] = 'true';
  const document = {
    documentElement: root,
    getElementById: (id) => (id === 'show-changes' ? button : null),
    addEventListener: () => {},
  };
  new Function('window', 'document', 'sessionStorage', src)({}, document, storage);
  return { root, button };
}
const state = (p, s) => ({
  hidden: p.button.hidden, pressed: p.button.getAttribute('aria-pressed'),
  root: p.root.getAttribute('data-changes'), stored: s.getItem('rdy-changes'),
});
const out = {};
const s1 = Store(false);
const p1 = load(true, s1); out.initial = state(p1, s1);
p1.button.click(); out.off = state(p1, s1);
p1.button.click(); out.on = state(p1, s1);
p1.button.click();
const p2 = load(true, s1); out.reload = state(p2, s1);
const p3 = load(false, Store(false)); out.none = p3.root.getAttribute('data-changes');
const p4 = load(true, Store(true)); p4.button.click();
out.broken = { hidden: p4.button.hidden, pressed: p4.button.getAttribute('aria-pressed'), root: p4.root.getAttribute('data-changes') };
console.log(JSON.stringify(out));
"##;

#[test]
fn rv1_show_changes_hides_and_shows_the_markers_and_remembers_for_the_session() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!(
            "node is not installed: the toggle harness is skipped (the visual review presses it)"
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/review.js"
    );
    let o = std::process::Command::new("node")
        .args(["-e", TOGGLE_HARNESS, "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "initial": {"hidden": false, "pressed": "true", "root": null, "stored": null},
            "off": {"hidden": false, "pressed": "false", "root": "hidden", "stored": "hidden"},
            "on": {"hidden": false, "pressed": "true", "root": null, "stored": null},
            "reload": {"hidden": false, "pressed": "false", "root": "hidden", "stored": "hidden"},
            "none": null,
            "broken": {"hidden": false, "pressed": "false", "root": "hidden"}
        })
    );
}
