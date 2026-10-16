//! di8 (plan Decision intake, Task 8): answers to questions and to tensions, and the relayed
//! dismissal. An answer to a question records the person's ruling as a proposed decision that
//! `answers` it (tb-Q970N). The person answers the advice once for the batch: accept it, and every
//! advised tension blocks; or dismiss it with a reason, and every advised tension is dismissed
//! (D-WMCFW). A relayed dismissal runs as the person, so the reviewer-or-human rule and the
//! self-review rule hold for them (D-562QR); the `ANSWER` beside it holds the relay, in the same
//! batch (O14). An agent relays an acceptance as itself; a human session's acceptance is plain
//! (D-67ZN7, C5).
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::answer::{AnswerInfo, AnswerRequest};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::pending::{Item, ItemKind, Pending};
use rdy_engine::tensions::api::{AcceptRequest, DismissRequest, ResolveRequest, run_resolve};
use rdy_engine::tensions::{TensionKind, TensionSeverity, TensionStatus, tension_label};
use rdy_engine::{
    DecisionInput, E_BAD_INPUT, E_PERMISSION, E_SELF_REVIEW, Rdy, RdyError, Scope, Session,
};
use rdy_model::{ActorRun, Answer, AnswerAct, BlockId, EvidenceKind, FieldValue, Hash, Role};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{land, version_of, write, writer};
use rdy_qa::intake::*;
use rdy_qa::tensions::{ag, created, of_kind, open, put, upd};
use rdy_store::kinds;
use serde_json::json;
use std::path::Path;

/// The relaying agent.
const ME: &str = "agent:relay";

/// A person who wrote no side of the fixtures' tensions.
const FOUNDER: &str = "human:founder";

fn agent() -> Session {
    Session::agent(ME.into(), Some("r1".into()), Role::Writer).expect("agent")
}

fn relayer() -> ActorRun {
    ActorRun {
        actor: ME.into(),
        run: Some("r1".into()),
    }
}

/// The person's words, the question they answered and the chat session, relayed over MCP (no
/// `relayed_by`).
fn words(quote: &str) -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some(quote.into()),
        question: Some("What do you answer?".into()),
        session_ref: Some("chat-8".into()),
        kind: None,
    }
}

fn req(r#ref: &str, option: &str, card_hash: Option<&str>, note: Option<&str>) -> AnswerRequest {
    AnswerRequest {
        r#ref: r#ref.into(),
        option: option.into(),
        card_hash: card_hash.map(str::to_string),
        note: note.map(str::to_string),
        relay: RelayInput::default(),
    }
}

fn queue(r: &Rdy) -> Pending {
    pending(r)
}

fn find<'a>(q: &'a Pending, r#ref: &str) -> Option<&'a Item> {
    q.items.iter().find(|i| i.r#ref == r#ref)
}

fn item(q: &Pending, r#ref: &str) -> Item {
    find(q, r#ref)
        .unwrap_or_else(|| {
            let refs: Vec<&str> = q.items.iter().map(|i| i.r#ref.as_str()).collect();
            panic!("{ref} in {refs:?}")
        })
        .clone()
}

/// Relays the person's `quote` for `option` on item `r#ref`, bound to the card the queue shows
/// now.
fn relay(
    r: &mut Rdy,
    person: &str,
    r#ref: &str,
    option: &str,
    note: Option<&str>,
    quote: &str,
) -> Result<AnswerInfo, RdyError> {
    let h = item(&queue(r), r#ref).card_hash;
    r.answer(
        &agent(),
        person,
        AnswerRequest {
            relay: words(quote),
            ..req(r#ref, option, Some(&h), note)
        },
    )
}

fn refused(e: &RdyError, code: &str, message: &str) {
    assert_eq!((e.code, e.message.as_str()), (code, message), "{e}");
}

fn answer_of(r: &Rdy, item: &str) -> Option<(Hash, Answer)> {
    r.store().reader().answer_of(item).expect("answer_of")
}

/// The kinds of the last batch's frames.
fn last_kinds(r: &Rdy) -> Vec<u8> {
    r.store().last_batch().iter().map(|(k, _)| *k).collect()
}

/// The hashes of the last batch's frames of kind `kind`, in write order.
fn last_of(r: &Rdy, kind: u8) -> Vec<Hash> {
    r.store()
        .last_batch()
        .iter()
        .filter(|(k, _)| *k == kind)
        .map(|(_, h)| *h)
        .collect()
}

fn str_field<'a>(b: &'a rdy_model::Block, name: &str) -> Option<&'a str> {
    match b.fields.get(name) {
        Some(FieldValue::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

// ---- questions ----

/// The spec doc in canon, and an open question with authored options landed beside it: the
/// question's id and its item ref.
fn with_question() -> (tempfile::TempDir, Rdy, Spec, BlockId, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let (r, q, item) = add_question(dir.path(), r, &spec);
    (dir, r, spec, q, item)
}

fn add_question(dir: &Path, mut r: Rdy, spec: &Spec) -> (Rdy, BlockId, String) {
    let w = write(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": spec.section.to_string(),
        "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead", "Either"],
            "recommended": "Finance",
            "recommend_reason": "Finance owns the ledger"
        }}]),
    )
    .unwrap();
    land(&mut r, "qs").unwrap();
    r.close().unwrap();
    let r = Rdy::open(dir).unwrap();
    let q = created(&w, "$q");
    let v = version_of(&r, &Scope::Canon, &q.to_string());
    let item = format!("{q}@{}", &v[..12]);
    (r, q, item)
}

