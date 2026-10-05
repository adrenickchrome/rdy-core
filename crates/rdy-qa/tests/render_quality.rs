//! The leak scan and structure checks over the golden, hostile and generated projects (scan1, struct1, nojs1).
use rdy_engine::Scope;
use rdy_qa::{golden, site::*};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

fn no_leaks(site: &Path, what: &str) {
    let found = leaks(site);
    assert!(
        found.is_empty(),
        "{what}: {} leaks\n{}",
        found.len(),
        found
            .iter()
            .map(|l| format!("  {} [{:?}] {}: {:?}", l.page, l.zone, l.pattern, l.text))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "SITE-BAR: Task post-G3"]
fn scan1_the_golden_site_has_no_leak_in_any_scope() {
    let (_d, _r, _ids, out) = golden::site_canon();
    no_leaks(out.path(), "canon");
    let (_d, _r, _ids, out) = golden::site_frozen();
    no_leaks(out.path(), "frozen");
    let (_d, _r, _ids, out) = golden::site_view();
    no_leaks(out.path(), "view");
}

#[test]
#[ignore = "SITE-BAR: Task post-G3"]
fn scan1_the_hostile_and_generated_sites_have_no_leak() {
    let (_d, mut r) = golden::hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    no_leaks(out.path(), "hostile");
    let (_d, mut r) = golden::generated_tree();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    no_leaks(out.path(), "generated");
}

/// Review Focus 3: a content hit is reported with the page, the zone, the pattern and the text.
#[test]
fn scan1_a_content_hit_names_the_page_zone_pattern_and_text() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.html"),
        "<main><div data-rdy=\"content\"><p>see wallets/transactions.yaml and d-7KQ2M</p></div></main>",
    )
    .unwrap();
    let found = leaks(dir.path());
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(
        (
            found[0].page.as_str(),
            found[0].zone,
            found[0].pattern,
            found[0].text.as_str()
        ),
        ("p.html", Zone::Content, "F1", "d-7KQ2M")
    );
    assert_eq!(
        (found[1].pattern, found[1].text.as_str()),
        ("F6", "wallets/transactions.yaml")
    );
}

#[test]
fn scan1_label_words_are_leaks_only_as_whole_labels() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.html"),
        "<main><h2>Table 3: shapes</h2><span class=\"eyebrow\">table</span><span class=\"meta\">folder · x</span><p data-rdy=\"content\">the flow is stable</p></main>",
    )
    .unwrap();
    let found = leaks(dir.path());
    assert_eq!(
        found
            .iter()
            .map(|l| (l.pattern, l.text.as_str()))
            .collect::<Vec<_>>(),
        [("F9", "table"), ("F9", "folder · x")]
    );
}

#[test]
fn scan1_the_agents_zone_is_never_scanned_and_the_trail_is_scanned_like_content() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.html"),
        "<main><details data-rdy=\"agents\"><summary>For agents</summary><p>r-7KQ2M in .rdy/store</p></details><div data-rdy=\"trail\"><p>ruled in d-7KQ2M</p></div></main>",
    )
    .unwrap();
    let found = leaks(dir.path());
    assert_eq!(
        found
            .iter()
            .map(|l| (l.zone, l.pattern, l.text.as_str()))
            .collect::<Vec<_>>(),
        [(Zone::Trail, "F1", "d-7KQ2M")]
    );
}

#[test]
fn scan1_chrome_only_patterns_skip_content_and_f11_reads_nav_version_crumbs_and_badges() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.html"),
        "<main><p data-rdy=\"content\">the doc_type field and a landing page</p><p>a landing note</p><nav><a>3 landings</a></nav><span class=\"version\">v1 + 2 unlanded changes</span><span class=\"badge\">fsck</span><span class=\"crumbs\">materialized</span><p>doc_type</p></main>",
    )
    .unwrap();
    let found = leaks(dir.path());
    assert_eq!(
        found
            .iter()
            .map(|l| (l.pattern, l.text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("F11", "landings"),
            ("F11", "unlanded"),
            ("F11", "fsck"),
            ("F11", "materialized"),
            ("F5", "doc_type"),
        ]
    );
}

// ------------------------------------------------------------------------- structure

