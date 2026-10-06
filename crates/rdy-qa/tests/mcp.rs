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
#[should_panic(expected = "no reply to request")]
fn a_missing_reply_is_critical() {
    let (_d, mut c) = server();
    // a notification has no reply: waiting for one must trip the deadline, not hang
    c.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let id = c.next_id();
    c.recv(id, std::time::Duration::from_millis(300));
}

#[test]
#[should_panic(expected = "did not accept")]
fn a_stalled_write_is_critical() {
    let (_d, mut c) = server();
    // 64 MiB cannot be accepted within a 1 ms budget: the send must trip its deadline, not block
    let big = "x".repeat(64 << 20);
    c.send_raw_within(&big, std::time::Duration::from_millis(1));
}

use std::time::Duration;

fn session(c: &mut McpClient) {
    c.call(
        "rdy_session",
        json!({"actor": "agent:qa", "run": "qa", "role": "writer"}),
    )
    .ok();
}

#[test]
fn hook1_the_release_binary_has_no_test_hook() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start_with_env(dir.path(), &[("RDY_TEST_HOOKS", "1")]);
    let reply = c.call(
        "rdy_session",
        json!({"actor": "__rdy_test_panic__", "role": "writer"}),
    );
    assert!(
        reply.code().is_none(),
        "the magic actor is an ordinary actor in a release build: {reply:?}"
    );
    c.alive();
    let bin = std::fs::read(rdy_bin()).unwrap();
    let marker = b"__rdy_test_panic__";
    assert!(
        !bin.windows(marker.len()).any(|w| w == marker),
        "the hook's marker is not in the binary"
    );
}

#[test]
fn mcp2_write_ops_and_max_tokens_are_limited() {
    let (_d, mut c) = server();
    session(&mut c);
    let doc = c.call("rdy_write", json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Big", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ]})).ok();
    let s = doc["created"]["$s"].as_str().unwrap().to_string();
    let op = |i: usize| json!({"op": "create", "type": "claim", "parent": s, "body": format!("claim {i}")});
    let over: Vec<_> = (0..5_001).map(op).collect();
    assert_eq!(
        c.call_within("rdy_write", json!({"ops": over}), Duration::from_secs(5))
            .code(),
        Some("E_LIMIT")
    );
    // the largest allowed write finishes within its named budget
    let max: Vec<_> = (0..5_000).map(op).collect();
    assert!(matches!(
        c.call_within("rdy_write", json!({"ops": max}), Duration::from_secs(5)),
        Reply::Ok(_)
    ));
    assert_eq!(
        c.call("rdy_overview", json!({"max_tokens": 100_001}))
            .code(),
        Some("E_LIMIT")
    );
    let limits = c.call("rdy_schema", json!({})).ok();
    assert_eq!(limits["limits"]["ops_per_write"], 5_000);
    assert_eq!(limits["limits"]["asset_bytes"], 64 * 1024 * 1024);
    // over stdio the 16 MiB request line binds first: the largest raw asset whose base64 fits
    assert_eq!(limits["limits"]["asset_bytes_stdio"], 12_579_840);
    c.alive();
}

#[test]
fn mcp2_an_oversized_request_gets_a_reply_and_the_server_lives() {
    let (_d, mut c) = server();
    session(&mut c);
    let id = c.next_id();
    let big = "x".repeat(17 * 1024 * 1024);
    // a 17 MiB line needs more than the default 2 s write deadline; 10 s is its named budget
    c.send_raw_within(
        &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"rdy_write","arguments":{{"message":"{big}","ops":[]}}}}}}"#),
        Duration::from_secs(10),
    );
    let reply = c.recv(id, Duration::from_secs(10));
    assert!(
        reply.to_string().contains("rdy_request_too_large"),
        "{reply}"
    );
    c.alive();
}

