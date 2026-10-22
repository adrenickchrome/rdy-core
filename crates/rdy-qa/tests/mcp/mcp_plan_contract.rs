//! cp14 (D-4JMPH, D-KQ4CA, D-EJBPD, D-AAQ9Z; reference/mcp-tools p-RW856, p-0SH30, p-1SH3P,
//! p-6YBR7; reference/errors p-ET7DJ, p-EVM5J, p-7JZCN, p-EESS3, p-5TZ28, p-PRSY3): `rdy_propose`
//! takes `plan_answers` and records them in the proposal's batch, and the published contract
//! (tool descriptions, `rdy_schema`, the error rows) says what the engine does. The tools run in
//! this process on a `Local` engine; the schema and descriptions go through the stdio server.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::{DecisionInput, Rdy, Scope, Session};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{
    CHANGE_PLAN_DOC, CHANGE_PLAN_EXAMPLES, ERROR_ROWS, McpState, PlanParams, ProposeParams,
    WRITE_OPS, rdy_plan, rdy_propose, rdy_schema, rdy_session,
};
use rdy_model::BlockId;
use rdy_qa::edit_loop::{brief_of, write_cited_as};
use rdy_qa::fixtures::{as_agent, land, project, write, writer};
use rdy_qa::mcp::McpClient;
use rdy_store::kinds;
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn map(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: doc `led` with section `s`, term `t`, claims `a` and `c`, and `b` (cites `a` and
/// `c`); `n` accepted decisions that cite `t`. Then `author` changes `t` and `a` in `view`: there
/// the decisions are suspect on `t`, and `b` on `a`. The plan of `view` has `n` reaffirm items
/// and one confirm item.
fn fx(r: &mut Rdy, n: usize, view: &str, author: &Session) {
    let seed = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "led", "title": "Ledger", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."}
    ]))
    .unwrap();
    let get = |k: &str| seed.created[k].clone();
    let (s, t, a) = (get("$s"), get("$t"), get("$a"));
    land(r, "seed").unwrap();
    let (sid, tid, aid) = (id(&s), id(&t), id(&a));
    let token = brief_of(r, &Scope::Canon, &[&sid]).base.token;
    write_cited_as(
        &mut *r,
        &writer(),
        "st",
        json!([{"op": "update", "id": s, "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    land(r, "st").unwrap();
    let token = brief_of(r, &Scope::Canon, &[&sid]).base.token;
    for i in 0..n {
        r.decide(
            &writer(),
            "dv",
            DecisionInput {
                parent: s.clone(),
                statement: format!("Rule {i}"),
                body: format!("Per the {{{{{t}}}}}."),
                brief: Some(token.clone()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide {i}: {e}"));
    }
    land(r, "dv").unwrap();
    let token = brief_of(r, &Scope::Canon, &[&tid, &aid]).base.token;
    write_cited_as(
        r,
        author,
        view,
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "update", "id": a, "body": "A holds 60 days."}
        ]),
        Some(&token),
    )
    .unwrap();
}

fn agent_state(engine: &Arc<Local>, actor: &str, run: &str) -> McpState {
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": actor, "run": run, "role": "writer"})),
    )
    .unwrap();
    st
}

/// The log end, as text: refused requests must leave it as it was.
fn log_end(st: &McpState) -> String {
    st.engine
        .write(Box::new(|r| {
            Ok(json!(format!("{:?}", r.store().log_end())))
        }))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// The kinds of the frames of the store's last batch.
fn last_kinds(st: &McpState) -> Vec<u8> {
    let v = write_typed(&*st.engine, |r| {
        Ok(r.store()
            .last_batch()
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<u8>>())
    })
    .unwrap();
    p(v)
}

fn plan(st: &mut McpState, args: Value) -> Value {
    rdy_plan(st, p(args)).unwrap_or_else(|e| panic!("rdy_plan: {e}"))
}

fn ids_of(v: &Value, kind: &str) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn cp14_propose_answers_are_atomic_over_mcp() {
    let (_dir, mut r) = project();
    fx(&mut r, 2, "w", &as_agent("agent:qa", "w"));
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine, "agent:qa", "w");
    let read = plan(&mut st, json!({"view": "w"}));
    let reaffirm = ids_of(&read, "reaffirm");
    let confirm = ids_of(&read, "confirm");
    assert_eq!((reaffirm.len(), confirm.len()), (2, 1), "{read:#}");
    let start = log_end(&st);

    // a refused last item records none of the valid ones, and no proposal
    let e = rdy_propose(
        &mut st,
        p(json!({"title": "Change", "plan_answers": [
            {"item": confirm[0], "act": "confirm", "reason": "still holds"},
            {"item": reaffirm[0], "act": "defer", "reason": "later"},
            {"item": "pi-zz", "act": "defer", "reason": "later"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM", "{e}");
    assert_eq!(log_end(&st), start, "a refused propose wrote nothing");
    // an act the item does not take is refused the same way
    let e = rdy_propose(
        &mut st,
        p(json!({"title": "Change", "plan_answers": [
            {"item": reaffirm[0], "act": "defer", "reason": "later"},
            {"item": reaffirm[1], "act": "confirm", "reason": "r"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(log_end(&st), start);
    // a deferral without its reason too
    let e = rdy_propose(
        &mut st,
        p(json!({"title": "Change", "plan_answers": [
            {"item": reaffirm[0], "act": "defer"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(log_end(&st), start);
    assert_eq!(plan(&mut st, json!({"view": "w"})), read, "nothing changed");

    // all valid: the proposal, its plan and every answer are one batch
    let v = rdy_propose(
        &mut st,
        p(json!({"title": "Change", "plan_answers": [
            {"item": confirm[0], "act": "confirm", "reason": "still holds"},
            {"item": reaffirm[0], "act": "defer", "reason": "later"},
            {"item": reaffirm[1], "act": "defer", "reason": "later too"}]})),
    )
    .unwrap();
    let batch = last_kinds(&st);
    assert!(
        batch.contains(&kinds::PROPOSAL) && batch.contains(&kinds::PLAN),
        "one batch: {batch:?}"
    );
    assert_eq!(
        (
            &v["plan"]["open"],
            &v["plan"]["closed"],
            &v["plan"]["deferred"]
        ),
        (&json!(0), &json!(1), &json!(2)),
        "{v:#}"
    );
    let bound = plan(&mut st, json!({"proposal": v["ref"]}));
    assert_eq!(bound["counts"]["deferred"], 2, "{bound:#}");
    assert_eq!(bound["counts"]["closed"], 1, "{bound:#}");
    let row = bound["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == reaffirm[0].as_str())
        .unwrap();
    assert_eq!(row["answer"]["act"], "defer");
    assert_eq!(row["answer"]["reason"], "later");
    assert_eq!(
        row["answer"]["by"]["actor"], "agent:qa",
        "an agent's own answers are the agent's"
    );

    // a relay proposes the person's own view: the version, its plan and the answers are the
    // person's, and the relaying agent is on the answer record
    let (dir2, mut r2) = project();
    std::fs::write(dir2.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    fx(&mut r2, 2, "work", &writer());
    let engine2 = Arc::new(Local::new(r2));
    let mut relay = agent_state(&engine2, "agent:relay", "r1");
    let read2 = plan(&mut relay, json!({"view": "work"}));
    let owed = ids_of(&read2, "reaffirm");
    assert_eq!(owed.len(), 2, "{read2:#}");
    let start2 = log_end(&relay);
    let e = rdy_propose(
        &mut relay,
        p(
            json!({"view": "work", "title": "Relayed", "relay": {"evidence": "yes, propose it"},
            "plan_answers": [
                {"item": owed[0], "act": "defer", "reason": "later"},
                {"item": "pi-zz", "act": "defer", "reason": "later"}]}),
        ),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM", "{e}");
    assert_eq!(
        log_end(&relay),
        start2,
        "a refused relay wrote no answer either"
    );
    let v2 = rdy_propose(
        &mut relay,
        p(json!({"view": "work", "title": "Relayed",
            "relay": {"evidence": "yes, propose it and defer the first", "question": "Propose work?"},
            "plan_answers": [{"item": owed[0], "act": "defer", "reason": "later"}]})),
    )
    .unwrap();
    assert_eq!(
        v2["relayed_by"],
        json!({"actor": "agent:relay", "run": "r1"}),
        "{v2:#}"
    );
    assert!(v2["answer"].is_string(), "the relay's answer: {v2:#}");
    let batch2 = last_kinds(&relay);
    for k in [kinds::PROPOSAL, kinds::ANSWER, kinds::PLAN] {
        assert!(batch2.contains(&k), "one batch: {batch2:?}");
    }
    let bound2 = plan(&mut relay, json!({"proposal": v2["ref"]}));
    let row2 = bound2["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == owed[0].as_str())
        .unwrap();
    assert_eq!(row2["state"], "deferred", "{bound2:#}");
    assert_eq!(
        row2["answer"]["by"]["actor"], "human:qa",
        "the person's answer, relayed: {row2:#}"
    );
    assert_eq!(v2["plan"]["open"], 2, "{v2:#}");
    assert_eq!(v2["plan"]["deferred"], 1, "{v2:#}");
}

#[test]
fn cp14_plan_and_propose_pages_keep_full_counts() {
    let (_dir, mut r) = project();
    fx(&mut r, 6, "w", &as_agent("agent:qa", "w"));
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine, "agent:qa", "w");
    let whole = plan(&mut st, json!({"view": "w"}));
    assert_eq!(whole["counts"]["open"], 7, "{whole:#}");

    // a low budget cuts the consequences, never the plan's counts or its open items
    let v = rdy_propose(&mut st, p(json!({"title": "Low", "max_tokens": 1}))).unwrap();
    assert_eq!(v["plan"]["open"], 7, "{v:#}");
    assert_eq!(v["plan"]["by_kind"]["reaffirm"]["open"], 6, "{v:#}");
    assert_eq!(v["plan"]["items"].as_array().unwrap().len(), 7, "{v:#}");
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "W_PLAN_OPEN" && w["message"] == "7 change-plan items are open"),
        "{v:#}"
    );

    // the same budget answers all seven in the propose, and the counts are still whole
    let every: Vec<Value> = whole["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| json!({"item": i["id"], "act": "defer", "reason": "a separate proposal"}))
        .collect();
    let v = rdy_propose(
        &mut st,
        p(json!({"title": "Low", "max_tokens": 1, "plan_answers": every})),
    )
    .unwrap();
    assert_eq!(
        (
            &v["plan"]["open"],
            &v["plan"]["closed"],
            &v["plan"]["deferred"]
        ),
        (&json!(0), &json!(0), &json!(7)),
        "{v:#}"
    );
    assert!(
        !v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "W_PLAN_OPEN"),
        "{v:#}"
    );

    // the plan read of that version: one item a page, the counts of the whole on each page
    let cut = plan(&mut st, json!({"proposal": v["ref"], "max_tokens": 1}));
    assert_eq!(cut["items"].as_array().unwrap().len(), 1, "{cut:#}");
    assert_eq!(cut["counts"]["deferred"], 7, "{cut:#}");
    let mut seen = 1;
    let mut next = cut["next"].as_str().map(str::to_string);
    while let Some(n) = next {
        let page = plan(&mut st, json!({"page": n, "max_tokens": 1}));
        assert_eq!(page["counts"]["deferred"], 7, "{page:#}");
        seen += page["items"].as_array().unwrap().len();
        next = page["next"].as_str().map(str::to_string);
    }
    assert_eq!(seen, 7);
}

#[test]
fn cp14_schema_names_every_new_input_and_refuses_typos() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let tool = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();

    // rdy_propose publishes plan_answers, and its item shape is the strict answer shape
    let propose = tool("rdy_propose");
    let props = propose["inputSchema"]["properties"].as_object().unwrap();
    assert!(props.contains_key("plan_answers"), "{propose}");
    let schema = propose["inputSchema"].to_string();
    let answer = &propose["inputSchema"]["$defs"]["PlanAnswerParams"];
    let mut keys: Vec<&str> = answer["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["act", "item", "reason"], "{answer}");
    assert_eq!(answer["additionalProperties"], false, "{answer}");
    assert_eq!(propose["inputSchema"]["additionalProperties"], false);
    for act in ["confirm", "defer", "done"] {
        assert!(schema.contains(&format!("\"{act}\"")), "{act}: {schema}");
    }
    let mut top: Vec<&str> = props.keys().map(String::as_str).collect();
    top.sort();
    assert_eq!(
        top,
        [
            "alternatives",
            "changes",
            "max_tokens",
            "plan_answers",
            "relay",
            "summary",
            "title",
            "view"
        ]
    );

    // every write op is published, in the schema and in the tool's ops text
    let ops_text = tool("rdy_write")["inputSchema"]["properties"]["ops"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    for name in [
        "create",
        "update",
        "move",
        "delete",
        "rebalance",
        "repin",
        "reaffirm",
    ] {
        assert!(
            WRITE_OPS.iter().any(|(n, _)| *n == name),
            "{name} is in WRITE_OPS"
        );
        assert!(
            ops_text.contains(&format!("\"op\":\"{name}\"")),
            "{name}: {ops_text}"
        );
    }
    let (_, repin) = WRITE_OPS.iter().find(|(n, _)| *n == "repin").unwrap();
    assert!(repin.contains("\"code\":<true>?"), "{repin}");
    let (_, reaffirm) = WRITE_OPS.iter().find(|(n, _)| *n == "reaffirm").unwrap();
    for part in [
        "\"id\":<decision id>?",
        "\"because\":<id or path>?",
        "\"base\":<version>?",
        "\"targets\":[<id or path>]?",
        "\"reason\":<1 to 2000 characters>",
        "Give id or because, not both",
        "reason is required",
    ] {
        assert!(reaffirm.contains(part), "{part}: {reaffirm}");
        assert!(ops_text.contains(part), "{part}: {ops_text}");
    }
    assert!(ops_text.contains("\"code\":<true>?"), "{ops_text}");

    // the strict parsers refuse a typo at every level and name the nearest known key
    let e =
        parse_params::<ProposeParams>("rdy_propose", map(json!({"title": "t", "plan_answer": []})))
            .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("plan_answers"), "{e}");
    let e = parse_params::<ProposeParams>(
        "rdy_propose",
        map(json!({"title": "t", "plan_answers": [
            {"item": "pi-1", "act": "defer", "reason": "x"},
            {"item": "pi-2", "act": "defer", "reson": "x"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("reson"), "{e}");
    assert!(e.fix.contains("reason"), "{e}");
    for bad in [
        json!({"title": "t", "plan_answers": [{"item": "pi-1", "act": "reject"}]}),
        json!({"title": "t", "plan_answers": [{"act": "defer", "reason": "x"}]}),
        json!({"title": "t", "plan_answers": {"item": "pi-1"}}),
    ] {
        let e = parse_params::<ProposeParams>("rdy_propose", map(bad.clone())).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad}: {e}");
    }
    // a write op: the new op takes no key it does not publish
    let e = rdy_engine::parse_ops(vec![
        json!({"op": "reaffirm", "id": "D-12345", "reson": "x"}),
    ])
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("reason"), "{e}");
    let e = rdy_engine::parse_ops(vec![json!({"op": "repin", "id": "D-12345", "coed": true})])
        .unwrap_err();
    assert!(e.fix.contains("code"), "{e}");

    // the daemon's propose input is as strict, at both levels
    let (dir2, r2) = project();
    let engine = Local::new(r2);
    let session = as_agent("agent:qa", "w");
    let cx = OpContext {
        engine: &engine,
        session: &session,
        human: "human:qa",
        cwd: dir2.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let e = dispatch(
        &cx,
        "propose",
        json!({"title": "t", "plan_answers": [{"item": "pi-1", "act": "defer", "reson": "x"}]}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message.contains("reson") && e.fix.contains("reason"),
        "{e}"
    );
    let e = dispatch(&cx, "propose", json!({"title": "t", "plan_answer": []})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("plan_answers"), "{e}");
}

/// The four selector scopes, kinds, acts and the allowed-act matrix, as the live engine has them.
fn live_matrix_text() -> Vec<&'static str> {
    vec![
        "update (change the subject to follow the decision)",
        "rejustify (give the subject a new justification)",
        "reaffirm (re-affirm the subject against the decision)",
        "confirm (confirm the subject as it is)",
        "update_implementation (change the code that implements the decision)",
        "update, rejustify and reaffirm take defer. confirm takes confirm or defer. update_implementation takes done or defer.",
        "confirm and defer need a reason (1 to 2000 characters); done keeps none.",
        "scope view:<name>",
        "scope proposal:P-<id>@<n>",
        "scope landing:<hash>",
        "scope decision:<id>",
        "{landing, version, items, outcomes}",
        "pass page: <next> alone to continue it",
        "reaffirmed maps each target block id to {against, reason}",
        "reflects_at is {files, commits}",
    ]
}

/// What the change-plan guidance says of the two queue kinds a plan adds (decision-intake
/// p-T69HT, p-WQB0V; reference/mcp-tools p-VVBVR, p-D3P76).
fn queue_guidance() -> [&'static str; 6] {
    [
        "A defer card (ref defer:P-x@n) asks the person to accept or refuse the deferrals of a proposal.",
        "A reaffirm card (ref reaffirm:<cause>@<version>) asks the person whether the decisions that a changed block made suspect still hold.",
        "Each needs the person's own choice and carries no recommended option.",
        "Show its options word for word and relay the choice with rdy_answer.",
        "all_recommended refuses both kinds with <ref> has no recommended option; answer each on its own.",
        "Keep them suspect needs a note of 1 to 2000 characters.",
    ]
}

#[test]
fn cp14_instructions_publish_the_change_plan_rule_at_initialization() {
    let prose = "Before proposing, read rdy_plan and close or defer each item. Give every deferral a reason. Re-affirm a suspect decision that still holds with a reaffirm op, and supersede one that does not.";
    // the compiled sentences, with the proposing guidance
    let at = rdy_mcp::server::INSTRUCTION_SENTENCES
        .iter()
        .position(|s| s.contains(prose))
        .expect("an instruction element carries the change-plan rule");
    assert!(
        rdy_mcp::server::INSTRUCTION_SENTENCES[at].contains("propose with rdy_propose."),
        "the rule sits with the proposing guidance"
    );
    // the stdio server's initialize reply publishes it
    let (dir, r) = project();
    drop(r);
    let c = McpClient::start(dir.path());
    let i = c.initialize_result()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(i.contains(prose), "{i}");
}

#[test]
fn cp14_queue_cards_need_the_persons_choice_in_every_published_text() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let desc = |n: &str| {
        tools
            .iter()
            .find(|t| t["name"] == n)
            .unwrap_or_else(|| panic!("{n} is listed"))["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // rdy_pending lists all six kinds and says the two new ones carry no recommendation
    let pending = desc("rdy_pending");
    assert!(
        pending.contains("kind (approve, confirm, answer, resolve, defer, reaffirm)"),
        "{pending}"
    );
    assert!(
        pending.contains("A defer card and a reaffirm card need the person's own choice and carry no recommended option; show both options and wait for the person."),
        "{pending}"
    );
    // rdy_answer says all_recommended never answers them
    let answer = desc("rdy_answer");
    assert!(
        answer.contains("all_recommended refuses a defer or a reaffirm card with E_BAD_INPUT (<ref> has no recommended option); answer each of them on its own."),
        "{answer}"
    );
    // rdy_plan points at the same cards
    for part in queue_guidance() {
        assert!(CHANGE_PLAN_DOC.contains(part), "{part}");
    }
}

#[test]
fn cp14_guidance_and_error_rows_match_the_live_contract() {
    // the exact prose, and the four samples, word for word
    let prose = "Before proposing, read rdy_plan and close or defer each item. Give every deferral a reason. Re-affirm a suspect decision that still holds with a reaffirm op, and supersede one that does not.";
    assert!(CHANGE_PLAN_DOC.starts_with(prose), "{CHANGE_PLAN_DOC}");
    for part in live_matrix_text() {
        assert!(CHANGE_PLAN_DOC.contains(part), "{part}");
    }
    // the queue kinds: named, a personal choice, no recommended option, all_recommended refuses
    for part in queue_guidance() {
        assert!(CHANGE_PLAN_DOC.contains(part), "{part}");
    }
    assert_eq!(
        CHANGE_PLAN_EXAMPLES,
        [
            r#"{"op":"reaffirm","id":"D-12345","reason":"The decision still holds after the premise changed."}"#,
            r#"{"op":"reaffirm","because":"p-12345","reason":"These decisions still hold after the premise changed."}"#,
            r#"{"op":"repin","id":"p-12345","code":true}"#,
            r#"{"view":"edit","answers":[{"item":"pi-0123456789abcdef","act":"defer","reason":"The follow-up needs a separate proposal."}]}"#,
        ]
    );
    // each sample parses as the input it shows
    for e in &CHANGE_PLAN_EXAMPLES[..3] {
        let v: Value = serde_json::from_str(e).unwrap();
        rdy_engine::parse_ops(vec![v]).unwrap_or_else(|err| panic!("{e}: {err}"));
    }
    let v: Value = serde_json::from_str(CHANGE_PLAN_EXAMPLES[3]).unwrap();
    parse_params::<PlanParams>("rdy_plan", map(v)).unwrap();

    // rdy_schema carries both
    let (_dir, r) = project();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "run": "w", "role": "writer"})),
    )
    .unwrap();
    let s = rdy_schema(&mut st, p(json!({}))).unwrap();
    assert_eq!(s["change_plan"]["rule"], CHANGE_PLAN_DOC);
    let examples: Vec<String> = s["change_plan"]["examples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.to_string())
        .collect();
    let want: Vec<String> = CHANGE_PLAN_EXAMPLES
        .iter()
        .map(|e| serde_json::from_str::<Value>(e).unwrap().to_string())
        .collect();
    assert_eq!(examples, want);
    // PLC2's briefing text is intact
    assert!(
        s["edit_loop"]
            .as_str()
            .unwrap()
            .contains("E_BRIEF_REQUIRED and E_BRIEF_STALE carry a fresh briefing"),
        "{}",
        s["edit_loop"]
    );
    for code in [
        "E_PLAN_ITEM",
        "E_DEFERRAL_OPEN",
        "W_PLAN_OPEN",
        "W_PLAN_ANSWER_LAPSED",
        "W_REFLECTS_NO_CODE",
        "E_FROZEN",
    ] {
        assert!(s["errors"][code]["when"].is_string(), "{code}");
    }

    // the rows, word for word, before E_HELD_BY_READER and the two publication rows
    let n = ERROR_ROWS.len();
    assert_eq!(
        ERROR_ROWS[n - 3..].iter().map(|r| r.0).collect::<Vec<_>>(),
        [
            "E_HELD_BY_READER",
            "E_PUBLICATION_REFERENCE",
            "W_PUBLICATION_REFERENCE"
        ]
    );
    let new = [
        (
            "E_PLAN_ITEM",
            "<id> is not an item of the change plan for <scope>",
            "read the plan with rdy_plan / rdy plan and pass an item id it lists",
        ),
        (
            "E_DEFERRAL_OPEN",
            "<n> deferrals of P-x@n have no answer",
            "answer the deferral card first: defer:P-x@n",
        ),
        (
            "W_PLAN_OPEN",
            "<n> change-plan items are open",
            "close or defer each item: rdy_plan / rdy plan",
        ),
        (
            "W_PLAN_ANSWER_LAPSED",
            "the answer to <id> lapsed: <block> changed",
            "answer the item again",
        ),
        (
            "W_REFLECTS_NO_CODE",
            "no linked checkout; rdy records no code state for <id>",
            "set code_paths, then repin it with code: true",
        ),
    ];
    let at = ERROR_ROWS
        .iter()
        .position(|r| r.0 == "E_PLAN_ITEM")
        .expect("an E_PLAN_ITEM row");
    assert_eq!(ERROR_ROWS[at..at + 5], new);
    assert_eq!(ERROR_ROWS[at + 6].0, "E_HELD_BY_READER");
    let frozen = ERROR_ROWS[at + 5];
    assert_eq!(frozen.0, "E_FROZEN");
    assert!(frozen.2.starts_with("take the landed side"), "{frozen:?}");
    assert!(
        frozen.2.ends_with(
            "For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead."
        ),
        "{frozen:?}"
    );
    let codes: Vec<&str> = ERROR_ROWS.iter().map(|r| r.0).collect();
    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes.len(), "one row per code");
    // the codes are the engine's
    assert_eq!(rdy_engine::E_PLAN_ITEM, "E_PLAN_ITEM");
    assert_eq!(rdy_engine::E_DEFERRAL_OPEN, "E_DEFERRAL_OPEN");
    assert_eq!(new[3].2, rdy_engine::plan::PLAN_ANSWER_LAPSED_FIX);
    assert_eq!(new[4].2, rdy_engine::reflects::NO_CODE_FIX);
    assert_eq!(new[2].2, rdy_engine::plan::PLAN_OPEN_FIX);
    assert_eq!(new[0].2, rdy_engine::plan::PLAN_ITEM_FIX);
}

#[test]
fn cp14_guidance_matches_the_engine_in_a_live_project() {
    let (dir, mut r) = project();
    fx(&mut r, 2, "w", &as_agent("agent:qa", "w"));
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine, "agent:qa", "w");
    let read = plan(&mut st, json!({"view": "w"}));
    let reaffirm = ids_of(&read, "reaffirm");
    let confirm = ids_of(&read, "confirm");

    // E_PLAN_ITEM: the row's message shape and fix are the engine's
    let e = rdy_propose(
        &mut st,
        p(json!({"title": "t", "plan_answers": [{"item": "pi-zz", "act": "defer", "reason": "x"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM");
    assert_eq!(
        e.message,
        "pi-zz is not an item of the change plan for view:w"
    );
    let row = ERROR_ROWS.iter().find(|r| r.0 == "E_PLAN_ITEM").unwrap();
    assert_eq!(e.fix, row.2);

    // W_PLAN_OPEN: the row's message with <n> filled in, and its fix
    let v = rdy_propose(&mut st, p(json!({"title": "open"}))).unwrap();
    let w = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "W_PLAN_OPEN")
        .unwrap_or_else(|| panic!("{v:#}"));
    let row = ERROR_ROWS.iter().find(|r| r.0 == "W_PLAN_OPEN").unwrap();
    assert_eq!(w["message"], row.1.replace("<n>", "3"));
    assert_eq!(w["fix"], row.2);

    // the allowed-act matrix: confirm takes confirm and defer; reaffirm takes defer only
    for (item, act, ok) in [
        (&reaffirm[0], "confirm", false),
        (&reaffirm[0], "done", false),
        (&reaffirm[0], "defer", true),
        (&confirm[0], "done", false),
        (&confirm[0], "confirm", true),
        (&confirm[0], "defer", true),
    ] {
        let r = rdy_propose(
            &mut st,
            p(json!({"title": "t", "plan_answers": [{"item": item, "act": act, "reason": "x"}]})),
        );
        assert_eq!(r.is_ok(), ok, "{act} on {item}: {r:?}");
    }

    // E_FROZEN: a repin of an accepted decision is refused with the reaffirm advice last
    let decision = read["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "reaffirm")
        .unwrap()["subject"]
        .as_str()
        .unwrap()
        .to_string();
    let token =
        rdy_mcp::tools::rdy_brief(&mut st, p(json!({"ids": [decision], "scope": "view:w"})))
            .unwrap()["base"]["token"]
            .as_str()
            .unwrap()
            .to_string();
    let e = rdy_mcp::tools::rdy_write(
        &mut st,
        p(json!({"view": "w", "ops": [{"op": "repin", "id": decision}], "brief": token})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
    let row = ERROR_ROWS.iter().find(|r| r.0 == "E_FROZEN").unwrap();
    let hint = "For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead.";
    assert!(e.fix.ends_with(hint) && row.2.ends_with(hint), "{e}");

    // the published descriptions
    drop((st, engine));
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let d = |n: &str| {
        tools.iter().find(|t| t["name"] == n).unwrap()["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        d("rdy_plan"),
        "Read a change plan, or record answers in the session's view. Read a proposal, landing or decision with its selector. Page with next. Confirm and defer need a reason."
    );
    assert!(
        d("rdy_propose").contains("plan_answers records answers in the same batch as the proposal. The reply includes the change plan. Open non-code items warn; deferred non-code items need the person's answer before landing."),
        "{}",
        d("rdy_propose")
    );
    assert!(
        d("rdy_write").contains("Use reaffirm for a suspect accepted decision that still holds. Use repin with code: true after checking a reflects block against its code."),
        "{}",
        d("rdy_write")
    );
}
