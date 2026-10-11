//! cc8: dismissing and restoring a passing mention over MCP (FW-CV4).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// A landed doc with a section and an entity "Ledger service" (alias ledger), served; an agent
/// session (run `a1`, so its view is `a1`). Returns (served, client, section).
fn mcp_project_with_mention() -> (Served, McpClient, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger service", "aliases": ["ledger"]}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a1", "role": "writer"}),
    )
    .ok();
    (s, c, w.created["$s"].clone())
}

/// A claim that names the entity without a link.
fn mention_write(section: &str) -> Value {
    json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "The ledger is append-only."}]})
}

/// A body edit of `block` that still names the entity.
fn touch(block: &str) -> Value {
    json!({"ops": [{"op": "update", "id": block, "body": "The ledger is append-only, always."}]})
}

#[test]
fn cc8_dismiss_then_restore_over_mcp_and_the_suggestion_comes_back() {
    let (_d, mut c, section) = mcp_project_with_mention();
    let w = c.call("rdy_write", mention_write(&section)).ok();
    let sg = &w["suggestions"][0];
    let (block, entity) = (
        sg["block"].as_str().unwrap().to_string(),
        sg["target"].as_str().unwrap().to_string(),
    );
    let v = c
        .call(
            "rdy_resolve",
            json!({"mention": {"block": block, "entity": entity}, "dismiss": {"reason": "everyday word"}}),
        )
        .ok();
    assert_eq!(v["status"], "dismissed", "{v}");
    assert_eq!(v["verdict"], "dismiss", "{v}");
    let w2 = c.call("rdy_write", touch(&block)).ok();
    assert!(
        w2["suggestions"]
            .as_array()
            .is_none_or(|a| a.iter().all(|s| s["target"] != entity.as_str())),
        "no suggestion for a dismissed pair: {w2}"
    );
    let v = c
        .call(
            "rdy_resolve",
            json!({"mention": {"block": block, "entity": entity}, "restore": {"reason": "it is the service"}}),
        )
        .ok();
    assert_eq!(v["status"], "open", "{v}");
    let w3 = c.call("rdy_write", touch(&block)).ok();
    assert!(
        w3["suggestions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["target"] == entity.as_str())),
        "the suggestion comes back after a restore: {w3}"
    );
}

#[test]
fn cc8_bad_combinations_name_their_fix() {
    let (_d, mut c, _s) = mcp_project_with_mention();
    for (p, msg) in [
        (
            json!({"dismiss": {"reason": "x"}}),
            "pass either tension or mention",
        ),
        (
            json!({"tension": "tn-000000", "mention": {"block": "c-AAAAA", "entity": "t-BBBBB"}, "dismiss": {"reason": "x"}}),
            "pass either tension or mention",
        ),
        (
            json!({"tension": "tn-000000", "restore": {"reason": "x"}}),
            "is not restored",
        ),
        (
            json!({"mention": {"block": "c-AAAAA", "entity": "t-BBBBB"}}),
            "pass dismiss: {reason} or restore: {reason}",
        ),
        (
            json!({"mention": {"block": "c-AAAAA", "entitty": "t-BBBBB"}, "dismiss": {"reason": "x"}}),
            "entitty",
        ),
    ] {
        let r = c.call("rdy_resolve", p.clone());
        assert!(!r.is_ok(), "{p}: {r:?}");
        let text = format!("{r:?}");
        assert!(text.contains(msg), "{p}: {text}");
    }
}
