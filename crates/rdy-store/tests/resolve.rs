//! res1: a change that writes a conflicted block depends on every side and resolves it.
use rdy_model::*;
use rdy_store::merge::{MergeChange, merge, merge_on_blocks};
use rdy_store::view::{Owner, ViewOpKind};
use rdy_store::{Store, StoreError};
use std::collections::{BTreeMap, BTreeSet};

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
fn res1_a_write_from_the_conflict_base_depends_on_every_side_and_resolves() {
    let (_d, mut s, v1, ca, cb) = conflicted();
    let m = s.view_merge("a").unwrap();
    assert_eq!(m.conflicts.len(), 1, "{m:?}");
    assert_eq!(m.conflicts[0].base, Some(v1));
    assert_eq!(
        *m.state.blocks.get(&id("c-00001")).unwrap(),
        v1,
        "a conflicted block reads as its base"
    );
    let current = |b: &BlockId| m.state.blocks.get(b).copied();
    let fix = change(
        "a",
        vec![Op::Update {
            base: v1,
            block: block("c-00001", "merged"),
        }],
        &prov("agent:a"),
    );
    let cr = s
        .commit_to_view_resolving("a", fix, &current, &m.conflicts, None)
        .unwrap();
    let deps: BTreeSet<Hash> = s.change(&cr).unwrap().deps.into_iter().collect();
    assert!(deps.contains(&ca) && deps.contains(&cb), "{deps:?}");
    let after = s.view_merge("a").unwrap();
    assert!(after.conflicts.is_empty(), "{after:?}");
    assert_eq!(
        s.get_block(after.state.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "merged"
    );
}

#[test]
fn res1_a_write_from_a_sides_version_is_accepted_and_an_unrelated_base_is_stale() {
    let (_d, mut s, _v1, _ca, _cb) = conflicted();
    let m = s.view_merge("a").unwrap();
    let side = m.conflicts[0].sides.iter().find_map(|x| x.version).unwrap();
    let current = |b: &BlockId| m.state.blocks.get(b).copied();
    let fix = change(
        "a",
        vec![Op::Update {
            base: side,
            block: block("c-00001", "from a side"),
        }],
        &prov("agent:a"),
    );
    s.commit_to_view_resolving("a", fix, &current, &m.conflicts, None)
        .unwrap();
    assert!(s.view_merge("a").unwrap().conflicts.is_empty());
    // a second conflict never existed for c-00002; an unrelated base is stale as ever
    let (_d2, mut s2, _v1, _ca, _cb) = conflicted();
    let m2 = s2.view_merge("a").unwrap();
    let current2 = |b: &BlockId| m2.state.blocks.get(b).copied();
    let bad = change(
        "a",
        vec![Op::Update {
            base: Hash::of(b"nope"),
            block: block("c-00001", "x"),
        }],
        &prov("agent:a"),
    );
    let e = s2
        .commit_to_view_resolving("a", bad, &current2, &m2.conflicts, None)
        .unwrap_err();
    assert!(matches!(e, StoreError::StaleBase { .. }), "{e}");
}

#[test]
fn res1_without_the_conflict_list_a_side_version_base_is_stale() {
    let (_d, mut s, _v1, _ca, _cb) = conflicted();
    let m = s.view_merge("a").unwrap();
    let side = m.conflicts[0].sides.iter().find_map(|x| x.version).unwrap();
    let current = |b: &BlockId| m.state.blocks.get(b).copied();
    let fix = change(
        "a",
        vec![Op::Update {
            base: side,
            block: block("c-00001", "x"),
        }],
        &prov("agent:a"),
    );
    let e = s.commit_to_view_with("a", fix, &current).unwrap_err();
    assert!(
        matches!(e, StoreError::StaleBase { .. }),
        "strictness is opt-out, not default: {e}"
    );
}

#[test]
fn res1_a_commit_claims_an_unowned_view_and_a_closed_view_refuses() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    s.commit_to_view(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00001", "x"),
            }],
            &by,
        ),
    )
    .unwrap();
    // only an explicit Own { None } makes an unowned view (true before and after M2b's owner rule)
    s.view_append("v", vec![ViewOpKind::Own { owner: None }], &prov("human:h"))
        .unwrap();
    assert_eq!(s.view_head("v").unwrap().unwrap().owner, None);
    let o = Owner {
        actor: "agent:a".into(),
        run: Some("r".into()),
    };
    let st = s.view_state("v").unwrap();
    let current = |b: &BlockId| st.blocks.get(b).copied();
    s.commit_to_view_resolving(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00002", "y"),
            }],
            &by,
        ),
        &current,
        &[],
        Some(o.clone()),
    )
    .unwrap();
    let h = s.view_head("v").unwrap().unwrap();
    assert_eq!(h.owner, Some(o.clone()));
    let commit = s.view_op(&h.head).unwrap();
    let own = s.view_op(&commit.prev.unwrap()).unwrap();
    assert_eq!(
        own.op,
        ViewOpKind::Own {
            owner: Some(o.clone())
        },
        "the claim is an op before the commit"
    );
    let st2 = s.view_state("v").unwrap();
    let current2 = |b: &BlockId| st2.blocks.get(b).copied();
    s.commit_to_view_resolving(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00003", "z"),
            }],
            &by,
        ),
        &current2,
        &[],
        Some(o.clone()),
    )
    .unwrap();
    assert_eq!(s.view_head("v").unwrap().unwrap().len, 3);
    assert_eq!(
        s.check_objects().unwrap().view_ops,
        6,
        "create, commit, own(none), own(claim), commit, commit: no second claim"
    );
    s.view_append("v", vec![ViewOpKind::Close { reason: "x".into() }], &by)
        .unwrap();
    let e = s
        .commit_to_view(
            "v",
            change(
                "v",
                vec![Op::Create {
                    block: block("c-00004", "w"),
                }],
                &by,
            ),
        )
        .unwrap_err();
    assert!(e.to_string().contains("closed"), "{e}");
    let w = s
        .commit_to_view_resolving(
            "w",
            change(
                "w",
                vec![Op::Create {
                    block: block("c-00005", "q"),
                }],
                &by,
            ),
            &|_| None,
            &[],
            Some(o.clone()),
        )
        .unwrap();
    assert!(s.has_kind(rdy_store::kinds::CHANGE, &w).unwrap());
    assert_eq!(
        s.view_head("w").unwrap().unwrap().owner,
        Some(o),
        "an implicit create carries the claim"
    );
}

