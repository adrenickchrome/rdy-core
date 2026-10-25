//! dgm10 (D-94BT2, D-E8P8J, D-WQ5GC, D-FV6G9, D-0VCQ5; system/site p-F63VB, tb-QCTS0, n-REYZ9,
//! n-3S9SC): the site's own not-found page. A local static render writes `404.html` in the site's
//! layout and theme, rooted at the project's `site.base_path`, with no author input. It is an
//! extra file: no reader index, page count or permalink lists it. An authored top-level doc
//! named 404 keeps its page, and the render says so once. Publications and live builds have no
//! generated 404 and never give that warning.
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::*;
use rdy_model::*;
use rdy_render::live::{ProfileHistoryMemo, build_live, build_profile_live};
use rdy_render::render_input::capture_inputs;
use rdy_render::{
    Mode, PublicationArtifact, SiteFiles, build_profile_site, build_publication, build_site,
    render_publication_page, render_site,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TAG: &str = "v1.0.0";
/// The generated page's marker on its html element: checks find the page by it, never by name.
const MARKER: &str = "<html lang=\"en\" data-rdy-kind=\"not-found\">";
const NOINDEX: &str = "<meta name=\"robots\" content=\"noindex\">";
const PAGE_SENTENCE: &str =
    "There is no page at this address. It may have moved, or the link may be wrong.";
/// The render's warning when an authored top-level doc named 404 keeps `404.html`.
const KEPT: &str = "the top-level doc named 404 has the page 404.html: the site's not-found page is left out. Rename the doc to publish it.";
const NOT_FOUND: (&str, &str, &str) = (
    "E_NOT_FOUND",
    "publication page was not found",
    "choose a page from this publication",
);

// --- fixtures -----------------------------------------------------------------------------------

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// F2 (D-DKJS5): the person confirms each exception beside version `p` before it lands.
fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    use rdy_engine::answer::{AnswerRequest, relay::RelayInput};
    use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        let req = AnswerRequest {
            r#ref: item.clone(),
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        };
        r.answer(s, &s.actor, req)
            .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn put(r: &mut Rdy, ops: Value) -> BTreeMap<String, String> {
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "dgm10".into(),
            },
        )
        .unwrap();
    w.created.into_iter().collect()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// The project file of a project named `name`, with `site.base_path` set to `base` when given.
fn config(name: &str, base: Option<&str>) -> String {
    let mut yaml = format!("pack: rdy-standard\nname: \"{name}\"\n");
    if let Some(b) = base {
        yaml.push_str(&format!("site:\n  base_path: {b}\n"));
    }
    yaml
}

/// A project named `name` (with `site.base_path` = `base` when given) holding `ops`, landed.
fn project(
    name: &str,
    base: Option<&str>,
    ops: Value,
) -> (tempfile::TempDir, Rdy, BTreeMap<String, String>) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy.yaml"), config(name, base)).unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let ids = put(&mut r, ops);
    land(&mut r);
    (dir, r, ids)
}

