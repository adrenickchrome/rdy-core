//! rdec1: related decisions (FW-EN5, FW-EN5a): refines, supersedes and revokes neighbours both
//! ways at any status, and live or proposed decisions on the same entities.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

struct World {
    s: String,
    e: String,
    a: String,
    b: String,
    d: String,
}

/// Entity Ledger service; decision A affects it; B refines A; D affects it and refines B. Each
/// landed in turn.
fn world(r: &mut Rdy) -> World {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service"}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let (s, e) = (w.created["$s"].clone(), w.created["$e"].clone());
    let decide = |r: &mut Rdy, statement: &str, affects: Vec<String>, refines: Vec<String>| {
        let id = r
            .decide(
                &writer(),
                "work",
                DecisionInput {
                    parent: s.clone(),
                    statement: statement.into(),
                    affects,
                    refines,
                    ..Default::default()
                },
            )
            .unwrap()
            .created["$decision"]
            .clone();
        land(r, "work").unwrap();
        id
    };
    let a = decide(r, "The ledger is append-only", vec![e.clone()], vec![]);
    let b = decide(r, "Corrections are new entries", vec![], vec![a.clone()]);
    let d = decide(
        r,
        "Entries are kept forever",
        vec![e.clone()],
        vec![b.clone()],
    );
    World { s, e, a, b, d }
}

fn related(r: &Rdy, id: &str) -> Value {
    let (rel, omitted) = r
        .read()
        .related_decisions(&Scope::Canon, &[BlockId::parse(id).unwrap()])
        .unwrap();
    assert_eq!(omitted, 0);
    serde_json::to_value(rel).unwrap()
}

fn pairs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["decision"].as_str().unwrap().to_string(),
                x["relation"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn rdec1_a_decision_lists_its_neighbours_both_ways_and_decisions_on_the_same_entity() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    assert_eq!(
        related(&r, &w.d),
        json!([
            {"for": w.d, "decision": w.b, "status": "live", "statement": "Corrections are new entries", "relation": "refines"},
            {"for": w.d, "decision": w.a, "status": "live", "statement": "The ledger is append-only", "relation": "same_entity", "entities": [w.e]}
        ])
    );
    assert_eq!(
        pairs(&related(&r, &w.a)),
        vec![
            (w.b.clone(), "refined_by".into()),
            (w.d.clone(), "same_entity".into())
        ]
    );
    assert_eq!(
        pairs(&related(&r, &w.b)),
        vec![
            (w.a.clone(), "refines".into()),
            (w.d.clone(), "refined_by".into())
        ]
    );
    // a block that is not a decision has nothing related
    assert_eq!(related(&r, &w.e), json!([]));
}

#[test]
fn rdec1_revokes_is_a_neighbour_and_neighbours_show_any_status() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let rv = r
        .revoke(&writer(), "work", &w.a, "wrong", &w.s, None)
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    assert_eq!(
        pairs(&related(&r, &w.d)),
        vec![(w.b.clone(), "refines".into())],
        "a revoked decision shares no entity"
    );
    // FW-EN5a: revokes both ways, at any status; the revoked decision still lists who shares its entity
    assert_eq!(
        pairs(&related(&r, &w.a)),
        vec![
            (w.b.clone(), "refined_by".into()),
            (rv.clone(), "revoked_by".into()),
            (w.d.clone(), "same_entity".into())
        ]
    );
    let rel = related(&r, &rv);
    assert_eq!(pairs(&rel), vec![(w.a.clone(), "revokes".into())]);
    assert_eq!(rel[0]["status"], "revoked");
    let x = r
        .supersede(
            &writer(),
            "work",
            &w.b,
            None,
            DecisionInput {
                parent: w.s.clone(),
                statement: "Corrections reverse entries".into(),
                no_inherit: vec!["refines".into()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let rel = related(&r, &w.b);
    let sup = rel
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "superseded_by")
        .unwrap();
    assert_eq!(sup["decision"], x);
    let rel = related(&r, &x);
    let old = rel
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["relation"] == "supersedes")
        .unwrap();
    assert_eq!(
        (old["decision"].as_str(), old["status"].as_str()),
        (Some(w.b.as_str()), Some("superseded"))
    );
}

#[test]
fn rdec1_a_decision_that_references_an_entity_shares_it() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let y = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: w.s.clone(),
                statement: format!("{{{{{}}}}} keeps entries for ten years", w.e),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let mut got = pairs(&related(&r, &y));
    got.sort();
    let mut want = vec![
        (w.a.clone(), "same_entity".to_string()),
        (w.d.clone(), "same_entity".to_string()),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn rdec1_a_proposed_decision_on_the_same_entity_is_related() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let p = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: w.s.clone(),
                statement: "Entries are signed".into(),
                affects: vec![w.e.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let (rel, omitted) = r
        .read()
        .related_decisions(
            &Scope::View("work".into()),
            &[BlockId::parse(&w.a).unwrap()],
        )
        .unwrap();
    assert_eq!(omitted, 0);
    let mut got: Vec<(String, String, String)> = rel
        .iter()
        .map(|x| (x.decision.clone(), x.relation.clone(), x.status.clone()))
        .collect();
    assert_eq!(
        got.remove(0),
        (w.b.clone(), "refined_by".to_string(), "live".to_string())
    );
    let mut want = vec![
        (w.d.clone(), "same_entity".to_string(), "live".to_string()),
        (p.clone(), "same_entity".to_string(), "proposed".to_string()),
    ];
    want.sort();
    assert_eq!(
        got, want,
        "same_entity entries come by decision id; a proposed decision counts (FW-EN5a)"
    );
    assert!(
        pairs(&related(&r, &w.a)).iter().all(|(d, _)| *d != p),
        "canon does not hold the proposal"
    );
}
