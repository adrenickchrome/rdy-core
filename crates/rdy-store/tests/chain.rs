//! View chains (plan M2a1, chain1).
use rdy_model::*;
use rdy_store::{Owner, Store, StoreError, ViewHead, ViewOp, ViewOpKind, ViewRecord, kinds};
use std::collections::BTreeMap;

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create(&dir.path().join("s")).unwrap();
    (dir, s)
}

/// Writes one op of `view` after `prev`; returns its id.
fn put_op(s: &mut Store, view: &str, prev: Option<Hash>, op: ViewOpKind) -> Hash {
    let o = ViewOp {
        view: view.into(),
        prev,
        op,
        actor: "someone".into(),
        run: None,
        at_ms: 5,
    };
    s.write(vec![(kinds::VIEW_OP, postcard::to_allocvec(&o).unwrap())])
        .unwrap()[0]
}

/// Points `view/<view>` at `head`.
fn set_head(s: &mut Store, view: &str, head: Hash, len: u64) {
    let v = ViewHead {
        head,
        base: None,
        len,
        owner: None,
        pinned: false,
        closed: false,
        last_write_ms: 5,
    };
    s.set_refs(vec![(
        format!("view/{view}"),
        postcard::to_allocvec(&v).unwrap(),
    )])
    .unwrap();
}

/// Writes `ops` as one chain of `view`, oldest first, and points its ref at the last; returns the
/// op ids, oldest first.
fn chain(s: &mut Store, view: &str, ops: Vec<ViewOpKind>) -> Vec<Hash> {
    let mut ids: Vec<Hash> = vec![];
    for op in ops {
        let id = put_op(s, view, ids.last().copied(), op);
        ids.push(id);
    }
    set_head(s, view, *ids.last().unwrap(), 0);
    ids
}

#[test]
fn chain1_the_walk_applies_the_ops_after_the_latest_checkpoint() {
    let (_d, mut s) = store();
    let fork = ViewOpKind::Fork {
        from: "other".into(),
        from_head: h(9),
        base: None,
        changes: vec![h(2), h(3)],
        owner: None,
    };
    let ids = chain(
        &mut s,
        "hand",
        vec![
            ViewOpKind::Create {
                base: None,
                owner: Some(Owner {
                    actor: "agent:a".into(),
                    run: None,
                }),
                pinned: false,
            },
            ViewOpKind::Commit { change: h(1) },
            fork.clone(),
            ViewOpKind::Commit { change: h(4) },
            ViewOpKind::Include {
                changes: vec![h(5), h(4)],
            },
            ViewOpKind::Exclude {
                changes: vec![h(2)],
            },
            ViewOpKind::Own { owner: None },
            ViewOpKind::Close {
                reason: "done".into(),
            },
            ViewOpKind::Commit { change: h(6) },
        ],
    );
    assert_eq!(
        s.included("hand").unwrap().as_slice(),
        &[h(3), h(4), h(5), h(6)]
    );
    assert_eq!(s.view_head("hand").unwrap().unwrap().head, ids[8]);
    assert_eq!(s.view_op(&ids[2]).unwrap().op, fork);
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(s.view_op_ids("hand").unwrap(), sorted);
    assert!(s.included("nobody").unwrap().is_empty());
    assert_eq!(s.view_head("nobody").unwrap(), None);
}

#[test]
fn chain1_the_walk_reads_back_only_to_the_checkpoint_then_only_to_the_cached_head() {
    let (_d, mut s) = store();
    let mut ops = vec![ViewOpKind::Create {
        base: None,
        owner: None,
        pinned: false,
    }];
    ops.extend((10..30).map(|n| ViewOpKind::Commit { change: h(n) }));
    ops.push(ViewOpKind::Rebase {
        landing: h(99),
        dropped: (10..30).map(h).collect(),
        remaining: vec![h(50)],
        conflicts: vec![],
    });
    ops.extend([
        ViewOpKind::Commit { change: h(51) },
        ViewOpKind::Commit { change: h(52) },
    ]);
    let ids = chain(&mut s, "hand", ops);
    let before = s.frames_read();
    assert_eq!(
        s.included("hand").unwrap().as_slice(),
        &[h(50), h(51), h(52)]
    );
    assert_eq!(s.frames_read() - before, 3, "two commits and the rebase");
    let before = s.frames_read();
    s.included("hand").unwrap();
    assert_eq!(s.frames_read(), before, "a cached set reads nothing");
    let next = put_op(
        &mut s,
        "hand",
        Some(*ids.last().unwrap()),
        ViewOpKind::Commit { change: h(53) },
    );
    set_head(&mut s, "hand", next, 4);
    let before = s.frames_read();
    assert_eq!(
        s.included("hand").unwrap().as_slice(),
        &[h(50), h(51), h(52), h(53)]
    );
    assert_eq!(
        s.frames_read() - before,
        1,
        "the walk stops at the cached head"
    );
}

