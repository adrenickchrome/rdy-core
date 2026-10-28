//! se1: verdict reviews, the pair/ and semantic/ refs and kind TENSION; fsck checks them.
use rdy_model::*;
use rdy_store::review::{PAIR_REF, SEMANTIC_REF};
use rdy_store::{Store, kinds};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}
fn items() -> Vec<PairItem> {
    let mut v = vec![
        PairItem {
            block: BlockId::parse("c-BBBBB").unwrap(),
            content: Hash([2; 32]),
        },
        PairItem {
            block: BlockId::parse("c-AAAAA").unwrap(),
            content: Hash([1; 32]),
        },
    ];
    v.sort();
    v
}
fn prov(at_ms: i64) -> Provenance {
    Provenance {
        actor: "agent:r".into(),
        run: Some("r".into()),
        role: Role::Reviewer,
        at_ms,
    }
}
fn verdict(v: Verdict, at_ms: i64) -> Review {
    let items = items();
    Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items,
        },
        case_hash: Some(Hash([9; 32])),
        verdict: v,
        reason: String::new(),
        citations: vec![Citation {
            block: BlockId::parse("c-AAAAA").unwrap(),
            note: "A says ten, B says five".into(),
        }],
        provenance: prov(at_ms),
    }
}
fn first_version(review: Hash) -> SemanticTension {
    let items = items();
    SemanticTension {
        key: pair_key(&items),
        items,
        review,
        advice: Some("it contradicts the gate rule".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: prov(3),
    }
}
/// The acceptance of `first`, at `prev`, relayed from chat.
fn accepted(first: &SemanticTension, prev: Hash) -> SemanticTension {
    SemanticTension {
        blocking: true,
        relay: Some(RelayEvidence {
            quoted: "accept the advice".into(),
            session_ref: "chat".into(),
            question: Some("Accept?".into()),
        }),
        prev: Some(prev),
        ..first.clone()
    }
}
/// A tension verdict with its first version, as the engine writes them.
fn put_tension(s: &mut Store, at_ms: i64) -> (Hash, Hash) {
    let r = verdict(Verdict::Tension, at_ms);
    let id = Hash::of(&postcard::to_allocvec(&r).unwrap());
    let (h, t) = s.put_verdict(&r, Some(&first_version(id))).unwrap();
    (h, t.unwrap())
}

#[test]
fn se1_kind_18_and_format_10() {
    assert_eq!(kinds::TENSION, 18);
    assert_eq!(rdy_store::format::LAST_SCALAR_STORE_FORMAT, 18);
}

#[test]
fn se1_the_pair_key_ignores_item_order_and_names_both_contents() {
    let a = items();
    let mut b = a.clone();
    b.reverse();
    assert_eq!(pair_key(&a), pair_key(&b));
    let mut c = a.clone();
    c[0].content = Hash([7; 32]);
    assert_ne!(pair_key(&a), pair_key(&c));
}

#[test]
fn se1_a_no_tension_verdict_sets_the_pair_ref_only() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    let (h, t) = s
        .put_verdict(&verdict(Verdict::NoTension, 1), None)
        .unwrap();
    assert_eq!(t, None);
    assert_eq!(
        s.get_ref(&format!("{PAIR_REF}{key}")).unwrap().unwrap(),
        h.0.to_vec()
    );
    assert_eq!(s.get_ref(&format!("{SEMANTIC_REF}{key}")).unwrap(), None);
    assert_eq!(s.verdicts().unwrap(), vec![(key, h)]);
    assert_eq!(s.review(&h).unwrap().vkey(), None);
}

#[test]
fn se1_a_tension_verdict_writes_the_tension_and_both_refs() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    let (h, t) = put_tension(&mut s, 1);
    assert_eq!(s.verdicts().unwrap(), vec![(key, h)]);
    assert_eq!(s.semantic_tensions().unwrap(), vec![(key, t)]);
    let st = s.semantic_tension(&t).unwrap();
    assert_eq!((st.review, st.blocking, st.prev), (h, false, None));
}

#[test]
fn se1_a_tension_verdict_overrides_a_no_tension_and_nothing_else_overrides() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    s.put_verdict(&verdict(Verdict::NoTension, 1), None)
        .unwrap();
    let e = s
        .put_verdict(&verdict(Verdict::NoTension, 2), None)
        .unwrap_err();
    assert!(
        e.to_string()
            .contains(&format!("pair {key} is settled by review")),
        "{e}"
    );
    let (h, _) = put_tension(&mut s, 3);
    assert_eq!(
        s.verdicts().unwrap(),
        vec![(key, h)],
        "a tension rewrites the ref"
    );
    assert!(
        s.put_verdict(&verdict(Verdict::NoTension, 4), None)
            .is_err()
    );
    let r = verdict(Verdict::Tension, 5);
    let id = Hash::of(&postcard::to_allocvec(&r).unwrap());
    assert!(
        s.put_verdict(&r, Some(&first_version(id))).is_err(),
        "a second tension is refused"
    );
}

