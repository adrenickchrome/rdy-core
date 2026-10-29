//! di7 (plan Decision intake, Task 7): one answer op, bound to the card the person was shown
//! (D-JV6Z8, D-ST423, F6). The relay table is one closed table for every act (D-HA7YZ, D-BGYNM):
//! a person answers as themselves, an agent only by relaying the person's words, and only as
//! itself. An approval waits for every exception's confirm (D-YFDG3, F2); a rejected decision
//! holds its batch back (F1); Later needs a chat session (F4). The daemon's relay texts do not
//! change (O19).
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::answer::relay::{RelayInput, RelayRefusal, relayed};
use rdy_engine::answer::{AnswerInfo, AnswerRequest};
use rdy_engine::engine::Local;
use rdy_engine::pending::{Item, ItemKind, Pending};
use rdy_engine::{
    E_BAD_INPUT, E_EXCEPTION_OPEN, E_ITEM_MOVED, E_PERMISSION, E_PROPOSAL_MOVED,
    E_PROPOSAL_NOT_READY, E_VIEW_CLOSED, Rdy, RdyError, Scope, Session,
};
use rdy_model::{ActorRun, Answer, AnswerAct, EvidenceKind, Hash, Role};
use rdy_qa::fixtures::{approver, land};
use rdy_qa::intake::*;
use rdy_qa::tensions::{ag, put};
use serde_json::{Value, json};

/// The relaying agent.
const ME: &str = "agent:relay";

fn agent() -> Session {
    Session::agent(ME.into(), Some("r1".into()), Role::Writer).expect("agent")
}

/// The person's words, the question they answered and the chat session, relayed over MCP (no
/// `relayed_by`).
fn words(quote: &str) -> RelayInput {
    RelayInput {
        relayed_by: None,
        evidence: Some(quote.into()),
        question: Some("Shall I record your answer?".into()),
        session_ref: Some("chat-7".into()),
        kind: None,
    }
}

fn req(r#ref: &str, option: &str, card_hash: Option<&str>, relay: RelayInput) -> AnswerRequest {
    AnswerRequest {
        r#ref: r#ref.into(),
        option: option.into(),
        card_hash: card_hash.map(str::to_string),
        note: None,
        relay,
        brief: None,
    }
}

