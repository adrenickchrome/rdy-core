//! di12 (D-KQ4CA, D-JV6Z8, D-ST423, F4, F5): the chat path to the "needs you" queue. `rdy_pending`
//! lists the cards; `rdy_answer` relays the person's answer, bound to the card they were shown.
//! The tools run in this process on a `Local` engine; the family test and the missing-words test
//! go through the stdio server.
use rdy_engine::engine::Local;
use rdy_engine::pending::{CARD_DEFAULT_TOKENS, PendingQuery};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::tools::{McpState, rdy_answer, rdy_pending, rdy_session};
use rdy_model::{AnswerAct, EvidenceKind};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::land;
use rdy_qa::intake::*;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_qa::tensions::{ag, put, upd};
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
    // as an approval reports: the consequences of the landed version, always a list
    assert!(info["landing"]["consequences"].is_array(), "{info}");
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

#[test]
fn sdc3_pending_describes_the_recommended_flag_and_reason() {
    // D-D8FAB: pins the tool text. The recommended option carries recommended: true and a
    // reason; a question block's own recommended field is the exact text of one option.
    let (d, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(d.path());
    let tools = c.tools();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "rdy_pending")
        .expect("rdy_pending is listed");
    assert_eq!(
        tool["description"].as_str().unwrap(),
        "Read what waits for the person: {items, not_ready, snoozed, omitted}. Each item has a ref, kind (approve, confirm, answer, resolve, defer, reaffirm), options, card and card_hash. Each option has an id, a label and its consequences. The recommended option carries recommended: true and a reason of at most 200 characters; no other option carries either. A question block's own recommended field is different: it is the exact text of one option.\n\nShow the card to the person word for word. Keep its options, recommendation and consequences unchanged. A batch (approve) stands first, its exceptions (confirm) beside it. Pass session_ref (the chat session) so Later hides a card only there; max_tokens cuts each card. A defer card and a reaffirm card need the person's own choice and carry no recommended option; show both options and wait for the person. not_ready lists the versions that need work from you first."
    );
}

/// A reason of 300 characters: longer than a card reason may be (D-D8FAB).
fn long_reason() -> String {
    "Finance owns the ledger. ".repeat(12)
}

/// The batch project (an approve card, its confirm cards and the advice resolve card), then an
/// open question in canon that recommends "Finance" with [`long_reason`]: the project, with its
/// person, an agent state on it, and the question's id.
fn every_kind() -> (tempfile::TempDir, McpState, Batch, String) {
    let (d, mut r, b) = batch_project();
    let w = put(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": b.spec.section.to_string(),
        "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead"],
            "recommended": "Finance",
            "recommend_reason": long_reason(),
        }}]),
    );
    let q = rdy_qa::tensions::created(&w, "$q").to_string();
    land(&mut r, "qs").expect("land the question");
    person_is_qa(d.path());
    (d, agent_state(r), b, q)
}

/// The items of kind `kind` in a `rdy_pending` reply.
fn of_kind(v: &Value, kind: &str) -> Vec<Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .cloned()
        .collect()
}

