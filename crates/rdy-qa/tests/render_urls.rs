//! Slug URLs, go/ permalinks, assets/ and the project name (url1).
use rdy_engine::*;
use rdy_qa::golden::*;
use rdy_render::render_site;
use serde_json::json;

#[test]
fn url1_pages_mirror_the_tree_and_decisions_have_no_page() {
    let (_d, mut r, ids) = site_with_decision();
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert!(out.path().join("wallets/index.html").exists());
    assert!(out.path().join("wallets/transactions.html").exists());
    assert!(!out.path().join("_decision").exists() && !out.path().join("decisions").exists());
    assert!(out.path().join("assets").is_dir() && !out.path().join("_assets").exists());
    assert_eq!(
        s.redirects, 7,
        "doc, folder, section, term, claim, question, decision"
    );
    let _ = ids;
}

/// Review Focus 1.
#[test]
fn url1_a_go_page_exists_for_every_doc_folder_and_block_and_names_no_id() {
    let (_d, mut r, ids) = site_with_decision();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    for (id, target) in [
        (&ids.doc, "../wallets/transactions.html"),
        (&ids.folder, "../wallets/index.html"),
        (&ids.section, "../wallets/transactions.html#guards"),
        (&ids.term, "../wallets/transactions.html#ledger"),
        (&ids.decision, "../wallets/transactions.html#ledger"),
    ] {
        let p = out.path().join(format!("go/{id}.html"));
        let h = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("{}", p.display()));
        assert!(h.contains(&format!("content=\"0; url={target}\"")), "{h}");
        assert!(h.contains(&format!("href=\"{target}\"")));
        assert!(
            !h.replace(&format!("go/{id}"), "").contains(&id.to_string()),
            "id in text: {h}"
        );
        assert!(h.contains("data-rdy-kind=\"redirect\""));
    }
}

#[test]
fn url1_the_site_name_comes_from_config_then_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("my-design_docs");
    let r = Rdy::init(&proj, "rdy-standard").unwrap();
    assert_eq!(r.project_name(), "My design docs");
    drop(r);
    std::fs::write(
        proj.join(".rdy/config.yaml"),
        "pack: rdy-standard\nname: Wallets\ndescription: The wallet service.\n",
    )
    .unwrap();
    let mut r = Rdy::open(&proj).unwrap();
    assert_eq!(r.project_name(), "Wallets");
    assert_eq!(
        r.project_description().as_deref(),
        Some("The wallet service.")
    );
    small_site(&mut r);
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("<title>Wallets</title>") && h.contains("The wallet service."),
        "{h}"
    );
    assert!(!h.contains("· rdy"));
}

#[test]
fn url1_a_reference_to_a_decision_lands_on_the_text_it_governs() {
    let (_d, mut r, ids) = site_with_decision();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("wallets/transactions.html")).unwrap();
    // the claim's body references the decision and the term: both land on #ledger
    assert!(h.matches("href=\"#ledger\"").count() >= 2, "{h}");
    assert!(!h.contains(&format!("{}.html", ids.decision)) && !h.contains("class=\"badge"));
}

#[test]
fn url1_labels_never_fall_back_to_ids() {
    let (_d, mut r) = tiny();
    let doc = write(
        &mut r,
        "work",
        json!([
            {"op":"create","type":"doc","id":"$d","fields":{"title":"","name":"untitled-one","doc_type":"glossary"}},
            {"op":"create","type":"section","parent":"$d","fields":{"title":"Terms"}}
        ]),
    )
    .unwrap()
    .created["$d"]
        .clone();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(h.contains(">Untitled one<"), "{h}");
    assert!(
        !h.contains(&doc.to_string()),
        "the home page never mentions the id: {h}"
    );
}