/// The decision the change `change` created, as view `view` holds it.
fn decision_of(r: &Rdy, view: &str, change: &Hash) -> rdy_model::Block {
    let c = r.store().reader().change(change).expect("the change");
    let id = c
        .ops
        .iter()
        .map(|op| op.block_id().clone())
        .next()
        .expect("one op");
    r.read()
        .block(&Scope::View(view.into()), &id)
        .expect("the decision in the view")
}

fn answers(b: &rdy_model::Block) -> Vec<String> {
    b.links
        .get(rdy_model::decision::ANSWERS)
        .into_iter()
        .flatten()
        .map(|l| l.target.to_string())
        .collect()
}

#[test]
fn di8_answering_a_question_writes_a_decision_that_answers_it() {
    let (_d, mut r, _spec, q, it) = with_question();
    let card = item(&queue(&r), &it);
    assert_eq!(card.kind, ItemKind::Answer);
    // relayed from chat: the agent's own view holds the person's ruling
    let info = relay(&mut r, PERSON, &it, "o1", None, "Finance, please").unwrap();
    assert_eq!(
        (
            info.act.as_str(),
            info.option.as_str(),
            info.label.as_str(),
            info.assurance
        ),
        ("answer_question", "o1", "Finance", "relayed")
    );
    assert_eq!(info.relayed_by, Some(relayer()));
    // the answer is a batch of its own after the decision's commit (O14: two batches)
    let kinds = last_kinds(&r);
    assert!(kinds.contains(&kinds::ANSWER), "{kinds:?}");
    assert!(!kinds.contains(&kinds::CHANGE), "{kinds:?}");
    let (h, a) = answer_of(&r, &it).expect("the answer");
    assert_eq!(info.answer.as_deref(), Some(h.to_string().as_str()));
    assert_eq!(a.act, AnswerAct::AnswerQuestion);
    assert_eq!(
        (a.option.as_deref(), a.label.as_deref(), a.note.as_deref()),
        (Some("o1"), Some("Finance"), None)
    );
    assert_eq!(
        a.options,
        vec!["Finance", "Support lead", "Either", "Later"],
        "the labels shown, from rdy's own card"
    );
    assert_eq!(a.card_hash.map(|h| h.to_string()), Some(card.card_hash));
    assert_eq!(a.person.actor, PERSON);
    assert_eq!(a.relayed_by, Some(relayer()));
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Card));
    assert_eq!(
        a.relay_evidence.as_ref().map(|e| e.quoted.as_str()),
        Some("Finance, please")
    );
    assert_eq!(a.targets.len(), 1, "the decision's change");
    let view = agent().default_view();
    let d = decision_of(&r, &view, &a.targets[0]);
    assert_eq!(d.block_type, "decision");
    assert_eq!(str_field(&d, "statement"), Some("Finance"));
    assert_eq!(str_field(&d, "status"), Some("proposed"));
    assert_eq!(str_field(&d, "authority_basis"), Some("direct_ruling"));
    assert_eq!(
        str_field(&d, "attested_by"),
        Some("human:qa (relayed by agent:relay/r1: \"Finance, please\")")
    );
    assert_eq!(answers(&d), vec![q.to_string()]);
    let change = r.store().reader().change(&a.targets[0]).unwrap();
    assert_eq!(change.provenance.actor, ME, "the agent writes its own view");

    // at the CLI, the person answers in their own view `work`, plainly
    let info = r
        .answer(&writer(), PERSON, req(&it, "o2", None, None))
        .unwrap();
    assert_eq!(
        (info.label.as_str(), info.assurance),
        ("Support lead", "local")
    );
    let (_, a) = answer_of(&r, &it).unwrap();
    assert_eq!((a.relayed_by, a.evidence_kind), (None, None));
    let d = decision_of(&r, "work", &a.targets[0]);
    assert_eq!(str_field(&d, "statement"), Some("Support lead"));
    assert_eq!(str_field(&d, "attested_by"), Some("human:qa"));
    assert_eq!(answers(&d), vec![q.to_string()]);
    r.fsck().expect("fsck");
}

