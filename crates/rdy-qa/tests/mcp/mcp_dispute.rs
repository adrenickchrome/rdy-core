//! ap5 (D-ZWP7Q "Disputes", D-C8RZW, D-QQCZK, D-KQ4CA, O21): a person opens, resolves and lists
//! disputes of landings, locally or relayed by an agent session (`rdy_dispute`). A relayed act
//! also writes an `ANSWER` in its batch; a local act writes none. A dispute shows and never
//! blocks.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::{Rdy, RdyError, Session};
use rdy_mcp::tools::{McpState, rdy_dispute, rdy_pending, rdy_session};
use rdy_model::dispute::DisputeOutcome;
use rdy_model::{AnswerAct, EvidenceKind, Role};
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::kinds;
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// A project whose person is `human:qa` (`.rdy/local.yaml`), with one landing; returns it.
fn landed() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    seed_doc(&mut r, "seed");
    let l = land(&mut r, "seed").unwrap();
    (d, r, l)
}

/// One more landing: a doc `name` in view `name`, landed by the person.
fn land_another(r: &mut Rdy, name: &str) -> String {
    write(
        r,
        name,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, name).unwrap()
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

/// Runs `f` on the project behind `st`, on its writer.
fn on<T: serde::Serialize + 'static>(
    st: &McpState,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<Value, RdyError> {
    write_typed(&*st.engine, f)
}

fn dispute(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_dispute(st, p(args))
}

/// The kinds of the frames of the store's last batch.
fn last_kinds(st: &McpState) -> Vec<u8> {
    let v = on(st, |r| {
        Ok(r.store()
            .last_batch()
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<u8>>())
    })
    .unwrap();
    p(v)
}

/// The answer to `item` (`None` when there is none), and its hash.
fn answer_of(st: &McpState, item: &str) -> Option<(String, rdy_model::Answer)> {
    let item = item.to_string();
    let v = on(st, move |r| {
        Ok(r.read()
            .reader()
            .answer_of(&item)
            .unwrap()
            .map(|(h, a)| json!({"hash": h.to_string(), "answer": a})))
    })
    .unwrap();
    (!v.is_null()).then(|| (p(v["hash"].clone()), p(v["answer"].clone())))
}

/// The person, local: a human session of `actor`.
fn person(actor: &str) -> Session {
    Session::human(actor.into(), Role::Approver).unwrap()
}

/// `actor` opens a dispute of `landing` locally (no relay).
fn open_local(st: &McpState, actor: &str, landing: &str, reason: &str) -> Value {
    let (s, landing, reason) = (person(actor), landing.to_string(), reason.to_string());
    on(st, move |r| {
        r.dispute_open_as(&s, "human:qa", &RelayInput::default(), &landing, &reason)
    })
    .unwrap()
}

/// `actor` resolves `dispute` locally (no relay).
fn resolve_local(
    st: &McpState,
    actor: &str,
    dispute: &str,
    outcome: DisputeOutcome,
    correction: Option<&str>,
) -> Result<Value, RdyError> {
    let (s, dispute) = (person(actor), dispute.to_string());
    let correction = correction.map(str::to_string);
    on(st, move |r| {
        r.dispute_resolve_as(
            &s,
            "human:qa",
            &RelayInput::default(),
            &dispute,
            outcome,
            correction.as_deref(),
        )
    })
}

fn ids(list: &Value) -> Vec<String> {
    list["disputes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn ap5_a_person_opens_and_resolves_a_dispute_locally() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    let v = open_local(&st, "human:qa", &landing, "It dropped the retention rule.");
    let row = &v["dispute"];
    let id = row["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 64, "{v}");
    assert_eq!(row["landing"], landing.as_str(), "{v}");
    assert_eq!(row["opened_by"], json!({"actor": "human:qa", "run": "cli"}));
    assert_eq!(row["reason"], "It dropped the retention rule.");
    assert_eq!(row["state"], "open");
    assert_eq!(row["resolution"], Value::Null);
    assert!(row["at"].as_str().unwrap().ends_with('Z'), "{v}");
    assert!(
        v.get("relayed_by").is_none() && v.get("answer").is_none(),
        "{v}"
    );
    // a local act writes no ANSWER: its own record holds the act
    let k = last_kinds(&st);
    assert!(
        k.contains(&kinds::DISPUTE) && !k.contains(&kinds::ANSWER),
        "{k:?}"
    );
    assert!(answer_of(&st, &format!("dispute:{landing}")).is_none());
    // resolved by a unique prefix of the id
    let v = resolve_local(&st, "human:qa", &id[..8], DisputeOutcome::Dismissed, None).unwrap();
    let row = &v["dispute"];
    assert_eq!(row["id"], id.as_str(), "{v}");
    assert_eq!(row["state"], "dismissed", "{v}");
    let res = &row["resolution"];
    assert_eq!(res["outcome"], "dismissed", "{v}");
    assert_eq!(res["by"], json!({"actor": "human:qa", "run": "cli"}));
    assert_eq!(res["correction"], Value::Null);
    assert_eq!(res["id"].as_str().map(str::len), Some(64), "{v}");
    let k = last_kinds(&st);
    assert!(
        k.contains(&kinds::DISPUTE_RESOLUTION) && !k.contains(&kinds::ANSWER),
        "{k:?}"
    );
    assert!(answer_of(&st, &format!("resolve:{id}")).is_none());
    // the agent reads the person's dispute
    let list = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(list["disputes"], json!([row]), "{list}");
    assert_eq!(list["omitted"], 0);
}

#[test]
fn ap5_an_agent_relays_a_dispute_with_an_answer() {
    let (_d, mut r, landing) = landed();
    let fix = land_another(&mut r, "fix");
    let mut st = agent_state(r);
    let v = dispute(
        &mut st,
        json!({"op": "open", "landing": landing, "reason": "It dropped the retention rule.",
               "evidence": "dispute that landing, it dropped the rule",
               "question": "Dispute the landing?", "session_ref": "chat-3"}),
    )
    .unwrap();
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    // the act runs as the person; the agent is the relay
    assert_eq!(
        v["dispute"]["opened_by"],
        json!({"actor": "human:qa", "run": "cli"})
    );
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"})
    );
    let answer = v["answer"].as_str().expect("the answer's hash").to_string();
    let k = last_kinds(&st);
    assert!(
        k.contains(&kinds::DISPUTE) && k.contains(&kinds::ANSWER),
        "one batch: {k:?}"
    );
    let (h, a) = answer_of(&st, &format!("dispute:{landing}")).expect("the answer");
    assert_eq!(h, answer);
    assert_eq!(a.act, AnswerAct::Dispute);
    assert_eq!(a.item, format!("dispute:{landing}"));
    assert_eq!(
        a.targets.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        [id.clone()]
    );
    assert_eq!(a.person.actor, "human:qa");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    let ev = a.relay_evidence.clone().unwrap();
    assert_eq!(
        (
            ev.quoted.as_str(),
            ev.question.as_deref(),
            ev.session_ref.as_str()
        ),
        (
            "dispute that landing, it dropped the rule",
            Some("Dispute the landing?"),
            "chat-3"
        )
    );
    // the relayed resolution: upheld, with the correcting landing
    let v = dispute(
        &mut st,
        json!({"op": "resolve", "dispute": id, "outcome": "upheld", "correction": fix,
               "evidence": "uphold it, the fix landed"}),
    )
    .unwrap();
    let res = &v["dispute"]["resolution"];
    assert_eq!(v["dispute"]["state"], "upheld", "{v}");
    assert_eq!(res["by"], json!({"actor": "human:qa", "run": "cli"}));
    assert_eq!(res["correction"], fix.as_str());
    let (h, a) = answer_of(&st, &format!("resolve:{id}")).expect("the answer");
    assert_eq!(Some(h.as_str()), v["answer"].as_str());
    assert_eq!(a.act, AnswerAct::ResolveDispute);
    assert_eq!(
        a.targets.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        [res["id"].as_str().unwrap().to_string()]
    );
    assert_eq!(a.relay_evidence.unwrap().session_ref, "mcp");
    let k = last_kinds(&st);
    assert!(
        k.contains(&kinds::DISPUTE_RESOLUTION) && k.contains(&kinds::ANSWER),
        "{k:?}"
    );
}

#[test]
fn ap5_an_agent_alone_cannot_open_a_dispute() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    let e = dispute(
        &mut st,
        json!({"op": "open", "landing": landing, "reason": "It dropped a rule."}),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_PERMISSION",
            "only a person opens a dispute",
            "relay the person's words with evidence"
        )
    );
    let list = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(list["disputes"], json!([]), "nothing is written: {list}");
    // and resolves none alone
    let v = open_local(&st, "human:qa", &landing, "It dropped a rule.");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    let e = dispute(
        &mut st,
        json!({"op": "resolve", "dispute": id, "outcome": "dismissed"}),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_PERMISSION",
            "only a person resolves a dispute",
            "relay the person's words with evidence"
        )
    );
    let list = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(list["disputes"][0]["state"], "open", "{list}");
}