#[test]
fn chain1_a_broken_chain_names_the_view_the_op_and_why() {
    let (_d, mut s) = store();
    let b = chain(
        &mut s,
        "b",
        vec![ViewOpKind::Create {
            base: None,
            owner: None,
            pinned: false,
        }],
    );
    let a = put_op(&mut s, "a", Some(b[0]), ViewOpKind::Commit { change: h(1) });
    set_head(&mut s, "a", a, 1);
    set_head(&mut s, "c", h(7), 0);
    let d = chain(&mut s, "d", vec![ViewOpKind::Commit { change: h(1) }]);
    let e = chain(
        &mut s,
        "e",
        vec![
            ViewOpKind::Create {
                base: None,
                owner: None,
                pinned: false,
            },
            ViewOpKind::Undo { target: h(1) },
        ],
    );
    for (view, at, why) in [
        ("a", b[0], "the op belongs to view \"b\""),
        ("c", h(7), "the op is missing"),
        ("d", d[0], "it ends without a checkpoint"),
    ] {
        match s.included(view) {
            Err(StoreError::Decode(m)) => assert_eq!(
                m,
                format!("view chain of {view:?} is broken at {at}: {why}")
            ),
            other => panic!("{view}: {other:?}"),
        }
    }
    s.set_refs(vec![("view/f".into(), b"junk".to_vec())])
        .unwrap();
    match s.view_head("f") {
        Err(StoreError::Decode(m)) => assert!(
            m.starts_with("view \"f\": its ref is not a view head: "),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    // vop1: the walk resolves an undo through its target, so an undo of a missing op fails there
    assert!(
        matches!(s.included("e"), Err(StoreError::NotFound(t)) if t == h(1)),
        "{:?} at {}",
        s.included("e"),
        e[1]
    );
}

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
        read_identity: None,
    }
}

fn create(view: &str, id: &str) -> Change {
    change(
        view,
        vec![Op::Create {
            block: block(id, "same length body"),
        }],
    )
}

#[test]
fn chain1_the_first_write_creates_the_view_and_each_write_appends_one_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let c1 = s.commit_to_view("work", create("work", "b-00001")).unwrap();
    let h1 = s.view_head("work").unwrap().unwrap();
    let commit1 = s.view_op(&h1.head).unwrap();
    assert_eq!(commit1.op, ViewOpKind::Commit { change: c1 });
    assert_eq!(
        (commit1.view.as_str(), commit1.actor.as_str()),
        ("work", "someone")
    );
    let first = s.view_op(&commit1.prev.unwrap()).unwrap();
    assert_eq!(
        first.op,
        ViewOpKind::Create {
            base: None,
            owner: Some(Owner {
                actor: "someone".into(),
                run: None
            }),
            pinned: false
        }
    );
    assert_eq!(first.prev, None);
    assert_eq!(
        (h1.len, h1.base, h1.owner.clone(), h1.pinned, h1.closed),
        (
            1,
            None,
            Some(Owner {
                actor: "someone".into(),
                run: None
            }),
            false,
            false
        )
    );
    let c2 = s.commit_to_view("work", create("work", "b-00002")).unwrap();
    let h2 = s.view_head("work").unwrap().unwrap();
    assert_eq!(s.view_op(&h2.head).unwrap().prev, Some(h1.head));
    assert_eq!(h2.len, 2);
    assert_eq!(
        s.view("work").unwrap(),
        ViewRecord {
            base: None,
            changes: vec![c1, c2]
        }
    );
    assert_eq!(s.included("work").unwrap().as_slice(), &[c1, c2]);
    assert_eq!(s.view_op_ids("work").unwrap().len(), 3);
}

