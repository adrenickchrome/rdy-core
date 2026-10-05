//! The Tide shell: tokens, toggle, no-flash script, fonts, layout (theme1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

fn site() -> (tempfile::TempDir, tempfile::TempDir) {
    let (d, r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    (d, out)
}

#[test]
fn theme1_the_init_script_is_inline_short_and_before_the_stylesheet_is_applied() {
    let (_d, out) = site();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    let i = h.find("<script>").unwrap();
    let j = h[i..].find("</script>").unwrap();
    let script = &h[i + 8..i + j];
    assert!(script.len() <= 300, "{} bytes", script.len());
    assert!(
        script.contains("localStorage.getItem(\"rdy-theme\")")
            && script.contains("setAttribute(\"data-theme\"")
    );
    assert!(
        i < h.find("assets/site.css").unwrap(),
        "the script precedes the stylesheet link, so the theme is set before the first paint"
    );
    assert!(h.contains("<body>\n<a class=\"skip\" href=\"#main\">Skip to content</a>"));
    assert!(
        h.contains("<details class=\"sidenav-fold\" open>")
            && h.contains("<button id=\"menu\" type=\"button\" aria-expanded=\"false\" aria-controls=\"sidebar\" hidden>Menu</button>")
    );
}

#[test]
fn theme1_every_page_has_the_toggle_landmarks_and_no_external_request() {
    let (_d, out) = site();
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        if h.contains("data-rdy-kind=\"redirect\"") {
            continue;
        }
        for s in [
            "role=\"radiogroup\" aria-label=\"Colour theme\"",
            "data-theme-choice=\"system\"",
            "<header class=\"head\">",
            "<main class=\"content\" id=\"main\"",
            "<footer class=\"foot\">",
            "<nav class=\"crumbs\" aria-label=\"Breadcrumb\">",
            "<html lang=\"en\">",
            "<noscript>",
        ] {
            assert!(h.contains(s), "{}: {s}", p.display());
        }
        assert!(
            !h.contains("http://") && !h.contains("https://") && !h.contains("//fonts."),
            "{}",
            p.display()
        );
    }
    let css = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    assert!(
        css.contains("@font-face")
            && css.contains("url('fonts/onest.woff2')")
            && css.contains("url('fonts/geist-mono.woff2')")
    );
    assert!(!css.contains("http"));
}

#[test]
fn theme1_the_vendored_fonts_are_written_and_sized() {
    let (_d, out) = site();
    assert!(
        out.path()
            .join("assets/fonts/onest.woff2")
            .metadata()
            .unwrap()
            .len()
            > 20_000
    );
    assert!(
        out.path()
            .join("assets/fonts/geist-mono.woff2")
            .metadata()
            .unwrap()
            .len()
            > 20_000
    );
}

fn css(out: &std::path::Path) -> rdy_qa::site::Css {
    rdy_qa::site::parse_css(&std::fs::read_to_string(out.join("assets/site.css")).unwrap())
}

fn at(css: &rdy_qa::site::Css, sel: &str, prop: &str, w: u32) -> String {
    rdy_qa::site::cascaded(css, sel, prop, w).unwrap_or_default()
}

/// FF-A: on a phone the block toolbar is a row of its own, not a float beside the text, and its
/// height is fixed so the Cite button appearing never moves the block.
#[test]
fn theme1_the_phone_toolbar_is_a_fixed_row_and_never_squeezes_the_block() {
    let (_d, out) = site();
    let css = css(out.path());
    for w in [360, 600, 900] {
        assert_eq!(at(&css, ".tools", "float", w), "none", "width {w}");
        assert_eq!(at(&css, ".tools", "display", w), "flex", "width {w}");
        assert_eq!(at(&css, ".tools", "flex-wrap", w), "nowrap", "width {w}");
        assert_eq!(at(&css, ".tools", "height", w), "40px", "width {w}");
        assert_eq!(at(&css, ".tools", "opacity", w), "1", "width {w}");
    }
    assert_eq!(at(&css, ".tools", "float", 1280), "right");
}

/// FF-B: an anchor lands below the sticky header, which is taller on a phone.
#[test]
fn theme1_anchors_land_below_the_sticky_header() {
    let (_d, out) = site();
    let css = css(out.path());
    let rem = |v: String| -> f64 {
        v.strip_suffix("rem")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("scroll-margin-top {v:?} is not in rem"))
    };
    let desk = rem(at(&css, "[id]", "scroll-margin-top", 1280));
    let phone = rem(at(&css, "[id]", "scroll-margin-top", 360));
    assert!(desk >= 3.6, "desktop header is 3.6rem: {desk}");
    assert!(
        phone > desk + 3.0,
        "the phone header has more rows: {phone}"
    );
}

/// FF-C: the brand sizes to its name, the version pill is never cut, and on a phone the pill
/// (with its lock and "snapshot") has a header row of its own.
#[test]
fn theme1_the_header_shows_the_whole_name_and_pill_at_every_width() {
    let (_d, out) = site();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("</span></a> <span class=\"version"),
        "the pill is the brand's sibling, its own grid item"
    );
    let css = css(out.path());
    for w in [901, 1280] {
        let cols = at(&css, ".head", "grid-template-columns", w);
        assert!(
            !cols.contains("16rem") && cols.starts_with("auto"),
            "{w}: {cols}"
        );
        assert_ne!(at(&css, ".version", "text-overflow", w), "ellipsis", "{w}");
        assert_ne!(at(&css, ".version", "display", w), "none", "{w}");
    }
    for w in [360, 900] {
        assert_ne!(at(&css, ".version", "display", w), "none", "{w}");
        assert_eq!(at(&css, ".version", "grid-row", w), "2", "{w}");
        assert_eq!(at(&css, ".head .search-wrap", "grid-row", w), "3", "{w}");
    }
}

/// FF-G: the code block's Copy button is a 40 by 40 target on a phone.
#[test]
fn theme1_the_code_copy_button_is_a_thumb_target() {
    let (_d, out) = site();
    let css = css(out.path());
    for w in [360, 900] {
        assert_eq!(
            at(&css, ".code-head button", "min-height", w),
            "40px",
            "{w}"
        );
        assert_eq!(at(&css, ".code-head button", "min-width", w), "40px", "{w}");
    }
}

#[test]
fn theme1_each_rule_is_defined_once() {
    let (_d, out) = site();
    let css = css(out.path());
    let n = css
        .blocks
        .iter()
        .filter(|b| b.media.is_none() && b.selector.trim() == ".visually-hidden")
        .count();
    assert_eq!(n, 1, ".visually-hidden is defined {n} times");
}