#[test]
fn di8_a_free_answer_needs_a_note() {
    let (_d, mut r, _spec, q, it) = with_question();
    let e = relay(&mut r, PERSON, &it, "o0", None, "something else").unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        "a free answer (o0) needs a note: the person's own answer",
    );
    assert!(answer_of(&r, &it).is_none(), "nothing written");
    let e = relay(&mut r, PERSON, &it, "o9", None, "nine").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let info = relay(
        &mut r,
        PERSON,
        &it,
        "o0",
        Some("Finance, up to 500 euros"),
        "Finance, up to 500 euros",
    )
    .unwrap();
    assert_eq!(
        (info.option.as_str(), info.label.as_str()),
        ("o0", "Finance, up to 500 euros")
    );
    let (_, a) = answer_of(&r, &it).unwrap();
    assert_eq!(
        (a.option.as_deref(), a.label, a.note.as_deref()),
        (Some("o0"), None, Some("Finance, up to 500 euros")),
        "a free answer has no label on the card; the note holds the words"
    );
    let d = decision_of(&r, &agent().default_view(), &a.targets[0]);
    assert_eq!(str_field(&d, "statement"), Some("Finance, up to 500 euros"));
    assert_eq!(answers(&d), vec![q.to_string()]);
}

// ---- advice ----

