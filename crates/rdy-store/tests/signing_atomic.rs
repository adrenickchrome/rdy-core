//! sig8: a signed act and its signatures share one batch (D-1Z951). The store prepares a
//! landing or a tag without writing, and its commit writes the act's frames and the signatures
//! of its subject in one write, or nothing.
use rdy_model::signature::{Signature, StatementKind};
use rdy_model::*;
use rdy_store::{LandRequest, Store, kinds};
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

/// A structurally valid signature of `subject` in `kind`'s namespace. The store checks
/// structure only; the engine checks the cryptography before it writes.
fn sig(subject: Hash, kind: StatementKind, n: u8) -> Signature {
    Signature {
        subject,
        namespace: kind.namespace().into(),
        statement: format!("statement {n}").into_bytes(),
        sig: vec![n; 8],
        signed_by: prov("agent:relay", Role::Writer),
    }
}

/// A store whose view `work` holds one proposed change; returns the proposal version.
fn proposed(s: &mut Store, id: &str) -> Hash {
    let w = prov("agent:w", Role::Writer);
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block(id, "claim", "x"),
            }],
            w.clone(),
        ),
    )
    .unwrap();
    s.propose("work", "p", w).unwrap()
}

fn req(p: Hash) -> LandRequest {
    LandRequest {
        proposal: p,
        accept: None,
        approver: prov("human:a", Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
    }
}

fn tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov("human:a", Role::Approver),
    }
}

fn relay_answer(name: &str) -> Answer {
    Answer {
        item: format!("tag:{name}"),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: prov("human:a", Role::Approver),
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("r1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "tag it".into(),
            session_ref: "chat".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        note: None,
    }
}

/// The kinds of the last batch, in write order.
fn last_kinds(s: &Store) -> Vec<u8> {
    s.last_batch().iter().map(|(k, _)| *k).collect()
}

#[test]
fn sig8_a_prepared_landing_writes_nothing_until_its_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let p = proposed(&mut s, "b-00001");
    let (end, canon) = (s.log_end(), s.canon_head().unwrap());
    let prepared = s.prepare_land(req(p)).unwrap();
    assert_eq!(s.log_end(), end, "the preparation writes nothing");
    assert_eq!(s.canon_head().unwrap(), canon);
    assert_eq!(
        prepared.landing().prev,
        canon,
        "the landing builds on canon now"
    );
    assert_eq!(prepared.landing().approved_version, Some(p));
    let lid = prepared.hash();
    assert_eq!(
        lid,
        Hash::of(&postcard::to_allocvec(prepared.landing()).unwrap())
    );

    let sigs = [
        sig(lid, StatementKind::Relay, 1),
        sig(lid, StatementKind::Approval, 2),
    ];
    let out = s.commit_land(prepared, &sigs).unwrap();
    assert_eq!(out.landing, lid);
    assert_eq!(s.canon_head().unwrap(), Some(lid));
    // one batch: the landing, then its refs, then the signatures in the order given
    let batch = s.last_batch().to_vec();
    let at = |h: &Hash| batch.iter().position(|(_, x)| x == h).unwrap();
    let ids: Vec<Hash> = sigs.iter().map(Signature::id).collect();
    assert!(at(&lid) < at(&ids[0]), "{batch:?}");
    assert!(at(&ids[0]) < at(&ids[1]), "relay before person: {batch:?}");
    assert_eq!(
        last_kinds(&s)[at(&ids[0])..],
        [kinds::SIGNATURE, kinds::SIGNATURE],
        "the signatures end the batch"
    );
    let mut stored = s.reader().signatures_of(&lid).unwrap();
    stored.sort();
    let mut want = ids.clone();
    want.sort();
    assert_eq!(stored, want);
}

#[test]
fn sig8_land_atomic_is_a_preparation_and_a_commit_with_no_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let p = proposed(&mut s, "b-00001");
    let prepared = s.prepare_land(req(p)).unwrap();
    let lid = prepared.hash();
    drop(prepared);
    let out = s.land_atomic(req(p)).unwrap();
    assert_eq!(out.landing, lid, "the same landing, byte for byte");
    assert!(!last_kinds(&s).contains(&kinds::SIGNATURE));
    assert!(s.reader().signatures_of(&lid).unwrap().is_empty());
}

