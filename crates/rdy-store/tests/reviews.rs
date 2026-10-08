//! rvw1: a dismissal is a REVIEW object and a ref by the tension's vkey; fsck checks the pair.
use rdy_model::*;
use rdy_store::review::DISMISS_REF;
use rdy_store::{Store, StoreError, kinds};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}
fn review(vkey: Hash, reason: &str) -> Review {
    Review {
        subject: ReviewSubject::Tension {
            id: Hash([1; 32]),
            vkey,
            kind: "edit_collision".into(),
            severity: "blocking".into(),
            sides: vec!["canon".into()],
            blocks: vec![BlockId::parse("c-AAAAA").unwrap()],
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: reason.into(),
        findings: vec![],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("x".into()),
            role: Role::Reviewer,
            at_ms: 7,
        },
    }
}

#[test]
fn rvw1_kind_17_and_format_7() {
    assert_eq!(kinds::REVIEW, 17);
    assert_eq!(rdy_store::format::STORE_FORMAT, 7);
}

#[test]
fn rvw1_a_dismissal_is_a_review_and_a_ref_by_vkey() {
    let (_d, mut s) = store();
    let k = Hash([5; 32]);
    let h = s.put_dismissal(&review(k, "fine")).unwrap();
    let bytes = s.get_ref(&format!("{DISMISS_REF}{k}")).unwrap().unwrap();
    assert_eq!(bytes, h.0.to_vec());
    assert_eq!(s.review(&h).unwrap(), review(k, "fine"));
    assert_eq!(s.review(&h).unwrap().vkey(), Some(k));
    assert_eq!(s.dismissals().unwrap(), vec![(k, h)]);
}

#[test]
fn rvw1_dismissals_survive_reindex_and_reopen() {
    let (d, mut s) = store();
    let (k1, k2) = (Hash([2; 32]), Hash([1; 32]));
    let h1 = s.put_dismissal(&review(k1, "a")).unwrap();
    let h2 = s.put_dismissal(&review(k2, "b")).unwrap();
    s.reindex().unwrap();
    assert_eq!(
        s.dismissals().unwrap(),
        vec![(k2, h2), (k1, h1)],
        "sorted by vkey"
    );
    drop(s);
    let s = Store::open(&d.path().join("s")).unwrap();
    assert_eq!(s.review(&h1).unwrap().reason, "a");
}

#[test]
fn rvw1_fsck_counts_reviews_and_checks_the_ref_names_its_own_tension() {
    let (_d, mut s) = store();
    let (k1, k2) = (Hash([3; 32]), Hash([4; 32]));
    s.put_dismissal(&review(k1, "a")).unwrap();
    let h2 = s.put_dismissal(&review(k2, "b")).unwrap();
    assert_eq!(s.check_objects().unwrap().reviews, 2);
    // point k1's ref at the review of k2
    s.set_refs(vec![(format!("{DISMISS_REF}{k1}"), h2.0.to_vec())])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(m.contains("another tension"), "{m}"),
        other => panic!("{other:?}"),
    }
}