/// `batch_project` with a second advised tension (the batch's claim Q against the spec's claim B)
/// and the batch proposed again: the batch and its advice item's ref.
fn two_advised() -> (tempfile::TempDir, Rdy, Batch, String) {
    let (dir, mut r, b) = batch_project();
    verdict_on(
        &mut r,
        &Scope::Inflight,
        &b.advised,
        &b.spec.b,
        "agent:r",
        true,
    );
    let (r, report) = propose_reported(dir.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let b = Batch {
        r#ref: report["ref"].as_str().unwrap().to_string(),
        report,
        ..b
    };
    let advice = format!("advice:{}", b.r#ref);
    assert_eq!(item(&queue(&r), &advice).members.len(), 2, "two advised");
    (dir, r, b, advice)
}

/// Every semantic tension, dismissed or not.
fn semantic(r: &Rdy) -> Vec<std::sync::Arc<rdy_engine::tensions::Tension>> {
    of_kind(r, TensionKind::Semantic)
}

/// The semantic tensions no one dismissed.
fn undismissed(r: &Rdy) -> usize {
    let st = r.read();
    semantic(r)
        .iter()
        .filter(|t| st.tension_status(t) == TensionStatus::Open)
        .count()
}

#[test]
fn di8_accepting_the_advice_once_blocks_every_advised_tension() {
    let (_d, mut r, _b, advice) = two_advised();
    assert!(
        semantic(&r)
            .iter()
            .all(|t| t.severity == TensionSeverity::Advisory)
    );
    let info = relay(&mut r, FOUNDER, &advice, "accept", None, "Yes, they block").unwrap();
    assert_eq!(
        (info.act.as_str(), info.label.as_str(), info.assurance),
        ("accept", "Accept the advice: they block", "relayed")
    );
    assert_eq!(info.answer, None, "the tension versions record the relay");
    assert!(answer_of(&r, &advice).is_none());
    let all = semantic(&r);
    assert_eq!(all.len(), 2);
    for t in &all {
        assert_eq!(t.severity, TensionSeverity::Blocking, "{}", t.summary);
        let acc = r.read().tension_info(t).judged.unwrap().accepted.unwrap();
        assert_eq!(
            (
                acc.actor.as_str(),
                acc.relayed_by.as_deref(),
                acc.evidence.as_deref(),
                acc.session_ref.as_deref()
            ),
            (
                FOUNDER,
                Some("agent:relay/r1"),
                Some("Yes, they block"),
                Some("chat-8")
            ),
            "the person accepted; the agent relayed as itself"
        );
    }
    assert!(
        find(&queue(&r), &advice).is_none(),
        "the advice is answered"
    );
    r.fsck().expect("fsck");
}

#[test]
fn di8_dismissing_the_advice_once_dismisses_every_advised_tension() {
    let (_d, mut r, _b, advice) = two_advised();
    let e = relay(&mut r, FOUNDER, &advice, "dismiss", None, "No").unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        "a dismissal needs a note: the person's reason",
    );
    assert_eq!(undismissed(&r), 2, "nothing dismissed");
    let info = relay(
        &mut r,
        FOUNDER,
        &advice,
        "dismiss",
        Some("they agree on the access rules"),
        "No, they agree on the access rules",
    )
    .unwrap();
    assert_eq!(
        (info.act.as_str(), info.label.as_str(), info.assurance),
        ("dismiss", "Dismiss with a reason", "relayed")
    );
    // one batch: both reviews and the answer
    let reviews = last_of(&r, kinds::REVIEW);
    assert_eq!(reviews.len(), 2, "{:?}", last_kinds(&r));
    let answers = last_of(&r, kinds::ANSWER);
    assert_eq!(answers.len(), 1, "{:?}", last_kinds(&r));
    let (h, a) = answer_of(&r, &advice).expect("the answer");
    assert_eq!(answers[0], h);
    assert_eq!(a.act, AnswerAct::Dismiss);
    assert_eq!(a.targets, reviews, "the answer names the reviews");
    assert_eq!(a.note.as_deref(), Some("they agree on the access rules"));
    assert_eq!(a.person.actor, FOUNDER);
    assert_eq!(a.relayed_by, Some(relayer()));
    for rv in &reviews {
        let review = r.store().reader().review(rv).unwrap();
        assert_eq!(
            review.provenance.actor, FOUNDER,
            "the review records the person"
        );
        assert_eq!(review.reason, "they agree on the access rules");
    }
    assert_eq!(semantic(&r).len(), 2);
    assert_eq!(undismissed(&r), 0, "every advised tension is dismissed");
    assert!(find(&queue(&r), &advice).is_none());
    r.fsck().expect("fsck");
}

// ---- the relayed dismissal ----

/// `batch_project` with a decision that depends on claim A and another view that edits A: two
/// open blocking tensions (a premise shift each way) that involve the batch, whose sides agents
/// wrote. Their labels, in id order.
fn blocking_on_the_batch() -> (tempfile::TempDir, Rdy, Vec<String>) {
    let (dir, mut r, b) = batch_project();
    decide_in(
        &mut r,
        BATCH_VIEW,
        DecisionInput {
            depends_on: vec![b.spec.a.to_string()],
            ..decision(&b.spec, "Guests rely on A", "Guests rely on A")
        },
    );
    put(&mut r, "other", upd(&b.spec.a, "A as other has it."));
    let (r, _report) =
        propose_reported(dir.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let mut labels: Vec<String> = open(&r)
        .iter()
        .filter(|t| t.severity == TensionSeverity::Blocking)
        .map(|t| tension_label(&t.id))
        .collect();
    labels.sort();
    assert_eq!(labels.len(), 2, "one premise shift each way");
    (dir, r, labels)
}

fn status(r: &Rdy, label: &str) -> TensionStatus {
    let st = r.read();
    let t = st.resolve_tension(label, rdy_engine::now_ms()).unwrap();
    st.tension_status(&t)
}

/// The last batch's kinds, read through the engine.
fn engine_last_kinds(e: &Local) -> Vec<u8> {
    let v = write_typed(e, |r| Ok(last_kinds(r))).unwrap();
    serde_json::from_value(v).unwrap()
}

#[test]
fn di8_a_relayed_dismissal_records_the_person_and_the_relay() {
    let (dir, mut r, labels) = blocking_on_the_batch();
    // through the answer: the card the person saw
    let card = item(&queue(&r), &labels[0]);
    assert!(card.blocking);
    let info = relay(
        &mut r,
        FOUNDER,
        &labels[0],
        "dismiss",
        Some("A is stable"),
        "Dismiss it, A is stable",
    )
    .unwrap();
    assert_eq!((info.act.as_str(), info.assurance), ("dismiss", "relayed"));
    let reviews = last_of(&r, kinds::REVIEW);
    assert_eq!(reviews.len(), 1, "{:?}", last_kinds(&r));
    assert_eq!(last_of(&r, kinds::ANSWER).len(), 1, "{:?}", last_kinds(&r));
    let review = r.store().reader().review(&reviews[0]).unwrap();
    assert_eq!(
        (review.provenance.actor.as_str(), review.reason.as_str()),
        (FOUNDER, "A is stable")
    );
    let (_, a) = answer_of(&r, &labels[0]).unwrap();
    assert_eq!(a.act, AnswerAct::Dismiss);
    assert_eq!(a.targets, reviews);
    assert_eq!(a.person.actor, FOUNDER);
    assert_eq!(a.relayed_by, Some(relayer()));
    assert_eq!(
        a.relay_evidence.as_ref().map(|e| e.quoted.as_str()),
        Some("Dismiss it, A is stable")
    );
    assert_eq!(
        (a.evidence_kind, a.card_hash.map(|h| h.to_string())),
        (Some(EvidenceKind::Card), Some(card.card_hash))
    );
    assert!(matches!(
        status(&r, &labels[0]),
        TensionStatus::Dismissed(_)
    ));

    // through rdy_resolve over MCP: the session is the relay; the words are the evidence
    let e = Local::new(r);
    let v = run_resolve(
        &e,
        &agent(),
        FOUNDER,
        ResolveRequest {
            tension: Some(labels[1].clone()),
            dismiss: Some(DismissRequest {
                reason: "both hold".into(),
                evidence: Some("Both hold, dismiss it".into()),
                question: Some("Dismiss the premise shift?".into()),
                session_ref: Some("chat-8".into()),
                relayed_by: None,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v["status"], "dismissed", "{v}");
    let kinds_now = engine_last_kinds(&e);
    assert!(
        kinds_now.contains(&kinds::REVIEW) && kinds_now.contains(&kinds::ANSWER),
        "{kinds_now:?}"
    );
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let review: Hash = v["review"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        r.store().reader().review(&review).unwrap().provenance.actor,
        FOUNDER
    );
    let (_, a) = answer_of(&r, &labels[1]).unwrap();
    assert_eq!(
        (a.act, a.targets.clone(), a.person.actor.as_str()),
        (AnswerAct::Dismiss, vec![review], FOUNDER)
    );
    assert_eq!(a.relayed_by, Some(relayer()));
    assert_eq!(
        (a.evidence_kind, a.card_hash, a.option, a.note.as_deref()),
        (Some(EvidenceKind::Text), None, None, Some("both hold")),
        "rdy_resolve shows no card"
    );
    r.fsck().expect("fsck");
}

#[test]
fn di8_an_agent_cannot_dismiss_advice_without_the_person() {
    let (dir, mut r, b) = batch_project();
    let advice = format!("advice:{}", b.r#ref);
    let tn = tension_label(&semantic(&r)[0].id);
    // the answer: no words, no relay
    let h = item(&queue(&r), &advice).card_hash;
    let e = r
        .answer(
            &agent(),
            FOUNDER,
            req(&advice, "dismiss", Some(&h), Some("fine")),
        )
        .unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "an agent answers only by relaying a person's words",
    );
    // the agent as itself: today's rule
    let e = r.dismiss_tension(&agent(), &tn, "fine").unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "dismissing a tension a reviewer advised to block needs the reviewer role or a human session",
    );
    let e = Local::new(r);
    let dismiss = |evidence: Option<&str>, question: Option<&str>| ResolveRequest {
        tension: Some(tn.clone()),
        dismiss: Some(DismissRequest {
            reason: "fine".into(),
            evidence: evidence.map(str::to_string),
            question: question.map(str::to_string),
            session_ref: None,
            relayed_by: None,
        }),
        ..Default::default()
    };
    // rdy_resolve with no evidence keeps today's rule
    let err = run_resolve(&e, &agent(), FOUNDER, dismiss(None, None)).unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err}");
    // half a relay
    let err = run_resolve(&e, &agent(), FOUNDER, dismiss(None, Some("Dismiss?"))).unwrap_err();
    refused(&err, E_BAD_INPUT, "question needs evidence");
    // blank words are no words
    let err = run_resolve(&e, &agent(), FOUNDER, dismiss(Some("  "), None)).unwrap_err();
    refused(
        &err,
        E_PERMISSION,
        "an agent answers only by relaying a person's words",
    );
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(undismissed(&r), 1, "nothing dismissed");
    assert!(answer_of(&r, &advice).is_none());
    assert!(answer_of(&r, &tn).is_none());
}

#[test]
fn di8_the_person_cannot_dismiss_their_own_side() {
    // human:qa wrote the spec's claim C, a side of the advised tension
    let (dir, mut r, b) = batch_project();
    let advice = format!("advice:{}", b.r#ref);
    let tn = tension_label(&semantic(&r)[0].id);
    let e = relay(&mut r, PERSON, &advice, "dismiss", Some("fine"), "fine").unwrap_err();
    assert_eq!(e.code, E_SELF_REVIEW, "{e}");
    assert!(
        e.message.starts_with("actor human:qa wrote side "),
        "{}",
        e.message
    );
    assert!(answer_of(&r, &advice).is_none(), "nothing written");
    let e = Local::new(r);
    let err = run_resolve(
        &e,
        &agent(),
        PERSON,
        ResolveRequest {
            tension: Some(tn.clone()),
            dismiss: Some(DismissRequest {
                reason: "fine".into(),
                evidence: Some("fine".into()),
                question: None,
                session_ref: None,
                relayed_by: None,
            }),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code, E_SELF_REVIEW, "{err}");
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(undismissed(&r), 1, "the tension stays open");
    assert!(answer_of(&r, &tn).is_none());
}

// ---- the acceptance (D-67ZN7, C5) ----

fn accept_req(b: &Batch) -> AcceptRequest {
    AcceptRequest {
        proposal: b.r#ref.clone(),
        advised_digest: b.report["advised_digest"].as_str().unwrap().into(),
        ..Default::default()
    }
}

#[test]
fn di8_a_human_session_accepts_plainly_and_cannot_relay() {
    let (_d, mut r, b) = batch_project();
    let relayed = AcceptRequest {
        evidence: Some("yes".into()),
        relayed_by: Some(ME.into()),
        ..accept_req(&b)
    };
    let e = r.accept_advice(&writer(), PERSON, relayed).unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "a person answers as themselves; only the relaying agent records a relay",
    );
    let words_only = AcceptRequest {
        evidence: Some("yes".into()),
        ..accept_req(&b)
    };
    let e = r.accept_advice(&writer(), PERSON, words_only).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    assert!(
        semantic(&r)
            .iter()
            .all(|t| t.severity == TensionSeverity::Advisory),
        "nothing written"
    );
    r.accept_advice(&writer(), PERSON, accept_req(&b)).unwrap();
    let t = &semantic(&r)[0];
    let acc = r.read().tension_info(t).judged.unwrap().accepted.unwrap();
    assert_eq!(
        (acc.actor.as_str(), acc.relayed_by, acc.evidence),
        ("human:qa", None, None),
        "a human session's acceptance is plain"
    );
}

fn args(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// A served project with an advised tension on view a's proposal: the proposal, its digest and
/// the tension's label.
fn served_advice() -> (Served, String, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let p = r.propose(&ag("a"), "a", "a").unwrap();
    r.close().unwrap();
    (s, p.r#ref, p.advised_digest.unwrap())
}

#[test]
fn di8_an_agent_relays_an_acceptance_as_itself_at_the_cli() {
    let (s, pref, digest) = served_advice();
    let accept = |extra: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = ["--json", "resolve", "--accept", &pref, "--digest", &digest]
            .iter()
            .map(|a| a.to_string())
            .collect();
        v.extend(extra.iter().map(|a| a.to_string()));
        v
    };
    // an agent with no relay flags
    let a = accept(&[]);
    let (ok, v) = s.json_as("agent:w", Some("r7"), &args(&a));
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("an agent accepts advice only by relaying a person's words")
        ),
        "{v}"
    );
    // an agent that names another relayer
    let a = accept(&["--relayed-by", "agent:other", "--evidence", "yes"]);
    let (ok, v) = s.json_as("agent:w", Some("r7"), &args(&a));
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("agent:w relays only as itself, not as agent:other")
        ),
        "{v}"
    );
    // a person does not relay (C5)
    let a = accept(&["--relayed-by", "agent:w", "--evidence", "yes"]);
    let (ok, v) = s.json(&args(&a));
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("a person answers as themselves; only the relaying agent records a relay")
        ),
        "{v}"
    );
    let (ok, v) = s.json(&["--json", "tensions", "--kind", "semantic"]);
    assert!(ok, "{v}");
    assert_eq!(
        v["tensions"][0]["severity"], "advisory",
        "nothing written: {v}"
    );
    // the agent relays as itself, with the person's words
    let a = accept(&[
        "--relayed-by",
        "agent:w/r7",
        "--evidence",
        "Yes, block on it",
        "--question",
        "Accept the advice?",
        "--session",
        "chat-9",
    ]);
    let (ok, v) = s.json_as("agent:w", Some("r7"), &args(&a));
    assert!(ok, "{v}");
    let (ok, v) = s.json(&["--json", "tensions", "--kind", "semantic"]);
    assert!(ok, "{v}");
    let t = &v["tensions"][0];
    assert_eq!(t["severity"], "blocking", "{t}");
    let acc = &t["judged"]["accepted"];
    assert_eq!(
        (
            acc["actor"].as_str(),
            acc["relayed_by"].as_str(),
            acc["evidence"].as_str(),
            acc["session_ref"].as_str()
        ),
        (
            Some("human:qa"),
            Some("agent:w/r7"),
            Some("Yes, block on it"),
            Some("chat-9")
        ),
        "{acc}"
    );
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
}

