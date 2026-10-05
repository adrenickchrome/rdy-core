//! snap1 at the gate: read-your-writes through an MCP-shaped sequence, and publish-before-return.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, project, write};
use serde_json::json;

#[test]
fn snap1_every_write_is_visible_to_the_next_read_and_the_state_is_republished_each_time() {
    let (_d, mut r) = project();
    let mut seqs = vec![r.read().seq()];
    for i in 0..10 {
        let res = write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": format!("d{i}"), "title": "D", "doc_type": "glossary"}}])).unwrap();
        let s = r.read();
        seqs.push(s.seq());
        let created = rdy_model::BlockId::parse(res.created.values().next().unwrap()).unwrap();
        assert!(
            s.materialized(&Scope::View("work".into()))
                .unwrap()
                .get(&created)
                .is_some(),
            "write {i}"
        );
    }
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    land(&mut r, "work").unwrap();
    let s = r.read();
    assert_eq!(s.materialized(&Scope::Canon).unwrap().len(), 10);
    assert!(s.included("work").is_empty());
    assert!(rdy_qa::invariants::check(&mut r, &Scope::Canon).is_ok());
}

#[test]
fn snap1_two_states_from_two_views_share_canons_build() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    land(&mut r, "work").unwrap();
    write(&mut r, "x", json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "B", "doc_type": "glossary"}}])).unwrap();
    let s = r.read();
    let canon = s.materialized(&Scope::Canon).unwrap();
    let x = s.materialized(&Scope::View("x".into())).unwrap();
    assert_eq!((canon.len(), x.len()), (1, 2));
    let shared = x.ids().find(|id| canon.get(id).is_some()).unwrap();
    assert!(
        std::sync::Arc::ptr_eq(
            &canon.block_arc(shared).unwrap(),
            &x.block_arc(shared).unwrap()
        ),
        "the view's build was seeded from canon with structural sharing"
    );
}