/// A folder `guide` with doc `intro`, and a top-level doc `other`.
fn guide_ops() -> Value {
    json!([
        {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$other", "fields": {"title": "Other section"}},
        {"op": "create", "type": "prose", "id": "$b", "parent": "$os", "body": "Other words."},
        {"op": "create", "type": "folder", "id": "$guide", "fields": {"name": "guide", "title": "Guide"}},
        {"op": "create", "type": "doc", "id": "$intro", "parent": "$guide", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
        {"op": "create", "type": "prose", "id": "$a", "parent": "$s", "body": "Intro words point at {{$other}}."}
    ])
}

/// The landing of tag `v1.0.0`, made now.
fn tag(r: &mut Rdy) -> Hash {
    r.tag(&founder(), TAG, None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == TAG)
        .unwrap()
        .1;
    r.store().read_tag(&tag).unwrap().landing
}

/// The guide project named `Lost project`, with every scope a site is built for: canon, view
/// `work` (an unlanded edit), its proposal, and the frozen site of `v1.0.0` (canon moved on).
fn scoped() -> (tempfile::TempDir, Rdy, Vec<(&'static str, Scope)>) {
    let (dir, mut r, ids) = project("Lost project", None, guide_ops());
    let landing = tag(&mut r);
    put(
        &mut r,
        json!([{"op": "create", "type": "prose", "parent": ids["$os"], "body": "Later words."}]),
    );
    land(&mut r);
    put(
        &mut r,
        json!([{"op": "update", "id": ids["$a"], "body": "Edited intro words."}]),
    );
    let p = r
        .propose(&writer(), "work", "Edit the intro", Some("test summary"))
        .unwrap();
    let proposal = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .unwrap();
    let scopes = vec![
        ("canon", Scope::Canon),
        ("view", Scope::View("work".into())),
        ("proposal", proposal),
        ("frozen", Scope::At(landing)),
    ];
    (dir, r, scopes)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The generated 404 of `site`: it carries the marker.
fn generated(site: &BTreeMap<String, Vec<u8>>) -> String {
    let html = text(site.get("404.html").expect("the site has 404.html"));
    assert!(html.contains(MARKER), "the generated page: {html}");
    html
}

/// Every value of attribute `attr` in `html`, as written.
fn attrs<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let key = format!(" {attr}=\"");
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(&key) {
        rest = &rest[i + key.len()..];
        let j = rest.find('"').expect("a closed attribute");
        out.push(&rest[..j]);
        rest = &rest[j..];
    }
    out
}

/// Every link of `html`: its hrefs, srcs and the search box's index.
fn links(html: &str) -> Vec<String> {
    let mut v: Vec<String> = attrs(html, "href")
        .into_iter()
        .chain(attrs(html, "src"))
        .chain(attrs(html, "data-index"))
        .map(|s| s.replace("&amp;", "&"))
        .collect();
    v.sort();
    v
}

/// `href` resolved against the page at absolute path `page`, as a browser resolves it.
fn resolve(page: &str, href: &str) -> String {
    let (path, frag) = match href.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (href, None),
    };
    let mut out = if path.is_empty() {
        page.to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        let mut parts: Vec<&str> = page.split('/').collect();
        parts.pop();
        for seg in path.split('/') {
            match seg {
                ".." => {
                    parts.pop();
                }
                "." => {}
                s => parts.push(s),
            }
        }
        parts.join("/")
    };
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

// --- tests --------------------------------------------------------------------------------------

#[test]
fn dgm10_static_404_has_exact_words_and_layout() {
    let (_d, r, scopes) = scoped();
    for (what, scope) in &scopes {
        let site = build_site(&r.read(), scope, Mode::Static).unwrap();
        let html = generated(&site.files);
        assert!(
            html.starts_with(&format!("<!doctype html>\n{MARKER}\n")),
            "{what}: {html}"
        );
        assert!(
            html.contains("<title>Page not found · Lost project</title>"),
            "{what}"
        );
        assert_eq!(html.matches(NOINDEX).count(), 1, "{what}");
        assert!(html.contains("<h1>Page not found</h1>"), "{what}");
        assert!(html.contains(&format!("<p>{PAGE_SENTENCE}</p>")), "{what}");
        assert!(
            html.contains("<p><a href=\"#search\">Search these pages</a>, or go to the <a href=\"/index.html\">home page</a>.</p>"),
            "{what}: {html}"
        );
        // the brand, the search box and the theme toggle of the selected site
        assert!(
            html.contains("<a class=\"brand\" href=\"/index.html\">")
                && html.contains("<span class=\"brand-name\">Lost project</span>"),
            "{what}"
        );
        assert!(
            html.contains("<input id=\"search\" type=\"search\"")
                && html.contains("data-index=\"/search-index.json\""),
            "{what}"
        );
        assert_eq!(
            html.matches("role=\"radiogroup\" aria-label=\"Colour theme\"")
                .count(),
            2
        );
        // one crumb, with no link
        assert!(
            html.contains("<nav class=\"crumbs\" aria-label=\"Breadcrumb\"><span aria-current=\"page\">Page not found</span></nav>"),
            "{what}"
        );
        // the footer landmark, with no colophon
        assert!(html.contains("<footer class=\"foot\"></footer>"), "{what}");
        assert!(html.contains("Search needs JavaScript."), "{what}");
        for gone in [
            "data-rdy-version",
            "class=\"pill",
            "class=\"chip",
            "review-bar",
            "review-title",
            "Reviewing ",
            "show-changes",
            "show-diff",
            "colophon",
            "data-rdy-label",
            "class=\"pager\"",
            "class=\"index\"",
            "On this page",
            "class=\"tools\"",
            "copy-ref",
            "class=\"trail",
            "<base",
            "Intro words",
            "v1.0.0",
        ] {
            assert!(!html.contains(gone), "{what}: the 404 holds {gone}: {html}");
        }
        // it has no version label or header slot
        assert!(!site.label_at.contains_key("404.html"), "{what}");
        // no ordinary page carries the marker or the robots line
        for (path, bytes) in &site.files {
            if path != "404.html" && path.ends_with(".html") {
                let t = text(bytes);
                assert!(!t.contains("not-found"), "{what} {path}");
                // a permalink page has its own robots line
                if !t.contains("data-rdy-kind=\"redirect\"") {
                    assert!(!t.contains("name=\"robots\""), "{what} {path}");
                }
            }
        }
    }
}

#[test]
fn dgm10_404_links_are_rooted_at_the_project_base() {
    // one project, its base path changed between the builds
    let (dir, mut r, _ids) = project("Lost project", None, guide_ops());
    let mut normal: Vec<BTreeMap<String, Vec<u8>>> = vec![];
    for base in ["/", "/docs/"] {
        drop(r);
        std::fs::write(
            dir.path().join(".rdy.yaml"),
            config("Lost project", Some(base)),
        )
        .unwrap();
        r = Rdy::open(dir.path()).unwrap();
        assert_eq!(r.read().site_base_path(), base);
        let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
        let html = generated(&site.files);
        let found = links(&html);
        assert!(found.len() >= 8, "{base}: {found:?}");
        for want in [
            format!("{base}assets/site.css"),
            format!("{base}assets/app.js"),
            format!("{base}index.html"),
            format!("{base}search-index.json"),
            format!("{base}guide/index.html"),
            format!("{base}other.html"),
            "#main".into(),
            "#search".into(),
        ] {
            assert!(found.contains(&want), "{base}: {want} in {found:?}");
        }
        assert!(!html.contains("<base"), "{base}");
        for link in &found {
            if link.starts_with('#') {
                // a fragment-only link keeps the page it is on
                let id = &link[1..];
                assert!(html.contains(&format!(" id=\"{id}\"")), "{base}: {link}");
                continue;
            }
            assert!(
                link.starts_with(base),
                "{base}: {link} is rooted at the base"
            );
            // served at a deep missing path or at its own, the page's links lead to the same files
            let at_root = resolve(&format!("{base}404.html"), link);
            let deep = resolve(&format!("{base}a/b/c/missing.html"), link);
            assert_eq!(deep, at_root, "{base}: {link}");
            let file = at_root
                .strip_prefix(base)
                .unwrap()
                .split('#')
                .next()
                .unwrap()
                .to_string();
            assert!(
                site.files.contains_key(&file),
                "{base}: {link} names {file}"
            );
        }
        // the ordinary pages keep their relative links
        let other = text(&site.files["other.html"]);
        for link in links(&other) {
            assert!(!link.starts_with('/'), "{base}: other.html {link}");
        }
        let mut rest = site.files.clone();
        rest.remove("404.html");
        normal.push(rest);
    }
    // only the generated page reads the base: every other file is the same
    assert!(normal[0] == normal[1], "the base changes only 404.html");
}

#[test]
fn dgm10_404_is_absent_from_reader_indexes_and_counts() {
    let (_d, r, _ids) = project("Lost project", None, guide_ops());
    let st = r.read();
    let site = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    generated(&site.files);
    let live = build_site(&st, &Scope::Canon, Mode::Live { root: "/" }).unwrap();
    assert!(
        !live.files.contains_key("404.html"),
        "a live build has none"
    );
    // the page count is the documentation pages', the same as the live build's
    let pages = site
        .files
        .keys()
        .filter(|p| p.ends_with(".html") && !p.starts_with("go/"))
        .count();
    assert_eq!(site.summary.pages, pages - 1, "{:?}", site.files.keys());
    assert_eq!(site.summary.pages, live.summary.pages);
    assert_eq!(site.summary.redirects, live.summary.redirects);
    // no other file names it or its words: navigation, pager, permalinks, llms files, search
    for (path, bytes) in &site.files {
        if path == "404.html" {
            continue;
        }
        let t = String::from_utf8_lossy(bytes);
        if !(path.ends_with(".html") || path.ends_with(".txt") || path.ends_with(".json")) {
            continue;
        }
        for word in ["404.html", "Page not found", "not-found", PAGE_SENTENCE] {
            assert!(!t.contains(word), "{path} holds {word}");
        }
    }
    let index: Value = serde_json::from_slice(&site.files["search-index.json"]).unwrap();
    let entries = index["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|e| !e.to_string().contains("404.html") && e["title"] != "Page not found")
    );
}

#[test]
fn dgm10_authored_404_wins_with_one_warning() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$lost", "fields": {"name": "404", "title": "Authored lost page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$lost", "fields": {"title": "Where to go"}},
        {"op": "create", "type": "prose", "parent": "$ls", "body": "AUTHOREDCANARY words."},
        {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$other", "fields": {"title": "Flow"}},
        {"op": "create", "type": "diagram", "parent": "$os", "fields": {"lang": "mermaid", "alt": "Broken"}, "body": "flowchart LR\n A["}
    ]);
    let (_d, r, _ids) = project("Lost project", None, ops);
    let st = r.read();
    let site = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let html = text(&site.files["404.html"]);
    assert!(!html.contains("data-rdy-kind"), "the authored page: {html}");
    assert!(html.contains("AUTHOREDCANARY words.") && html.contains("data-rdy-version"));
    // the warning comes once, last: after the diagram's
    let w = &site.summary.warnings;
    assert_eq!(w.iter().filter(|w| *w == KEPT).count(), 1, "{w:?}");
    assert_eq!(w.last().map(String::as_str), Some(KEPT), "{w:?}");
    assert!(
        w[..w.len() - 1].iter().any(|w| w.contains("Mermaid")),
        "{w:?}"
    );
    // rdy render writes the authored page and gives the same warning
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&st, &Scope::Canon, &out.path().join("site")).unwrap();
    assert_eq!(s.warnings, *w);
    let written = std::fs::read_to_string(out.path().join("site/404.html")).unwrap();
    assert_eq!(written, html);

    // a doc named 404 in a folder and a top-level folder named 404 own other pages: the site
    // keeps its generated page and gives no warning
    let ops = json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "404", "title": "Folder 404"}},
        {"op": "create", "type": "doc", "id": "$in", "parent": "$f", "fields": {"name": "404", "title": "Inner 404", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$is", "parent": "$in", "fields": {"title": "Inner"}},
        {"op": "create", "type": "prose", "parent": "$is", "body": "Inner words."}
    ]);
    let (_d, r, _ids) = project("Lost project", None, ops);
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    generated(&site.files);
    assert!(site.files.contains_key("404/index.html") && site.files.contains_key("404/404.html"));
    assert!(!site.summary.warnings.iter().any(|w| w == KEPT));
}