/// A served project with a blocking premise shift whose sides agents wrote: its label.
fn served_blocking() -> (Served, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let sp = spec_doc(&mut r);
    put(
        &mut r,
        "dep",
        json!([{"op": "create", "type": "claim", "parent": sp.section.to_string(),
                "body": format!("Builds on {{{{{}}}}}.", sp.a)}]),
    );
    put(&mut r, "edit-a", upd(&sp.a, "A changed."));
    let label = open(&r)
        .iter()
        .find(|t| t.severity == TensionSeverity::Blocking)
        .map(|t| tension_label(&t.id))
        .expect("a blocking tension");
    r.close().unwrap();
    (s, label)
}

#[test]
fn di8_an_agent_relays_a_dismissal_at_the_cli() {
    let (s, tn) = served_blocking();
    // clap pairs the relay flags, as for an approval
    let o = s.rdy_as(
        "agent:w",
        Some("r7"),
        &["resolve", &tn, "--dismiss", "fine", "--evidence", "fine"],
    );
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    // a person does not relay
    let (ok, v) = s.json(&[
        "--json",
        "resolve",
        &tn,
        "--dismiss",
        "fine",
        "--relayed-by",
        "agent:w",
        "--evidence",
        "fine",
    ]);
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    // the agent relays the person's dismissal as itself
    let (ok, v) = s.json_as(
        "agent:w",
        Some("r7"),
        &[
            "--json",
            "resolve",
            &tn,
            "--dismiss",
            "A is settled",
            "--relayed-by",
            "agent:w",
            "--evidence",
            "Dismiss it, A is settled",
            "--session",
            "chat-9",
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["status"], "dismissed", "{v}");
    let (ok, v) = s.json(&["--json", "tensions", "--status", "dismissed", "--reviews"]);
    assert!(ok, "{v}");
    let text = v.to_string();
    assert!(
        text.contains("human:qa"),
        "the review records the person: {v}"
    );
    // the daemon holds the store while it runs
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let (_, a) = answer_of(&r, &tn).expect("the answer");
    assert_eq!(
        (a.act, a.person.actor.as_str(), a.relayed_by),
        (
            AnswerAct::Dismiss,
            "human:qa",
            Some(ActorRun {
                actor: "agent:w".into(),
                run: Some("r7".into())
            })
        )
    );
    assert_eq!(
        (a.evidence_kind, a.relay_evidence.map(|e| e.session_ref)),
        (Some(EvidenceKind::Text), Some("chat-9".to_string()))
    );
    r.fsck().expect("fsck");
}
