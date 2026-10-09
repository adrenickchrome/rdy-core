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
    s.set_refs_unchecked_for_tests(vec![(format!("{DISMISS_REF}{k1}"), h2.0.to_vec())])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(m.contains("another tension"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rvw1_a_second_dismissal_of_one_vkey_is_refused() {
    let (_d, mut s) = store();
    let k = Hash([6; 32]);
    let h1 = s.put_dismissal(&review(k, "first")).unwrap();
    match s.put_dismissal(&review(k, "second, by someone else")) {
        Err(StoreError::Decode(m)) => {
            assert!(m.contains("already dismissed"), "{m}");
            assert!(m.contains(&h1.to_string()), "{m}");
        }
        other => panic!("{other:?}"),
    }
    let bytes = s.get_ref(&format!("{DISMISS_REF}{k}")).unwrap().unwrap();
    assert_eq!(bytes, h1.0.to_vec(), "the ref still names the first review");
    assert_eq!(s.dismissals().unwrap(), vec![(k, h1)]);
    assert_eq!(s.check_objects().unwrap().reviews, 1);
}

#[test]
fn rvw1_the_same_dismissal_again_is_an_idempotent_retry_that_writes_nothing() {
    let (_d, mut s) = store();
    let k = Hash([7; 32]);
    let h = s.put_dismissal(&review(k, "fine")).unwrap();
    let writes = s.write_count();
    assert_eq!(s.put_dismissal(&review(k, "fine")).unwrap(), h);
    assert_eq!(s.write_count(), writes, "a retry writes nothing");
    assert_eq!(s.dismissals().unwrap(), vec![(k, h)]);
}

#[test]
fn rvw1_a_dismissal_is_one_batch_of_the_review_and_its_ref() {
    let (_d, mut s) = store();
    s.put_dismissal(&review(Hash([8; 32]), "fine")).unwrap();
    let kinds_written: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(kinds_written, vec![kinds::REVIEW, kinds::REF_UPDATE]);
}

#[test]
fn rvw1_the_general_ref_api_refuses_a_dismiss_ref() {
    // final review M12: only put_dismissal writes a dismiss/ ref (it is never rewritten)
    let (_d, mut s) = store();
    let k = Hash([9; 32]);
    let h = s.put_dismissal(&review(k, "fine")).unwrap();
    for name in [
        format!("{DISMISS_REF}{k}"),
        format!("{DISMISS_REF}{}", Hash([10; 32])),
    ] {
        match s.set_refs(vec![(name.clone(), Hash([11; 32]).0.to_vec())]) {
            Err(StoreError::Decode(m)) => assert!(m.contains("put_dismissal"), "{m}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert_eq!(s.dismissals().unwrap(), vec![(k, h)]);
}

/// The fsck error of a store whose refs `refs` were forged in.
fn fsck_after(refs: Vec<(String, Vec<u8>)>) -> String {
    let (_d, mut s) = store();
    s.put_dismissal(&review(Hash([12; 32]), "fine")).unwrap();
    s.set_refs_unchecked_for_tests(refs).unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}

#[test]
fn rvw1_fsck_refuses_a_dismiss_ref_to_a_missing_review() {
    let k = Hash([13; 32]);
    let m = fsck_after(vec![(
        format!("{DISMISS_REF}{k}"),
        Hash([14; 32]).0.to_vec(),
    )]);
    assert!(m.contains("is not a stored review"), "{m}");
    assert!(m.contains(&k.to_string()), "{m}");
}

#[test]
fn rvw1_fsck_refuses_a_dismiss_ref_to_an_object_of_another_kind() {
    let (_d, mut s) = store();
    let blob = s.put_blob(b"not a review").unwrap();
    let k = Hash([15; 32]);
    s.set_refs_unchecked_for_tests(vec![(format!("{DISMISS_REF}{k}"), blob.0.to_vec())])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => {
            assert!(m.contains("is not a stored review"), "{m}");
            assert!(m.contains(&k.to_string()), "{m}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn rvw1_fsck_refuses_a_malformed_dismiss_key() {
    let m = fsck_after(vec![(
        format!("{DISMISS_REF}not-a-vkey"),
        Hash([16; 32]).0.to_vec(),
    )]);
    assert!(m.contains("dismiss/not-a-vkey"), "{m}");
    assert!(m.contains("tension vkey"), "{m}");
}