#[test]
fn url1_glossary_and_questions_pages_exist_only_when_needed() {
    let (_d, mut r) = tiny();
    write(
        &mut r,
        "work",
        json!([
            {"op":"create","type":"doc","id":"$d","fields":{"title":"Plain","doc_type":"overview"}},
            {"op":"create","type":"section","parent":"$d","fields":{"title":"Summary"}},
            {"op":"create","type":"section","parent":"$d","fields":{"title":"Goals"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert!(
        !out.path().join("glossary.html").exists() && !out.path().join("questions.html").exists()
    );
    let (_d, mut r, _ids) = site_with_decision(); // has a term and a question
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let g = std::fs::read_to_string(out.path().join("glossary.html")).unwrap();
    assert!(
        g.contains("<dt id=\"ledger\">Ledger</dt>")
            && g.contains("defined in Transactions › Guards"),
        "{g}"
    );
    assert!(
        std::fs::read_to_string(out.path().join("questions.html"))
            .unwrap()
            .contains("Open question")
    );
}

/// A doc named like a collected page keeps its page: the collected page is not written over it.
#[test]
fn url1_a_doc_named_glossary_keeps_its_page() {
    let (_d, mut r) = tiny();
    write(
        &mut r,
        "work",
        json!([
            {"op":"create","type":"doc","id":"$d","fields":{"name":"glossary","title":"Our words","doc_type":"glossary"}},
            {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},
            {"op":"create","type":"term","parent":"$s","fields":{"name":"Ledger"},"body":"Balances."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    let s = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert_eq!(s.pages, 2, "the home page and the doc");
    let g = std::fs::read_to_string(out.path().join("glossary.html")).unwrap();
    assert!(g.contains("Our words") && !g.contains("defined in"), "{g}");
}

/// The builders every later `render_*` test uses build what their doc comments say.
#[test]
fn url1_the_golden_builders_build_what_they_say() {
    let name = |r: &Rdy| r.project_name();
    let (_d, r) = tiny();
    assert_eq!(name(&r), "Tiny");
    let (_d, mut r) = tiny_with_doc();
    assert_eq!(name(&r), "Tiny");
    let doc = first_doc_id(&mut r);
    assert_eq!(
        r.materialized(&Scope::Canon)
            .unwrap()
            .children(Some(&doc))
            .len(),
        2
    );
    let (_d, mut r) = tree_site();
    assert_eq!(name(&r), "Tree");
    let m = r.materialized(&Scope::Canon).unwrap();
    let top: Vec<String> = m
        .children(None)
        .iter()
        .map(|b| m.name_of(&b.id).unwrap())
        .collect();
    assert_eq!(top, ["intro", "a", "b"]);
    let (_d, mut r) = wide_folder(12);
    assert_eq!(name(&r), "Many");
    let m = r.materialized(&Scope::Canon).unwrap();
    let folder = m.children(None)[0].id.clone();
    assert_eq!(m.children(Some(&folder)).len(), 12);
    let (_d, mut r, ids) = every_type_site();
    assert_eq!(name(&r), "All");
    let canon = r.materialized(&Scope::Canon).unwrap();
    assert!(canon.get(&ids.diagram).is_some() && canon.get(&ids.decision).is_some());
    assert!(canon.get(&ids.dangling_prose).is_none());
    let view = r.materialized(&Scope::View("dangling".into())).unwrap();
    assert!(view.get(&ids.dangling_prose).is_some());
    let (_d, mut r, ids) = decided_site();
    assert_eq!(name(&r), "Ledgers");
    let canon = r.materialized(&Scope::Canon).unwrap();
    for id in [&ids.d0, &ids.d1, &ids.d2, &ids.d3, &ids.d3_revoker, &ids.q] {
        assert!(canon.get(id).is_some(), "{id}");
    }
    assert!(
        canon.get(&ids.d4).is_none(),
        "canon holds no proposed decision"
    );
    let view = r.materialized(&Scope::View("pending".into())).unwrap();
    assert!(view.get(&ids.d4).is_some());
}

/// Fix round 1: a block anchor never equals an id the page itself uses (`<main id="main">`,
/// the skip link, the search and nav chrome), so it is suffixed like any other clash.
#[test]
fn url1_anchors_never_collide_with_the_pages_own_element_ids() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op":"create","type":"doc","id":"$d","fields":{"title":"Chrome","doc_type":"overview"}},
        {"op":"create","type":"section","id":"$m","parent":"$d","fields":{"title":"Main"}},
        {"op":"create","type":"section","parent":"$d","fields":{"title":"Search"}},
        {"op":"create","type":"section","parent":"$d","fields":{"title":"Contents"}},
        {"op":"create","type":"section","parent":"$d","fields":{"title":"Versions"}},
        {"op":"create","type":"term","parent":"$m","fields":{"name":"Main"},"body":"The main thing."},
        {"op":"create","type":"question","parent":"$m","fields":{"prompt":"Main?"}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let doc = std::fs::read_to_string(out.path().join("chrome.html")).unwrap();
    for (title, anchor) in [
        ("Main", "main-2"),
        ("Search", "search-2"),
        ("Contents", "contents-2"),
        ("Versions", "versions-2"),
    ] {
        assert!(
            doc.contains(&format!("id=\"{anchor}\"")),
            "{title} -> {anchor}: {doc}"
        );
    }
    assert_eq!(
        doc.matches("id=\"main\"").count(),
        1,
        "only the page's own <main>: {doc}"
    );
    let g = std::fs::read_to_string(out.path().join("glossary.html")).unwrap();
    assert!(g.contains("<dt id=\"main-2\">Main</dt>"), "{g}");
    let q = std::fs::read_to_string(out.path().join("questions.html")).unwrap();
    assert!(
        q.contains("id=\"main-2\"") && q.matches("id=\"main\"").count() == 1,
        "{q}"
    );
}
