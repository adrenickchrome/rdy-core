//! ok7: the merge of a replayed change (`ChangeLinks::replayed_from` set) and the store's one
//! write of a prepared replay. An op whose result equals the block's version agrees with it,
//! whatever the bases and whoever the writers, replays included. A replay op whose base differs
//! from the block's version and whose result differs too conflicts. A replay op on a conflicted
//! block keeps every side and adds its own. Ordinary changes keep the existing rules. The
//! prepared write stores the changes with the deps the caller gave and one `Include` in one
//! batch.
use rdy_model::*;
use rdy_store::kinds;
use rdy_store::merge::{Conflict, MergeChange, Side, conflict_id, merge};
use rdy_store::view::ViewOpKind;
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
        read_identity: None,
    }
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(label: &str) -> Hash {
    Hash::of(label.as_bytes())
}

/// One change as the merge sees it, with owned deps and ops.
struct Owned {
    id: Hash,
    deps: Vec<Hash>,
    ops: Vec<Op>,
    from: Option<Hash>,
}

/// The source of every `owned(.., true)` replay: a change none of these merges holds.
const ELSEWHERE: &str = "a source elsewhere";

fn owned(label: &str, deps: Vec<Hash>, ops: Vec<Op>, replay: bool) -> Owned {
    Owned {
        id: h(label),
        deps,
        ops,
        from: replay.then(|| h(ELSEWHERE)),
    }
}

/// A replay of the change labelled `source`.
fn replay_of(label: &str, deps: Vec<Hash>, ops: Vec<Op>, source: &str) -> Owned {
    Owned {
        id: h(label),
        deps,
        ops,
        from: Some(h(source)),
    }
}

fn input(cs: &[Owned]) -> Vec<MergeChange<'_>> {
    cs.iter()
        .map(|c| MergeChange {
            id: c.id,
            deps: &c.deps,
            ops: &c.ops,
            is_replay: c.from.is_some(),
        })
        .collect()
}

const CANON: &str = "canon producer";

/// The base state (block b-00001 at `v0`, produced by CANON) and the merge of `cs` onto it.
fn merged(base: &StateSnapshot, cs: &[Owned]) -> rdy_store::merge::Merged {
    let producer = |v: &Hash| base.blocks.values().any(|x| x == v).then(|| h(CANON));
    merge(base, &input(cs), &producer, &|_| None)
}

/// The merge of `cs` onto `base`, each base version produced by the change labelled `by`.
fn merged_by(base: &StateSnapshot, cs: &[Owned], by: &str) -> rdy_store::merge::Merged {
    let producer = |v: &Hash| base.blocks.values().any(|x| x == v).then(|| h(by));
    merge(base, &input(cs), &producer, &|_| None)
}

fn conflict(block_: &BlockId, base: Option<Hash>, mut sides: Vec<Side>) -> Conflict {
    sides.sort();
    Conflict {
        id: conflict_id(block_, base, &sides),
        block: block_.clone(),
        base,
        sides,
    }
}

fn side(change: Option<Hash>, version: Option<Hash>) -> Side {
    Side { change, version }
}

/// Block b-00001 at the target with versions v0 (base), v1 (an earlier version a source read)
/// and v2 (a third body).
struct Fixture {
    b: BlockId,
    v0: Block,
    v1: Block,
    v2: Block,
    base: StateSnapshot,
}

fn fixture() -> Fixture {
    let b = bid("b-00001");
    let v0 = block("b-00001", "target");
    let v1 = block("b-00001", "source base");
    let v2 = block("b-00001", "source result");
    let mut base = StateSnapshot::default();
    base.blocks.insert(b.clone(), v0.version_hash());
    Fixture {
        b,
        v0,
        v1,
        v2,
        base,
    }
}

