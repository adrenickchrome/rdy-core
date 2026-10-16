//! di10 (plan Decision intake, Task 10): the relayed view hand-over and stale close. `own` and
//! `close --stale` are a person's acts (D-KQ4CA). An agent relays them with the person's words:
//! the act runs as the person, and one batch holds the view ops and an `ANSWER` that holds the
//! relay (O14, D-DGCH7). The relay table is the one of every relayed act (D-HA7YZ): a human
//! session cannot relay, and an agent needs the person's words.
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::Local;
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use rdy_model::{ActorRun, Answer, AnswerAct, EvidenceKind, Hash, Role};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_store::kinds;
use rdy_store::view::{ViewOp, ViewOpKind};
use serde_json::json;

/// The relaying agent.
const ME: &str = "agent:relay";

/// The person whose words the agent relays.
const FOUNDER: &str = "human:founder";

const DAY_MS: i64 = 86_400_000;

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
        question: Some("Who takes the view?".into()),
        session_ref: Some("chat-10".into()),
        kind: None,
    }
}

fn req(v: serde_json::Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn new_doc(name: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}])
}

/// A project with the seed doc landed, and views `a` (owned by `agent:a/r1`) and `b` (owned by
/// `agent:b/r1`); `seed` stays open.
fn project_with_views() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "a",
        claim(&section, "a"),
    )
    .unwrap();
    write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        claim(&section, "b"),
    )
    .unwrap();
    (d, r)
}

fn head(r: &Rdy, v: &str) -> String {
    r.read().view_head(v).unwrap().head.to_string()
}

fn owner_of(r: &Rdy, v: &str) -> Option<(String, Option<String>)> {
    r.read()
        .view_head(v)
        .unwrap()
        .owner
        .clone()
        .map(|o| (o.actor, o.run))
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

fn answer_of(r: &Rdy, item: &str) -> Option<(Hash, Answer)> {
    r.store().reader().answer_of(item).expect("answer_of")
}

fn view_op(r: &Rdy, h: &Hash) -> ViewOp {
    r.store().reader().view_op(h).expect("view op")
}

fn refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

#[test]
fn di10_a_relayed_hand_over_records_the_person_and_an_answer() {
    let (dir, mut r) = project_with_views();
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a".into(), Some("r1".into())))
    );
    let info = r
        .view_own_as(
            &agent(),
            FOUNDER,
            &words("Give a to b"),
            "a",
            Some("agent:b/r2"),
        )
        .unwrap();
    assert_eq!(
        info.head.owner,
        Some(OwnerInfo {
            actor: "agent:b".into(),
            run: Some("r2".into())
        })
    );
    assert_eq!(info.relayed_by, Some(relayer()));
    // the view op and the answer are one batch
    let ops = last_of(&r, kinds::VIEW_OP);
    let answers = last_of(&r, kinds::ANSWER);
    assert_eq!((ops.len(), answers.len()), (1, 1), "{:?}", last_kinds(&r));
    assert_eq!(info.answer, Some(answers[0].to_string()));
    // the view op runs as the person
    let op = view_op(&r, &ops[0]);
    assert!(
        matches!(op.op, ViewOpKind::Own { owner: Some(_) }),
        "{op:?}"
    );
    assert_eq!(op.actor, FOUNDER);
    // the answer holds the person and the relay
    let (id, a) = answer_of(&r, "own:a").expect("the answer");
    assert_eq!(id, answers[0]);
    assert_eq!(a.act, AnswerAct::ViewOwn);
    assert_eq!(a.targets, ops);
    assert_eq!(a.person.actor, FOUNDER);
    assert_eq!(a.relayed_by, Some(relayer()));
    let ev = a.relay_evidence.as_ref().expect("evidence");
    assert_eq!(
        (
            ev.quoted.as_str(),
            ev.session_ref.as_str(),
            ev.question.as_deref()
        ),
        ("Give a to b", "chat-10", Some("Who takes the view?"))
    );
    assert_eq!(
        (a.evidence_kind, a.card_hash, a.option, a.note.as_deref()),
        (Some(EvidenceKind::Text), None, None, None)
    );
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:b".into(), Some("r2".into())))
    );

    // leaving a view unowned, through run_view over MCP: the session is the relay
    let e = Local::new(r);
    let v = run_view(
        &e,
        &agent(),
        FOUNDER,
        "work-agent:relay",
        req(json!({"op": "own", "view": "b", "none": true,
            "evidence": "Leave b unowned", "session_ref": "chat-10"})),
    )
    .unwrap();
    assert_eq!(v["owner"], json!(null), "{v}");
    assert_eq!(v["relayed_by"], json!({"actor": ME, "run": "r1"}), "{v}");
    let answer: Hash = v["answer"].as_str().expect("answer").parse().unwrap();
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let (id, a) = answer_of(&r, "own:b").expect("the answer to b");
    assert_eq!(id, answer);
    assert_eq!(
        (a.act, a.person.actor.as_str(), a.relayed_by.clone()),
        (AnswerAct::ViewOwn, FOUNDER, Some(relayer()))
    );
    assert_eq!(
        a.relay_evidence.as_ref().map(|e| e.question.clone()),
        Some(None),
        "no question was passed"
    );
    assert_eq!(owner_of(&r, "b"), None);
    r.fsck().expect("fsck");
}

