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
        citations: vec![],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("x".into()),
            role: Role::Reviewer,
            at_ms: 7,
        },
    }
}

#[test]
fn rvw1_kind_17_and_format_10() {
    assert_eq!(kinds::REVIEW, 17);
    assert_eq!(rdy_store::format::STORE_FORMAT, 17);
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
    // final review M6: a deliberate refusal, not damage
    match s.put_dismissal(&review(k, "second, by someone else")) {
        Err(StoreError::Refused(m)) => {
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

use rdy_store::review::{MENTION_REF, mention_ref};

fn mention(block: &str, entity: &str, verdict: Verdict, at_ms: i64) -> Review {
    Review {
        subject: ReviewSubject::Mention {
            block: BlockId::parse(block).unwrap(),
            entity: BlockId::parse(entity).unwrap(),
        },
        case_hash: None,
        verdict,
        reason: "an everyday word here".into(),
        citations: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: Some("r".into()),
            role: Role::Writer,
            at_ms,
        },
    }
}

#[test]
fn cc1_format_10() {
    assert_eq!(rdy_store::format::STORE_FORMAT, 17);
}

#[test]
fn cc1_a_mention_review_sets_its_ref_and_the_latest_wins() {
    let (_d, mut s) = store();
    let (b, e) = ("c-AAAAA", "t-BBBBB");
    let h1 = s
        .put_mention_review(&mention(b, e, Verdict::Dismiss, 1))
        .unwrap();
    let key = mention_ref(&BlockId::parse(b).unwrap(), &BlockId::parse(e).unwrap());
    assert_eq!(key, format!("{MENTION_REF}{b}/{e}"));
    assert_eq!(s.get_ref(&key).unwrap().unwrap(), h1.0.to_vec());
    let h2 = s
        .put_mention_review(&mention(b, e, Verdict::Restore, 2))
        .unwrap();
    assert_ne!(h1, h2);
    assert_eq!(
        s.mention_reviews().unwrap(),
        vec![(BlockId::parse(b).unwrap(), BlockId::parse(e).unwrap(), h2)]
    );
    assert_eq!(
        s.review(&h1).unwrap().verdict,
        Verdict::Dismiss,
        "the older review stays stored"
    );
    assert_eq!(s.review(&h2).unwrap().vkey(), None);
}

#[test]
fn cc1_the_same_review_again_writes_nothing() {
    let (_d, mut s) = store();
    let r = mention("c-AAAAA", "t-BBBBB", Verdict::Dismiss, 1);
    let h = s.put_mention_review(&r).unwrap();
    let writes = s.write_count();
    assert_eq!(s.put_mention_review(&r).unwrap(), h);
    assert_eq!(s.write_count(), writes, "a retry writes nothing");
}

#[test]
fn cc1_a_tension_review_is_not_a_mention_review_and_the_reverse() {
    let (_d, mut s) = store();
    assert!(s.put_mention_review(&review(Hash([5; 32]), "x")).is_err());
    assert!(
        s.put_dismissal(&mention("c-AAAAA", "t-BBBBB", Verdict::Dismiss, 1))
            .is_err()
    );
}

#[test]
fn cc1_mention_reviews_survive_reindex_and_fsck_checks_them() {
    let (_d, mut s) = store();
    s.put_mention_review(&mention("c-AAAAA", "t-BBBBB", Verdict::Dismiss, 1))
        .unwrap();
    s.reindex().unwrap();
    assert_eq!(s.mention_reviews().unwrap().len(), 1);
    assert_eq!(
        s.check_objects().unwrap().reviews,
        1,
        "fsck counts the mention review"
    );
}

#[test]
fn cc1_the_general_ref_api_refuses_a_mention_ref() {
    let (_d, mut s) = store();
    let name = format!("{MENTION_REF}c-AAAAA/t-BBBBB");
    assert!(s.set_refs(vec![(name, Hash([11; 32]).0.to_vec())]).is_err());
}

/// guard: existing tension reviews keep their bytes, so their hashes do not move.
#[test]
fn cc1_appending_variants_keeps_tension_review_hashes() {
    let r = review(Hash([5; 32]), "fine");
    let bytes = postcard::to_allocvec(&r).unwrap();
    assert_eq!(bytes[0], 0, "Tension stays variant 0");
}

#[test]
fn cc1_fsck_refuses_a_mention_ref_to_a_tension_review() {
    let (_d, mut s) = store();
    let h = s.put_dismissal(&review(Hash([17; 32]), "fine")).unwrap();
    s.set_refs_unchecked_for_tests(vec![(
        format!("{MENTION_REF}c-AAAAA/t-BBBBB"),
        h.0.to_vec(),
    )])
    .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(
            m.contains(&format!(
                "mention c-AAAAA/t-BBBBB: review {h} is not a mention review"
            )),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cc1_fsck_refuses_a_mention_ref_to_another_mentions_review() {
    let (_d, mut s) = store();
    let h = s
        .put_mention_review(&mention("c-AAAAA", "t-BBBBB", Verdict::Dismiss, 1))
        .unwrap();
    s.set_refs_unchecked_for_tests(vec![(
        format!("{MENTION_REF}c-CCCCC/t-BBBBB"),
        h.0.to_vec(),
    )])
    .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(
            m.contains(&format!(
                "mention c-CCCCC/t-BBBBB: review {h} is for another mention"
            )),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cc1_fsck_refuses_a_dismiss_ref_to_a_restore_review() {
    let (_d, mut s) = store();
    let k = Hash([18; 32]);
    let h = s
        .put_mention_review(&mention("c-AAAAA", "t-BBBBB", Verdict::Restore, 1))
        .unwrap();
    s.set_refs_unchecked_for_tests(vec![(format!("{DISMISS_REF}{k}"), h.0.to_vec())])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(
            m.contains(&format!("dismissal {k}: review {h} is not a dismissal")),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cc1_fsck_refuses_a_malformed_mention_key() {
    let m = fsck_after(vec![(
        format!("{MENTION_REF}c-AAAAA"),
        Hash([19; 32]).0.to_vec(),
    )]);
    assert!(m.contains("mention/c-AAAAA"), "{m}");
    assert!(
        m.contains("does not name a mention by its block and entity"),
        "{m}"
    );
}

#[test]
fn cc1_fsck_refuses_a_mention_ref_to_a_missing_review() {
    let h = Hash([7; 32]);
    let m = fsck_after(vec![(
        format!("{MENTION_REF}c-AAAAA/t-BBBBB"),
        h.0.to_vec(),
    )]);
    assert!(
        m.contains(&format!(
            "mention c-AAAAA/t-BBBBB: {h} is not a stored review"
        )),
        "{m}"
    );
}

#[test]
fn cc1_fsck_refuses_a_mention_ref_to_an_object_of_another_kind() {
    let (_d, mut s) = store();
    let blob = s.put_blob(b"not a review").unwrap();
    s.set_refs_unchecked_for_tests(vec![(
        format!("{MENTION_REF}c-AAAAA/t-BBBBB"),
        blob.0.to_vec(),
    )])
    .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(
            m.contains(&format!(
                "mention c-AAAAA/t-BBBBB: {blob} is not a stored review"
            )),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

/// final review M6: put_dismissal and put_mention_review refuse a review of the wrong shape as
/// StoreError::Refused (a refusal, not damage), and write nothing.
#[test]
fn rvw1_a_review_of_the_wrong_shape_is_refused_not_damage() {
    let (_d, mut s) = store();
    let writes = s.write_count();
    let m = mention("c-AAAAA", "t-BBBBB", Verdict::Dismiss, 1);
    assert!(matches!(s.put_dismissal(&m), Err(StoreError::Refused(_))));
    assert!(matches!(
        s.put_mention_review(&review(Hash([20; 32]), "fine")),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(s.write_count(), writes);
}
