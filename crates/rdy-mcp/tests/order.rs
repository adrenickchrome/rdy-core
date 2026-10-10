//! ord1: an MCP session's request order is stamped at the transport, so pipelined tool calls
//! apply, and are answered, in arrival order on any runtime (DECISIONS §8: explicit order, not
//! scheduler order).
use rdy_engine::Rdy;
use rdy_engine::engine::{Engine, Local};
use rdy_mcp::server::serve_on;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn send(w: &mut (impl AsyncWriteExt + Unpin), v: Value) {
    let mut line = v.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await.unwrap();
}

async fn recv(r: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(30), r.read_line(&mut line))
        .await
        .expect("a reply within 30 s")
        .unwrap();
    assert!(n > 0, "the session closed");
    serde_json::from_str(&line).unwrap()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// The daemon serves sessions on its multi-thread runtime; this runs `serve_on` on one, with
/// several sessions at once, so tasks are spread over the workers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ord1_pipelined_calls_on_a_multi_thread_runtime_apply_and_answer_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let engine: Arc<dyn Engine> =
        Arc::new(Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap()));
    let mut sessions = vec![];
    for s in 0..3u64 {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server);
        tokio::spawn(serve_on(Arc::clone(&engine), s + 1, sr, sw));
        let engine_dir = format!("o{s}");
        sessions.push(tokio::spawn(async move {
            let (cr, mut cw) = tokio::io::split(client);
            let mut cr = BufReader::new(cr);
            send(&mut cw, json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ord1", "version": "0"}}})).await;
            assert_eq!(recv(&mut cr).await["id"], 0);
            send(
                &mut cw,
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            )
            .await;
            send(
                &mut cw,
                call(
                    1,
                    "rdy_session",
                    json!({"actor": format!("agent:{engine_dir}"), "run": engine_dir, "role": "writer"}),
                ),
            )
            .await;
            assert_eq!(recv(&mut cr).await["id"], 1);
            // 100 write/read pairs, all sent before any reply is read
            for i in 0..100u64 {
                send(&mut cw, call(10 + 2 * i, "rdy_write", json!({"ops": [{"op": "create", "type": "doc", "fields": {"name": format!("n{i:03}"), "title": "N", "doc_type": "glossary"}}]}))).await;
                send(&mut cw, call(11 + 2 * i, "rdy_overview", json!({}))).await;
            }
            for i in 0..100u64 {
                let w = recv(&mut cr).await;
                assert_eq!(w["id"], 10 + 2 * i, "reply {i}: the write's: {w}");
                let r = recv(&mut cr).await;
                assert_eq!(r["id"], 11 + 2 * i, "reply {i}: the read's: {r}");
                let o: Value =
                    serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap())
                        .unwrap();
                let changes = o["views"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["name"] == engine_dir.as_str())
                    .map(|v| v["changes"].as_u64().unwrap())
                    .unwrap_or(0);
                assert_eq!(changes, i + 1, "read {i} did not see the writes before it");
            }
        }));
    }
    for s in sessions {
        s.await.unwrap();
    }
}

#[tokio::test]
async fn hd4_serve_on_answers_an_oversized_line_with_the_limit_error_and_lives() {
    let dir = tempfile::tempdir().unwrap();
    let engine: Arc<dyn Engine> =
        Arc::new(Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap()));
    let (client, server) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server);
    tokio::spawn(serve_on(engine, 1, sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let mut cr = BufReader::new(cr);
    send(&mut cw, json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "hd4", "version": "0"}}})).await;
    assert_eq!(recv(&mut cr).await["id"], 0);
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    send(
        &mut cw,
        call(
            1,
            "rdy_session",
            json!({"actor": "agent:hd4", "run": "hd4", "role": "writer"}),
        ),
    )
    .await;
    assert_eq!(recv(&mut cr).await["id"], 1);
    let big = "x".repeat(rdy_mcp::tools::MAX_REQUEST_BYTES + 1024);
    send(
        &mut cw,
        call(9, "rdy_write", json!({"message": big, "ops": []})),
    )
    .await;
    let reply = recv(&mut cr).await;
    assert_eq!(reply["id"], 9, "{reply}");
    let text = reply.to_string();
    assert!(
        text.contains("rdy_request_too_large") && text.contains("E_LIMIT"),
        "{text}"
    );
    send(&mut cw, call(10, "rdy_overview", json!({}))).await;
    assert_eq!(recv(&mut cr).await["id"], 10, "the session lives");
}
