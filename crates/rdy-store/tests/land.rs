//! Atomic landing (plan M2a1, atom1): one write; the final version, landing and rebase agree.
use rdy_model::*;
use rdy_store::{LandRequest, Store, ViewOpKind};
use std::collections::BTreeMap;

fn block(id: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn change(view: &str, ops: Vec<Op>, by: Provenance) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: by,
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn atom1_a_landing_with_an_acceptance_is_one_write_and_its_records_agree() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let w = prov("agent:w", Role::Writer);
    let a = prov("human:a", Role::Approver);
    let d = block("b-00001", "claim", "proposed");
    let c1 = s
        .commit_to_view(
            "work",
            change("work", vec![Op::Create { block: d.clone() }], w.clone()),
        )
        .unwrap();
    let p = s.propose("work", "p", w.clone()).unwrap();
    let later = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block("b-00002", "claim", "later"),
                }],
                w,
            ),
        )
        .unwrap();
    let accepted = block("b-00001", "claim", "accepted");
    let accept = change(
        "work",
        vec![Op::Update {
            base: d.version_hash(),
            block: accepted.clone(),
        }],
        a.clone(),
    );
    let writes = s.write_count();
    let out = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: Some(accept),
            approver: a,
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    assert_eq!(s.write_count(), writes + 1, "one batch");
    let aid = out.accept.unwrap();
    let fin = s.proposal(&out.proposal).unwrap();
    assert_eq!(
        (fin.stable, fin.prev, &fin.changes),
        (Some(p), Some(p), &vec![c1, aid])
    );
    let rec = s.landing(&out.landing).unwrap();
    assert_eq!((rec.proposal, &rec.changes), (out.proposal, &vec![c1, aid]));
    assert_eq!(
        (rec.assurance, rec.approved_version),
        (Assurance::Local, Some(p)),
        "the version the approver saw"
    );
    assert_eq!(s.canon_head().unwrap(), Some(out.landing));
    assert_eq!(
        s.canon_state().unwrap().blocks[&d.id],
        accepted.version_hash()
    );
    let head = s.view_head("work").unwrap().unwrap();
    let rebase = s.view_op(&head.head).unwrap();
    assert_eq!(
        rebase.op,
        ViewOpKind::Rebase {
            landing: out.landing,
            dropped: vec![c1, aid],
            remaining: vec![later],
            conflicts: vec![],
        }
    );
    assert_eq!(
        s.view_op(&rebase.prev.unwrap()).unwrap().op,
        ViewOpKind::Commit { change: aid }
    );
    assert_eq!((head.base, head.len), (Some(out.landing), 1));
    for c in [c1, aid] {
        assert_eq!(s.landing_of(&c).unwrap(), Some(out.landing));
    }
    assert_eq!(s.change(&aid).unwrap().provenance.actor, "human:a");
}

#[test]
fn atom1_an_index_failure_leaves_nothing_of_the_landing() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let w = prov("agent:w", Role::Writer);
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "claim", "x"),
            }],
            w.clone(),
        ),
    )
    .unwrap();
    let p = s.propose("work", "p", w).unwrap();
    let (canon, view) = (s.canon_head().unwrap(), s.view_head("work").unwrap());
    s.fail_next_index_apply();
    assert!(s.land(&p, prov("human:a", Role::Approver)).is_err());
    assert_eq!(
        (s.canon_head().unwrap(), s.view_head("work").unwrap()),
        (canon, view.clone())
    );
    drop(s);
    let mut s = Store::create(&dir).unwrap();
    s.reindex().unwrap();
    assert_eq!(
        (s.canon_head().unwrap(), s.view_head("work").unwrap()),
        (canon, view)
    );
    assert!(
        s.land(&p, prov("human:a", Role::Approver)).is_ok(),
        "and it lands afterwards"
    );
}
