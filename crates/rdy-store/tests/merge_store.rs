//! The merge in the store (plan M2a1, merge1): deleter deps, conflicts as StaleBase at land, and
//! a change covering canon's side lands.
use rdy_model::*;
use rdy_store::{Store, StoreError, kinds};
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

fn seeded(s: &mut Store, b: &Block) {
    s.commit_to_view(
        "seed",
        change("seed", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    let p = s.propose("seed", "seed", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
}

/// Review Focus 5.
#[test]
fn merge1_delete_then_recreate_in_one_view_gets_a_dep_and_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let x = block("b-00001", "one");
    seeded(&mut s, &x);
    let d = s
        .commit_to_view(
            "run",
            change(
                "run",
                vec![Op::Delete {
                    base: x.version_hash(),
                    id: x.id.clone(),
                }],
            ),
        )
        .unwrap();
    let c = s
        .commit_to_view(
            "run",
            change(
                "run",
                vec![Op::Create {
                    block: block("b-00001", "again"),
                }],
            ),
        )
        .unwrap();
    assert!(s.change(&c).unwrap().deps.contains(&d));
    assert!(s.view_merge("run").unwrap().conflicts.is_empty());
    let p = s.propose("run", "move", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
    assert_eq!(
        s.canon_state().unwrap().blocks[&x.id],
        block("b-00001", "again").version_hash()
    );
}

#[test]
fn merge1_landing_over_a_conflict_is_stale_base_naming_both_versions() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let x = block("b-00001", "zero");
    seeded(&mut s, &x);
    let (va, vb) = (block("b-00001", "a"), block("b-00001", "b"));
    s.commit_to_view(
        "a",
        change(
            "a",
            vec![Op::Update {
                base: x.version_hash(),
                block: va.clone(),
            }],
        ),
    )
    .unwrap();
    s.commit_to_view(
        "b",
        change(
            "b",
            vec![Op::Update {
                base: x.version_hash(),
                block: vb,
            }],
        ),
    )
    .unwrap();
    let pa = s.propose("a", "a", prov(Role::Writer)).unwrap();
    let pb = s.propose("b", "b", prov(Role::Writer)).unwrap();
    s.land(&pa, prov(Role::Approver)).unwrap();
    let head = s.canon_head().unwrap();
    match s.land(&pb, prov(Role::Approver)) {
        Err(StoreError::StaleBase {
            id,
            expected,
            actual,
        }) => {
            assert_eq!(
                (id, expected, actual),
                (x.id.clone(), x.version_hash(), Some(va.version_hash()))
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.canon_head().unwrap(), head, "nothing written");
}

/// Review Focus 5: the rebased-then-resolved case at store level. C edits X from x0 after canon
/// moved X to y (made by P); R writes X from C's version and depends on both C and P.
#[test]
fn merge1_a_change_that_covers_canon_and_its_own_side_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let x0 = block("b-00001", "zero");
    seeded(&mut s, &x0);
    let cv = block("b-00001", "view");
    let c = s
        .commit_to_view(
            "v",
            change(
                "v",
                vec![Op::Update {
                    base: x0.version_hash(),
                    block: cv.clone(),
                }],
            ),
        )
        .unwrap();
    let y = block("b-00001", "canon");
    s.commit_to_view(
        "other",
        change(
            "other",
            vec![Op::Update {
                base: x0.version_hash(),
                block: y.clone(),
            }],
        ),
    )
    .unwrap();
    let po = s.propose("other", "other", prov(Role::Writer)).unwrap();
    s.land(&po, prov(Role::Approver)).unwrap();
    let p_id = s.producer(&y.version_hash()).unwrap().unwrap();
    let resolved = block("b-00001", "resolved");
    let r = Change {
        deps: vec![c, p_id],
        ..change(
            "v",
            vec![Op::Update {
                base: cv.version_hash(),
                block: resolved.clone(),
            }],
        )
    };
    let mut items = vec![(kinds::BLOCK, postcard::to_allocvec(&resolved).unwrap())];
    items.push((kinds::CHANGE, postcard::to_allocvec(&r).unwrap()));
    let r_id = s.write(items).unwrap()[1];
    let changes = [s.change(&c).unwrap(), s.change(&r_id).unwrap()];
    let m = s
        .merge_onto(s.canon_head().unwrap(), &s.canon_state().unwrap(), &changes)
        .unwrap();
    assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
    // a version carrying the view's change and the resolution (written raw, so not in the view);
    // a landing needs the proposal's ref, which propose and propose_next write
    let p0 = s.propose("v", "resolved", prov(Role::Writer)).unwrap();
    let pid = s
        .propose_next(
            &p0,
            vec![c, r_id],
            "resolved",
            None,
            prov(Role::Writer),
            None,
        )
        .unwrap();
    s.land(&pid, prov(Role::Approver)).unwrap();
    assert_eq!(
        s.canon_state().unwrap().blocks[&x0.id],
        resolved.version_hash()
    );
}

/// Fix round 1: a version two views produce byte-identically has two producers. A follow-up in
/// one view depends on its own producer, never on the other view's, so the merge orders the
/// view's two changes by their dep and not by id. Each message gives the follow-up another id.
#[test]
fn merge1_a_follow_up_to_an_edit_another_view_made_identically_depends_on_its_own_view() {
    for i in 0..16 {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::create(&dir.path().join("s")).unwrap();
        let x0 = block("b-00001", "zero");
        seeded(&mut s, &x0);
        let same = block("b-00001", "same");
        let up = |view: &str, base: &Block, to: &Block, msg: String| Change {
            message: msg,
            ..change(
                view,
                vec![Op::Update {
                    base: base.version_hash(),
                    block: to.clone(),
                }],
            )
        };
        let a1 = s
            .commit_to_view("a", up("a", &x0, &same, String::new()))
            .unwrap();
        let b1 = s
            .commit_to_view("b", up("b", &x0, &same, String::new()))
            .unwrap();
        let after = block("b-00001", "after");
        let a2 = s
            .commit_to_view("a", up("a", &same, &after, format!("follow-up {i}")))
            .unwrap();
        assert_eq!(s.change(&a2).unwrap().deps, vec![a1], "run {i}: b1 is {b1}");
        let m = s.view_merge("a").unwrap();
        assert!(m.conflicts.is_empty(), "run {i}: {:?}", m.conflicts);
        assert_eq!(m.state.blocks[&x0.id], after.version_hash(), "run {i}");
        let p = s.propose("a", "a", prov(Role::Writer)).unwrap();
        s.land(&p, prov(Role::Approver)).unwrap();
        assert_eq!(
            s.canon_state().unwrap().blocks[&x0.id],
            after.version_hash(),
            "run {i}"
        );
    }
}

/// Fix round 1: canon's side of a version names the change that landed it, even when another
/// view wrote the identical version later; a view floating on canon depends on that change.
#[test]
fn merge1_a_canon_version_names_the_change_that_landed_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let x0 = block("b-00001", "zero");
    seeded(&mut s, &x0);
    let same = block("b-00001", "same");
    let up = |view: &str, base: &Block, to: &Block| {
        change(
            view,
            vec![Op::Update {
                base: base.version_hash(),
                block: to.clone(),
            }],
        )
    };
    // view a takes its base while X is still x0, pinned so b's landing does not rebase it (reb1)
    s.view_create(
        "a",
        s.canon_head().unwrap(),
        None,
        true,
        &prov(Role::Writer),
    )
    .unwrap();
    s.commit_to_view(
        "a",
        change(
            "a",
            vec![Op::Create {
                block: block("b-00002", "two"),
            }],
        ),
    )
    .unwrap();
    let b1 = s.commit_to_view("b", up("b", &x0, &same)).unwrap();
    let pb = s.propose("b", "b", prov(Role::Writer)).unwrap();
    s.land(&pb, prov(Role::Approver)).unwrap();
    // a writes the identical version after b landed it
    let a1 = s.commit_to_view("a", up("a", &x0, &same)).unwrap();
    assert_ne!(a1, b1);
    assert_eq!(s.producer(&same.version_hash()).unwrap(), Some(b1));
    let c1 = s
        .commit_to_view("c", up("c", &same, &block("b-00001", "c")))
        .unwrap();
    assert_eq!(s.change(&c1).unwrap().deps, vec![b1]);
}

/// Review Focus 4: a damaged producer entry is a read error, never "no producer".
#[test]
fn msw4_an_unreadable_producer_entry_fails_the_landing_instead_of_settling_canons_side() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let x0 = block("b-00001", "zero");
    let y = block("b-00001", "canon");
    let (u1, u2) = (block("b-00001", "u1"), block("b-00001", "u2"));
    {
        let mut s = Store::create(&path).unwrap();
        seeded(&mut s, &x0);
        let c1 = s
            .commit_to_view(
                "v",
                change(
                    "v",
                    vec![Op::Update {
                        base: x0.version_hash(),
                        block: u1.clone(),
                    }],
                ),
            )
            .unwrap();
        let c2 = s
            .commit_to_view(
                "v",
                change(
                    "v",
                    vec![Op::Update {
                        base: u1.version_hash(),
                        block: u2.clone(),
                    }],
                ),
            )
            .unwrap();
        assert!(s.change(&c2).unwrap().deps.contains(&c1));
        s.commit_to_view(
            "other",
            change(
                "other",
                vec![Op::Update {
                    base: x0.version_hash(),
                    block: y.clone(),
                }],
            ),
        )
        .unwrap();
        let po = s.propose("other", "other", prov(Role::Writer)).unwrap();
        s.land(&po, prov(Role::Approver)).unwrap();
        let p = s.propose("v", "v", prov(Role::Writer)).unwrap();
        assert!(
            matches!(
                s.land(&p, prov(Role::Approver)),
                Err(StoreError::StaleBase { .. })
            ),
            "healthy index: stale"
        );
    }
    {
        const PRODUCERS: redb::TableDefinition<&[u8], &[u8]> =
            redb::TableDefinition::new("producers");
        let db = redb::Database::open(path.join("index.redb")).unwrap();
        let w = db.begin_write().unwrap();
        {
            let mut t = w.open_table(PRODUCERS).unwrap();
            let mut key = y.version_hash().0.to_vec();
            key.extend_from_slice(&[0xAB; 3]); // a damaged producer entry: not 64 bytes
            t.insert(&key[..], &[][..]).unwrap();
        }
        w.commit().unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    assert!(
        s.producer(&y.version_hash()).is_err(),
        "the damaged entry reads as an error"
    );
    let res = s
        .propose("v", "again", prov(Role::Writer))
        .and_then(|p| s.land(&p, prov(Role::Approver)));
    assert!(res.is_err(), "must not land over canon's side: {res:?}");
    assert_eq!(
        s.canon_state().unwrap().blocks[&x0.id],
        y.version_hash(),
        "canon keeps y"
    );
}

/// The landing in a store at `path` that proposes view v (x0 -> u) while view w holds w1
/// (x0 -> a) and w2 (a -> c, on w1): the store is closed after the setup, so the caller can
/// damage the index before it lands.
fn msw4_rebase_setup(path: &std::path::Path, x0: &Block, u: &Block) -> rdy_model::Hash {
    let (a, c) = (block("b-00001", "a"), block("b-00001", "c"));
    let mut s = Store::create(path).unwrap();
    seeded(&mut s, x0);
    let w1 = s
        .commit_to_view(
            "w",
            change(
                "w",
                vec![Op::Update {
                    base: x0.version_hash(),
                    block: a.clone(),
                }],
            ),
        )
        .unwrap();
    let w2 = s
        .commit_to_view(
            "w",
            change(
                "w",
                vec![Op::Update {
                    base: a.version_hash(),
                    block: c,
                }],
            ),
        )
        .unwrap();
    assert!(s.change(&w2).unwrap().deps.contains(&w1));
    s.commit_to_view(
        "v",
        change(
            "v",
            vec![Op::Update {
                base: x0.version_hash(),
                block: u.clone(),
            }],
        ),
    )
    .unwrap();
    s.propose("v", "v", prov(Role::Writer)).unwrap()
}

fn msw4_land(s: &mut Store, p: rdy_model::Hash) -> Result<rdy_store::LandOutcome, StoreError> {
    s.land_atomic(rdy_store::LandRequest {
        proposal: p,
        accept: None,
        approver: prov(Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
    })
}

/// Review Focus 4, the rebase half: the producer of the landed version is read for every view
/// the landing rebases. A damaged entry fails the landing; read as "no producer", it would settle
/// canon's side and drop view w's conflict.
#[test]
fn msw4_an_unreadable_producer_of_the_landed_version_fails_the_landing_instead_of_dropping_a_views_conflict()
 {
    let x0 = block("b-00001", "zero");
    let u = block("b-00001", "u");
    // control: with a healthy index, the landing rebases w with exactly one conflict
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let p = msw4_rebase_setup(&path, &x0, &u);
        let mut s = Store::open(&path).unwrap();
        let out = msw4_land(&mut s, p).unwrap();
        let w = out.rebased.iter().find(|r| r.view == "w").unwrap();
        assert_eq!(w.conflicts.len(), 1, "healthy index: {:?}", w.conflicts);
        assert_eq!(s.canon_state().unwrap().blocks[&x0.id], u.version_hash());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let p = msw4_rebase_setup(&path, &x0, &u);
    {
        const PRODUCERS: redb::TableDefinition<&[u8], &[u8]> =
            redb::TableDefinition::new("producers");
        let db = redb::Database::open(path.join("index.redb")).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let mut t = tx.open_table(PRODUCERS).unwrap();
            let mut key = u.version_hash().0.to_vec();
            key.extend_from_slice(&[0xAB; 3]); // a damaged producer entry: not 64 bytes
            t.insert(&key[..], &[][..]).unwrap();
        }
        tx.commit().unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    assert!(
        s.producer(&u.version_hash()).is_err(),
        "the damaged entry reads as an error"
    );
    let res = msw4_land(&mut s, p);
    assert!(res.is_err(), "must not drop w's conflict: {res:?}");
    assert_eq!(
        s.canon_state().unwrap().blocks[&x0.id],
        x0.version_hash(),
        "canon keeps x0"
    );
}
