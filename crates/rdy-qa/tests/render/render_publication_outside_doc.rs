//! ndh4 (D-P3YD5, D-0VCQ5): a profile site publishes a decision with no doc that governs a
//! selected block, so the trail of that block shows the decision. A public profile keeps its
//! default: no trail.
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::tiny;
use rdy_render::{SiteFiles, build_profile_site};
use serde_json::json;

/// A file of a site, as text.
fn page(s: &SiteFiles, path: &str) -> String {
    String::from_utf8(
        s.files
            .get(path)
            .unwrap_or_else(|| panic!("no file {path}: {:?}", s.files.keys()))
            .clone(),
    )
    .unwrap()
}

#[test]
fn ndh4_a_profile_site_trail_shows_a_governing_decision_with_no_doc() {
    let (_dir, mut r) = tiny();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Guide section"}},
            {"op": "create", "type": "decision", "id": "$dec",
             "fields": {"statement": "ROOTCANARY ruling.", "status": "proposed"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "Governed words.",
             "links": {"decided_by": [{"target": "$dec"}]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    let (guide, admin, dec) = (
        w.created["$guide"].clone(),
        w.created["$admin"].clone(),
        w.created["$dec"].clone(),
    );
    write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "trail": true, "select": [guide]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "pubsite", "title": "Public site", "audience": "public", "select": [guide]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the profiles: {e}"));
    land(&mut r, "w1").unwrap_or_else(|e| panic!("land: {e}"));
    let st = r.read();
    let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    assert_eq!(
        m.get(&rdy_model::BlockId::parse(&dec).unwrap())
            .unwrap()
            .parent,
        None
    );

    // the org profile with its trail on shows the decision on the governed claim
    let site = build_profile_site(&st, "canon", "site").unwrap_or_else(|e| panic!("site: {e}"));
    let g = page(&site, "guide.html");
    let trail = g.find("data-rdy=\"trail\"").expect("the trail");
    assert!(g[trail..].contains("ROOTCANARY ruling."), "{g}");

    // a public profile with no trail field: no trail markup on the page
    let public =
        build_profile_site(&st, "canon", "pubsite").unwrap_or_else(|e| panic!("pubsite: {e}"));
    let g = page(&public, "guide.html");
    assert!(!g.contains("data-rdy=\"trail\""), "{g}");
    assert!(g.contains("Governed words."), "{g}");
}
