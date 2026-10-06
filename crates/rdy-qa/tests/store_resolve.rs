//! res1 (gate): excluding a side drops a conflict; a resolution from the base lands.
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
    }
}
fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}
fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
fn version(s: &Store, view: &str, b: &str) -> Hash {
    *s.view_state(view).unwrap().blocks.get(&id(b)).unwrap()
}
fn land(s: &mut Store, view: &str) -> Hash {
    let p = s.propose(view, "t", prov("human:h")).unwrap();
    s.land(&p, prov("human:h")).unwrap()
}

/// Canon holds X; view a and view b each update X from canon's version; a includes b's change.
fn conflicted() -> (tempfile::TempDir, Store, Hash, Hash, Hash) {
    let (d, mut s) = store();
    let by = prov("agent:a");
    s.commit_to_view(
        "seed",
        change(
            "seed",
            vec![Op::Create {
                block: block("c-00001", "v1"),
            }],
            &by,
        ),
    )
    .unwrap();
    land(&mut s, "seed");
    let v1 = version(&s, "seed", "c-00001");
    let ca = s
        .commit_to_view(
            "a",
            change(
                "a",
                vec![Op::Update {
                    base: v1,
                    block: block("c-00001", "a"),
                }],
                &by,
            ),
        )
        .unwrap();
    let cb = s
        .commit_to_view(
            "b",
            change(
                "b",
                vec![Op::Update {
                    base: v1,
                    block: block("c-00001", "b"),
                }],
                &prov("agent:b"),
            ),
        )
        .unwrap();
    s.view_append("a", vec![ViewOpKind::Include { changes: vec![cb] }], &by)
        .unwrap();
    (d, s, v1, ca, cb)
}

#[test]
fn res1_excluding_a_side_drops_the_conflict_and_a_resolution_lands() {
    let (_d, mut s, v1, ca, cb) = conflicted();
    // excluding b's side: the conflict is gone and a's own version reads
    s.view_append(
        "a",
        vec![ViewOpKind::Exclude { changes: vec![cb] }],
        &prov("agent:a"),
    )
    .unwrap();
    let m = s.view_merge("a").unwrap();
    assert!(m.conflicts.is_empty(), "{m:?}");
    assert_eq!(
        s.get_block(m.state.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "a"
    );
    // in a fresh scene, a resolution from the base lands and canon holds it
    let (_d2, mut s2, _v1, _ca, _cb) = conflicted();
    let m2 = s2.view_merge("a").unwrap();
    let current = |b: &BlockId| m2.state.blocks.get(b).copied();
    let fix = change(
        "a",
        vec![Op::Update {
            base: v1,
            block: block("c-00001", "merged"),
        }],
        &prov("agent:a"),
    );
    s2.commit_to_view_resolving("a", fix, &current, &m2.conflicts, None)
        .unwrap();
    land(&mut s2, "a");
    let canon = s2.canon_state().unwrap();
    assert_eq!(
        s2.get_block(canon.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "merged"
    );
    assert_eq!(
        s2.landings().unwrap()[0].1.changes.len(),
        3,
        "a's change, b's change and the resolution all landed"
    );
    let _ = ca;
}