/// The 1.6 GB growth item: a write appends a constant number of bytes, however long the view.
#[test]
fn chain1_bytes_written_per_commit_do_not_grow_with_the_view() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let mut sizes = vec![];
    for i in 0..300 {
        let before = s.log_end();
        s.commit_to_view("work", create("work", &format!("b-{i:05}")))
            .unwrap();
        let after = s.log_end();
        if after.segment == before.segment {
            sizes.push(after.offset - before.offset);
        }
    }
    let (early, last) = (sizes[5], *sizes.last().unwrap());
    assert!(
        last <= early + 16,
        "{early} bytes per write early, {last} at 300 changes"
    );
}

#[test]
fn chain1_landing_appends_a_rebase_checkpoint_with_the_remaining_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let c1 = s.commit_to_view("work", create("work", "b-00001")).unwrap();
    let p = s.propose("work", "one", prov(Role::Writer)).unwrap();
    let c2 = s.commit_to_view("work", create("work", "b-00002")).unwrap();
    let l = s.land(&p, prov(Role::Approver)).unwrap();
    let head = s.view_head("work").unwrap().unwrap();
    assert_eq!(
        s.view_op(&head.head).unwrap().op,
        ViewOpKind::Rebase {
            landing: l,
            dropped: vec![c1],
            remaining: vec![c2],
            conflicts: vec![],
        }
    );
    assert_eq!((head.base, head.len), (Some(l), 1));
    assert_eq!(
        s.view("work").unwrap(),
        ViewRecord {
            base: Some(l),
            changes: vec![c2]
        }
    );
}

#[test]
fn chain1_the_walk_stops_at_the_latest_checkpoint() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let mut s = Store::create(&dir).unwrap();
        for i in 0..50 {
            s.commit_to_view("work", create("work", &format!("b-{i:05}")))
                .unwrap();
        }
        let p = s.propose("work", "all", prov(Role::Writer)).unwrap();
        s.land(&p, prov(Role::Approver)).unwrap();
        for i in 50..52 {
            s.commit_to_view("work", create("work", &format!("b-{i:05}")))
                .unwrap();
        }
    }
    let s = Store::create(&dir).unwrap();
    let before = s.frames_read();
    assert_eq!(s.included("work").unwrap().len(), 2);
    assert!(
        s.frames_read() - before <= 3,
        "read {} ops",
        s.frames_read() - before
    );
}

#[test]
fn chain1_a_broken_chain_is_an_error_naming_the_view() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let mut s = Store::create(&dir).unwrap();
        s.commit_to_view("a", create("a", "b-00001")).unwrap();
        s.commit_to_view("b", create("b", "b-00002")).unwrap();
        let hb = s.view_head("b").unwrap().unwrap();
        let mut ha = s.view_head("a").unwrap().unwrap();
        ha.head = hb.head;
        s.set_refs(vec![("view/a".into(), postcard::to_allocvec(&ha).unwrap())])
            .unwrap();
    }
    let s = Store::create(&dir).unwrap();
    match s.view("a") {
        Err(StoreError::Decode(m)) => assert!(
            m.contains("view chain of \"a\" is broken at") && m.contains("belongs to view \"b\""),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn sess2_an_implicit_create_records_the_changes_actor_and_run_as_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let mut c = create("v", "b-00001");
    c.provenance.actor = "agent:w".into();
    c.provenance.run = Some("r1".into());
    s.commit_to_view("v", c).unwrap();
    let owner = Owner {
        actor: "agent:w".into(),
        run: Some("r1".into()),
    };
    let h = s.view_head("v").unwrap().unwrap();
    assert_eq!(h.owner, Some(owner.clone()));
    let commit = s.view_op(&h.head).unwrap();
    let first = s.view_op(&commit.prev.unwrap()).unwrap();
    assert_eq!(
        first.op,
        ViewOpKind::Create {
            base: None,
            owner: Some(owner.clone()),
            pinned: false
        }
    );
    // a later write by someone else does not take the view
    s.commit_to_view("v", create("v", "b-00002")).unwrap();
    assert_eq!(s.view_head("v").unwrap().unwrap().owner, Some(owner));
}