#[test]
fn ap5_an_unknown_landing_is_e_not_found() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    let other = if landing.starts_with("abcdef") {
        "fedcba"
    } else {
        "abcdef"
    };
    for text in [other, "zz"] {
        let e = dispute(
            &mut st,
            json!({"op": "open", "landing": text, "reason": "r", "evidence": "yes"}),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_NOT_FOUND", "{text}: {e:?}");
    }
    let e = dispute(&mut st, json!({"op": "list", "landing": other})).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:?}");
    // an unknown dispute
    for text in ["abcdef123456", "nope"] {
        let e = dispute(
            &mut st,
            json!({"op": "resolve", "dispute": text, "outcome": "dismissed", "evidence": "yes"}),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.clone(), e.fix.as_str()),
            (
                "E_NOT_FOUND",
                format!("no dispute {text}"),
                "list disputes with rdy_dispute {op: \"list\"} / rdy dispute list"
            )
        );
    }
    // an unknown correcting landing
    let v = open_local(&st, "human:qa", &landing, "r");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    let e = resolve_local(&st, "human:qa", &id, DisputeOutcome::Upheld, Some(other)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:?}");
}

#[test]
fn ap5_an_empty_or_long_reason_is_e_bad_input() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    for reason in [String::new(), "   ".into(), "é".repeat(2001)] {
        let e = dispute(
            &mut st,
            json!({"op": "open", "landing": landing, "reason": reason, "evidence": "yes"}),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_BAD_INPUT",
                "a dispute reason is 1 to 2000 characters",
                "give the reason in 1 to 2000 characters"
            ),
            "{} characters",
            reason.chars().count()
        );
    }
    // no reason at all is an empty reason
    let e = dispute(
        &mut st,
        json!({"op": "open", "landing": landing, "evidence": "yes"}),
    )
    .unwrap_err();
    assert_eq!(e.message, "a dispute reason is 1 to 2000 characters");
    let list = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(list["disputes"], json!([]), "{list}");
    // 2,000 characters are accepted
    let reason = "é".repeat(2000);
    let v = dispute(
        &mut st,
        json!({"op": "open", "landing": landing, "reason": reason, "evidence": "yes"}),
    )
    .unwrap();
    assert_eq!(v["dispute"]["reason"], reason.as_str());
}

