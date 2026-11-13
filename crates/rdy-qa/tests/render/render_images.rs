//! rfr2 (D-3DAX4): remote images in block text and figures with a dark image. The site, the PDF
//! and the single HTML file show a remote image's alt text, inside its link when it has one, and
//! load no remote image; the Markdown outputs keep the image. A figure's dark image follows the
//! site's Auto, Light and Dark setting; print shows the light image; Markdown writes the
//! generated four-line picture.
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::tiny;
use rdy_render::print::md::{Layout, write_markdown};
use rdy_render::print::{self, fonts::Fonts};
use rdy_render::{Mode, build_site};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn put(r: &mut Rdy, ops: Value) -> BTreeMap<String, BlockId> {
    let w = write(r, "w1", ops).unwrap_or_else(|e| panic!("write: {e}"));
    land(r, "w1").unwrap();
    w.created
        .iter()
        .map(|(k, v)| (k.clone(), BlockId::parse(v).unwrap()))
        .collect()
}

fn page(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(
        files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", files.keys()))
            .clone(),
    )
    .unwrap()
}

/// `html` without its Copy template: what the page shows and loads.
fn shown(html: &str) -> String {
    match (
        html.find("<template data-copy-page"),
        html.find("</template>"),
    ) {
        (Some(a), Some(b)) => format!("{}{}", &html[..a], &html[b..]),
        _ => html.to_string(),
    }
}

/// Every file under `dir`, by its `/` path.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let rel: Vec<_> = path
                    .strip_prefix(base)
                    .unwrap()
                    .iter()
                    .map(|c| c.to_string_lossy())
                    .collect();
                out.insert(rel.join("/"), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn markdown(r: &Rdy) -> BTreeMap<String, Vec<u8>> {
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    tree(&out)
}

/// A generic picture's bytes: a PNG signature and a distinct tail.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

/// The prose of the remote-image cases: two supported images (one a link's whole label), then
/// forms that stay literal text.
const IMAGES: &str = "Status ![Build <ok> & \\[green\\]](https://img.example.com/badge.svg) \
and [![Docs](https://img.example.com/docs.svg)](https://docs.example.com/guide).";
const LITERAL: [&str; 5] = [
    "![](https://img.example.com/empty.png)",
    "![Plain](http://img.example.com/plain.png)",
    "![Script](javascript:alert)",
    "![Relative](logo.png)",
    "![Open](https://img.example.com/open.png",
];

fn image_project() -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = tiny();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "images", "title": "Images", "doc_type": "page"}}),
        json!({"op": "create", "type": "prose", "parent": "$d", "body": IMAGES}),
    ];
    for l in LITERAL {
        ops.push(json!({"op": "create", "type": "prose", "parent": "$d", "body": format!("Literal {l} end.")}));
    }
    put(&mut r, Value::Array(ops));
    (dir, r)
}

