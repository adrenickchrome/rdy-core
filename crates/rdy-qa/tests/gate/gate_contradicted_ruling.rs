//! A pending proposal whose new decision contradicts an accepted decision that it is tied to
//! only by text (the M4d replay P1 shape). The approve card names that decision under `Related
//! decisions` (D-Q6A3F), and recommends no option while the proposer recommends reject or a
//! reviewer's tension is open (D-3N83P). A reviewer submits the short request id the case shows. The tests build real proposals and read the queue as a person does.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::check::api::{ReviewRequest, run_review};
use rdy_engine::engine::Local;
use rdy_engine::pending::queue::{
    RECOMMENDATION_FACT, RECOMMENDATION_NONE, RECOMMENDATION_PROPOSER, RECOMMENDATION_SEPARATOR,
    RECOMMENDATION_TENSION,
};
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery};
use rdy_engine::semantic::api::{ReviewNextRequest, run_review_next, run_review_submit};
use rdy_engine::semantic::candidates::request_label;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
use rdy_engine::{DecisionInput, ProposalInfo, Rdy, ReadState, Session};
use rdy_model::{BlockId, Role};
use rdy_qa::fixtures::{approver, land, writer};
use rdy_qa::intake::{PERSON, decide_in};
use rdy_qa::tensions::{j_typo, put_j};
use serde_json::json;
use std::collections::BTreeSet;

/// The accepted ruling, as D-RCE05 was: ingress is decided per service.
const RULING: &str = "The ingress service is decided per service as it is designed";
/// The contrary decision, as D-V1MG2 was: it settles ingress now.
const CONTRARY: &str = "The ingress service is settled now on the Pingora framework";

/// Canon: doc `platform` with section `Ingress`, entity `Ingress service`, and the accepted
/// ruling, which names the entity in its statement and links nothing.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    s: BlockId,
    ruling: BlockId,
}

fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "platform", "title": "Platform", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ingress"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ingress service"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let s = BlockId::parse(&w.created["$s"]).unwrap();
    let ruling = decide_in(&mut r, "rulings", decision(&s, RULING, false));
    land(&mut r, "rulings").unwrap();
    World {
        _dir: dir,
        r,
        s,
        ruling,
    }
}

/// A decision under `s`; `reject` gives it the proposer's reject recommendation.
fn decision(s: &BlockId, statement: &str, reject: bool) -> DecisionInput {
    DecisionInput {
        parent: s.to_string(),
        statement: statement.into(),
        title: Some(statement.into()),
        why: Some(format!("{statement}: the person decides it")),
        reversibility: Some("two_way".into()),
        recommend: reject.then(|| "reject".into()),
        recommend_reason: reject.then(|| "Test candidate: it contradicts the ruling".into()),
        ..Default::default()
    }
}

/// The contrary decision in view `view`, proposed with `summary`.
fn propose_contrary(
    w: &mut World,
    view: &str,
    reject: bool,
    summary: &str,
) -> (BlockId, ProposalInfo) {
    let d = decide_in(&mut w.r, view, decision(&w.s, CONTRARY, reject));
    rdy_qa::intake::answer_open_items(&mut w.r, &writer(), view);
    let p =
        w.r.propose(&writer(), view, view, Some(summary))
            .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    (d, p)
}