#[test]
fn ap5_a_second_resolution_is_e_bad_input_with_the_fix() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    let v = open_local(&st, "human:qa", &landing, "r");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    resolve_local(&st, "human:qa", &id, DisputeOutcome::Dismissed, None).unwrap();
    let short = &id[..12];
    for outcome in ["upheld", "dismissed", "withdrawn"] {
        let e = dispute(
            &mut st,
            json!({"op": "resolve", "dispute": id, "outcome": outcome, "evidence": "yes"}),
        )
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.clone(), e.fix.as_str()),
            (
                "E_BAD_INPUT",
                format!("dispute {short} is already resolved (dismissed)"),
                "open a new dispute"
            ),
            "{outcome}"
        );
    }
    let list = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(list["disputes"][0]["state"], "dismissed", "{list}");
}

#[test]
fn ap5_only_the_opener_withdraws() {
    let (_d, r, landing) = landed();
    let st = agent_state(r);
    let v = open_local(&st, "human:qa", &landing, "r");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    let e = resolve_local(&st, "human:other", &id, DisputeOutcome::Withdrawn, None).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.as_str()),
        (
            "E_PERMISSION",
            format!(
                "only the person who opened dispute {} withdraws it",
                &id[..12]
            ),
            "resolve it as upheld or dismissed"
        )
    );
    // another person dismisses or upholds; the opener withdraws
    let v2 = open_local(&st, "human:qa", &landing, "r2");
    let id2 = v2["dispute"]["id"].as_str().unwrap().to_string();
    let v = resolve_local(&st, "human:other", &id2, DisputeOutcome::Dismissed, None).unwrap();
    assert_eq!(
        v["dispute"]["resolution"]["by"],
        json!({"actor": "human:other", "run": "cli"})
    );
    let v = resolve_local(&st, "human:qa", &id, DisputeOutcome::Withdrawn, None).unwrap();
    assert_eq!(v["dispute"]["state"], "withdrawn", "{v}");
}

