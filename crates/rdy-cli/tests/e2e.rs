use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

struct Mcp {
    child: std::process::Child,
    lines: Receiver<String>,
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
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|e| {
                    panic!("no response to {method} within 30s (or server closed stdout): {e}")
                });
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {r}"));
        let v: Value = serde_json::from_str(text).unwrap();
        assert_ne!(r["result"]["isError"], json!(true), "{name} failed: {v}");
        v
    }
}

/// A user config naming `human:founder` as the human actor.
fn human_config() -> tempfile::TempDir {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join("rdy")).unwrap();
    std::fs::write(cfg.path().join("rdy/config.yaml"), "actor: human:founder\n").unwrap();
    cfg
}

/// `rdy --no-daemon --json <args>` in `dir` as the human `human:founder`.
fn rdy(dir: &std::path::Path, args: &[&str]) -> Value {
    let cfg = human_config();
    let out = Command::new(env!("CARGO_BIN_EXE_rdy"))
        .args(["--no-daemon", "--json"])
        .args(args)
        .current_dir(dir)
        .env("RDY_ACTOR", "human:founder")
        .env("XDG_CONFIG_HOME", cfg.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn agent_over_mcp_then_human_approves() {
    let dir = tempfile::tempdir().unwrap();
    rdy(dir.path(), &["init"]);

    let cfg = human_config();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdy"))
        .args(["--no-daemon", "mcp"])
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", cfg.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut m = Mcp {
        child,
        lines,
        next: 0,
    };
    let init = m.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "rdy", "{init}");
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{init}"
    );
    m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = m.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "rdy_session",
        "rdy_write",
        "rdy_context",
        "rdy_propose",
        "rdy_decide",
    ] {
        assert!(names.contains(&t), "missing tool {t}");
    }

    m.tool(
        "rdy_session",
        json!({"actor": "agent:designer", "run": "run-1", "role": "writer"}),
    );
    let res = m.tool("rdy_write", json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Wallets", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]}));
    let s = res["created"]["$s"].as_str().unwrap().to_string();
    let d = m.tool(
        "rdy_decide",
        json!({"parent": s, "statement": "Wallets don't move", "authority_basis": "direct_ruling"}),
    );
    let did = d["created"]["$decision"].as_str().unwrap().to_string();
    let bad = m.call("tools/call", json!({"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "claim", "parent": s, "body": "See {{t-ZZZZZ}}"}]}}));
    assert_eq!(bad["result"]["isError"], json!(true));
    assert!(
        bad["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("E_UNKNOWN_REF")
    );
    let p = m.tool("rdy_propose", json!({"title": "wallet ruling"}));
    let pid = p["id"].as_str().unwrap().to_string();
    drop(m.child.stdin.take());
    // Wait for exit (releases the store lock), bounded.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if m.child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = m.child.kill();
            panic!("rdy mcp did not exit within 10s after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let landed = rdy(dir.path(), &["approve", &pid]);
    assert!(landed["landing"].is_string());
    let show = rdy(dir.path(), &["show", &did]);
    assert_eq!(show["fields"]["status"], "accepted");
    let site = dir.path().join("site");
    rdy(dir.path(), &["render", "--out", site.to_str().unwrap()]);
    // a decision has no page of its own: its permalink leads to the text it sits in
    assert!(!site.join("_decision").exists());
    assert!(
        std::fs::read_to_string(site.join(format!("go/{did}.html")))
            .unwrap()
            .contains("url=../wallets.html#decisions")
    );
    let ex = dir.path().join("export");
    assert_eq!(
        rdy(dir.path(), &["export", "--out", ex.to_str().unwrap()])["docs"],
        1
    );
    assert_eq!(rdy(dir.path(), &["fsck"])["landings"], 1);
}