fn approve_item(st: &ReadState, pref: &str) -> Item {
    let q = st
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    q.items
        .into_iter()
        .find(|i| i.r#ref == pref && i.kind == ItemKind::Approve)
        .unwrap_or_else(|| panic!("an approve item for {pref}; not ready: {:#?}", q.not_ready))
}

fn fact<'a>(i: &'a Item, name: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fact {name} in {:#?}", i.facts))
        .value
        .as_str()
}

/// The recommended option of `i` and its reason.
fn recommended(i: &Item) -> Option<(&str, &str)> {
    i.options
        .iter()
        .find_map(|o| Some((o.id.as_str(), o.recommended.as_ref()?.reason())))
}

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

fn dismisser() -> Session {
    Session::agent("agent:d".into(), Some("d".into()), Role::Reviewer).unwrap()
}

#[test]
fn ki_related_decisions_name_the_ruling_found_by_text() {
    let mut w = world();
    let (d, p) = propose_contrary(&mut w, "p1", false, "settles ingress now");
    let item = approve_item(&w.r.read(), &p.r#ref);
    let value = fact(&item, "Related decisions");
    // the ruling is tied to the new decision by text only: it is counted and named, with its
    // id and its statement
    assert!(value.contains(" found by text, "), "{value}");
    assert!(
        value.contains(&format!(
            "{d} same_entity {} [live] via text: {RULING}",
            w.ruling
        )),
        "{value}"
    );
    assert!(item.card.contains(w.ruling.as_str()), "{}", item.card);
}

/// The `Recommendation` fact and the order around it: right after Policy, and every option
/// unrecommended.
fn assert_held(item: &Item, causes: &[String]) {
    let names: Vec<&str> = item.facts.iter().map(|f| f.name.as_ref()).collect();
    let at = names
        .iter()
        .position(|n| *n == RECOMMENDATION_FACT)
        .unwrap_or_else(|| panic!("a Recommendation fact in {names:?}"));
    assert_eq!(names[at - 1], "Policy", "{names:?}");
    assert_eq!(
        fact(item, RECOMMENDATION_FACT),
        format!(
            "{RECOMMENDATION_NONE}{}",
            causes.join(RECOMMENDATION_SEPARATOR)
        )
    );
    assert_eq!(recommended(item), None, "{:#?}", item.options);
    assert_eq!(item.options[0].id, "approve", "Approve keeps its place");
    assert!(!item.card.contains("Why recommended:"), "{}", item.card);
}

fn not_held(item: &Item) {
    assert!(
        item.facts.iter().all(|f| f.name != RECOMMENDATION_FACT),
        "{:#?}",
        item.facts
    );
    assert_eq!(recommended(item).map(|r| r.0), Some("approve"));
}

#[test]
fn ki_approve_card_recommends_nothing_while_the_proposer_recommends_reject() {
    let mut w = world();
    // control: no reject recommendation, and a summary that says it contradicts the ruling
    // (the summary is never read)
    let summary = format!("It contradicts the accepted ruling {}.", w.ruling);
    let (_, plain) = propose_contrary(&mut w, "plain", false, &summary);
    not_held(&approve_item(&w.r.read(), &plain.r#ref));

    // the M4d case: the confirm card beside the batch recommends Reject
    let (d, p) = propose_contrary(&mut w, "p1", true, &summary);
    let item = approve_item(&w.r.read(), &p.r#ref);
    assert_held(
        &item,
        &[RECOMMENDATION_PROPOSER.replace("{id}", d.as_str())],
    );
    // a list of recommended answers refuses the card as a whole (D-91DEW)
    let e =
        w.r.answer_all_recommended(
            &approver(),
            PERSON,
            std::slice::from_ref(&p.r#ref),
            &[],
            RelayInput::default(),
        )
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:#?}");
    // readiness is unchanged
    assert!(p.ready, "{:#?}", p.issues);

    // the cause clears once its confirm card is answered: the computed reason comes back
    let confirm = format!("{d}@");
    let card =
        w.r.read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap()
            .items
            .into_iter()
            .find(|i| i.kind == ItemKind::Confirm && i.r#ref.starts_with(&confirm))
            .expect("the confirm card");
    w.r.answer(
        &approver(),
        PERSON,
        AnswerRequest {
            r#ref: card.r#ref,
            option: "confirm".into(),
            card_hash: None,
            note: None,
            relay: RelayInput::default(),
            brief: None,
        },
    )
    .unwrap();
    not_held(&approve_item(&w.r.read(), &p.r#ref));
}

#[test]
fn ki_approve_card_recommends_nothing_while_a_reviewer_tension_is_open() {
    let mut w = world();
    let (d, p) = propose_contrary(&mut w, "p1", false, "settles ingress now");
    not_held(&approve_item(&w.r.read(), &p.r#ref));
    // a reviewer's tension verdict on the pair (contrary decision, ruling)
    let st = w.r.read();
    let q = st
        .review_requests()
        .unwrap()
        .into_iter()
        .find(|q| {
            let b: BTreeSet<_> = q.items.iter().map(|i| &i.block).collect();
            b == BTreeSet::from([&d, &w.ruling])
        })
        .expect("a review request for the pair");
    let hash = case_hash(&st.case_of(&q.key, &q.items).unwrap()).to_string();
    let sides: Vec<String> = q.items.iter().map(|i| i.block.to_string()).collect();
    drop(st);
    let sub =
        w.r.submit_verdict(
            &reviewer(),
            ReviewSubmitRequest {
                request: request_label(&q.key),
                case_hash: hash,
                verdict: "tension".into(),
                citations: vec![CitationRequest {
                    block: d.to_string(),
                    note: "it settles now what the ruling leaves to each service".into(),
                }],
                severity: None,
                reason: None,
            },
        )
        .unwrap();
    let tn = sub.tension.expect("the verdict opened a tension");
    // the version's change plan takes the tension as an item; once it is answered, the next
    // version is ready and its card shows the cause
    rdy_qa::intake::answer_open_items(&mut w.r, &writer(), "p1");
    let p =
        w.r.propose(&writer(), "p1", "p1", Some("settles ingress now"))
            .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    let item = approve_item(&w.r.read(), &p.r#ref);
    let cause = RECOMMENDATION_TENSION
        .replace("{tn}", &tn)
        .replace("{a}", &sides[0])
        .replace("{b}", &sides[1]);
    assert_held(&item, &[cause]);
    // a dismissed tension is no cause
    w.r.dismiss_tension(&dismisser(), &tn, "they agree after all")
        .unwrap();
    not_held(&approve_item(&w.r.read(), &p.r#ref));
}

#[test]
fn ki_a_contradiction_case_shows_the_request_id_a_submit_takes() {
    let mut w = world();
    let (d, _p) = propose_contrary(&mut w, "p1", false, "settles ingress now");
    let e = Local::new(w.r);
    let asked = run_review(
        &e,
        &writer(),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(json!({
            "op": "request",
            "check": "contradiction",
            "subject": [d.to_string(), w.ruling.to_string()],
        }))
        .unwrap(),
    )
    .unwrap();
    let n = run_review_next(&e, &reviewer(), 7, ReviewNextRequest { kind: "any".into() }).unwrap();
    assert_eq!(n["request"], asked["request"], "{n:#}");
    // the case shows its pair's short id beside the request's full id; the short id submits
    // the same request (p-80ED3: only a changed case or a settled request is stale)
    let short = n["case"]["request"].as_str().unwrap().to_string();
    assert_ne!(json!(short), n["request"], "{n:#}");
    let submit = |request: String| ReviewSubmitRequest {
        request,
        case_hash: n["case_hash"].as_str().unwrap().to_string(),
        verdict: "tension".into(),
        citations: vec![CitationRequest {
            block: d.to_string(),
            note: "it settles now what the ruling leaves to each service".into(),
        }],
        severity: None,
        reason: None,
    };
    let sub = run_review_submit(&e, &reviewer(), submit(short.clone()))
        .unwrap_or_else(|err| panic!("the id the case shows is accepted: {err:#?}"));
    assert_eq!(sub["verdict"], "tension", "{sub:#}");
    // the request is settled now: the same id again is stale
    let again = run_review_submit(&e, &reviewer(), submit(short)).unwrap_err();
    assert_eq!(again.code, "E_REVIEW_STALE", "{again:#?}");
}
