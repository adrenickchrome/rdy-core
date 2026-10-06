//! reb1 at the store: a landing rebases every live, unpinned, non-empty view in its own batch,
//! computes conflicts on the touched intersection, and writes all of it or nothing.
use rdy_model::*;
use rdy_store::view::{Owner, ViewOpKind};
use rdy_store::{LandRequest, Store, StoreError};
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
fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
fn create(s: &mut Store, view: &str, b: &str) -> Hash {
    s.commit_to_view(
        view,
        change(
            view,
            vec![Op::Create {
                block: block(b, "v1"),
            }],
            &prov("agent:seed"),
        ),
    )
    .unwrap()
}
fn update(s: &mut Store, view: &str, b: &str, base: Hash, body: &str) -> Hash {
    s.commit_to_view(
        view,
        change(
            view,
            vec![Op::Update {
                base,
                block: block(b, body),
            }],
            &prov(&format!("agent:{view}")),
        ),
    )
    .unwrap()
}
fn canon_version(s: &Store, b: &str) -> Hash {
    *s.canon_state().unwrap().blocks.get(&id(b)).unwrap()
}
fn land(s: &mut Store, view: &str) -> rdy_store::LandOutcome {
    let p = s.propose(view, "t", prov("human:h")).unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: prov("human:h"),
        chosen: None,
        relay: None,
        evidence: None,
    })
    .unwrap()
}
fn rebase_of(s: &Store, view: &str) -> (Hash, Vec<Hash>, Vec<Hash>, Vec<rdy_store::Conflict>) {
    let h = s.view_head(view).unwrap().unwrap();
    match s.view_op(&h.head).unwrap().op {
        ViewOpKind::Rebase {
            landing,
            dropped,
            remaining,
            conflicts,
        } => (landing, dropped, remaining, conflicts),
        other => panic!("{view}: head is {other:?}"),
    }
}

/// Canon: X, Y, Z. Views: a and b edit X; c edits Y; d is pinned and edits X; e edits X then
/// is closed; f is created and left empty. Returns the store and canon's version of X.
fn scene() -> (tempfile::TempDir, Store, Hash) {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("s")).unwrap();
    for b in ["c-00001", "c-00002", "c-00003"] {
        create(&mut s, "seed", b);
    }
    land(&mut s, "seed");
    let x = canon_version(&s, "c-00001");
    let y = canon_version(&s, "c-00002");
    update(&mut s, "a", "c-00001", x, "a");
    update(&mut s, "b", "c-00001", x, "b");
    update(&mut s, "c", "c-00002", y, "c");
    s.view_create(
        "d",
        s.canon_head().unwrap(),
        Some(Owner {
            actor: "agent:d".into(),
            run: None,
        }),
        true,
        &prov("agent:d"),
    )
    .unwrap();
    update(&mut s, "d", "c-00001", x, "d");
    update(&mut s, "e", "c-00001", x, "e");
    s.view_append(
        "e",
        vec![ViewOpKind::Close { reason: "x".into() }],
        &prov("agent:e"),
    )
    .unwrap();
    s.view_create("f", s.canon_head().unwrap(), None, false, &prov("agent:f"))
        .unwrap();
    (d, s, x)
}

#[test]
fn reb1_a_landing_rebases_every_live_unpinned_nonempty_view_and_skips_the_rest() {
    let (_d, mut s, x) = scene();
    let before: BTreeMap<String, rdy_store::ViewHead> = ["d", "e", "f"]
        .iter()
        .map(|v| (v.to_string(), s.view_head(v).unwrap().unwrap()))
        .collect();
    let out = land(&mut s, "a");
    let names: Vec<&str> = out.rebased.iter().map(|r| r.view.as_str()).collect();
    assert_eq!(
        names,
        vec!["a", "b", "c"],
        "ref order; d pinned, e closed, f empty are skipped"
    );
    for v in ["d", "e", "f"] {
        assert_eq!(s.view_head(v).unwrap().unwrap(), before[v], "{v} untouched");
    }
    let (l, dropped, remaining, conflicts) = rebase_of(&s, "a");
    assert_eq!(
        (l, dropped.len(), remaining.len(), conflicts.len()),
        (out.landing, 1, 0, 0)
    );
    let a_change = dropped[0];
    let (l, dropped, remaining, conflicts) = rebase_of(&s, "b");
    assert_eq!(
        (l, dropped.len(), remaining.len(), conflicts.len()),
        (out.landing, 0, 1, 1),
        "b collides with canon on X"
    );
    assert_eq!(conflicts[0].block, id("c-00001"));
    assert_eq!(
        conflicts[0].base,
        Some(x),
        "the conflict's base is the version b edited"
    );
    assert!(
        conflicts[0]
            .sides
            .iter()
            .any(|side| side.change == Some(a_change)),
        "canon's side names a's landed change"
    );
    assert!(
        conflicts[0]
            .sides
            .iter()
            .any(|side| side.change == Some(remaining[0])),
        "the other side is b's change"
    );
    assert_eq!(
        out.rebased[1].conflicts, conflicts,
        "the outcome reports what the op records"
    );
    let (_, dropped, remaining, conflicts) = rebase_of(&s, "c");
    assert_eq!(
        (dropped.len(), remaining.len(), conflicts.len()),
        (0, 1, 0),
        "c touches Y only"
    );
    for v in ["a", "b", "c"] {
        let h = s.view_head(v).unwrap().unwrap();
        assert_eq!(h.base, Some(out.landing), "{v} is based on the new canon");
    }
    assert_eq!(
        s.view_merge("b").unwrap().conflicts,
        rebase_of(&s, "b").3,
        "the merge agrees with the record"
    );
    assert_eq!(s.view_merge("c").unwrap().conflicts, vec![]);
    s.check_objects().unwrap();
}

