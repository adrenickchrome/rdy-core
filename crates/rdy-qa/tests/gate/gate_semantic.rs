//! se11: the semantic gate end to end. Pending requests never block; advice gates nothing; an
//! accepted tension refuses (D-KJ71P, D-2K8Q5, D-WMCFW); a landed one stays in canon (F6).
use rdy_engine::semantic::candidates::request_label;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::{FindingRequest, ReviewSubmitRequest};
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::*;
use rdy_engine::*;
use rdy_model::{BlockId, Role};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use std::collections::BTreeSet;

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(&writer(), view, view, None).unwrap()
}
fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<String, RdyError> {
    r.land(&approver(), &p.id)
}
fn reviewer(actor: &str) -> Session {
    Session::agent(actor.into(), Some("rr".into()), Role::Reviewer).unwrap()
}

/// Submits agent:r's `tension` verdict on the request whose blocks are `pair` (the first one
/// agent:r may take when `pair` is `None`), with advice to block and a reason when `advise`.
fn judge_pair(r: &mut Rdy, pair: Option<(&BlockId, &BlockId)>, advise: bool) {
    let st = r.read();
    let q = match pair {
        None => st
            .next_request("agent:r", &BTreeSet::new())
            .unwrap()
            .expect("a request"),
        Some((x, y)) => st
            .review_requests()
            .unwrap()
            .into_iter()
            .find(|q| {
                let b: BTreeSet<_> = q.items.iter().map(|i| &i.block).collect();
                b == BTreeSet::from([x, y])
            })
            .expect("the request for the pair"),
    };
    let hash = case_hash(&st.case_of(&q.key, &q.items).unwrap()).to_string();
    let label = request_label(&q.key);
    let on = q.items[0].block.to_string();
    drop(st);
    r.submit_verdict(
        &reviewer("agent:r"),
        ReviewSubmitRequest {
            request: label,
            case_hash: hash,
            verdict: "tension".into(),
            findings: vec![FindingRequest {
                block: on,
                note: "these two cannot both hold".into(),
            }],
            severity: advise.then(|| "blocking".into()),
            reason: advise.then(|| "it contradicts the gate rule".into()),
        },
    )
    .unwrap();
}
fn judge(r: &mut Rdy, advise: bool) {
    judge_pair(r, None, advise);
}
/// A person's acceptance of the advice shown on `p`.
fn accept(r: &mut Rdy, p: &ProposalInfo) {
    r.accept_advice(
        &writer(),
        "human:qa",
        AcceptRequest {
            proposal: p.r#ref.clone(),
            advised_digest: p.advised_digest.clone().expect("advised digest"),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn se11_pending_requests_never_block_a_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C changed."));
    assert!(!r.read().review_requests().unwrap().is_empty());
    let p = propose(&mut r, "a");
    approve(&mut r, &p).unwrap();
}

#[test]
fn se11_an_advised_semantic_tension_never_refuses_and_an_accepted_one_does() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    judge(&mut r, true);
    let p = propose(&mut r, "a");
    let t = p
        .tensions
        .iter()
        .find(|t| t.kind == TensionKind::Semantic)
        .unwrap_or_else(|| panic!("{:?}", p.tensions));
    assert_eq!(t.refuses, Some(false), "advice gates nothing");
    assert!(t.judged.as_ref().unwrap().advice.is_some());
    accept(&mut r, &p);
    let p = propose(&mut r, "a");
    let t = p
        .tensions
        .iter()
        .find(|t| t.kind == TensionKind::Semantic)
        .unwrap();
    assert_eq!(t.refuses, Some(true), "an accepted tension refuses");
    let id = t.id.clone();
    let e = approve(&mut r, &p).unwrap_err();
    assert_eq!(e.code, E_BLOCKING_TENSION);
    assert!(
        e.issues
            .iter()
            .any(|i| i.code == E_BLOCKING_TENSION && i.message.contains(&id)),
        "{e:?}"
    );
    r.dismiss_tension(&reviewer("agent:d"), &id, "B and C agree after all")
        .unwrap();
    let p = propose(&mut r, "a");
    approve(&mut r, &p).unwrap();
}

/// final ruling 2: with B edited in view a and C in view b, each view's change is paired with
/// the other block as its own landing would leave it, canon's: (B@a, C@canon) and
/// (B@canon, C@b), never (B@a, C@b). An accepted tension refuses only the landing of the view
/// that holds its change side; once the other view lands C, the judged text of C is gone from
/// canon and the tension with it.
#[test]
fn se11_each_view_is_judged_on_its_own_landing_form() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let wa = put(&mut r, "a", upd(&s.b, "B changed."));
    let wb = put(&mut r, "b", upd(&s.c, "C contradicts B."));
    let pairs: Vec<Vec<bool>> = r
        .read()
        .review_requests()
        .unwrap()
        .into_iter()
        .filter(|q| {
            let b: BTreeSet<_> = q.items.iter().map(|i| &i.block).collect();
            b == BTreeSet::from([&s.b, &s.c])
        })
        .map(|q| {
            q.items
                .iter()
                .map(|i| matches!(i.side, SideRef::Change(_)))
                .collect()
        })
        .collect();
    assert_eq!(pairs.len(), 2, "one request per view's landing form");
    assert!(
        pairs.iter().all(|p| p.iter().filter(|c| **c).count() == 1),
        "each pairs one change with canon: {pairs:?}"
    );
    judge_pair(&mut r, Some((&s.b, &s.c)), true);
    let t = one(&r, TensionKind::Semantic);
    let mine = change_sides(&t);
    assert!(mine == vec![change_of(&wa)] || mine == vec![change_of(&wb)]);
    let (held, other) = if mine == vec![change_of(&wa)] {
        ("a", "b")
    } else {
        ("b", "a")
    };
    let p = propose(&mut r, held);
    accept(&mut r, &p);
    let p = propose(&mut r, held);
    assert_eq!(
        approve(&mut r, &p).unwrap_err().code,
        E_BLOCKING_TENSION,
        "the accepted form refuses its own view"
    );
    let q = propose(&mut r, other);
    assert!(
        !q.tensions.iter().any(|t| t.kind == TensionKind::Semantic),
        "the other view's landing form was never judged: {:?}",
        q.tensions
    );
    approve(&mut r, &q).unwrap();
    assert!(
        of_kind(&r, TensionKind::Semantic).is_empty(),
        "the judged canon text is gone"
    );
    let p = propose(&mut r, held);
    approve(&mut r, &p).unwrap();
}

/// F6 (RULING, D-VA8DA): a landed semantic tension stays in canon, and an all-canon one never
/// refuses a landing.
#[test]
fn se11_a_landed_semantic_tension_stays_in_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    judge(&mut r, false);
    let p = propose(&mut r, "a");
    approve(&mut r, &p).unwrap();
    let set = r.read().tensions().unwrap();
    let t = set
        .iter()
        .find(|t| t.kind == TensionKind::Semantic)
        .expect("the semantic tension");
    assert!(set.from_canon(&t.id), "every side is canon");
    put(
        &mut r,
        "c",
        serde_json::json!([{"op": "create", "type": "claim", "id": "$n",
            "parent": s.section.to_string(), "body": "Unrelated."}]),
    );
    let p = propose(&mut r, "c");
    approve(&mut r, &p).unwrap();
    assert_eq!(one(&r, TensionKind::Semantic).id, t.id, "still there");
}