struct Page {
    rel: String,
    html: String,
    /// The `<page>.json` twin, when there is one.
    json: Option<String>,
}

struct Canon {
    pages: Vec<Page>,
    css: Option<String>,
}

/// The canon site, rendered once for the whole file and read into memory.
fn canon() -> &'static Canon {
    static CANON: std::sync::OnceLock<Canon> = std::sync::OnceLock::new();
    CANON.get_or_init(|| {
        let (_d, _r, _ids, out) = golden::site_canon();
        let pages = golden::pages(out.path())
            .into_iter()
            .map(|p| Page {
                rel: p
                    .strip_prefix(out.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                html: std::fs::read_to_string(&p).unwrap(),
                json: std::fs::read_to_string(p.with_extension("json")).ok(),
            })
            .collect();
        Canon {
            pages,
            css: std::fs::read_to_string(out.path().join("assets/site.css")).ok(),
        }
    })
}

fn redirect(html: &str) -> bool {
    html.contains("data-rdy-kind=\"redirect\"")
}

/// A doc page: not a folder or home page, a collected page or a `go/` page.
fn doc_page(rel: &str, html: &str) -> bool {
    !redirect(html)
        && !rel.starts_with("go/")
        && !rel.ends_with("index.html")
        && rel != "glossary.html"
        && rel != "questions.html"
}

/// Runs `check` over every page of the canon site, skipping `go/` pages when `skip_go`.
fn each_page(skip_go: bool, check: impl Fn(&Page)) {
    let site = canon();
    assert!(site.pages.len() > 10, "{} pages", site.pages.len());
    for page in &site.pages {
        if skip_go && (page.rel.starts_with("go/") || redirect(&page.html)) {
            continue;
        }
        check(page);
    }
}

fn block_ids(html: &str) -> BTreeSet<String> {
    attrs(html, "data-rdy-block")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// What is wrong with the `<h1>` count of a page, if anything. A `go/` permalink page is a
/// one-line "this page has moved" redirect (Task 4's template) and carries no heading.
fn h1_problem(rel: &str, html: &str) -> Option<String> {
    if redirect(html) {
        return None;
    }
    let n = page_titles(html).iter().filter(|(l, _)| *l == 1).count();
    (n != 1).then(|| format!("{rel}: {n} h1"))
}

#[test]
fn struct1_every_page_has_exactly_one_h1() {
    each_page(false, |Page { rel, html, .. }| {
        if let Some(problem) = h1_problem(rel, html) {
            panic!("{problem}");
        }
    });
}

/// Task 4's redirect template, verbatim in shape: a `<main>` with one sentence and no heading.
const REDIRECT_PAGE: &str = "<!doctype html><html lang=\"en\" data-rdy-kind=\"redirect\"><head><meta charset=\"utf-8\"><title>Intro</title></head><body><main id=\"main\"><p>This page has moved to <a href=\"../intro.html\">Intro</a>.</p></main></body></html>";

#[test]
fn struct1_a_redirect_page_is_not_held_to_the_one_h1_rule() {
    assert_eq!(h1_problem("go/d-7KQ2M.html", REDIRECT_PAGE), None);
    assert!(
        h1_problem("go/d-7KQ2M.html", "<body><main><h2>x</h2></main></body>").is_some(),
        "a non-redirect page without an h1 still fails"
    );
    assert!(
        h1_problem("a.html", "<body><h1>a</h1><h1>b</h1></body>").is_some(),
        "two h1 still fail"
    );
}

#[test]
fn struct1_heading_levels_never_skip() {
    each_page(false, |Page { rel, html, .. }| {
        let levels = page_titles(html);
        if let Some((first, _)) = levels.first() {
            assert_eq!(*first, 1, "{rel}: {levels:?}");
        }
        for w in levels.windows(2) {
            assert!(w[1].0 <= w[0].0 + 1, "{rel}: {levels:?}");
        }
    });
}

#[test]
fn struct1_landmarks_are_present() {
    each_page(true, |Page { rel, html, .. }| {
        assert!(html.contains("<header"), "{rel}: header");
        assert!(
            html.contains("<nav aria-label=") || html.contains("<nav class=\"crumbs\" aria-label="),
            "{rel}: nav with a label"
        );
        assert!(html.contains("<main"), "{rel}: main");
        assert!(html.contains("<footer"), "{rel}: footer");
        if doc_page(rel, html) {
            assert!(html.contains("<aside"), "{rel}: aside");
        }
    });
}

#[test]
fn struct1_a_skip_link_comes_first_in_the_body() {
    each_page(true, |Page { rel, html, .. }| {
        let body = html
            .find("<body")
            .unwrap_or_else(|| panic!("{rel}: no body"));
        let after = &html[body..];
        let after = after[after.find('>').unwrap() + 1..].trim_start();
        assert!(
            after.starts_with("<a class=\"skip\" href=\"#main\">"),
            "{rel}: {:?}",
            &after[..after.len().min(60)]
        );
    });
}

#[test]
fn struct1_every_page_has_the_version_pill_the_crumbs_and_the_theme_toggle() {
    each_page(true, |Page { rel, html, .. }| {
        assert!(html.contains("data-rdy-version"), "{rel}: version pill");
        assert!(html.contains("class=\"crumbs\""), "{rel}: crumbs");
        assert!(
            html.contains("role=\"radiogroup\""),
            "{rel}: theme radiogroup"
        );
    });
}

#[test]
fn struct1_doc_pages_have_previous_and_next() {
    each_page(true, |Page { rel, html, .. }| {
        if doc_page(rel, html) {
            assert!(
                html.contains("<nav class=\"pager\" aria-label=\"Previous and next\">"),
                "{rel}: pager"
            );
        }
    });
}

#[test]
fn struct1_no_link_has_an_underscore_directory() {
    each_page(false, |Page { rel, html, .. }| {
        for attr in ["href", "src", "action"] {
            for v in attrs(html, attr) {
                let v = unescape(v);
                let path = v.split(['#', '?']).next().unwrap_or("");
                assert!(
                    !path.split('/').any(|s| s.starts_with('_')),
                    "{rel}: {attr}={v}"
                );
            }
        }
    });
}

/// A value that names another origin: a scheme or a protocol-relative `//`.
fn external(v: &str) -> bool {
    let v = v.trim();
    if v.starts_with("//") {
        return true;
    }
    let scheme = v
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        })
        .filter(|s| s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    scheme.is_some_and(|s| !s.eq_ignore_ascii_case("mailto"))
}

