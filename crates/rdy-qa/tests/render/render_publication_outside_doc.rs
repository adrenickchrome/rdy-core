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

/// Ruling task-4 item 1: a pack lets a question hold a decision. A root question holds a
/// decision that a selected claim is `decided_by`. The decision has no doc, so the profile
/// publishes it, without the question: resolve, the check and the profile site all succeed.
#[test]
fn ndh4_a_decision_under_a_root_question_is_published_without_the_question() {
    let (dir, r) = tiny();
    // the store is locked while a handle is open
    drop(r);
    let pack = "name: p\nversion: '1'\nextends: rdy-standard\nblock_types:\n  question: { prefix: Q, children: [decision] }\n";
    std::fs::write(dir.path().join(".rdy/packs/p.yaml"), pack).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: p\nname: Nested\n").unwrap();
    let mut r = rdy_engine::Rdy::open(dir.path()).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Guide section"}},
            {"op": "create", "type": "question", "id": "$q", "fields": {"prompt": "QUESTIONCANARY which ledger?"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$q",
             "fields": {"statement": "NESTEDCANARY ruling.", "status": "proposed"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "Governed words.",
             "links": {"decided_by": [{"target": "$dec"}]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    let (guide, admin, q, dec) = (
        w.created["$guide"].clone(),
        w.created["$admin"].clone(),
        w.created["$q"].clone(),
        w.created["$dec"].clone(),
    );
    write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "trail": true, "select": [guide]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the profile: {e}"));
    land(&mut r, "w1").unwrap_or_else(|e| panic!("land: {e}"));
    let st = r.read();
    let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    let (qid, did) = (
        rdy_model::BlockId::parse(&q).unwrap(),
        rdy_model::BlockId::parse(&dec).unwrap(),
    );
    assert_eq!(m.get(&qid).unwrap().parent, None);
    assert_eq!(m.get(&did).unwrap().parent, Some(qid.clone()));

    let res = rdy_engine::publication::resolve(&st, &rdy_engine::Scope::Canon, "site")
        .unwrap_or_else(|e| panic!("resolve: {e}"));
    let ids: Vec<_> = res.entries.iter().map(|e| e.id.clone()).collect();
    assert!(ids.contains(&did), "{ids:?}");
    assert!(!ids.contains(&qid), "{ids:?}");

    let v = rdy_engine::publication::check(&st, "site", None, None)
        .unwrap_or_else(|e| panic!("check: {e}"));
    let entries = v["selection"]["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["id"] == json!(dec)), "{v}");
    assert!(!entries.iter().any(|e| e["id"] == json!(q)), "{v}");

    let site = build_profile_site(&st, "canon", "site").unwrap_or_else(|e| panic!("site: {e}"));
    let g = page(&site, "guide.html");
    let trail = g.find("data-rdy=\"trail\"").expect("the trail");
    assert!(g[trail..].contains("NESTEDCANARY ruling."), "{g}");
    for (path, bytes) in &site.files {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("QUESTIONCANARY"),
            "{path}: the question is not published"
        );
    }
}

/// The `trail-what` texts of the first trail of `page`, in order.
fn trail_texts(page: &str) -> Vec<String> {
    let at = page.find("data-rdy=\"trail\"").expect("the trail");
    let end = page[at..].find("</details>").map_or(page.len(), |e| at + e);
    page[at..end]
        .split("<span class=\"trail-what\">")
        .skip(1)
        .map(|s| s[..s.find("</span>").unwrap()].to_string())
        .collect()
}

/// Ruling final item 1: a root decision governs a selected claim and is superseded by another
/// root decision that governs nothing selected yet. The profile keeps the successor, so the
/// profile site's trail reads as the source site's: the ruling gave way, it is not current.
#[test]
fn ndh4_a_profile_site_trail_shows_a_superseded_root_decision_as_the_source_does() {
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
        rdy_model::BlockId::parse(&w.created["$dec"]).unwrap(),
    );
    write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "trail": true, "select": [guide]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the profile: {e}"));
    land(&mut r, "w1").unwrap_or_else(|e| panic!("land: {e}"));
    let next = rdy_qa::intake::supersede_in(
        &mut r,
        "w2",
        &dec,
        rdy_engine::DecisionInput {
            statement: "SUCCESSORCANARY ruling.".into(),
            ..Default::default()
        },
    );
    land(&mut r, "w2").unwrap_or_else(|e| panic!("land the supersession: {e}"));
    let st = r.read();
    let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    assert_eq!(m.get(&next).unwrap().parent, None);

    let source = rdy_render::build_site(&st, &rdy_engine::Scope::Canon, rdy_render::Mode::Static)
        .unwrap_or_else(|e| panic!("source site: {e}"));
    let site = build_profile_site(&st, "canon", "site").unwrap_or_else(|e| panic!("site: {e}"));
    let want = trail_texts(&page(&source, "guide.html"));
    assert!(
        want.iter()
            .any(|t| t == "Superseded: ROOTCANARY ruling. gave way to SUCCESSORCANARY ruling."),
        "{want:?}"
    );
    assert_eq!(trail_texts(&page(&site, "guide.html")), want);

    let res = rdy_engine::publication::resolve(&st, &rdy_engine::Scope::Canon, "site")
        .unwrap_or_else(|e| panic!("resolve: {e}"));
    let ids: Vec<_> = res.entries.iter().map(|e| e.id.clone()).collect();
    assert!(ids.contains(&next), "{ids:?}");
}

/// Ruling final item 2, the reviewer's probe: in a project with no decision home, a person
/// answers a root question (the decision sits at the root and `answers` it), an agent decides
/// with no parent and `depends_on` a root finding, and a selected claim is `decided_by` both. A
/// public profile keeps both targets, so its check is clear and its site builds.
#[test]
fn ndh4_a_public_profile_keeps_the_root_blocks_its_root_decisions_link() {
    let (_dir, mut r) = tiny();
    let w = write(
        &mut r,
        "qs",
        json!([
            {"op": "create", "type": "question", "id": "$q",
             "fields": {"prompt": "Who approves refunds?", "options": ["Finance", "Support lead"]}},
            {"op": "create", "type": "finding", "id": "$f",
             "fields": {"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days."}]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    land(&mut r, "qs").unwrap_or_else(|e| panic!("land: {e}"));
    let (q, f) = (w.created["$q"].clone(), w.created["$f"].clone());
    let m = r.read().materialized(&rdy_engine::Scope::Canon).unwrap();
    let qid = rdy_model::BlockId::parse(&q).unwrap();
    let version = m.version_of(&qid).unwrap().to_string();
    r.answer(
        &rdy_qa::fixtures::writer(),
        rdy_qa::intake::PERSON,
        rdy_engine::answer::AnswerRequest {
            r#ref: format!("{q}@{}", &version[..12]),
            option: "o1".into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("answer: {e}"));
    let answered = {
        let item = format!("{q}@{}", &version[..12]);
        let (_, a) = r.store().answer_of(&item).unwrap().unwrap();
        let change = r.store().reader().change(&a.targets[0]).unwrap();
        change.ops[0].block_id().to_string()
    };
    let decided = rdy_qa::intake::decide_in(
        &mut r,
        "rulings",
        rdy_engine::DecisionInput {
            statement: "Refunds keep seven days of records.".into(),
            depends_on: vec![f.clone()],
            ..Default::default()
        },
    );
    land(&mut r, "rulings").unwrap_or_else(|e| panic!("land the ruling: {e}"));
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Guide section"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "Governed words.",
             "links": {"decided_by": [{"target": answered}, {"target": decided.to_string()}]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the guide: {e}"));
    let (guide, admin) = (w.created["$guide"].clone(), w.created["$admin"].clone());
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "pubp", "title": "Public", "audience": "public", "select": [guide]}}
        ]),
    )
    .unwrap_or_else(|e| panic!("write the profile: {e}"));
    land(&mut r, "work").unwrap_or_else(|e| panic!("land the work: {e}"));
    let st = r.read();
    let m = st.materialized(&rdy_engine::Scope::Canon).unwrap();
    for id in [&answered, &decided.to_string(), &q, &f] {
        let b = m.get(&rdy_model::BlockId::parse(id).unwrap()).unwrap();
        assert_eq!(b.parent, None, "{id} sits at the root");
    }

    let v = rdy_engine::publication::check(&st, "pubp", None, None)
        .unwrap_or_else(|e| panic!("check: {e}"));
    assert_eq!(v["references"]["status"], json!("clear"), "{v}");
    let entries = v["selection"]["entries"].as_array().unwrap();
    for id in [&answered, &decided.to_string(), &q, &f] {
        assert!(entries.iter().any(|e| e["id"] == json!(id)), "{id}: {v}");
    }
    build_profile_site(&st, "canon", "pubp").unwrap_or_else(|e| panic!("pubp: {e}"));
}