#[test]
fn se1_the_same_verdict_again_writes_nothing() {
    let (_d, mut s) = store();
    let r = verdict(Verdict::NoTension, 1);
    let first = s.put_verdict(&r, None).unwrap();
    let writes = s.write_count();
    assert_eq!(s.put_verdict(&r, None).unwrap(), first);
    assert_eq!(s.write_count(), writes);
}

#[test]
fn se1_a_verdict_needs_its_first_version_and_only_a_tension_has_one() {
    let (_d, mut s) = store();
    let r = verdict(Verdict::Tension, 1);
    assert!(s.put_verdict(&r, None).is_err());
    let id = Hash::of(&postcard::to_allocvec(&r).unwrap());
    assert!(
        s.put_verdict(&verdict(Verdict::NoTension, 1), Some(&first_version(id)))
            .is_err()
    );
    let mut wrong = first_version(Hash([4; 32]));
    assert!(
        s.put_verdict(&r, Some(&wrong)).is_err(),
        "the version names its verdict"
    );
    wrong = first_version(id);
    wrong.blocking = true;
    assert!(
        s.put_verdict(&r, Some(&wrong)).is_err(),
        "a first version is never blocking"
    );
    wrong = first_version(id);
    wrong.relay = Some(RelayEvidence {
        quoted: "x".into(),
        session_ref: "chat".into(),
        question: None,
    });
    assert!(
        s.put_verdict(&r, Some(&wrong)).is_err(),
        "a first version carries no relay"
    );
}

#[test]
fn se1_an_acceptance_needs_the_latest_as_prev() {
    let (_d, mut s) = store();
    let (h, t1) = put_tension(&mut s, 1);
    let first = first_version(h);
    assert!(
        s.put_tension_versions(&[accepted(&first, Hash([8; 32]))])
            .is_err()
    );
    let t2 = s.put_tension_versions(&[accepted(&first, t1)]).unwrap();
    assert_eq!(
        s.semantic_tensions().unwrap(),
        vec![(pair_key(&items()), t2[0])]
    );
    let v = s.semantic_tension(&t2[0]).unwrap();
    assert!(v.blocking && v.relay.is_some());
    let e = s
        .put_tension_versions(&[accepted(&first, t2[0])])
        .unwrap_err();
    assert!(e.to_string().contains("is accepted already"), "{e}");
}

#[test]
fn se1_an_acceptance_keeps_the_pair_verdict_and_advice() {
    let (_d, mut s) = store();
    let (h, t1) = put_tension(&mut s, 1);
    let first = first_version(h);
    let mut not_blocking = accepted(&first, t1);
    not_blocking.blocking = false;
    let mut no_advice = accepted(&first, t1);
    no_advice.advice = None;
    let mut other_review = accepted(&first, t1);
    other_review.review = Hash([4; 32]);
    for bad in [not_blocking, no_advice, other_review] {
        assert!(s.put_tension_versions(&[bad]).is_err());
    }
    assert_eq!(
        s.semantic_tensions().unwrap(),
        vec![(pair_key(&items()), t1)],
        "nothing written"
    );
}

#[test]
fn se1_a_batch_of_acceptances_is_all_or_nothing() {
    let (_d, mut s) = store();
    let (h, t1) = put_tension(&mut s, 1);
    let good = accepted(&first_version(h), t1);
    let bad = accepted(&first_version(h), Hash([8; 32]));
    let writes = s.write_count();
    assert!(s.put_tension_versions(&[good.clone(), bad]).is_err());
    assert_eq!(s.write_count(), writes);
    let e = s.put_tension_versions(&[good.clone(), good]).unwrap_err();
    assert!(e.to_string().contains("appears twice"), "{e}");
}

#[test]
fn se1_records_survive_reindex_and_fsck_checks_them() {
    let (_d, mut s) = store();
    put_tension(&mut s, 1);
    s.reindex().unwrap();
    assert_eq!(s.verdicts().unwrap().len(), 1);
    let c = s.check_objects().unwrap();
    assert_eq!((c.reviews, c.tensions), (1, 1));
}

#[test]
fn se1_the_general_ref_api_refuses_pair_and_semantic_refs() {
    let (_d, mut s) = store();
    for p in [PAIR_REF, SEMANTIC_REF] {
        let name = format!("{p}{}", Hash([3; 32]));
        assert!(
            s.set_refs(vec![(name, Hash([11; 32]).0.to_vec())]).is_err(),
            "{p}"
        );
    }
}

#[test]
fn se1_a_verdict_is_not_a_dismissal() {
    let (_d, mut s) = store();
    assert!(s.put_dismissal(&verdict(Verdict::NoTension, 1)).is_err());
}

