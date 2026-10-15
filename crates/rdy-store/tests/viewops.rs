//! vop1: view ops are appended one per operation; the walk resolves Undo; Rebase records conflicts.
use rdy_model::*;
use rdy_store::merge::{Conflict, Side};
use rdy_store::view::{Owner, ViewOpKind};
use rdy_store::{Store, StoreError};
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

fn owner(a: &str) -> Owner {
    Owner {
        actor: a.into(),
        run: Some("r".into()),
    }
}

#[test]
fn vop1_the_store_format_is_11() {
    assert_eq!(rdy_store::format::STORE_FORMAT, 11);
}

#[test]
fn vop1_create_fork_and_append_write_one_op_each_and_update_the_head() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let h0 = s
        .view_create("a", None, Some(owner("agent:a")), true, &by)
        .unwrap();
    assert_eq!(
        (h0.len, h0.pinned, h0.closed, h0.owner.clone()),
        (0, true, false, Some(owner("agent:a")))
    );
    let op = s.view_op(&h0.head).unwrap();
    assert_eq!(
        op.op,
        ViewOpKind::Create {
            base: None,
            owner: Some(owner("agent:a")),
            pinned: true
        }
    );
    assert_eq!((op.prev, op.actor.as_str(), op.at_ms), (None, "agent:a", 7));
    let c1 = create(&mut s, "a", "c-00001", &by);
    let c2 = create(&mut s, "a", "c-00002", &by);
    let hf = s
        .view_fork("a", "b", Some(owner("agent:b")), &prov("agent:b"))
        .unwrap();
    assert_eq!(
        (hf.len, hf.pinned, hf.closed, hf.base, hf.owner.clone()),
        (2, true, false, None, Some(owner("agent:b")))
    );
    let fork = s.view_op(&hf.head).unwrap();
    let src = s.view_head("a").unwrap().unwrap();
    assert_eq!(
        fork.op,
        ViewOpKind::Fork {
            from: "a".into(),
            from_head: src.head,
            base: None,
            changes: vec![c1, c2],
            owner: Some(owner("agent:b"))
        }
    );
    assert_eq!(fork.prev, None);
    assert_eq!(s.included("b").unwrap().as_ref(), &vec![c1, c2]);
    let h1 = s
        .view_append("b", vec![ViewOpKind::Exclude { changes: vec![c1] }], &by)
        .unwrap();
    assert_eq!((h1.len, h1.last_write_ms), (1, 7));
    assert_eq!(s.view_op(&h1.head).unwrap().prev, Some(hf.head));
    assert_eq!(s.included("b").unwrap().as_ref(), &vec![c2]);
    let h2 = s
        .view_append(
            "b",
            vec![
                ViewOpKind::Close {
                    reason: "done".into(),
                },
                ViewOpKind::Own { owner: None },
            ],
            &prov("human:h"),
        )
        .unwrap();
    assert!(h2.closed && h2.owner.is_none(), "{h2:?}");
    assert_eq!(h2.last_write_ms, 7, "close and own are not writes");
    let close = s
        .view_op(&s.view_op(&h2.head).unwrap().prev.unwrap())
        .unwrap();
    assert_eq!(
        close.op,
        ViewOpKind::Close {
            reason: "done".into()
        }
    );
    assert_eq!(
        s.view_head("a").unwrap().unwrap().len,
        2,
        "the source is untouched by the fork"
    );
}

#[test]
fn vop1_the_walk_resolves_undo_of_commit_include_exclude_and_undo() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let c2 = create(&mut s, "a", "c-00002", &by);
    let commit2 = s.view_head("a").unwrap().unwrap().head;
    let h = s
        .view_append("a", vec![ViewOpKind::Undo { target: commit2 }], &by)
        .unwrap();
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c1],
        "undo of a commit excludes its change"
    );
    assert_eq!(h.len, 1);
    let undo = h.head;
    s.view_append("a", vec![ViewOpKind::Undo { target: undo }], &by)
        .unwrap();
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c1, c2],
        "undo of the undo includes it again"
    );
    let inc = s
        .view_append("a", vec![ViewOpKind::Exclude { changes: vec![c1] }], &by)
        .unwrap()
        .head;
    s.view_append("a", vec![ViewOpKind::Undo { target: inc }], &by)
        .unwrap();
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c2, c1],
        "undo of an exclude includes at the end"
    );
    let c3 = create(&mut s, "b", "c-00003", &by);
    let inc3 = s
        .view_append("a", vec![ViewOpKind::Include { changes: vec![c3] }], &by)
        .unwrap()
        .head;
    s.view_append("a", vec![ViewOpKind::Undo { target: inc3 }], &by)
        .unwrap();
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![c2, c1]);
    // reopen the store: the walk, not the cache, gives the same set
    let dir = s.dir().to_path_buf();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![c2, c1]);
    assert_eq!(s.check_objects().unwrap().views, 2);
}

