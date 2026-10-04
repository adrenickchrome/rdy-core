use rdy_qa::fixtures::*;
use rdy_qa::mcp::*;
use serde_json::json;

fn server() -> (tempfile::TempDir, McpClient) {
    let (dir, r) = project();
    drop(r); // release the store lock for the server
    let c = McpClient::start(dir.path());
    (dir, c)
}

#[test]
fn the_client_talks_to_a_real_server_within_deadlines() {
    let (_d, mut c) = server();
    let names: Vec<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"rdy_write".to_string()), "{names:?}");
    assert!(
        matches!(c.call("rdy_overview", json!({})), Reply::ToolError(_)),
        "needs a session"
    );
    assert!(matches!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "role": "writer"})
        ),
        Reply::Ok(_)
    ));
    c.alive();
    assert!(c.calls() >= 3);
    assert!(c.latencies().iter().all(|(_, d)| *d < DEADLINE));
}

#[test]
#[should_panic(expected = "CRITICAL")]
fn a_missing_reply_is_critical() {
    let (_d, mut c) = server();
    // a notification has no reply: waiting for one must trip the deadline, not hang
    c.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let id = c.next_id();
    c.recv(id, std::time::Duration::from_millis(300));
}
