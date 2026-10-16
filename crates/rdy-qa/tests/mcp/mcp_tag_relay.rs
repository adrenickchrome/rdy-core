//! di9 (D-DGCH7, D-KQ4CA, C2, C3, O32): an agent relays the person's tag (`rdy_tag`) and the
//! person's proposal of their own view (`rdy_propose {relay}`). Each act and its `ANSWER` are
//! one batch (O14); the record names the person as the actor and the agent as the relay.
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::tools::{McpState, rdy_propose, rdy_session, rdy_tag, rdy_tags};
use rdy_model::{AnswerAct, EvidenceKind, Hash};
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::kinds;
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// A project whose person is `human:qa` (`.rdy/local.yaml`), with one landing.
fn landed() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    (d, r)
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
) -> Value {
    write_typed(&*st.engine, f).unwrap()
}

/// The kinds of the frames of the store's last batch.
fn last_kinds(st: &McpState) -> Vec<u8> {
    let v = on(st, |r| {
        Ok(r.store()
            .last_batch()
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<u8>>())
    });
    p(v)
}

/// The answer to `item`, as JSON (`null` when there is none), and its hash.
fn answer_of(st: &McpState, item: &str) -> Value {
    let item = item.to_string();
    on(st, move |r| {
        Ok(r.read()
            .reader()
            .answer_of(&item)
            .unwrap()
            .map(|(h, a)| json!({"hash": h.to_string(), "answer": a})))
    })
}

/// A doc `name` written into `view` by the person.
fn doc_in(r: &mut Rdy, view: &str, name: &str) {
    write(
        r,
        view,
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    )
    .unwrap();
}

fn tag(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_tag(st, p(args))
}

#[test]
fn di9_rdy_tag_needs_the_persons_words() {
    let (d, r) = landed();
    drop(r);
    // over stdio: the tool is served, its evidence is required, and blank words are no relay
    let mut c = McpClient::start(d.path());
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:relay", "run": "r1", "role": "writer"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let e = c.call("rdy_tag", json!({"version": "v0.1.0"}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    let e = c.call("rdy_tag", json!({"version": "v0.1.0", "evidence": "   "}));
    assert_eq!(e.code(), Some("E_PERMISSION"), "{e:?}");
    let Reply::ToolError(v) = e else {
        unreachable!()
    };
    assert_eq!(
        v["message"], "an agent answers only by relaying a person's words",
        "{v}"
    );
    // the relay is the session: relayed_by is no parameter of rdy_tag
    let e = c.call(
        "rdy_tag",
        json!({"version": "v0.1.0", "evidence": "yes", "relayed_by": "agent:relay"}),
    );
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    // a quote past the limit is refused by the relay table
    let long = "y".repeat(2001);
    let e = c.call("rdy_tag", json!({"version": "v0.1.0", "evidence": long}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    let v = c.call("rdy_tags", json!({})).ok();
    assert_eq!(
        v["versions"],
        json!([]),
        "a refused relay tags nothing: {v}"
    );
}

#[test]
fn di9_a_relayed_tag_records_the_person_and_an_answer() {
    let (_d, r) = landed();
    let mut st = agent_state(r);
    let v = tag(
        &mut st,
        json!({"version": "v0.1.0", "message": "first", "evidence": "yes, call it v0.1.0",
               "question": "Tag the head as v0.1.0?", "session_ref": "chat-7"}),
    )
    .unwrap();
    assert_eq!(v["name"], "v0.1.0", "{v}");
    assert_eq!(v["message"], "first", "{v}");
    assert_eq!(v["tagged_by"], "human:qa", "the person tags: {v}");
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"})
    );
    assert!(v["at_ms"].is_i64(), "{v}");
    assert_eq!(v["landing"].as_str().map(str::len), Some(64), "{v}");
    let answer = v["answer"].as_str().expect("the answer's hash").to_string();
    // one batch: the TAG, its ref, the ANSWER and its ref
    let kinds_ = last_kinds(&st);
    assert!(
        kinds_.contains(&kinds::TAG) && kinds_.contains(&kinds::ANSWER),
        "{kinds_:?}"
    );
    let tag_at = kinds_.iter().position(|k| *k == kinds::TAG).unwrap();
    let answer_at = kinds_.iter().position(|k| *k == kinds::ANSWER).unwrap();
    assert!(tag_at < answer_at, "the target comes first: {kinds_:?}");
    // the ANSWER holds the relay and names the tag
    let a = answer_of(&st, "tag:v0.1.0");
    assert_eq!(a["hash"], answer.as_str(), "{a}");
    let a: rdy_model::Answer = p(a["answer"].clone());
    assert_eq!(a.act, AnswerAct::Tag);
    assert_eq!(a.person.actor, "human:qa");
    assert_eq!(
        a.relayed_by
            .as_ref()
            .map(|b| (b.actor.as_str(), b.run.as_deref())),
        Some(("agent:relay", Some("r1")))
    );
    let ev = a.relay_evidence.clone().unwrap();
    assert_eq!(
        (
            ev.quoted.as_str(),
            ev.question.as_deref(),
            ev.session_ref.as_str()
        ),
        (
            "yes, call it v0.1.0",
            Some("Tag the head as v0.1.0?"),
            "chat-7"
        )
    );
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Text));
    assert_eq!((a.option.as_deref(), a.card_hash), (None, None));
    assert_eq!(a.targets.len(), 1);
    // the TAG encodes as before (D-F0904): its bytes hash to its id, and the person tagged it
    let tag_hash = v["tag"].as_str().unwrap().parse::<Hash>().unwrap();
    assert_eq!(a.targets[0], tag_hash);
    let t = on(&st, move |r| {
        let (k, _) = r.store().get_raw(&tag_hash).unwrap();
        let t = r.store().read_tag(&tag_hash).unwrap();
        Ok(
            json!({"kind": k, "same_id": t.id() == tag_hash, "by": t.tagged_by.actor,
                  "run": t.tagged_by.run}),
        )
    });
    assert_eq!(
        t,
        json!({"kind": kinds::TAG, "same_id": true, "by": "human:qa", "run": "cli"})
    );
    let fsck = on(&st, |r| r.fsck().map(|_| true));
    assert_eq!(fsck, json!(true));
    let v = rdy_tags(&mut st, p(json!({}))).unwrap();
    assert_eq!(v["current"], "v0.1.0", "{v}");
}

