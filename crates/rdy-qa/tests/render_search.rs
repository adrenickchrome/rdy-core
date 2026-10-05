//! The search index (srch1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn srch1_the_index_lists_docs_sections_terms_questions_and_folders_in_tree_order() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let j: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("search-index.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(j["version"], 1);
    assert_eq!(j["site"], "Tree");
    let e = j["entries"].as_array().unwrap();
    let kinds: Vec<&str> = e.iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert_eq!(
        &kinds[..3],
        ["doc", "section", "folder"],
        "intro, its section, then Alpha: {kinds:?}"
    );
    let term = e.iter().find(|x| x["kind"] == "term").unwrap();
    assert_eq!(term["title"], "Ledger");
    assert_eq!(term["page"], "a/one.html");
    assert_eq!(term["anchor"], "ledger");
    assert_eq!(term["doc"], "One");
    assert!(term["text"].as_str().unwrap().starts_with("Balances"));
    let q = e.iter().find(|x| x["kind"] == "question").unwrap();
    assert_eq!(q["page"], "questions.html");
    assert!(
        e.iter()
            .all(|x| x["text"].as_str().unwrap().chars().count() <= 400)
    );
}

/// Review Focus 5: hostile text in the index.
#[test]
fn srch1_hostile_text_survives_the_index_as_valid_json() {
    let (_d, mut r) = hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let text = std::fs::read_to_string(out.path().join("search-index.json")).unwrap();
    let j: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    let all = j["entries"].to_string();
    assert!(
        all.contains("<script>alert(1)</script>"),
        "the raw text is data in JSON, inserted by the client with textContent: {all}"
    );
}

#[test]
fn srch1_the_page_carries_the_search_box_and_the_client_handles_a_missing_index() {
    let (_d, mut r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("<input id=\"search\" type=\"search\"")
            && h.contains("data-index=\"search-index.json\"")
    );
    let js = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    assert!(js.contains("Search is not available on this copy of the site."));
    assert!(js.contains("key === \"/\""));
}