#[test]
fn ap5_a_correction_needs_upheld() {
    let (_d, mut r, landing) = landed();
    let fix = land_another(&mut r, "fix");
    let st = agent_state(r);
    let v = open_local(&st, "human:qa", &landing, "r");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    for outcome in [DisputeOutcome::Dismissed, DisputeOutcome::Withdrawn] {
        let e = resolve_local(&st, "human:qa", &id, outcome, Some(&fix)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_BAD_INPUT",
                "only an upheld dispute names a correction",
                "drop correction, or resolve it as upheld"
            ),
            "{outcome:?}"
        );
    }
    let v = resolve_local(
        &st,
        "human:qa",
        &id,
        DisputeOutcome::Upheld,
        Some(&fix[..10]),
    )
    .unwrap();
    assert_eq!(v["dispute"]["resolution"]["correction"], fix.as_str());
    assert_eq!(v["dispute"]["state"], "upheld");
}

#[test]
fn ap5_list_filters_by_landing_and_open() {
    let (_d, mut r, l1) = landed();
    let l2 = land_another(&mut r, "two");
    let mut st = agent_state(r);
    let d1 = open_local(&st, "human:qa", &l1, "one")["dispute"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let d2 = open_local(&st, "human:qa", &l2, "two")["dispute"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let d3 = open_local(&st, "human:qa", &l1, "three")["dispute"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    resolve_local(&st, "human:qa", &d1, DisputeOutcome::Upheld, None).unwrap();
    let all = dispute(&mut st, json!({"op": "list"})).unwrap();
    assert_eq!(
        ids(&all),
        [d1.as_str(), d2.as_str(), d3.as_str()],
        "oldest first: {all}"
    );
    assert_eq!(all["omitted"], 0);
    let on_l1 = dispute(&mut st, json!({"op": "list", "landing": l1})).unwrap();
    assert_eq!(ids(&on_l1), [d1.as_str(), d3.as_str()], "{on_l1}");
    let open = dispute(&mut st, json!({"op": "list", "open": true})).unwrap();
    assert_eq!(ids(&open), [d2.as_str(), d3.as_str()], "{open}");
    let both = dispute(
        &mut st,
        json!({"op": "list", "landing": &l1[..8], "open": true}),
    )
    .unwrap();
    assert_eq!(ids(&both), [d3.as_str()], "{both}");
    assert_eq!(both["disputes"][0]["state"], "open");
    assert_eq!(all["disputes"][0]["state"], "upheld");
}

#[test]
fn ap5_a_landing_may_hold_two_open_disputes() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    for reason in ["first", "second"] {
        let v = dispute(
            &mut st,
            json!({"op": "open", "landing": landing, "reason": reason, "evidence": "yes"}),
        )
        .unwrap();
        assert_eq!(v["dispute"]["state"], "open", "{v}");
    }
    let list = dispute(
        &mut st,
        json!({"op": "list", "landing": landing, "open": true}),
    )
    .unwrap();
    let reasons: Vec<&str> = list["disputes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["first", "second"], "{list}");
}

#[test]
fn ap5_an_open_dispute_blocks_no_landing() {
    let (_d, r, landing) = landed();
    let mut st = agent_state(r);
    let v = open_local(&st, "human:qa", &landing, "It dropped a rule.");
    let id = v["dispute"]["id"].as_str().unwrap().to_string();
    // a later version waits for the person; the queue names no dispute
    on(&st, |r| {
        write(
            r,
            "later",
            json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "page"}}]),
        )
        .map(|_| ())
    })
    .unwrap();
    on(&st, |r| {
        rdy_qa::intake::answer_open_items(r, &writer(), "later");
        let summary = rdy_qa::intake::summary_for(r, "later");
        r.propose(&writer(), "later", "later", summary).map(|_| ())
    })
    .unwrap();
    let pending = rdy_pending(&mut st, p(json!({}))).unwrap().to_string();
    assert!(pending.contains("later"), "the version waits: {pending}");
    assert!(
        !pending.contains(&id) && !pending.contains(&id[..12]) && !pending.contains("dispute"),
        "the queue shows nothing for the dispute: {pending}"
    );
    // the approval of the later version lands while the dispute is open
    let l = on(&st, |r| land(r, "later")).unwrap();
    assert_eq!(l.as_str().map(str::len), Some(64), "{l}");
    let list = dispute(&mut st, json!({"op": "list", "open": true})).unwrap();
    assert_eq!(ids(&list), [id.as_str()], "the dispute stays open: {list}");
}