/// Every `url(...)` argument and `@import` target in `text`.
fn css_urls(text: &str) -> Vec<String> {
    let mut out = vec![];
    let strip = |s: &str| s.trim().trim_matches(['"', '\'']).trim().to_string();
    let mut rest = text;
    while let Some(i) = rest.find("url(") {
        rest = &rest[i + 4..];
        let end = rest.find(')').unwrap_or(rest.len());
        out.push(strip(&rest[..end]));
    }
    let mut rest = text;
    while let Some(i) = rest.find("@import") {
        rest = &rest[i + 7..];
        let end = rest.find([';', '\n']).unwrap_or(rest.len());
        let arg = rest[..end].trim();
        if !arg.starts_with("url(") {
            out.push(strip(arg.split_whitespace().next().unwrap_or("")));
        }
    }
    out
}

#[test]
fn struct1_nothing_names_another_origin() {
    let site = canon();
    let mut texts: Vec<(&str, &str)> = site
        .pages
        .iter()
        .map(|p| (p.rel.as_str(), p.html.as_str()))
        .collect();
    if let Some(css) = &site.css {
        texts.push(("assets/site.css", css));
    }
    for (rel, text) in texts {
        for attr in ["href", "src", "action"] {
            for v in attrs(text, attr) {
                assert!(!external(&unescape(v)), "{rel}: {attr}={v}");
            }
        }
        for u in css_urls(text) {
            assert!(!external(&u), "{rel}: url/@import {u}");
        }
    }
}

#[test]
fn struct1_every_image_has_alt_text() {
    each_page(false, |Page { rel, html, .. }| {
        for (i, _) in html.match_indices("<img") {
            let tag = &html[i..i + html[i..].find('>').unwrap_or(html.len() - i)];
            let alt = attrs(tag, "alt");
            assert!(
                alt.first().is_some_and(|a| !a.trim().is_empty()),
                "{rel}: {tag}"
            );
        }
    });
}

