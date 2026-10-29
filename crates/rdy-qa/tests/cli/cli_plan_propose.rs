//! cp14 (D-4JMPH, D-KQ4CA, D-AAQ9Z; reference/mcp-tools p-RW856, reference/cli p-ET7DJ): `rdy
//! propose` has no answer flag. It binds the answers `rdy plan` recorded, and its reply carries
//! the same plan summary `rdy_propose` and the daemon's `propose` op give.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::{DecisionInput, Rdy, Scope, Session};
use rdy_mcp::tools::{McpState, rdy_plan, rdy_propose, rdy_session};
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{brief_of, write_cited_as};
use rdy_qa::fixtures::{as_agent, land, write, writer};
use serde_json::{Value, json};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: doc `led` with section `s`, term `t`, claims `a` and `c`, and `b` (cites `a` and
/// `c`); two accepted decisions that cite `t`. Then `author` changes `t` and `a` in view `w`: its
/// plan has two reaffirm items and one confirm item.
fn build(s: &Served, author: &Session) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let seed = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "led", "title": "Ledger", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."}
    ]))
    .unwrap();
    let get = |k: &str| seed.created[k].clone();
    let (sec, t, a) = (get("$s"), get("$t"), get("$a"));
    land(&mut r, "seed").unwrap();
    let (sid, tid, aid) = (id(&sec), id(&t), id(&a));
    let token = brief_of(&r, &Scope::Canon, &[&sid]).base.token;
    write_cited_as(
        &mut r,
        &writer(),
        "st",
        json!([{"op": "update", "id": sec, "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    land(&mut r, "st").unwrap();
    let token = brief_of(&r, &Scope::Canon, &[&sid]).base.token;
    for i in 0..2 {
        r.decide(
            &writer(),
            "dv",
            DecisionInput {
                parent: sec.clone(),
                statement: format!("Rule {i}"),
                body: format!("Per the {{{{{t}}}}}."),
                brief: Some(token.clone()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide {i}: {e}"));
    }
    land(&mut r, "dv").unwrap();
    let token = brief_of(&r, &Scope::Canon, &[&tid, &aid]).base.token;
    write_cited_as(
        &mut r,
        author,
        "w",
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "update", "id": a, "body": "A holds 60 days."}
        ]),
        Some(&token),
    )
    .unwrap();
    r.close().unwrap();
}

const ACTOR: &str = "agent:qa";
const RUN: &str = "w";

fn ids_of(v: &Value, kind: &str) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn text_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

#[test]
fn cp14_cli_plan_then_propose_uses_recorded_answers() {
    let s = Served::new();
    build(&s, &as_agent(ACTOR, RUN));
    let json_as = |args: &[&str]| s.json_as(ACTOR, Some(RUN), args);
    let (ok, read) = json_as(&["--json", "plan", "--view", "w"]);
    assert!(ok, "{read}");
    let reaffirm = ids_of(&read, "reaffirm");
    let confirm = ids_of(&read, "confirm");
    assert_eq!((reaffirm.len(), confirm.len()), (2, 1), "{read:#}");

    // propose has no answer flag: the answers are recorded with rdy plan
    let out = s.rdy_as(
        ACTOR,
        Some(RUN),
        &[
            "propose",
            "--title",
            "Change",
            "--view",
            "w",
            "--plan-answers",
            "[]",
        ],
    );
    assert!(!out.status.success(), "{out:?}");

    // a plain propose binds the plan with three open items; they are on stable content, so the
    // version is not ready (E_PLAN_OPEN) and no W_PLAN_OPEN warns
    let (ok, first) = json_as(&["--json", "propose", "--title", "Change", "--view", "w"]);
    assert!(ok, "{first}");
    assert_eq!(first["plan"]["open"], 3, "{first:#}");
    assert_eq!(first["plan"]["closed"], 0, "{first:#}");
    assert!(
        !first["warnings"]
            .as_array()
            .map(|w| w.iter().any(|w| w["code"] == "W_PLAN_OPEN"))
            .unwrap_or(false),
        "{first:#}"
    );
    let (ok, queue) = s.json(&["--json", "pending"]);
    assert!(ok, "{queue}");
    let row = queue["not_ready"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["ref"] == first["ref"])
        .unwrap_or_else(|| panic!("{} is not ready: {queue:#}", first["ref"]));
    let open: Vec<&Value> = row["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["code"] == "E_PLAN_OPEN")
        .collect();
    assert_eq!(open.len(), 1, "{row:#}");
    assert_eq!(
        open[0]["message"], "3 change-plan items are open",
        "{row:#}"
    );

    // rdy plan records the answers; the next propose binds them
    let out = s.rdy_as(
        ACTOR,
        Some(RUN),
        &[
            "plan",
            "defer",
            &reaffirm[0],
            &reaffirm[1],
            "--reason",
            "later",
            "--view",
            "w",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let (ok, c) = json_as(&[
        "--json",
        "plan",
        "confirm",
        &confirm[0],
        "--reason",
        "still holds",
        "--view",
        "w",
    ]);
    assert!(ok, "{c}");
    let (ok, v) = json_as(&["--json", "propose", "--title", "Change", "--view", "w"]);
    assert!(ok, "{v}");
    assert_eq!(
        (
            &v["plan"]["open"],
            &v["plan"]["closed"],
            &v["plan"]["deferred"]
        ),
        (&json!(0), &json!(1), &json!(2)),
        "{v:#}"
    );
    assert!(
        !v["warnings"]
            .as_array()
            .map(|w| w.iter().any(|w| w["code"] == "W_PLAN_OPEN"))
            .unwrap_or(false),
        "{v:#}"
    );
    let r#ref = v["ref"].as_str().unwrap().to_string();
    let (ok, bound) = json_as(&["--json", "plan", "--proposal", &r#ref]);
    assert!(ok, "{bound}");
    assert_eq!(bound["counts"]["deferred"], 2, "{bound:#}");
    assert_eq!(bound["counts"]["closed"], 1, "{bound:#}");
    let row = bound["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == reaffirm[0].as_str())
        .unwrap();
    assert_eq!(row["answer"]["reason"], "later", "{row}");

    // the text reply says the same in its closed format
    let out = s.rdy_as(
        ACTOR,
        Some(RUN),
        &["propose", "--title", "Change again", "--view", "w"],
    );
    assert!(out.status.success(), "{out:?}");
    let text = text_of(&out);
    assert!(
        text.contains("\nchange plan: 0 open, 1 closed, 2 deferred"),
        "{text}"
    );
}

/// The parts of a propose reply that must agree across surfaces: the plan counts, the open
/// items by kind, and the warning codes. Ids and hashes differ between projects.
fn summary(v: &Value) -> Value {
    let mut items: Vec<Value> = v["plan"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| json!({"kind": i["kind"], "state": i["state"], "edge": i["edge"]}))
        .collect();
    // the items are in full-hash order, which differs between projects
    items.sort_by_key(|i| i.to_string());
    let mut warnings: Vec<Value> = v["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .map(|w| json!({"code": w["code"], "message": w["message"], "fix": w["fix"]}))
                .collect()
        })
        .unwrap_or_default();
    warnings.sort_by_key(|w| w.to_string());
    let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    keys.sort();
    json!({
        "keys": keys,
        "open": v["plan"]["open"], "closed": v["plan"]["closed"], "deferred": v["plan"]["deferred"],
        "by_kind": v["plan"]["by_kind"], "code": v["plan"]["code"],
        "omitted": v["plan"]["items_omitted"],
        "items": items, "warnings": warnings,
    })
}

#[test]
fn cp14_public_propose_returns_counts_and_open_items() {
    // three projects with the same content: the command line, the MCP tool and the daemon op
    let (cli, mcp, op) = (Served::new(), Served::new(), Served::new());
    for s in [&cli, &mcp, &op] {
        build(s, &as_agent(ACTOR, RUN));
    }

    // the MCP tool, in this process
    let engine = Arc::new(Local::new(Rdy::open(mcp.dir.path()).unwrap()));
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": ACTOR, "run": RUN, "role": "writer"})).unwrap(),
    )
    .unwrap();
    // the daemon's op, in this process
    let op_engine = Local::new(Rdy::open(op.dir.path()).unwrap());
    let session = as_agent(ACTOR, RUN);
    let cx = OpContext {
        engine: &op_engine,
        session: &session,
        human: "human:qa",
        cwd: op.dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };

    // open items: every surface reports the same counts, items and warning
    let (ok, c1) = cli.json_as(
        ACTOR,
        Some(RUN),
        &["--json", "propose", "--title", "Change", "--view", "w"],
    );
    assert!(ok, "{c1}");
    let m1 = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"title": "Change", "view": "w"})).unwrap(),
    )
    .unwrap();
    let o1 = dispatch(&cx, "propose", json!({"title": "Change", "view": "w"})).unwrap();
    assert_eq!(c1["plan"]["open"], 3, "{c1:#}");
    assert_eq!(summary(&c1), summary(&m1));
    assert_eq!(summary(&c1), summary(&o1));

    // answers: the command line records them first, the tool and the op take them with the
    // proposal; the replies agree again
    let (_, read) = cli.json_as(ACTOR, Some(RUN), &["--json", "plan", "--view", "w"]);
    let (reaffirm, confirm) = (ids_of(&read, "reaffirm"), ids_of(&read, "confirm"));
    let out = cli.rdy_as(
        ACTOR,
        Some(RUN),
        &[
            "plan",
            "defer",
            &reaffirm[0],
            "--reason",
            "later",
            "--view",
            "w",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let out = cli.rdy_as(
        ACTOR,
        Some(RUN),
        &[
            "plan",
            "confirm",
            &confirm[0],
            "--reason",
            "still holds",
            "--view",
            "w",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let (ok, c2) = cli.json_as(
        ACTOR,
        Some(RUN),
        &["--json", "propose", "--title", "Change", "--view", "w"],
    );
    assert!(ok, "{c2}");

    let items_of = |v: &Value| (ids_of(v, "reaffirm"), ids_of(v, "confirm"));
    let m_read = rdy_plan(
        &mut st,
        serde_json::from_value(json!({"view": "w"})).unwrap(),
    )
    .unwrap();
    let (mr, mc) = items_of(&m_read);
    let m2 = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"title": "Change", "view": "w", "plan_answers": [
            {"item": mr[0], "act": "defer", "reason": "later"},
            {"item": mc[0], "act": "confirm", "reason": "still holds"}]}))
        .unwrap(),
    )
    .unwrap();
    let o_read = dispatch(&cx, "plan", json!({"view": "w"})).unwrap();
    let (or, oc) = items_of(&o_read);
    let o2 = dispatch(
        &cx,
        "propose",
        json!({"title": "Change", "view": "w", "plan_answers": [
            {"item": or[0], "act": "defer", "reason": "later"},
            {"item": oc[0], "act": "confirm", "reason": "still holds"}]}),
    )
    .unwrap();
    assert_eq!(
        (
            &c2["plan"]["open"],
            &c2["plan"]["closed"],
            &c2["plan"]["deferred"]
        ),
        (&json!(1), &json!(1), &json!(1)),
        "{c2:#}"
    );
    assert_eq!(c2["plan"]["items"].as_array().unwrap().len(), 1, "{c2:#}");
    assert_eq!(summary(&c2), summary(&m2));
    assert_eq!(summary(&c2), summary(&o2));

    // a refused answer through the op writes nothing, as through the tool
    let end = |e: &Local| {
        rdy_engine::engine::Engine::write(
            e,
            Box::new(|r| Ok(json!(format!("{:?}", r.store().log_end())))),
        )
        .unwrap()
    };
    let before = end(&op_engine);
    let e = dispatch(
        &cx,
        "propose",
        json!({"title": "Change", "view": "w", "plan_answers": [
            {"item": or[1], "act": "defer", "reason": "later"},
            {"item": "pi-zz", "act": "defer", "reason": "later"}]}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM", "{e}");
    assert_eq!(end(&op_engine), before);
}