#[test]
fn ok7_replay_equal_result_on_another_base_agrees() {
    let f = fixture();
    // the source updated v1 to the body the target already holds
    let op = Op::Update {
        base: f.v1.version_hash(),
        block: f.v0.clone(),
    };
    // an ordinary change that reaches the same result agrees with the target (existing rule)
    let ordinary = merged(&f.base, &[owned("w", vec![], vec![op.clone()], false)]);
    assert!(ordinary.conflicts.is_empty(), "{:?}", ordinary.conflicts);
    // so does a replay of a source unrelated to the target's version: the op is already
    // satisfied (ruling task-7-round1)
    let replay = merged(&f.base, &[owned("r", vec![], vec![op.clone()], true)]);
    assert!(replay.conflicts.is_empty(), "{:?}", replay.conflicts);
    assert_eq!(replay.state.blocks.get(&f.b), Some(&f.v0.version_hash()));
    // and a replay of the version's own producer, landed
    let own = merged(&f.base, &[replay_of("r", vec![], vec![op], CANON)]);
    assert!(own.conflicts.is_empty(), "{:?}", own.conflicts);
    assert_eq!(own.state.blocks.get(&f.b), Some(&f.v0.version_hash()));
    // and a create of the id with the body the target holds
    let create = Op::Create {
        block: f.v0.clone(),
    };
    let same = merged(&f.base, &[owned("r", vec![], vec![create], true)]);
    assert!(same.conflicts.is_empty(), "{:?}", same.conflicts);
    // another base and another result is a divergence: the source base, the target's side and
    // the replay's own side
    let other = Op::Update {
        base: f.v1.version_hash(),
        block: f.v2.clone(),
    };
    let replay = merged(&f.base, &[owned("r", vec![], vec![other], true)]);
    let want = conflict(
        &f.b,
        Some(f.v1.version_hash()),
        vec![
            side(Some(h(CANON)), Some(f.v0.version_hash())),
            side(Some(h("r")), Some(f.v2.version_hash())),
        ],
    );
    assert_eq!(replay.conflicts, vec![want]);
    // the conflicted block reads as its conflict's base
    assert_eq!(replay.state.blocks.get(&f.b), Some(&f.v1.version_hash()));
}

#[test]
fn ok7_replay_on_the_target_base_applies() {
    let f = fixture();
    let op = Op::Update {
        base: f.v0.version_hash(),
        block: f.v2.clone(),
    };
    let m = merged(&f.base, &[owned("r", vec![], vec![op], true)]);
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    assert_eq!(m.state.blocks.get(&f.b), Some(&f.v2.version_hash()));
}

#[test]
fn ok7_replay_keeps_every_side_of_a_conflict() {
    let f = fixture();
    // r1 diverges from the target; r2 builds on r1 (a same-block dep)
    let r1_op = Op::Update {
        base: f.v1.version_hash(),
        block: f.v2.clone(),
    };
    let v3 = block("b-00001", "the second source result");
    let r2_op = Op::Update {
        base: f.v2.version_hash(),
        block: v3.clone(),
    };
    let replays = [
        owned("r1", vec![], vec![r1_op.clone()], true),
        owned("r2", vec![h("r1")], vec![r2_op.clone()], true),
    ];
    let m = merged(&f.base, &replays);
    let want = conflict(
        &f.b,
        Some(f.v1.version_hash()),
        vec![
            side(Some(h(CANON)), Some(f.v0.version_hash())),
            side(Some(h("r1")), Some(f.v2.version_hash())),
            side(Some(h("r2")), Some(v3.version_hash())),
        ],
    );
    assert_eq!(m.conflicts, vec![want]);
    // an ordinary change with the same dep drops the side it covers (existing rule)
    let ordinary = [
        owned("r1", vec![], vec![r1_op], true),
        owned("w2", vec![h("r1")], vec![r2_op], false),
    ];
    let m = merged(&f.base, &ordinary);
    let mut want = vec![
        side(Some(h(CANON)), Some(f.v0.version_hash())),
        side(Some(h("w2")), Some(v3.version_hash())),
    ];
    want.sort();
    assert_eq!(m.conflicts[0].sides, want);
}

