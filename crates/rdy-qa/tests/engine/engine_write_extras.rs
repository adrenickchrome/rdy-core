//! rdec1: a write that records a decision lists its related decisions beside the consequences
//! (FW-EN5); other writes carry no such key.
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn rdec1_a_write_that_records_a_decision_lists_related_decisions() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$o", "fields": {"name": "Ledger service"}}
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
    let eng = Local::new(r);
    let (o2, e2) = (o.clone(), e.clone());
    let v = write_and_report(&eng, "work", None, move |r| {
        r.decide(
            &writer(),
            "work",
            DecisionInput {
                parent: o2,
                statement: "Entries are kept forever".into(),
                affects: vec![e2],
                ..Default::default()
            },
        )
    })
    .unwrap();
    let rel = &v["related_decisions"];
    assert_eq!(
        (rel[0]["decision"].as_str(), rel[0]["relation"].as_str()),
        (Some(a.as_str()), Some("same_entity")),
        "{v}"
    );
    assert_eq!(rel[0]["entities"], json!([e]));
    let v = write_and_report(&eng, "work", None, move |r| {
        write(
            r,
            "work",
            json!([{"op": "create", "type": "claim", "parent": o, "body": "Plain."}]),
        )
    })
    .unwrap();
    assert!(v.get("related_decisions").is_none(), "{v}");
}