#[test]
fn dgm10_profile_404_uses_only_selected_identity() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "HIDDENCANARY section"}},
        {"op": "create", "type": "prose", "parent": "$hs", "body": "HIDDENCANARY prose."},
        {"op": "create", "type": "doc", "id": "$shown", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ss", "parent": "$shown", "fields": {"title": "Shown section"}},
        {"op": "create", "type": "prose", "parent": "$ss", "body": "Shown words."},
        {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
    ]);
    let (_d, mut r, mut ids) = project("SOURCECANARY project", None, ops);
    let p = put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": ids["$admin"],
            "fields": {"key": "shownorg", "title": "Shown site", "audience": "org", "select": [ids["$shown"]]}}]),
    );
    ids.extend(p);
    land(&mut r);
    let s = build_profile_site(&r.read(), "canon", "shownorg").unwrap();
    let html = generated(&s.files);
    assert!(
        html.contains("<title>Page not found · Shown site</title>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"brand-name\">Shown site</span>"),
        "{html}"
    );
    assert_eq!(
        html.matches("Shown site").count(),
        2,
        "only the title and the brand: {html}"
    );
    // the selected navigation
    assert!(
        html.contains("<a href=\"/shown.html\">Shown page</a>"),
        "{html}"
    );
    for word in [
        "HIDDENCANARY",
        "SOURCECANARY",
        "hidden.html",
        "admin.html",
        "Admin page",
    ] {
        assert!(!html.contains(word), "{word}: {html}");
    }
    for ph in ["$hid", "$hs", "$admin", "$p"] {
        assert!(!html.contains(&ids[ph]), "{ph}");
    }
    assert!(!s.summary.warnings.iter().any(|w| w == KEPT));
}

