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
        i < h.find("</head>").unwrap() && i > h.find("assets/site.css").unwrap(),
        "the script follows the stylesheet link and precedes </head>"
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