/// D-D8FAB: the options of item `i` that carry `recommended: true`, at most one. Each carries a
/// `reason` of at most 200 characters; every other option carries neither field.
fn recommended_of(i: &Value) -> Vec<Value> {
    let options = i["options"].as_array().unwrap();
    for o in options {
        match o.get("recommended") {
            Some(flag) => {
                assert_eq!(flag, &json!(true), "{o}");
                let reason = o["reason"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a reason: {o}"));
                assert!(reason.chars().count() <= 200, "{o}");
            }
            None => assert!(o.get("reason").is_none(), "{o}"),
        }
    }
    let marked: Vec<Value> = options
        .iter()
        .filter(|o| o.get("recommended").is_some())
        .cloned()
        .collect();
    assert!(marked.len() <= 1, "{i}");
    marked
}

/// The ids of the recommended options of each item of `kind`, in queue order.
fn recommended_ids(v: &Value, kind: &str) -> Vec<Vec<String>> {
    let items = of_kind(v, kind);
    assert!(!items.is_empty(), "a {kind} item in {v}");
    items
        .iter()
        .map(|i| {
            recommended_of(i)
                .iter()
                .map(|o| o["id"].as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

#[test]
fn sdc3_an_approve_card_marks_one_option_with_a_reason() {
    let (_d, mut st, _b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    assert_eq!(recommended_ids(&v, "approve"), vec![vec!["approve"]], "{v}");
}

#[test]
fn sdc3_a_confirm_card_marks_one_option_with_a_reason() {
    let (_d, mut st, b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let ids = recommended_ids(&v, "confirm");
    assert_eq!(ids.len(), 4, "{v}");
    for (i, rec) in of_kind(&v, "confirm").iter().zip(&ids) {
        let want = if i["ref"]
            .as_str()
            .unwrap()
            .starts_with(&b.not_recommended.to_string())
        {
            "reject"
        } else {
            "confirm"
        };
        assert_eq!(rec, &vec![want.to_string()], "{i}");
    }
    let rejected = of_kind(&v, "confirm")
        .into_iter()
        .find(|i| i["exception"] == "not_recommended")
        .unwrap();
    assert_eq!(
        recommended_of(&rejected)[0]["reason"],
        "guests should not edit"
    );
}

#[test]
fn sdc3_an_answer_card_marks_one_option_with_a_reason() {
    let (_d, mut st, _b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    assert_eq!(recommended_ids(&v, "answer"), vec![vec!["o1"]], "{v}");
    // a reason longer than 200 characters is cut to 200, ending with an ellipsis
    let answer = &of_kind(&v, "answer")[0];
    let reason = recommended_of(answer)[0]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(reason.chars().count(), 200, "{reason}");
    assert!(reason.ends_with('…'), "{reason}");
    assert!(long_reason().starts_with(reason.trim_end_matches('…')));
}

#[test]
fn sdc3_a_resolve_card_marks_one_option_with_a_reason() {
    let (_d, mut st, b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let advice = item_of(&v, &format!("advice:{}", b.r#ref));
    let rec = recommended_of(&advice);
    assert_eq!(rec.len(), 1, "{advice}");
    assert_eq!(
        (&rec[0]["id"], &rec[0]["reason"]),
        (&json!("accept"), &json!("the reviewer advised it"))
    );
    // every resolve item keeps the shape; a blocking tension's card recommends nothing
    recommended_ids(&v, "resolve");
}

#[test]
fn sdc3_a_question_block_keeps_recommended_as_option_text() {
    let (_d, mut st, _b, q) = every_kind();
    // the stored field is the exact text of one option, not a flag
    let block = st
        .engine
        .read()
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .get(&q.parse().unwrap())
        .cloned()
        .expect("the question in canon");
    assert_eq!(block.field_str("recommended"), Some("Finance"));
    // the card's option with that label carries the flag
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let answer = &of_kind(&v, "answer")[0];
    let rec = recommended_of(answer);
    assert_eq!(
        (&rec[0]["label"], &rec[0]["recommended"]),
        (&json!("Finance"), &json!(true))
    );
}

#[test]
fn sdc3_the_approve_reason_is_computed_never_the_summary() {
    // decisions stand beside this batch: it lands after each has a confirm answer
    let (_d, mut st, b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let batch = item_of(&v, &b.r#ref);
    assert_eq!(
        recommended_of(&batch)[0]["reason"],
        "its checks pass, and it lands after you confirm each decision beside it"
    );
    assert_ne!(recommended_of(&batch)[0]["reason"], BATCH_SUMMARY);
    // nothing stands beside this one
    let (d, r, made) = ready_and_not_ready();
    person_is_qa(d.path());
    let mut st = agent_state(r);
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let ready = item_of(&v, &made.ready.r#ref);
    let rec = recommended_of(&ready);
    assert_eq!(
        (&rec[0]["id"], &rec[0]["reason"]),
        (
            &json!("approve"),
            &json!("its checks pass and nothing blocks it")
        )
    );
}

#[test]
fn sdc3_the_card_prints_recommended_and_the_reason() {
    let (_d, mut st, b, _q) = every_kind();
    let v = pending_json(&mut st, json!({"max_tokens": 20000}));
    let card = |i: &Value| i["card"].as_str().unwrap().to_string();
    // the approve card: the word beside the option, the computed reason under it
    let batch = card(&item_of(&v, &b.r#ref));
    let label = format!("Approve {} and confirm {}", b.r#ref, b.member);
    let pad = " ".repeat(47usize.saturating_sub(label.chars().count()).max(2));
    let want = format!(
        "  1. {label}{pad}recommended\n     Lands the proposal in canon.\n     Why recommended: its checks pass, and it lands after you\n     confirm each decision beside it\n  2. Reject\n"
    );
    assert!(batch.contains(&want), "{batch}");
    // a confirm card that recommends reject
    let rejected = of_kind(&v, "confirm")
        .into_iter()
        .find(|i| i["exception"] == "not_recommended")
        .unwrap();
    let want = format!(
        "  2. Reject{}recommended\n     The batch waits until its author drops or revises this\n     decision.\n     Why recommended: guests should not edit\n  3. Later",
        " ".repeat(41)
    );
    assert!(card(&rejected).contains(&want), "{}", card(&rejected));
    // the question card: the cut reason, wrapped under the option
    let answer = card(&of_kind(&v, "answer")[0]);
    assert!(
        answer.contains(&format!("  1. Finance{}recommended\n", " ".repeat(40))),
        "{answer}"
    );
    assert!(
        answer.contains("     Why recommended: Finance owns the ledger. Finance owns the"),
        "{answer}"
    );
    // the other option has no mark
    assert!(answer.contains("  2. Support lead\n"), "{answer}");
}

/// The spec doc in canon, then `n` open questions with authored options landed under its
/// section: the project, with its person, and an agent state on it.
fn with_questions(n: usize) -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let ops: Vec<Value> = (0..n)
        .map(|k| {
            json!({"op": "create", "type": "question", "parent": spec.section.to_string(),
            "fields": {
                "prompt": format!("Who approves refunds of kind {k}?"),
                "title": format!("Refunds of kind {k}"),
                "options": ["Finance", "Support lead"],
            }})
        })
        .collect();
    put(&mut r, "qs", Value::Array(ops));
    land(&mut r, "qs").expect("land the questions");
    person_is_qa(dir.path());
    (dir, agent_state(r))
}

/// The refs of the items of a `rdy_pending` reply.
fn refs_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ref"].as_str().unwrap().to_string())
        .collect()
}

/// Final review fix round 1 (R2): a card that a snooze brings onto the first page of the queue
/// is answered as shown. The answer binds to the whole queue, not to its first page: with 51
/// questions, Later on the first brings the 51st onto the page of that chat session, and its
/// answer, with the card hash and the session it was shown in, is recorded.
#[test]
fn di12_a_card_a_snooze_brings_onto_the_page_is_answered_as_shown() {
    let (_d, mut st) = with_questions(51);
    let session = json!({"session_ref": "cap-chat"});
    let first = pending_json(&mut st, session.clone());
    assert_eq!(refs_of(&first).len(), 50, "{first}");
    assert_eq!(first["omitted"], 1, "{first}");
    let top = first["items"][0].clone();
    let later = answer(
        &mut st,
        json!({"ref": top["ref"], "option": "later", "card_hash": top["card_hash"],
            "evidence": "Later.", "question": "Shall we do this one later?",
            "session_ref": "cap-chat"}),
    )
    .unwrap();
    assert_eq!(later["act"], "later", "{later}");
    let page = pending_json(&mut st, session);
    assert_eq!(
        (&page["snoozed"], &page["omitted"]),
        (&json!(1), &json!(0)),
        "{page}"
    );
    let shown = refs_of(&first);
    let new = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| !shown.contains(&i["ref"].as_str().unwrap().to_string()))
        .expect("the card the snooze brought onto the page")
        .clone();
    let info = answer(
        &mut st,
        json!({"ref": new["ref"], "option": "o1", "card_hash": new["card_hash"],
            "evidence": "Finance.", "question": "Who approves these refunds?",
            "session_ref": "cap-chat"}),
    )
    .unwrap_or_else(|e| panic!("answered as shown: {e}"));
    assert_eq!(
        (&info["act"], &info["ref"]),
        (&json!("answer_question"), &new["ref"]),
        "{info}"
    );
    // the binding still checks the card: the same item with another hash is moved
    let rest = pending_json(&mut st, json!({"session_ref": "cap-chat"}));
    let last = rest["items"].as_array().unwrap().last().unwrap().clone();
    let e = answer(
        &mut st,
        json!({"ref": last["ref"], "option": "o1", "card_hash": "0".repeat(64),
            "evidence": "Finance.", "question": "Who approves these refunds?",
            "session_ref": "cap-chat"}),
    )
    .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_ITEM_MOVED, "{e}");
    assert_eq!(
        e.corrected.as_ref().unwrap()["card_hash"],
        last["card_hash"]
    );
}

/// Final review fix round 1 (R6): the session's summary counts every item of the queue and every
/// blocking item, past the cap of 50 too. 52 claims cite claim A, an edit of A lands, and the
/// claims are proposed: 52 blocking tensions, each a blocking item.
#[test]
fn di12_the_session_counts_every_blocking_item_past_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let ops: Vec<Value> = (0..52)
        .map(|k| {
            json!({"op": "create", "type": "claim", "parent": spec.section.to_string(),
                "body": format!("Claim {k} builds on {{{{{}}}}}.", spec.a)})
        })
        .collect();
    put(&mut r, "cites", Value::Array(ops));
    put(&mut r, "edit-a", upd(&spec.a, "A changed."));
    land(&mut r, "edit-a").expect("land edit-a");
    r.propose_subset(
        &ag("cites"),
        "cites",
        "cites",
        None,
        Some("Adds claims on A."),
    )
    .expect("propose");
    person_is_qa(dir.path());
    let mut st = agent_state(r);
    let q = pending_json(&mut st, json!({}));
    let shown = q["items"].as_array().unwrap();
    assert_eq!(shown.len(), 50, "{q}");
    assert!(shown.iter().all(|i| i["blocking"] == true), "{q}");
    assert_eq!(q["omitted"], 2, "{q}");
    let v = rdy_session(
        &mut st,
        p(json!({"actor": "agent:relay", "run": "r1", "role": "writer"})),
    )
    .unwrap();
    assert_eq!(v["pending"], json!({"items": 52, "blocking": 52}), "{v}");
}

/// A project whose view `work` holds a prose block with a secret, one recorded removal request
/// for it, and `work` proposed with `summary`: the project, an agent state on it, and the
/// version's ref.
fn removal_version(summary: Option<&str>) -> (tempfile::TempDir, McpState, String) {
    use rdy_engine::redaction::RedactInput;
    use rdy_engine::{Session, WriteRequest};
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let human = Session::human(PERSON.into(), rdy_model::Role::Writer).unwrap();
    let w = r
        .write(
            &human,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
                    {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "The launch code is ZEBRA-7731."}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let res = r
        .redact(
            &human,
            RedactInput {
                targets: vec![w.created["$p"].to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some(), "the request is recorded");
    let p = r
        .propose(&human, "work", "Remove the code", summary)
        .unwrap();
    person_is_qa(dir.path());
    (dir, agent_state(r), p.r#ref)
}

/// Every object key in `v`, at any depth, with the path of objects above it.
fn keys_in(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let at = format!("{path}.{k}");
                out.push(at.clone());
                keys_in(x, &at, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| keys_in(x, &format!("{path}[]"), out)),
        _ => {}
    }
}

#[test]
fn sdc3_a_not_ready_removal_card_recommends_nothing() {
    // D-D8FAB: the approve reason says its checks pass, so a removal card that proposal show
    // gives for a version that is not ready (here: no summary) has no recommended option
    let read = |st: &mut McpState, pref: &str| {
        rdy_mcp::tools::rdy_read(st, p(json!({"id": pref}))).unwrap()["proposal"].clone()
    };
    let (_d, mut st, pref) = removal_version(None);
    let show = read(&mut st, &pref);
    let card = show["removal_card"]["card"]
        .as_str()
        .unwrap_or_else(|| panic!("{show}"));
    assert!(card.contains("not ready to land"), "{card}");
    assert!(card.contains("  1. Approve "), "{card}");
    assert!(!card.lines().any(|l| l.ends_with("recommended")), "{card}");
    assert!(!card.contains("Why recommended"), "{card}");
    // no recommended flag anywhere; the only reason is the removal request's own
    let mut keys = vec![];
    keys_in(&show, "", &mut keys);
    assert!(
        !keys.iter().any(|k| k.ends_with(".recommended")),
        "{keys:?}"
    );
    let reasons: Vec<&String> = keys.iter().filter(|k| k.ends_with(".reason")).collect();
    assert_eq!(reasons, vec![".removal_card.removal[].reason"], "{show}");
    assert_eq!(
        show["removal_card"]["removal"][0]["reason"],
        "a leaked code"
    );
    // the same version, ready, recommends approve with the computed reason
    let (_d, mut st, pref) = removal_version(Some("Removes the leaked code."));
    let card = read(&mut st, &pref)["removal_card"]["card"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!card.contains("not ready to land"), "{card}");
    assert!(
        card.lines()
            .any(|l| l.starts_with("  1. Approve ") && l.ends_with("recommended")),
        "{card}"
    );
    assert!(
        card.contains("     Why recommended: its checks pass and nothing blocks it\n"),
        "{card}"
    );
}
