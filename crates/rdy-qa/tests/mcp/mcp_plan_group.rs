//! rg11 (FW-UX1 D-KQ4CA; D-ME2HD, D-MPJM0; O5, O13, O14): `rdy_plan` takes the `dismiss` act for
//! a tension item, answers a group of open items in place of one item, and repins the matched
//! subjects with `repin: true`. An unknown key in an answer or its group is refused with a
//! did-you-mean. The tools run in this process on a `Local` engine.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::{Rdy, Scope};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{McpState, PlanParams, rdy_plan, rdy_session};
use rdy_model::BlockId;
use rdy_qa::edit_loop::write_cited_as;
use rdy_qa::fixtures::{as_agent, land, project, write};
use serde_json::{Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: draft doc `notes` with claim `a` and claim `b`, which cites `a`; doc `old`, deprecated,
/// with claim `x`. Nothing is open until the view `w` changes something.
struct Fx {
    a: String,
    b: String,
    x: String,
    n: String,
}

fn canon(r: &mut Rdy) -> Fx {
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
    Fx {
        a: get("$a"),
        b: get("$b"),
        x: get("$x"),
        n: get("$n"),
    }
}

fn agent_state(engine: &Arc<Local>) -> McpState {
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "role": "writer", "run": "w"})),
    )
    .unwrap();
    st
}

fn edit(r: &mut Rdy, ops: Value) {
    write_cited_as(r, &as_agent("agent:qa", "w"), "w", ops, None)
        .unwrap_or_else(|e| panic!("write to w: {e}"));
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
fn rg11_mcp_plan_dismisses_a_tension_item_by_its_tn_id() {
    let (_dir, mut r) = project();
    let f = canon(&mut r);
    edit(
        &mut r,
        json!([{"op": "create", "type": "claim", "id": "$y", "parent": f.n,
            "body": format!("Y follows {{{{{}}}}}.", f.x)}]),
    );
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine);

    let read = plan(&mut st, json!({"view": "w"}));
    let tension = items_of(&read, "tension");
    assert_eq!(tension.len(), 1, "{read:#}");
    let tn = tension[0]["id"].as_str().unwrap().to_string();
    assert!(tn.starts_with("tn-") && tn.len() == 15, "{tn}");
    assert_eq!(tension[0]["state"], "open");

    // a tension item takes a reason
    let e = rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [{"item": tn, "act": "dismiss"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(e.message, "dismiss needs a reason of 1 to 2000 characters");

    let after = plan(
        &mut st,
        json!({"view": "w", "answers": [
            {"item": tn, "act": "dismiss", "reason": "Y cites it as history"}]}),
    );
    let row = items_of(&after, "tension")[0];
    assert_eq!(row["state"], "closed", "{after:#}");
    assert_eq!(row["answer"]["act"], "dismiss");
    assert_eq!(row["answer"]["reason"], "Y cites it as history");
    assert_eq!(
        after["counts"]["by_kind"]["tension"],
        json!({"open": 0, "closed": 1, "deferred": 0})
    );
}

#[test]
fn rg11_mcp_plan_answers_a_group_and_repins() {
    let (_dir, mut r) = project();
    let f = canon(&mut r);
    edit(
        &mut r,
        json!([{"op": "update", "id": f.a, "body": "A holds 60 days."}]),
    );
    let engine = Arc::new(Local::new(r));
    let mut st = agent_state(&engine);

    let read = plan(&mut st, json!({"view": "w"}));
    let confirm = items_of(&read, "confirm");
    assert_eq!(confirm.len(), 1, "{read:#}");
    assert_eq!(confirm[0]["subject"], f.b.as_str());
    let changes = engine.read().included("w").len();
    let pinned_current = |engine: &Arc<Local>| -> bool {
        let (a, b) = (f.a.clone(), f.b.clone());
        engine
            .write(Box::new(move |r| {
                let tracking = r.store().tracking().clone();
                let st = r.read();
                let m = st.materialized(&Scope::View("w".into())).unwrap();
                let now = m.get(&id(&a)).unwrap().content_hash_with(&tracking);
                let pin = m.get(&id(&b)).unwrap().pins.get(&id(&a)).copied();
                Ok(json!(pin == Some(now)))
            }))
            .unwrap()
            .as_bool()
            .unwrap()
    };
    assert!(!pinned_current(&engine), "b starts with a stale pin");

    // a group names no item; repin goes with a confirm group
    let e = rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [
            {"group": {"kind": "confirm"}, "act": "defer", "reason": "later", "repin": true}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(e.message, "repin goes with a confirm group");
    assert_eq!(
        engine.read().included("w").len(),
        changes,
        "nothing written"
    );

    let after = plan(
        &mut st,
        json!({"view": "w", "answers": [
            {"group": {"kind": "confirm"}, "act": "confirm", "reason": "b follows a", "repin": true}]}),
    );
    let row = items_of(&after, "confirm")[0];
    assert_eq!(row["state"], "closed", "{after:#}");
    assert_eq!(row["answer"]["reason"], "b follows a");
    assert_eq!(
        engine.read().included("w").len(),
        changes + 1,
        "one change repins every matched subject"
    );
    assert!(pinned_current(&engine), "b pins a's current version");
}

#[test]
fn rg11_mcp_plan_refuses_an_unknown_group_key_with_did_you_mean() {
    for (args, path, near) in [
        (
            json!({"answers": [{"group": {"knd": "confirm"}, "act": "confirm", "reason": "r"}]}),
            "knd",
            "kind",
        ),
        (
            json!({"answers": [{"grup": {"kind": "confirm"}, "act": "confirm", "reason": "r"}]}),
            "grup",
            "group",
        ),
        (
            json!({"answers": [{"group": {"kind": "confirm"}, "act": "confirm", "reason": "r", "repn": true}]}),
            "repn",
            "repin",
        ),
    ] {
        let e =
            parse_params::<PlanParams>("rdy_plan", args.as_object().unwrap().clone()).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert!(e.message.contains(path), "{e}");
        assert!(
            e.fix.contains(&format!("did you mean \"{near}\"")),
            "{path}: {e}"
        );
    }
    // dismiss is an act of rdy_propose's plan_answers too
    let ok = parse_params::<rdy_mcp::tools::ProposeParams>(
        "rdy_propose",
        json!({"view": "w", "plan_answers": [{"item": "tn-0123456789ab", "act": "dismiss", "reason": "r"}]})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(ok.is_ok(), "{ok:?}");
}