/// Review Focus 5: the write's base came from canon, so the batch moves the view's base first.
#[test]
fn res1_a_first_commit_into_an_empty_floating_view_moves_its_base_first() {
    let (_d, mut s) = store();
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
    let l1 = land(&mut s, "seed");
    s.view_create("f", Some(l1), None, false, &by).unwrap();
    let v1 = version(&s, "seed", "c-00001");
    s.commit_to_view(
        "seed",
        change(
            "seed",
            vec![Op::Update {
                base: v1,
                block: block("c-00001", "v2"),
            }],
            &by,
        ),
    )
    .unwrap();
    let l2 = land(&mut s, "seed");
    let canon = s.canon_state().unwrap();
    let v2 = *canon.blocks.get(&id("c-00001")).unwrap();
    let current = |b: &BlockId| canon.blocks.get(b).copied();
    let fix = change(
        "f",
        vec![Op::Update {
            base: v2,
            block: block("c-00001", "v3 in f"),
        }],
        &by,
    );
    s.commit_to_view_resolving("f", fix, &current, &[], None)
        .unwrap();
    let h = s.view_head("f").unwrap().unwrap();
    assert_eq!((h.base, h.len), (Some(l2), 1));
    let commit = s.view_op(&h.head).unwrap();
    assert!(
        matches!(s.view_op(&commit.prev.unwrap()).unwrap().op, ViewOpKind::Rebase { landing, .. } if landing == l2)
    );
    let m = s.view_merge("f").unwrap();
    assert!(
        m.conflicts.is_empty(),
        "no phantom conflict against the old base: {m:?}"
    );
    assert_eq!(
        s.get_block(m.state.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "v3 in f"
    );
}

#[test]
fn res1_merge_on_blocks_matches_the_full_merge_on_the_named_blocks() {
    let base = StateSnapshot::default();
    let mk = |i: u8, blk: &str, b: Option<Hash>| {
        let ops = vec![match b {
            None => Op::Create {
                block: block(blk, &format!("{i}")),
            },
            Some(h) => Op::Update {
                base: h,
                block: block(blk, &format!("{i}")),
            },
        }];
        (Hash::of(&[i]), ops)
    };
    let (i1, o1) = mk(1, "c-00001", None);
    let (i2, o2) = mk(2, "c-00001", None);
    let (i3, o3) = mk(3, "c-00002", None);
    let changes = vec![
        MergeChange {
            id: i1,
            deps: &[],
            ops: &o1,
            replayed_from: None,
        },
        MergeChange {
            id: i2,
            deps: &[],
            ops: &o2,
            replayed_from: None,
        },
        MergeChange {
            id: i3,
            deps: &[],
            ops: &o3,
            replayed_from: None,
        },
    ];
    let full = merge(&base, &changes, &|_| None, &|_| None);
    assert_eq!(
        full.conflicts.len(),
        1,
        "two creates of c-00001 conflict; c-00002 is clean"
    );
    let only1: BTreeSet<BlockId> = [id("c-00001")].into();
    let part = merge_on_blocks(&base, &changes, &|_| None, &|_| None, &only1);
    assert_eq!(part.conflicts, full.conflicts);
    assert_eq!(
        part.state.blocks.len(),
        0,
        "the conflicted block reads as its (absent) base; c-00002 is not in the result"
    );
    let only2: BTreeSet<BlockId> = [id("c-00002")].into();
    let part2 = merge_on_blocks(&base, &changes, &|_| None, &|_| None, &only2);
    assert!(part2.conflicts.is_empty());
    assert_eq!(
        part2.state.blocks.keys().collect::<Vec<_>>(),
        vec![&id("c-00002")]
    );
    assert_eq!(part2.order, full.order, "the order is the full order");
}

/// View a lands X (→ L1, a empty on L1, unpinned); view b updates X and creates Z (→ L2).
/// Returns (L1, L2, X's version at L1, X's version at L2).
fn emptied_behind_canon() -> (tempfile::TempDir, Store, Hash, Hash, Hash, Hash) {
    let (d, mut s) = store();
    let by = prov("agent:a");
    let x1 = Op::Create {
        block: block("c-00001", "x1"),
    };
    s.commit_to_view("a", change("a", vec![x1], &by)).unwrap();
    let l1 = land(&mut s, "a");
    let v1 = version(&s, "a", "c-00001");
    let ops = vec![
        Op::Update {
            base: v1,
            block: block("c-00001", "x2 by b"),
        },
        Op::Create {
            block: block("c-00002", "z by b"),
        },
    ];
    s.commit_to_view("b", change("b", ops, &prov("agent:b")))
        .unwrap();
    let l2 = land(&mut s, "b");
    let v2 = *s.canon_state().unwrap().blocks.get(&id("c-00001")).unwrap();
    let h = s.view_head("a").unwrap().unwrap();
    assert_eq!((h.base, h.len, h.pinned), (Some(l1), 0, false));
    (d, s, l1, l2, v1, v2)
}

/// Fix round 1: the catch-up moves the view onto canon, so a write checked against a `current`
/// that disagrees with canon on the blocks it writes was based on a state the view no longer
/// reads; it is refused and the view is left as it was.
#[test]
fn res1_a_write_checked_against_the_old_base_of_an_emptied_floating_view_is_stale() {
    let (_d, mut s, l1, _l2, v1, v2) = emptied_behind_canon();
    let by = prov("agent:a");
    let old = s.state_of(Some(l1)).unwrap();
    let current = |b: &BlockId| old.blocks.get(b).copied();
    let upd = Op::Update {
        base: v1,
        block: block("c-00001", "x3 by a"),
    };
    let e = s
        .commit_to_view_resolving("a", change("a", vec![upd], &by), &current, &[], None)
        .unwrap_err();
    match e {
        StoreError::StaleBase {
            id: i,
            expected,
            actual,
        } => {
            assert_eq!((i, expected, actual), (id("c-00001"), v1, Some(v2)));
        }
        e => panic!("expected StaleBase, got {e}"),
    }
    let mk = Op::Create {
        block: block("c-00002", "z by a"),
    };
    let e = s
        .commit_to_view_resolving("a", change("a", vec![mk], &by), &current, &[], None)
        .unwrap_err();
    assert!(
        matches!(e, StoreError::AlreadyExists(ref i) if *i == id("c-00002")),
        "{e}"
    );
    let h = s.view_head("a").unwrap().unwrap();
    assert_eq!(
        (h.base, h.len),
        (Some(l1), 0),
        "a refused write moves nothing"
    );
    // a block canon and the old base agree on passes: the check is per written block
    let ok = Op::Create {
        block: block("c-00003", "y"),
    };
    s.commit_to_view_resolving("a", change("a", vec![ok], &by), &current, &[], None)
        .unwrap();
    assert!(s.view_merge("a").unwrap().conflicts.is_empty());
}

/// Fix round 1: `commit_to_view` reads an empty unpinned view as canon (the floating read rule),
/// so it agrees with its own catch-up: a canon base is accepted and reads clean, the old base is
/// stale, and an id canon holds already exists.
#[test]
fn res1_commit_to_view_reads_an_emptied_floating_view_as_canon() {
    let (_d, mut s, _l1, l2, v1, v2) = emptied_behind_canon();
    let by = prov("agent:a");
    let stale = Op::Update {
        base: v1,
        block: block("c-00001", "x3 by a"),
    };
    let e = s
        .commit_to_view("a", change("a", vec![stale], &by))
        .unwrap_err();
    assert!(
        matches!(e, StoreError::StaleBase { expected, actual: Some(a), .. } if expected == v1 && a == v2),
        "{e}"
    );
    let dup = Op::Create {
        block: block("c-00002", "z by a"),
    };
    let e = s
        .commit_to_view("a", change("a", vec![dup], &by))
        .unwrap_err();
    assert!(matches!(e, StoreError::AlreadyExists(_)), "{e}");
    let upd = Op::Update {
        base: v2,
        block: block("c-00001", "x3 by a"),
    };
    s.commit_to_view("a", change("a", vec![upd], &by)).unwrap();
    let h = s.view_head("a").unwrap().unwrap();
    assert_eq!((h.base, h.len), (Some(l2), 1));
    let m = s.view_merge("a").unwrap();
    assert!(m.conflicts.is_empty(), "{m:?}");
    assert_eq!(
        s.get_block(m.state.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "x3 by a"
    );
    assert!(
        m.state.blocks.contains_key(&id("c-00002")),
        "the view carries canon's Z"
    );
    let p = s.propose("a", "t", prov("human:h")).unwrap();
    s.land(&p, prov("human:h")).unwrap();
}

/// Fix round 1: a conflict one of whose sides is a frozen decision cannot be resolved by a
/// write, from its base or from any side: the write would rewrite (or delete) an accepted
/// decision. Canon holds a proposed decision; a edits its statement, b accepts it; a includes b.
#[test]
fn res1_a_conflict_with_a_frozen_side_refuses_every_resolving_write() {
    let (_d, mut s) = store();
    let by = prov("agent:a");
    let decision = |status: &str, statement: &str| {
        let mut b = block("D-00001", "");
        b.block_type = decision::DECISION.into();
        b.fields
            .insert("status".into(), FieldValue::Str(status.into()));
        b.fields
            .insert("statement".into(), FieldValue::Str(statement.into()));
        b
    };
    let d0 = decision("proposed", "D");
    s.commit_to_view("seed", change("seed", vec![Op::Create { block: d0 }], &by))
        .unwrap();
    land(&mut s, "seed");
    let v0 = version(&s, "seed", "D-00001");
    let upd = |b: Block| Op::Update { base: v0, block: b };
    s.commit_to_view(
        "a",
        change("a", vec![upd(decision("proposed", "D by a"))], &by),
    )
    .unwrap();
    let cb = s
        .commit_to_view(
            "b",
            change("b", vec![upd(decision("accepted", "D"))], &prov("agent:b")),
        )
        .unwrap();
    s.view_append("a", vec![ViewOpKind::Include { changes: vec![cb] }], &by)
        .unwrap();
    let m = s.view_merge("a").unwrap();
    assert_eq!(m.conflicts.len(), 1, "{m:?}");
    let current = |b: &BlockId| m.state.blocks.get(b).copied();
    let mut bases = vec![v0];
    bases.extend(m.conflicts[0].sides.iter().filter_map(|x| x.version));
    for base in bases {
        for op in [
            Op::Update {
                base,
                block: decision("proposed", "rewritten"),
            },
            Op::Delete {
                base,
                id: id("D-00001"),
            },
        ] {
            let e = s
                .commit_to_view_resolving(
                    "a",
                    change("a", vec![op], &by),
                    &current,
                    &m.conflicts,
                    None,
                )
                .unwrap_err();
            assert!(matches!(e, StoreError::Frozen(_)), "base {base}: {e}");
        }
    }
    assert_eq!(
        s.view_merge("a").unwrap().conflicts.len(),
        1,
        "nothing written"
    );
}