#[test]
#[ignore = "SITE-BAR: Task 13"]
fn struct1_json_twin_ids_equal_the_pages_block_ids() {
    each_page(true, |Page { rel, html, json }| {
        let twin = json
            .as_deref()
            .unwrap_or_else(|| panic!("{rel}: no .json twin"));
        let twin: serde_json::Value =
            serde_json::from_str(twin).unwrap_or_else(|e| panic!("{rel}: twin is not JSON: {e}"));
        let ids: BTreeSet<String> = twin["ids"]
            .as_array()
            .unwrap_or_else(|| panic!("{rel}: twin has no `ids` array"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, block_ids(html), "{rel}");
    });
}

#[test]
fn struct1_html_declares_its_language_and_viewport() {
    each_page(false, |Page { rel, html, .. }| {
        assert!(html.contains("<html lang=\""), "{rel}: lang");
        assert!(
            html.contains(
                "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
            ),
            "{rel}: viewport"
        );
    });
}

// ------------------------------------------------------------------------------ no JS

/// One element that holds authored content: the content zone or a block.
struct Container {
    /// `content zone` or the block's id.
    what: String,
    open_tag: String,
    inner: String,
}

/// Every element carrying `data-rdy="content"` or `data-rdy-block`, with its inner markup
/// measured to its own closing tag (same-name elements nest).
fn containers(html: &str) -> Vec<Container> {
    static OPEN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<([A-Za-z][A-Za-z0-9]*)\b[^>]*>").unwrap());
    static BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"data-rdy-block="([^"]*)""#).unwrap());
    let mut out = vec![];
    for open in OPEN.captures_iter(html) {
        let tag_text = open.get(0).unwrap();
        let what = if tag_text.as_str().contains("data-rdy=\"content\"") {
            "content zone".to_string()
        } else if let Some(id) = BLOCK.captures(tag_text.as_str()) {
            id[1].to_string()
        } else {
            continue;
        };
        let name = open[1].to_ascii_lowercase();
        let (opener, closer) = (format!("<{name}"), format!("</{name}"));
        let lower = html.to_ascii_lowercase();
        let (mut depth, mut at, mut end) = (1usize, tag_text.end(), html.len());
        while depth > 0 {
            let next_open = lower[at..].find(&opener).map(|i| at + i);
            let Some(next_close) = lower[at..].find(&closer).map(|i| at + i) else {
                break;
            };
            match next_open {
                Some(o)
                    if o < next_close
                        && lower[o + opener.len()..].starts_with(|c: char| {
                            c.is_ascii_whitespace() || c == '>' || c == '/'
                        }) =>
                {
                    depth += 1;
                    at = o + opener.len();
                }
                Some(o) if o < next_close => at = o + opener.len(),
                _ => {
                    depth -= 1;
                    at = next_close + closer.len();
                    if depth == 0 {
                        end = next_close;
                    }
                }
            }
        }
        out.push(Container {
            what,
            open_tag: tag_text.as_str().to_string(),
            inner: html[tag_text.end()..end.max(tag_text.end())].to_string(),
        });
    }
    out
}

/// An opening tag that hides its element without a script: the `hidden` attribute or an inline
/// `display:none` / `visibility:hidden`.
fn hides(tag: &str) -> bool {
    static HIDDEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)(\shidden(\s|=|>|/))|((display|visibility)\s*:\s*(none|hidden))"#)
            .unwrap()
    });
    HIDDEN.is_match(tag)
}

/// What a page asks of JavaScript to show its content, read from the markup as served: the
/// content zone and every authored block must carry their text (or an image, diagram or rule)
/// in the HTML itself, none may be hidden, and no script may sit inside the content.
fn served_problems(html: &str) -> Vec<String> {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    static SCRIPT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<(script|style)\b.*?</(script|style)\s*>").unwrap());
    static OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[A-Za-z][^>]*>").unwrap());
    static NON_TEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)<(img|svg|video|hr|iframe|object)\b").unwrap());
    let mut out = vec![];
    for c in containers(html) {
        if hides(&c.open_tag) {
            out.push(format!("{}: hidden in the served markup", c.what));
        }
        if let Some(tag) = OPEN.find_iter(&c.inner).find(|t| hides(t.as_str())) {
            out.push(format!(
                "{}: hidden element inside, {}",
                c.what,
                tag.as_str()
            ));
        }
        if c.inner.to_ascii_lowercase().contains("<script") {
            out.push(format!("{}: a script inside the content", c.what));
        }
        let visible = SCRIPT.replace_all(&c.inner, "");
        let text = unescape(&TAG.replace_all(&visible, " "));
        if text.trim().is_empty() && !NON_TEXT.is_match(&visible) {
            out.push(format!("{}: empty in the served markup", c.what));
        }
    }
    out
}