/// A project whose views `a`, `b` and `seed` are idle, and whose `work` the person wrote to.
fn idle_project() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project_with_views();
    write(&mut r, "work", new_doc("mine")).unwrap();
    (d, r)
}

#[test]
fn di10_a_relayed_stale_close_closes_what_the_person_would() {
    let later = now_ms() + DAY_MS * 2;
    // what the person's own `rdy view close --stale` closes
    let (_d1, mut mine) = idle_project();
    let by_hand = mine.view_close_stale(&human_writer(), 0, later).unwrap();
    assert_eq!(
        by_hand
            .closed
            .iter()
            .map(|c| c.view.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "seed"]
    );
    assert_eq!(
        (by_hand.relayed_by.clone(), by_hand.answer.clone()),
        (None, None)
    );

    // the same, relayed
    let (_d2, mut r) = idle_project();
    let res = r
        .view_close_stale_as(&agent(), FOUNDER, &words("Close the idle ones"), 0, later)
        .unwrap();
    assert_eq!(res.closed, by_hand.closed);
    assert_eq!(res.kept, by_hand.kept);
    assert_eq!(
        res.kept.as_deref(),
        Some("work"),
        "the person's own view stays"
    );
    assert_eq!(res.relayed_by, Some(relayer()));
    // one batch: a close op for each view, and the answer
    let ops = last_of(&r, kinds::VIEW_OP);
    let answers = last_of(&r, kinds::ANSWER);
    assert_eq!((ops.len(), answers.len()), (3, 1), "{:?}", last_kinds(&r));
    assert_eq!(res.answer, Some(answers[0].to_string()));
    for h in &ops {
        let op = view_op(&r, h);
        assert!(
            matches!(&op.op, ViewOpKind::Close { reason } if reason == "idle for more than 0 days"),
            "{op:?}"
        );
        assert_eq!(op.actor, FOUNDER, "the close runs as the person");
        assert!(r.read().view_head(&op.view).unwrap().closed);
    }
    assert!(!r.read().view_head("work").unwrap().closed);
    // the answer names the person's default view, and the ops it wrote
    let (id, a) = answer_of(&r, "stale:work").expect("the answer");
    assert_eq!(id, answers[0]);
    assert_eq!(a.act, AnswerAct::CloseStale);
    assert_eq!(a.targets, ops);
    assert_eq!(a.person.actor, FOUNDER);
    assert_eq!(a.relayed_by, Some(relayer()));
    assert_eq!(
        a.relay_evidence.as_ref().map(|e| e.quoted.as_str()),
        Some("Close the idle ones")
    );
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));

    // nothing idle: no view changes, but the person's words are still recorded (an answer with
    // no targets), and its hash is returned
    let heads: Vec<String> = ["a", "b", "seed", "work"]
        .iter()
        .map(|v| head(&r, v))
        .collect();
    let none = r
        .view_close_stale_as(&agent(), FOUNDER, &words("Again"), 14, now_ms())
        .unwrap();
    assert!(none.closed.is_empty(), "{none:?}");
    assert_eq!(none.relayed_by, Some(relayer()));
    assert_eq!(
        (
            last_of(&r, kinds::ANSWER).len(),
            last_of(&r, kinds::VIEW_OP).len()
        ),
        (1, 0),
        "one answer and no view op: {:?}",
        last_kinds(&r)
    );
    let (id2, a2) = answer_of(&r, "stale:work").expect("the answer");
    assert_ne!(id2, id, "a new answer");
    assert_eq!(none.answer, Some(id2.to_string()));
    assert_eq!(a2.act, AnswerAct::CloseStale);
    assert!(a2.targets.is_empty(), "{a2:?}");
    assert_eq!(a2.person.actor, FOUNDER);
    assert_eq!(a2.relayed_by, Some(relayer()));
    assert_eq!(
        a2.relay_evidence.as_ref().map(|e| e.quoted.as_str()),
        Some("Again")
    );
    assert_eq!(a2.evidence_kind, Some(EvidenceKind::Text));
    let after: Vec<String> = ["a", "b", "seed", "work"]
        .iter()
        .map(|v| head(&r, v))
        .collect();
    assert_eq!(after, heads, "no view head moved");
    r.fsck().expect("fsck");

    // through run_view over MCP: the session is the relay
    let (dir, r) = idle_project();
    std::thread::sleep(std::time::Duration::from_millis(30));
    let e = Local::new(r);
    let v = run_view(
        &e,
        &agent(),
        FOUNDER,
        "work-agent:relay",
        req(json!({"op": "close", "stale": 0, "evidence": "Close the idle ones"})),
    )
    .unwrap();
    let closed: Vec<&str> = v["closed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["view"].as_str())
        .collect();
    assert_eq!(closed, vec!["a", "b", "seed"], "{v}");
    assert_eq!(
        (v["kept"].as_str(), &v["relayed_by"]),
        (Some("work"), &json!({"actor": ME, "run": "r1"})),
        "{v}"
    );
    let answer: Hash = v["answer"].as_str().expect("answer").parse().unwrap();
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(answer_of(&r, "stale:work").map(|(h, _)| h), Some(answer));
    r.fsck().expect("fsck");
}

