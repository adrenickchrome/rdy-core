//! cfmt1: intents and alternative sets are versioned objects with a ref per stable id; fsck
//! checks their chains and the links that name them.
use rdy_model::*;
use rdy_store::coord::{ALT_REF, INTENT_REF};
use rdy_store::{Store, StoreError, kinds};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}
fn prov(actor: &str) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("r".into()),
        role: Role::Writer,
        at_ms: 7,
    }
}
fn intent(view: &str) -> Intent {
    Intent {
        stable: None,
        prev: None,
        actor: "agent:a".into(),
        run: Some("r".into()),
        view: view.into(),
        area: Some("payments".into()),
        blocks: vec![],
        angle: "rework fees".into(),
        expires_at_ms: 10_000,
        status: IntentStatus::Open,
        at_ms: 5,
    }
}
fn set(views: &[&str]) -> AltSet {
    AltSet {
        stable: None,
        prev: None,
        title: "Fee model".into(),
        question: None,
        members: views
            .iter()
            .map(|v| AltMember {
                label: v.to_string(),
                view: v.to_string(),
            })
            .collect(),
        status: AltStatus::Open,
        provenance: prov("agent:a"),
    }
}

#[test]
fn cfmt1_kinds_15_and_16_and_format_10() {
    assert_eq!(kinds::INTENT, 15);
    assert_eq!(kinds::ALT_SET, 16);
    assert_eq!(rdy_store::format::STORE_FORMAT, 12);
}

#[test]
fn cfmt1_a_version_chain_moves_the_ref_and_a_stale_prev_is_refused() {
    let (_d, mut s) = store();
    let first = s.put_intent(&intent("a")).unwrap();
    assert_eq!(s.intent_heads().unwrap(), vec![(first, first)]);
    let bytes = s.get_ref(&format!("{INTENT_REF}{first}")).unwrap().unwrap();
    assert_eq!(bytes, first.0.to_vec());
    let renewed = Intent {
        stable: Some(first),
        prev: Some(first),
        expires_at_ms: 20_000,
        ..intent("a")
    };
    let second = s.put_intent(&renewed).unwrap();
    assert_eq!(s.intent_heads().unwrap(), vec![(first, second)]);
    assert_eq!(s.intent(&second).unwrap().expires_at_ms, 20_000);
    // a version built on the first one again: the ref moved
    let stale = Intent {
        stable: Some(first),
        prev: Some(first),
        expires_at_ms: 30_000,
        ..intent("a")
    };
    match s.put_intent(&stale) {
        Err(StoreError::Decode(m)) => assert!(m.contains("moved"), "{m}"),
        other => panic!("{other:?}"),
    }
    // half a chain is refused
    let half = Intent {
        stable: Some(first),
        prev: None,
        ..intent("a")
    };
    assert!(s.put_intent(&half).is_err());
    assert!(
        s.intent(&first).is_ok() && s.alt_set(&first).is_err(),
        "kinds are typed"
    );
}

#[test]
fn cfmt1_alt_sets_have_their_own_refs_and_survive_reindex() {
    let (d, mut s) = store();
    let a = s.put_alt_set(&set(&["a", "b"])).unwrap();
    let next = AltSet {
        stable: Some(a),
        prev: Some(a),
        members: set(&["a", "b", "c"]).members,
        ..set(&[])
    };
    let b = s.put_alt_set(&next).unwrap();
    assert_eq!(s.alt_heads().unwrap(), vec![(a, b)]);
    assert!(s.get_ref(&format!("{ALT_REF}{a}")).unwrap().is_some());
    s.reindex().unwrap();
    assert_eq!(s.alt_heads().unwrap(), vec![(a, b)]);
    drop(s);
    let s = Store::open(&d.path().join("s")).unwrap();
    assert_eq!(s.alt_set(&b).unwrap().members.len(), 3);
}

#[test]
fn cfmt1_fsck_counts_and_checks_coordination_objects() {
    let (_d, mut s) = store();
    let i = s.put_intent(&intent("a")).unwrap();
    let a = s.put_alt_set(&set(&["a", "b"])).unwrap();
    // member views must exist: create them (empty) first
    for v in ["a", "b"] {
        s.view_create(v, None, None, false, &prov("agent:a"))
            .unwrap();
    }
    let c = s.check_objects().unwrap();
    assert_eq!((c.intents, c.alt_sets), (1, 1));
    // a change linking an intent and a set that exist checks clean
    let mut ch = Change {
        view: "a".into(),
        ops: vec![],
        deps: vec![],
        provenance: prov("agent:a"),
        justification: None,
        message: String::new(),
        links: ChangeLinks {
            intent: Some(i),
            alternative: Some(AltChoice {
                set: a,
                member: "a".into(),
            }),
            client_nonce: None,
            imported_by: None,
        },
    };
    let block = Block {
        id: BlockId::parse("c-AAAAA").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: Default::default(),
        body: "x".into(),
        links: Default::default(),
        pins: Default::default(),
    };
    ch.ops.push(Op::Create { block });
    s.commit_to_view("a", ch.clone()).unwrap();
    s.check_objects().unwrap();
    // a link to a set that does not exist is reported
    let mut bad = ch;
    bad.links.alternative = Some(AltChoice {
        set: Hash([9; 32]),
        member: "a".into(),
    });
    if let Op::Create { block } = &mut bad.ops[0] {
        block.id = BlockId::parse("c-BBBBB").unwrap();
    }
    s.commit_to_view("a", bad).unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(m.contains("alternative set"), "{m}"),
        other => panic!("{other:?}"),
    }
}

