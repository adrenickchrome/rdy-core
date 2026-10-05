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

fn supersede_input(s: &str) -> DecisionInput {
    DecisionInput {
        parent: s.to_string(),
        statement: "Newer".into(),
        ..Default::default()
    }
}

/// A kind name rdy_supersede does not know is refused, not ignored: a typo would still inherit.
#[test]
fn sup1_an_unknown_no_inherit_kind_is_refused() {
    let (_d, mut r) = project();
    let (s, _ids, d1) = setup(&mut r);
    let before = r.store().view("work").unwrap().changes.len();
    let mut d = supersede_input(&s);
    d.no_inherit = vec!["affect".into()];
    let e = r.supersede(&writer(), "work", &d1, None, d).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("affect"), "{e}");
    for kind in ["affects", "depends_on", "conditional_on", "refines"] {
        assert!(e.fix.contains(kind), "the fix lists {kind}: {e}");
    }
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
}

/// An inherited link whose target was deleted in the view is refused up front, naming the old
/// decision, the kind and the target, instead of a generic failure about a decision never made.
#[test]
fn sup1_an_inherited_link_to_a_deleted_target_is_refused_before_writing() {
    let (_d, mut r) = project();
    let (s, ids, d1) = setup(&mut r);
    let t = ids["$t"].as_str().unwrap().to_string();
    write(&mut r, "work", json!([{"op": "delete", "id": t}])).unwrap();
    let before = r.store().view("work").unwrap().changes.len();
    let e = r
        .supersede(&writer(), "work", &d1, None, supersede_input(&s))
        .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e}");
    assert!(
        e.message
            .contains(&format!("{d1}'s affects link to {t} no longer exists")),
        "{e}"
    );
    assert!(e.message.contains("can't be inherited"), "{e}");
    assert!(e.fix.contains("no_inherit"), "{e}");
    assert!(e.fix.contains("\"affects\": []"), "{e}");
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
    // giving the kind, or dropping it, gets past it
    let mut d = supersede_input(&s);
    d.no_inherit = vec!["affects".into()];
    r.supersede(&writer(), "work", &d1, None, d).unwrap();
}

/// An inherited link keeps its `part`: dropping it would widen the new decision's scope to the
/// whole target.
#[test]
fn sup1_inherited_links_keep_their_part() {
    let (_d, mut r) = project();
    let (s, ids, _d1) = setup(&mut r);
    let t = ids["$t"].as_str().unwrap().to_string();
    let d1 = write(
        &mut r,
        "work",
        json!([{
            "op": "create", "type": "decision", "parent": s,
            "fields": {"statement": "Money back is a refund", "status": "proposed"},
            "links": {"affects": [{"target": t, "part": "Money back"}]}
        }]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    land(&mut r, "work").unwrap();
    let res = r
        .supersede(&writer(), "work", &d1, None, supersede_input(&s))
        .unwrap();
    let d2 = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["$decision"]).unwrap(),
        )
        .unwrap();
    let l = &d2.links["affects"];
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].target.to_string(), t);
    assert_eq!(l[0].part.as_deref(), Some("Money back"));
    assert_eq!(
        serde_json::to_value(&res.inherited).unwrap(),
        json!({"affects": [t]})
    );
}