/// The project's captured inputs at `landing` and its publication of `selector`.
fn publication(r: &Rdy, landing: Hash, selector: &str) -> (RenderInputs, PublicationArtifact) {
    let read = r.read();
    let inputs = capture_inputs(&read, &Scope::At(landing)).unwrap();
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    let a = build_publication(&source, &m, TAG, false, selector, &inputs).unwrap();
    (inputs, a)
}

fn one_page(
    r: &Rdy,
    landing: Hash,
    selector: &str,
    path: &str,
) -> Result<rdy_render::RenderedPage, RdyError> {
    let read = r.read();
    let inputs = capture_inputs(&read, &Scope::At(landing)).unwrap();
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    render_publication_page(&source, &m, TAG, false, selector, &inputs, path)
}

/// Every link of every page of a publication is relative.
fn all_relative(files: &BTreeMap<String, Vec<u8>>) {
    for (path, bytes) in files.iter().filter(|(p, _)| p.ends_with(".html")) {
        for link in links(&text(bytes)) {
            assert!(!link.starts_with('/'), "{path}: {link}");
        }
    }
}

#[test]
fn dgm10_publications_have_no_generated_404() {
    let (dir, mut r, ids) = project("Lost project", Some("/docs/"), guide_ops());
    let profile = put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": ids["$other"],
            "fields": {"key": "guideorg", "title": "Guide site", "audience": "org", "select": [ids["$intro"]]}}]),
    );
    land(&mut r);
    // the local static render of the same project and profile has one
    generated(
        &build_site(&r.read(), &Scope::Canon, Mode::Static)
            .unwrap()
            .files,
    );
    generated(
        &build_profile_site(&r.read(), "canon", "guideorg")
            .unwrap()
            .files,
    );
    let landing = tag(&mut r);
    for selector in [IMPLICIT_ORG.to_string(), profile["$p"].clone()] {
        let (_, a) = publication(&r, landing, &selector);
        assert!(!a.files.contains_key("404.html"), "{selector}");
        assert!(!a.manifest.files.iter().any(|e| e.path == "404.html"));
        assert!(!a.warnings.iter().any(|w| w == KEPT), "{selector}");
        for bytes in a.files.values() {
            assert!(
                !String::from_utf8_lossy(bytes).contains("not-found"),
                "{selector}"
            );
        }
        all_relative(&a.files);
        let e = one_page(&r, landing, &selector, "404.html").unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            NOT_FOUND,
            "{selector}"
        );
        let page = one_page(&r, landing, &selector, "index.html").unwrap();
        all_relative(&BTreeMap::from([("index.html".to_string(), page.html)]));
    }
    // the base path is no input of a publication: its inputs are the same bytes without it
    let (with_base, _) = publication(&r, landing, IMPLICIT_ORG);
    let keys: Vec<String> = serde_json::to_value(&with_base)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        [
            "description",
            "disagreements",
            "pack",
            "rdy",
            "theme",
            "title"
        ]
    );
    let with_base = serde_json::to_string(&with_base).unwrap();
    assert!(
        !with_base.contains("/docs/"),
        "the inputs hold no base path"
    );
    drop(r);
    std::fs::write(dir.path().join(".rdy.yaml"), config("Lost project", None)).unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.read().site_base_path(), "/");
    let (inputs, _) = publication(&r, landing, IMPLICIT_ORG);
    assert_eq!(serde_json::to_string(&inputs).unwrap(), with_base);

    // an authored selected 404 is an ordinary page of the publication
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$lost", "fields": {"name": "404", "title": "Authored lost page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$lost", "fields": {"title": "Where to go"}},
        {"op": "create", "type": "prose", "parent": "$ls", "body": "AUTHOREDCANARY words."}
    ]);
    let (_d, mut r, _ids) = project("Lost project", None, ops);
    let landing = tag(&mut r);
    let (_, a) = publication(&r, landing, IMPLICIT_ORG);
    let html = text(&a.files["404.html"]);
    assert!(html.contains("AUTHOREDCANARY words.") && !html.contains("data-rdy-kind"));
    assert!(!a.warnings.iter().any(|w| w == KEPT), "{:?}", a.warnings);
    let page = one_page(&r, landing, IMPLICIT_ORG, "404.html").unwrap();
    assert!(page.html == a.files["404.html"]);
    all_relative(&a.files);
}

