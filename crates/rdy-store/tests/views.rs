use rdy_model::*;
use rdy_store::{Store, StoreError};
use std::collections::BTreeMap;

fn block(id: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "someone".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
    }
}

#[test]
fn writes_go_to_the_view_not_canon_until_landed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let b = block("b-AAAAA", "claim", "x");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    assert_eq!(
        s.view_state("work").unwrap().blocks[&b.id],
        b.version_hash()
    );
    assert!(s.canon_state().unwrap().blocks.is_empty());

    let p = s.propose("work", "first", prov(Role::Writer)).unwrap();
    let l = s.land(&p, prov(Role::Approver)).unwrap();
    assert_eq!(s.canon_head().unwrap(), Some(l));
    assert_eq!(s.canon_state().unwrap().blocks[&b.id], b.version_hash());
    assert!(s.view("work").unwrap().changes.is_empty());
    assert_eq!(s.view("work").unwrap().base, Some(l));
}

#[test]
fn stale_base_is_refused_and_names_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let v1 = block("b-AAAAA", "claim", "one");
    let v2 = block("b-AAAAA", "claim", "two");
    s.commit_to_view("w", change("w", vec![Op::Create { block: v1.clone() }]))
        .unwrap();
    s.commit_to_view(
        "w",
        change(
            "w",
            vec![Op::Update {
                base: v1.version_hash(),
                block: v2.clone(),
            }],
        ),
    )
    .unwrap();
    let stale = change(
        "w",
        vec![Op::Update {
            base: v1.version_hash(),
            block: block("b-AAAAA", "claim", "three"),
        }],
    );
    match s.commit_to_view("w", stale) {
        Err(StoreError::StaleBase {
            actual, expected, ..
        }) => {
            assert_eq!(expected, v1.version_hash());
            assert_eq!(actual, Some(v2.version_hash()));
        }
        other => panic!("expected StaleBase, got {other:?}"),
    }
    assert_eq!(s.view("w").unwrap().changes.len(), 2, "nothing written");
}

#[test]
fn frozen_decisions_cannot_be_updated_or_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let mut d = block("D-W2HXR", decision::DECISION, "");
    d.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    s.commit_to_view("w", change("w", vec![Op::Create { block: d.clone() }]))
        .unwrap();
    let mut edited = d.clone();
    edited
        .fields
        .insert("statement".into(), FieldValue::Str("changed".into()));
    let upd = change(
        "w",
        vec![Op::Update {
            base: d.version_hash(),
            block: edited,
        }],
    );
    assert!(matches!(
        s.commit_to_view("w", upd),
        Err(StoreError::Frozen(_))
    ));
    let del = change(
        "w",
        vec![Op::Delete {
            base: d.version_hash(),
            id: d.id.clone(),
        }],
    );
    assert!(matches!(
        s.commit_to_view("w", del),
        Err(StoreError::Frozen(_))
    ));
}

#[test]
fn landing_a_stale_view_fails_without_overwriting_canon() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let v1 = block("b-AAAAA", "claim", "one");
    s.commit_to_view("a", change("a", vec![Op::Create { block: v1.clone() }]))
        .unwrap();
    let p = s.propose("a", "seed", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
    // two views both edit v1
    s.commit_to_view(
        "x",
        change(
            "x",
            vec![Op::Update {
                base: v1.version_hash(),
                block: block("b-AAAAA", "claim", "x"),
            }],
        ),
    )
    .unwrap();
    s.commit_to_view(
        "y",
        change(
            "y",
            vec![Op::Update {
                base: v1.version_hash(),
                block: block("b-AAAAA", "claim", "y"),
            }],
        ),
    )
    .unwrap();
    let px = s.propose("x", "x", prov(Role::Writer)).unwrap();
    s.land(&px, prov(Role::Approver)).unwrap();
    let py = s.propose("y", "y", prov(Role::Writer)).unwrap();
    let head = s.canon_head().unwrap();
    assert!(matches!(
        s.land(&py, prov(Role::Approver)),
        Err(StoreError::StaleBase { .. })
    ));
    assert_eq!(s.canon_head().unwrap(), head);
}

#[test]
fn history_and_as_of_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (first, b2) = {
        let mut s = Store::create(dir.path()).unwrap();
        let b1 = block("b-AAAAA", "claim", "one");
        s.commit_to_view("w", change("w", vec![Op::Create { block: b1 }]))
            .unwrap();
        let p1 = s.propose("w", "one", prov(Role::Writer)).unwrap();
        let first = s.land(&p1, prov(Role::Approver)).unwrap();
        let b2 = block("b-BBBBB", "claim", "two");
        s.commit_to_view("w", change("w", vec![Op::Create { block: b2.clone() }]))
            .unwrap();
        let p2 = s.propose("w", "two", prov(Role::Writer)).unwrap();
        s.land(&p2, prov(Role::Approver)).unwrap();
        (first, b2)
    };
    let s = Store::create(dir.path()).unwrap();
    let hist = s.landings().unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[1].0, first);
    assert!(!s.snapshot_at(&first).unwrap().blocks.contains_key(&b2.id));
    let canon = s.blocks(&s.canon_state().unwrap()).unwrap();
    assert_eq!(canon[&b2.id], b2);
}

#[test]
fn commit_with_known_state_checks_against_that_state_without_replaying() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let v1 = block("b-AAAAA", "claim", "one");
    let v2 = block("b-AAAAA", "claim", "two");
    s.commit_to_view("w", change("w", vec![Op::Create { block: v1.clone() }]))
        .unwrap();
    let state = s.view_state("w").unwrap();

    // a duplicate create and a stale update are refused against the supplied state
    assert!(matches!(
        s.commit_to_view_at(
            "w",
            change("w", vec![Op::Create { block: v1.clone() }]),
            &state
        ),
        Err(StoreError::AlreadyExists(_))
    ));
    let stale = change(
        "w",
        vec![Op::Update {
            base: v2.version_hash(),
            block: block("b-AAAAA", "claim", "three"),
        }],
    );
    assert!(matches!(
        s.commit_to_view_at("w", stale, &state),
        Err(StoreError::StaleBase { .. })
    ));
    assert_eq!(s.view("w").unwrap().changes.len(), 1, "nothing written");

    // a valid change lands and the view replays to the same state the caller would compute
    s.commit_to_view_at(
        "w",
        change(
            "w",
            vec![Op::Update {
                base: v1.version_hash(),
                block: v2.clone(),
            }],
        ),
        &state,
    )
    .unwrap();
    assert_eq!(s.view_state("w").unwrap().blocks[&v2.id], v2.version_hash());
    assert_eq!(s.view("w").unwrap().changes.len(), 2);
}