use rdy_store::coord::{abandoned_reason, chosen_reason};
use rdy_store::view::ViewOpKind;
use rdy_store::{AltLanding, LandRequest};

fn claim(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: Default::default(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
    }
}
fn commit_create(s: &mut Store, view: &str, id: &str) -> Hash {
    s.commit_to_view(
        view,
        Change {
            view: view.into(),
            ops: vec![Op::Create {
                block: claim(id, view),
            }],
            deps: vec![],
            provenance: prov("agent:a"),
            justification: None,
            message: String::new(),
            links: ChangeLinks::default(),
        },
    )
    .unwrap()
}

#[test]
fn altw1_abandon_closes_open_members_only_in_one_batch() {
    let (_d, mut s) = store();
    for v in ["a", "b", "c", "other"] {
        s.view_create(v, None, None, false, &prov("agent:a"))
            .unwrap();
    }
    s.view_append(
        "c",
        vec![ViewOpKind::Close {
            reason: "by hand".into(),
        }],
        &prov("agent:a"),
    )
    .unwrap();
    let first = s.put_alt_set(&set(&["a", "b", "c"])).unwrap();
    let next = AltSet {
        stable: Some(first),
        prev: Some(first),
        status: AltStatus::Abandoned {
            reason: "both wrong".into(),
        },
        ..set(&["a", "b", "c"])
    };
    let before = s.write_count();
    let reason = abandoned_reason(&first, "both wrong");
    let out = s
        .alt_set_write(
            &next,
            &["a".into(), "b".into(), "c".into()],
            &reason,
            &prov("agent:a"),
        )
        .unwrap();
    assert_eq!(s.write_count(), before + 1, "one batch");
    assert_eq!(
        out.closed,
        vec!["a".to_string(), "b".to_string()],
        "c was already closed"
    );
    assert_eq!(s.alt_heads().unwrap(), vec![(first, out.version)]);
    for v in ["a", "b"] {
        let h = s.view_head(v).unwrap().unwrap();
        assert!(h.closed);
        match s.view_op(&h.head).unwrap().op {
            ViewOpKind::Close { reason: r } => assert_eq!(r, reason),
            other => panic!("{other:?}"),
        }
    }
    assert!(
        !s.view_head("other").unwrap().unwrap().closed,
        "a view outside the set is untouched"
    );
    assert_eq!(
        reason,
        format!(
            "alternative set abandoned: as-{}: both wrong",
            first.short()
        )
    );
    s.check_objects().unwrap();
}

#[test]
fn altw1_landing_a_member_chooses_it_and_closes_the_others() {
    let (_d, mut s) = store();
    for v in ["a", "b"] {
        s.view_create(v, None, None, false, &prov("agent:a"))
            .unwrap();
    }
    let ca = commit_create(&mut s, "a", "c-AAAAA");
    commit_create(&mut s, "b", "c-BBBBB");
    let first = s.put_alt_set(&set(&["a", "b"])).unwrap();
    let p = s
        .propose_changes("a", "pick a", None, prov("agent:a"), vec![ca], None)
        .unwrap();
    let out = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: Provenance {
                role: Role::Approver,
                ..prov("human:qa")
            },
            alt: Some(AltLanding {
                set: first,
                prev: first,
                member: "a".into(),
                close: vec!["b".into()],
            }),
            relay: None,
            evidence: None,
        })
        .unwrap();
    assert_eq!(out.closed, vec!["b".to_string()]);
    let rec = s.landing(&out.landing).unwrap();
    assert_eq!(
        rec.chosen,
        Some(AltChoice {
            set: first,
            member: "a".into()
        })
    );
    let v = out.alt_version.unwrap();
    assert_eq!(s.alt_heads().unwrap(), vec![(first, v)]);
    assert_eq!(
        s.alt_set(&v).unwrap().status,
        AltStatus::Chosen {
            member: "a".into(),
            landing: out.landing
        }
    );
    let b = s.view_head("b").unwrap().unwrap();
    assert!(b.closed, "the other member is closed, not rebased");
    match s.view_op(&b.head).unwrap().op {
        ViewOpKind::Close { reason } => assert_eq!(reason, chosen_reason(&first, "a")),
        other => panic!("{other:?}"),
    }
    assert!(out.rebased.iter().all(|r| r.view != "b"));
    assert!(!s.view_head("a").unwrap().unwrap().closed);
    s.check_objects().unwrap();
}

#[test]
fn altw1_a_set_that_moved_refuses_the_landing_and_writes_nothing() {
    let (_d, mut s) = store();
    s.view_create("a", None, None, false, &prov("agent:a"))
        .unwrap();
    s.view_create("b", None, None, false, &prov("agent:a"))
        .unwrap();
    let ca = commit_create(&mut s, "a", "c-AAAAA");
    let first = s.put_alt_set(&set(&["a", "b"])).unwrap();
    let moved = s
        .put_alt_set(&AltSet {
            stable: Some(first),
            prev: Some(first),
            title: "renamed".into(),
            ..set(&["a", "b"])
        })
        .unwrap();
    let p = s
        .propose_changes("a", "pick a", None, prov("agent:a"), vec![ca], None)
        .unwrap();
    let before = s.write_count();
    let e = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: Provenance {
                role: Role::Approver,
                ..prov("human:qa")
            },
            alt: Some(AltLanding {
                set: first,
                prev: first,
                member: "a".into(),
                close: vec!["b".into()],
            }),
            relay: None,
            evidence: None,
        })
        .unwrap_err();
    assert!(e.to_string().contains("moved or is no longer open"), "{e}");
    assert_eq!(s.write_count(), before);
    assert_eq!(s.alt_heads().unwrap(), vec![(first, moved)]);
}
