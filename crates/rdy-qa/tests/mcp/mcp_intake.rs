//! di12 (D-KQ4CA, D-JV6Z8, D-ST423, F4, F5): the chat path to the "needs you" queue. `rdy_pending`
//! lists the cards; `rdy_answer` relays the person's answer, bound to the card they were shown.
//! The tools run in this process on a `Local` engine; the family test and the missing-words test
//! go through the stdio server.
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_DEFAULT_TOKENS, PendingQuery};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_pending, rdy_session};
use rdy_model::{AnswerAct, EvidenceKind};
use rdy_qa::intake::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// Makes `human:qa` the project's person (`.rdy/local.yaml`).
fn person_is_qa(dir: &std::path::Path) {
    std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
}

/// An MCP state on `r` with the agent session `agent:relay`, run `r1`.
fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:relay", "run": "r1", "role": "writer"})),
    )
    .unwrap();
    st
}

/// The batch project with its person and an agent state on it.
fn batch() -> (tempfile::TempDir, McpState, Batch) {
    let (d, r, b) = batch_project();
    person_is_qa(d.path());
    (d, agent_state(r), b)
}

fn pending_json(st: &mut McpState, args: Value) -> Value {
    rdy_pending(st, p(args)).unwrap()
}

fn item_of(v: &Value, r#ref: &str) -> Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == r#ref)
        .unwrap_or_else(|| panic!("{ref} in {v}"))
        .clone()
}

/// The exception items beside batch `b`, in queue order.
fn exceptions(v: &Value, batch: &str) -> Vec<Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "confirm" && i["batch"] == batch)
        .cloned()
        .collect()
}