#[test]
fn dgm10_live_builds_have_no_collision_warning() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$lost", "fields": {"name": "404", "title": "Authored lost page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$lost", "fields": {"title": "Where to go"}},
        {"op": "create", "type": "prose", "parent": "$ls", "body": "AUTHOREDCANARY words."},
        {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$other", "fields": {"title": "Other section"}},
        {"op": "create", "type": "prose", "parent": "$os", "body": "Other words."}
    ]);
    let (_d, mut r, ids) = project("Lost project", None, ops);
    put(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": ids["$other"],
            "fields": {"key": "lostorg", "title": "Lost site", "audience": "org", "select": [ids["$lost"], ids["$other"]]}}]),
    );
    land(&mut r);
    let st = r.read();
    // the local static render warns once
    let stat = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    assert_eq!(
        stat.summary.warnings.iter().filter(|w| *w == KEPT).count(),
        1
    );
    let prof = build_profile_site(&st, "canon", "lostorg").unwrap();
    assert_eq!(
        prof.summary.warnings.iter().filter(|w| *w == KEPT).count(),
        1
    );
    // the live builds keep the authored page and never warn
    let authored = |what: &str, files: &BTreeMap<String, Vec<u8>>, warnings: &[String]| {
        assert!(!warnings.iter().any(|w| w == KEPT), "{what}: {warnings:?}");
        let html = text(&files[&"404.html".to_string()]);
        assert!(
            html.contains("AUTHOREDCANARY words.") && !html.contains("data-rdy-kind"),
            "{what}"
        );
    };
    let SiteFiles { files, summary, .. } =
        build_site(&st, &Scope::Canon, Mode::Live { root: "/" }).unwrap();
    authored("build_site live", &files, &summary.warnings);
    let b = build_live(&st, &Scope::Canon, &SiteTarget::Source(ScopeRef::Canon)).unwrap();
    authored("build_live", &b.files, &b.warnings);
    for target in [
        SiteTarget::ProfileOnly {
            key: "lostorg".into(),
        },
        SiteTarget::Profile {
            scope: ScopeRef::Canon,
            key: "lostorg".into(),
        },
    ] {
        let p = build_profile_live(&st, &target, &mut ProfileHistoryMemo::default()).unwrap();
        authored(&format!("{target:?}"), &p.live.files, &p.live.warnings);
    }
    // a live build of a project without an authored 404 has no generated one either
    let (_d, r, _ids) = project("Lost project", None, guide_ops());
    let st = r.read();
    let b = build_live(&st, &Scope::Canon, &SiteTarget::Source(ScopeRef::Canon)).unwrap();
    assert!(!b.files.contains_key("404.html"));
    assert!(!b.warnings.iter().any(|w| w == KEPT));
}
