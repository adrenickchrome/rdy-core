//! priv1 (intake §2, API hygiene): `Materialized` hides its maps; readers use accessors.
use rdy_engine::Scope;
use rdy_qa::fixtures::{project, write};
use serde_json::json;

#[test]
fn priv1_accessors_agree_with_each_other_and_with_the_store() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "T", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    let m = r.materialized(&Scope::View("work".into())).unwrap().clone();
    assert_eq!(m.len(), 3);
    assert!(!m.is_empty());
    let ids: Vec<_> = m.ids().cloned().collect();
    assert_eq!(ids, m.iter().map(|b| b.id.clone()).collect::<Vec<_>>());
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "id order");
    for b in m.iter() {
        assert_eq!(m.version_of(&b.id), Some(b.version_hash()));
        assert_eq!(m.get(&b.id), Some(b));
    }
    assert_eq!(m.versions().count(), 3);
    let snap = m.snapshot();
    assert_eq!(snap.blocks.len(), 3);
    assert_eq!(m.snapshot_id(), snap.id());
    assert_eq!(
        snap,
        r.store().view_state("work").unwrap(),
        "the accessor snapshot is the store's"
    );
    let doc = rdy_model::BlockId::parse(&res.created["$d"]).unwrap();
    assert_eq!(m.version_of(&doc), snap.blocks.get(&doc).copied());
}

#[test]
fn priv1_the_graph_builds_from_an_iterator_and_looks_blocks_up_through_the_lookup() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "T", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Cites {{$a}}."}
    ]))
    .unwrap();
    let m = r.materialized(&Scope::View("work".into())).unwrap().clone();
    let g = rdy_graph::Graph::build_from(m.iter());
    let lookup: &dyn rdy_graph::BlockLookup = &*m;
    let none = |_: &rdy_model::Hash| None;
    assert!(g.suspects(lookup, &none).is_empty());
    let doc = m.iter().find(|b| b.block_type == "doc").unwrap();
    assert!(
        !g.outgoing(&doc.id).is_empty(),
        "the doc contains its section"
    );
    let cites = m.iter().find(|b| b.body.starts_with("Cites")).unwrap();
    assert!(
        g.outgoing(&cites.id)
            .iter()
            .any(|e| e.kind == rdy_graph::EdgeKind::References)
    );
}