#[test]
fn ap5_an_unknown_key_is_refused_with_did_you_mean() {
    let (d, r, landing) = landed();
    drop(r);
    // over stdio: the schema walk names the near key
    let mut c = McpClient::start(d.path());
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:relay", "run": "r1", "role": "writer"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let e = c.call(
        "rdy_dispute",
        json!({"op": "open", "landing": landing, "reasn": "r", "evidence": "yes"}),
    );
    let Reply::ToolError(v) = e else {
        panic!("expected a tool error: {e:?}")
    };
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        v["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"reason\"?"),
        "{v}"
    );
    // the session is the relay: relayed_by is no parameter of rdy_dispute
    let e = c.call(
        "rdy_dispute",
        json!({"op": "open", "landing": landing, "reason": "r", "evidence": "yes", "relayed_by": "agent:relay"}),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    drop(c);
    // the daemon op refuses it the same way
    let r = Rdy::open(d.path()).unwrap();
    let engine = Local::new(r);
    let s = as_agent("agent:relay", "r1");
    let cx = OpContext {
        engine: &engine,
        session: &s,
        human: "human:qa",
        cwd: d.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let e = dispatch(
        &cx,
        "dispute",
        json!({"op": "resolve", "dispute": "abcdef", "outcme": "upheld"}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert!(e.fix.starts_with("did you mean \"outcome\"?"), "{e:?}");
    // a key the op does not take
    let e = dispatch(&cx, "dispute", json!({"op": "list", "reason": "r"})).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_INPUT", "list does not take reason")
    );
    engine.close().unwrap();
}