#[test]
fn vop1_undo_of_close_reopens_and_undo_of_own_restores_the_previous_owner() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    s.view_create("a", None, None, false, &by).unwrap();
    create(&mut s, "a", "c-00001", &by);
    let h = s
        .view_append(
            "a",
            vec![ViewOpKind::Own {
                owner: Some(owner("agent:a")),
            }],
            &by,
        )
        .unwrap();
    let own = h.head;
    let h = s
        .view_append(
            "a",
            vec![ViewOpKind::Own {
                owner: Some(owner("agent:b")),
            }],
            &by,
        )
        .unwrap();
    let own2 = h.head;
    let h = s
        .view_append("a", vec![ViewOpKind::Undo { target: own2 }], &by)
        .unwrap();
    assert_eq!(
        h.owner,
        Some(owner("agent:a")),
        "the owner before the undone op"
    );
    let h = s
        .view_append("a", vec![ViewOpKind::Undo { target: own }], &by)
        .unwrap();
    assert_eq!(h.owner, None, "the owner before the first own was none");
    let h = s
        .view_append("a", vec![ViewOpKind::Close { reason: "x".into() }], &by)
        .unwrap();
    let close = h.head;
    assert!(h.closed);
    let h = s
        .view_append("a", vec![ViewOpKind::Undo { target: close }], &by)
        .unwrap();
    assert!(!h.closed, "undo of close reopens");
    let create_op = {
        let mut cur = Some(h.head);
        let mut last = None;
        while let Some(x) = cur {
            let op = s.view_op(&x).unwrap();
            cur = op.prev;
            last = Some(x);
        }
        last.unwrap()
    };
    let h = s
        .view_append("a", vec![ViewOpKind::Undo { target: create_op }], &by)
        .unwrap();
    assert!(h.closed, "undo of create closes");
}

#[test]
fn vop1_a_rebase_op_carries_its_conflicts_and_is_a_checkpoint() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let c2 = create(&mut s, "a", "c-00002", &by);
    let conflict = Conflict {
        id: Hash::of(b"x"),
        block: BlockId::parse("c-00001").unwrap(),
        base: Some(Hash::of(b"v1")),
        sides: vec![
            Side {
                change: Some(c1),
                version: Some(Hash::of(b"v2")),
            },
            Side {
                change: None,
                version: Some(Hash::of(b"v3")),
            },
        ],
    };
    let landing = Hash::of(b"landing");
    let h = s
        .view_append(
            "a",
            vec![ViewOpKind::Rebase {
                landing,
                dropped: vec![c2],
                remaining: vec![c1],
                conflicts: vec![conflict.clone()],
            }],
            &by,
        )
        .unwrap();
    assert_eq!((h.base, h.len), (Some(landing), 1));
    let op = s.view_op(&h.head).unwrap();
    match op.op {
        ViewOpKind::Rebase {
            conflicts,
            remaining,
            ..
        } => {
            assert_eq!(conflicts, vec![conflict]);
            assert_eq!(remaining, vec![c1]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c1],
        "the walk stops at the checkpoint"
    );
    let e = s
        .view_append("a", vec![ViewOpKind::Undo { target: h.head }], &by)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("cannot be undone")),
        "{e}"
    );
    assert_eq!(
        s.view_head("a").unwrap().unwrap().head,
        h.head,
        "nothing written"
    );
}

