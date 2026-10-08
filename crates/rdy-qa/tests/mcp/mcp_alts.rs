//! malt1: rdy_alternatives over MCP, rdy_propose alternatives, and members seeing each other.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::alt_tools::{ALT_DOC, rdy_alternatives};
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};
use std::sync::Arc;

#[test]
fn malt1_members_see_each_other_and_propose_their_own_member() {
    let (_d, mut r) = project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    write_as(
        &mut r,
        &agent("agent:a", Some("a")),
        "a",
        json!([{"op": "update", "id": claim, "base": v, "body": "A"}]),
    )
    .unwrap();
    write_as(
        &mut r,
        &agent("agent:b", Some("b")),
        "b",
        json!([{"op": "update", "id": claim, "base": v, "body": "B"}]),
    )
    .unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:a", "run": "a", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let c = rdy_alternatives(&mut st, from_value(json!({"op": "create", "title": "Wording", "members": [{"label": "x", "view": "a"}, {"label": "y", "view": "b"}]})).unwrap()).unwrap();
    let set = c["set"]["id"].as_str().unwrap().to_string();
    let e = rdy_alternatives(
        &mut st,
        from_value(json!({"op": "remove", "set": set, "member": "y"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.fix.contains("abandon"), "{}", e.fix);
    let o = rdy_overview(&mut st, from_value(json!({})).unwrap()).unwrap();
    assert_eq!(o["alternatives"]["set"], set);
    assert_eq!(o["alternatives"]["members"][0]["mine"], true);
    assert_eq!(o["alternatives"]["members"][1]["view"], "b");
    let ctx = rdy_context(&mut st, from_value(json!({"id": claim})).unwrap()).unwrap();
    assert_eq!(
        ctx["alternatives"],
        json!([{"set": set, "label": "y", "view": "b"}])
    );
    let e = rdy_propose(&mut st, from_value(json!({"title": "x"})).unwrap()).unwrap_err();
    assert_eq!(e.code, E_ALT_UNCHOSEN);
    let e = rdy_propose(
        &mut st,
        from_value(json!({"title": "x", "alternatives": {"set": set, "chosen": "y"}})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER);
    let p = rdy_propose(
        &mut st,
        from_value(json!({"title": "x", "alternatives": {"set": set, "chosen": "x"}})).unwrap(),
    )
    .unwrap();
    assert_eq!(p["alternative"]["member"], "a");
    let cmp = rdy_alternatives(
        &mut st,
        from_value(json!({"op": "compare", "set": set})).unwrap(),
    )
    .unwrap();
    assert_eq!(cmp["members"].as_array().unwrap().len(), 2);
    let l = rdy_alternatives(&mut st, from_value(json!({"op": "list"})).unwrap()).unwrap();
    assert_eq!(l["sets"][0]["id"], set);
    let e = rdy_alternatives(&mut st, from_value(json!({"op": "merge"})).unwrap()).unwrap_err();
    assert!(
        e.fix
            .contains("create, add, remove, list, compare, abandon")
    );
}

#[test]
fn malt1_schema_rows_and_doc() {
    let (_d, r) = project();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:a", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let s = rdy_schema(&mut st, from_value(json!({})).unwrap()).unwrap();
    assert_eq!(s["alternatives"], ALT_DOC);
    for code in ["E_ALT_UNCHOSEN", "E_ALT_MEMBER"] {
        assert!(s["errors"][code]["fix"].is_string(), "{code}");
    }
}
