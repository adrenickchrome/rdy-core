//! bent1: briefings carry the entity work (FW-EN3, FW-EN4, FW-EN5): the blocks that mention a new
//! doc, graph coverage for what they cover, and related decisions for a covered decision.
use rdy_engine::brief::{Briefing, Coverage};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn brief(r: &Rdy, ids: &[&str], path: Option<&str>) -> Briefing {
    r.read()
        .brief(
            &Scope::View("work".into()),
            &Coverage {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                path: path.map(String::from),
            },
            None,
        )
        .unwrap()
}

fn json_of(b: &Briefing) -> Value {
    serde_json::to_value(b).unwrap()
}

#[test]
fn bent1_a_briefing_on_a_new_doc_lists_the_blocks_that_mention_it() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "claim", "id": "$m", "parent": "$o", "body": "Every entry is copied to the Archive nightly."},
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "archive", "title": "Archive", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$a", "body": "See {{$m}}."},
        {"op": "create", "type": "claim", "parent": "$a", "body": "Ops owns this."}
    ]))
    .unwrap();
    let v = json_of(&brief(&r, &[], Some("archive")));
    assert_eq!(
        v["mentioned_by"],
        json!([{"doc": w.created["$a"], "block": w.created["$m"], "field": "body", "mention": "Archive"}]),
        "{v}"
    );
    // ops is linked from the archive, so it is not new, though "Ops owns this." names it
    let v = json_of(&brief(&r, &[], Some("ops")));
    assert!(v.get("mentioned_by").is_none(), "{v}");
}

#[test]
fn bent1_a_briefing_carries_coverage_for_what_it_covers() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service", "aliases": ["ledger"]}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$o", "body": "The ledger is append-only.", "links": {"describes": [{"target": "$e"}]}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$o", "body": "The ledger closes daily."}
    ]))
    .unwrap();
    let b = brief(
        &r,
        &[w.created["$c1"].as_str(), w.created["$c2"].as_str()],
        None,
    );
    let c = &b.coverage;
    let g = &c.grounded;
    assert_eq!(
        (
            c.scope.as_str(),
            g.facts,
            g.facts_tied,
            g.content,
            g.content_tied,
            g.mentions,
            g.mentions_linked
        ),
        ("covered", 2, 1, 2, 1, 2, 1)
    );
    let v = json_of(&b);
    assert!(
        v["coverage"]["grounded"].is_object() && v.get("health").is_none(),
        "FW-CV1: {v}"
    );
}

#[test]
fn bent1_related_decisions_in_a_briefing_stay_within_the_cap() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service"}}
    ]))
    .unwrap();
    let (o, e) = (w.created["$o"].clone(), w.created["$e"].clone());
    let mut ds = vec![];
    for i in 0..22 {
        let d = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: o.clone(),
                    statement: format!("Rule {i}"),
                    affects: vec![e.clone()],
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        ds.push(d);
    }
    // each of the two covered decisions has 21 proposed decisions on the same entity: 20 each
    // after the per-decision cap (2 dropped), 40 in all, 20 shown (20 more dropped)
    let v = json_of(&brief(&r, &[ds[0].as_str(), ds[1].as_str()], None));
    assert_eq!(
        v["related_decisions"].as_array().map(Vec::len),
        Some(rdy_engine::related::RELATED_CAP),
        "{v}"
    );
    assert_eq!(v["related_decisions_omitted"], 22, "{v}");
}

#[test]
fn bent1_a_briefing_on_a_decision_lists_related_decisions() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$o", "body": "Plain."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let (o, e) = (w.created["$o"].clone(), w.created["$e"].clone());
    let a = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o.clone(),
                statement: "The ledger is append-only".into(),
                affects: vec![e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let b = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o,
                statement: "Entries are kept forever".into(),
                affects: vec![e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let v = json_of(&brief(&r, &[b.as_str()], None));
    assert_eq!(
        v["related_decisions"],
        json!([{"for": b, "decision": a, "status": "live", "statement": "The ledger is append-only", "relation": "same_entity", "entities": [e]}])
    );
    let v = json_of(&brief(&r, &[w.created["$c"].as_str()], None));
    assert!(v.get("related_decisions").is_none(), "{v}");
}
