//! mint1: rdy_intent over MCP, intent on write tools, the schema row and the instruction line.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::intent_tools::{INTENT_DOC, rdy_intent};
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use serde_json::{from_value, json};
use std::sync::Arc;

fn state(r: Rdy, actor: &str, run: &str) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": actor, "run": run, "role": "writer"})).unwrap(),
    )
    .unwrap();
    st
}

#[test]
fn mint1_post_list_renew_withdraw_over_mcp() {
    let (_d, mut r) = qa_project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let mut st = state(r, "agent:a", "ra");
    let v = rdy_intent(
        &mut st,
        from_value(json!({"op": "post", "area": "payments", "angle": "fees", "ttl": "30m"}))
            .unwrap(),
    )
    .unwrap();
    let id = v["intent"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["intent"]["view"], "ra");
    let l = rdy_intent(&mut st, from_value(json!({"op": "list"})).unwrap()).unwrap();
    assert_eq!(l["intents"][0]["id"], id);
    let e = rdy_intent(
        &mut st,
        from_value(json!({"op": "post", "angle": "x"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    let e = rdy_intent(
        &mut st,
        from_value(json!({"op": "post", "area": "payments", "angle": "x", "ttl": "8d"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    let e = rdy_intent(&mut st, from_value(json!({"op": "lock"})).unwrap()).unwrap_err();
    assert!(e.fix.contains("post, renew, withdraw, list"), "{}", e.fix);
    rdy_intent(
        &mut st,
        from_value(json!({"op": "renew", "intent": id, "ttl": "1h"})).unwrap(),
    )
    .unwrap();
    let w = rdy_intent(
        &mut st,
        from_value(json!({"op": "withdraw", "intent": id})).unwrap(),
    )
    .unwrap();
    assert_eq!(w["intent"]["status"], "withdrawn");
    // rdy_write takes intent and records it
    let p = rdy_intent(
        &mut st,
        from_value(json!({"op": "post", "blocks": [claim], "angle": "wording"})).unwrap(),
    )
    .unwrap();
    let pid = p["intent"]["id"].as_str().unwrap();
    let base = st
        .engine
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&rdy_model::BlockId::parse(&claim).unwrap())
        .unwrap()
        .to_string();
    let out = rdy_write(&mut st, from_value(json!({"ops": [{"op": "update", "id": claim, "base": base, "body": "w"}], "intent": pid})).unwrap()).unwrap();
    let ch = st
        .engine
        .read()
        .reader()
        .change(&out["change"].as_str().unwrap().parse().unwrap())
        .unwrap();
    assert_eq!(
        rdy_engine::intent::intent_label(&ch.links.intent.unwrap()),
        pid
    );
    assert!(
        out.get("intents").is_none(),
        "own intents are not noted: {out}"
    );
}

#[test]
fn mint1_schema_row_doc_and_instruction() {
    let (_d, r) = project();
    let mut st = state(r, "agent:a", "ra");
    let s = rdy_schema(&mut st, from_value(json!({})).unwrap()).unwrap();
    assert_eq!(s["intents"], INTENT_DOC);
    assert!(
        s["errors"]["E_INTENT_EXPIRED"]["fix"]
            .as_str()
            .unwrap()
            .contains("post a new one")
    );
    assert!(ERROR_ROWS.iter().any(|(c, _, _)| *c == "E_INTENT_EXPIRED"));
    assert!(rdy_mcp::server::INSTRUCTIONS.contains("post an intent (rdy_intent)"));
}