#[test]
fn di9_a_relayed_tag_keeps_every_tag_rule() {
    let (_d, r) = landed();
    let mut st = agent_state(r);
    let words = |v: &str| json!({"version": v, "evidence": format!("yes, {v}")});
    tag(&mut st, words("v0.2.0")).unwrap();
    let e = tag(&mut st, words("v0.2.0")).unwrap_err();
    assert_eq!(e.code, "E_TAG_EXISTS", "{e:?}");
    let e = tag(&mut st, words("v0.1.0")).unwrap_err();
    assert_eq!(e.code, "E_VERSION_ORDER", "{e:?}");
    let e = tag(&mut st, words("not-a-version")).unwrap_err();
    assert_eq!(e.code, "E_BAD_TAG_NAME", "{e:?}");
    // a refused tag writes no answer either
    assert_eq!(answer_of(&st, "tag:v0.1.0"), Value::Null);
    assert_eq!(answer_of(&st, "tag:not-a-version"), Value::Null);
    assert!(answer_of(&st, "tag:v0.2.0").is_object());
}

#[test]
fn di9_an_agent_proposes_the_persons_view_on_their_word() {
    let (_d, mut r) = landed();
    // the person's default view, and a view the person owns
    doc_in(&mut r, "work", "plans");
    doc_in(&mut r, "notes", "notes");
    let mut st = agent_state(r);
    let v = rdy_propose(
        &mut st,
        p(
            json!({"view": "work", "title": "Seed", "summary": "The seed doc.",
                 "relay": {"evidence": "yes, propose my work", "question": "Propose work?",
                           "session_ref": "chat-7"}}),
        ),
    )
    .unwrap();
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"})
    );
    let answer = v["answer"].as_str().expect("the answer's hash").to_string();
    let version = v["id"].as_str().unwrap().parse::<Hash>().unwrap();
    let kinds_ = last_kinds(&st);
    assert!(
        kinds_.contains(&kinds::PROPOSAL) && kinds_.contains(&kinds::ANSWER),
        "one batch: {kinds_:?}"
    );
    let a = answer_of(&st, "propose:work");
    assert_eq!(a["hash"], answer.as_str(), "{a}");
    let a: rdy_model::Answer = p(a["answer"].clone());
    assert_eq!(a.act, AnswerAct::Propose);
    assert_eq!(a.targets, vec![version]);
    assert_eq!(a.person.actor, "human:qa");
    assert_eq!(a.relay_evidence.unwrap().quoted, "yes, propose my work");
    // the proposal is the person's
    let by = on(&st, move |r| {
        Ok(r.read()
            .reader()
            .proposal(&version)
            .unwrap()
            .provenance
            .actor)
    });
    assert_eq!(by, "human:qa");
    // a view the person owns is theirs to propose too
    let v = rdy_propose(
        &mut st,
        p(json!({"view": "notes", "title": "Notes", "relay": {"evidence": "and notes"}})),
    )
    .unwrap();
    assert!(v["answer"].is_string(), "{v}");
    assert!(answer_of(&st, "propose:notes").is_object());
    let fsck = on(&st, |r| r.fsck().map(|_| true));
    assert_eq!(fsck, json!(true));
}

#[test]
fn di9_without_a_relay_the_persons_view_stays_theirs() {
    let (_d, mut r) = landed();
    doc_in(&mut r, "work", "plans");
    // a view another agent owns
    let other = agent("agent:other", Some("o1"));
    write_as(
        &mut r,
        &other,
        "o1",
        json!([{"op": "create", "type": "doc", "fields": {"name": "o", "title": "O", "doc_type": "page"}}]),
    )
    .unwrap();
    let mut st = agent_state(r);
    let e = rdy_propose(&mut st, p(json!({"view": "work", "title": "Seed"}))).unwrap_err();
    assert_eq!(e.code, "E_VIEW_OWNER", "{e:?}");
    // a relay does not open another agent's view
    let e = rdy_propose(
        &mut st,
        p(json!({"view": "o1", "title": "O", "relay": {"evidence": "yes"}})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VIEW_OWNER", "{e:?}");
    // a relay needs the person's words
    let e = rdy_propose(
        &mut st,
        p(json!({"view": "work", "title": "Seed", "relay": {"evidence": " "}})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION", "{e:?}");
    // a relay proposes the person's view only, never the agent's own (a closed set, O32)
    let e = rdy_propose(
        &mut st,
        p(json!({"view": "r1", "title": "Mine", "relay": {"evidence": "yes"}})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    assert_eq!(answer_of(&st, "propose:work"), Value::Null);
    assert_eq!(answer_of(&st, "propose:o1"), Value::Null);
    assert_eq!(answer_of(&st, "propose:r1"), Value::Null);
}
