//! cp11 (D-4JMPH, D-EJBPD, D-01QX5, D-KQ4CA; reference/mcp-tools p-92F5M, p-CSBA7, p-PZX3T,
//! p-0SH30, p-1SH3P): `rdy_plan` reads the four selectors and answers a view's items, and
//! `rdy_write` takes the reaffirm op and a code repin under the ordinary write guards. The tools
//! run in this process on a `Local` engine; the schema and the missing-session tests go through
//! the stdio server.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::plan::PlanQuery;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{McpState, PlanParams, rdy_brief, rdy_plan, rdy_session, rdy_write};
use rdy_model::BlockId;
use rdy_qa::edit_loop::{brief_of, write_cited, write_cited_as};
use rdy_qa::fixtures::{as_agent, land, project, write, writer};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn map(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Canon: doc `led` with section `s`, term `t`, claims `a` and `c`, and `b` (cites `a` and
/// `c`); `n` accepted decisions that cite `t`. The agent's view `w` (its run's name, so its
/// default view) changes `t` and `a`: in `w` the decisions are suspect on `t`, and `b` on `a`.
struct Fx {
    t: String,
    b: String,
    d: Vec<String>,
}

fn fx(r: &mut Rdy, n: usize) -> Fx {
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
    let (s, t, a, b) = (get("$s"), get("$t"), get("$a"), get("$b"));
    land(r, "seed").unwrap();
    // the section is stable: writes to it need a briefing
    let (sid, tid, aid) = (id(&s), id(&t), id(&a));
    let token = brief_of(r, &Scope::Canon, &[&sid]).base.token;
    write_cited(
        r,
        "st",
        json!([{"op": "update", "id": s, "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    land(r, "st").unwrap();
    let token = brief_of(r, &Scope::Canon, &[&sid]).base.token;
    let mut d = vec![];
    for i in 0..n {
        d.push(
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
            .unwrap_or_else(|e| panic!("decide {i}: {e}"))
            .created["$decision"]
                .clone(),
        );
    }
    land(r, "dv").unwrap();
    d.sort();
    let token = brief_of(r, &Scope::Canon, &[&tid, &aid]).base.token;
    write_cited_as(
        r,
        &as_agent("agent:qa", "w"),
        "w",
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "update", "id": a, "body": "A holds 60 days."}
        ]),
        Some(&token),
    )
    .unwrap();
    Fx { t, b, d }
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn state(engine: &Arc<Local>, actor: &str, run: Option<&str>, role: &str) -> McpState {
    let mut st = McpState::new(engine.clone());
    let mut params = json!({"actor": actor, "role": role});
    if let Some(run) = run {
        params["run"] = json!(run);
    }
    rdy_session(&mut st, p(params)).unwrap();
    st
}

fn agent_state(engine: &Arc<Local>) -> McpState {
    state(engine, "agent:qa", Some("w"), "writer")
}

/// The log end, as text: reads and refused requests must leave it as it was.
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

fn plan(st: &mut McpState, args: Value) -> Value {
    rdy_plan(st, p(args)).unwrap_or_else(|e| panic!("rdy_plan: {e}"))
}

fn items_of<'v>(v: &'v Value, kind: &str) -> Vec<&'v Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .collect()
}

#[test]
fn cp11_mcp_plan_reads_and_answers_with_exact_schema() {
    let (dir, mut r) = project();
    let f = fx(&mut r, 2);
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine);
    // a session leaves a record of its own: every session starts before the log end is taken
    let mut reader = state(&engine, "agent:reader", None, "reader");
    let mut other = state(&engine, "agent:other", Some("o1"), "writer");
    let start = log_end(&st);

    // a read is the engine's reply, whole, and writes nothing
    let direct = serde_json::to_value(
        engine
            .read()
            .change_plan(
                &PlanQuery {
                    view: Some("w".into()),
                    ..Default::default()
                },
                "w",
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(plan(&mut st, json!({"view": "w"})), direct);
    assert_eq!(
        plan(&mut st, json!({"view": "view:w"})),
        direct,
        "view: is dropped"
    );
    assert_eq!(plan(&mut st, json!({})), direct, "the session's view");
    assert_eq!(
        plan(&mut st, json!({"answers": []})),
        direct,
        "empty answers is a read"
    );
    let confirm = items_of(&direct, "confirm");
    assert_eq!(confirm.len(), 1, "{direct:#}");
    assert_eq!(confirm[0]["subject"], f.b.as_str());
    assert_eq!(items_of(&direct, "reaffirm").len(), 2, "{direct:#}");
    assert_eq!(direct["counts"]["open"], 3, "{direct:#}");
    assert_eq!(log_end(&st), start, "reads wrote nothing");

    // the other selectors go through the same engine call
    for (name, args) in [
        ("proposal", json!({"proposal": "P-0000@1"})),
        ("landing", json!({"landing": "0".repeat(64)})),
        ("decision", json!({"decision": f.d[0]})),
    ] {
        let q: PlanQuery = p(args.clone());
        let want = engine.read().change_plan(&q, "w");
        let got = rdy_plan(&mut st, p(args));
        match (want, got) {
            (Ok(w), Ok(g)) => assert_eq!(serde_json::to_value(w).unwrap(), g, "{name}"),
            (Err(w), Err(g)) => assert_eq!((w.code, w.message), (g.code, g.message), "{name}"),
            (w, g) => panic!("{name}: engine {w:?} but tool {g:?}"),
        }
    }
    assert_eq!(log_end(&st), start);

    // refusals before anything is read or written
    let answer = json!([{"item": "pi-0", "act": "defer", "reason": "x"}]);
    for (args, code, message) in [
        (
            json!({"view": "w", "proposal": "P-1@1"}),
            "E_BAD_INPUT",
            "plan takes at most one of view, proposal, landing and decision",
        ),
        (
            json!({"page": "plan1:1:x:view:w", "view": "w"}),
            "E_BAD_INPUT",
            "page continues a plan: pass it alone",
        ),
        (
            json!({"proposal": "P-1@1", "answers": answer}),
            "E_BAD_INPUT",
            "plan answers go with view only",
        ),
        (
            json!({"page": "x", "answers": answer}),
            "E_BAD_INPUT",
            "page continues a plan: pass it alone",
        ),
        (
            json!({"max_tokens": 100_001, "answers": answer}),
            "E_LIMIT",
            "max_tokens 100001 is over the limit of 100000",
        ),
    ] {
        let e = rdy_plan(&mut st, p(args.clone())).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (code, message), "{args}");
    }
    assert_eq!(log_end(&st), start);

    // answers are validated whole first: one bad item records none
    let reaffirm = items_of(&direct, "reaffirm")[0]["id"].as_str().unwrap();
    let e = rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [
            {"item": reaffirm, "act": "defer", "reason": "later"},
            {"item": "pi-zz", "act": "defer", "reason": "later"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM", "{e}");
    assert_eq!(log_end(&st), start, "a refused request wrote nothing");
    // an act the item does not take is refused too
    let e = rdy_plan(
        &mut st,
        p(json!({"answers": [{"item": reaffirm, "act": "confirm", "reason": "r"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(log_end(&st), start);

    // a reader reads and cannot answer; another agent answers only its own view
    assert_eq!(plan(&mut reader, json!({"view": "w"})), direct);
    let mine = json!({"view": "w", "answers": [{"item": reaffirm, "act": "defer", "reason": "r"}]});
    let e = rdy_plan(&mut reader, p(mine.clone())).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION", "{e}");
    let e = rdy_plan(&mut other, p(mine)).unwrap_err();
    assert_eq!(e.code, "E_VIEW_OWNER", "{e}");
    assert_eq!(log_end(&st), start);

    // the answers are recorded together, and the reply is the plan after them
    let confirm_id = confirm[0]["id"].as_str().unwrap();
    let answered = plan(
        &mut st,
        json!({"answers": [
            {"item": confirm_id, "act": "confirm", "reason": "still holds"},
            {"item": reaffirm, "act": "defer", "reason": "later"}]}),
    );
    assert_ne!(log_end(&st), start, "the answers were recorded");
    assert_eq!(
        plan(&mut st, json!({})),
        answered,
        "the reply is the plan now"
    );
    assert_eq!(answered["counts"]["open"], 1, "{answered:#}");
    assert_eq!(answered["counts"]["closed"], 1, "{answered:#}");
    assert_eq!(answered["counts"]["deferred"], 1, "{answered:#}");
    let row = items_of(&answered, "confirm")[0];
    assert_eq!(row["state"], "closed");
    assert_eq!(row["answer"]["act"], "confirm");
    assert_eq!(row["answer"]["reason"], "still holds");

    // a small budget cuts the reply, keeps the counts of the whole, and pages
    let after = log_end(&st);
    let other_reaffirm = items_of(&answered, "reaffirm")
        .into_iter()
        .find(|i| i["id"] != reaffirm)
        .unwrap()["id"]
        .clone();
    let cut = plan(
        &mut st,
        json!({"max_tokens": 1, "answers": [
            {"item": other_reaffirm, "act": "defer", "reason": "again"}]}),
    );
    assert_ne!(log_end(&st), after);
    assert_eq!(cut["items"].as_array().unwrap().len(), 1, "{cut:#}");
    assert_eq!(cut["counts"]["deferred"], 2, "full counts on a cut page");
    let next = cut["next"].as_str().expect("a cut page has next");
    let rest = plan(&mut st, json!({"page": next, "max_tokens": 100_000}));
    assert_eq!(rest["items"].as_array().unwrap().len(), 2, "{rest:#}");
    assert!(rest.get("next").is_none(), "{rest:#}");

    // the strict parameters: a typo, an unknown answer key and a bad act are refused
    let e = parse_params::<PlanParams>("rdy_plan", map(json!({"viw": "w"}))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("viw") || e.fix.contains("viw"), "{e}");
    for bad in [
        json!({"answers": [{"item": "pi-1", "act": "defer", "why": "x"}]}),
        json!({"answers": [{"item": "pi-1", "act": "reject"}]}),
        json!({"answers": [{"item": "pi-1"}]}),
    ] {
        let e = parse_params::<PlanParams>("rdy_plan", map(bad.clone())).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad}: {e}");
    }

    // through the server: the tool is listed, a session is required, a typo is refused
    drop((st, reader, other, engine));
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let listed = tools
        .iter()
        .find(|t| t["name"] == "rdy_plan")
        .expect("rdy_plan is listed");
    // the strict schema: exactly these keys, no others
    let mut keys: Vec<&str> = listed["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "answers",
            "decision",
            "landing",
            "max_tokens",
            "page",
            "proposal",
            "view"
        ]
    );
    assert_eq!(listed["inputSchema"]["additionalProperties"], false);
    let schema = listed["inputSchema"].to_string();
    assert!(
        schema.contains("\"confirm\"")
            && schema.contains("\"defer\"")
            && schema.contains("\"done\""),
        "the acts are an enum: {schema}"
    );
    assert_eq!(
        c.call("rdy_plan", json!({})).code(),
        Some("E_NO_SESSION"),
        "a session is required, also for a read"
    );
    let s = c.call("rdy_session", json!({"actor": "agent:r", "role": "reader"}));
    assert!(s.code().is_none(), "{s:?}");
    let v = c.call("rdy_plan", json!({"view": "w"})).ok();
    assert_eq!(v["scope"], "view:w");
    assert_eq!(v["counts"]["deferred"], 2, "{v:#}");
    let e = c.call("rdy_plan", json!({"vew": "w"}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    // mparm1: the standard wrong-type text, naming the parameter and its shape
    let e = c.call("rdy_plan", json!({"view": ["w"]}));
    let rdy_qa::mcp::Reply::ToolError(e) = e else {
        panic!("a tool error: {e:?}");
    };
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"],
        "rdy_plan: parameter \"view\" must be a string, not a list"
    );
    assert_eq!(e["fix"], "pass \"view\" as a string");
}

#[test]
fn cp11_mcp_reaffirm_and_code_repin_use_normal_write_guards() {
    let (_dir, mut r) = project();
    let f = fx(&mut r, 2);
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine);
    let mut other = state(&engine, "agent:other", Some("o1"), "writer");
    let brief = |st: &mut McpState, ids: &[&str]| -> String {
        let b = rdy_brief(st, p(json!({"ids": ids, "scope": "view:w"}))).unwrap();
        b["base"]["token"].as_str().unwrap().to_string()
    };
    let op = json!([{"op": "reaffirm", "because": f.t, "reason": "Holds in pence."}]);
    let w = |st: &mut McpState, ops: Value, token: Option<&str>| {
        rdy_write(st, p(json!({"view": "w", "ops": ops, "brief": token})))
    };
    let start = log_end(&st);

    // briefing: a re-affirmation of stable decisions needs one that covers them
    let e = w(&mut st, op.clone(), None).unwrap_err();
    assert_eq!(e.code, "E_BRIEF_REQUIRED", "{e}");
    let wrong = brief(&mut st, &[&f.b]);
    let e = w(&mut st, op.clone(), Some(&wrong)).unwrap_err();
    assert_eq!(e.code, "E_BRIEF_REQUIRED", "{e}");
    // owner: another agent has no write in this view, with a briefing of its own
    let theirs = rdy_brief(
        &mut other,
        p(json!({"ids": [f.d[0], f.d[1]], "scope": "view:w"})),
    )
    .unwrap()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let e = rdy_write(
        &mut other,
        p(json!({"view": "w", "ops": op, "brief": theirs})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VIEW_OWNER", "{e}");
    // frozen: a repin of an accepted decision is refused, and a re-affirmation is its way
    let token = brief(&mut st, &[&f.d[0], &f.d[1]]);
    let e = w(
        &mut st,
        json!([{"op": "repin", "id": f.d[0]}]),
        Some(&token),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
    assert!(e.fix.contains("reaffirm op"), "{e}");
    let e = w(
        &mut st,
        json!([{"op": "repin", "id": f.d[0], "code": true}]),
        Some(&token),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
    // a code repin takes its key and its own guard: the block must declare reflects
    let tb = brief(&mut st, &[&f.b]);
    let e = w(
        &mut st,
        json!([{"op": "repin", "id": f.b, "code": true}]),
        Some(&tb),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("has no reflects globs"), "{e}");
    let e = w(
        &mut st,
        json!([{"op": "repin", "id": f.b, "coed": true}]),
        Some(&tb),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("code") || e.message.contains("code"), "{e}");
    assert_eq!(log_end(&st), start, "every refusal wrote nothing");

    // with the briefing the re-affirmation lands, and its report lists the decisions
    let ok = w(&mut st, op, Some(&token)).unwrap();
    assert_eq!(
        ok["reaffirmed"],
        json!([
            {"id": f.d[0], "causes": [f.t]},
            {"id": f.d[1], "causes": [f.t]}
        ])
    );
    // the ordinary repin of a claim lands too (the pin to the changed `a`)
    let ok = w(&mut st, json!([{"op": "repin", "id": f.b}]), Some(&tb)).unwrap();
    assert!(ok["change"].is_string(), "{ok}");
}