/// guard: appending variants keeps the bytes of earlier reviews.
#[test]
fn se1_appending_variants_keeps_earlier_review_bytes() {
    let r = Review {
        subject: ReviewSubject::Tension {
            id: Hash([1; 32]),
            vkey: Hash([2; 32]),
            kind: "fork".into(),
            severity: "blocking".into(),
            sides: vec!["canon".into()],
            blocks: vec![],
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "fine".into(),
        citations: vec![],
        provenance: prov(1),
    };
    let bytes = postcard::to_allocvec(&r).unwrap();
    assert_eq!(bytes[0], 0, "Tension stays variant 0");
}

#[test]
fn se1_a_tension_verdict_is_one_batch() {
    let (_d, mut s) = store();
    put_tension(&mut s, 1);
    let written: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        written,
        vec![
            kinds::REVIEW,
            kinds::TENSION,
            kinds::REF_UPDATE,
            kinds::REF_UPDATE
        ]
    );
}

/// The fsck error of `s`, which must fail.
fn fsck_error(s: &Store) -> String {
    match s.check_objects() {
        Err(rdy_store::StoreError::Decode(m)) => m,
        other => panic!("{other:?}"),
    }
}

#[test]
fn se1_fsck_refuses_a_pair_ref_that_names_a_dismissal() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    let h = s.put_dismissal(&se1_tension_review()).unwrap();
    s.set_refs_unchecked_for_tests(vec![(format!("{PAIR_REF}{key}"), h.0.to_vec())])
        .unwrap();
    let m = fsck_error(&s);
    assert!(
        m.contains(&format!(
            "pair {key}: review {h} is not a verdict on this pair"
        )),
        "{m}"
    );
}

#[test]
fn se1_fsck_refuses_a_tension_that_does_not_follow_from_the_pairs_verdict() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    let (no, _) = s
        .put_verdict(&verdict(Verdict::NoTension, 1), None)
        .unwrap();
    let (_, t) = put_tension(&mut s, 2);
    // the pair ref names the no_tension review again; the tension's first version names the other
    s.set_refs_unchecked_for_tests(vec![(format!("{PAIR_REF}{key}"), no.0.to_vec())])
        .unwrap();
    let m = fsck_error(&s);
    assert!(
        m.contains(&format!(
            "semantic tension {key}: version {t} does not follow from the pair's tension verdict"
        )),
        "{m}"
    );
}

#[test]
fn se1_fsck_refuses_a_later_version_that_is_not_an_acceptance() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    let (h, t1) = put_tension(&mut s, 1);
    let mut bad = accepted(&first_version(h), t1);
    bad.advice = Some("other advice".into());
    let b = s.put(kinds::TENSION, &bad).unwrap();
    s.set_refs_unchecked_for_tests(vec![(format!("{SEMANTIC_REF}{key}"), b.0.to_vec())])
        .unwrap();
    let m = fsck_error(&s);
    assert!(
        m.contains(&format!(
            "semantic tension {key}: version {b} is not an acceptance of the first"
        )),
        "{m}"
    );
}

#[test]
fn se1_fsck_refuses_a_tension_verdict_with_no_semantic_record() {
    let (_d, mut s) = store();
    let key = pair_key(&items());
    // a tension verdict whose first version and semantic/ ref were never written
    let h = s.put(kinds::REVIEW, &verdict(Verdict::Tension, 1)).unwrap();
    s.set_refs_unchecked_for_tests(vec![(format!("{PAIR_REF}{key}"), h.0.to_vec())])
        .unwrap();
    let m = fsck_error(&s);
    assert!(
        m.contains(&format!(
            "pair {key}: tension verdict {h} has no semantic tension record"
        )),
        "{m}"
    );
}

#[test]
fn se1_fsck_refuses_a_pair_review_that_is_not_two_blocks() {
    let block = BlockId::parse("c-AAAAA").unwrap();
    let same_block = vec![
        PairItem {
            block: block.clone(),
            content: Hash([1; 32]),
        },
        PairItem {
            block: block.clone(),
            content: Hash([2; 32]),
        },
    ];
    let mut three = items();
    three.push(PairItem {
        block: BlockId::parse("c-CCCCC").unwrap(),
        content: Hash([3; 32]),
    });
    three.sort();
    for items in [same_block, three, vec![items()[0].clone()]] {
        let (_d, mut s) = store();
        let key = pair_key(&items);
        let r = Review {
            subject: ReviewSubject::Pair { key, items },
            ..verdict(Verdict::NoTension, 1)
        };
        let h = s.put(kinds::REVIEW, &r).unwrap();
        s.set_refs_unchecked_for_tests(vec![(format!("{PAIR_REF}{key}"), h.0.to_vec())])
            .unwrap();
        let m = fsck_error(&s);
        assert!(
            m.contains(&format!(
                "pair {key}: review {h} is not a verdict on this pair"
            )),
            "{m}"
        );
    }
}

