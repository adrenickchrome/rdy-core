//! The gate: landing twice (land1); a re-created folder against other views' folders (gate1, guard).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn doc_ops(title: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}])
}

#[test]
fn land1_approving_a_landed_proposal_again_is_refused_with_its_landing() {
    let (_d, mut r) = project();
    write(&mut r, "work", doc_ops("One")).unwrap();
    let p = r.propose(&writer(), "work", "first").unwrap();
    let landing = r.land(&approver(), &p.id).unwrap();
    let before = r.store().landings().unwrap().len();
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_LANDED", "{e}");
    assert!(e.message.contains(&landing), "{e}");
    assert_eq!(
        r.store().landings().unwrap().len(),
        before,
        "nothing written"
    );
}

#[test]
fn land1_a_landed_proposal_with_decisions_is_refused_the_same_way() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: res.created["$s"].clone(),
            statement: "Ship".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "work", "with a decision").unwrap();
    let landing = r.land(&approver(), &p.id).unwrap();
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_LANDED", "not E_STALE_BASE: {e}");
    assert!(e.message.contains(&landing), "{e}");
}

/// Review Focus 3.
#[test]
fn land1_an_older_proposal_whose_changes_landed_in_a_newer_one_is_refused() {
    let (_d, mut r) = project();
    write(&mut r, "work", doc_ops("One")).unwrap();
    let older = r.propose(&writer(), "work", "older").unwrap();
    write(&mut r, "work", doc_ops("Two")).unwrap();
    let newer = r.propose(&writer(), "work", "newer").unwrap();
    let landing = r.land(&approver(), &newer.id).unwrap();
    let e = r.land(&approver(), &older.id).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_LANDED", "{e}");
    assert!(e.message.contains(&landing), "{e}");
    check(&r, &Scope::Canon).unwrap();
}

/// gate1 guard (passes before any change): a re-created block keeps its type (its id prefix), and
/// a re-created folder's depth is checked with the folders other views put under it.
#[test]
fn gate1_a_recreated_folder_is_checked_with_the_folders_other_views_added_under_it() {
    let (_d, mut r) = project();
    let seed = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "moving"}},
            {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "home"}}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (f, g) = (seed.created["$f"].clone(), seed.created["$g"].clone());
    // view x nests 31 folders under f: 32 deep counting f, the most allowed
    let mut ops = vec![];
    let mut parent = f.clone();
    for i in 0..31 {
        let id = format!("$n{i}");
        ops.push(json!({"op": "create", "type": "folder", "id": id, "parent": parent, "fields": {"name": format!("n{i}")}}));
        parent = id;
    }
    write(&mut r, "x", json!(ops)).unwrap();
    // view y, from the same canon, deletes f and re-creates it (same id) under g
    write(&mut r, "y", json!([{"op": "delete", "id": f}])).unwrap();
    write(&mut r, "y", json!([{"op": "create", "type": "folder", "id": f, "parent": g, "fields": {"name": "moving"}}])).unwrap();
    land(&mut r, "x").unwrap();
    let e = land(&mut r, "y").unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(e.issues.iter().any(|i| i.code == "E_TOO_DEEP"), "{e:?}");
    check(&r, &Scope::Canon).unwrap();
}

#[test]
fn msw2_the_gate_refuses_a_cross_class_id_prefix() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    // ra- is rationale's prefix (a supporting type): no claim can carry it
    raw_commit(
        &mut r,
        "x",
        vec![raw("ra-AAAAA", "claim", Some(&section), "zz", &[], "x")],
    );
    let e = land(&mut r, "x").unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_PREFIX"),
        "{:?}",
        e.issues
    );
    let id = rdy_model::BlockId::parse("ra-AAAAA").unwrap();
    assert!(
        r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .get(&id)
            .is_none()
    );
}