#[test]
fn ok7_replay_never_resolves_a_conflict_its_deps_cover() {
    let f = fixture();
    // two ordinary changes conflict on b; a replay that depends on both still adds a side
    let w1 = Op::Update {
        base: f.v0.version_hash(),
        block: f.v1.clone(),
    };
    let w2 = Op::Update {
        base: f.v0.version_hash(),
        block: f.v2.clone(),
    };
    let v3 = block("b-00001", "replayed");
    let r = Op::Update {
        base: f.v1.version_hash(),
        block: v3.clone(),
    };
    let cs = [
        owned("w1", vec![h(CANON)], vec![w1.clone()], false),
        owned("w2", vec![h(CANON)], vec![w2.clone()], false),
        owned("r", vec![h("w1"), h("w2")], vec![r.clone()], true),
    ];
    let m = merged(&f.base, &cs);
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(
        m.conflicts[0]
            .sides
            .iter()
            .map(|s| s.change)
            .collect::<Vec<_>>(),
        {
            let mut v = vec![Some(h("w1")), Some(h("w2")), Some(h("r"))];
            v.sort();
            v
        }
    );
    assert_eq!(m.conflicts[0].base, Some(f.v0.version_hash()));
    // the same change as an ordinary write resolves it (existing rule)
    let cs = [
        owned("w1", vec![h(CANON)], vec![w1], false),
        owned("w2", vec![h(CANON)], vec![w2], false),
        owned("r", vec![h("w1"), h("w2")], vec![r], false),
    ];
    let m = merged(&f.base, &cs);
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    assert_eq!(m.state.blocks.get(&f.b), Some(&v3.version_hash()));
}

#[test]
fn ok7_replay_delete_and_create_cases_keep_base_and_sides() {
    let f = fixture();
    // a delete on another base: a null replay side beside the target's
    let del = Op::Delete {
        base: f.v1.version_hash(),
        id: f.b.clone(),
    };
    let m = merged(&f.base, &[owned("r", vec![], vec![del], true)]);
    assert_eq!(
        m.conflicts,
        vec![conflict(
            &f.b,
            Some(f.v1.version_hash()),
            vec![
                side(Some(h(CANON)), Some(f.v0.version_hash())),
                side(Some(h("r")), None),
            ],
        )]
    );
    // a create of an id the target holds, with another body: a collision
    let create = Op::Create {
        block: f.v2.clone(),
    };
    let m = merged(&f.base, &[owned("r", vec![], vec![create], true)]);
    assert_eq!(
        m.conflicts,
        vec![conflict(
            &f.b,
            None,
            vec![
                side(Some(h(CANON)), Some(f.v0.version_hash())),
                side(Some(h("r")), Some(f.v2.version_hash())),
            ],
        )]
    );
    // an update of a block the target lacks: a null target side
    let other = block("b-00002", "elsewhere");
    let upd = Op::Update {
        base: other.version_hash(),
        block: block("b-00002", "changed"),
    };
    let m = merged(&f.base, &[owned("r", vec![], vec![upd], true)]);
    assert_eq!(
        m.conflicts,
        vec![conflict(
            &bid("b-00002"),
            Some(other.version_hash()),
            vec![
                side(None, None),
                side(
                    Some(h("r")),
                    Some(block("b-00002", "changed").version_hash())
                ),
            ],
        )]
    );
}

#[test]
fn ok7_replay_and_ordinary_order_never_decides_a_divergence() {
    // a replay and an ordinary change create the same block with the same body, or update it
    // from one base to one result. Whichever the merge applies first (its order is by id), they
    // agree, whether or not the ordinary change is the replay's source (ruling task-7-round1)
    let base = StateSnapshot::default();
    let b = bid("b-00009");
    let v = block("b-00009", "same");
    let create = Op::Create { block: v.clone() };
    let mut seen = vec![];
    for (replay, ordinary) in [("first", "second"), ("second", "first")] {
        for cs in [
            // a replay of a source the merge does not hold
            [
                owned(replay, vec![], vec![create.clone()], true),
                owned(ordinary, vec![], vec![create.clone()], false),
            ],
            // the replay of the ordinary change itself
            [
                replay_of(replay, vec![], vec![create.clone()], ordinary),
                owned(ordinary, vec![], vec![create.clone()], false),
            ],
        ] {
            let m = merged(&base, &cs);
            assert!(m.conflicts.is_empty(), "replay {replay}: {:?}", m.conflicts);
            assert_eq!(m.state.blocks.get(&b), Some(&v.version_hash()));
        }
        seen.push(h(replay) < h(ordinary));
    }
    // both orders ran
    assert_ne!(seen[0], seen[1]);
    // the same for an update from one base to one result, in either order
    let f = fixture();
    let upd = Op::Update {
        base: f.v0.version_hash(),
        block: f.v2.clone(),
    };
    for (replay, ordinary) in [("first", "second"), ("second", "first")] {
        for cs in [
            [
                owned(replay, vec![h(CANON)], vec![upd.clone()], true),
                owned(ordinary, vec![h(CANON)], vec![upd.clone()], false),
            ],
            [
                replay_of(replay, vec![h(CANON)], vec![upd.clone()], ordinary),
                owned(ordinary, vec![h(CANON)], vec![upd.clone()], false),
            ],
        ] {
            let m = merged(&f.base, &cs);
            assert!(m.conflicts.is_empty(), "replay {replay}: {:?}", m.conflicts);
            assert_eq!(m.state.blocks.get(&f.b), Some(&f.v2.version_hash()));
        }
    }
    // two ordinary creates still agree (existing rule)
    let cs = [
        owned("first", vec![], vec![create.clone()], false),
        owned("second", vec![], vec![create], false),
    ];
    assert!(merged(&base, &cs).conflicts.is_empty());
}

