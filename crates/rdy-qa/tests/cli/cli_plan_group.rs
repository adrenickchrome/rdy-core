//! rg11 (FW-UX1 D-KQ4CA; D-ME2HD "The CLI takes the same group fields and `repin` as flags of
//! `rdy plan`"): `rdy plan dismiss`, the group flags and `--repin` build the answers `rdy_plan`
//! takes, and the daemon runs the one function the tool runs.
use rdy_engine::engine::Local;
use rdy_engine::{Coverage, Rdy, Scope, Session, WriteRequest, parse_ops};
use rdy_mcp::tools::{McpState, rdy_plan, rdy_propose, rdy_session};
use rdy_model::{EditorialClass, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::write_cited_as;
use rdy_qa::fixtures::{as_agent, land, project, write, writer};
use serde_json::{Value, json};
use std::sync::Arc;

/// Canon: draft doc `notes` with claim `a` and claim `b`, which cites `a`; doc `old`, deprecated,
/// with claim `x`. The view `w` then edits `a` (a confirm item for `b`) and adds claim `y` that
/// cites `x` (a tension item).
fn world(r: &mut Rdy, by: &Session) {
    let w = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$n", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$n", "body": "A holds 30 days."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$n", "body": "B builds on {{$a}}."},
        {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "old", "title": "Old", "doc_type": "page", "lifecycle": "deprecated"}},
        {"op": "create", "type": "section", "id": "$o", "parent": "$od", "fields": {"title": "Old rules"}},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$o", "body": "X is the old rule."}
    ]))
    .unwrap();
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    write_cited_as(
        r,
        by,
        "w",
        json!([
            {"op": "update", "id": get("$a"), "body": "A holds 60 days."},
            {"op": "create", "type": "claim", "id": "$y", "parent": get("$n"),
                "body": format!("Y follows {{{{{}}}}}.", get("$x"))}
        ]),
        None,
    )
    .unwrap();
}

fn text_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

fn items_of<'v>(v: &'v Value, kind: &str) -> Vec<&'v Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .collect()
}

/// What two projects with the same shape must agree on: each item's kind, state and answer, and
/// the counts. Block ids differ between projects, so the ids stay out.
fn shape(v: &Value) -> (Vec<String>, Value) {
    let mut rows: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!(
                "{} {} {} {}",
                i["kind"], i["state"], i["answer"]["act"], i["answer"]["reason"]
            )
        })
        .collect();
    rows.sort();
    (rows, v["counts"].clone())
}

#[test]
fn rg11_cli_plan_dismiss_and_group_flags_match_mcp() {
    // the CLI, on a served project
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    world(&mut r, &writer());
    r.close().unwrap();
    let (_, read) = s.json(&["--json", "plan", "--view", "w"]);
    let tn = items_of(&read, "tension")[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(tn.starts_with("tn-"), "{tn}");

    let out = s.rdy(&[
        "plan",
        "dismiss",
        &tn,
        "--reason",
        "Y cites it as history",
        "--view",
        "w",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text_of(&out), format!("answered {tn}: dismiss"));
    let out = s.rdy(&[
        "plan",
        "confirm",
        "--kind",
        "confirm",
        "--repin",
        "--reason",
        "b follows a",
        "--view",
        "w",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text_of(&out), "answered group: confirm");
    let (_, via_cli) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(via_cli["counts"]["open"], 0, "{via_cli:#}");

    // the tool, on a project of the same shape
    let (_dir, mut r) = project();
    world(&mut r, &as_agent("agent:qa", "w"));
    let engine = Arc::new(Local::new(r));
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "run": "w"})).unwrap(),
    )
    .unwrap();
    let mut run = |args: Value| {
        rdy_plan(&mut st, serde_json::from_value(args).unwrap())
            .unwrap_or_else(|e| panic!("rdy_plan: {e}"))
    };
    let read = run(json!({"view": "w"}));
    let tn = items_of(&read, "tension")[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    run(json!({"view": "w", "answers": [
        {"item": tn, "act": "dismiss", "reason": "Y cites it as history"}]}));
    let via_mcp = run(json!({"view": "w", "answers": [
        {"group": {"kind": "confirm"}, "act": "confirm", "reason": "b follows a", "repin": true}]}));

    assert_eq!(shape(&via_cli), shape(&via_mcp));
    let row = items_of(&via_cli, "tension")[0];
    assert_eq!(row["answer"]["act"], "dismiss", "{via_cli:#}");
    assert_eq!(items_of(&via_cli, "confirm")[0]["state"], "closed");
}