fn served_ok(site: &Path, page: &str) {
    let html = golden::html(site, page);
    let problems = served_problems(&html);
    assert!(problems.is_empty(), "{page}:\n{}", problems.join("\n"));
    assert!(
        visible_runs(&html).iter().any(|r| !r.text.is_empty()),
        "{page}: no visible text at all"
    );
}

#[test]
fn nojs1_every_kind_of_page_serves_its_content_without_scripts() {
    let (_d, _r, _ids, out) = golden::site_canon();
    for page in [
        "index.html",
        "specification/index.html",
        "specification/patterns.html",
        "glossary.html",
        "questions.html",
    ] {
        served_ok(out.path(), page);
    }
}

#[test]
fn nojs1_a_frozen_page_serves_its_content_without_scripts() {
    let (_d, _r, _ids, out) = golden::site_frozen();
    served_ok(out.path(), "specification/patterns.html");
}

fn problems(html: &str) -> String {
    served_problems(html).join("\n")
}

#[test]
fn nojs1_a_fully_served_page_has_no_problem() {
    let ok = "<main><div data-rdy=\"content\"><section data-rdy-block=\"s-0AAAA\"><h2>A</h2><p data-rdy-block=\"p-0AAAA\">text</p><figure data-rdy-block=\"fg-0AAAA\"><img src=\"a.png\" alt=\"x\"></figure><pre class=\"mermaid\">graph TD</pre></section></div></main><script src=\"a.js\" defer></script><script>init()</script>";
    assert_eq!(problems(ok), "");
}

#[test]
fn nojs1_an_empty_content_container_waiting_for_a_script_is_caught() {
    let found =
        problems("<main><div data-rdy=\"content\"></div></main><script src=\"a.js\"></script>");
    assert!(found.contains("empty"), "{found}");
    let found = problems("<div data-rdy=\"content\"><p data-rdy-block=\"p-0AAAA\"></p></div>");
    assert!(
        found.contains("p-0AAAA") && found.contains("empty"),
        "{found}"
    );
}

#[test]
fn nojs1_content_hidden_until_a_script_reveals_it_is_caught() {
    for hide in [
        "hidden",
        "style=\"display:none\"",
        "style=\"color: red; display : none;\"",
        "style=\"visibility:hidden\"",
    ] {
        let page = format!(
            "<div data-rdy=\"content\"><p>shown</p><section {hide}><p data-rdy-block=\"p-0BBBB\">later</p></section></div>"
        );
        assert!(
            problems(&page).contains("hidden"),
            "{hide}: {}",
            problems(&page)
        );
    }
    let page = "<div data-rdy=\"content\" hidden><p>x</p></div>";
    assert!(problems(page).contains("hidden"), "{}", problems(page));
}

#[test]
fn nojs1_chrome_may_be_hidden_and_content_may_not_hold_a_script() {
    let chrome = "<nav><div hidden>results</div></nav><div data-rdy=\"content\"><p>x</p></div>";
    assert_eq!(problems(chrome), "");
    let found = problems("<div data-rdy=\"content\"><p>x</p><script>fill()</script></div>");
    assert!(found.contains("script"), "{found}");
}

#[test]
fn nojs1_nested_same_name_elements_are_measured_to_their_own_close() {
    let page = "<div data-rdy=\"content\"><div><div>a</div></div><div data-rdy-block=\"p-0CCCC\"></div></div><div>tail</div>";
    let found = problems(page);
    assert!(found.contains("p-0CCCC"), "{found}");
    assert!(!found.contains("content container"), "{found}");
}
