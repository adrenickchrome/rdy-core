//! rpl16 (repo links): a verdict on a pair with a cited side is stored, upgraded and checked.
use rdy_model::foreign::ForeignId;
use rdy_model::review::{ReviewSubjectV1, ReviewV1};
use rdy_model::semantic::{ForeignPairItem, PairItem, PairSide, SemanticTensionV1};
use rdy_model::*;
use rdy_store::Store;

fn sides() -> Vec<PairSide> {
    let mut v = vec![
        PairSide::Local(PairItem {
            block: BlockId::parse("c-AAAAA").unwrap(),
            content: Hash([1; 32]),
        }),
        PairSide::Foreign(ForeignPairItem {
            target: ForeignId::new(RepoId([7; 16]), BlockId::parse("c-BBBBB").unwrap()),
            content: Hash([2; 32]),
        }),
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

fn review(v: Verdict, at_ms: i64) -> ReviewV1 {
    let items = sides();
    ReviewV1 {
        subject: ReviewSubjectV1::Pair {
            key: pair_key_sides(&items),
            items,
        },
        case_hash: Some(Hash([9; 32])),
        verdict: v,
        reason: String::new(),
        citations: vec![],
        provenance: prov(at_ms),
    }
}

fn first(review: Hash) -> SemanticTensionV1 {
    let items = sides();
    SemanticTensionV1 {
        key: pair_key_sides(&items),
        items,
        review,
        advice: Some("it contradicts the cited rule".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: prov(3),
    }
}

fn review_id(r: &ReviewV1) -> Hash {
    Hash::of(&rdy_store::codec::item_new(rdy_store::kinds::REVIEW, r).1)
}

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

#[test]
fn rpl16_fsck_checks_a_foreign_no_tension_verdict() {
    let (_d, mut s) = store();
    s.put_verdict_v1(&review(Verdict::NoTension, 1), None)
        .unwrap();
    s.check_objects().unwrap();
}

#[test]
fn rpl16_fsck_checks_a_foreign_tension_and_its_accepted_advice() {
    let (_d, mut s) = store();
    let r = review(Verdict::Tension, 2);
    let rid = review_id(&r);
    let t = first(rid);
    let (_, tid) = s.put_verdict_v1(&r, Some(&t)).unwrap();
    s.check_objects().unwrap();
    let accepted = SemanticTensionV1 {
        blocking: true,
        relay: Some(RelayEvidence {
            quoted: "accept the advice".into(),
            session_ref: "chat".into(),
            question: Some("Accept?".into()),
        }),
        prev: tid,
        ..t
    };
    s.put_tension_versions_v1(&[accepted]).unwrap();
    s.check_objects().unwrap();
}

#[test]
fn rpl16_a_foreign_tension_replaces_a_foreign_no_tension() {
    let (_d, mut s) = store();
    s.put_verdict_v1(&review(Verdict::NoTension, 1), None)
        .unwrap();
    let r = review(Verdict::Tension, 2);
    let t = first(review_id(&r));
    s.put_verdict_v1(&r, Some(&t)).unwrap();
    s.check_objects().unwrap();
}
