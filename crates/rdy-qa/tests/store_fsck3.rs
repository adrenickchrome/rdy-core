//! fsck3: relay rules, the dependency DAG, closed included sets, undo chains.
use rdy_model::*;
use rdy_store::view::ViewOpKind;
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
    }
}
fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role: Role::Writer,
        at_ms: 7,
    }
}
fn change(view: &str, ops: Vec<Op>, deps: Vec<Hash>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps,
        provenance: prov("agent:a"),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
    }
}
fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(&d.path().join("s")).unwrap();
    (d, s)
}
fn fsck_err(s: &Store) -> String {
    match s.check_objects() {
        Err(StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}
fn put_canon(s: &mut Store, rec: &LandingRecord) {
    let h = s.put(kinds::LANDING, rec).unwrap();
    s.set_refs(vec![("canon".into(), h.0.to_vec())]).unwrap();
}

#[test]
fn fsck3_relay_fields_must_agree_with_assurance() {
    let (_d, mut s) = store();
    s.commit_to_view(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00001", "x"),
            }],
            vec![],
        ),
    )
    .unwrap();
    let p = s.propose("v", "t", prov("human:h")).unwrap();
    let l = s.land(&p, prov("human:h")).unwrap();
    let mut rec = s.landing(&l).unwrap();
    // the edited records carry no changes, so the change -> landing index (first landing wins)
    // stays consistent and only the relay rules can fail
    rec.changes = vec![];
    rec.assurance = Assurance::Relayed;
    let bad = s.put(kinds::LANDING, &rec).unwrap();
    s.set_refs(vec![("canon".into(), bad.0.to_vec())]).unwrap();
    assert!(fsck_err(&s).contains("assurance is relayed but the relay fields are unset"));
    rec.assurance = Assurance::Local;
    rec.relayed_by = Some(ActorRun {
        actor: "agent:r".into(),
        run: None,
    });
    let bad2 = s.put(kinds::LANDING, &rec).unwrap();
    s.set_refs(vec![("canon".into(), bad2.0.to_vec())]).unwrap();
    assert!(fsck_err(&s).contains("both be set or both unset"));
    rec.relay_evidence = Some(RelayEvidence {
        quoted: "q".into(),
        session_ref: "cli".into(),
        question: None,
    });
    rec.assurance = Assurance::Relayed;
    rec.evidence = Some(ApprovalEvidence {
        kind: EvidenceKind::Text,
        options: vec![],
        choice: None,
        card_hash: None,
    });
    rec.rebased_from = vec![Hash::of(b"not a landing")];
    let bad3 = s.put(kinds::LANDING, &rec).unwrap();
    s.set_refs(vec![("canon".into(), bad3.0.to_vec())]).unwrap();
    assert!(fsck_err(&s).contains("rebased_from names"));
    rec.rebased_from = vec![];
    let good = s.put(kinds::LANDING, &rec).unwrap();
    s.set_refs(vec![("canon".into(), good.0.to_vec())]).unwrap();
    assert_eq!(s.check_objects().unwrap().relayed, 1);
}

#[test]
fn evk1_fsck_checks_the_evidence_kind_against_assurance() {
    let (_d, mut s) = store();
    s.commit_to_view(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00001", "x"),
            }],
            vec![],
        ),
    )
    .unwrap();
    let p = s.propose("v", "t", prov("human:h")).unwrap();
    let l = s.land(&p, prov("human:h")).unwrap();
    let mut rec = s.landing(&l).unwrap();
    rec.changes = vec![];
    let ev = |kind| ApprovalEvidence {
        kind,
        options: vec![],
        choice: None,
        card_hash: None,
    };
    rec.evidence = Some(ev(EvidenceKind::Text));
    put_canon(&mut s, &rec);
    assert!(fsck_err(&s).contains("evidence kind text needs assurance relayed"));
    rec.evidence = Some(ev(EvidenceKind::Elicited));
    put_canon(&mut s, &rec);
    s.check_objects().unwrap();
    rec.assurance = Assurance::Relayed;
    rec.relayed_by = Some(ActorRun {
        actor: "agent:r".into(),
        run: None,
    });
    rec.relay_evidence = Some(RelayEvidence {
        quoted: "q".into(),
        session_ref: "cli".into(),
        question: Some("land it?".into()),
    });
    put_canon(&mut s, &rec);
    assert!(fsck_err(&s).contains("evidence kind elicited needs assurance local"));
    rec.evidence = None;
    put_canon(&mut s, &rec);
    assert!(fsck_err(&s).contains("names its evidence kind"));
    rec.evidence = Some(ev(EvidenceKind::Card));
    put_canon(&mut s, &rec);
    assert!(fsck_err(&s).contains("card evidence needs card_hash"));
    rec.evidence = Some(ApprovalEvidence {
        card_hash: Some(Hash::of(b"card")),
        ..ev(EvidenceKind::Card)
    });
    put_canon(&mut s, &rec);
    assert_eq!(s.check_objects().unwrap().relayed, 1);
}

