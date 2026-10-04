use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Bounded lifetime: never leave the server running if the test fails.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no text in {r}"));
        let v: Value = serde_json::from_str(text).unwrap();
        assert_ne!(r["result"]["isError"], json!(true), "{name} failed: {v}");
        v
    }
}

fn rdy(dir: &std::path::Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rdy")).arg("--json").args(args).current_dir(dir).env("RDY_ACTOR", "human:founder").output().unwrap();
    assert!(out.status.success(), "rdy {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn agent_over_mcp_then_human_approves() {
    let dir = tempfile::tempdir().unwrap();
    rdy(dir.path(), &["init"]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_rdy")).arg("mcp").current_dir(dir.path()).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut m = Mcp { child, out, next: 0 };
    let init = m.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = m.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for t in ["rdy_session", "rdy_write", "rdy_context", "rdy_propose", "rdy_decide"] {
        assert!(names.contains(&t), "missing tool {t}");
    }

    m.tool("rdy_session", json!({"actor": "agent:designer", "run": "run-1", "role": "writer"}));
    let res = m.tool("rdy_write", json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Wallets", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]}));
    let s = res["created"]["$s"].as_str().unwrap().to_string();
    let d = m.tool("rdy_decide", json!({"parent": s, "statement": "Wallets don't move", "authority_basis": "direct_ruling"}));
    let did = d["created"]["$decision"].as_str().unwrap().to_string();
    let bad = m.call("tools/call", json!({"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "claim", "parent": s, "body": "See {{t-ZZZZZ}}"}]}}));
    assert_eq!(bad["result"]["isError"], json!(true));
    assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains("E_UNKNOWN_REF"));
    let p = m.tool("rdy_propose", json!({"title": "wallet ruling"}));
    let pid = p["id"].as_str().unwrap().to_string();
    drop(m.child.stdin.take());
    m.child.wait().unwrap(); // releases the store lock

    let landed = rdy(dir.path(), &["approve", &pid]);
    assert!(landed["landing"].is_string());
    let show = rdy(dir.path(), &["show", &did]);
    assert_eq!(show["fields"]["status"], "accepted");
    let site = dir.path().join("site");
    rdy(dir.path(), &["render", "--out", site.to_str().unwrap()]);
    assert!(std::fs::read_to_string(site.join(format!("decision/{did}.html"))).unwrap().contains("Wallets don"));
    let ex = dir.path().join("export");
    assert_eq!(rdy(dir.path(), &["export", "--out", ex.to_str().unwrap()])["docs"], 1);
    assert_eq!(rdy(dir.path(), &["fsck"])["landings"], 1);
}
