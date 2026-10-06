//! conc1 (spec 15.3 area daemon): real concurrent clients over the socket; read-your-writes across
//! processes; reads never wait for a write; per-connection order holds under the pool; the lock
//! between direct mode and the daemon is clean.
use rdy_daemon::{Attempt, Hello, PROTO_VERSION, Proto, RunDir, connect};
use rdy_engine::{E_INTERNAL, E_LOCKED};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::deep;
use rdy_qa::mcp::McpClient;
use serde_json::json;
use std::time::{Duration, Instant};

/// Each call's budget. These tests check what lands and in what order, not latency (that is
/// the perf tier's): on ext4 one `fdatasync` stalls for hundreds of ms while the other tests in
/// this binary flush, and eight agents queue their writes behind the one writer, so the default
/// 2 s budget failed `conc1_eight...` on the Linux bench with no defect behind it.
const CALL_BUDGET: Duration = Duration::from_secs(30);

#[test]
fn conc1_eight_mcp_proxies_and_a_cli_hammer_one_daemon() {
    let s = Served::new();
    let per = if deep() { 120 } else { 40 };
    let agents: Vec<_> = (0..8)
        .map(|a| {
            let mut c = McpClient::start_proxy(&s);
            std::thread::spawn(move || {
                let r = c.call_within("rdy_session", json!({"actor": format!("agent:a{a}"), "role": "writer"}), CALL_BUDGET);
                assert!(r.code().is_none());
                for i in 0..per {
                    let w = c.call_within("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "fields": {"name": format!("a{a}-{i}"), "title": "A", "doc_type": "glossary"}}]}), CALL_BUDGET);
                    assert!(w.code().is_none(), "agent {a} write {i}: {:?}", w.ok());
                    if i % 5 == 0 {
                        let o = c.call_within("rdy_overview", json!({}), CALL_BUDGET);
                        assert!(o.code().is_none());
                    }
                }
                c.calls()
            })
        })
        .collect();
    let cli = {
        let s_dir = s.dir.path().to_path_buf();
        let env = s.env_pairs();
        std::thread::spawn(move || {
            for i in 0..per {
                let out = std::process::Command::new(rdy_qa::fixtures::rdy_bin())
                    .args(["--project"])
                    .arg(&s_dir)
                    .args(["--json", "new", "doc", &format!("h{i}")])
                    .envs(env.clone())
                    .env_remove("RDY_ACTOR")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "cli write {i}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let out = std::process::Command::new(rdy_qa::fixtures::rdy_bin())
                    .args(["--project"])
                    .arg(&s_dir)
                    .args(["--json", "status"])
                    .envs(env.clone())
                    .env_remove("RDY_ACTOR")
                    .output()
                    .unwrap();
                assert!(out.status.success());
            }
        })
    };
    let calls: usize = agents.into_iter().map(|h| h.join().unwrap()).sum();
    cli.join().unwrap();
    assert!(calls >= 8 * (per + 1));
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    for a in 0..8 {
        let view = v["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == format!("work-agent-a{a}"))
            .unwrap_or_else(|| panic!("view of agent {a} missing: {v}"));
        assert_eq!(view["changes"], per, "{v}");
    }
    assert_eq!(
        v["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == "work")
            .unwrap()["changes"],
        per
    );
    let (ok, f) = s.json(&["--json", "fsck"]);
    assert!(ok, "{f}");
}

#[test]
fn conc1_reads_on_another_connection_never_wait_behind_a_long_write() {
    let s = Served::new();
    let mut a = McpClient::start_proxy(&s);
    a.call_within(
        "rdy_session",
        json!({"actor": "agent:big", "role": "writer"}),
        CALL_BUDGET,
    );
    let w = a.call_within("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "B", "doc_type": "constraints"}}, {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}]}), CALL_BUDGET).ok();
    let section = w["created"]["$s"]
        .as_str()
        .expect("the section id")
        .to_string();
    let ops: Vec<_> = (0..rdy_mcp::tools::MAX_OPS_PER_WRITE).map(|i| json!({"op": "create", "type": "claim", "parent": section, "body": format!("Claim {i}.")})).collect();
    let id = a.next_id();
    a.send_raw_within(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "rdy_write", "arguments": {"ops": ops}}}).to_string(), Duration::from_secs(20));
    // while the big write runs on the writer, twenty reads on another connection answer fast
    let rt = tokio::runtime::Runtime::new().unwrap();
    let worst = rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            proto: Proto::Ctl,
            cwd: s.dir.path().into(),
        };
        let Attempt::Ready(mut c) = connect(&RunDir::of_project(s.dir.path()), &hello).await else {
            panic!("no daemon")
        };
        c.ctl("session", json!({"actor": null, "run": null}))
            .await
            .unwrap();
        let mut worst = Duration::ZERO;
        for _ in 0..20 {
            let t = Instant::now();
            c.ctl("status", json!({})).await.unwrap();
            worst = worst.max(t.elapsed());
        }
        worst
    });
    let v = a.recv(id, Duration::from_secs(60));
    assert!(v["result"].is_object(), "{v}");
    eprintln!("conc1: the worst of twenty status reads during the big write took {worst:?}");
    assert!(
        worst < Duration::from_millis(50),
        "a read waited {worst:?} behind the big write"
    );
}