#[test]
fn vop1_preconditions_create_exists_append_unknown_target_off_chain() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    create(&mut s, "a", "c-00001", &by);
    let e = s.view_create("a", None, None, false, &by).unwrap_err();
    assert!(e.to_string().contains("exists"), "{e}");
    let e = s.view_fork("nope", "b", None, &by).unwrap_err();
    assert!(
        matches!(e, StoreError::UnknownView(ref v) if v == "nope"),
        "{e}"
    );
    let e = s
        .view_append("nope", vec![ViewOpKind::Close { reason: "x".into() }], &by)
        .unwrap_err();
    assert!(matches!(e, StoreError::UnknownView(_)), "{e}");
    create(&mut s, "b", "c-00002", &by);
    let b_head = s.view_head("b").unwrap().unwrap().head;
    let e = s
        .view_append("a", vec![ViewOpKind::Undo { target: b_head }], &by)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("belongs to view \"b\"")),
        "{e}"
    );
    let e = s
        .view_append(
            "a",
            vec![ViewOpKind::Commit {
                change: Hash::of(b"c"),
            }],
            &by,
        )
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("commit path")),
        "{e}"
    );
    let e = s.view_append("a", vec![], &by).unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("no ops")),
        "{e}"
    );
}

/// Review Focus 5: an empty unpinned view read as canon; its first include must move its base.
#[test]
fn vop1_an_include_into_an_empty_unpinned_view_first_moves_its_base_to_canon() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let l0 = s.canon_head().unwrap();
    s.view_create("f", l0, Some(owner("agent:a")), false, &by)
        .unwrap();
    let pinned = s
        .view_create("p", l0, Some(owner("agent:a")), true, &by)
        .unwrap();
    create(&mut s, "seed", "c-00001", &by);
    let p = s.propose("seed", "t", prov("human:h")).unwrap();
    let l1 = s.land(&p, prov("human:h")).unwrap();
    assert_eq!(
        s.view_head("f").unwrap().unwrap().base,
        l0,
        "an empty view gets no op at a landing"
    );
    let c2 = create(&mut s, "b", "c-00002", &by);
    let h = s
        .view_append("f", vec![ViewOpKind::Include { changes: vec![c2] }], &by)
        .unwrap();
    assert_eq!((h.base, h.len), (Some(l1), 1));
    let include = s.view_op(&h.head).unwrap();
    let catch_up = s.view_op(&include.prev.unwrap()).unwrap();
    assert_eq!(
        catch_up.op,
        ViewOpKind::Rebase {
            landing: l1,
            dropped: vec![],
            remaining: vec![],
            conflicts: vec![]
        }
    );
    assert_eq!(s.included("f").unwrap().as_ref(), &vec![c2]);
    let hp = s
        .view_append("p", vec![ViewOpKind::Include { changes: vec![c2] }], &by)
        .unwrap();
    assert_eq!(hp.base, l0, "a pinned view keeps its base");
    assert_eq!(
        s.view_op(&hp.head).unwrap().prev,
        Some(pinned.head),
        "and gets no catch-up op"
    );
    let hf = s
        .view_append("f", vec![ViewOpKind::Close { reason: "x".into() }], &by)
        .unwrap();
    assert_eq!(
        s.view_op(&hf.head).unwrap().prev,
        Some(h.head),
        "a non-empty view needs no catch-up"
    );
}

/// A fork never starts behind canon: an unpinned, non-empty fork of a view whose base is behind
/// canon (here a closed one) is rebased in the same write; a pinned fork keeps its base.
#[test]
fn vop1_a_fork_of_a_view_behind_canon_is_rebased_onto_canon_in_the_same_write() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let c3 = create(&mut s, "a", "c-00003", &by);
    let c2 = create(&mut s, "seed", "c-00002", &by);
    s.commit_to_view(
        "seed",
        change(
            "seed",
            vec![Op::Create {
                block: block("c-00003", "other"),
            }],
            &by,
        ),
    )
    .unwrap();
    s.view_append("a", vec![ViewOpKind::Include { changes: vec![c2] }], &by)
        .unwrap();
    // closed before the landing, so the landing leaves it behind canon (reb1 skips closed views)
    s.view_append("a", vec![ViewOpKind::Close { reason: "x".into() }], &by)
        .unwrap();
    let p = s.propose("seed", "t", prov("human:h")).unwrap();
    let l1 = s.land(&p, prov("human:h")).unwrap();
    assert_eq!(
        s.view_head("a").unwrap().unwrap().base,
        None,
        "a closed view is not rebased"
    );
    let writes = s.write_count();
    let h = s
        .view_fork("a", "b", Some(owner("agent:b")), &prov("agent:b"))
        .unwrap();
    assert_eq!(s.write_count() - writes, 1, "one write");
    assert_eq!((h.base, h.len, h.closed), (Some(l1), 2, false));
    let rebase = s.view_op(&h.head).unwrap();
    assert_eq!(rebase.actor, "agent:b");
    let ViewOpKind::Rebase {
        landing,
        dropped,
        remaining,
        conflicts,
    } = rebase.op
    else {
        panic!("{:?}", rebase.op)
    };
    assert_eq!(
        (landing, dropped, remaining.clone()),
        (l1, vec![c2], vec![c1, c3])
    );
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0].block, BlockId::parse("c-00003").unwrap());
    assert!(matches!(
        s.view_op(&rebase.prev.unwrap()).unwrap().op,
        ViewOpKind::Fork { .. }
    ));
    assert_eq!(s.included("b").unwrap().as_ref(), &remaining);
    assert_eq!(s.check_objects().unwrap().views, 3);
    // a pinned source forks pinned, on its own base, with no rebase
    s.view_create("p", None, None, true, &by).unwrap();
    s.view_append("p", vec![ViewOpKind::Include { changes: vec![c1] }], &by)
        .unwrap();
    let hp = s.view_fork("p", "q", None, &by).unwrap();
    assert_eq!((hp.base, hp.len, hp.pinned), (None, 1, true));
    assert!(matches!(
        s.view_op(&hp.head).unwrap().op,
        ViewOpKind::Fork { .. }
    ));
}

