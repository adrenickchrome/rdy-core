//! port1 at the agent boundary: over the real stdio server, a read after a write sees it, and a
//! pipelined write and read are applied and answered in request order (tools on the blocking pool).
use rdy_qa::fixtures::project;
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::time::Duration;

#[test]
fn port1_a_read_after_a_write_sees_it_and_overview_counts_the_states_views() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
    )
    .ok();
    let w = c.call("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]})).ok();
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let read = c.call("rdy_read", json!({"id": id})).ok();
    assert!(read["text"].as_str().unwrap().contains(&id), "{read}");
    let o = c.call("rdy_overview", json!({})).ok();
    let views = o["views"].as_array().unwrap();
    assert_eq!(views.len(), 1, "{o}");
    assert_eq!(views[0]["name"], "r1");
    assert_eq!(views[0]["changes"], 1);
    assert_eq!(o["tree"].as_array().unwrap().len(), 1);
    // how fast a call is: the perf tier's `daemon_mcp_overview`
}

#[test]
fn port1_a_pipelined_write_then_read_is_answered_in_order_and_the_read_sees_the_write() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "r1", "role": "writer"}),
    )
    .ok();
    let w = c.next_id();
    c.send_raw(&json!({"jsonrpc": "2.0", "id": w, "method": "tools/call", "params": {"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "doc", "fields": {"name": "p", "title": "P", "doc_type": "page"}}]}}}).to_string());
    let o = c.next_id();
    c.send_raw(&json!({"jsonrpc": "2.0", "id": o, "method": "tools/call", "params": {"name": "rdy_overview", "arguments": {}}}).to_string());
    let first = c.recv_next(Duration::from_secs(30));
    assert_eq!(first["id"], w, "{first}");
    let second = c.recv_next(Duration::from_secs(30));
    assert_eq!(second["id"], o, "{second}");
    let body: serde_json::Value =
        serde_json::from_str(second["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["views"][0]["changes"], 1, "{body}");
}
