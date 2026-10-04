//! An MCP stdio client for tests. The server is untrusted: every request has a deadline, and a
//! missing or late reply is a Critical failure (the server is killed and the test panics).
use crate::fixtures::rdy_bin;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub const DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum Reply {
    Ok(Value),
    ToolError(Value),
    RpcError(Value),
}

impl Reply {
    pub fn code(&self) -> Option<&str> {
        match self {
            Reply::ToolError(v) => v["code"].as_str(),
            _ => None,
        }
    }
    pub fn ok(self) -> Value {
        match self {
            Reply::Ok(v) => v,
            other => panic!("expected a result, got {other:?}"),
        }
    }
}

pub struct McpClient {
    child: Child,
    lines: Receiver<String>,
    pending: Vec<Value>,
    next: u64,
    calls: usize,
    latencies: Vec<(String, Duration)>,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpClient {
    pub fn start(project: &Path) -> McpClient {
        McpClient::start_with_env(project, &[])
    }

    pub fn start_with_env(project: &Path, env: &[(&str, &str)]) -> McpClient {
        let mut child = Command::new(rdy_bin())
            .args(["mcp", "--project"])
            .arg(project)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rdy mcp");
        let out = child.stdout.take().expect("stdout");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut c = McpClient {
            child,
            lines: rx,
            pending: vec![],
            next: 0,
            calls: 0,
            latencies: vec![],
        };
        let id = c.next_id();
        c.send(&json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "rdy-qa", "version": "0"}}}));
        c.recv(id, Duration::from_secs(10));
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    pub fn next_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn send(&mut self, v: &Value) {
        self.send_raw(&v.to_string());
    }

    pub fn send_raw(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin");
        if writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            self.critical("the server closed its input");
        }
    }

    fn critical(&mut self, what: &str) -> ! {
        let _ = self.child.kill();
        panic!("CRITICAL: {what}");
    }

    /// The raw JSON-RPC response with `id`, waiting at most `budget`.
    pub fn recv(&mut self, id: u64, budget: Duration) -> Value {
        if let Some(i) = self.pending.iter().position(|v| v["id"] == json!(id)) {
            return self.pending.remove(i);
        }
        let end = Instant::now() + budget;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(_) => self.critical(&format!("no reply to request {id} within {budget:?}")),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"] == json!(id) {
                return v;
            }
            if v.get("id").is_some() {
                self.pending.push(v);
            }
        }
    }

    pub fn call(&mut self, tool: &str, args: Value) -> Reply {
        self.call_within(tool, args, DEADLINE)
    }

    pub fn call_within(&mut self, tool: &str, args: Value, budget: Duration) -> Reply {
        let id = self.next_id();
        let t = Instant::now();
        self.calls += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
        let v = self.recv(id, budget);
        self.latencies.push((tool.to_string(), t.elapsed()));
        if v.get("error").is_some() {
            return Reply::RpcError(v["error"].clone());
        }
        let text = v["result"]["content"][0]["text"].as_str().unwrap_or("null");
        let body: Value = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
        if v["result"]["isError"] == json!(true) {
            Reply::ToolError(body)
        } else {
            Reply::Ok(body)
        }
    }

    pub fn tools(&mut self) -> Vec<Value> {
        let id = self.next_id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}}));
        self.recv(id, DEADLINE)["result"]["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The server still answers a normal read within the deadline.
    pub fn alive(&mut self) {
        match self.call("rdy_overview", json!({})) {
            Reply::Ok(_) | Reply::ToolError(_) => {}
            Reply::RpcError(e) => {
                self.critical(&format!("rdy_overview failed at the protocol level: {e}"))
            }
        }
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn latencies(&self) -> &[(String, Duration)] {
        &self.latencies
    }
}
