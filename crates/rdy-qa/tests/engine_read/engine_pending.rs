//! di6 (plan Decision intake, Task 6): the queue's questions, tensions and snoozes. An open
//! question with authored options is an `answer` item (F7); an advised tension is one `resolve`
//! item for its version (D-WMCFW); a blocking tension of an open proposal is a `resolve` item of
//! its own (D-562QR); a snooze hides an item in its chat session only, and ends with the item's
//! version (F4). Nothing of the queue reaches the site (D-X61DT).
use rdy_engine::coverage::IssueCode;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, Pending, PendingQuery, Recommended};
use rdy_engine::{DecisionInput, Rdy, Scope, Session};
use rdy_model::{ActorRun, Answer, AnswerAct, BlockId, EvidenceKind, RelayEvidence, Role};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{land, version_of, write, writer};
use rdy_qa::intake::*;
use rdy_qa::tensions::{ag, created, open, put, upd};
use rdy_render::render_site;
use serde_json::json;
use std::path::Path;

/// The queue of `r` for [`PERSON`] in chat session `session`, every card whole.
fn queue(r: &Rdy, session: Option<&str>) -> Pending {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: session.map(str::to_string),
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
}

fn find<'a>(q: &'a Pending, r#ref: &str) -> Option<&'a Item> {
    q.items.iter().find(|i| i.r#ref == r#ref)
}

