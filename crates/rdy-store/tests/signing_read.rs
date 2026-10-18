//! sig9: a read of a subject's signatures as of a published state leaves out every signature
//! whose frame lies at or after that state's log end: the frames of a held batch that is not
//! sealed yet, and of a batch sealed after the state. The per-subject bound counts only the
//! signatures returned.
use rdy_model::signature::{Signature, StatementKind};
use rdy_model::*;
use rdy_store::signature::MAX_PER_SUBJECT;
use rdy_store::{LandRequest, Store};
use std::collections::BTreeMap;

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

/// A store with one landing; returns it.
fn landed(s: &mut Store) -> Hash {
    let w = prov("agent:w", Role::Writer);
    let block = Block {
        id: BlockId::parse("b-00001").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "x".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    s.commit_to_view(
        "work",
        Change {
            view: "work".into(),
            ops: vec![Op::Create { block }],
            deps: vec![],
            provenance: w.clone(),
            justification: None,
            message: String::new(),
            links: ChangeLinks::default(),
            read_identity: None,
        },
    )
    .unwrap();
    let p = s.propose("work", "p", w).unwrap();
    let prepared = s
        .prepare_land(LandRequest {
            proposal: p,
            accept: None,
            approver: prov("human:a", Role::Approver),
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    s.commit_land(prepared, &[]).unwrap().landing
}

/// A structurally valid signature of `subject`, distinct by `n`.
fn sig(subject: Hash, n: u32) -> Signature {
    Signature {
        subject,
        namespace: StatementKind::Approval.namespace().into(),
        statement: format!("statement {n}").into_bytes(),
        sig: n.to_be_bytes().to_vec(),
        signed_by: prov("human:a", Role::Approver),
    }
}

fn sorted(mut v: Vec<Hash>) -> Vec<Hash> {
    v.sort();
    v
}

#[test]
fn sig9_signatures_before_leave_out_frames_at_or_after_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let l = landed(&mut s);
    let first = s.put_signature(&sig(l, 1)).unwrap();
    let published = s.log_end();
    assert_eq!(
        s.reader().signatures_before(&l, published).unwrap(),
        [first]
    );

    // a held batch's signature is in the index, but not before the published end
    s.set_held(true).unwrap();
    let held = s.put_signature(&sig(l, 2)).unwrap();
    assert_eq!(
        sorted(s.reader().signatures_of(&l).unwrap()),
        sorted(vec![first, held]),
        "the index holds the held entry"
    );
    assert_eq!(
        s.reader().signatures_before(&l, published).unwrap(),
        [first]
    );
    assert_eq!(
        sorted(s.reader().signatures_before(&l, s.log_end()).unwrap()),
        sorted(vec![first, held])
    );

    // sealed after the state: still not in it; in the state of the seal
    s.seal(vec![]).unwrap();
    assert_eq!(
        s.reader().signatures_before(&l, published).unwrap(),
        [first]
    );
    assert_eq!(
        sorted(s.reader().signatures_before(&l, s.log_end()).unwrap()),
        sorted(vec![first, held])
    );
}

#[test]
fn sig9_the_per_subject_bound_counts_only_the_signatures_before_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let l = landed(&mut s);
    let sigs: Vec<Signature> = (0..MAX_PER_SUBJECT as u32).map(|n| sig(l, n)).collect();
    s.write(rdy_store::signature::frames(&sigs).unwrap())
        .unwrap();
    let published = s.log_end();
    s.set_held(true).unwrap();
    s.put_signature(&sig(l, MAX_PER_SUBJECT as u32)).unwrap();
    assert_eq!(
        s.reader().signatures_before(&l, published).unwrap().len(),
        MAX_PER_SUBJECT,
        "the held signature does not count toward the bound"
    );
    let over = "signing input exceeds 4096 signatures per subject";
    assert!(
        matches!(s.reader().signatures_before(&l, s.log_end()), Err(rdy_store::StoreError::Refused(m)) if m == over)
    );
}
