//! Objects read back in their kind (plan M2a1, repr1): every id is the stored hash; a read of
//! the wrong kind names what was found.
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
    }
}

#[test]
fn repr1_reading_the_wrong_kind_names_what_was_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    let h = s.put(kinds::SNAPSHOT, &StateSnapshot::default()).unwrap();
    for (what, r) in [
        ("proposal", s.proposal(&h).map(|_| ())),
        ("landing", s.landing(&h).map(|_| ())),
        ("change", s.change(&h).map(|_| ())),
        ("tag", s.read_tag(&h).map(|_| ())),
    ] {
        match r {
            Err(StoreError::Decode(m)) => assert!(m.contains(&format!("is not a {what}")), "{m}"),
            other => panic!("{what}: {other:?}"),
        }
    }
    assert!(matches!(
        s.change(&Hash::of(b"none")),
        Err(StoreError::NotFound(_))
    ));
}

/// Review Focus 1 and 6: what this binary writes reads back equal, with the stored hash as id,
/// and the landing carries M2's values.
#[test]
fn repr1_objects_read_back_in_their_kind_with_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    let linked = Change {
        links: ChangeLinks {
            client_nonce: Some("n".into()),
            ..Default::default()
        },
        ..change(
            "work",
            vec![Op::Create {
                block: block("b-AAAAA", "x"),
            }],
        )
    };
    let c = s.commit_to_view("work", linked.clone()).unwrap();
    assert!(s.has_kind(kinds::CHANGE, &c).unwrap());
    let rc = s.change(&c).unwrap();
    assert_eq!((rc.id(), &rc.links), (c, &linked.links));
    let p = s.propose("work", "first", prov(Role::Writer)).unwrap();
    assert!(s.has_kind(kinds::PROPOSAL, &p).unwrap());
    let rp = s.proposal(&p).unwrap();
    assert_eq!(rp.id(), p);
    assert_eq!(
        (rp.stable, rp.prev, &rp.alternative, &rp.changes),
        (None, None, &None, &vec![c])
    );
    let l = s.land(&p, prov(Role::Approver)).unwrap();
    assert!(s.has_kind(kinds::LANDING, &l).unwrap());
    let rl = s.landing(&l).unwrap();
    assert_eq!(
        (rl.id(), rl.proposal, rl.prev, &rl.changes),
        (l, p, None, &vec![c])
    );
    assert!(rl.checks.is_empty() && rl.chosen.is_none());
    assert_eq!(
        (rl.assurance, rl.approved_version),
        (Assurance::Local, Some(p)),
        "Review Focus 6: M2 writes local"
    );
    assert!(rl.relayed_by.is_none() && rl.relay_evidence.is_none() && rl.rebased_from.is_empty());
    assert_eq!(s.landings().unwrap(), vec![(l, rl)]);
    let v = s.propose_version(&p, &[c]).unwrap();
    let rv = s.proposal(&v).unwrap();
    assert_eq!(
        (rv.id(), rv.stable, rv.prev, &rv.changes),
        (v, Some(p), Some(p), &vec![c, c])
    );
    let v2 = s.propose_version(&v, &[]).unwrap();
    let rv2 = s.proposal(&v2).unwrap();
    assert_eq!(
        (rv2.stable, rv2.prev),
        (Some(p), Some(v)),
        "stable stays the first version"
    );
}

/// Review Focus 6: every post-spec field set, through the store and a reindex.
#[test]
fn repr1_a_landing_with_every_field_set_survives_the_store_and_a_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    let by = prov(Role::Approver);
    let l = LandingRecord {
        proposal: Hash::of(b"p"),
        prev: None,
        state: StateSnapshot::default().id(),
        approved_by: by,
        changes: vec![Hash::of(b"c")],
        chosen: Some(AltChoice {
            set: Hash::of(b"set"),
            member: "alt-a".into(),
        }),
        checks: vec![],
        assurance: Assurance::Relayed,
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("run-1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "land it".into(),
            session_ref: "sess-1".into(),
            question: Some("land it?".into()),
        }),
        approved_version: Some(Hash::of(b"p")),
        rebased_from: vec![Hash::of(b"an older landing")],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Text,
            options: vec!["land".into(), "wait".into()],
            choice: Some("land".into()),
            card_hash: None,
        }),
    };
    let lh = s
        .write(vec![(kinds::LANDING, postcard::to_allocvec(&l).unwrap())])
        .unwrap()[0];
    assert_eq!(lh, l.id());
    assert_eq!(s.landing(&lh).unwrap(), l);
    s.reindex().unwrap();
    assert_eq!(s.landing(&lh).unwrap(), l);
}
