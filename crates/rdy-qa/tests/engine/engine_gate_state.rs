//! snap1 at the gate: landing publishes canon incrementally with structural sharing; the gate's
//! checks run on the state, never on a clone of canon.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, project, write};
use rdy_qa::invariants::check;
use serde_json::json;
use std::sync::Arc;

#[test]
fn snap1_a_landing_seeds_canon_from_the_previous_canon_and_shares_untouched_blocks() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "one"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let before = r.read().materialized(&Scope::Canon).unwrap();
    let section = before
        .iter()
        .find(|b| b.block_type == "section")
        .unwrap()
        .id
        .clone();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": "two"}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let s = r.read();
    assert_eq!(
        s.is_built(&Scope::Canon),
        (true, true),
        "seeded by the landing; the gate's tension check (tgat1) read the graph before it, so \
         the graph is carried too"
    );
    let after = s.materialized(&Scope::Canon).unwrap();
    assert_eq!(after.len(), 4);
    for id in before.ids() {
        assert!(
            Arc::ptr_eq(
                &before.block_arc(id).unwrap(),
                &after.block_arc(id).unwrap()
            ),
            "{id}"
        );
    }
    assert_eq!(
        after.snapshot(),
        r.store().canon_state().unwrap(),
        "the seeded canon is the store's"
    );
    assert!(check(&r, &Scope::Canon).is_ok());
}

#[test]
fn snap1_propose_and_land_check_against_the_states_canon_and_refuse_as_before() {
    let (_d, mut r) = project();
    let res = write(&mut r, "a", json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "dup", "title": "A", "doc_type": "page"}}])).unwrap();
    write(&mut r, "b", json!([{"op": "create", "type": "doc", "fields": {"name": "DUP", "title": "B", "doc_type": "page"}}])).unwrap();
    land(&mut r, "a").unwrap();
    let e = r
        .propose(&rdy_qa::fixtures::writer(), "b", "clash")
        .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_VALIDATION);
    assert!(e.issues.iter().any(|i| i.code == "E_PATH_CLASH"), "{e:?}");
    let held = r.read();
    let doc = rdy_model::BlockId::parse(&res.created["$d"]).unwrap();
    assert!(
        held.materialized(&Scope::Canon)
            .unwrap()
            .get(&doc)
            .is_some()
    );
    // a proposal republishes the state (its frame must reach the event stream) without changing content
    write(&mut r, "c", json!([{"op": "create", "type": "doc", "fields": {"name": "c", "title": "C", "doc_type": "page"}}])).unwrap();
    let s1 = r.read();
    r.propose(&rdy_qa::fixtures::writer(), "c", "fine").unwrap();
    let s2 = r.read();
    assert!(s2.seq() > s1.seq());
    assert!(
        Arc::ptr_eq(
            &s1.materialized(&Scope::View("c".into())).unwrap(),
            &s2.materialized(&Scope::View("c".into())).unwrap()
        ),
        "carried over"
    );
}
