//! ls12: one generated glossary, grouped by the top-level folder of each defining doc
//! (FW-GL1, D-Y4884).
use rdy_engine::{Rdy, Scope};
use rdy_render::{RenderSummary, render_site};
use serde_json::json;

const INTRO: &str = "<p class=\"lede\">Every term these docs define is listed here, grouped by the part of the docs that defines it. Each term links to the page that defines it.</p>";

/// `Tiny` with: folder `beta` ("Beta area", first in the tree) holding folder `inner` with doc
/// "Deep" (terms "Zeta" and "alpha"); folder `alpha` ("Alpha area") with doc "Mid" (term "Mid");
/// and the top-level doc "Loose" (term "Kappa"). Landed.
fn grouped() -> (tempfile::TempDir, Rdy) {
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, mut r) = rdy_qa::golden::tiny();
    rdy_qa::golden::write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$b", "fields": {"name": "beta", "title": "Beta area"}},
        {"op": "create", "type": "folder", "id": "$i", "parent": "$b", "fields": {"name": "inner"}},
        {"op": "create", "type": "doc", "id": "$deep", "parent": "$i", "fields": {"title": "Deep", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ds", "parent": "$deep", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "parent": "$ds", "fields": {"name": "Zeta"}, "body": "The last."},
        {"op": "create", "type": "term", "parent": "$ds", "fields": {"name": "alpha"}, "body": "The first."},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "alpha", "title": "Alpha area"}},
        {"op": "create", "type": "doc", "id": "$mid", "parent": "$a", "fields": {"title": "Mid", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ms", "parent": "$mid", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "parent": "$ms", "fields": {"name": "Mid"}, "body": "The middle."},
        {"op": "create", "type": "doc", "id": "$loose", "fields": {"title": "Loose", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$loose", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "parent": "$ls", "fields": {"name": "Kappa"}, "body": "Apart."}
    ])).unwrap();
    rdy_qa::golden::land(&mut r, "work").unwrap();
    (d, r)
}

/// The rendered canon, its glossary page and the render's summary.
fn glossary(r: &Rdy) -> (tempfile::TempDir, String, RenderSummary) {
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let g = std::fs::read_to_string(out.path().join("glossary.html")).unwrap();
    (out, g, s)
}

