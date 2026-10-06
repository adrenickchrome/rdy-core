//! The search index (srch1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn srch1_the_index_lists_docs_sections_terms_questions_and_folders_in_tree_order() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    let (_d, r) = hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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
    let (_d, r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("<input id=\"search\" type=\"search\"")
            && h.contains("data-index=\"search-index.json\"")
    );
    let js = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    assert!(js.contains("Search is not available on this copy of the site."));
    assert!(js.contains("key === \"/\""));
}

/// FW-D5 refined: what a page hides (a rationale, FW-D10) is in neither the search index nor
/// `llms.txt`.
#[test]
fn people1_search_and_llms_carry_no_text_the_page_hides() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Why", "name": "why", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "Kept on the page, directly."},
        {"op": "create", "type": "rationale", "parent": "$d", "body": "Hidden reasons stay off."},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Kept on the page."},
        {"op": "create", "type": "rationale", "parent": "$s", "body": "Hidden reasons stay off."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("why.html")).unwrap();
    assert!(page.contains("Kept on the page.") && !page.contains("Hidden reasons"));
    for file in ["search-index.json", "llms.txt"] {
        let text = std::fs::read_to_string(out.path().join(file)).unwrap();
        assert!(!text.contains("Hidden reasons"), "{file}: {text}");
    }
    let idx = std::fs::read_to_string(out.path().join("search-index.json")).unwrap();
    assert!(idx.contains("Kept on the page."), "{idx}");
}
