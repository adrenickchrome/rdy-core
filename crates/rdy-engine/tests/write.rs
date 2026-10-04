use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session {
    Session { actor: "agent:designer".into(), run: Some("run-1".into()), role: Role::Writer }
}

fn ops(v: serde_json::Value) -> Vec<OpInput> {
    serde_json::from_value(v).unwrap()
}

fn req(v: serde_json::Value) -> WriteRequest {
    WriteRequest { view: "work".into(), ops: ops(v), justification: None, message: "test".into() }
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn skeleton(r: &mut Rdy) -> WriteResult {
    r.write(&agent(), req(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$terms", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$ledger", "parent": "$terms", "fields": {"name": "Ledger"}, "body": "The record of balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$terms", "body": "Balances live on the {{$ledger}}."}
    ]))).unwrap()
}

#[test]
fn placeholders_build_a_document_in_one_write() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let ledger = BlockId::parse(&res.created["$ledger"]).unwrap();
    let c1 = BlockId::parse(&res.created["$c1"]).unwrap();
    let claim = r.block(&Scope::View("work".into()), &c1).unwrap();
    assert_eq!(claim.body, format!("Balances live on the {{{{{ledger}}}}}."));
    assert!(claim.pins.is_empty(), "same-write targets are unpinned");
    assert!(res.warnings.is_empty());
    assert!(r.materialized(&Scope::Canon).unwrap().blocks.is_empty(), "nothing in canon yet");
}

#[test]
fn provenance_comes_from_the_session() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let change: Change = r.store().get(&res.change.parse().unwrap()).unwrap();
    assert_eq!(change.provenance.actor, "agent:designer");
    assert_eq!(change.provenance.run.as_deref(), Some("run-1"));
}

#[test]
fn references_to_existing_blocks_are_pinned() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let ledger_id = res.created["$ledger"].clone();
    let res2 = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": format!("See {{{{{ledger_id}}}}}.")}
    ]))).unwrap();
    let id = BlockId::parse(&res2.created["op0"]).unwrap();
    let scope = Scope::View("work".into());
    let ledger = r.block(&scope, &BlockId::parse(&ledger_id).unwrap()).unwrap();
    let claim = r.block(&scope, &id).unwrap();
    assert_eq!(claim.pins[&ledger.id], ledger.version_hash());
}

#[test]
fn unknown_and_malformed_references_are_refused_and_nothing_is_written() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let before = r.store().view("work").unwrap().changes.len();
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": "See {{t-ZZZZZ}}."}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF");
    assert!(e.message.contains("t-ZZZZZ"));
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": "See {{the ledger}}."}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF");
    assert!(e.fix.contains("{{"), "{}", e.fix);
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
}

#[test]
fn validation_errors_carry_issues() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": ""}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert_eq!(e.issues[0].code, "E_BODY_REQUIRED");
}

#[test]
fn stale_base_is_refused_with_the_current_hash() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let c1 = res.created["$c1"].clone();
    let scope = Scope::View("work".into());
    let v1 = r.block(&scope, &BlockId::parse(&c1).unwrap()).unwrap().version_hash();
    r.write(&agent(), req(json!([{"op": "update", "id": c1, "base": v1.to_string(), "body": "Edited once."}]))).unwrap();
    let v2 = r.block(&scope, &BlockId::parse(&c1).unwrap()).unwrap().version_hash();
    let e = r.write(&agent(), req(json!([{"op": "update", "id": c1, "base": v1.to_string(), "body": "Edited twice."}]))).unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains(&v2.to_string()));
}

#[test]
fn after_controls_sibling_order() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let terms = res.created["$terms"].clone();
    let ledger = res.created["$ledger"].clone();
    let res2 = r.write(&agent(), req(json!([
        {"op": "create", "type": "note", "parent": terms, "after": ledger, "body": "Between ledger and claim."}
    ]))).unwrap();
    let scope = Scope::View("work".into());
    let m = r.materialized(&scope).unwrap();
    let kids: Vec<String> = m.children(Some(&BlockId::parse(&terms).unwrap())).iter().map(|b| b.id.to_string()).collect();
    assert_eq!(kids, vec![ledger, res2.created["op0"].clone(), res.created["$c1"].clone()]);
}

#[test]
fn permissions_and_structure_rules() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let reader = Session { role: Role::Reader, ..agent() };
    assert_eq!(r.write(&reader, req(json!([{"op": "delete", "id": res.created["$c1"]}]))).unwrap_err().code, "E_PERMISSION");
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "decision", "parent": res.created["$terms"], "fields": {"statement": "Use a ledger", "status": "accepted"}}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    assert_eq!(r.write(&agent(), req(json!([{"op": "delete", "id": res.created["$terms"]}]))).unwrap_err().code, "E_HAS_CHILDREN");
    let canon_write = WriteRequest { view: "canon".into(), ops: ops(json!([{"op": "delete", "id": res.created["$c1"]}])), justification: None, message: String::new() };
    assert_eq!(r.write(&agent(), canon_write).unwrap_err().code, "E_RESERVED_VIEW");
    assert_eq!(r.write(&agent(), req(json!([{"op": "create", "type": "claim", "parent": "$nope", "body": "x"}]))).unwrap_err().code, "E_BAD_INPUT");
}

#[test]
fn missing_skeleton_sections_are_warnings() {
    let (_d, mut r) = project();
    let res = r.write(&agent(), req(json!([
        {"op": "create", "type": "doc", "fields": {"title": "Overview", "doc_type": "overview"}}
    ]))).unwrap();
    let codes: Vec<_> = res.warnings.iter().map(|w| w.code).collect();
    assert_eq!(codes, vec!["W_SKELETON_MISSING", "W_SKELETON_MISSING"]); // Summary, Goals
}
