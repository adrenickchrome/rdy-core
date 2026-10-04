//! Supersession carries the old decision's scope and premises (sup1).
use rdy_engine::*;
use rdy_model::{Block, BlockId};
use rdy_qa::fixtures::*;
use serde_json::json;

fn targets(b: &Block, kind: &str) -> Vec<String> {
    b.links
        .get(kind)
        .into_iter()
        .flatten()
        .map(|l| l.target.to_string())
        .collect()
}

/// A landed decision D1 with affects, depends_on and refines links; returns (section, ids, d1).
fn setup(r: &mut Rdy) -> (String, serde_json::Value, String) {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Refunds", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Refund"}, "body": "Money back."},
        {"op": "create", "type": "term", "id": "$t2", "parent": "$s", "fields": {"name": "Chargeback"}, "body": "Bank reversal."},
        {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "Refunds go through the ledger."},
        {"op": "create", "type": "decision", "id": "$d0", "parent": "$s", "fields": {"statement": "Refunds exist", "status": "proposed"}}
    ]))
    .unwrap();
    let ids = serde_json::to_value(&res.created).unwrap();
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let d1 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: id("$s"),
                statement: "Approvers sign refunds".into(),
                affects: vec![id("$t")],
                depends_on: vec![id("$p")],
                refines: vec![id("$d0")],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "work").unwrap();
    (id("$s"), ids, d1)
}

#[test]
fn sup1_supersede_inherits_the_old_decisions_links() {
    let (_d, mut r) = project();
    let (s, ids, d1) = setup(&mut r);
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let res = r
        .supersede(
            &writer(),
            "work",
            &d1,
            None,
            DecisionInput {
                parent: s.clone(),
                statement: "Two approvers sign refunds".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let d2 = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["$decision"]).unwrap(),
        )
        .unwrap();
    assert_eq!(targets(&d2, "affects"), vec![id("$t")]);
    assert_eq!(targets(&d2, "depends_on"), vec![id("$p")]);
    assert_eq!(targets(&d2, "refines"), vec![id("$d0")]);
    assert_eq!(targets(&d2, "supersedes"), vec![d1.clone()]);
    assert_eq!(
        serde_json::to_value(&res.inherited).unwrap(),
        json!({
            "affects": [id("$t")], "depends_on": [id("$p")], "refines": [id("$d0")]
        })
    );
    // after landing, the term's governing decision is the live one
    land(&mut r, "work").unwrap();
    let ctx = r
        .context(&Scope::Canon, &BlockId::parse(&id("$t")).unwrap())
        .unwrap();
    assert_eq!(ctx.governing, vec![d2.id.to_string()]);
}

#[test]
fn sup1_the_caller_overrides_a_kind_or_drops_it() {
    let (_d, mut r) = project();
    let (s, ids, d1) = setup(&mut r);
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let res = r
        .supersede(
            &writer(),
            "work",
            &d1,
            None,
            DecisionInput {
                parent: s.clone(),
                statement: "Chargebacks only".into(),
                affects: vec![id("$t2")],
                no_inherit: vec!["refines".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let d2 = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["$decision"]).unwrap(),
        )
        .unwrap();
    assert_eq!(
        targets(&d2, "affects"),
        vec![id("$t2")],
        "the caller's kind wins"
    );
    assert!(targets(&d2, "refines").is_empty(), "no_inherit drops it");
    assert_eq!(targets(&d2, "depends_on"), vec![id("$p")]);
    assert_eq!(
        serde_json::to_value(&res.inherited).unwrap(),
        json!({"depends_on": [id("$p")]})
    );
}

#[test]
fn sup1_mcp_supersede_drops_a_kind_given_as_an_empty_list() {
    let (_d, mut r) = project();
    let (s, _ids, d1) = setup(&mut r);
    let mut st = rdy_mcp::tools::McpState::new(r);
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    let out = rdy_mcp::tools::rdy_supersede(
        &mut st,
        serde_json::from_value(json!({
            "old": d1, "decision": {"parent": s, "statement": "No scope", "affects": []}
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(out["inherited"].get("affects").is_none(), "{out}");
    assert!(out["inherited"]["depends_on"].is_array(), "{out}");
}