#[test]
fn sig8_a_failed_commit_leaves_no_part_of_the_act() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let p = proposed(&mut s, "b-00001");
    let (end, canon, view) = (
        s.log_end(),
        s.canon_head().unwrap(),
        s.view_head("work").unwrap(),
    );

    // a signature of another subject is refused before anything is written
    let prepared = s.prepare_land(req(p)).unwrap();
    let stray = sig(Hash([9; 32]), StatementKind::Approval, 3);
    assert!(s.commit_land(prepared, &[stray]).is_err());
    assert_eq!(s.log_end(), end);
    assert_eq!(s.canon_head().unwrap(), canon);

    // an index failure after the append leaves nothing of the landing or its signatures
    let prepared = s.prepare_land(req(p)).unwrap();
    let lid = prepared.hash();
    let g = sig(lid, StatementKind::Approval, 4);
    s.fail_next_index_apply();
    assert!(s.commit_land(prepared, std::slice::from_ref(&g)).is_err());
    assert_eq!(s.log_end(), end);
    assert_eq!(
        (s.canon_head().unwrap(), s.view_head("work").unwrap()),
        (canon, view.clone())
    );
    drop(s);
    let mut s = Store::create(&dir).unwrap();
    s.reindex().unwrap();
    assert_eq!(s.canon_head().unwrap(), canon);
    assert!(s.reader().signatures_of(&lid).unwrap().is_empty());
    assert!(
        s.reader().signature(&g.id()).is_err(),
        "no signature record"
    );

    // it lands afterwards, with its signature
    let prepared = s.prepare_land(req(p)).unwrap();
    assert_eq!(prepared.hash(), lid);
    s.commit_land(prepared, std::slice::from_ref(&g)).unwrap();
    assert_eq!(s.reader().signatures_of(&lid).unwrap(), vec![g.id()]);
}

#[test]
fn sig8_a_failed_seal_of_a_held_signed_landing_loses_the_whole_batch() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let p = proposed(&mut s, "b-00001");
    let canon = s.canon_head().unwrap();
    s.set_held(true).unwrap();
    let prepared = s.prepare_land(req(p)).unwrap();
    let lid = prepared.hash();
    let g = sig(lid, StatementKind::Approval, 5);
    s.commit_land(prepared, std::slice::from_ref(&g)).unwrap();
    s.fail_next_index_apply();
    assert!(s.seal(vec![]).is_err(), "the seal fails");
    drop(s);
    let s = Store::create(&dir).unwrap();
    assert_eq!(s.canon_head().unwrap(), canon, "no landing");
    assert!(
        s.reader().signatures_of(&lid).unwrap().is_empty(),
        "no signature"
    );
    s.verify().unwrap();
    s.check_objects().unwrap();
}

#[test]
fn sig8_a_prepared_tag_holds_the_final_answer_and_signs_in_one_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let p = proposed(&mut s, "b-00001");
    let lid = s.land(&p, prov("human:a", Role::Approver)).unwrap();

    // a plain tag: no answer
    let plain = s.prepare_tag(&tag("v1.0.0", lid), None).unwrap();
    assert!(plain.answer().is_none());
    let end = s.log_end();
    let t1 = plain.hash();
    assert_eq!(s.log_end(), end, "the preparation writes nothing");
    let g1 = sig(t1, StatementKind::Tag, 6);
    assert_eq!(
        s.commit_tag(plain, std::slice::from_ref(&g1)).unwrap(),
        (t1, None)
    );
    assert_eq!(
        last_kinds(&s),
        [kinds::TAG, kinds::REF_UPDATE, kinds::SIGNATURE]
    );

    // a relayed tag: the answer targets the tag, and the signatures follow the answer
    let relayed = s
        .prepare_tag(&tag("v1.1.0", lid), Some(relay_answer("v1.1.0")))
        .unwrap();
    let t2 = relayed.hash();
    let (aid, a) = relayed.answer().unwrap();
    assert_eq!(a.targets, vec![t2], "the final answer names the tag");
    assert_eq!(aid, Hash::of(&postcard::to_allocvec(a).unwrap()));
    let sigs = [
        sig(t2, StatementKind::Relay, 7),
        sig(t2, StatementKind::Tag, 8),
    ];
    assert_eq!(s.commit_tag(relayed, &sigs).unwrap(), (t2, Some(aid)));
    assert_eq!(
        last_kinds(&s),
        [
            kinds::TAG,
            kinds::REF_UPDATE,
            kinds::ANSWER,
            kinds::REF_UPDATE,
            kinds::SIGNATURE,
            kinds::SIGNATURE
        ]
    );
    assert_eq!(s.reader().answer_of("tag:v1.1.0").unwrap().unwrap().0, aid);
    assert_eq!(s.reader().signatures_of(&t2).unwrap().len(), 2);

    // put_tag_answered is the same preparation and a commit with no signatures
    let again = s
        .prepare_tag(&tag("v1.2.0", lid), Some(relay_answer("v1.2.0")))
        .unwrap();
    let (t3, a3) = (again.hash(), again.answer().unwrap().0);
    assert_eq!(
        s.put_tag_answered(&tag("v1.2.0", lid), relay_answer("v1.2.0"))
            .unwrap(),
        (t3, a3)
    );

    // an index failure leaves nothing of the tag, its answer or its signatures
    let failing = s
        .prepare_tag(&tag("v1.3.0", lid), Some(relay_answer("v1.3.0")))
        .unwrap();
    let t4 = failing.hash();
    let end = s.log_end();
    s.fail_next_index_apply();
    assert!(
        s.commit_tag(failing, &[sig(t4, StatementKind::Relay, 9)])
            .is_err()
    );
    assert_eq!(s.log_end(), end);
    assert!(s.reader().answer_of("tag:v1.3.0").unwrap().is_none());
    assert!(s.reader().signatures_of(&t4).unwrap().is_empty());
}