fn item(q: &Pending, r#ref: &str) -> Item {
    q.items
        .iter()
        .find(|i| i.r#ref == r#ref)
        .unwrap_or_else(|| {
            let refs: Vec<&str> = q.items.iter().map(|i| i.r#ref.as_str()).collect();
            panic!("{ref} in {refs:?}")
        })
        .clone()
}

/// The `confirm` items beside batch `batch`, in queue order.
fn exceptions_of(q: &Pending, batch: &str) -> Vec<Item> {
    q.items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(batch))
        .cloned()
        .collect()
}

/// Relays the person's "yes" for `option` on item `r#ref`, bound to the card the queue shows now.
fn relay_answer(r: &mut Rdy, r#ref: &str, option: &str) -> Result<AnswerInfo, RdyError> {
    let h = item(&pending(r), r#ref).card_hash;
    r.answer(&agent(), PERSON, req(r#ref, option, Some(&h), words("yes")))
}

fn refused(e: &RdyError, code: &str, message: &str) {
    assert_eq!((e.code, e.message.as_str()), (code, message), "{e}");
}

/// Every answer and snooze the published state holds.
fn answers(r: &Rdy) -> Vec<(String, Answer)> {
    let st = r.read();
    ["answer/", "pending/"]
        .iter()
        .flat_map(|p| st.reader().answers_with_prefix(p).expect("answers"))
        .map(|(name, _, a)| (name, a))
        .collect()
}

fn answer_of(r: &Rdy, item: &str) -> Answer {
    r.read()
        .reader()
        .answer_of(item)
        .expect("answer_of")
        .unwrap_or_else(|| panic!("no answer to {item}"))
        .1
}

fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

fn card_json(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

fn landings(r: &Rdy) -> usize {
    r.read().log_rows().expect("log").len()
}

fn hash(text: &str) -> Hash {
    text.parse().expect("hash")
}

#[test]
fn di7_the_relay_table_holds_for_every_act() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let ex = exceptions_of(&q, &b.r#ref);
    let batch = item(&q, &b.r#ref);
    let acts = [
        (b.r#ref.clone(), "approve", batch.card_hash.clone()),
        (b.r#ref.clone(), "reject", batch.card_hash.clone()),
        (b.r#ref.clone(), "later", batch.card_hash.clone()),
        (ex[0].r#ref.clone(), "confirm", ex[0].card_hash.clone()),
    ];
    let head = r.read().canon_head();
    let long = "x".repeat(2001);
    for (at, option, h) in &acts {
        let ask = |relay: RelayInput| req(at, option, Some(h), relay);
        // 2. a human session that names a relay
        for relay in [
            RelayInput {
                relayed_by: Some(ME.into()),
                ..words("yes")
            },
            words("yes"),
            RelayInput {
                relayed_by: Some(ME.into()),
                ..RelayInput::default()
            },
        ] {
            let e = r.answer(&approver(), PERSON, ask(relay)).unwrap_err();
            refused(
                &e,
                E_PERMISSION,
                "a person answers as themselves; only the relaying agent records a relay",
            );
            assert_eq!(
                e.fix,
                "drop the relay flags, or let the agent answer with its own RDY_ACTOR"
            );
        }
        // 3. an agent session with no evidence
        for relay in [
            RelayInput::default(),
            RelayInput {
                relayed_by: Some(ME.into()),
                ..RelayInput::default()
            },
            RelayInput {
                evidence: Some("   ".into()),
                ..RelayInput::default()
            },
        ] {
            let e = r.answer(&agent(), PERSON, ask(relay)).unwrap_err();
            refused(
                &e,
                E_PERMISSION,
                "an agent answers only by relaying a person's words",
            );
            assert_eq!(
                e.fix,
                "ask the person, then pass their words as evidence and your question"
            );
        }
        // 4. a relayed_by that is not the session's own actor: the flag names the actor alone
        // (D-PWB82), so the actor joined to the session's run is another actor
        for by in [
            "agent:other",
            "agent:relay/r1",
            "agent:relay/r2",
            "agent:rel",
        ] {
            let relay = RelayInput {
                relayed_by: Some(by.into()),
                ..words("yes")
            };
            let e = r.answer(&agent(), PERSON, ask(relay)).unwrap_err();
            refused(
                &e,
                E_PERMISSION,
                &format!("agent:relay (run r1) relays only as itself, not as {by}"),
            );
        }
        // 4. a relayed_by outside the actor form: the grammar's refusal, never split
        let relay = RelayInput {
            relayed_by: Some("agent:relay/r1/x".into()),
            ..words("yes")
        };
        let e = r.answer(&agent(), PERSON, ask(relay)).unwrap_err();
        refused(
            &e,
            E_BAD_INPUT,
            "the actor \"agent:relay/r1/x\" is not in rdy's actor form",
        );
        // 5. question or session_ref with no evidence
        for (field, relay) in [
            (
                "question",
                RelayInput {
                    question: Some("Land it?".into()),
                    ..RelayInput::default()
                },
            ),
            (
                "session_ref",
                RelayInput {
                    session_ref: Some("chat-7".into()),
                    ..RelayInput::default()
                },
            ),
        ] {
            let e = r.answer(&approver(), PERSON, ask(relay)).unwrap_err();
            refused(&e, E_BAD_INPUT, &format!("{field} needs evidence"));
            assert_eq!(e.fix, format!("pass evidence with it, or drop {field}"));
        }
        // 6. a quote or a question past 2000 characters
        for relay in [
            words(&long),
            RelayInput {
                question: Some(long.clone()),
                ..words("yes")
            },
        ] {
            let e = r.answer(&agent(), PERSON, ask(relay)).unwrap_err();
            refused(
                &e,
                E_BAD_INPUT,
                "evidence and question take at most 2000 characters each",
            );
        }
    }
    // a refusal writes nothing
    assert!(answers(&r).is_empty(), "{:#?}", answers(&r));
    assert_eq!(r.read().canon_head(), head);

    // 4, at the CLI: relayed_by is the actor; the run is the session's; session_ref defaults
    // to cli
    let snooze = r
        .answer(
            &agent(),
            PERSON,
            req(
                &b.r#ref,
                "later",
                Some(&batch.card_hash),
                RelayInput {
                    relayed_by: Some(ME.into()),
                    session_ref: None,
                    ..words("later, please")
                },
            ),
        )
        .unwrap();
    let me = ActorRun {
        actor: ME.into(),
        run: Some("r1".into()),
    };
    assert_eq!(
        (snooze.assurance, snooze.relayed_by.as_ref()),
        ("relayed", Some(&me))
    );
    let (_, a) = r
        .read()
        .reader()
        .snooze_of(PERSON, &b.r#ref)
        .unwrap()
        .expect("a snooze");
    assert_eq!(
        (a.person.actor.as_str(), a.person.role),
        (PERSON, Role::Approver)
    );
    assert_eq!(a.relayed_by.as_ref(), Some(&me));
    assert_eq!(a.relay_evidence.as_ref().unwrap().session_ref, "cli");
    // 4, over MCP: no relayed_by, the session is the relay; session_ref defaults to mcp
    let info = r
        .answer(
            &agent(),
            PERSON,
            req(
                &ex[0].r#ref,
                "confirm",
                Some(&ex[0].card_hash),
                RelayInput {
                    session_ref: None,
                    ..words("confirm it")
                },
            ),
        )
        .unwrap();
    assert_eq!(info.assurance, "relayed");
    let a = answer_of(&r, &ex[0].r#ref);
    assert_eq!(a.person.actor, PERSON);
    assert_eq!(a.relayed_by.as_ref(), Some(&me));
    let ev = a.relay_evidence.as_ref().unwrap();
    assert_eq!(
        (ev.quoted.as_str(), ev.session_ref.as_str()),
        ("confirm it", "mcp")
    );
    assert_eq!(ev.question.as_deref(), Some("Shall I record your answer?"));
    // 1. a human session answers as itself: local, no relay
    let info = r
        .answer(
            &approver(),
            PERSON,
            req(&b.r#ref, "reject", None, RelayInput::default()),
        )
        .unwrap();
    assert_eq!((info.assurance, info.relayed_by), ("local", None));
    let a = answer_of(&r, &b.r#ref);
    assert_eq!(a.person.actor, PERSON);
    assert!(a.relayed_by.is_none() && a.relay_evidence.is_none() && a.evidence_kind.is_none());

    // the table itself: the sessions it gives and its closed refusals
    let person = Session::human(PERSON.into(), Role::Approver).unwrap();
    let (s, relay) = relayed(&approver(), PERSON, &RelayInput::default(), false).unwrap();
    assert_eq!((s, relay.is_none()), (person.clone(), true));
    let (s, relay) = relayed(&agent(), PERSON, &words("yes"), true).unwrap();
    assert_eq!(s, person);
    assert_eq!(relay.expect("a relay").by, me);
    let mcp_by = RelayInput {
        relayed_by: Some(ME.into()),
        ..words("yes")
    };
    assert_eq!(
        relayed(&agent(), PERSON, &mcp_by, true).unwrap_err(),
        RelayRefusal::RelayedByOverMcp
    );
    assert_eq!(
        relayed(&agent(), PERSON, &words("yes"), false).unwrap_err(),
        RelayRefusal::NoRelayedBy
    );
    let e = RelayRefusal::RelayedByOverMcp.error();
    refused(
        &e,
        E_BAD_INPUT,
        "relayed_by names the agent relaying at the command line; over MCP the session is the relay",
    );
    // exactly 2000 characters is a quote
    let at_most = "y".repeat(2000);
    assert!(relayed(&agent(), PERSON, &words(&at_most), true).is_ok());
}

#[test]
fn di7_a_relayed_approval_records_card_evidence() {
    let (_d, mut r, m) = ready_and_not_ready();
    let it = item(&pending(&r), &m.ready.r#ref);
    let before = landings(&r);
    // elicited answers wait for M3 (F5)
    let elicited = RelayInput {
        kind: Some(EvidenceKind::Elicited),
        ..words("Yes, land it.")
    };
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(&it.r#ref, "approve", Some(&it.card_hash), elicited),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    // a relayed answer names the card the person was shown
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(&it.r#ref, "approve", None, words("Yes, land it.")),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(landings(&r), before, "a refusal lands nothing");
    let info = r
        .answer(
            &agent(),
            PERSON,
            req(
                &it.r#ref,
                "approve",
                Some(&it.card_hash),
                words("Yes, land it."),
            ),
        )
        .unwrap();
    assert_eq!(
        (info.r#ref.as_str(), info.act.as_str(), info.option.as_str()),
        (it.r#ref.as_str(), "approve", "approve")
    );
    assert_eq!(info.label, it.options[0].label);
    assert_eq!(info.assurance, "relayed");
    assert!(info.answer.is_none(), "an approval writes no ANSWER");
    let landing = info.landing.as_ref().expect("a landing");
    assert_eq!(
        (landing.approved.as_str(), landing.assurance.as_str()),
        (it.r#ref.as_str(), "relayed")
    );
    let rows = r.read().log_rows().unwrap();
    let row = &rows[0];
    assert_eq!(row.approved_by, PERSON);
    assert_eq!(row.assurance, "relayed");
    let by = row.relayed_by.as_ref().expect("relayed_by");
    assert_eq!((by.actor.as_str(), by.run.as_deref()), (ME, Some("r1")));
    let quoted = row.relay_evidence.as_ref().expect("relay evidence");
    assert_eq!(
        (quoted.quoted.as_str(), quoted.session_ref.as_str()),
        ("Yes, land it.", "chat-7")
    );
    let ev = row.evidence.as_ref().expect("evidence");
    assert_eq!(ev.kind, "card");
    assert_eq!(ev.options.as_ref(), Some(&labels(&it)));
    assert_eq!(ev.choice.as_deref(), Some(it.options[0].label.as_str()));
    assert_eq!(ev.card_hash.as_deref(), Some(it.card_hash.as_str()));
    assert!(answers(&r).is_empty(), "{:#?}", answers(&r));
    assert!(pending(&r).items.iter().all(|i| i.r#ref != it.r#ref));
    r.fsck().expect("fsck");
}

#[test]
fn di7_a_local_approval_at_the_cli_records_local() {
    let (_d, mut r, m) = ready_and_not_ready();
    let info = r
        .answer(
            &approver(),
            PERSON,
            req(&m.ready.r#ref, "approve", None, RelayInput::default()),
        )
        .unwrap();
    assert_eq!((info.assurance, info.relayed_by.as_ref()), ("local", None));
    assert_eq!(info.landing.as_ref().unwrap().assurance, "local");
    let rows = r.read().log_rows().unwrap();
    let row = &rows[0];
    assert_eq!(
        (row.approved_by.as_str(), row.assurance.as_str()),
        (PERSON, "local")
    );
    assert!(row.relayed_by.is_none() && row.relay_evidence.is_none() && row.evidence.is_none());
    r.fsck().expect("fsck");
}

#[test]
fn di7_a_moved_card_refuses_and_returns_the_fresh_card() {
    // ruling task-12 R3 (p-ST0E9, D-ME2HD): the card moves by a landing in the version's reach,
    // not by Base. Canon: claim y builds on claim x; the version edits x, so y is in its reach
    let (_d, mut r) = rdy_qa::fixtures::project();
    let w = rdy_qa::tensions::put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$t", "body": "Y builds on {{$x}}."}
        ]),
        rdy_qa::tensions::j_typo(),
    );
    land(&mut r, "seed").expect("land seed");
    let (x, y) = (w.created["$x"].clone(), w.created["$y"].clone());
    let edit = |r: &mut Rdy, view: &str, id: &str, body: &str| {
        rdy_qa::tensions::put_j(
            r,
            view,
            rdy_qa::tensions::upd(&id.parse().unwrap(), body),
            rdy_qa::tensions::j_typo(),
        );
    };
    edit(&mut r, "a", &x, "X holds twice.");
    let p = r
        .propose(&rdy_qa::fixtures::writer(), "a", "a", Some(TEST_SUMMARY))
        .expect("propose a");
    assert!(p.ready, "{:#?}", p.issues);
    // an unrelated landing first, so the card's Base line reads "moved" before it is shown
    put(
        &mut r,
        "other",
        json!([{"op": "create", "type": "doc", "id": "$d",
                "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}]),
    );
    land(&mut r, "other").expect("land other");
    let shown = item(&pending(&r), &p.r#ref);
    // a landing in its reach: the card the queue shows is the same, and the approval is refused
    // at landing by freshness, with the fresh card
    edit(&mut r, "l", &y, &format!("Y builds on {{{{{x}}}}} twice."));
    let l: Hash = land(&mut r, "l").expect("land l").parse().unwrap();
    assert_eq!(item(&pending(&r), &p.r#ref).card_hash, shown.card_hash);
    let before = landings(&r);
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(
                &shown.r#ref,
                "approve",
                Some(&shown.card_hash),
                words("Yes."),
            ),
        )
        .unwrap_err();
    refused(
        &e,
        E_PROPOSAL_MOVED,
        &format!(
            "proposal {} moved: landing {} changed {y} after its assessment",
            p.r#ref,
            l.short()
        ),
    );
    assert_eq!(
        e.fix,
        format!(
            "review the fresh card in corrected and approve {} again",
            p.r#ref
        )
    );
    let fresh = item(&pending(&r), &p.r#ref);
    assert_ne!(fresh.card_hash, shown.card_hash);
    assert_eq!(e.corrected, Some(json!({"card": card_json(&fresh)})));
    assert_eq!(landings(&r), before, "nothing landed");
    // the fresh card's hash binds
    let info = r
        .answer(
            &agent(),
            PERSON,
            req(
                &fresh.r#ref,
                "approve",
                Some(&fresh.card_hash),
                words("Yes."),
            ),
        )
        .unwrap();
    assert!(info.landing.is_some());
    // an item that is gone: it landed
    let e = r
        .answer(
            &approver(),
            PERSON,
            req(&fresh.r#ref, "approve", None, RelayInput::default()),
        )
        .unwrap_err();
    refused(
        &e,
        E_ITEM_MOVED,
        &format!("{} changed since it was shown", fresh.r#ref),
    );

    // another item: a card that is not the one the queue shows is E_ITEM_MOVED, with the fresh card
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let ex = exceptions_of(&q, &b.r#ref);
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(
                &ex[0].r#ref,
                "confirm",
                Some(&ex[1].card_hash),
                words("yes"),
            ),
        )
        .unwrap_err();
    refused(
        &e,
        E_ITEM_MOVED,
        &format!("{} changed since it was shown", ex[0].r#ref),
    );
    assert_eq!(e.corrected, Some(card_json(&ex[0])));
    assert!(answers(&r).is_empty());
    // an answered exception is gone from the queue
    relay_answer(&mut r, &ex[0].r#ref, "confirm").unwrap();
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(
                &ex[0].r#ref,
                "confirm",
                Some(&ex[0].card_hash),
                words("yes"),
            ),
        )
        .unwrap_err();
    refused(
        &e,
        E_ITEM_MOVED,
        &format!("{} changed since it was shown", ex[0].r#ref),
    );
}

#[test]
fn di7_a_new_version_refuses_with_e_proposal_moved() {
    let (_d, mut r, m) = ready_and_not_ready();
    let old = item(&pending(&r), &m.ready.r#ref);
    put(
        &mut r,
        "ready",
        json!([{"op": "create", "type": "claim", "parent": m.spec.section.to_string(),
                "body": "Another claim of ready."}]),
    );
    let p2 = r
        .propose_subset(
            &ag("ready"),
            "ready",
            "ready",
            None,
            Some("Adds two claims."),
        )
        .expect("propose again");
    assert_ne!(p2.r#ref, old.r#ref);
    let fresh = item(&pending(&r), &p2.r#ref);
    let before = landings(&r);
    for (s, relay, h) in [
        (agent(), words("Yes."), Some(old.card_hash.as_str())),
        (approver(), RelayInput::default(), None),
    ] {
        let e = r
            .answer(&s, PERSON, req(&old.r#ref, "approve", h, relay))
            .unwrap_err();
        refused(
            &e,
            E_PROPOSAL_MOVED,
            &format!(
                "proposal {} moved: the latest version is {}",
                old.r#ref, p2.r#ref
            ),
        );
        assert_eq!(e.corrected, Some(card_json(&fresh)));
    }
    assert_eq!(landings(&r), before, "nothing landed");
    assert!(answers(&r).is_empty());
}

#[test]
fn di7_an_approval_with_an_open_exception_is_refused() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let batch = item(&q, &b.r#ref);
    let ex = exceptions_of(&q, &b.r#ref);
    assert_eq!(ex.len(), 4, "{ex:#?}");
    let refs: Vec<&str> = ex.iter().map(|i| i.r#ref.as_str()).collect();
    let head = r.read().canon_head();
    for (s, relay, h) in [
        (
            agent(),
            words("Yes, land it."),
            Some(batch.card_hash.as_str()),
        ),
        (approver(), RelayInput::default(), None),
    ] {
        let e = r
            .answer(&s, PERSON, req(&b.r#ref, "approve", h, relay))
            .unwrap_err();
        refused(
            &e,
            E_EXCEPTION_OPEN,
            &format!("4 exceptions of {} have no answer", b.r#ref),
        );
        assert_eq!(
            e.fix,
            format!("answer each exception card first: {}", refs.join(", "))
        );
        let cards: Vec<Value> = ex.iter().map(card_json).collect();
        assert_eq!(e.corrected, Some(json!({"exceptions": cards})));
    }
    assert_eq!(r.read().canon_head(), head, "nothing landed");
    assert!(answers(&r).is_empty());
}

#[test]
fn di7_a_confirmed_exception_lets_the_batch_land() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let ex = exceptions_of(&q, &b.r#ref);
    assert_eq!(ex.len(), 4);
    for (n, i) in ex.iter().enumerate() {
        let info = relay_answer(&mut r, &i.r#ref, "confirm").unwrap();
        assert_eq!(
            (info.act.as_str(), info.label.as_str()),
            ("confirm", "Confirm")
        );
        assert!(info.landing.is_none());
        let a = answer_of(&r, &i.r#ref);
        assert_eq!(Some(a.id().to_string()), info.answer);
        assert_eq!(
            (a.act, a.option.as_deref(), a.label.as_deref()),
            (AnswerAct::Confirm, Some("confirm"), Some("Confirm"))
        );
        assert_eq!(a.options, labels(i));
        assert_eq!(a.card_hash, Some(hash(&i.card_hash)));
        assert_eq!(a.evidence_kind, Some(EvidenceKind::Card));
        assert_eq!(a.person.actor, PERSON);
        let left = ex.len() - n - 1;
        if left > 0 {
            let e = relay_answer(&mut r, &b.r#ref, "approve").unwrap_err();
            refused(
                &e,
                E_EXCEPTION_OPEN,
                &format!("{left} exceptions of {} have no answer", b.r#ref),
            );
        }
    }
    let info = relay_answer(&mut r, &b.r#ref, "approve").expect("the batch lands");
    assert!(info.landing.is_some());
    let canon = r.read().materialized(&Scope::Canon).unwrap();
    for d in [
        &b.member,
        &b.one_way,
        &b.against,
        &b.unapplied,
        &b.not_recommended,
    ] {
        assert_eq!(
            canon.get(d).unwrap().field_str("status"),
            Some("accepted"),
            "{d}"
        );
    }
    r.fsck().expect("fsck");
}

#[test]
fn di7_a_rejected_decision_makes_the_batch_not_ready() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    let one_way = exceptions_of(&q, &b.r#ref)
        .into_iter()
        .find(|i| i.r#ref.starts_with(&format!("{}@", b.one_way)))
        .expect("the one_way exception");
    let head = r.read().canon_head();
    let info = relay_answer(&mut r, &one_way.r#ref, "reject").unwrap();
    assert_eq!(info.act, "reject_decision");
    assert_eq!(answer_of(&r, &one_way.r#ref).act, AnswerAct::RejectDecision);
    let q = pending(&r);
    assert!(
        q.items
            .iter()
            .all(|i| i.r#ref != b.r#ref && i.batch.as_deref() != Some(b.r#ref.as_str())),
        "{q:#?}"
    );
    let issue = format!(
        "{} was rejected; drop or revise it, then propose again",
        b.one_way
    );
    let not_ready = q
        .not_ready
        .iter()
        .find(|n| n.r#ref == b.r#ref)
        .expect("the batch is not ready");
    assert!(
        not_ready.issues.iter().any(|i| i.message == issue),
        "{not_ready:#?}"
    );
    // the version cannot be approved until its author drops or revises the decision: the
    // approval is refused with the queue's readiness issues and no card (D-ME2HD)
    let e = r
        .answer(
            &approver(),
            PERSON,
            req(&b.r#ref, "approve", None, RelayInput::default()),
        )
        .unwrap_err();
    refused(
        &e,
        E_PROPOSAL_NOT_READY,
        &format!(
            "{} is not ready: {} issue{}",
            b.r#ref,
            not_ready.issues.len(),
            if not_ready.issues.len() == 1 { "" } else { "s" }
        ),
    );
    assert_eq!(e.issues, not_ready.issues, "{e:#?}");
    assert!(e.corrected.is_none(), "{e:#?}");
    assert_eq!(r.read().canon_head(), head, "nothing reaches canon");
}

#[test]
fn di7_reject_keeps_canon_and_drops_the_item() {
    let (_d, mut r, m) = ready_and_not_ready();
    let it = item(&pending(&r), &m.ready.r#ref);
    let head = r.read().canon_head();
    let info = r
        .answer(
            &approver(),
            PERSON,
            req(&it.r#ref, "reject", None, RelayInput::default()),
        )
        .unwrap();
    assert_eq!(
        (info.act.as_str(), info.label.as_str(), info.assurance),
        ("reject_proposal", "Reject", "local")
    );
    assert!(info.landing.is_none());
    let a = answer_of(&r, &it.r#ref);
    assert_eq!(Some(a.id().to_string()), info.answer);
    assert_eq!(
        (a.act, a.option.as_deref(), a.label.as_deref()),
        (AnswerAct::RejectProposal, Some("reject"), Some("Reject"))
    );
    assert_eq!(a.options, labels(&it));
    assert_eq!(
        (a.person.actor.as_str(), a.person.role),
        (PERSON, Role::Approver)
    );
    assert!(
        a.card_hash.is_none(),
        "a local answer with no card binds to the version"
    );
    assert_eq!(r.read().canon_head(), head, "canon does not change");
    assert!(pending(&r).items.iter().all(|i| i.r#ref != it.r#ref));
    // the version stays open in its view; only the queue drops it
    assert!(
        r.read()
            .open_proposals()
            .unwrap()
            .iter()
            .any(|(p, _)| p.to_string() == it.r#ref)
    );
    let e = r
        .answer(
            &approver(),
            PERSON,
            req(&it.r#ref, "reject", None, RelayInput::default()),
        )
        .unwrap_err();
    assert_eq!(e.code, E_ITEM_MOVED, "{e}");
    r.fsck().expect("fsck");
}

#[test]
fn di7_later_needs_a_relay_and_writes_a_snooze() {
    let (_d, mut r, m) = ready_and_not_ready();
    let it = item(&pending(&r), &m.ready.r#ref);
    let e = r
        .answer(
            &approver(),
            PERSON,
            req(&it.r#ref, "later", None, RelayInput::default()),
        )
        .unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        "Later needs a chat session; the CLI has no Later",
    );
    assert!(answers(&r).is_empty());
    let info = r
        .answer(
            &agent(),
            PERSON,
            req(&it.r#ref, "later", Some(&it.card_hash), words("Not now.")),
        )
        .unwrap();
    assert_eq!((info.act.as_str(), info.label.as_str()), ("later", "Later"));
    let (h, a) = r
        .read()
        .reader()
        .snooze_of(PERSON, &it.r#ref)
        .unwrap()
        .expect("a snooze");
    assert_eq!(Some(h.to_string()), info.answer);
    assert_eq!(a.act, AnswerAct::Later);
    assert_eq!(a.relay_evidence.as_ref().unwrap().session_ref, "chat-7");
    assert_eq!(a.card_hash, Some(hash(&it.card_hash)));
    let names: Vec<String> = answers(&r).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec![format!("pending/{PERSON}/{}", it.r#ref)]);
    assert!(r.read().reader().answer_of(&it.r#ref).unwrap().is_none());
    // the binding reads the queue with no session: a snooze does not hide the item from an answer
    assert!(pending(&r).items.iter().any(|i| i.r#ref == it.r#ref));
    let info = relay_answer(&mut r, &it.r#ref, "approve").expect("approve after Later");
    assert!(info.landing.is_some());
    r.fsck().expect("fsck");
}

#[test]
fn di7_all_recommended_records_each_item_on_its_own() {
    let (_d, mut r, b) = batch_project();
    let q = pending(&r);
    // the not-recommended decision last: its recommended answer rejects it, and a rejected
    // decision takes its batch and the batch's other exceptions out of the queue (F1)
    let not_recommended = format!("{}@", b.not_recommended);
    let mut ex = exceptions_of(&q, &b.r#ref);
    ex.sort_by_key(|i| i.r#ref.starts_with(&not_recommended));
    assert!(ex[3].r#ref.starts_with(&not_recommended));
    let refs: Vec<String> = ex.iter().map(|i| i.r#ref.clone()).collect();
    let hashes: Vec<String> = ex.iter().map(|i| i.card_hash.clone()).collect();
    let relay = words("Do what you recommend.");
    // one card hash for each ref
    let e = r
        .answer_all_recommended(&agent(), PERSON, &refs, &hashes[..1], relay.clone())
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let done = r
        .answer_all_recommended(&agent(), PERSON, &refs[..2], &hashes[..2], relay.clone())
        .unwrap();
    assert_eq!(done.len(), 2);
    // the rest, then the batch: the not-recommended decision is rejected, so the batch is not
    // ready; the call stops there and reports what it did
    let mut rest = refs[2..].to_vec();
    rest.push(b.r#ref.clone());
    let mut rest_hashes = hashes[2..].to_vec();
    rest_hashes.push(item(&q, &b.r#ref).card_hash);
    let e = r
        .answer_all_recommended(&agent(), PERSON, &rest, &rest_hashes, relay)
        .unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_NOT_READY, "{e}");
    let corrected = e.corrected.as_ref().expect("what was done");
    let reported: Vec<&str> = corrected["done"]
        .as_array()
        .expect("done")
        .iter()
        .map(|d| d["ref"].as_str().unwrap())
        .collect();
    assert_eq!(
        reported,
        refs[2..].iter().map(String::as_str).collect::<Vec<_>>()
    );
    // each item has its own answer: its recommended option, its own card hash, the same words
    let mut ids = std::collections::BTreeSet::new();
    for (n, i) in ex.iter().enumerate() {
        let rec = i
            .options
            .iter()
            .find(|o| o.recommended.is_some())
            .expect("a recommended option");
        let a = answer_of(&r, &i.r#ref);
        assert_eq!(a.option.as_deref(), Some(rec.id.as_str()), "{}", i.r#ref);
        let act = if i.r#ref.starts_with(&not_recommended) {
            AnswerAct::RejectDecision
        } else {
            AnswerAct::Confirm
        };
        assert_eq!(a.act, act, "{}", i.r#ref);
        assert_eq!(a.card_hash, Some(hash(&hashes[n])));
        assert_eq!(
            a.relay_evidence.as_ref().unwrap().quoted,
            "Do what you recommend."
        );
        ids.insert(a.id());
        if n < 2 {
            assert_eq!(done[n].r#ref, i.r#ref);
            assert_eq!(done[n].answer, Some(a.id().to_string()));
        }
    }
    assert_eq!(ids.len(), 4, "four answers");
    assert_eq!(answers(&r).len(), 4);
}

fn ctx<'a>(e: &'a Local, s: &'a Session, cwd: &'a std::path::Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        human: PERSON,
        cwd,
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    }
}

/// A guard (O19): `relay_approver` goes through the relay table and keeps the daemon's own codes
/// and texts.
#[test]
fn di7_the_daemon_relay_texts_do_not_change() {
    let dir = tempfile::tempdir().unwrap();
    let e = Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let human = Session::human(PERSON.into(), Role::Writer).unwrap();
    let a = agent();
    let cwd = dir.path().to_path_buf();
    dispatch(&ctx(&e, &human, &cwd), "new_doc", json!({"path": "a"})).unwrap();
    let p = dispatch(
        &ctx(&e, &human, &cwd),
        "propose",
        json!({"title": "t", "summary": "test summary"}),
    )
    .unwrap();
    let id = p["id"].as_str().unwrap().to_string();
    let approve = |s: &Session, extra: Value| {
        let mut args = json!({"proposal": id});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        dispatch(&ctx(&e, s, &cwd), "approve", args)
    };
    let check = |r: Result<Value, RdyError>, code: &str, message: &str, fix: &str| {
        let err = r.unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str(), err.fix.as_str()),
            (code, message, fix)
        );
    };
    let not_relaying = "an agent approves only by relaying a person's approval";
    let not_relaying_fix = "ask the person, then pass --relayed-by <your actor> --evidence \"<their words>\" --question \"<what you asked>\"";
    let pair = "relayed_by and evidence go together";
    let pair_fix = "pass both (--relayed-by and --evidence) or neither";
    check(
        approve(&a, json!({})),
        E_PERMISSION,
        not_relaying,
        not_relaying_fix,
    );
    check(
        approve(&a, json!({"evidence": "yes"})),
        E_PERMISSION,
        not_relaying,
        not_relaying_fix,
    );
    check(
        approve(&a, json!({"relayed_by": ME})),
        E_BAD_INPUT,
        pair,
        pair_fix,
    );
    check(
        approve(&a, json!({"relayed_by": "agent:other", "evidence": "yes"})),
        E_PERMISSION,
        "agent:relay (run r1) relays only as itself, not as agent:other",
        "pass --relayed-by agent:relay with the person's words in --evidence",
    );
    check(
        approve(&a, json!({"relayed_by": ME, "evidence": "  "})),
        E_BAD_INPUT,
        "the evidence quote is empty",
        "quote the human's words with --evidence",
    );
    check(
        approve(&human, json!({"relayed_by": ME, "evidence": "yes"})),
        E_PERMISSION,
        "a person approves as themselves; only the relaying agent records a relay",
        "drop --relayed-by to approve yourself, or let the agent run rdy approve with RDY_ACTOR set to its actor",
    );
    check(
        approve(&human, json!({"evidence": "yes"})),
        E_BAD_INPUT,
        pair,
        pair_fix,
    );
    check(
        approve(&human, json!({"question": "Land it?"})),
        E_BAD_INPUT,
        "question needs relayed_by",
        "pass relayed_by and evidence with it, or drop question",
    );
    check(
        dispatch(&ctx(&e, &a, &cwd), "tag", json!({"version": "v0.1.0"})),
        E_PERMISSION,
        "agent sessions cannot tag or import",
        "tags and imports are the person's acts: ask the person to run the command",
    );
    let v = approve(
        &a,
        json!({"relayed_by": ME, "evidence": "yes", "session_ref": "chat-7"}),
    )
    .unwrap();
    assert_eq!(v["assurance"], "relayed", "{v}");
    assert_eq!(v["relayed_by"], json!({"actor": ME, "run": "r1"}), "{v}");
}

/// Fix round 1: the same decision version proposed in two views stands beside each batch as its
/// own card, with the same ref and different hashes (each card names its batch). A relayed
/// answer binds to the card whose ref and hash it names, not to the first card with the ref.
#[test]
fn di7_a_decision_in_two_batches_binds_to_the_card_shown() {
    let (dir, mut r, b) = batch_project();
    r.view_fork(&ag("batch2"), rdy_qa::intake::BATCH_VIEW, "batch2")
        .expect("fork the batch view");
    // O23: the fork's plan answers its own items before its propose
    rdy_qa::intake::answer_open_items(&mut r, &rdy_qa::fixtures::writer(), "batch2");
    let (mut r, report) = rdy_qa::intake::propose_reported(
        dir.path(),
        r,
        "batch2",
        "Guest mode again",
        Some(rdy_qa::intake::BATCH_SUMMARY),
    );
    let second = report["ref"].as_str().expect("ref").to_string();
    assert_ne!(second, b.r#ref);
    let q = pending(&r);
    let firsts = exceptions_of(&q, &b.r#ref);
    let seconds = exceptions_of(&q, &second);
    let shared = seconds
        .iter()
        .find(|s| firsts.iter().any(|f| f.r#ref == s.r#ref))
        .unwrap_or_else(|| panic!("a shared exception in {firsts:#?} and {seconds:#?}"))
        .r#ref
        .clone();
    // the queue orders the two batches by when they were raised, then by ref: take whichever
    // card of the ref comes second, the one a lookup by ref alone never reaches
    let cards: Vec<&Item> = q.items.iter().filter(|i| i.r#ref == shared).collect();
    assert_eq!(cards.len(), 2, "one card beside each batch: {cards:#?}");
    let (other, shown) = (cards[0].clone(), cards[1].clone());
    assert_ne!(other.batch, shown.batch, "two batches");
    assert_ne!(
        other.card_hash, shown.card_hash,
        "each card names its batch"
    );
    let info = r
        .answer(
            &agent(),
            PERSON,
            req(
                &shown.r#ref,
                "confirm",
                Some(&shown.card_hash),
                words("Yes."),
            ),
        )
        .expect("the card shown binds");
    assert_eq!(
        (info.r#ref.as_str(), info.act.as_str()),
        (shown.r#ref.as_str(), "confirm")
    );
    let a = answer_of(&r, &shown.r#ref);
    assert_eq!(a.card_hash, Some(hash(&shown.card_hash)));
}

/// Fix round 1 (O3): an item ref outside the closed grammar is bad input, never a moved item; a
/// well-formed ref whose item is gone stays `E_ITEM_MOVED`.
#[test]
fn di7_a_malformed_item_ref_is_bad_input() {
    let (_d, mut r, _m) = ready_and_not_ready();
    for bad in [
        "garbage",
        "",
        "P-abc",
        "P-0123abcd",
        "P-0123abcd@",
        "P-0123abcd@x",
        "D-ABCDE",
        "D-ABCDE@",
        "D-ABCDE@0123456789a",
        "D-ABCDE@0123456789AB",
        "D-ABC@0123456789ab",
        "advice:garbage",
        "tag:v0.1.0",
        "own:work",
        "tn-xyz",
    ] {
        let e = r
            .answer(
                &approver(),
                PERSON,
                req(bad, "approve", None, RelayInput::default()),
            )
            .unwrap_err();
        refused(&e, E_BAD_INPUT, &format!("{bad} is not an item ref"));
        assert_eq!(
            e.fix,
            "pass an item ref as rdy_pending gives it: P-<id>@<n>, <D-id>@<version>, <Q-id>@<version>, advice:P-<id>@<n> or a tension id",
            "{bad}"
        );
    }
    for gone in [
        "P-0123abcd@1",
        "D-ABCDE@0123456789ab",
        "Q-ABCDE@0123456789ab",
        "advice:P-0123abcd@1",
        "tn-0123456789ab",
    ] {
        let e = r
            .answer(
                &approver(),
                PERSON,
                req(gone, "approve", None, RelayInput::default()),
            )
            .unwrap_err();
        assert_ne!(e.code, E_BAD_INPUT, "{gone}: {e}");
    }
    assert!(answers(&r).is_empty());
}

/// Fix round 1: the daemon's approval runs the whole relay through the shared table: the
/// question and the session_ref are held to its limits too, and its own texts keep their order.
#[test]
fn di7_the_daemon_approval_checks_the_whole_relay() {
    let dir = tempfile::tempdir().unwrap();
    let e = Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let human = Session::human(PERSON.into(), Role::Writer).unwrap();
    let a = agent();
    let cwd = dir.path().to_path_buf();
    dispatch(&ctx(&e, &human, &cwd), "new_doc", json!({"path": "a"})).unwrap();
    let p = dispatch(
        &ctx(&e, &human, &cwd),
        "propose",
        json!({"title": "t", "summary": "test summary"}),
    )
    .unwrap();
    let id = p["id"].as_str().unwrap().to_string();
    let approve = |s: &Session, extra: Value| {
        let mut args = json!({"proposal": id});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        dispatch(&ctx(&e, s, &cwd), "approve", args)
    };
    let too_long = RelayRefusal::TooLong.error();
    let err = approve(
        &a,
        json!({"relayed_by": ME, "evidence": "Yes.", "question": "q".repeat(2001)}),
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            too_long.code,
            too_long.message.as_str(),
            too_long.fix.as_str()
        )
    );
    let long_ref = RelayRefusal::SessionRefTooLong.error();
    let err = approve(
        &a,
        json!({"relayed_by": ME, "evidence": "Yes.", "session_ref": "s".repeat(500)}),
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (long_ref.code, long_ref.message.as_str())
    );
    // a person's session_ref is named before their question, as before the table
    let err = approve(
        &human,
        json!({"question": "Land it?", "session_ref": "chat-7"}),
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_BAD_INPUT,
            "session_ref needs relayed_by",
            "pass relayed_by and evidence with it, or drop session_ref"
        )
    );
    // nothing landed: the version is still there to approve
    let v = approve(
        &a,
        json!({"relayed_by": ME, "evidence": "Yes.", "question": "q".repeat(2000)}),
    )
    .unwrap();
    assert_eq!(v["assurance"], "relayed", "{v}");
}

/// D-K0SS1: a relayed approval of a version that a close of its view withdrew is refused with
/// `E_VIEW_CLOSED`, bound to the card the person was shown; nothing lands.
#[test]
fn vw2_a_relayed_approval_of_a_withdrawn_version_is_e_view_closed() {
    let (_d, mut r, m) = ready_and_not_ready();
    let shown = item(&pending(&r), &m.ready.r#ref);
    let before = landings(&r);
    r.view_close(&ag("ready"), &["ready".into()], "done")
        .unwrap();
    let e = r
        .answer(
            &agent(),
            PERSON,
            req(
                &shown.r#ref,
                "approve",
                Some(&shown.card_hash),
                words("Yes."),
            ),
        )
        .unwrap_err();
    refused(
        &e,
        E_VIEW_CLOSED,
        &format!("{} was withdrawn when view \"ready\" closed", shown.r#ref),
    );
    assert_eq!(landings(&r), before);
}
