use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session {
    Session { actor: "agent:designer".into(), run: Some("run-1".into()), role: Role::Writer }
}
fn founder() -> Session {
    Session { actor: "human:founder".into(), run: None, role: Role::Approver }
}

fn setup() -> (tempfile::TempDir, Rdy, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r.write(&agent(), WriteRequest {
        view: "work".into(),
        ops: serde_json::from_value(json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Decisions", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
        ])).unwrap(),
        justification: None,
        message: "skeleton".into(),
    }).unwrap();
    let section = res.created["$s"].clone();
    (dir, r, section)
}

#[test]
fn decide_then_land_accepts_and_freezes() {
    let (_d, mut r, s) = setup();
    let res = r.decide(&agent(), "work", DecisionInput { parent: s.clone(), statement: "Wallets don't move".into(), authority_basis: Some("direct_ruling".into()), ..Default::default() }).unwrap();
    let did = BlockId::parse(&res.created["$decision"]).unwrap();
    let work = Scope::View("work".into());
    let d = r.block(&work, &did).unwrap();
    assert_eq!(d.field_str("status"), Some("proposed"));
    assert!(d.field_str("recorded_at").unwrap().ends_with('Z'));

    let p = r.propose(&agent(), "work", "wallet ruling").unwrap();
    assert_eq!(p.changes, 2);
    assert_eq!(r.land(&agent(), &p.id).unwrap_err().code, "E_PERMISSION");
    let landing = r.land(&founder(), &p.id).unwrap();

    let canon = r.block(&Scope::Canon, &did).unwrap();
    assert_eq!(canon.field_str("status"), Some("accepted"));
    assert!(canon.field_str("decided_at").is_some());
    let rec: LandingRecord = r.store().get(&landing.parse().unwrap()).unwrap();
    assert_eq!(rec.approved_by.actor, "human:founder");

    let e = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([{"op": "update", "id": did.to_string(), "fields": {"statement": "changed"}}])).unwrap(), justification: None, message: String::new() }).unwrap_err();
    assert_eq!(e.code, "E_FROZEN");
}

#[test]
fn supersede_links_to_the_old_decision() {
    let (_d, mut r, s) = setup();
    let first = r.decide(&agent(), "work", DecisionInput { parent: s.clone(), statement: "Wallets move with members".into(), ..Default::default() }).unwrap();
    let old = first.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "one").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let res = r.supersede(&agent(), "work", &old, Some("moves with them".into()), DecisionInput { parent: s.clone(), statement: "Wallets don't move".into(), ..Default::default() }).unwrap();
    let new = r.block(&Scope::View("work".into()), &BlockId::parse(&res.created["$decision"]).unwrap()).unwrap();
    let link = &new.links["supersedes"][0];
    assert_eq!(link.target.to_string(), old);
    assert_eq!(link.part.as_deref(), Some("moves with them"));
    assert_eq!(r.supersede(&agent(), "work", &s, None, DecisionInput { parent: s.clone(), statement: "x".into(), ..Default::default() }).unwrap_err().code, "E_BAD_INPUT");
}

#[test]
fn propose_rules() {
    let (_d, mut r, s) = setup();
    assert_eq!(r.propose(&agent(), "empty", "nothing").unwrap_err().code, "E_BAD_INPUT");
    // land a term and a claim that references it
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "parent": s, "body": "Uses the {{$t}}."}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let p = r.propose(&agent(), "work", "terms").unwrap();
    r.land(&founder(), &p.id).unwrap();
    // another view deletes the term while the claim still references it
    r.write(&agent(), WriteRequest { view: "cleanup".into(), ops: serde_json::from_value(json!([{"op": "delete", "id": res.created["$t"]}])).unwrap(), justification: None, message: String::new() }).unwrap();
    let e = r.propose(&agent(), "cleanup", "remove term").unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(e.issues.iter().any(|i| i.code == "E_DANGLING_REF"));
}

#[test]
fn rfc3339_shape() {
    let t = rfc3339_now();
    assert_eq!(t.len(), 20);
    assert!(t.starts_with("20") && t.ends_with('Z') && &t[10..11] == "T");
}
