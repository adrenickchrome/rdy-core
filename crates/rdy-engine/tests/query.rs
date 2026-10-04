use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session { Session { actor: "agent:a".into(), run: None, role: Role::Writer } }
fn founder() -> Session { Session { actor: "human:founder".into(), run: None, role: Role::Approver } }

#[test]
fn context_and_suspects_after_landing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Wallets", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let d = r.decide(&agent(), "work", DecisionInput { parent: s.clone(), statement: "Wallets don't move".into(), ..Default::default() }).unwrap();
    let did = d.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    // a claim decided by the (now accepted) decision
    let c = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Leavers open a new wallet per {{{{{did}}}}}."), "links": {"decided_by": [{"target": did}]}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let cid = BlockId::parse(&c.created["op0"]).unwrap();
    let work = Scope::View("work".into());
    let ctx = r.context(&work, &cid).unwrap();
    assert_eq!(ctx.governing, vec![did.clone()]);
    assert!(ctx.suspects.is_empty());
    assert_eq!(ctx.doc.as_deref(), Some(res.created["$doc"].as_str()));

    let imp = r.impact(&work, &BlockId::parse(&did).unwrap(), 2).unwrap();
    assert_eq!(imp.iter().map(|i| i.id.clone()).collect::<Vec<_>>(), vec![cid.to_string()]);

    // supersede the decision and land: the claim becomes suspect
    r.supersede(&agent(), "work", &did, None, DecisionInput { parent: s.clone(), statement: "Wallets move".into(), ..Default::default() }).unwrap();
    let p = r.propose(&agent(), "work", "reverse").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let sus = r.suspects(&Scope::Canon).unwrap();
    assert!(sus.iter().any(|x| x.block == cid && x.reason == "superseded"));
    let st = r.context(&Scope::Canon, &BlockId::parse(&did).unwrap()).unwrap().status;
    assert!(matches!(st, Some(rdy_graph::DerivedStatus::Superseded { .. })));
}
