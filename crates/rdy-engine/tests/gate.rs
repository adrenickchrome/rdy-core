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

fn upd(r: &mut Rdy, view: &str, id: &str, fields: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(&agent(), WriteRequest { view: view.into(), ops: serde_json::from_value(json!([{"op": "update", "id": id, "fields": fields}])).unwrap(), justification: None, message: String::new() })
}

#[test]
fn land_refuses_when_view_edited_a_proposed_decision_after_propose() {
    let (_d, mut r, s) = setup();
    let res = r.decide(&agent(), "work", DecisionInput { parent: s, statement: "A".into(), ..Default::default() }).unwrap();
    let did = res.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "p").unwrap();
    upd(&mut r, "work", &did, json!({"statement": "A2"})).unwrap();
    let head = r.store().canon_head().unwrap();
    let n = r.store().view("work").unwrap().changes.len();
    let e = r.land(&founder(), &p.id).unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.message.contains(&did));
    assert!(e.fix.contains("propose again"));
    assert_eq!(r.store().view("work").unwrap().changes.len(), n);
    assert_eq!(r.block(&Scope::View("work".into()), &BlockId::parse(&did).unwrap()).unwrap().field_str("status"), Some("proposed"));
    assert_eq!(r.store().canon_head().unwrap(), head);
}

#[test]
fn revoke_links_and_requires_a_decision() {
    let (_d, mut r, s) = setup();
    let res = r.decide(&agent(), "work", DecisionInput { parent: s.clone(), statement: "A".into(), ..Default::default() }).unwrap();
    let did = res.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "p").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let rv = r.revoke(&agent(), "work", &did, "no longer true", &s).unwrap();
    let b = r.block(&Scope::View("work".into()), &BlockId::parse(&rv.created["$decision"]).unwrap()).unwrap();
    assert_eq!(b.links["revokes"][0].target.to_string(), did);
    assert_eq!(b.field_str("statement"), Some(format!("Revoke {did}: no longer true").as_str()));
    assert_eq!(r.revoke(&agent(), "work", &s, "x", &s).unwrap_err().code, "E_BAD_INPUT");
}

#[test]
fn propose_detects_stale_base_early() {
    let (_d, mut r, s) = setup();
    let p = r.propose(&agent(), "work", "skeleton").unwrap();
    r.land(&founder(), &p.id).unwrap();
    upd(&mut r, "a", &s, json!({"title": "A"})).unwrap();
    upd(&mut r, "b", &s, json!({"title": "B"})).unwrap();
    let pb = r.propose(&agent(), "b", "b").unwrap();
    r.land(&founder(), &pb.id).unwrap();
    assert_eq!(r.propose(&agent(), "a", "a").unwrap_err().code, "E_STALE_BASE");
}

#[test]
fn land_excludes_later_view_changes_and_stamps_the_approver() {
    let (_d, mut r, s) = setup();
    let res = r.decide(&agent(), "work", DecisionInput { parent: s.clone(), statement: "A".into(), ..Default::default() }).unwrap();
    let did = BlockId::parse(&res.created["$decision"]).unwrap();
    let p = r.propose(&agent(), "work", "p").unwrap();
    let later = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": "later"}])).unwrap(), justification: None, message: String::new() }).unwrap();
    let cid = BlockId::parse(&later.created["$c"]).unwrap();
    let landing = r.land(&founder(), &p.id).unwrap();
    assert!(r.block(&Scope::Canon, &cid).is_err());
    assert!(r.block(&Scope::View("work".into()), &cid).is_ok());
    assert_eq!(r.block(&Scope::Canon, &did).unwrap().field_str("status"), Some("accepted"));
    let rec: LandingRecord = r.store().get(&landing.parse().unwrap()).unwrap();
    let prop: Proposal = r.store().get(&rec.proposal).unwrap();
    let last: Change = r.store().get(prop.changes.last().unwrap()).unwrap();
    assert_eq!(last.provenance.actor, "human:founder");
}

#[test]
fn readers_cannot_propose() {
    let (_d, mut r, _s) = setup();
    let reader = Session { actor: "agent:r".into(), run: None, role: Role::Reader };
    let e = r.propose(&reader, "work", "nope").unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
}

fn w(r: &mut Rdy, s: &Session, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(s, WriteRequest { view: view.into(), ops: serde_json::from_value(ops).unwrap(), justification: None, message: String::new() })
}

/// Seeds canon with section S and term T, then: run-2 adds a claim under S referencing T,
/// run-1 deletes T and S. Returns (dir, rdy, run-2 session).
fn two_views() -> (tempfile::TempDir, Rdy, Session) {
    let (d, mut r, s) = setup();
    let t = w(&mut r, &agent(), "work", json!([{"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Ledger"}, "body": "Balances."}])).unwrap().created["$t"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let run2 = Session { actor: "agent:designer".into(), run: Some("run-2".into()), role: Role::Writer };
    w(&mut r, &run2, "run-2", json!([{"op": "create", "type": "claim", "parent": s, "body": "Uses the {{" .to_string() + &t + "}}."}])).unwrap();
    w(&mut r, &agent(), "run-1", json!([{"op": "delete", "id": t}, {"op": "delete", "id": s}])).unwrap();
    (d, r, run2)
}

#[test]
fn propose_refuses_orphans_and_dangling_refs_after_canon_moved() {
    let (_d, mut r, run2) = two_views();
    let p1 = r.propose(&agent(), "run-1", "remove").unwrap();
    r.land(&founder(), &p1.id).unwrap();
    let head = r.store().canon_head().unwrap();
    let e = r.propose(&run2, "run-2", "add claim").unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(e.issues.iter().any(|i| i.code == "E_MISSING_PARENT"), "{:?}", e.issues);
    assert!(e.issues.iter().any(|i| i.code == "E_DANGLING_REF"), "{:?}", e.issues);
    assert_eq!(r.store().canon_head().unwrap(), head);
}

#[test]
fn land_refuses_orphans_and_dangling_refs_when_canon_moved_after_propose() {
    let (_d, mut r, run2) = two_views();
    let p2 = r.propose(&run2, "run-2", "add claim").unwrap();
    let p1 = r.propose(&agent(), "run-1", "remove").unwrap();
    r.land(&founder(), &p1.id).unwrap();
    let head = r.store().canon_head().unwrap();
    let e = r.land(&founder(), &p2.id).unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(e.issues.iter().any(|i| i.code == "E_MISSING_PARENT"), "{:?}", e.issues);
    assert_eq!(r.store().canon_head().unwrap(), head);
    let canon = r.materialized(&Scope::Canon).unwrap();
    assert!(canon.blocks.values().all(|b| b.block_type != "claim"));
}

#[test]
fn land_with_a_hash_that_is_not_a_proposal_is_bad_input() {
    let (_d, mut r, s) = setup();
    let section = r.block(&Scope::View("work".into()), &BlockId::parse(&s).unwrap()).unwrap();
    let e = r.land(&founder(), &section.version_hash().to_string()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
}