/// The walk follows at most 64 undos of undos; an undo it could not resolve is never written.
#[test]
fn vop1_an_undo_chain_deeper_than_64_is_refused_before_it_is_written() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let mut target = s.view_head("a").unwrap().unwrap().head;
    for _ in 0..64 {
        target = s
            .view_append("a", vec![ViewOpKind::Undo { target }], &by)
            .unwrap()
            .head;
    }
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c1],
        "an even number of undos"
    );
    let e = s
        .view_append("a", vec![ViewOpKind::Undo { target }], &by)
        .unwrap_err();
    assert!(matches!(&e, StoreError::UndoTooDeep { .. }), "{e}");
    assert_eq!(
        s.view_head("a").unwrap().unwrap().head,
        target,
        "nothing written"
    );
    let dir = s.dir().to_path_buf();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c1],
        "the walk reads the whole chain"
    );
}

/// An undo of an undo of an `Own` restores the owner the undone undo replaced.
#[test]
fn vop1_undo_of_an_undo_of_own_sets_the_owner_again() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    s.view_create("a", None, Some(owner("agent:a")), false, &by)
        .unwrap();
    let own = s
        .view_append(
            "a",
            vec![ViewOpKind::Own {
                owner: Some(owner("agent:b")),
            }],
            &by,
        )
        .unwrap()
        .head;
    let u1 = s
        .view_append("a", vec![ViewOpKind::Undo { target: own }], &by)
        .unwrap();
    assert_eq!(u1.owner, Some(owner("agent:a")), "the create's owner");
    let u2 = s
        .view_append("a", vec![ViewOpKind::Undo { target: u1.head }], &by)
        .unwrap();
    assert_eq!(
        u2.owner,
        Some(owner("agent:b")),
        "the owner before the first undo"
    );
    assert_eq!(
        s.undo_effect("a", &u2.head).unwrap(),
        rdy_store::ResolvedOp::SetOwner(Some(owner("agent:a")))
    );
}

#[test]
fn msw20_included_before_reads_a_views_set_as_the_log_held_it_when_an_object_was_written() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let c1 = create(&mut s, "a", "c-00001", &by);
    let mark = s
        .put_alt_set(&AltSet {
            stable: None,
            prev: None,
            title: "T".into(),
            question: None,
            members: vec![],
            status: AltStatus::Open,
            provenance: by.clone(),
        })
        .unwrap();
    let c2 = create(&mut s, "a", "c-00002", &by);
    s.view_append("a", vec![ViewOpKind::Exclude { changes: vec![c1] }], &by)
        .unwrap();
    create(&mut s, "b", "c-00003", &by);
    assert_eq!(s.included("a").unwrap().as_ref(), &vec![c2]);
    assert_eq!(s.included_before("a", &mark).unwrap().as_ref(), &vec![c1]);
    assert_eq!(
        s.included("a").unwrap().as_ref(),
        &vec![c2],
        "the cache still serves the head"
    );
    assert!(
        s.included_before("b", &mark).unwrap().is_empty(),
        "b had no op then"
    );
    assert!(s.included_before("none", &mark).unwrap().is_empty());
}

