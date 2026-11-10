//! kfu3: the tool table and the router agree. Every registered tool has exactly one policy row,
//! and only the two session tools are exempt (FW-MH.kungfu, D-Y18E1).
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_engine::kungfu::{self, Response};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{RdyServer, serve};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The names the project server lists, sorted.
async fn registered() -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let server = RdyServer::new(std::sync::Arc::new(Local::new(rdy)), None, NotifyHub::new());
    let (client, end) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(end);
    tokio::spawn(serve(server, sr, sw));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    for l in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "kfu3", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    ] {
        w.write_all(format!("{l}\n").as_bytes()).await.unwrap();
    }
    loop {
        let mut line = String::new();
        assert!(
            r.read_line(&mut line).await.unwrap() > 0,
            "the server closed"
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["id"] == json!(2) {
            let mut names: Vec<String> = v["result"]["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect();
            names.sort();
            return names;
        }
    }
}

#[tokio::test]
async fn kfu3_every_registered_tool_has_one_policy() {
    let names = registered().await;
    assert_eq!(names.len(), 61);
    let unique: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(unique.len(), 61, "a tool is listed twice");
    let rows: BTreeSet<&str> = kungfu::TOOLS.iter().map(|p| p.tool).collect();
    assert_eq!(rows.len(), kungfu::TOOLS.len(), "a tool has two rows");
    assert_eq!(
        unique, rows,
        "the router and the table list different tools"
    );
    for name in &names {
        let p = kungfu::policy(name).unwrap_or_else(|| panic!("{name} has no row"));
        assert_eq!(p.response, Response::Warn, "{name}");
    }
    let exempt: Vec<&str> = kungfu::TOOLS
        .iter()
        .filter(|p| p.ruleset.is_none())
        .map(|p| p.tool)
        .collect();
    assert_eq!(exempt, ["rdy_instructions", "rdy_session"]);
    let owners: BTreeSet<&str> = kungfu::TOOLS.iter().filter_map(|p| p.ruleset).collect();
    assert_eq!(owners.len(), 9);
}