#[test]
fn fsck3_an_included_change_with_a_missing_or_unlanded_dep_fails_the_closure_check() {
    let (_d, mut s) = store();
    let c1 = s
        .commit_to_view(
            "a",
            change(
                "a",
                vec![Op::Create {
                    block: block("c-00001", "x"),
                }],
                vec![],
            ),
        )
        .unwrap();
    let v1 = s.view_state("a").unwrap().blocks[&BlockId::parse("c-00001").unwrap()];
    let c2 = s
        .commit_to_view(
            "a",
            change(
                "a",
                vec![Op::Update {
                    base: v1,
                    block: block("c-00001", "y"),
                }],
                vec![],
            ),
        )
        .unwrap();
    assert_eq!(s.change(&c2).unwrap().deps, vec![c1]);
    s.commit_to_view(
        "b",
        change(
            "b",
            vec![Op::Create {
                block: block("c-00002", "z"),
            }],
            vec![],
        ),
    )
    .unwrap();
    s.view_append(
        "b",
        vec![ViewOpKind::Include { changes: vec![c2] }],
        &prov("agent:b"),
    )
    .unwrap();
    let m = fsck_err(&s);
    assert!(
        m.contains("view \"b\"") && m.contains("neither included nor in the view's base"),
        "{m}"
    );
    s.view_append(
        "b",
        vec![ViewOpKind::Include { changes: vec![c1] }],
        &prov("agent:b"),
    )
    .unwrap();
    s.check_objects().unwrap();
    // landing a: c1 and c2 land (Task 3, in the same group, also drops them from b)
    let p = s.propose("a", "t", prov("human:h")).unwrap();
    s.land(&p, prov("human:h")).unwrap();
    s.check_objects().unwrap();
    // a change whose dep landed is closed in an unpinned view (its base is the canon head)
    let v2 = s.canon_state().unwrap().blocks[&BlockId::parse("c-00001").unwrap()];
    let c3 = s
        .commit_to_view(
            "c",
            change(
                "c",
                vec![Op::Update {
                    base: v2,
                    block: block("c-00001", "z"),
                }],
                vec![],
            ),
        )
        .unwrap();
    assert_eq!(s.change(&c3).unwrap().deps, vec![c2]);
    s.check_objects().unwrap();
}

#[test]
fn fsck3_a_missing_dep_is_named() {
    let (_d, mut s) = store();
    let a = change(
        "v",
        vec![Op::Create {
            block: block("c-00001", "x"),
        }],
        vec![Hash::of(b"missing")],
    );
    let ha = s.put(kinds::CHANGE, &a).unwrap();
    s.commit_to_view(
        "v",
        change(
            "v",
            vec![Op::Create {
                block: block("c-00009", "seed"),
            }],
            vec![],
        ),
    )
    .unwrap();
    s.view_append(
        "v",
        vec![ViewOpKind::Include { changes: vec![ha] }],
        &prov("agent:a"),
    )
    .unwrap();
    let m = fsck_err(&s);
    assert!(
        m.contains(&format!("dep {} is missing", Hash::of(b"missing"))),
        "{m}"
    );
}

#[test]
fn fsck3_undo_chains_and_recorded_conflicts_are_checked_and_counted() {
    let (_d, mut s) = store();
    s.commit_to_view(
        "seed",
        change(
            "seed",
            vec![Op::Create {
                block: block("c-00009", "s"),
            }],
            vec![],
        ),
    )
    .unwrap();
    let p = s.propose("seed", "t", prov("human:h")).unwrap();
    let l = s.land(&p, prov("human:h")).unwrap();
    let c1 = s
        .commit_to_view(
            "a",
            change(
                "a",
                vec![Op::Create {
                    block: block("c-00001", "x"),
                }],
                vec![],
            ),
        )
        .unwrap();
    let head = s.view_head("a").unwrap().unwrap().head;
    let u = s
        .view_append(
            "a",
            vec![ViewOpKind::Undo { target: head }],
            &prov("agent:a"),
        )
        .unwrap()
        .head;
    s.view_append("a", vec![ViewOpKind::Undo { target: u }], &prov("agent:a"))
        .unwrap();
    let good = rdy_store::merge::Conflict {
        id: Hash::of(b"c"),
        block: BlockId::parse("c-00001").unwrap(),
        base: None,
        sides: vec![rdy_store::merge::Side {
            change: Some(c1),
            version: None,
        }],
    };
    s.view_append(
        "a",
        vec![ViewOpKind::Rebase {
            landing: l,
            dropped: vec![],
            remaining: vec![c1],
            conflicts: vec![good.clone()],
        }],
        &prov("human:h"),
    )
    .unwrap();
    assert_eq!(
        s.check_objects().unwrap().conflicts_recorded,
        1,
        "an undo of an undo walks, and a recorded side that is a change passes"
    );
    let bogus = rdy_store::merge::Conflict {
        sides: vec![rdy_store::merge::Side {
            change: Some(Hash::of(b"not a change")),
            version: None,
        }],
        ..good
    };
    s.view_append(
        "a",
        vec![ViewOpKind::Rebase {
            landing: l,
            dropped: vec![],
            remaining: vec![c1],
            conflicts: vec![bogus],
        }],
        &prov("human:h"),
    )
    .unwrap();
    let m = fsck_err(&s);
    assert!(
        m.contains("view \"a\"")
            && m.contains("records a conflict side")
            && m.contains("not a change"),
        "{m}"
    );
}
