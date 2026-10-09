//! pver1 and evk1 at the store: proposal refs, the version check at landing, relayed landings
//! and the approval evidence kind.
use rdy_model::*;
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
fn canon_version(s: &Store, b: &str) -> Hash {
    *s.canon_state().unwrap().blocks.get(&id(b)).unwrap()
}
fn text_evidence() -> ApprovalEvidence {
    ApprovalEvidence {
        kind: EvidenceKind::Text,
        options: vec![],
        choice: None,
        card_hash: None,
    }
}

#[test]
fn pver1_propose_writes_the_refs_and_propose_next_moves_the_latest() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    let c1 = create(&mut s, "v", "c-00001");
    let p1 = s.propose("v", "t", prov("agent:v")).unwrap();
    let hash = |v: Option<Vec<u8>>| v.map(|b| Hash(b.try_into().unwrap()));
    assert_eq!(
        hash(s.get_ref(&format!("proposal/{p1}")).unwrap()),
        Some(p1)
    );
    assert_eq!(hash(s.get_ref("open/v").unwrap()), Some(p1));
    let c2 = create(&mut s, "v", "c-00002");
    let p2 = s
        .propose_next(&p1, vec![c1, c2], "t2", None, prov("agent:v2"), None)
        .unwrap();
    let v2 = s.proposal(&p2).unwrap();
    assert_eq!(
        (
            v2.stable,
            v2.prev,
            &v2.changes,
            v2.provenance.actor.as_str(),
            v2.title.as_str()
        ),
        (Some(p1), Some(p1), &vec![c1, c2], "agent:v2", "t2")
    );
    assert_eq!(
        hash(s.get_ref(&format!("proposal/{p1}")).unwrap()),
        Some(p2),
        "the stable ref follows the latest"
    );
    assert!(
        s.get_ref(&format!("proposal/{p2}")).unwrap().is_none(),
        "only the stable id has a ref"
    );
    assert_eq!(hash(s.get_ref("open/v").unwrap()), Some(p1));
    s.reindex().unwrap();
    assert_eq!(
        hash(s.get_ref(&format!("proposal/{p1}")).unwrap()),
        Some(p2),
        "refs survive a reindex"
    );
}

#[test]
fn pver1_landing_an_older_version_is_proposal_moved_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    let c1 = create(&mut s, "v", "c-00001");
    let p1 = s.propose("v", "t", prov("agent:v")).unwrap();
    let c2 = create(&mut s, "v", "c-00002");
    let p2 = s
        .propose_next(&p1, vec![c1, c2], "t", None, prov("agent:v"), None)
        .unwrap();
    let end = s.log_end();
    let e = s
        .land_atomic(LandRequest {
            proposal: p1,
            accept: None,
            approver: prov("human:h"),
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap_err();
    assert!(
        matches!(e, StoreError::ProposalMoved { stable, latest } if stable == p1 && latest == p2),
        "{e}"
    );
    assert_eq!(s.log_end(), end);
    let out = s
        .land_atomic(LandRequest {
            proposal: p2,
            accept: None,
            approver: prov("human:h"),
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    let l = s.landing(&out.landing).unwrap();
    assert_eq!(
        (l.approved_version, l.changes.len(), l.assurance),
        (Some(p2), 2, Assurance::Local)
    );
    assert_eq!(
        s.get_ref("open/v").unwrap(),
        Some(vec![]),
        "the view has no open proposal after the landing"
    );
    let hash = |v: Option<Vec<u8>>| v.map(|b| Hash(b.try_into().unwrap()));
    assert_eq!(
        hash(s.get_ref(&format!("proposal/{p1}")).unwrap()),
        Some(p2),
        "the stable ref stays on the landed version"
    );
}

#[test]
fn pver1_a_relayed_landing_records_the_relay_and_assurance_relayed() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    create(&mut s, "v", "c-00001");
    let p = s.propose("v", "t", prov("agent:v")).unwrap();
    let relay = rdy_store::Relay {
        by: ActorRun {
            actor: "agent:relay".into(),
            run: Some("run-1".into()),
        },
        evidence: RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "cli".into(),
            question: Some("land it?".into()),
        },
    };
    let out = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: prov("human:h"),
            alt: None,
            relay: Some(relay.clone()),
            evidence: Some(ApprovalEvidence {
                kind: EvidenceKind::Text,
                options: vec![],
                choice: None,
                card_hash: None,
            }),
        })
        .unwrap();
    let l = s.landing(&out.landing).unwrap();
    assert_eq!(l.assurance, Assurance::Relayed);
    assert_eq!(l.relayed_by, Some(relay.by));
    assert_eq!(l.relay_evidence, Some(relay.evidence));
    assert_eq!(
        l.approved_by.actor, "human:h",
        "approved_by names the human"
    );
    assert_eq!(
        l.evidence.as_ref().map(|e| e.kind),
        Some(EvidenceKind::Text)
    );
    s.check_objects().unwrap();
}

