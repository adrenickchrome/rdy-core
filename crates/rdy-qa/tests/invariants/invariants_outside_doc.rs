//! ndh1 (D-P3YD5, D-X3136): a decision, a question, a finding or a source may sit at the project
//! root or in a folder, so the placement rule passes it; every other block at the top level is
//! still a violation.
use rdy_engine::{Rdy, Scope};
use rdy_model::{Block, BlockId, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::invariants::{check, check_all};
use serde_json::json;

/// Stores a landing whose snapshot adds the prose `p-H0ST3` with no parent to canon, as
/// `pub12_implicit_refuses_a_stored_orphan_doc_as_a_named_profile_does` forges its orphan doc.
fn orphan_prose_landing(r: &mut Rdy) -> Hash {
    let head = r.read().canon_head().unwrap();
    let mut snapshot = r.read().materialized(&Scope::Canon).unwrap().snapshot();
    let prose = Block {
        id: BlockId::parse("p-H0ST3").unwrap(),
        block_type: "prose".into(),
        parent: None,
        order: "V".into(),
        fields: Default::default(),
        body: "Loose words.".into(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    };
    let hash = r.store_mut().put(rdy_store::kinds::BLOCK, &prose).unwrap();
    snapshot.blocks.insert(prose.id.clone(), hash);
    let state = r
        .store_mut()
        .put(rdy_store::kinds::SNAPSHOT, &snapshot)
        .unwrap();
    let mut landing = r.store().landing(&head).unwrap();
    // a forged record from a read: it no longer has the read's identity
    landing.read_identity = None;
    landing.state = state;
    landing.prev = Some(head);
    let damaged = r
        .store_mut()
        .put(rdy_store::kinds::LANDING, &landing)
        .unwrap();
    r.invalidate();
    damaged
}

#[test]
fn ndh1_invariants_accept_outside_doc_blocks() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$area", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "One claim."},
        {"op": "create", "type": "decision", "fields": {"statement": "Use one ledger.", "status": "proposed"}},
        {"op": "create", "type": "question", "parent": "$area", "fields": {"prompt": "Which ledger?"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    check_all(&r).unwrap_or_else(|v| panic!("{v:?}"));

    let damaged = orphan_prose_landing(&mut r);
    let vs = check(&r, &Scope::At(damaged)).unwrap_err();
    let placement: Vec<(Option<String>, &str)> = vs
        .iter()
        .filter(|v| v.rule == "placement")
        .map(|v| (v.block.as_ref().map(|b| b.to_string()), v.detail.as_str()))
        .collect();
    assert_eq!(
        placement,
        vec![(Some("p-H0ST3".to_string()), "a prose at the top level")],
        "{vs:?}"
    );
}