#[test]
fn conc1_read_your_writes_across_processes() {
    let s = Served::new();
    let mut m = McpClient::start_proxy(&s);
    m.call_within(
        "rdy_session",
        json!({"actor": "agent:r", "role": "reader", "view": "canon"}),
        CALL_BUDGET,
    );
    for i in 0..10 {
        let (ok, _) = s.json(&["--json", "new", "doc", &format!("d{i}")]);
        assert!(ok);
        let o = m
            .call_within("rdy_overview", json!({"scope": "work"}), CALL_BUDGET)
            .ok();
        let work = o["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "work")
            .unwrap();
        assert_eq!(
            work["changes"],
            i + 1,
            "the MCP process did not see the CLI's acknowledged write: {o}"
        );
    }
}

#[test]
fn conc1_two_connections_each_keep_their_own_order_under_the_pool() {
    let s = Served::new();
    let hs: Vec<_> = (0..2)
        .map(|k| {
            let mut c = McpClient::start_proxy(&s);
            std::thread::spawn(move || {
                c.call_within("rdy_session", json!({"actor": format!("agent:k{k}"), "role": "writer"}), CALL_BUDGET);
                let mut ids = vec![];
                for i in 0..60 {
                    let w = c.next_id();
                    c.send_raw(&json!({"jsonrpc": "2.0", "id": w, "method": "tools/call", "params": {"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "doc", "fields": {"name": format!("k{k}-{i}"), "title": "K", "doc_type": "glossary"}}]}}}).to_string());
                    let r = c.next_id();
                    c.send_raw(&json!({"jsonrpc": "2.0", "id": r, "method": "tools/call", "params": {"name": "rdy_overview", "arguments": {}}}).to_string());
                    ids.push((w, r, i + 1));
                }
                let t = Instant::now();
                for (w, r, expect) in ids {
                    let b = || Duration::from_secs(60).saturating_sub(t.elapsed()).max(Duration::from_millis(1));
                    assert_eq!(c.recv_next(b())["id"], w);
                    let v = c.recv_next(b());
                    assert_eq!(v["id"], r);
                    let o: serde_json::Value = serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
                    let n = o["views"].as_array().unwrap().iter().find(|x| x["name"] == format!("work-agent-k{k}")).map(|x| x["changes"].as_u64().unwrap()).unwrap_or(0);
                    assert_eq!(n, expect as u64);
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}

#[test]
fn conc1_direct_mode_and_the_daemon_contend_for_the_lock_cleanly() {
    let s = Served::new();
    let hs: Vec<_> = (0..4)
        .map(|t| {
            let s_dir = s.dir.path().to_path_buf();
            let env = s.env_pairs();
            std::thread::spawn(move || {
                let mut codes = vec![];
                let start = Instant::now();
                for i in 0..12 {
                    let name = format!("x{t}-{i}");
                    let direct = (t + i) % 2 == 0;
                    let mut cmd = std::process::Command::new(rdy_qa::fixtures::rdy_bin());
                    cmd.args(["--project"]).arg(&s_dir).arg("--json");
                    if direct {
                        cmd.arg("--no-daemon");
                    }
                    let out = cmd
                        .args(["new", "doc", &name])
                        .envs(env.clone())
                        .env_remove("RDY_ACTOR")
                        .output()
                        .unwrap();
                    if !out.status.success() {
                        let v: serde_json::Value =
                            serde_json::from_slice(&out.stderr).unwrap_or(serde_json::Value::Null);
                        codes.push(v["code"].as_str().unwrap_or("?").to_string());
                    }
                    assert!(start.elapsed() < Duration::from_secs(120), "a command hung");
                }
                codes
            })
        })
        .collect();
    let codes: Vec<String> = hs.into_iter().flat_map(|h| h.join().unwrap()).collect();
    for c in &codes {
        assert!(c == E_LOCKED || c == rdy_daemon::E_DAEMON_START, "{c}");
        assert_ne!(c, E_INTERNAL);
    }
    let (ok, _) = s.json(&["--json", "status"]);
    assert!(ok, "the daemon serves at the end");
}
