//! vop1 (gate): an undo chain written through the store survives a reindex and fsck.
use rdy_model::*;
use rdy_store::Store;
use rdy_store::view::ViewOpKind;
use std::collections::BTreeMap;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("r".into()),
        role: Role::Writer,
        at_ms: 7,
    }
}

fn change(view: &str, ops: Vec<Op>, by: &Provenance) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: by.clone(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn create(s: &mut Store, view: &str, id: &str, by: &Provenance) -> Hash {
    s.commit_to_view(
        view,
        change(
            view,
            vec![Op::Create {
                block: block(id, "body"),
            }],
            by,
        ),
    )
    .unwrap()
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

#[test]
fn vop1_an_undo_chain_survives_a_reindex_and_fsck() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let c2 = create(&mut s, "a", "c-00002", &by);
    let commit2 = s.view_head("a").unwrap().unwrap().head;
    let u = s
        .view_append("a", vec![ViewOpKind::Undo { target: commit2 }], &by)
        .unwrap()
        .head;
    s.view_append("a", vec![ViewOpKind::Undo { target: u }], &by)
        .unwrap();
    s.reindex().unwrap();
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![c1, c2]);
    let c = s.check_objects().unwrap();
    assert_eq!(
        (c.views, c.view_ops),
        (1, 5),
        "create, two commits, two undos"
    );
}
