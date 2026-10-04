//! Which edges impact traverses (imp2).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn imp2_impact_follows_dependencies_and_shows_refiners_once() {
    let (_d, mut r) = project();
    let dec = |id: &str, statement: &str, links: serde_json::Value| {
        json!({"op": "create", "type": "decision", "id": id, "parent": "$s",
               "fields": {"statement": statement, "status": "proposed"}, "links": links})
    };
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Refunds", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Refunds go through the ledger."},
        dec("$d1", "Approvers sign refunds", json!({"depends_on": [{"target": "$p"}]})),
        dec("$d2", "Approvers sign refunds over 100", json!({"refines": [{"target": "$d1"}]})),
        dec("$d3", "Support may sign small refunds", json!({"depends_on": [{"target": "$d2"}]})),
        dec("$d4", "Two approvers sign refunds", json!({"supersedes": [{"target": "$d1"}]})),
        {"op": "create", "type": "note", "id": "$n", "parent": "$s", "body": "See {{$d1}}."}
    ]))
    .unwrap();
    let id = |k: &str| res.created[k].clone();
    let scope = Scope::View("work".into());
    let got = |r: &mut Rdy, k: &str| -> Vec<(u32, String, String)> {
        r.impact(&scope, &BlockId::parse(&id(k)).unwrap(), 5)
            .unwrap()
            .into_iter()
            .map(|i| (i.depth, i.id, i.via.kind))
            .collect()
    };
    // the premise reaches the decision that depends on it and that decision's dependents, but not
    // the decisions that only refine or supersede it
    let mut want = vec![
        (1, id("$d1"), "depends_on".to_string()),
        (2, id("$n"), "references".to_string()),
    ];
    want.sort();
    let mut have = got(&mut r, "$p");
    have.sort();
    assert_eq!(have, want);
    // the decision itself lists its refiner and superseder at depth 1, and doesn't follow them
    let have = got(&mut r, "$d1");
    assert!(
        have.contains(&(1, id("$d2"), "refines".to_string())),
        "{have:?}"
    );
    assert!(
        have.contains(&(1, id("$d4"), "supersedes".to_string())),
        "{have:?}"
    );
    assert!(
        have.contains(&(1, id("$n"), "references".to_string())),
        "{have:?}"
    );
    assert!(
        !have.iter().any(|(_, b, _)| *b == id("$d3")),
        "refines is not followed: {have:?}"
    );
}