#[test]
fn di10_an_agent_without_the_persons_words_is_refused() {
    let (dir, mut r) = project_with_views();
    let before = (head(&r, "a"), head(&r, "b"), head(&r, "seed"));
    let nothing = RelayInput::default();
    let e = r
        .view_own_as(&agent(), FOUNDER, &nothing, "a", None)
        .unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "only a human reassigns a view",
        "relay the person's words with evidence",
    );
    let e = r
        .view_close_stale_as(&agent(), FOUNDER, &nothing, 0, now_ms() + DAY_MS * 2)
        .unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "only a human closes views in bulk",
        "relay the person's words with evidence",
    );
    // blank words are no words
    let blank = RelayInput {
        evidence: Some("   ".into()),
        ..RelayInput::default()
    };
    let e = r
        .view_own_as(&agent(), FOUNDER, &blank, "a", None)
        .unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "an agent answers only by relaying a person's words",
        "ask the person, then pass their words as evidence and your question",
    );
    // a question or a session with no words is case 5 of the table
    for (relay, field) in [
        (
            RelayInput {
                question: Some("Who?".into()),
                ..RelayInput::default()
            },
            "question",
        ),
        (
            RelayInput {
                session_ref: Some("chat".into()),
                ..RelayInput::default()
            },
            "session_ref",
        ),
    ] {
        let e = r
            .view_own_as(&agent(), FOUNDER, &relay, "a", None)
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.clone()),
            (E_BAD_INPUT, format!("{field} needs evidence")),
            "{e}"
        );
    }
    // over run_view as well
    let e = Local::new(r);
    let err = run_view(
        &e,
        &agent(),
        FOUNDER,
        "work-agent:relay",
        req(json!({"op": "own", "view": "a", "none": true})),
    )
    .unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err}");
    assert_eq!(err.fix, "relay the person's words with evidence");
    let err = run_view(
        &e,
        &agent(),
        FOUNDER,
        "work-agent:relay",
        req(json!({"op": "close", "stale": 0})),
    )
    .unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "{err}");
    e.close().unwrap();
    // nothing was written
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(
        (head(&r, "a"), head(&r, "b"), head(&r, "seed")),
        before,
        "no view moved"
    );
    assert!(answer_of(&r, "own:a").is_none());
    assert!(answer_of(&r, "stale:work").is_none());
}

