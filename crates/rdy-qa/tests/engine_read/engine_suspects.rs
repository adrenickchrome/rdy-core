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

/// A project whose own pack has a presentational `aside` type (a section may hold one).
fn presentational_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/p.yaml"),
        rdy_qa::golden::PRESENTATIONAL_PACK,
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: p\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

#[test]
fn pres1_a_change_to_a_presentational_block_makes_nothing_suspect() {
    let (_d, mut r) = presentational_project();
    let res = write(&mut r, "work", json!([
        {"op":"create","type":"doc","id":"$d","fields":{"name":"t","title":"T","doc_type":"page"}},
        {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},
        {"op":"create","type":"aside","id":"$a","parent":"$s","body":"Decorative."},
        {"op":"create","type":"claim","id":"$c","parent":"$s","body":"Depends on {{$a}}.","links":{"depends_on":[{"target":"$a"}]}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op":"update","id":res.created["$a"],"body":"Still decorative."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    assert!(
        r.read().suspects(&Scope::Canon).unwrap().is_empty(),
        "{:?}",
        r.read().suspects(&Scope::Canon).unwrap()
    );

    let claim = rdy_model::BlockId::parse(&res.created["$c"]).unwrap();
    assert!(
        r.read()
            .context(&Scope::Canon, &claim)
            .unwrap()
            .suspects
            .is_empty()
    );

    // the control: the same edges from a prose block do make the claim suspect
    let res = write(&mut r, "work", json!([
        {"op":"create","type":"prose","id":"$p","parent":res.created["$s"],"body":"Plain."},
        {"op":"create","type":"claim","id":"$c2","parent":res.created["$s"],"body":"Depends on {{$p}}.","links":{"depends_on":[{"target":"$p"}]}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op":"update","id":res.created["$p"],"body":"Plainer."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let sus = r.read().suspects(&Scope::Canon).unwrap();
    assert_eq!(sus.len(), 1, "{sus:?}");
    assert_eq!(sus[0].block.to_string(), res.created["$c2"]);
    assert_eq!(sus[0].target.to_string(), res.created["$p"]);
}

#[test]
fn pres1_deleting_a_presentational_block_makes_nothing_suspect_but_a_deleted_prose_does() {
    let (_d, mut r) = presentational_project();
    let res = write(&mut r, "work", json!([
        {"op":"create","type":"doc","id":"$d","fields":{"name":"t","title":"T","doc_type":"page"}},
        {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},
        {"op":"create","type":"aside","id":"$a","parent":"$s","body":"Decorative."},
        {"op":"create","type":"prose","id":"$p","parent":"$s","body":"Plain."},
        {"op":"create","type":"claim","id":"$c","parent":"$s","body":"On the aside.","links":{"depends_on":[{"target":"$a"}]}},
        {"op":"create","type":"claim","id":"$c2","parent":"$s","body":"On the prose.","links":{"depends_on":[{"target":"$p"}]}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op":"delete","id":res.created["$a"]},{"op":"delete","id":res.created["$p"]}]),
    )
    .unwrap();
    let view = Scope::View("work".into());
    let sus = r.read().suspects(&view).unwrap();
    assert_eq!(sus.len(), 1, "only the prose's dependant: {sus:?}");
    assert_eq!(sus[0].block.to_string(), res.created["$c2"]);
    assert_eq!(sus[0].reason, "deleted");
    let claim = rdy_model::BlockId::parse(&res.created["$c"]).unwrap();
    assert!(r.read().context(&view, &claim).unwrap().suspects.is_empty());
}