#[test]
fn reb1_conflicts_are_computed_on_the_intersection_only() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("s")).unwrap();
    for b in ["c-00001", "c-00002", "c-00003"] {
        create(&mut s, "seed", b);
    }
    land(&mut s, "seed");
    let (x, y, z) = (
        canon_version(&s, "c-00001"),
        canon_version(&s, "c-00002"),
        canon_version(&s, "c-00003"),
    );
    update(&mut s, "v", "c-00001", x, "vx");
    update(&mut s, "v", "c-00002", y, "vy");
    update(&mut s, "v", "c-00003", z, "vz");
    update(&mut s, "w", "c-00002", y, "wy");
    land(&mut s, "w");
    let (_, dropped, remaining, conflicts) = rebase_of(&s, "v");
    assert_eq!((dropped.len(), remaining.len()), (0, 3));
    assert_eq!(
        conflicts
            .iter()
            .map(|c| c.block.clone())
            .collect::<Vec<_>>(),
        vec![id("c-00002")]
    );
    let full = s.view_merge("v").unwrap();
    assert_eq!(
        full.conflicts, conflicts,
        "the full merge finds exactly the recorded conflict"
    );
    assert_eq!(
        s.get_block(full.state.blocks.get(&id("c-00001")).unwrap())
            .unwrap()
            .body,
        "vx",
        "untouched blocks keep the view's version"
    );
}

#[test]
fn reb1_a_cascade_across_five_views_re_keys_the_conflict_at_each_landing() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("s")).unwrap();
    create(&mut s, "seed", "c-00001");
    land(&mut s, "seed");
    let x = canon_version(&s, "c-00001");
    // each body differs from the seed's ("v1"), so v1's edit is a real one
    for v in ["v1", "v2", "v3", "v4", "v5"] {
        update(&mut s, v, "c-00001", x, &format!("{v}'s edit"));
    }
    land(&mut s, "v1");
    let first: Vec<Hash> = ["v2", "v3", "v4", "v5"]
        .iter()
        .map(|v| rebase_of(&s, v).3[0].id)
        .collect();
    assert_eq!(
        first
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4,
        "each view's conflict has its own id (its own side)"
    );
    // v2 resolves from the conflict base and lands; the others get a new conflict against the new canon
    let m = s.view_merge("v2").unwrap();
    let current = |b: &BlockId| m.state.blocks.get(b).copied();
    s.commit_to_view_resolving(
        "v2",
        change(
            "v2",
            vec![Op::Update {
                base: x,
                block: block("c-00001", "v2 merged"),
            }],
            &prov("agent:v2"),
        ),
        &current,
        &m.conflicts,
        None,
    )
    .unwrap();
    assert!(s.view_merge("v2").unwrap().conflicts.is_empty());
    let out = land(&mut s, "v2");
    assert_eq!(
        out.rebased
            .iter()
            .map(|r| r.view.as_str())
            .collect::<Vec<_>>(),
        vec!["v2", "v3", "v4", "v5"]
    );
    for (i, v) in ["v3", "v4", "v5"].iter().enumerate() {
        let c = &rebase_of(&s, v).3;
        assert_eq!(c.len(), 1, "{v}");
        assert_ne!(
            c[0].id,
            first[i + 1],
            "{v}: canon's side changed, so the conflict is a new one"
        );
        assert_eq!(s.view_merge(v).unwrap().conflicts, *c);
    }
    assert_eq!(
        s.get_block(&canon_version(&s, "c-00001")).unwrap().body,
        "v2 merged"
    );
    s.check_objects().unwrap();
}

/// Review Focus 6: the proposing view rebases even when pinned; its landed changes drop out.
#[test]
fn reb1_a_pinned_proposing_view_drops_its_landed_changes() {
    let (_d, mut s, x) = scene();
    let before = s.view_head("d").unwrap().unwrap();
    assert!(before.pinned);
    let out = land(&mut s, "d");
    let names: Vec<&str> = out.rebased.iter().map(|r| r.view.as_str()).collect();
    assert_eq!(
        names,
        vec!["a", "b", "c", "d"],
        "ref order; the pinned proposer is in the batch, e and f are not"
    );
    let (l, dropped, remaining, _) = rebase_of(&s, "d");
    assert_eq!((l, dropped.len(), remaining.len()), (out.landing, 1, 0));
    let h = s.view_head("d").unwrap().unwrap();
    assert_eq!(
        (h.base, h.pinned, h.len),
        (Some(out.landing), true, 0),
        "still pinned, now at the landing it made"
    );
    assert_eq!(
        s.get_block(&canon_version(&s, "c-00001")).unwrap().body,
        "d"
    );
    for v in ["a", "b"] {
        assert_eq!(
            rebase_of(&s, v).3.len(),
            1,
            "{v} now collides with d's landed edit of X"
        );
    }
    let _ = x;
}

