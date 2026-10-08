//! dhome1: over MCP, rdy_decide and rdy_revoke without parent record in the decision home;
//! rdy_schema names the home and the rule; the server instructions carry the rule; with no home,
//! rdy_decide without parent is E_BAD_INPUT.
use rdy_mcp::server::INSTRUCTIONS;
use rdy_mcp::tools::DECISIONS_RULE;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

#[test]
fn dhome1_rdy_decide_and_rdy_revoke_without_parent_record_in_the_home() {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "decisions", "title": "Decisions"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "register", "title": "Register", "doc_type": "overview"}}
    ]))
    .unwrap();
    let reg = w.created["$d"].clone();
    // a landed decision for rdy_revoke to revoke (revoking a still-proposed one is not this test)
    let old = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: reg.clone(),
                statement: "Search is semantic".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    drop(r);
    let cfg = dir.path().join(".rdy/config.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "decisions:\n  home: decisions/register\n";
    std::fs::write(&cfg, text).unwrap();
    let mut c = McpClient::start(dir.path());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:h", "run": "h1", "role": "writer"}),
    );
    assert!(s.code().is_none(), "{s:?}");
    let d = c
        .call(
            "rdy_decide",
            json!({"statement": "Search is literal by default"}),
        )
        .ok();
    let id = d["created"]["$decision"].as_str().unwrap().to_string();
    assert_eq!(c.call("rdy_context", json!({"id": id})).ok()["parent"], reg);
    let v = c
        .call(
            "rdy_revoke",
            json!({"target": old, "reason": "search is deterministic now"}),
        )
        .ok();
    let rid = v["created"]["$decision"].as_str().unwrap().to_string();
    assert_eq!(
        c.call("rdy_context", json!({"id": rid})).ok()["parent"],
        reg
    );
    let schema = c.call("rdy_schema", json!({})).ok();
    assert_eq!(
        schema["decisions"],
        json!({"home": "decisions/register", "rule": DECISIONS_RULE})
    );
    c.alive();
}

#[test]
fn dhome1_without_a_home_rdy_decide_needs_a_parent_and_the_instructions_say_where() {
    assert!(INSTRUCTIONS.contains(
        "Record decisions in the project's decision home (rdy_decide without parent records there; rdy_schema names it) and link content to them with decided_by; don't put decisions inside content docs."
    ));
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:h", "run": "h2", "role": "writer"})
        )
        .code()
        .is_none()
    );
    let e = c.call("rdy_decide", json!({"statement": "x"}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"), "{e:?}");
    assert_eq!(
        c.call("rdy_schema", json!({})).ok()["decisions"]["home"],
        json!(null)
    );
    c.alive();
}