/// The group headings and the terms of `g`, in page order, as `h2 <label>` and `dt <name>`.
fn order(g: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"<(h2|dt) id="[^"]*">([^<]*)</(?:h2|dt)>"#).unwrap();
    re.captures_iter(g)
        .map(|c| format!("{} {}", &c[1], &c[2]))
        .collect()
}

#[test]
fn ls12_terms_group_by_top_level_folder_in_nav_order() {
    let (_d, r) = grouped();
    let (_o, g, _s) = glossary(&r);
    assert_eq!(
        order(&g),
        [
            "h2 Beta area",
            "dt alpha",
            "dt Zeta",
            "h2 Alpha area",
            "dt Mid",
            "h2 Tiny",
            "dt Kappa"
        ],
        "{g}"
    );
    assert!(g.contains(INTRO), "the fixed intro: {g}");
}

#[test]
fn ls12_on_this_page_names_the_groups() {
    let (_d, r) = grouped();
    let (_o, g, _s) = glossary(&r);
    let toc: Vec<(&str, &str)> = g
        .split("<a class=\"toc-link\" href=\"#")
        .skip(1)
        .map(|s| {
            let (anchor, rest) = s.split_once("\">").unwrap();
            (anchor, rest.split_once('<').unwrap().0)
        })
        .collect();
    let labels: Vec<&str> = toc.iter().map(|(_, l)| *l).collect();
    assert_eq!(labels, ["Beta area", "Alpha area", "Tiny"]);
    for (anchor, label) in toc {
        assert!(
            g.contains(&format!("<h2 id=\"{anchor}\">{label}</h2>")),
            "{anchor}"
        );
    }
}

#[test]
fn ls12_each_entry_links_to_its_defining_doc() {
    // guard: today's link rule holds through the regrouping
    let (_d, r) = grouped();
    let (_o, g, _s) = glossary(&r);
    for (term, page) in [
        ("Zeta", "beta/inner/deep.html#"),
        ("Mid", "alpha/mid.html#"),
        ("Kappa", "loose.html#"),
    ] {
        let dd = g
            .split_once(&format!(">{term}</dt>"))
            .unwrap()
            .1
            .split_once("</dd>")
            .unwrap()
            .0;
        assert!(
            dd.contains(&format!("class=\"defined-in\" href=\"{page}")),
            "{term}: {dd}"
        );
    }
}

/// The id of the first section of the top-level doc titled `title` in canon.
fn section_of(r: &Rdy, title: &str) -> String {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .children(None)
        .into_iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap();
    m.children(Some(&doc.id))[0].id.to_string()
}

#[test]
fn ls12_a_top_level_doc_named_glossary_gives_way() {
    let (_d, mut r) = grouped();
    let w = rdy_qa::golden::write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "id": "$h", "parent": "$s", "fields": {"name": "Hidden"}, "body": "Kept apart."}
    ])).unwrap();
    // fix round 1 (ruling task-12): another doc refers to the hidden term
    let hidden = w.created["$h"].clone();
    let words = section_of(&r, "Loose");
    let (uses, see) = (
        format!("Uses {{{{{hidden}}}}} a lot."),
        format!("Balances, see {{{{{hidden}}}}}."),
    );
    rdy_qa::golden::write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "parent": words, "body": uses},
        {"op": "create", "type": "term", "parent": words, "fields": {"name": "Ledger"}, "body": see}
    ])).unwrap();
    rdy_qa::golden::land(&mut r, "work").unwrap();
    let (out, g, s) = glossary(&r);
    assert!(
        g.contains(INTRO),
        "the generated page owns glossary.html: {g}"
    );
    assert!(!g.contains("Our words"), "{g}");
    assert_eq!(
        s.warnings,
        [
            "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it."
        ]
    );
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(!home.contains("Our words"), "no link to the doc: {home}");
    // a reference to a block that resolves but has no page reads as its label, unlinked
    let unlinked = |what: &str, text: &str| {
        assert!(!text.contains("unresolved"), "{what}: {text}");
        assert!(!text.contains(">Hidden</a>"), "{what}: {text}");
    };
    let loose = std::fs::read_to_string(out.path().join("loose.html")).unwrap();
    unlinked("loose.html", &loose);
    assert!(loose.contains(">Uses Hidden a lot.</"), "{loose}");
    let full = std::fs::read_to_string(out.path().join("llms-full.txt")).unwrap();
    unlinked("llms-full.txt", &full);
    assert!(full.contains("Uses Hidden a lot."), "{full}");
    assert!(full.contains("Balances, see Hidden."), "{full}");
    let index = std::fs::read_to_string(out.path().join("search-index.json")).unwrap();
    assert!(index.contains("Uses Hidden a lot."), "{index}");
    assert!(index.contains("Balances, see Hidden."), "{index}");
    // the glossary lists the hidden doc's term in the project's group, with no link
    unlinked("glossary.html", &g);
    assert_eq!(
        order(&g),
        [
            "h2 Beta area",
            "dt alpha",
            "dt Zeta",
            "h2 Alpha area",
            "dt Mid",
            "h2 Tiny",
            "dt Hidden",
            "dt Kappa",
            "dt Ledger"
        ],
        "{g}"
    );
    let dd = |term: &str| {
        g.split_once(&format!(">{term}</dt>"))
            .unwrap()
            .1
            .split_once("</dd>")
            .unwrap()
            .0
            .to_string()
    };
    assert_eq!(dd("Hidden"), "<dd>Kept apart.", "no link: {g}");
    assert!(
        dd("Ledger").starts_with("<dd>Balances, see Hidden. <a class=\"defined-in\""),
        "{g}"
    );
}

#[test]
fn ls12_a_top_level_doc_named_glossary_in_any_case_gives_way() {
    // fix round 1: names are case-insensitive, and so is a case-insensitive file system
    let (_d, mut r) = grouped();
    rdy_qa::golden::write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "Glossary", "title": "Our words", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Hidden"}, "body": "Kept apart."}
    ])).unwrap();
    rdy_qa::golden::land(&mut r, "work").unwrap();
    let (out, g, s) = glossary(&r);
    assert_eq!(
        s.warnings,
        [
            "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it."
        ]
    );
    let pages: Vec<String> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("glossary.html"))
        .collect();
    assert_eq!(pages, ["glossary.html"], "the doc builds no page");
    assert!(
        g.contains(INTRO),
        "the generated page owns glossary.html: {g}"
    );
    assert!(!g.contains("Our words"), "{g}");
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(!home.contains("Our words"), "no link to the doc: {home}");
}

#[test]
fn ls12_rdy_standard_has_no_glossary_doc_type() {
    let (_d, mut r) = rdy_qa::golden::tiny();
    let e = rdy_qa::golden::write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Words", "doc_type": "glossary"}}
        ]),
    )
    .unwrap_err();
    assert!(
        format!("{e:?}").contains("unknown doc_type \\\"glossary\\\""),
        "{e:?}"
    );
}

#[test]
fn ls12_the_grouped_glossary_passes_the_leak_scan() {
    // guard
    let (_d, r) = grouped();
    let (out, _g, _s) = glossary(&r);
    assert!(rdy_qa::site::leaks(out.path()).is_empty());
}