/// A tension review over the pair's blocks, for a dismissal.
fn se1_tension_review() -> Review {
    Review {
        subject: ReviewSubject::Tension {
            id: Hash([1; 32]),
            vkey: Hash([2; 32]),
            kind: "fork".into(),
            severity: "blocking".into(),
            sides: vec!["canon".into()],
            blocks: vec![],
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "fine".into(),
        citations: vec![],
        provenance: prov(1),
    }
}

/// A deliberate refusal of a verdict or a version is `Refused`, never `Decode`: `Decode` is a
/// stored object or ref that does not decode (damage).
#[test]
fn se1_a_refused_verdict_or_version_is_refused_not_decode() {
    use rdy_store::StoreError;
    let refused = |e: StoreError| assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
    let (_d, mut s) = store();
    let first = first_version(Hash([4; 32]));
    refused(
        s.put_tension_versions(&[accepted(&first, Hash([8; 32]))])
            .unwrap_err(),
    );
    refused(
        s.put_verdict(&verdict(Verdict::Tension, 1), None)
            .unwrap_err(),
    );
    let (h, t1) = put_tension(&mut s, 2);
    refused(
        s.put_verdict(&verdict(Verdict::NoTension, 3), None)
            .unwrap_err(),
    );
    let first = first_version(h);
    refused(
        s.put_tension_versions(&[accepted(&first, Hash([8; 32]))])
            .unwrap_err(),
    );
    let good = accepted(&first, t1);
    refused(
        s.put_tension_versions(&[good.clone(), good.clone()])
            .unwrap_err(),
    );
    let mut bad = good.clone();
    bad.advice = None;
    refused(s.put_tension_versions(&[bad]).unwrap_err());
    let t2 = s.put_tension_versions(&[good]).unwrap();
    refused(
        s.put_tension_versions(&[accepted(&first, t2[0])])
            .unwrap_err(),
    );
}

/// A latest version whose bytes do not decode stays `Decode`: it is damage, not a refusal.
#[test]
fn se1_a_latest_version_that_does_not_decode_is_decode() {
    let (_d, mut s) = store();
    let (h, _) = put_tension(&mut s, 1);
    let bad = s.write(vec![(kinds::TENSION, vec![0xff; 3])]).unwrap()[0];
    let key = pair_key(&items());
    s.set_refs_unchecked_for_tests(vec![(format!("{SEMANTIC_REF}{key}"), bad.0.to_vec())])
        .unwrap();
    let e = s
        .put_tension_versions(&[accepted(&first_version(h), bad)])
        .unwrap_err();
    assert!(matches!(e, rdy_store::StoreError::Decode(_)), "{e:?}");
}

const OLD_BYTES: &str = "0203030303030303030303030303030303030303030303030303030303030303030107632d4141414141040404040404040404040404040404040404040404040404040404040404040401090909090909090909090909090909090909090909090909090909090909090902037768790207632d4141414141026e3107632d4242424242026e32076167656e743a72010178020e";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn rf2_review() -> Review {
    let a = BlockId::parse("c-AAAAA").unwrap();
    Review {
        subject: ReviewSubject::Pair {
            key: Hash([3; 32]),
            items: vec![PairItem {
                block: a.clone(),
                content: Hash([4; 32]),
            }],
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: "why".into(),
        citations: vec![
            Citation {
                block: a,
                note: "n1".into(),
            },
            Citation {
                block: BlockId::parse("c-BBBBB").unwrap(),
                note: "n2".into(),
            },
        ],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("x".into()),
            role: Role::Reviewer,
            at_ms: 7,
        },
    }
}

/// rf2: a Citation review encodes byte for byte as the pre-change fixture and decodes the old
/// bytes; no serde alias is present.
#[test]
fn rf2_review_rename_keeps_positional_bytes() {
    let r = rf2_review();
    assert_eq!(hex(&postcard::to_allocvec(&r).unwrap()), OLD_BYTES);
    let old: Vec<u8> = (0..OLD_BYTES.len() / 2)
        .map(|i| u8::from_str_radix(&OLD_BYTES[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    assert_eq!(postcard::from_bytes::<Review>(&old).unwrap(), r);
    // no alias: the JSON form has the new key only
    let j = serde_json::to_value(&r).unwrap();
    assert!(
        j.get("citations").is_some() && j.get("findings").is_none(),
        "{j}"
    );
    let mut old_json = j.clone();
    let c = old_json
        .as_object_mut()
        .unwrap()
        .remove("citations")
        .unwrap();
    old_json["findings"] = c;
    assert!(serde_json::from_value::<Review>(old_json).is_err());
}