#[test]
fn rfr2_remote_images_are_alt_text_except_in_markdown() {
    let (_d, r) = image_project();
    let st = r.read();

    // the site: alt text, inside its link; no image element and no image URL on the page
    let s = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let html = page(&s.files, "images.html");
    let seen = shown(&html);
    assert!(
        seen.contains("Status Build &lt;ok&gt; &amp; [green] and"),
        "{seen}"
    );
    assert!(
        seen.contains("href=\"https://docs.example.com/guide\"") && seen.contains(">Docs</a>"),
        "{seen}"
    );
    for url in ["badge.svg", "docs.svg"] {
        assert!(!seen.contains(url), "{url}: {seen}");
    }
    assert!(!seen.contains("remote-image"), "{seen}");
    assert!(
        !seen.contains("<img"),
        "a page with no figure has no image: {seen}"
    );
    // the forms that stay literal text, as written
    for (l, want) in [
        (LITERAL[0], "![](https://img.example.com/empty.png)"),
        (LITERAL[1], "![Plain](http://img.example.com/plain.png)"),
        (LITERAL[2], "![Script](javascript:alert)"),
        (LITERAL[3], "![Relative](logo.png)"),
        (LITERAL[4], "![Open](https://img.example.com/open.png"),
    ] {
        assert!(
            seen.contains(&format!("Literal {want} end.")),
            "{l}: {seen}"
        );
    }

    // the single HTML file and the PDF: the alt text, no image URL
    let p = print::build(&st, &Scope::Canon).unwrap();
    let single = String::from_utf8(print::single::single_html(&p)).unwrap();
    assert!(
        single.contains("Status Build &lt;ok&gt; &amp; [green] and"),
        "{single}"
    );
    assert!(single.contains(">Docs</a>"), "{single}");
    for url in ["badge.svg", "docs.svg"] {
        assert!(!single.contains(url), "{url}");
    }
    let (pdf, _, _) = print::pdf::pdf_bytes(&p, &Fonts::load());
    for url in ["badge.svg", "docs.svg"] {
        assert!(
            !pdf.windows(url.len()).any(|w| w == url.as_bytes()),
            "{url}"
        );
    }
    assert!(
        pdf.windows(b"docs.example.com/guide".len())
            .any(|w| w == b"docs.example.com/guide"),
        "the link stays a link"
    );

    // the Markdown tree and the page's Copy payload: the image, inside its link
    let md = page(&markdown(&r), "images.md");
    let image = "![Build \\<ok\\> & \\[green\\]](https://img.example.com/badge.svg)";
    let linked = "[![Docs](https://img.example.com/docs.svg)](https://docs.example.com/guide)";
    assert!(
        md.contains(&format!("Status {image} and {linked}.")),
        "{md}"
    );
    let copy = html
        .split("<template data-copy-page hidden>")
        .nth(1)
        .unwrap();
    assert!(copy.contains("img.example.com/badge.svg"), "{copy}");
    assert!(copy.contains("![Docs]("), "{copy}");
    // a literal form never becomes an image in Markdown either
    for url in [
        "empty.png",
        "plain.png",
        "javascript:alert",
        "logo.png",
        "open.png",
    ] {
        let line = md.lines().find(|l| l.contains(url)).unwrap();
        assert!(line.starts_with("Literal \\!\\["), "{url}: {line}");
    }
}

#[test]
fn rfr2_remote_image_limits_reuse_the_inline_parser_bounds() {
    // a URL over the inline parser's 2,048-byte bound stays literal text; an image in a list
    // nested past the parser's six levels is still an image, in the sixth level
    let (_d, mut r) = tiny();
    let long = format!("https://img.example.com/{}.png", "a".repeat(2_100));
    let mut nested = String::new();
    for level in 0..8 {
        nested.push_str(&format!("{}- level {level}\n", "  ".repeat(level)));
    }
    nested.push_str(&format!(
        "{}- ![Deep](https://img.example.com/deep.png)",
        "  ".repeat(8)
    ));
    put(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "limits", "title": "Limits", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": format!("Long ![Long]({long}) end.")},
            {"op": "create", "type": "prose", "parent": "$d", "body": nested}
        ]),
    );
    let md = page(&markdown(&r), "limits.md");
    let long_line = md.lines().find(|l| l.starts_with("Long ")).unwrap();
    assert!(long_line.starts_with("Long \\!\\[Long\\]("), "{long_line}");
    assert!(
        md.contains("![Deep](https://img.example.com/deep.png)"),
        "{md}"
    );
    let s = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let seen = shown(&page(&s.files, "limits.html"));
    assert!(!seen.contains("<img"), "{seen}");
    assert!(!seen.contains("deep.png"), "{seen}");
    assert!(seen.contains("Deep"), "{seen}");
    assert!(seen.matches("<ul").count() <= 6, "{seen}");
}