#[test]
fn evk1_an_elicited_approval_is_local_and_a_relay_needs_a_relay_kind() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    create(&mut s, "v", "c-00001");
    let p = s.propose("v", "t", prov("agent:v")).unwrap();
    let relay = rdy_store::Relay {
        by: ActorRun {
            actor: "agent:relay".into(),
            run: None,
        },
        evidence: RelayEvidence {
            quoted: "yes".into(),
            session_ref: "cli".into(),
            question: Some("land it?".into()),
        },
    };
    let req = |relay: Option<rdy_store::Relay>, evidence: Option<ApprovalEvidence>| LandRequest {
        proposal: p,
        accept: None,
        approver: prov("human:h"),
        alt: None,
        relay,
        evidence,
    };
    let end = s.log_end();
    let e = s.land_atomic(req(Some(relay.clone()), None)).unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("evidence kind")),
        "{e}"
    );
    let elicited = ApprovalEvidence {
        kind: EvidenceKind::Elicited,
        options: vec!["land".into(), "wait".into()],
        choice: Some("land".into()),
        card_hash: None,
    };
    let e = s
        .land_atomic(req(Some(relay), Some(elicited.clone())))
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("elicited")),
        "{e}"
    );
    let e = s.land_atomic(req(None, Some(text_evidence()))).unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("needs a relay")),
        "{e}"
    );
    assert_eq!(s.log_end(), end, "nothing written");
    let out = s.land_atomic(req(None, Some(elicited.clone()))).unwrap();
    let l = s.landing(&out.landing).unwrap();
    assert_eq!(
        (l.assurance, l.relayed_by.clone(), l.evidence.clone()),
        (Assurance::Local, None, Some(elicited))
    );
    s.check_objects().unwrap();
}

#[test]
fn pver1_every_version_records_the_canon_it_was_proposed_on() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    create(&mut s, "seed", "c-00001");
    let p0 = s.propose("seed", "t", prov("agent:v")).unwrap();
    assert_eq!(s.proposal(&p0).unwrap().canon_at, None);
    let l = s.land(&p0, prov("human:h")).unwrap();
    create(&mut s, "v", "c-00002");
    let p1 = s.propose("v", "t", prov("agent:v")).unwrap();
    assert_eq!(s.proposal(&p1).unwrap().canon_at, Some(l));
    let set = s.included("v").unwrap().as_ref().clone();
    let p2 = s
        .propose_next(&p1, set, "t2", None, prov("agent:v"), None)
        .unwrap();
    assert_eq!(s.proposal(&p2).unwrap().canon_at, Some(l));
}

/// gconf1 fix round 1 (defence in depth): the store writes no proposal version with no changes,
/// whichever path asks for one.
#[test]
fn gconf1_the_store_refuses_an_empty_proposal_version() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    let e = s.propose("v", "t", prov("agent:v")).unwrap_err();
    assert!(
        matches!(e, StoreError::EmptyProposal(ref v) if v == "v"),
        "{e}"
    );
    let c1 = create(&mut s, "v", "c-00001");
    let e = s
        .propose_changes("v", "t", None, prov("agent:v"), vec![], None)
        .unwrap_err();
    assert!(matches!(e, StoreError::EmptyProposal(_)), "{e}");
    assert_eq!(s.get_ref("open/v").unwrap(), None, "nothing written");
    let p1 = s.propose("v", "t", prov("agent:v")).unwrap();
    let e = s
        .propose_next(&p1, vec![], "t2", None, prov("agent:v"), None)
        .unwrap_err();
    assert!(matches!(e, StoreError::EmptyProposal(_)), "{e}");
    let hash = |v: Option<Vec<u8>>| v.map(|b| Hash(b.try_into().unwrap()));
    assert_eq!(
        hash(s.get_ref(&format!("proposal/{p1}")).unwrap()),
        Some(p1),
        "the latest version did not move"
    );
    assert_eq!(s.proposal(&p1).unwrap().changes, vec![c1]);
}

#[test]
fn msw4_a_next_version_of_a_superseded_version_is_proposal_moved_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    let c1 = create(&mut s, "v", "c-00001");
    let p1 = s.propose("v", "t", prov("agent:v")).unwrap();
    let c2 = create(&mut s, "v", "c-00002");
    let p2 = s
        .propose_next(&p1, vec![c1, c2], "t2", None, prov("agent:v"), None)
        .unwrap();
    let end = s.log_end();
    let e = s
        .propose_next(&p1, vec![c1], "t3", None, prov("agent:v"), None)
        .unwrap_err();
    assert!(
        matches!(e, StoreError::ProposalMoved { stable, latest } if stable == p1 && latest == p2),
        "{e}"
    );
    let e = s.propose_version(&p1, &[c2]).unwrap_err();
    assert!(matches!(e, StoreError::ProposalMoved { .. }), "{e}");
    assert_eq!(s.log_end(), end, "nothing written");
    let latest = s
        .get_ref(&format!("proposal/{p1}"))
        .unwrap()
        .map(|b| Hash(b.try_into().unwrap()));
    assert_eq!(latest, Some(p2), "the ref did not move");
}
