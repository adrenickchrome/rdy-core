//! rg11 (FW-UX1 D-KQ4CA; D-ME2HD "The CLI takes the same group fields and `repin` as flags of
//! `rdy plan`"): `rdy plan dismiss`, the group flags and `--repin` build the answers `rdy_plan`
//! takes, and the daemon runs the one function the tool runs.
use rdy_engine::Rdy;
use rdy_engine::Session;
use rdy_engine::engine::Local;
use rdy_mcp::tools::{McpState, rdy_plan, rdy_session};
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