#[test]
fn ok7_two_replays_of_one_source_agree() {
    // source S updates b from v0 to v2, and is replayed as r1 into one view and r2 into
    // another (ruling task-7-round1, case a)
    let f = fixture();
    let upd = Op::Update {
        base: f.v0.version_hash(),
        block: f.v2.clone(),
    };
    // in flight, both in one merge, in either id order: they agree
    let mut seen = vec![];
    for (r1, r2) in [("r1", "r2"), ("r2", "r1")] {
        let cs = [
            replay_of(r1, vec![h(CANON)], vec![upd.clone()], "S"),
            replay_of(r2, vec![h(CANON)], vec![upd.clone()], "S"),
        ];
        let m = merged(&f.base, &cs);
        assert!(m.conflicts.is_empty(), "{r1}: {:?}", m.conflicts);
        assert_eq!(m.state.blocks.get(&f.b), Some(&f.v2.version_hash()));
        seen.push(h(r1) < h(r2));
    }
    assert_ne!(seen[0], seen[1]);
    // r1 landed before S: the base holds v2, produced by r1, and r2 still agrees with it
    let mut landed = StateSnapshot::default();
    landed.blocks.insert(f.b.clone(), f.v2.version_hash());
    let m = merged_by(
        &landed,
        &[replay_of("r2", vec![h(CANON)], vec![upd.clone()], "S")],
        "r1",
    );
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    assert_eq!(m.state.blocks.get(&f.b), Some(&f.v2.version_hash()));
    // and so does S itself, an ordinary change
    let m = merged_by(
        &landed,
        &[owned("S", vec![h(CANON)], vec![upd], false)],
        "r1",
    );
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
}

#[test]
fn ok7_replay_chain_applies_after_the_target_whatever_its_ids() {
    // the target moved b from v0 to t; a replay chain read v0 (r1) and built on it (r2). Even
    // when the replays' ids sort before the target's change, they apply after it: the chain
    // keeps both its sides beside the target's
    let f = fixture();
    let t_op = Op::Update {
        base: f.v0.version_hash(),
        block: f.v1.clone(),
    };
    let r1_op = Op::Update {
        base: f.v0.version_hash(),
        block: f.v2.clone(),
    };
    let v3 = block("b-00001", "built on r1");
    let r2_op = Op::Update {
        base: f.v2.version_hash(),
        block: v3.clone(),
    };
    let mut orders = BTreeMap::new();
    for (t, r1, r2) in [("a", "b", "c"), ("c", "a", "b"), ("b", "c", "a")] {
        let cs = [
            owned(t, vec![h(CANON)], vec![t_op.clone()], false),
            owned(r1, vec![], vec![r1_op.clone()], true),
            owned(r2, vec![h(r1)], vec![r2_op.clone()], true),
        ];
        let m = merged(&f.base, &cs);
        assert_eq!(
            m.conflicts,
            vec![conflict(
                &f.b,
                Some(f.v0.version_hash()),
                vec![
                    side(Some(h(t)), Some(f.v1.version_hash())),
                    side(Some(h(r1)), Some(f.v2.version_hash())),
                    side(Some(h(r2)), Some(v3.version_hash())),
                ],
            )],
            "target {t}"
        );
        assert_eq!(m.order, vec![h(t), h(r1), h(r2)]);
        orders.insert(h(t) < h(r1), ());
    }
    // the ids sorted both ways
    assert_eq!(orders.len(), 2);
}