#[test]
fn di10_a_human_session_cannot_relay_a_hand_over() {
    let (_d, mut r) = project_with_views();
    let before = head(&r, "a");
    let said = "a person answers as themselves; only the relaying agent records a relay";
    let fix = "drop the relay flags, or let the agent answer with its own RDY_ACTOR";
    let me = human_writer();
    for relay in [
        words("Give a to b"),
        RelayInput {
            relayed_by: Some(ME.into()),
            ..RelayInput::default()
        },
    ] {
        let e = r
            .view_own_as(&me, "human:qa", &relay, "a", Some("agent:b/r2"))
            .unwrap_err();
        refused(&e, E_PERMISSION, said, fix);
        let e = r
            .view_close_stale_as(&me, "human:qa", &relay, 0, now_ms() + DAY_MS * 2)
            .unwrap_err();
        refused(&e, E_PERMISSION, said, fix);
    }
    assert_eq!(head(&r, "a"), before, "nothing was written");
    assert!(answer_of(&r, "own:a").is_none());
    // a person's own act is plain: no relay, no answer
    let info = r
        .view_own_as(
            &me,
            "human:qa",
            &RelayInput::default(),
            "a",
            Some("agent:b/r2"),
        )
        .unwrap();
    assert_eq!((&info.relayed_by, &info.answer), (&None, &None));
    assert!(
        !last_kinds(&r).contains(&kinds::ANSWER),
        "{:?}",
        last_kinds(&r)
    );
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:b".into(), Some("r2".into())))
    );
    let op = view_op(&r, &last_of(&r, kinds::VIEW_OP)[0]);
    assert_eq!(op.actor, "human:qa");
    // the reply of a plain act keeps its bytes: no relayed_by or answer keys
    let v = serde_json::to_value(&info).unwrap();
    assert!(
        v.get("relayed_by").is_none() && v.get("answer").is_none(),
        "{v}"
    );
    let plain = r.view_close_stale(&me, 14, now_ms()).unwrap();
    let v = serde_json::to_value(&plain).unwrap();
    assert_eq!(v, json!({"closed": [], "kept": null}), "{v}");
}

#[test]
fn di10_a_relay_applies_to_own_and_close_with_stale_only() {
    let (dir, r) = project_with_views();
    let e = Local::new(r);
    let run = |v: serde_json::Value| {
        run_view(&e, &agent(), FOUNDER, "work-agent:relay", req(v)).unwrap_err()
    };
    let err = run(json!({"op": "include", "changes": [], "evidence": "yes"}));
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(
        err.message,
        "evidence, question, session_ref and relayed_by apply to own and close with stale"
    );
    let err = run(json!({"op": "close", "view": "a", "evidence": "yes"}));
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = run(json!({"op": "close", "views": ["a", "b"], "stale": 0, "evidence": "yes"}));
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    assert_eq!(
        err.message,
        "a relay applies to close with stale, not to named views"
    );
    let r = {
        e.close().unwrap();
        Rdy::open(dir.path()).unwrap()
    };
    assert!(!r.read().view_head("a").unwrap().closed, "nothing closed");
}

