//! dhome1, ndh2: over MCP, rdy_decide and rdy_revoke without parent record in the decision home;
//! rdy_schema names the home and the rule; with no home, a decision with no parent sits at the
//! project root (D-P3YD5).
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
    let cfg = dir.path().join(".rdy.yaml");
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

/// The tool descriptions' sentence on an omitted parent (D-P3YD5).
const OMIT_PARENT: &str = "Omit parent to record it in the decision home, or at the project root when the project names none.";

#[test]
fn ndh2_mcp_decide_supersede_and_revoke_without_home_record_at_the_root() {
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
    let at_root = |c: &mut McpClient, id: &str| {
        let ctx = c.call("rdy_context", json!({"id": id})).ok();
        assert_eq!(ctx["doc"], json!(null), "{ctx}");
        assert_eq!(ctx["parent"], json!(null), "{ctx}");
    };
    let d = c
        .call("rdy_decide", json!({"statement": "Search is semantic"}))
        .ok();
    let first = d["created"]["$decision"].as_str().unwrap().to_string();
    at_root(&mut c, &first);
    let s = c
        .call(
            "rdy_supersede",
            json!({"old": first, "decision": {"statement": "Search is literal by default"}}),
        )
        .ok();
    let second = s["created"]["$decision"].as_str().unwrap().to_string();
    at_root(&mut c, &second);
    let v = c
        .call(
            "rdy_revoke",
            json!({"target": second, "reason": "search is deterministic now"}),
        )
        .ok();
    let third = v["created"]["$decision"].as_str().unwrap().to_string();
    at_root(&mut c, &third);
    let schema = c.call("rdy_schema", json!({})).ok();
    assert_eq!(
        schema["decisions"],
        json!({"home": null, "rule": DECISIONS_RULE})
    );
    assert!(
        DECISIONS_RULE.contains(
            "With no home configured, a decision with no parent sits at the project root."
        )
    );
    let tools = c.tools();
    for name in ["rdy_decide", "rdy_revoke"] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert!(
            t["description"].as_str().unwrap().contains(OMIT_PARENT),
            "{name}: {t}"
        );
    }
    c.alive();
}