/// The words the agent relays: the person's quote, the question, the chat session.
fn words(extra: Value) -> Value {
    let mut v = json!({"evidence": "Yes.", "question": "Shall I record your answer?", "session_ref": "chat-7"});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn answer(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_answer(st, p(args))
}

/// Answers `item` as the card in `v` shows it, with `option`.
fn relay(st: &mut McpState, v: &Value, r#ref: &str, option: &str) -> Result<Value, RdyError> {
    let it = item_of(v, r#ref);
    answer(
        st,
        words(json!({"ref": r#ref, "option": option, "card_hash": it["card_hash"]})),
    )
}

#[test]
fn di12_rdy_pending_returns_the_batch_and_its_exceptions() {
    let (_d, mut st, b) = batch();
    // a session is required, and no role
    let e = rdy_pending(&mut McpState::new(Arc::clone(&st.engine)), p(json!({}))).unwrap_err();
    assert_eq!(e.code, "E_NO_SESSION", "{e:?}");
    let v = pending_json(&mut st, json!({}));
    assert_eq!(v["snoozed"], 0, "{v}");
    assert_eq!(v["omitted"], 0, "{v}");
    assert_eq!(v["not_ready"], json!([]), "{v}");
    let batch = item_of(&v, &b.r#ref);
    assert_eq!(batch["kind"], "approve", "{batch}");
    // every item carries its card and the card's hash
    for i in v["items"].as_array().unwrap() {
        assert!(i["card"].as_str().is_some_and(|c| !c.is_empty()), "{i}");
        assert_eq!(i["card_hash"].as_str().map(str::len), Some(64), "{i}");
    }
    assert!(
        batch["card"].as_str().unwrap().contains("Lets a visitor"),
        "the author's summary leads the card: {batch}"
    );
    // the exceptions stand beside the batch, one of each kind its decisions carry
    let mut kinds: Vec<String> = exceptions(&v, &b.r#ref)
        .iter()
        .map(|i| i["exception"].as_str().unwrap().to_string())
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        ["against", "not_recommended", "one_way", "unapplied"],
        "{v}"
    );
    // the tool is the queue as the engine computes it for the person, at the default card budget
    let direct = st
        .engine
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_DEFAULT_TOKENS,
        })
        .unwrap();
    assert_eq!(v, serde_json::to_value(direct).unwrap());
}

#[test]
fn di12_rdy_pending_cuts_to_max_tokens() {
    let (_d, mut st, b) = batch();
    let full = pending_json(&mut st, json!({"max_tokens": 20000}));
    let cut = pending_json(&mut st, json!({"max_tokens": 50}));
    let (a, c) = (item_of(&full, &b.r#ref), item_of(&cut, &b.r#ref));
    let (a_card, c_card) = (a["card"].as_str().unwrap(), c["card"].as_str().unwrap());
    assert!(c_card.len() < a_card.len(), "{c_card}");
    assert!(
        c_card.contains("cut; rdy_pending with a larger max_tokens shows them)"),
        "{c_card}"
    );
    // the hash is the card at full size, whatever budget the agent asked for
    assert_eq!(a["card_hash"], c["card_hash"]);
    let e = rdy_pending(&mut st, p(json!({"max_tokens": 100_001}))).unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e:?}");
}

#[test]
fn di12_rdy_answer_relays_a_batch_approval() {
    let (_d, mut st, b) = batch();
    let v = pending_json(&mut st, json!({}));
    // a one-word yes cannot approve while an exception waits (F2); the error carries the cards
    let e = relay(&mut st, &v, &b.r#ref, "approve").unwrap_err();
    assert_eq!(e.code, "E_EXCEPTION_OPEN", "{e:?}");
    let cards = e.corrected.as_ref().expect("the cards")["exceptions"]
        .as_array()
        .expect("exceptions")
        .clone();
    assert_eq!(cards.len(), 4, "{cards:?}");
    assert!(
        cards
            .iter()
            .all(|c| c["card"].is_string() && c["ref"].is_string())
    );
    // confirm each exception on its card; the first on the person's own words (kind text)
    let ex = exceptions(&v, &b.r#ref);
    let first = &ex[0];
    let info = answer(
        &mut st,
        words(json!({"ref": first["ref"], "option": "confirm", "card_hash": first["card_hash"], "kind": "text"})),
    )
    .unwrap();
    assert_eq!(
        (info["act"].as_str(), info["assurance"].as_str()),
        (Some("confirm"), Some("relayed")),
        "{info}"
    );
    let a = st
        .engine
        .read()
        .reader()
        .answer_of(first["ref"].as_str().unwrap())
        .unwrap()
        .expect("an answer")
        .1;
    assert_eq!(a.act, AnswerAct::Confirm);
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    for i in &ex[1..] {
        let info = relay(&mut st, &v, i["ref"].as_str().unwrap(), "confirm").unwrap();
        assert_eq!(info["act"], "confirm", "{info}");
    }
    // elicited answers wait for M3 (F5), and a kind outside the set is bad input
    for kind in ["elicited", "spoken"] {
        let it = item_of(&v, &b.r#ref);
        let e = answer(
            &mut st,
            words(json!({"ref": b.r#ref, "option": "approve", "card_hash": it["card_hash"], "kind": kind})),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{kind}: {e:?}");
    }
    // the approval lands, as the person, relayed by this session
    let info = relay(&mut st, &v, &b.r#ref, "approve").unwrap();
    assert_eq!(info["act"], "approve", "{info}");
    assert_eq!(info["assurance"], "relayed", "{info}");
    assert_eq!(info["landing"]["approved"], b.r#ref.as_str(), "{info}");
    let rows = st.engine.read().log_rows().unwrap();
    let row = rows.first().expect("a landing");
    assert_eq!(row.approved_by, PERSON);
    assert_eq!(row.assurance, "relayed");
    let by = row.relayed_by.as_ref().expect("relayed_by");
    assert_eq!(
        (by.actor.as_str(), by.run.as_deref()),
        ("agent:relay", Some("r1"))
    );
    let ev = row.evidence.as_ref().expect("evidence");
    assert_eq!(ev.kind, "card", "the default kind is card");
    let left = pending_json(&mut st, json!({}));
    assert!(
        left["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["batch"] != b.r#ref.as_str() && i["ref"] != b.r#ref.as_str())
    );
}

#[test]
fn di12_rdy_answer_needs_the_persons_words() {
    let (d, r, b) = batch_project();
    person_is_qa(d.path());
    drop(r);
    // over stdio: the tool is served, its evidence is required, and blank words are no relay
    let mut c = McpClient::start(d.path());
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:relay", "run": "r1", "role": "writer"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let v = c.call("rdy_pending", json!({})).ok();
    let it = item_of(&v, &b.r#ref);
    let bound = json!({"ref": b.r#ref, "option": "reject", "card_hash": it["card_hash"]});
    let with = |extra: Value| {
        let mut a = bound.clone();
        for (k, x) in extra.as_object().unwrap() {
            a[k] = x.clone();
        }
        a
    };
    let e = c.call("rdy_answer", bound.clone());
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    let e = c.call("rdy_answer", with(json!({"evidence": "   "})));
    assert_eq!(e.code(), Some("E_PERMISSION"), "{e:?}");
    let Reply::ToolError(v) = e else {
        unreachable!()
    };
    assert_eq!(
        v["message"], "an agent answers only by relaying a person's words",
        "{v}"
    );
    // the relay is the session: relayed_by is no parameter of rdy_answer
    let e = c.call(
        "rdy_answer",
        with(json!({"evidence": "yes", "relayed_by": "agent:relay"})),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    // a relayed answer names the card the person was shown
    let e = c.call(
        "rdy_answer",
        json!({"ref": b.r#ref, "option": "reject", "evidence": "no"}),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    // a quote past the limit is refused by the relay table
    let long = "y".repeat(2001);
    let e = c.call("rdy_answer", with(json!({"evidence": long})));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    // a refused relay answers nothing: the batch still waits
    let v = c.call("rdy_pending", json!({})).ok();
    assert_eq!(item_of(&v, &b.r#ref)["card_hash"], it["card_hash"], "{v}");
}

#[test]
fn di12_rdy_answer_refuses_a_stale_card() {
    let (_d, mut st, b) = batch();
    let v = pending_json(&mut st, json!({}));
    let ex = exceptions(&v, &b.r#ref);
    let target = &ex[0];
    let r#ref = target["ref"].as_str().unwrap();
    // a card hash the person was not shown: the item moved; the fresh card comes back
    let stale = "0".repeat(64);
    let e = answer(
        &mut st,
        words(json!({"ref": r#ref, "option": "confirm", "card_hash": stale})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_ITEM_MOVED", "{e:?}");
    assert_eq!(e.message, format!("{} changed since it was shown", r#ref));
    let fresh = e.corrected.as_ref().expect("the fresh card");
    assert_eq!(fresh["ref"], r#ref);
    assert_eq!(fresh["card_hash"], target["card_hash"]);
    assert_eq!(fresh["card"], target["card"]);
    // an approval on a stale card is E_PROPOSAL_MOVED
    let e = answer(
        &mut st,
        words(json!({"ref": b.r#ref, "option": "reject", "card_hash": stale})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PROPOSAL_MOVED", "{e:?}");
    // nothing was written; asking again on the fresh card works
    assert!(
        st.engine
            .read()
            .reader()
            .answer_of(r#ref)
            .unwrap()
            .is_none()
    );
    let ok = answer(
        &mut st,
        words(json!({"ref": r#ref, "option": "confirm", "card_hash": fresh["card_hash"]})),
    )
    .unwrap();
    assert_eq!(ok["act"], "confirm", "{ok}");
}

#[test]
fn di12_rdy_answer_all_recommended_answers_each_item() {
    let (_d, mut st, b) = batch();
    let v = pending_json(&mut st, json!({}));
    let not_recommended = format!("{}@", b.not_recommended);
    let ex: Vec<Value> = exceptions(&v, &b.r#ref)
        .into_iter()
        .filter(|i| !i["ref"].as_str().unwrap().starts_with(&not_recommended))
        .collect();
    assert_eq!(ex.len(), 3);
    let entries: Vec<Value> = ex
        .iter()
        .map(|i| json!({"ref": i["ref"], "card_hash": i["card_hash"]}))
        .collect();
    // with all_recommended, ref and option are absent
    for extra in [json!({"ref": ex[0]["ref"]}), json!({"option": "confirm"})] {
        let mut args = words(json!({"all_recommended": entries}));
        for (k, x) in extra.as_object().unwrap() {
            args[k] = x.clone();
        }
        let e = answer(&mut st, args).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    }
    let e = answer(&mut st, words(json!({"all_recommended": []}))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    // nothing above wrote an answer
    for i in &ex {
        let r#ref = i["ref"].as_str().unwrap();
        assert!(
            st.engine
                .read()
                .reader()
                .answer_of(r#ref)
                .unwrap()
                .is_none()
        );
    }
    let done = answer(&mut st, words(json!({"all_recommended": entries}))).unwrap();
    let done = done.as_array().expect("a list of answers").clone();
    assert_eq!(done.len(), 3, "{done:?}");
    for (d, i) in done.iter().zip(&ex) {
        assert_eq!(d["ref"], i["ref"], "{d}");
        assert_eq!(d["act"], "confirm", "{d}");
        assert_eq!(d["assurance"], "relayed", "{d}");
        // each item has its own answer: the recommended option and the card it was bound to
        let a = st
            .engine
            .read()
            .reader()
            .answer_of(i["ref"].as_str().unwrap())
            .unwrap()
            .expect("an answer")
            .1;
        assert_eq!(a.act, AnswerAct::Confirm);
        assert_eq!(
            a.card_hash.map(|h| h.to_string()),
            i["card_hash"].as_str().map(str::to_string)
        );
    }
}

#[test]
fn di12_later_hides_an_item_for_its_session() {
    let (_d, mut st, b) = batch();
    let v = pending_json(&mut st, json!({"session_ref": "chat-7"}));
    let target = exceptions(&v, &b.r#ref)[0].clone();
    let r#ref = target["ref"].as_str().unwrap().to_string();
    let info = answer(
        &mut st,
        words(json!({"ref": r#ref, "option": "later", "card_hash": target["card_hash"]})),
    )
    .unwrap();
    assert_eq!(info["act"], "later", "{info}");
    // hidden in the chat session that said Later, and counted
    let same = pending_json(&mut st, json!({"session_ref": "chat-7"}));
    assert!(
        same["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["ref"] != r#ref.as_str())
    );
    assert_eq!(same["snoozed"], 1, "{same}");
    // another chat session sees it again (F4), and so does a query with no session
    for args in [json!({"session_ref": "chat-8"}), json!({})] {
        let other = pending_json(&mut st, args);
        assert_eq!(other["snoozed"], 0, "{other}");
        assert_eq!(item_of(&other, &r#ref)["card_hash"], target["card_hash"]);
    }
    // Later is a snooze, not an answer: the item still waits for one
    assert!(
        st.engine
            .read()
            .reader()
            .answer_of(&r#ref)
            .unwrap()
            .is_none()
    );
}

#[test]
fn di12_every_tool_is_in_one_family() {
    // guard: a family left out of `RdyServer::new` would leave its tools unregistered, and a
    // tool in two families would be listed twice
    let (d, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(d.path());
    let names: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        names.len(),
        "a tool is listed twice: {names:?}"
    );
    for tool in ["rdy_pending", "rdy_answer"] {
        assert!(names.iter().any(|n| n == tool), "{tool} in {names:?}");
    }
}