#[test]
fn di10_a_relayed_batch_writes_nothing_when_a_view_fails_its_checks() {
    let (_d, mut r) = project_with_views();
    let a = Answer {
        item: "stale:work".into(),
        act: AnswerAct::CloseStale,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: human_writer().provenance(),
        relayed_by: Some(relayer()),
        relay_evidence: Some(rdy_model::RelayEvidence {
            quoted: "yes".into(),
            session_ref: "chat".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        note: None,
    };
    let close = || vec![ViewOpKind::Close { reason: "x".into() }];
    let by = human_writer().provenance();
    let (ha, hb) = (head(&r, "a"), head(&r, "b"));
    // an unknown view
    let e = r
        .store_mut()
        .view_append_answered(
            &[("a".into(), close()), ("nope".into(), close())],
            &by,
            a.clone(),
        )
        .unwrap_err();
    assert!(e.to_string().contains("nope"), "{e}");
    // a view named twice, and no views
    for appends in [
        vec![("a".to_string(), close()), ("a".to_string(), close())],
        vec![],
    ] {
        r.store_mut()
            .view_append_answered(&appends, &by, a.clone())
            .unwrap_err();
    }
    r.invalidate();
    assert_eq!((head(&r, "a"), head(&r, "b")), (ha, hb), "no view moved");
    assert!(answer_of(&r, "stale:work").is_none(), "no answer");
    // both views and the answer are one batch
    let (heads, answer) = r
        .store_mut()
        .view_append_answered(&[("a".into(), close()), ("b".into(), close())], &by, a)
        .unwrap();
    assert!(heads.iter().all(|h| h.closed));
    r.seal_open().unwrap();
    assert_eq!(
        (
            last_of(&r, kinds::VIEW_OP).len(),
            last_of(&r, kinds::ANSWER)
        ),
        (2, vec![answer])
    );
    let (_, stored) = answer_of(&r, "stale:work").unwrap();
    assert_eq!(stored.targets, last_of(&r, kinds::VIEW_OP));
}

/// A served project with views `a` (owned by `agent:a/r1`) and `seed`, all idle.
fn served_views() -> Served {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "a",
        claim(&section, "a"),
    )
    .unwrap();
    r.close().unwrap();
    s
}

#[test]
fn di10_the_cli_relays_own_and_stale_close() {
    let s = served_views();
    let own = |extra: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = ["--json", "view", "own", "a", "agent:b/r2"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        v.extend(extra.iter().map(|a| a.to_string()));
        v
    };
    let args = |v: &[String]| -> Vec<String> { v.to_vec() };
    let run_as = |actor: &str, run: Option<&str>, v: Vec<String>| {
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        s.json_as(actor, run, &refs)
    };
    // an agent with no relay flags
    let (ok, v) = run_as("agent:w", Some("r7"), args(&own(&[])));
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str(), v["fix"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("only a human reassigns a view"),
            Some("relay the person's words with evidence")
        ),
        "{v}"
    );
    // clap pairs the relay flags, as for an approval
    let v = own(&["--evidence", "give it to b"]);
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    let o = s.rdy_as("agent:w", Some("r7"), &refs);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    // an agent that names another relayer
    let (ok, v) = run_as(
        "agent:w",
        Some("r7"),
        own(&["--relayed-by", "agent:other", "--evidence", "give it to b"]),
    );
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("agent:w relays only as itself, not as agent:other")
        ),
        "{v}"
    );
    // a person does not relay
    let (ok, v) = s.json(
        &own(&["--relayed-by", "agent:w", "--evidence", "give it to b"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("a person answers as themselves; only the relaying agent records a relay")
        ),
        "{v}"
    );
    let (ok, v) = s.json(&["--json", "view", "show", "a"]);
    assert!(ok, "{v}");
    assert_eq!(
        v["head"]["owner"]["actor"], "agent:a",
        "nothing written: {v}"
    );

    // the agent relays the person's hand-over as itself
    let (ok, v) = run_as(
        "agent:w",
        Some("r7"),
        own(&[
            "--relayed-by",
            "agent:w/r7",
            "--evidence",
            "Give it to b",
            "--question",
            "Who takes a?",
            "--session",
            "chat-10",
        ]),
    );
    assert!(ok, "{v}");
    assert_eq!(v["owner"], json!({"actor": "agent:b", "run": "r2"}), "{v}");
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:w", "run": "r7"}),
        "{v}"
    );
    let answer = v["answer"].as_str().expect("answer").to_string();
    // the text names the relayer
    let o = s.rdy_as(
        "agent:w",
        Some("r7"),
        &[
            "view",
            "own",
            "a",
            "--none",
            "--relayed-by",
            "agent:w",
            "--evidence",
            "Leave it unowned",
        ],
    );
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success(), "{o:?}");
    assert_eq!(out.trim(), "view a owner none, relayed by agent:w/r7");

    // the stale close: the person's default view stays, and the idle ones close
    std::thread::sleep(std::time::Duration::from_millis(30));
    let stale = |extra: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = ["--json", "view", "close", "--stale", "0"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        v.extend(extra.iter().map(|a| a.to_string()));
        v
    };
    let (ok, v) = run_as("agent:w", Some("r7"), stale(&[]));
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["fix"].as_str()),
        (
            Some("E_PERMISSION"),
            Some("relay the person's words with evidence")
        ),
        "{v}"
    );
    let (ok, v) = run_as(
        "agent:w",
        Some("r7"),
        stale(&[
            "--relayed-by",
            "agent:w",
            "--evidence",
            "Close what is idle",
        ]),
    );
    assert!(ok, "{v}");
    let closed: Vec<&str> = v["closed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["view"].as_str())
        .collect();
    assert_eq!(closed, vec!["a", "seed"], "{v}");
    assert_eq!(v["relayed_by"]["actor"], "agent:w", "{v}");
    assert!(v["answer"].is_string(), "{v}");
    // the relay flags need --stale
    let o = s.rdy_as(
        "agent:w",
        Some("r7"),
        &[
            "view",
            "close",
            "a",
            "--relayed-by",
            "agent:w",
            "--evidence",
            "x",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{o:?}");

    // the daemon holds the store while it runs
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let (h, a) = answer_of(&r, "own:a").expect("the answer");
    assert_eq!(h.to_string().len(), answer.len());
    assert_eq!(
        (a.act, a.person.actor.as_str(), a.relayed_by.clone()),
        (
            AnswerAct::ViewOwn,
            "human:qa",
            Some(ActorRun {
                actor: "agent:w".into(),
                run: Some("r7".into())
            })
        )
    );
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    let (_, a) = answer_of(&r, "stale:work").expect("the stale answer");
    assert_eq!(
        (a.act, a.person.actor.as_str(), a.targets.len()),
        (AnswerAct::CloseStale, "human:qa", 2)
    );
    assert!(r.read().view_head("a").unwrap().closed);
    r.fsck().expect("fsck");
}
