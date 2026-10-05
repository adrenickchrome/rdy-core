//! View chains (plan M2a1, chain1).
use rdy_model::Hash;
use rdy_store::{Owner, Store, StoreError, ViewHead, ViewOp, ViewOpKind, kinds};

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("s")).unwrap();
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
        ("e", e[1], "an undo op needs a newer rdy"),
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
}