/// A project with a paired figure and a light-only one, both on the top-level page `figs`.
fn figure_project(alt: &str) -> (tempfile::TempDir, Rdy, Hash, Hash, Hash) {
    let (dir, mut r) = tiny();
    let light = r
        .store_mut()
        .put_blob(&picture("light image bytes"))
        .unwrap();
    let dark = r
        .store_mut()
        .put_blob(&picture("dark image bytes"))
        .unwrap();
    let plain = r
        .store_mut()
        .put_blob(&picture("plain image bytes"))
        .unwrap();
    put(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "figs", "title": "Figures", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {
                "blob": light.to_string(), "media_type": "image/png",
                "dark_blob": dark.to_string(), "dark_media_type": "image/png", "alt": alt}},
            {"op": "create", "type": "figure", "parent": "$d", "fields": {
                "blob": plain.to_string(), "media_type": "image/png", "alt": "Plain figure"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Literal <picture><img src=\"x.png\"></picture> stays text."}
        ]),
    );
    (dir, r, light, dark, plain)
}

/// `b` in standard base64, as a data URI writes it.
fn b64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One CSS rule of site.css: its media condition (empty outside `@media`), its selector and its
/// `display` value.
struct Rule {
    media: String,
    selector: String,
    display: String,
}

/// The `display` rules of `css`, in order, one `@media` level deep.
fn display_rules(css: &str) -> Vec<Rule> {
    let mut out = vec![];
    let mut media: Option<String> = None;
    let mut rest = css;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix("/*") {
            rest = &r[r.find("*/").unwrap() + 2..];
            continue;
        }
        if let Some(r) = rest.strip_prefix('}') {
            media = None;
            rest = r;
            continue;
        }
        let open = rest.find('{').unwrap();
        let head = rest[..open].trim().to_string();
        if let Some(cond) = head.strip_prefix("@media") {
            media = Some(cond.trim().to_string());
            rest = &rest[open + 1..];
            continue;
        }
        let close = rest.find('}').unwrap();
        let body = &rest[open + 1..close];
        for decl in body.split(';') {
            if let Some((k, v)) = decl.split_once(':')
                && k.trim() == "display"
            {
                for sel in head.split(',') {
                    out.push(Rule {
                        media: media.clone().unwrap_or_default(),
                        selector: sel.trim().to_string(),
                        display: v.trim().to_string(),
                    });
                }
            }
        }
        rest = &rest[close + 1..];
    }
    out
}

/// Which image of a themed figure shows: the cascade of site.css's `display` rules for the
/// figure's two images, with the system scheme `os_dark`, the page's `data-theme` (none for
/// Auto) and the medium (`screen` or `print`) on a wide window.
fn showing(css: &str, os_dark: bool, theme: Option<&str>, medium: &str) -> (bool, bool) {
    let media_ok = |m: &str| {
        m.split(" and ").all(|part| {
            let part = part.trim();
            match part {
                "" => true,
                "screen" | "print" => part == medium,
                "(prefers-color-scheme: dark)" => os_dark,
                p if p.starts_with("(max-width") => false,
                p if p.starts_with("(min-width") => true,
                _ => false,
            }
        })
    };
    // the selector's parts before the image and its specificity, when it can name `img_class`
    let matches = |sel: &str, img_class: &str| -> Option<usize> {
        let parts: Vec<&str> = sel.split_whitespace().collect();
        let (last, ancestors) = parts.split_last()?;
        let hits_img =
            *last == format!(".{img_class}") || (*last == "img" && ancestors == [".figure"]);
        if !hits_img {
            return None;
        }
        for a in ancestors {
            let ok = match *a {
                ".figure" | ".figure-themed" => true,
                ":root:not([data-theme=\"light\"])" => theme != Some("light"),
                ":root[data-theme=\"dark\"]" => theme == Some("dark"),
                _ => false,
            };
            if !ok {
                return None;
            }
        }
        Some(sel.matches('.').count() + sel.matches('[').count() + sel.matches(":root").count())
    };
    let shows = |img_class: &str| {
        let mut best: Option<(usize, String)> = None;
        for r in display_rules(css) {
            if !media_ok(&r.media) {
                continue;
            }
            if let Some(spec) = matches(&r.selector, img_class)
                && best.as_ref().is_none_or(|(b, _)| spec >= *b)
            {
                best = Some((spec, r.display.clone()));
            }
        }
        best.is_none_or(|(_, d)| d != "none")
    };
    (shows("figure-light"), shows("figure-dark"))
}

