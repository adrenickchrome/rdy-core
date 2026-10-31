//! nb8 (D-YXBXH, D-MPJM0; FW-UX1 D-KQ4CA): one grouped answer `group: {edge: neighbor}` with a
//! reason closes every open neighbor item of a stable change through `rdy_plan`, and the version
//! lands. No repin is written and no neighbor changes. The tool runs in this process on a `Local`
//! engine.
use rdy_engine::consequence::NEIGHBOR_EDGE;
use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::{Rdy, Scope};
use rdy_mcp::tools::{McpState, rdy_plan, rdy_session};
use rdy_qa::fixtures::{approver, land, project, version_of};
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use serde_json::{Value, json};
use std::sync::Arc;

/// Canon: a stable doc whose section holds claims `a`, `c` and `d`, none citing another; then
/// view `w` (agent:w) edits `a`'s text. Returns (a, c, d).
pub(crate) fn neighbors_world(r: &mut Rdy) -> (String, String, String) {
    let w = put_j(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "D stands."}
        ]),
        j_typo(),
    );
    land(r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    let (a, c, d) = (get("$a"), get("$c"), get("$x"));
    put_j(
        r,
        "w",
        upd(&rdy_model::BlockId::parse(&a).unwrap(), "A holds 60 days."),
        j_typo(),
    );
    (a, c, d)
}

/// The neighbor rows of a plan reply: (cause, subject, kind, state), sorted.
pub(crate) fn neighbor_rows(v: &Value) -> Vec<(String, String, String, String)> {
    let mut rows: Vec<_> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["edge"] == NEIGHBOR_EDGE)
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap().to_string();
            (s("cause"), s("subject"), s("kind"), s("state"))
        })
        .collect();
    rows.sort();
    rows
}

fn agent_state(engine: &Arc<Local>) -> McpState {
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:w", "role": "writer", "run": "w"})).unwrap(),
    )
    .unwrap();
    st
}

#[test]
fn nb8_group_neighbor_answers_close_without_repins() {
    let (_dir, mut r) = project();
    let (a, c, d) = neighbors_world(&mut r);
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine);
    let mut run = |args: Value| {
        rdy_plan(&mut st, serde_json::from_value(args).unwrap())
            .unwrap_or_else(|e| panic!("rdy_plan: {e}"))
    };
    let read = run(json!({"view": "w"}));
    let open = |cause: &str, subject: &str, state: &str| {
        (
            cause.to_string(),
            subject.to_string(),
            "confirm".to_string(),
            state.to_string(),
        )
    };
    let mut want = vec![open(&a, &c, "open"), open(&a, &d, "open")];
    want.sort();
    assert_eq!(neighbor_rows(&read), want, "{read:#}");
    let w = Scope::View("w".into());
    let snapshot = |e: &Arc<Local>| {
        let r = e.read();
        let m = r.materialized(&w).unwrap();
        let pins: Vec<_> = [&c, &d]
            .iter()
            .map(|b| {
                m.get(&rdy_model::BlockId::parse(b).unwrap())
                    .unwrap()
                    .pins
                    .clone()
            })
            .collect();
        (r.included("w").len(), pins)
    };
    let before = snapshot(&engine);
    let versions = |e: &Arc<Local>, scope: &Scope| -> Vec<String> {
        let (c, d, scope) = (c.clone(), d.clone(), scope.clone());
        with_writer_result(&**e, move |r| {
            Ok(vec![version_of(r, &scope, &c), version_of(r, &scope, &d)])
        })
        .unwrap()
    };
    let canon_before = versions(&engine, &Scope::Canon);

    let after = run(json!({"view": "w", "answers": [
        {"group": {"edge": NEIGHBOR_EDGE}, "act": "confirm", "reason": "they still hold"}]}));
    let mut want = vec![open(&a, &c, "closed"), open(&a, &d, "closed")];
    want.sort();
    assert_eq!(neighbor_rows(&after), want, "{after:#}");
    for i in after["items"].as_array().unwrap() {
        assert_eq!(i["answer"]["act"], "confirm", "{i:#}");
        assert_eq!(i["answer"]["reason"], "they still hold");
    }
    assert_eq!(after["counts"]["open"], 0, "{after:#}");
    assert_eq!(snapshot(&engine), before, "no repin change, no pin moved");

    // the version is ready and lands; the neighbors keep their versions
    let landed = with_writer_result(&*engine, |r| {
        let p = r.propose(&ag("w"), "w", "w", Some(TEST_SUMMARY))?;
        assert!(p.ready, "{:#?}", p.issues);
        r.land(&approver(), &p.id)
    });
    assert!(landed.is_ok(), "{landed:?}");
    assert_eq!(versions(&engine, &Scope::Canon), canon_before);
}