#[test]
fn rg11_cli_plan_with_no_item_and_no_group_exits_2() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    world(&mut r, &writer());
    r.close().unwrap();
    for act in ["confirm", "defer", "done", "dismiss"] {
        let out = s.rdy(&["plan", act, "--reason", "x", "--view", "w"]);
        assert_eq!(out.status.code(), Some(2), "{act}: {out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("name an item, or a group with --kind, --follow-up, --doc or --edge"),
            "{act}: {err}"
        );
    }
    // --repin goes with a group
    let out = s.rdy(&["plan", "confirm", "--repin", "--reason", "x", "--view", "w"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    // a refused request wrote nothing
    let (_, v) = s.json(&["--json", "plan", "--view", "w"]);
    assert!(v["plan"].is_null(), "{v:#}");
}

/// Canon: a stable doc `spec` with claim `a` and claim `b`, which cites `a`. The view `w` then
/// makes a briefed, justified edit of `a`: a confirm item for `b` on stable content. Returns
/// `b`'s id.
fn stable_world(r: &mut Rdy, by: &Session) -> String {
    let clarify = || {
        Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "the rule moved".into(),
        })
    };
    let ops = |v: Value| parse_ops(v.as_array().unwrap().clone()).unwrap();
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "seed".into(),
                ops: ops(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page", "lifecycle": "stable"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                    {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
                    {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."}
                ])),
                justification: clarify(),
                message: String::new(),
            },
        )
        .unwrap();
    land(r, "seed").unwrap();
    let a = w.created["$a"].clone();
    let brief = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![a.clone()],
                path: None,
            },
            None,
        )
        .unwrap();
    brief.register(&r.briefs());
    r.write_briefed(
        by,
        WriteRequest {
            view: "w".into(),
            ops: ops(json!([{"op": "update", "id": a, "body": "A holds 60 days."}])),
            justification: clarify(),
            message: String::new(),
        },
        Some(brief.base.token.clone()),
    )
    .unwrap();
    w.created["$b"].clone()
}

/// Ruling final F2 (FW-UX1 D-KQ4CA): `rdy plan confirm --repin` takes the repin change's
/// justification as `rdy_plan` does, and both give the same refusals and the same plan: a stable
/// repin with no justification, and a justification without `--repin`, are refused; with
/// `--editorial` the repin lands its justification and the proposal is ready.
#[test]
fn rg_final_cli_plan_repin_justification_matches_mcp() {
    // the CLI, on a served project
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let b_cli = stable_world(&mut r, &writer());
    r.close().unwrap();
    let base = [
        "--json",
        "plan",
        "confirm",
        "--kind",
        "confirm",
        "--reason",
        "b follows a",
        "--view",
        "w",
    ];
    let with = |extra: &[&str]| -> Vec<String> {
        base.iter()
            .chain(extra)
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
    };
    let editorial = ["--editorial", "clarify", "b pins a's new text"];
    let mut cli_refusals = vec![];
    for args in [with(&["--repin"]), with(&editorial)] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (ok, v) = s.json(&args);
        assert!(!ok, "{args:?}: {v}");
        cli_refusals.push(json!([v["code"], v["message"], v["fix"]]));
    }
    let (_, untouched) = s.json(&["--json", "plan", "--view", "w"]);
    assert!(
        untouched["plan"].is_null(),
        "nothing written: {untouched:#}"
    );
    let args = with(&[&["--repin"][..], &editorial[..]].concat());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = s.rdy(&args[1..]);
    assert!(out.status.success(), "{out:?}");
    let (_, via_cli) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(via_cli["counts"]["open"], 0, "{via_cli:#}");
    let (ok, proposed) = s.json(&[
        "--json",
        "propose",
        "--title",
        "w",
        "--summary",
        "a moves to 60 days",
        "--view",
        "w",
    ]);
    assert!(ok, "{proposed}");

    // the tool, on a project of the same shape
    let (_dir, mut r) = project();
    let b_mcp = stable_world(&mut r, &as_agent("agent:qa", "w"));
    let engine = Arc::new(Local::new(r));
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "run": "w"})).unwrap(),
    )
    .unwrap();
    let group = json!({"group": {"kind": "confirm"}, "act": "confirm", "reason": "b follows a"});
    let spec = |extra: Value| {
        let mut g = group.clone();
        for (k, v) in extra.as_object().unwrap() {
            g[k] = v.clone();
        }
        json!({"view": "w", "answers": [g]})
    };
    let editorial = json!({"class": "clarify", "reason": "b pins a's new text"});
    let mut mcp_refusals = vec![];
    for args in [
        spec(json!({"repin": true})),
        spec(json!({"editorial": editorial})),
    ] {
        let e = rdy_plan(&mut st, serde_json::from_value(args).unwrap()).unwrap_err();
        mcp_refusals.push(json!([e.code, e.message, e.fix]));
    }
    // the same refusals; the block named is each project's own `b` (TASK-933)
    let expected = |b: &str| {
        vec![
            json!([
                "E_BAD_INPUT",
                format!("the repin changes stable block {b}, so it needs a justification"),
                "pass implements, editorial or reflects, as rdy_write takes"
            ]),
            json!([
                "E_BAD_INPUT",
                "a justification goes with repin",
                "set repin to true, or remove the justification"
            ]),
        ]
    };
    assert_eq!(cli_refusals, expected(&b_cli));
    assert_eq!(mcp_refusals, expected(&b_mcp));
    let via_mcp = rdy_plan(
        &mut st,
        serde_json::from_value(spec(json!({"repin": true, "editorial": editorial}))).unwrap(),
    )
    .unwrap_or_else(|e| panic!("rdy_plan: {e}"));
    assert_eq!(shape(&via_cli), shape(&via_mcp));
    let proposed_mcp = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"view": "w", "title": "w", "summary": "a moves to 60 days"}))
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("rdy_propose: {e}"));
    assert_eq!(proposed["ready"], json!(true), "{proposed:#}");
    assert_eq!(proposed_mcp["ready"], json!(true), "{proposed_mcp:#}");
}