// ---- the prepared write ----

fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("r".into()),
        role: Role::Writer,
        at_ms: 7,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov("agent:a"),
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn refs(s: &Store) -> Vec<(String, Vec<u8>)> {
    s.reader().refs_with_prefix("").unwrap()
}

#[test]
fn ok7_prepared_replay_is_one_write_with_given_deps() {
    let (_d, mut s) = store();
    // a source in view src, and an empty target view t
    let src = s
        .commit_to_view(
            "src",
            change(
                "src",
                vec![Op::Create {
                    block: block("b-00001", "one"),
                }],
            ),
        )
        .unwrap();
    s.view_create("t", None, None, false, &prov("agent:a"))
        .unwrap();
    let mut r1 = change(
        "t",
        vec![Op::Create {
            block: block("b-00001", "one"),
        }],
    );
    r1.links.replayed_from = Some(src);
    let r1_id = r1.id();
    let mut r2 = change(
        "t",
        vec![Op::Create {
            block: block("b-00002", "two"),
        }],
    );
    r2.links.replayed_from = Some(src);
    r2.deps = vec![r1_id];
    let r2_id = r2.id();
    let end = s.log_end();
    let (include, ids) = s
        .view_replay_prepared("t", &[r1, r2], &prov("agent:a"))
        .unwrap();
    assert_eq!(ids, vec![r1_id, r2_id]);
    // the deps are kept as given (an ordinary commit would recompute them)
    assert_eq!(s.change(&r2_id).unwrap().deps, vec![r1_id]);
    assert!(s.change(&r1_id).unwrap().deps.is_empty());
    // the head is the include of both, and the view includes them
    let head = s.view_head("t").unwrap().unwrap();
    assert_eq!(head.head, include);
    assert_eq!(
        s.view_op(&include).unwrap().op,
        ViewOpKind::Include {
            changes: vec![r1_id, r2_id]
        }
    );
    assert_eq!(s.included("t").unwrap().as_ref(), &vec![r1_id, r2_id]);
    // one batch: one marker after the write
    let mut markers = 0;
    s.reader()
        .for_each_frame_from(end, s.log_end(), |_, kind, _, _, _| {
            if rdy_store::codec::logical_kind(kind) == kinds::BATCH_COMMIT {
                markers += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(markers, 1);
}

#[test]
fn ok7_prepared_replay_refuses_before_writing() {
    let (_d, mut s) = store();
    let src = s
        .commit_to_view(
            "src",
            change(
                "src",
                vec![Op::Create {
                    block: block("b-00001", "one"),
                }],
            ),
        )
        .unwrap();
    s.view_create("t", None, None, false, &prov("agent:a"))
        .unwrap();
    let replay = |body: &str| {
        let mut c = change(
            "t",
            vec![Op::Create {
                block: block("b-00003", body),
            }],
        );
        c.links.replayed_from = Some(src);
        c
    };
    let end = s.log_end();
    let before = refs(&s);
    let by = prov("agent:a");
    // no changes
    assert!(matches!(
        s.view_replay_prepared("t", &[], &by),
        Err(StoreError::Decode(_))
    ));
    // a change that is no replay
    assert!(matches!(
        s.view_replay_prepared("t", &[change("t", vec![])], &by),
        Err(StoreError::Decode(_))
    ));
    // the same change twice
    assert!(matches!(
        s.view_replay_prepared("t", &[replay("x"), replay("x")], &by),
        Err(StoreError::Decode(_))
    ));
    // a dep the store lacks
    let mut lost = replay("y");
    lost.deps = vec![h("nowhere")];
    assert!(matches!(
        s.view_replay_prepared("t", &[lost], &by),
        Err(StoreError::NotFound(_))
    ));
    // no view
    assert!(matches!(
        s.view_replay_prepared("none", &[replay("z")], &by),
        Err(StoreError::UnknownView(_))
    ));
    assert_eq!(s.log_end(), end);
    assert_eq!(refs(&s), before);
}