fn author(actor: &str, run: Option<&str>, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: run.map(str::to_string),
        role: Role::Writer,
        at_ms,
    }
}

/// mfb3 fix round 1: `commit_many` commits several changes to one view in one write, all or
/// nothing. Each change keeps its author's provenance; every view op it writes (the implicit
/// create, the commits) and the head's last write are the committer's.
#[test]
fn mfb3_commit_many_is_one_write_with_the_committers_view_ops() {
    let (_d, mut s) = store();
    let a = author("agent:a", Some("r1"), 5);
    let b = author("agent:b", Some("r2"), 6);
    let imp = author("human:imp", None, 1000);
    let changes = || {
        vec![
            change(
                "v",
                vec![Op::Create {
                    block: block("c-00001", "one"),
                }],
                &a,
            ),
            change(
                "v",
                vec![Op::Create {
                    block: block("c-00002", "two"),
                }],
                &b,
            ),
        ]
    };
    // a failed write leaves nothing: no view, no change
    s.fail_next_index_apply();
    let e = s
        .commit_many("v", changes(), &|_| None, &[], None, &imp)
        .unwrap_err();
    assert!(matches!(e, StoreError::Db(_)), "{e}");
    assert!(s.view_head("v").unwrap().is_none());
    s.verify().unwrap();
    // the retry commits both
    let ids = s
        .commit_many("v", changes(), &|_| None, &[], None, &imp)
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(s.included("v").unwrap().as_ref(), &ids);
    assert_eq!(s.change(&ids[0]).unwrap().provenance, a);
    assert_eq!(s.change(&ids[1]).unwrap().provenance, b);
    let head = s.view_head("v").unwrap().unwrap();
    assert_eq!(
        (head.len, head.last_write_ms, head.owner.clone()),
        (
            2,
            1000,
            Some(Owner {
                actor: "human:imp".into(),
                run: None
            })
        )
    );
    let mut chain = vec![];
    let mut cur = Some(head.head);
    while let Some(h) = cur {
        let op = s.view_op(&h).unwrap();
        cur = op.prev;
        chain.push((h, op));
    }
    assert_eq!(chain.len(), 3, "create, commit, commit: {chain:?}");
    for (_, op) in &chain {
        assert_eq!(
            (op.actor.as_str(), op.run.as_deref(), op.at_ms),
            ("human:imp", None, 1000),
            "{op:?}"
        );
    }
    assert_eq!(chain[0].1.op, ViewOpKind::Commit { change: ids[1] });
    assert_eq!(chain[1].1.op, ViewOpKind::Commit { change: ids[0] });
    // the two commits are one write's ops in a row; two commits of two writes are not
    assert!(s.view_ops_in_a_row(&chain[1].0, &chain[0].0).unwrap());
    assert!(!s.view_ops_in_a_row(&chain[0].0, &chain[1].0).unwrap());
    let c3 = create(&mut s, "v", "c-00003", &a);
    let c4 = create(&mut s, "v", "c-00004", &a);
    let head = s.view_head("v").unwrap().unwrap();
    let last = s.view_op(&head.head).unwrap();
    assert_eq!(last.op, ViewOpKind::Commit { change: c4 });
    let before = last.prev.unwrap();
    assert_eq!(
        s.view_op(&before).unwrap().op,
        ViewOpKind::Commit { change: c3 }
    );
    assert!(!s.view_ops_in_a_row(&before, &head.head).unwrap());
    s.verify().unwrap();
}

/// mfb3 fix round 1: the changes of one `commit_many` write disjoint blocks; a block in two of
/// them is refused before anything is written.
#[test]
fn mfb3_commit_many_refuses_a_block_in_two_changes() {
    let (_d, mut s) = store();
    let a = author("agent:a", Some("r1"), 5);
    let imp = author("human:imp", None, 1000);
    let one = change(
        "v",
        vec![Op::Create {
            block: block("c-00001", "one"),
        }],
        &a,
    );
    let mut two = one.clone();
    two.message = "again".into();
    let e = s
        .commit_many("v", vec![one, two], &|_| None, &[], None, &imp)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Conflict { id } if id.as_str() == "c-00001"),
        "{e}"
    );
    assert!(s.view_head("v").unwrap().is_none());
}
