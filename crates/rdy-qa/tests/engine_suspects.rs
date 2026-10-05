//! Which edges raise suspects (sus2): references and premises; superseded targets through any link.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn setup(r: &mut Rdy) -> serde_json::Value {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Checkout", "doc_type": "flows"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Flows"}},
        {"op": "create", "type": "flow", "id": "$f", "parent": "$s", "fields": {"name": "Pay", "steps": ["Open wallet", "Pay"]}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Per {{$f}}."},
        {"op": "create", "type": "decision", "id": "$aff", "parent": "$s",
         "fields": {"statement": "Pay needs a wallet", "status": "proposed"}, "links": {"affects": [{"target": "$f"}]}},
        {"op": "create", "type": "decision", "id": "$dep", "parent": "$s",
         "fields": {"statement": "Balances are in pence", "status": "proposed"}, "links": {"depends_on": [{"target": "$t"}]}}
    ]))
    .unwrap();
    serde_json::to_value(&res.created).unwrap()
}

fn suspects_of(r: &mut Rdy, scope: &Scope, block: &str) -> Vec<(String, String)> {
    r.read()
        .suspects(scope)
        .unwrap()
        .into_iter()
        .filter(|s| s.block.to_string() == block)
        .map(|s| (s.target.to_string(), s.reason))
        .collect()
}

#[test]
fn sus2_editing_what_a_decision_affects_does_not_make_it_suspect() {
    let (_d, mut r) = project();
    let ids = setup(&mut r);
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let scope = Scope::View("work".into());
    write(&mut r, "work", json!([{"op": "update", "id": id("$f"), "fields": {"steps": ["Open wallet", "Confirm", "Pay"]}}])).unwrap();
    assert_eq!(
        suspects_of(&mut r, &scope, &id("$c")),
        vec![(id("$f"), "changed".to_string())],
        "a reference still raises one"
    );
    assert!(
        suspects_of(&mut r, &scope, &id("$aff")).is_empty(),
        "affects is not a premise"
    );
}

#[test]
fn sus2_premises_still_make_suspects() {
    let (_d, mut r) = project();
    let ids = setup(&mut r);
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let scope = Scope::View("work".into());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$t"), "body": "Balances in pence."}]),
    )
    .unwrap();
    assert_eq!(
        suspects_of(&mut r, &scope, &id("$dep")),
        vec![(id("$t"), "changed".to_string())]
    );
}

/// A guard (passes before and after): a superseded target is suspect through any link.
#[test]
fn sus2_a_superseded_target_is_suspect_through_any_link() {
    let (_d, mut r) = project();
    let ids = setup(&mut r);
    let s = ids["$s"].as_str().unwrap().to_string();
    let d1 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Refunds in a day".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let x = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Support follows refunds".into(),
                affects: vec![d1.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    r.supersede(
        &writer(),
        "work",
        &d1,
        None,
        DecisionInput {
            parent: s.clone(),
            statement: "Refunds in an hour".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    assert_eq!(
        suspects_of(&mut r, &Scope::Canon, &x),
        vec![(d1, "superseded".to_string())]
    );
}
