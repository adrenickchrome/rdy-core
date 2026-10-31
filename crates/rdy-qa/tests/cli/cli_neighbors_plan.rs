//! nb8 (D-YXBXH, D-MPJM0; FW-UX1 D-KQ4CA "The CLI takes the same group fields"): `rdy plan
//! confirm --edge neighbor --reason …` closes every open neighbor item, as `rdy_plan` does with
//! `group: {edge: neighbor}`, and writes no repin; the version is then ready.
use rdy_engine::consequence::NEIGHBOR_EDGE;
use rdy_engine::engine::Local;
use rdy_engine::{Coverage, Rdy, Scope, Session, WriteRequest, parse_ops};
use rdy_mcp::tools::{McpState, rdy_plan, rdy_propose, rdy_session};
use rdy_model::{EditorialClass, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{as_agent, land, project, writer};
use serde_json::{Value, json};
use std::sync::Arc;

/// Canon: a stable doc whose section holds claims `a`, `c` and `d`, none citing another. The
/// view `w` then makes a briefed, justified edit of `a`'s text as `by`: a confirm item for each
/// neighbor. Returns (a, c, d).
fn world(r: &mut Rdy, by: &Session) -> (String, String, String) {
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
                    {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
                    {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "D stands."}
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
    (a, w.created["$c"].clone(), w.created["$x"].clone())
}

fn text_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

/// What two projects with the same shape must agree on: each item's kind, edge, state and
/// answer, and the counts. Block ids differ between projects, so the ids stay out.
fn shape(v: &Value) -> (Vec<String>, Value) {
    let mut rows: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!(
                "{} {} {} {} {}",
                i["kind"], i["edge"], i["state"], i["answer"]["act"], i["answer"]["reason"]
            )
        })
        .collect();
    rows.sort();
    (rows, v["counts"].clone())
}

/// The neighbor rows of a plan reply: (cause, subject, state), sorted.
fn neighbor_rows(v: &Value) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["edge"] == NEIGHBOR_EDGE && i["kind"] == "confirm")
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap().to_string();
            (s("cause"), s("subject"), s("state"))
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn nb8_group_neighbor_answers_close_without_repins() {
    // the CLI, on a served project
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (a, c, d) = world(&mut r, &writer());
    r.close().unwrap();
    let (_, read) = s.json(&["--json", "plan", "--view", "w"]);
    let rows = |state: &str| {
        let mut v = vec![
            (a.clone(), c.clone(), state.to_string()),
            (a.clone(), d.clone(), state.to_string()),
        ];
        v.sort();
        v
    };
    assert_eq!(neighbor_rows(&read), rows("open"), "{read:#}");
    let out = s.rdy(&[
        "plan",
        "confirm",
        "--edge",
        NEIGHBOR_EDGE,
        "--reason",
        "they still hold",
        "--view",
        "w",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text_of(&out), "answered group: confirm");
    let (_, via_cli) = s.json(&["--json", "plan", "--view", "w"]);
    assert_eq!(neighbor_rows(&via_cli), rows("closed"), "{via_cli:#}");
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
    assert_eq!(proposed["ready"], json!(true), "{proposed:#}");
    s.stop();
    // no repin change: the view holds the one edit, and the neighbors hold no pin
    let r = Rdy::open(s.dir.path()).unwrap();
    assert_eq!(r.read().included("w").len(), 1);
    let m = r.read().materialized(&Scope::View("w".into())).unwrap();
    for b in [&c, &d] {
        let blk = m.get(&rdy_model::BlockId::parse(b).unwrap()).unwrap();
        assert!(blk.pins.is_empty(), "{blk:#?}");
    }
    drop(m);
    r.close().unwrap();

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
    let via_mcp = rdy_plan(
        &mut st,
        serde_json::from_value(json!({"view": "w", "answers": [
            {"group": {"edge": NEIGHBOR_EDGE}, "act": "confirm", "reason": "they still hold"}]}))
        .unwrap(),
    )
    .unwrap_or_else(|e| panic!("rdy_plan: {e}"));
    assert_eq!(shape(&via_cli), shape(&via_mcp));
    let proposed_mcp = rdy_propose(
        &mut st,
        serde_json::from_value(json!({"view": "w", "title": "w", "summary": "a moves to 60 days"}))
            .unwrap(),
    )
    .unwrap_or_else(|e| panic!("rdy_propose: {e}"));
    assert_eq!(proposed_mcp["ready"], json!(true), "{proposed_mcp:#}");
}