fn refs(q: &Pending) -> Vec<&str> {
    q.items.iter().map(|i| i.r#ref.as_str()).collect()
}

/// A snooze of `item` by [`PERSON`], relayed in chat session `session`.
fn snooze(item: &str, session: &str) -> Answer {
    Answer {
        relayed_by: Some(ActorRun {
            actor: "agent:chat".into(),
            run: Some("r1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "later".into(),
            session_ref: session.into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        ..answer(item, AnswerAct::Later, "later", "Later")
    }
}

/// The spec doc in canon, and a question with `fields` landed from view `qs`; the question's id
/// and its item ref.
fn with_question(fields: serde_json::Value) -> (tempfile::TempDir, Rdy, Spec, BlockId, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let (r, q, item) = add_question(dir.path(), r, &spec, fields);
    (dir, r, spec, q, item)
}

fn add_question(
    dir: &Path,
    mut r: Rdy,
    spec: &Spec,
    fields: serde_json::Value,
) -> (Rdy, BlockId, String) {
    let w = write(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": spec.section.to_string(),
                "fields": fields}]),
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

fn refund_question() -> serde_json::Value {
    json!({
        "prompt": "Who approves refunds?",
        "title": "Who approves refunds?",
        "why": "Support waits on this",
        "options": ["Finance", "Support lead", "Either"],
        "recommended": "Finance",
        "recommend_reason": "Finance owns the ledger"
    })
}

#[test]
fn di6_a_question_with_options_is_an_answer_item() {
    let (_d, r, _s, _q, item) = with_question(refund_question());
    let q = queue(&r, None);
    let i = find(&q, &item).unwrap_or_else(|| panic!("{item} in {:?}", refs(&q)));
    assert_eq!(
        (i.kind, i.title.as_str(), i.why.as_str(), i.blocking),
        (
            ItemKind::Answer,
            "Who approves refunds?",
            "Support waits on this",
            false
        )
    );
    let opts: Vec<(&str, &str, Option<&str>)> = i
        .options
        .iter()
        .map(|o| {
            let reason = o.recommended.as_ref().map(Recommended::reason);
            (o.id.as_str(), o.label.as_str(), reason)
        })
        .collect();
    assert_eq!(
        opts,
        vec![
            ("o1", "Finance", Some("Finance owns the ledger")),
            ("o2", "Support lead", None),
            ("o3", "Either", None),
            ("later", "Later", None),
        ]
    );
    assert_eq!(
        i.options[0].consequences,
        vec!["Records a decision with this answer."]
    );
    assert_eq!(
        i.options[3].consequences,
        vec!["Hides this card until the chat session changes."]
    );
    assert!(
        i.card
            .starts_with(&format!("{item}  Who approves refunds?")),
        "{}",
        i.card
    );
    assert!(!i.card_hash.is_empty());
    assert_eq!((i.batch.as_deref(), i.exception), (None, None));
    assert_eq!(i.raised_by, "human:qa");
}

#[test]
fn di6_a_question_without_options_is_not_an_item() {
    // F7: it stays on the coverage report
    let (_d, r, _s, q, _item) = with_question(json!({"prompt": "Who approves refunds?"}));
    assert!(queue(&r, None).items.is_empty());
    let issues = r.read().coverage_issues(&Scope::Canon).unwrap();
    let codes: Vec<IssueCode> = issues.blocks[&q].issues.iter().map(|x| x.code).collect();
    assert!(codes.contains(&IssueCode::OpenQuestion), "{codes:?}");
    // one option, five options and a blank option are not authored options either
    for options in [
        json!(["Finance"]),
        json!(["a", "b", "c", "d", "e"]),
        json!(["Finance", " "]),
    ] {
        let (_d, r, _s, _q, item) =
            with_question(json!({"prompt": "Who approves refunds?", "options": options}));
        assert!(find(&queue(&r, None), &item).is_none(), "{options}");
    }
}

#[test]
fn di6_an_answered_question_leaves_the_queue() {
    let (dir, r, spec, q, item) = with_question(refund_question());
    assert!(find(&queue(&r, None), &item).is_some());
    // the person's answer is recorded: the question is still open in canon, so it stays
    let mut r = put_answer(
        dir.path(),
        r,
        &answer(&item, AnswerAct::AnswerQuestion, "o1", "Finance"),
    );
    assert!(find(&queue(&r, None), &item).is_some());
    // a decision that answers it lands: the question is answered in canon
    r.decide(
        &writer(),
        "decide",
        DecisionInput {
            parent: spec.section.to_string(),
            statement: "Finance approves refunds".into(),
            answers: vec![q.to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    // proposed in a view only: canon still holds the question open
    assert!(find(&queue(&r, None), &item).is_some());
    land(&mut r, "decide").unwrap();
    assert!(find(&queue(&r, None), &item).is_none());
    assert!(
        queue(&r, None)
            .items
            .iter()
            .all(|i| i.kind != ItemKind::Answer)
    );
}

#[test]
fn di6_an_abandoned_answering_decision_leaves_the_question_in_the_queue() {
    let (dir, mut r, spec, q, item) = with_question(refund_question());
    r.decide(
        &writer(),
        "decide",
        DecisionInput {
            parent: spec.section.to_string(),
            statement: "Finance approves refunds".into(),
            answers: vec![q.to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    let r = put_answer(
        dir.path(),
        r,
        &answer(&item, AnswerAct::AnswerQuestion, "o1", "Finance"),
    );
    let mut r = r;
    // the answering decision is abandoned: it never reaches canon
    r.view_close(&writer(), &["decide".to_string()], "not this one")
        .unwrap();
    let after = queue(&r, None);
    assert!(
        find(&after, &item).is_some(),
        "the question is unanswered in canon: {:?}",
        refs(&after)
    );
}

#[test]
fn di6_a_question_that_a_landed_decision_answers_is_no_item() {
    // no answer record: the decision's `answers` link alone closes the question
    let (_d, mut r, spec, q, item) = with_question(refund_question());
    r.decide(
        &writer(),
        "decide",
        DecisionInput {
            parent: spec.section.to_string(),
            statement: "Finance approves refunds".into(),
            answers: vec![q.to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    // proposed in a view only: canon still holds the question open
    assert!(find(&queue(&r, None), &item).is_some());
    land(&mut r, "decide").unwrap();
    assert!(find(&queue(&r, None), &item).is_none());
}

#[test]
fn di6_advised_tensions_are_one_item_per_version() {
    let (_d, mut r, b) = batch_project();
    let advice = format!("advice:{}", b.r#ref);
    let q = queue(&r, None);
    let i = find(&q, &advice).expect("the advice item");
    assert_eq!(
        (i.kind, i.blocking, i.batch.as_deref(), i.title.as_str()),
        (
            ItemKind::Resolve,
            false,
            Some(b.r#ref.as_str()),
            "1 tension a reviewer advised to block"
        )
    );
    assert_eq!(
        i.why,
        format!("the reviewers advise that these block {}", b.r#ref)
    );
    assert_eq!(i.members.len(), 1);
    assert!(
        i.members[0].contains("these two cannot both hold"),
        "{:?}",
        i.members
    );
    assert_eq!(i.facts.len(), 1);
    assert_eq!(i.facts[0].name, "Advised digest");
    assert_eq!(
        i.facts[0].value,
        b.report["advised_digest"].as_str().unwrap()
    );
    let ids: Vec<&str> = i.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["accept", "dismiss", "later"]);
    // it raised with the batch
    let batch = find(&q, &b.r#ref).unwrap();
    assert_eq!(
        (&i.raised_by, &i.raised_at, &i.age),
        (&batch.raised_by, &batch.raised_at, &batch.age)
    );
    // a second version is a second batch: the advice moves to it, still one item
    put(
        &mut r,
        BATCH_VIEW,
        json!([{"op": "create", "type": "claim", "parent": b.spec.section.to_string(),
                "body": "One more claim."}]),
    );
    let v2 = r
        .propose_subset(
            &rdy_qa::tensions::ag(BATCH_VIEW),
            BATCH_VIEW,
            BATCH_TITLE,
            None,
            Some(BATCH_SUMMARY),
        )
        .unwrap();
    assert_ne!(v2.r#ref, b.r#ref);
    let q = queue(&r, None);
    let advices: Vec<&str> = q
        .items
        .iter()
        .filter(|i| i.r#ref.starts_with("advice:"))
        .map(|i| i.r#ref.as_str())
        .collect();
    assert_eq!(advices, vec![format!("advice:{}", v2.r#ref)]);
    // the advice stands last in its batch's group
    let at = q.items.iter().position(|i| i.r#ref == v2.r#ref).unwrap();
    let group: Vec<&Item> = q.items[at..]
        .iter()
        .take_while(|i| i.r#ref == v2.r#ref || i.batch.as_deref() == Some(v2.r#ref.as_str()))
        .collect();
    assert_eq!(group.last().unwrap().r#ref, format!("advice:{}", v2.r#ref));
}

/// `batch_project` with a decision that depends on claim A and another view that edits A: an
/// pair of open blocking tensions (a premise shift each way) that involve the batch. Their
/// labels, in id order.
fn blocking_on_the_batch() -> (tempfile::TempDir, Rdy, Batch, Vec<String>) {
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
    let (r, report) = propose_reported(dir.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let b = Batch {
        r#ref: report["ref"].as_str().unwrap().to_string(),
        report,
        ..b
    };
    let mut labels: Vec<String> = open(&r)
        .iter()
        .filter(|t| t.severity == rdy_engine::tensions::TensionSeverity::Blocking)
        .map(|t| rdy_engine::tensions::tension_label(&t.id))
        .collect();
    labels.sort();
    assert_eq!(labels.len(), 2, "one premise shift each way");
    (dir, r, b, labels)
}

#[test]
fn di6_a_blocking_tension_on_a_proposal_is_a_resolve_item() {
    let (_d, mut r, b, labels) = blocking_on_the_batch();
    let q = queue(&r, None);
    assert!(q.not_ready.is_empty(), "{:#?}", q.not_ready);
    let label = labels[0].clone();
    let i = find(&q, &label).unwrap_or_else(|| panic!("{label} in {:?}", refs(&q)));
    assert_eq!(
        (i.kind, i.blocking, i.batch.as_deref(), i.exception),
        (ItemKind::Resolve, true, None, None)
    );
    let ids: Vec<&str> = i.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["dismiss", "later"]);
    assert_eq!(
        i.options[0].consequences,
        vec!["Dismisses the tension with your reason."]
    );
    assert!(i.options.iter().all(|o| o.recommended.is_none()));
    assert!(i.why.contains(&b.r#ref), "{}", i.why);
    assert!(
        i.facts
            .iter()
            .any(|f| f.name == "Involves" && f.value == b.r#ref),
        "{:#?}",
        i.facts
    );
    // blocking items stand first (O5), one for each tension, in order
    let first: Vec<&str> = q.items[..2].iter().map(|i| i.r#ref.as_str()).collect();
    assert_eq!(first, labels.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(q.items[..2].iter().all(|i| i.blocking));
    assert!(q.items[2..].iter().all(|i| !i.blocking));
    // a person dismisses one: it leaves the queue, the other stays
    let person = Session::human("human:founder".into(), Role::Approver);
    r.dismiss_tension(&person, &label, "A is stable").unwrap();
    let q = queue(&r, None);
    assert!(find(&q, &label).is_none(), "{:?}", refs(&q));
    assert!(find(&q, &labels[1]).is_some(), "{:?}", refs(&q));
}

#[test]
fn di6_a_blocking_tension_that_no_open_proposal_holds_is_no_item() {
    // a premise shift whose decision no open proposal carries
    let (_d, r, _b) = batch_project();
    let q = queue(&r, None);
    assert!(q.items.iter().all(|i| !i.blocking), "{:?}", refs(&q));
}

/// D-K0SS1: closing the batch's view withdraws its version: its card, its confirm items and
/// the resolve items of its blocking tensions leave the queue.
#[test]
fn vw1_closing_the_batch_view_takes_its_card_and_blocking_items_out_of_the_queue() {
    let (_d, mut r, b, labels) = blocking_on_the_batch();
    let q = queue(&r, None);
    assert!(
        find(&q, &b.r#ref).is_some() || q.not_ready.iter().any(|n| n.r#ref == b.r#ref),
        "{:?} {:#?}",
        refs(&q),
        q.not_ready
    );
    for l in &labels {
        assert!(find(&q, l).is_some(), "{l} in {:?}", refs(&q));
    }
    r.view_close(&ag(BATCH_VIEW), &[BATCH_VIEW.into()], "done")
        .unwrap();
    let q = queue(&r, None);
    assert!(
        q.items
            .iter()
            .all(|i| i.r#ref != b.r#ref && i.batch.as_deref() != Some(b.r#ref.as_str())),
        "{:?}",
        refs(&q)
    );
    assert!(
        q.not_ready.iter().all(|n| n.r#ref != b.r#ref),
        "{:#?}",
        q.not_ready
    );
    for l in &labels {
        assert!(find(&q, l).is_none(), "{l} in {:?}", refs(&q));
    }
    assert!(q.items.iter().all(|i| !i.blocking), "{:?}", refs(&q));
}

#[test]
fn di6_an_accepted_advice_leaves_its_blocking_tension_as_an_item() {
    // the person accepted the advice: the advice card is answered, but the tension blocks
    // until it is dismissed or a side changes, so it is a blocking resolve item
    let (_d, mut r, b) = batch_project();
    let advice = format!("advice:{}", b.r#ref);
    assert!(find(&queue(&r, None), &advice).is_some());
    r.accept_advice(
        &writer(),
        "human:qa",
        rdy_engine::tensions::api::AcceptRequest {
            proposal: b.r#ref.clone(),
            advised_digest: b.report["advised_digest"].as_str().unwrap().into(),
            ..Default::default()
        },
    )
    .unwrap();
    let q = queue(&r, None);
    assert!(find(&q, &advice).is_none(), "{:?}", refs(&q));
    assert_eq!(q.not_ready.len(), 1);
    let blocking: Vec<&Item> = q.items.iter().filter(|i| i.blocking).collect();
    assert_eq!(blocking.len(), 1, "{:?}", refs(&q));
    let i = blocking[0];
    assert_eq!((i.kind, i.batch.as_deref()), (ItemKind::Resolve, None));
    assert!(i.why.contains(&b.r#ref), "{}", i.why);
    // a person dismisses it: it leaves the queue
    let person = Session::human("human:founder".into(), Role::Approver);
    r.dismiss_tension(&person, &i.r#ref.clone(), "judged fine")
        .unwrap();
    let q = queue(&r, None);
    assert!(q.items.iter().all(|i| !i.blocking), "{:?}", refs(&q));
}

#[test]
fn di6_a_snooze_hides_an_item_in_its_session_only() {
    // F4
    let (dir, r, b) = batch_project();
    let advice = format!("advice:{}", b.r#ref);
    let all = queue(&r, Some("s1"));
    assert!(find(&all, &b.r#ref).is_some() && find(&all, &advice).is_some());
    let total = all.items.len();
    let r = put_answer(dir.path(), r, &snooze(&b.r#ref, "s1"));
    let r = put_answer(dir.path(), r, &snooze(&advice, "s1"));
    // in the session of the snooze: hidden and counted; the rest stays
    let q = queue(&r, Some("s1"));
    assert!(find(&q, &b.r#ref).is_none() && find(&q, &advice).is_none());
    assert_eq!((q.snoozed, q.items.len()), (2, total - 2));
    assert!(q.items.iter().any(|i| i.kind == ItemKind::Confirm));
    // another session sees it
    let q = queue(&r, Some("s2"));
    assert!(find(&q, &b.r#ref).is_some() && find(&q, &advice).is_some());
    assert_eq!((q.snoozed, q.items.len()), (0, total));
    // no session: nothing is hidden
    let q = queue(&r, None);
    assert_eq!((q.snoozed, q.items.len()), (0, total));
    // another person is not hidden either
    let other = r
        .read()
        .pending(&PendingQuery {
            person: "human:other".into(),
            session_ref: Some("s1".into()),
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    assert_eq!((other.snoozed, other.items.len()), (0, total));
}

#[test]
fn di6_a_local_snooze_hides_nothing() {
    // the CLI has no Later: a snooze with no relay names no session
    let (dir, r, b) = batch_project();
    let a = answer(&b.r#ref, AnswerAct::Later, "later", "Later");
    let r = put_answer(dir.path(), r, &a);
    let q = queue(&r, Some("s1"));
    assert!(find(&q, &b.r#ref).is_some());
    assert_eq!(q.snoozed, 0);
}

#[test]
fn di6_a_new_version_ends_a_snooze() {
    let (dir, r, b) = batch_project();
    let r = put_answer(dir.path(), r, &snooze(&b.r#ref, "s1"));
    assert!(find(&queue(&r, Some("s1")), &b.r#ref).is_none());
    let mut r = r;
    put(
        &mut r,
        BATCH_VIEW,
        json!([{"op": "create", "type": "claim", "parent": b.spec.section.to_string(),
                "body": "One more claim."}]),
    );
    let v2 = r
        .propose_subset(
            &rdy_qa::tensions::ag(BATCH_VIEW),
            BATCH_VIEW,
            BATCH_TITLE,
            None,
            Some(BATCH_SUMMARY),
        )
        .unwrap();
    let q = queue(&r, Some("s1"));
    assert!(find(&q, &v2.r#ref).is_some(), "{:?}", refs(&q));
    // a question's snooze ends with a new version of the question as well
    let (dir, r, _spec, qid, item) = with_question(refund_question());
    let mut r = put_answer(dir.path(), r, &snooze(&item, "s1"));
    assert!(find(&queue(&r, Some("s1")), &item).is_none());
    write(
        &mut r,
        "qs2",
        json!([{"op": "update", "id": qid.to_string(), "fields": {"why": "Support still waits"}}]),
    )
    .unwrap();
    land(&mut r, "qs2").unwrap();
    let v = version_of(&r, &Scope::Canon, &qid.to_string());
    let newer = format!("{qid}@{}", &v[..12]);
    assert_ne!(newer, item);
    assert!(find(&queue(&r, Some("s1")), &newer).is_some());
}

#[test]
fn di6_the_queue_survives_reindex() {
    // a project with every part: a batch with exceptions and advice, a blocking tension, a
    // question, and a snooze
    let (dir, r, b, labels) = blocking_on_the_batch();
    let label = labels[0].clone();
    let (r, _q, item) = add_question(dir.path(), r, &b.spec, refund_question());
    let advice = format!("advice:{}", b.r#ref);
    let mut r = put_answer(dir.path(), r, &snooze(&advice, "s1"));
    for session in [Some("s1"), Some("s2"), None] {
        let before = queue(&r, session);
        r.reindex().unwrap();
        let after = queue(&r, session);
        assert_eq!(before, after, "{session:?}");
        assert!(!after.items.is_empty());
    }
    let q = queue(&r, Some("s1"));
    assert!(find(&q, &label).is_some() && find(&q, &item).is_some());
    assert!(find(&q, &advice).is_none());
    // and a reopen
    r.close().unwrap();
    let again = Rdy::open(dir.path()).unwrap();
    assert_eq!(q, queue(&again, Some("s1")));
}

/// Every file under `dir`, as (relative path, text).
fn site_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

fn render(r: &Rdy, scope: &Scope) -> Vec<(String, String)> {
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), scope, out.path()).unwrap();
    site_files(out.path())
}

#[test]
fn di6_nothing_from_the_queue_reaches_the_site() {
    // guard (D-X61DT): the site has no item ref, card line or option label, and the open
    // questions page does not move when a person answers
    let (dir, r, b, labels) = blocking_on_the_batch();
    let label = labels[0].clone();
    let (r, _q, item) = add_question(dir.path(), r, &b.spec, refund_question());
    let q = queue(&r, None);
    let canon = render(&r, &Scope::Canon);
    let view = render(&r, &Scope::View(BATCH_VIEW.into()));
    let questions = canon
        .iter()
        .find(|(p, _)| p == "questions.html")
        .map(|(_, t)| t.clone())
        .expect("questions.html");
    assert!(questions.contains("Who approves refunds?"));
    // answers of every kind, then the same renders
    let mut r = r;
    let advice = format!("advice:{}", b.r#ref);
    for a in [
        snooze(&item, "s1"),
        snooze(&advice, "s1"),
        answer(&item, AnswerAct::AnswerQuestion, "o1", "Finance"),
        answer(
            &q.items
                .iter()
                .find(|i| i.r#ref.starts_with(&format!("{}@", b.one_way)))
                .unwrap()
                .r#ref,
            AnswerAct::Confirm,
            "confirm",
            "Confirm",
        ),
    ] {
        r = put_answer(dir.path(), r, &a);
    }
    let canon_after = render(&r, &Scope::Canon);
    let view_after = render(&r, &Scope::View(BATCH_VIEW.into()));
    assert_eq!(canon, canon_after, "an answer changes no page");
    assert_eq!(view, view_after, "an answer changes no page");
    let questions_after = canon_after
        .iter()
        .find(|(p, _)| p == "questions.html")
        .map(|(_, t)| t.as_str())
        .unwrap();
    assert_eq!(questions, questions_after, "questions.html is byte-equal");
    // no item ref, card line or option label on any page
    let mut needles: Vec<String> = vec![label, advice, item];
    for i in &q.items {
        needles.push(i.r#ref.clone());
        needles.extend(i.card.lines().map(|l| l.trim().to_string()));
        needles.extend(i.options.iter().map(|o| o.label.clone()));
    }
    needles.retain(|n| n.len() > 5);
    needles.sort();
    needles.dedup();
    for (page, text) in canon.iter().chain(view.iter()) {
        for n in &needles {
            assert!(!text.contains(n.as_str()), "{page} holds {n:?}");
        }
    }
}
