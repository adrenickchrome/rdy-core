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
fn cfmt1_kinds_15_and_16_and_format_6() {
    assert_eq!(kinds::INTENT, 15);
    assert_eq!(kinds::ALT_SET, 16);
    assert_eq!(rdy_store::format::STORE_FORMAT, 6);
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