#[test]
fn reb1_an_index_failure_during_a_landing_with_rebases_writes_nothing() {
    let (_d, mut s, _x) = scene();
    let heads: Vec<(String, rdy_store::ViewHead)> = s
        .refs_with_prefix("view/")
        .unwrap()
        .into_iter()
        .map(|(n, _)| {
            let n = n["view/".len()..].to_string();
            let h = s.view_head(&n).unwrap().unwrap();
            (n, h)
        })
        .collect();
    let canon = s.canon_head().unwrap();
    let p = s.propose("a", "t", prov("human:h")).unwrap();
    s.fail_next_index_apply();
    let e = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: prov("human:h"),
            chosen: None,
            relay: None,
            evidence: None,
        })
        .unwrap_err();
    assert!(matches!(e, StoreError::Db(_)), "{e}");
    assert_eq!(s.canon_head().unwrap(), canon);
    for (n, h) in heads {
        assert_eq!(s.view_head(&n).unwrap().unwrap(), h, "{n}");
    }
    s.verify().unwrap();
    let dir = s.dir().to_path_buf();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(
        s.canon_head().unwrap(),
        canon,
        "nothing of the landing survives a reopen"
    );
}

/// Fix round 1: two views write a byte-identical version of X, a third collides, and the view
/// that lands is not the first writer. Canon's side of the recorded conflict names the change
/// that landed (the producer the index prefers after the write), so the record and the merge
/// derive the same conflict id.
#[test]
fn reb1_canon_side_names_the_landed_producer_when_another_view_wrote_the_same_version() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("s")).unwrap();
    create(&mut s, "seed", "c-00001");
    land(&mut s, "seed");
    let x = canon_version(&s, "c-00001");
    let cc = update(&mut s, "c", "c-00001", x, "same");
    let ca = update(&mut s, "a", "c-00001", x, "same");
    assert_ne!(cc, ca);
    update(&mut s, "b", "c-00001", x, "other");
    let out = land(&mut s, "a");
    let (_, _, remaining, conflicts) = rebase_of(&s, "b");
    assert_eq!(conflicts.len(), 1);
    assert!(
        conflicts[0]
            .sides
            .iter()
            .any(|side| side.change == Some(ca)),
        "canon's side names a's landed change"
    );
    assert!(
        conflicts[0]
            .sides
            .iter()
            .all(|side| side.change != Some(cc)),
        "not c's unlanded change"
    );
    assert!(
        conflicts[0]
            .sides
            .iter()
            .any(|side| side.change == Some(remaining[0]))
    );
    let rb = out.rebased.iter().find(|r| r.view == "b").unwrap();
    assert_eq!(
        rb.conflicts, conflicts,
        "the outcome reports what the op records"
    );
    assert_eq!(
        s.view_merge("b").unwrap().conflicts,
        conflicts,
        "the merge agrees with the record"
    );
}

/// Fix round 1: the acceptance change writes a version another view already wrote. Canon's side
/// names the acceptance change (it landed), not the other view's unlanded change.
#[test]
fn reb1_canon_side_names_the_acceptance_change_when_another_view_wrote_the_same_version() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(&d.path().join("s")).unwrap();
    for b in ["c-00001", "c-00002"] {
        create(&mut s, "seed", b);
    }
    land(&mut s, "seed");
    let x = canon_version(&s, "c-00001");
    let y = canon_version(&s, "c-00002");
    let cc = update(&mut s, "c", "c-00001", x, "same");
    update(&mut s, "a", "c-00002", y, "a");
    update(&mut s, "b", "c-00001", x, "other");
    let p = s.propose("a", "t", prov("human:h")).unwrap();
    let out = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: Some(change(
                "a",
                vec![Op::Update {
                    base: x,
                    block: block("c-00001", "same"),
                }],
                &prov("human:h"),
            )),
            approver: prov("human:h"),
            chosen: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    let aid = out.accept.unwrap();
    let (_, _, _, conflicts) = rebase_of(&s, "b");
    assert_eq!(conflicts.len(), 1);
    assert!(
        conflicts[0]
            .sides
            .iter()
            .any(|side| side.change == Some(aid)),
        "canon's side names the acceptance change"
    );
    assert!(
        conflicts[0]
            .sides
            .iter()
            .all(|side| side.change != Some(cc)),
        "not c's unlanded change"
    );
    assert_eq!(
        s.view_merge("b").unwrap().conflicts,
        conflicts,
        "the merge agrees with the record"
    );
}
