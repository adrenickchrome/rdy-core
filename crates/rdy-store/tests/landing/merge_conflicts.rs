//! Every merge conflict and its kind, on any base: `merge_conflicts` names each conflicting block
//! once with its kind, and `merge_checked`'s refusal is its first entry.
use rdy_model::*;
use rdy_store::{ConflictKind, MergeConflict, Store, StoreError};
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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn decision(status: &str, statement: &str) -> Block {
    let mut b = block("D-00001", "");
    b.block_type = decision::DECISION.into();
    b.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    b.fields
        .insert("statement".into(), FieldValue::Str(statement.into()));
    b
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

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

/// Commits `ops` to `view` and lands it; returns the landing.
fn landed(s: &mut Store, view: &str, ops: Vec<Op>) -> Hash {
    s.commit_to_view(view, change(view, ops)).unwrap();
    let p = s.propose(view, view, prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap()
}

fn conflict(block: &str, kind: ConflictKind) -> MergeConflict {
    MergeConflict {
        block: id(block),
        kind,
    }
}

/// `merge_conflicts` onto canon's head, as `merge_checked` reads it.
fn on_canon(s: &Store, changes: &[Change]) -> Vec<MergeConflict> {
    let canon = s.canon_state().unwrap();
    s.merge_conflicts(s.canon_head().unwrap(), &canon, changes)
        .unwrap()
        .1
}

/// Canon holds a, b and c; a later landing updated a and deleted c. Changes written on the
/// earlier state update a (stale), create b with no base (exists) and re-create c (deleted since).
fn three_conflicts() -> (tempfile::TempDir, Store, Vec<Change>, Hash, Hash) {
    let (d, mut s) = store();
    let (a0, b0, c0) = (
        block("c-00001", "a0"),
        block("c-00002", "b0"),
        block("c-00003", "c0"),
    );
    landed(
        &mut s,
        "seed",
        vec![
            Op::Create { block: a0.clone() },
            Op::Create { block: b0.clone() },
            Op::Create { block: c0.clone() },
        ],
    );
    let a1 = block("c-00001", "a1");
    landed(
        &mut s,
        "later",
        vec![
            Op::Update {
                base: a0.version_hash(),
                block: a1.clone(),
            },
            Op::Delete {
                base: c0.version_hash(),
                id: c0.id.clone(),
            },
        ],
    );
    let changes = vec![change(
        "w",
        vec![
            Op::Update {
                base: a0.version_hash(),
                block: block("c-00001", "a by w"),
            },
            Op::Create {
                block: block("c-00002", "b by w"),
            },
            Op::Create {
                block: block("c-00003", "c by w"),
            },
        ],
    )];
    (d, s, changes, a0.version_hash(), a1.version_hash())
}

/// An accepted decision D in canon and its version.
fn accepted_decision() -> (tempfile::TempDir, Store, Hash) {
    let (d, mut s) = store();
    let d0 = decision("proposed", "D");
    landed(&mut s, "seed", vec![Op::Create { block: d0.clone() }]);
    let d1 = decision("accepted", "D");
    landed(
        &mut s,
        "accept",
        vec![Op::Update {
            base: d0.version_hash(),
            block: d1.clone(),
        }],
    );
    (d, s, d1.version_hash())
}

#[test]
fn crb2_every_conflict_is_named_with_its_kind() {
    let (_d, s, changes, _, _) = three_conflicts();
    let (merged, conflicts) = {
        let canon = s.canon_state().unwrap();
        s.merge_conflicts(s.canon_head().unwrap(), &canon, &changes)
            .unwrap()
    };
    assert_eq!(merged.conflicts.len(), 3, "{merged:?}");
    assert_eq!(
        conflicts,
        vec![
            conflict("c-00001", ConflictKind::StaleBase),
            conflict("c-00002", ConflictKind::AlreadyExists),
            conflict("c-00003", ConflictKind::DeletedSince),
        ]
    );
    // the kind words
    assert_eq!(
        serde_json::to_value(&conflicts).unwrap(),
        serde_json::json!([
            {"block": "c-00001", "kind": "stale_base"},
            {"block": "c-00002", "kind": "already_exists"},
            {"block": "c-00003", "kind": "deleted_since"},
        ])
    );
    assert_eq!(
        serde_json::to_value(ConflictKind::Frozen).unwrap(),
        serde_json::json!("frozen")
    );
}

#[test]
fn crb2_frozen_is_named() {
    let (_d, s, v) = accepted_decision();
    let up = change(
        "w",
        vec![Op::Update {
            base: v,
            block: decision("accepted", "D rewritten"),
        }],
    );
    let canon = s.canon_state().unwrap();
    let (merged, conflicts) = s
        .merge_conflicts(s.canon_head().unwrap(), &canon, std::slice::from_ref(&up))
        .unwrap();
    assert!(merged.conflicts.is_empty(), "{merged:?}");
    assert_eq!(conflicts, vec![conflict("D-00001", ConflictKind::Frozen)]);
    // a frozen op follows the merge's conflicts
    let (_d2, mut s, v) = accepted_decision();
    let x0 = block("c-00001", "x0");
    landed(&mut s, "x", vec![Op::Create { block: x0.clone() }]);
    landed(
        &mut s,
        "x2",
        vec![Op::Update {
            base: x0.version_hash(),
            block: block("c-00001", "x1"),
        }],
    );
    let changes = vec![change(
        "w",
        vec![
            Op::Update {
                base: v,
                block: decision("accepted", "D rewritten"),
            },
            Op::Update {
                base: x0.version_hash(),
                block: block("c-00001", "x by w"),
            },
        ],
    )];
    assert_eq!(
        on_canon(&s, &changes),
        vec![
            conflict("c-00001", ConflictKind::StaleBase),
            conflict("D-00001", ConflictKind::Frozen),
        ]
    );
}

/// `merge_checked`'s refusal is `merge_conflicts`' first entry, with today's error.
#[test]
fn crb2_first_entry_equals_merge_checked() {
    fn check(s: &Store, changes: &[Change], want: StoreError, first: MergeConflict) {
        let canon = s.canon_state().unwrap();
        let got = s.merge_checked(&canon, changes).unwrap_err();
        assert_eq!(format!("{got:?}"), format!("{want:?}"));
        assert_eq!(got.to_string(), want.to_string());
        assert_eq!(on_canon(s, changes).first(), Some(&first));
    }
    // StaleBase: the base and canon's version
    let (_d, s, changes, a0, a1) = three_conflicts();
    check(
        &s,
        &changes[..],
        StoreError::StaleBase {
            id: id("c-00001"),
            expected: a0,
            actual: Some(a1),
        },
        conflict("c-00001", ConflictKind::StaleBase),
    );
    let only = |n: usize| vec![change("w", vec![changes[0].ops[n].clone()])];
    check(
        &s,
        &only(1),
        StoreError::AlreadyExists(id("c-00002")),
        conflict("c-00002", ConflictKind::AlreadyExists),
    );
    check(
        &s,
        &only(2),
        StoreError::DeletedSince(id("c-00003")),
        conflict("c-00003", ConflictKind::DeletedSince),
    );
    // a stale update of a block canon deleted: StaleBase with no actual version
    let stale_gone = vec![change(
        "w",
        vec![Op::Update {
            base: block("c-00003", "c0").version_hash(),
            block: block("c-00003", "c by w"),
        }],
    )];
    check(
        &s,
        &stale_gone,
        StoreError::StaleBase {
            id: id("c-00003"),
            expected: block("c-00003", "c0").version_hash(),
            actual: None,
        },
        conflict("c-00003", ConflictKind::StaleBase),
    );
    // a clean merge is Ok and names nothing
    let clean = vec![change(
        "w",
        vec![Op::Update {
            base: a1,
            block: block("c-00001", "a2"),
        }],
    )];
    let canon = s.canon_state().unwrap();
    let (m, _) = s.merge_checked(&canon, &clean).unwrap();
    let (m2, c2) = s
        .merge_conflicts(s.canon_head().unwrap(), &canon, &clean)
        .unwrap();
    assert_eq!(m, m2);
    assert!(c2.is_empty(), "{c2:?}");
    // Frozen
    let (_d2, s2, v) = accepted_decision();
    let up = vec![change(
        "w",
        vec![Op::Update {
            base: v,
            block: decision("accepted", "D rewritten"),
        }],
    )];
    check(
        &s2,
        &up,
        StoreError::Frozen(id("D-00001")),
        conflict("D-00001", ConflictKind::Frozen),
    );
    // a frozen block's tracking-only or equal update passes, as today
    let same = vec![change(
        "w",
        vec![Op::Update {
            base: v,
            block: decision("accepted", "D"),
        }],
    )];
    let canon2 = s2.canon_state().unwrap();
    s2.merge_checked(&canon2, &same).unwrap();
    assert!(on_canon(&s2, &same).is_empty());
}

/// Deletions are read as of `at`: x is absent at L1 (never created), created at L2, deleted at
/// L3, and canon's head L4 is later still. A create of x is clean on L1 and deleted since on L3
/// (not canon's head) and on canon.
#[test]
fn crb2_any_base_reads_its_own_deletions() {
    let (_d, mut s) = store();
    let l1 = landed(
        &mut s,
        "one",
        vec![Op::Create {
            block: block("c-00001", "y"),
        }],
    );
    let x = block("c-00002", "x");
    let l2 = landed(&mut s, "two", vec![Op::Create { block: x.clone() }]);
    let l3 = landed(
        &mut s,
        "three",
        vec![Op::Delete {
            base: x.version_hash(),
            id: x.id.clone(),
        }],
    );
    landed(
        &mut s,
        "four",
        vec![Op::Create {
            block: block("c-00003", "z"),
        }],
    );
    let re = vec![change(
        "w",
        vec![Op::Create {
            block: block("c-00002", "x again"),
        }],
    )];
    let on = |at: Hash| {
        s.merge_conflicts(Some(at), &s.state_of(Some(at)).unwrap(), &re)
            .unwrap()
    };
    let (m1, c1) = on(l1);
    assert!(m1.conflicts.is_empty() && c1.is_empty(), "{m1:?} {c1:?}");
    assert_eq!(
        on(l2).1,
        vec![conflict("c-00002", ConflictKind::AlreadyExists)]
    );
    assert_eq!(
        on(l3).1,
        vec![conflict("c-00002", ConflictKind::DeletedSince)]
    );
    assert_eq!(
        on_canon(&s, &re),
        vec![conflict("c-00002", ConflictKind::DeletedSince)]
    );
}