#[test]
fn rfr2_theme_toggle_selects_the_stored_variant() {
    let (_d, r, light, dark, plain) = figure_project("A loop");
    let st = r.read();
    let s = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let html = page(&s.files, "figs.html");
    let file = |h: &Hash| {
        s.files
            .keys()
            .find(|k| k.starts_with("assets/") && k.contains(&h.to_string()[..16]))
            .unwrap_or_else(|| panic!("no asset for {h}: {:?}", s.files.keys()))
            .clone()
    };
    let (lf, df, pf) = (file(&light), file(&dark), file(&plain));
    // the paired figure carries both images, each with the alt text; the light-only one is as
    // before
    let themed = format!(
        "<figure class=\"figure figure-themed\"><img class=\"figure-light\" src=\"{lf}\" alt=\"A loop\" loading=\"lazy\"><img class=\"figure-dark\" src=\"{df}\" alt=\"A loop\" loading=\"lazy\">"
    );
    assert!(html.contains(&themed), "{html}");
    assert!(
        html.contains(&format!("<img src=\"{pf}\" alt=\"Plain figure\"")),
        "{html}"
    );
    assert_eq!(html.matches("figure-themed").count(), 1, "{html}");

    // the cascade: Auto follows the system; Light and Dark win against the opposite system
    // setting; print always shows the light image
    let css = page(&s.files, "assets/site.css");
    for (os_dark, theme, want) in [
        (false, None, (true, false)),
        (true, None, (false, true)),
        (true, Some("light"), (true, false)),
        (false, Some("dark"), (false, true)),
        (false, Some("light"), (true, false)),
        (true, Some("dark"), (false, true)),
    ] {
        assert_eq!(
            showing(&css, os_dark, theme, "screen"),
            want,
            "os dark {os_dark}, theme {theme:?}"
        );
        assert_eq!(
            showing(&css, os_dark, theme, "print"),
            (true, false),
            "print: os dark {os_dark}, theme {theme:?}"
        );
    }
    // the toggle writes the explicit choice as data-theme and Auto as none
    let js = page(&s.files, "assets/app.js");
    assert!(js.contains("root.setAttribute('data-theme', choice)"));
    assert!(js.contains("root.removeAttribute('data-theme')"));

    // the PDF and the single HTML file hold the light image only
    let p = print::build(&st, &Scope::Canon).unwrap();
    let single = String::from_utf8(print::single::single_html(&p)).unwrap();
    assert_eq!(single.matches("<img").count(), 2, "{single}");
    assert!(single.contains(&b64(&picture("light image bytes"))));
    assert!(!single.contains(&b64(&picture("dark image bytes"))));
}

#[test]
fn rfr2_markdown_writes_only_generated_picture_html() {
    let alt = "A \"loop\" <b> & 'q'";
    let (_d, r, light, dark, plain) = figure_project(alt);
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    let files = markdown(&r);
    let md = page(&files, "figs.md");
    let (lf, df, pf) = (
        &p.blobs[&light].file,
        &p.blobs[&dark].file,
        &p.blobs[&plain].file,
    );
    let picture_lines = format!(
        "<picture>\n<source media=\"(prefers-color-scheme: dark)\" srcset=\"{df}\">\n<img alt=\"A &quot;loop&quot; &lt;b&gt; &amp; &#39;q&#39;\" src=\"{lf}\">\n</picture>\n"
    );
    assert!(md.contains(&picture_lines), "{md}");
    assert_eq!(md.matches("<picture>").count(), 1, "{md}");
    // the light-only figure keeps its ordinary image form
    assert!(md.contains(&format!("![Plain figure]({pf})")), "{md}");
    // both raw asset bytes are written
    assert_eq!(files[lf.as_str()], picture("light image bytes"));
    assert_eq!(files[df.as_str()], picture("dark image bytes"));
    assert_eq!(files[pf.as_str()], picture("plain image bytes"));
    // authored HTML in a block never passes through
    let line = md.lines().find(|l| l.starts_with("Literal ")).unwrap();
    assert!(!line.contains("<picture>"), "{line}");
    assert!(line.contains("stays text."), "{line}");
}
