//! arn1: a block is in the areas its doc's path matches; a decision also in its affected blocks'
//! areas; impact groups by area. pkfix1: an invalid pack file says to fix the file.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use serde_json::json;

fn work() -> Scope {
    Scope::View("work".into())
}
fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
/// A doc at `path` with one section and one claim, in `work`; returns (doc, section, claim).
fn doc_at(r: &mut Rdy, path: &str) -> (String, String, String) {
    r.new_doc(&writer(), "work", path, Some("glossary"), Some("T"))
        .unwrap();
    let doc = r.read().resolve_path(&work(), path).unwrap().to_string();
    let res = write(r, "work", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "x"}
    ]))
    .unwrap();
    (doc, res.created["$s"].clone(), res.created["$c"].clone())
}

#[test]
fn arn1_blocks_take_their_docs_areas_and_decisions_their_affected_blocks_areas() {
    let (_d, mut r) = qa_project();
    let (pdoc, _ps, pclaim) = doc_at(&mut r, "payments/fees");
    let (_idoc, isec, iclaim) = doc_at(&mut r, "identity/login");
    let st = r.read();
    assert_eq!(
        st.areas_of(&work(), &id(&pclaim)).unwrap(),
        vec!["payments".to_string()]
    );
    assert_eq!(
        st.areas_of(&work(), &id(&pdoc)).unwrap(),
        vec!["payments".to_string()]
    );
    assert_eq!(
        st.areas_of(&work(), &id(&iclaim)).unwrap(),
        vec!["identity".to_string()]
    );
    let payments_folder = st.resolve_path(&work(), "payments").unwrap();
    assert_eq!(
        st.areas_of(&work(), &payments_folder).unwrap(),
        vec!["payments".to_string()],
        "'payments/**' covers the folder itself (M2s glob1)"
    );
    drop(st);
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: isec.clone(),
                statement: "Fees apply at login".into(),
                affects: vec![pclaim.clone()],
                ..Default::default()
            },
        )
        .unwrap();
    let dec = id(&d.created["$decision"]);
    let st = r.read();
    assert_eq!(
        st.areas_of(&work(), &dec).unwrap(),
        vec!["identity".to_string(), "payments".to_string()]
    );
    assert_eq!(
        st.context(&work(), &dec).unwrap().areas,
        vec!["identity".to_string(), "payments".to_string()]
    );
}

#[test]
fn arn1_impact_items_carry_areas_and_group_by_area() {
    let (_d, mut r) = qa_project();
    let (_pd, _ps, pclaim) = doc_at(&mut r, "payments/fees");
    let (_id, isec, _ic) = doc_at(&mut r, "identity/login");
    write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": isec, "body": format!("See {{{{{pclaim}}}}}.")}])).unwrap();
    let items = r.read().impact(&work(), &id(&pclaim), 3).unwrap();
    let hit = items
        .iter()
        .find(|i| i.block_type == "claim")
        .expect("the citing claim");
    assert_eq!(hit.areas, vec!["identity".to_string()]);
    assert_eq!(by_area(&items).get("identity"), Some(&1));
    let v = serde_json::to_value(&items).unwrap();
    // a project without areas keeps the old item shape
    let (_d2, mut r2) = project();
    let (_doc, _s, c) = seed_doc(&mut r2, "work");
    let items2 = r2.read().impact(&work(), &id(&c), 3).unwrap();
    assert!(
        serde_json::to_value(&items2)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("areas").is_none())
    );
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .any(|i| i.get("areas").is_some())
    );
}

#[test]
fn pkfix1_an_invalid_pack_file_says_fix_the_file_and_an_unknown_pack_says_set_the_config() {
    let (d, r) = project();
    drop(r);
    std::fs::write(
        d.path().join(".rdy/packs/bad.yaml"),
        "name: bad\nversion: \"1\"\nextends: rdy-standard\nareas:\n  Pay:\n    paths: [x]\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy.yaml"), "pack: bad\n").unwrap();
    let e = Rdy::open(d.path()).err().expect("the pack is refused");
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(e.fix, "fix the pack file");
    std::fs::write(d.path().join(".rdy.yaml"), "pack: nowhere\n").unwrap();
    let e = Rdy::open(d.path()).err().expect("the pack is refused");
    assert!(e.fix.contains("set `pack:` in .rdy.yaml"), "{}", e.fix);
}

#[test]
fn arn1_a_decision_without_affects_is_in_its_own_docs_area() {
    let (_d, mut r) = qa_project();
    let (_pdoc, psec, _pclaim) = doc_at(&mut r, "payments/fees");
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: psec,
                statement: "Fees are flat".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = id(&d.created["$decision"]);
    let st = r.read();
    assert_eq!(
        st.areas_of(&work(), &dec).unwrap(),
        vec!["payments".to_string()],
        "Q4 (a): its own doc's area"
    );
    assert_eq!(
        st.context(&work(), &dec).unwrap().areas,
        vec!["payments".to_string()]
    );
}

#[test]
fn arn1_a_decision_in_a_doc_without_areas_takes_only_its_targets_areas() {
    let (_d, mut r) = qa_project();
    let (_pdoc, _psec, pclaim) = doc_at(&mut r, "payments/fees");
    let (_odoc, osec, _oclaim) = doc_at(&mut r, "ops/runbook");
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: osec,
                statement: "Runbook covers fee refunds".into(),
                affects: vec![pclaim],
                ..Default::default()
            },
        )
        .unwrap();
    let dec = id(&d.created["$decision"]);
    assert_eq!(
        r.read().areas_of(&work(), &dec).unwrap(),
        vec!["payments".to_string()],
        "Q4 (c) only: ops/runbook is in no area"
    );
}