#[test]
fn mcp2_an_oversized_request_with_the_id_last_gets_a_reply() {
    // the TypeScript SDK writes `id` after `params`, so it sits at the end of a huge line
    let (_d, mut c) = server();
    session(&mut c);
    let id = c.next_id();
    let big = "x".repeat(17 * 1024 * 1024);
    c.send_raw_within(
        &format!(r#"{{"jsonrpc":"2.0","method":"tools/call","params":{{"name":"rdy_write","arguments":{{"message":"{big}","ops":[{{"op":"edit","id":"c-ABCDE","body":"b"}}]}}}},"id":{id}}}"#),
        Duration::from_secs(10),
    );
    let reply = c.recv(id, Duration::from_secs(10));
    assert!(
        reply.to_string().contains("rdy_request_too_large"),
        "{reply}"
    );
    c.alive();
}

#[test]
fn mcp2_the_asset_limit_is_checked_before_decoding() {
    let (dir, r) = project();
    let mut st =
        rdy_mcp::tools::McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let huge = "A".repeat(rdy_mcp::tools::MAX_ASSET_BYTES / 3 * 4 + 8);
    let e = rdy_mcp::tools::rdy_asset(
        &mut st,
        serde_json::from_value(json!({"data_base64": huge})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    drop(dir);
}

#[test]
fn mcp_every_tool_survives_hostile_params_within_the_deadline() {
    let (_d, mut c) = server();
    session(&mut c);
    let lookalikes = [
        "с-ABCDE",
        "c-ABCDE ",
        " c-ABCDE",
        "c‐ABCDE",
        "C-ABCDE",
        "c-abcde",
        "c-ABCDＥ",
        "../../etc",
        "",
        "\u{0}",
        "c-ABCDE\u{200b}",
    ];
    let hash_like = [
        "A".repeat(64),
        "a".repeat(63),
        format!(" {}", "a".repeat(64)),
        "0x".to_string() + &"a".repeat(64),
        "g".repeat(64),
    ];
    let deep_json = (0..100).fold(json!(1), |acc, _| json!([acc]));
    let mut variants: Vec<serde_json::Value> =
        vec![json!({}), json!(null), json!([]), json!("str"), json!(42)];
    for tool in c.tools() {
        let name = tool["name"].as_str().unwrap().to_string();
        let props = tool["inputSchema"]["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut cases: Vec<serde_json::Value> = variants.clone();
        for (p, _) in &props {
            for v in [
                json!(null),
                json!(1),
                json!(true),
                json!({"x": 1}),
                json!(["a"]),
                json!("x".repeat(1 << 20)),
                deep_json.clone(),
            ] {
                cases.push(json!({ p: v }));
            }
            for l in lookalikes {
                cases.push(json!({ p: l }));
            }
            for h in &hash_like {
                cases.push(json!({ p: h, "id": "c-ABCDE" }));
            }
        }
        for (i, args) in cases.into_iter().enumerate() {
            // any reply is fine; no reply within the deadline is Critical
            let _ = c.call(&name, args);
            if i % 10 == 0 {
                c.alive();
            }
        }
        c.alive();
    }
    variants.clear();
    eprintln!("mcp sweep: {} calls", c.calls());
    assert!(c.calls() > 100, "the sweep ran ({} calls)", c.calls());
}

#[test]
fn mcp_pipelined_requests_are_all_answered_in_time() {
    let (_d, mut c) = server();
    session(&mut c);
    let ids: Vec<u64> = (0..50).map(|_| {
        let id = c.next_id();
        c.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "rdy_overview", "arguments": {}}}).to_string());
        id
    }).collect();
    let start = std::time::Instant::now();
    for (n, id) in ids.iter().enumerate() {
        let v = c.recv_next(
            Duration::from_secs(5)
                .saturating_sub(start.elapsed())
                .max(Duration::from_millis(1)),
        );
        assert_eq!(v["id"], *id, "reply {n} out of order: {v}");
    }
}
