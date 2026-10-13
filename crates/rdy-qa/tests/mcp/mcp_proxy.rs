//! proxy1: rdy mcp forwards stdio to the daemon's socket, keeps request order, refuses oversized
//! lines before the daemon, and reports a vanished daemon.
use rdy_daemon::{Attempt, Hello, MAX_CTL_LINE_BYTES, PROTO_VERSION, Proto, RunDir, connect};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::rdy_bin;
use rdy_qa::mcp::{DEADLINE, McpClient};
use serde_json::json;
use std::time::{Duration, Instant};

fn session(c: &mut McpClient, actor: &str) {
    let r = c.call("rdy_session", json!({"actor": actor, "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
}

#[test]
fn proxy1_limits_agree() {
    assert_eq!(MAX_CTL_LINE_BYTES, rdy_mcp::tools::MAX_REQUEST_BYTES);
}

#[test]
fn proxy1_the_proxy_starts_the_daemon_and_tools_run_against_it() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:p");
    let pid = s.daemon_pid().expect("the proxy started a daemon");
    let w = c.call("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}]})).ok();
    assert!(w["change"].is_string(), "{w}");
    let o = c.call("rdy_overview", json!({})).ok();
    assert!(
        o["views"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "work-agent-p" && v["changes"] == 1),
        "{o}"
    );
    assert!(o["open_report"].is_object());
    // the CLI sees the agent's write through the same daemon
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok);
    assert_eq!(s.daemon_pid(), Some(pid));
    assert!(
        v["views"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "work-agent-p"),
        "{v}"
    );
}

#[test]
fn proxy1_two_hundred_pipelined_mixed_requests_answer_in_order_and_each_read_sees_prior_writes() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:o");
    let mut ids = vec![];
    for i in 0..100 {
        let w = c.next_id();
        c.send_raw(&json!({"jsonrpc": "2.0", "id": w, "method": "tools/call", "params": {"name": "rdy_write", "arguments": {"ops": [{"op": "create", "type": "doc", "fields": {"name": format!("n{i:03}"), "title": "N", "doc_type": "glossary"}}]}}}).to_string());
        let r = c.next_id();
        c.send_raw(&json!({"jsonrpc": "2.0", "id": r, "method": "tools/call", "params": {"name": "rdy_overview", "arguments": {}}}).to_string());
        ids.push((w, r, i + 1));
    }
    let start = Instant::now();
    for (n, (w, r, expect)) in ids.iter().enumerate() {
        let budget = || {
            Duration::from_secs(30)
                .saturating_sub(start.elapsed())
                .max(Duration::from_millis(1))
        };
        let v = c.recv_next(budget());
        assert_eq!(v["id"], *w, "reply {n}: expected the write's reply: {v}");
        let v = c.recv_next(budget());
        assert_eq!(v["id"], *r, "reply {n}: expected the read's reply: {v}");
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let o: serde_json::Value = serde_json::from_str(text).unwrap();
        let changes = o["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == "work-agent-o")
            .map(|x| x["changes"].as_u64().unwrap())
            .unwrap_or(0);
        assert_eq!(
            changes, *expect as u64,
            "read {n} did not see the writes before it: {o}"
        );
    }
}

#[test]
fn proxy1_oversized_lines_are_refused_in_the_proxy_and_the_daemon_stays_up() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:l");
    let big = "x".repeat(MAX_CTL_LINE_BYTES + 10);
    let id = c.next_id();
    c.send_raw_within(&format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"rdy_write","arguments":{{"ops":[],"message":"{big}"}}}}}}"#), Duration::from_secs(30));
    let v = c.recv(id, Duration::from_secs(30));
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("E_LIMIT"),
        "{v}"
    );
    c.alive();
    assert!(s.daemon_pid().is_some());
}

#[test]
fn proxy1_a_stopped_daemon_ends_the_proxy_with_daemon_gone() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:g");
    s.stop();
    let (code, stderr) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("E_DAEMON_GONE"), "{stderr}");
}

#[test]
fn proxy1_stdin_eof_ends_the_proxy_cleanly_and_frees_the_daemon_session() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:e");
    c.close_stdin();
    let (code, stderr) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(code, Some(0), "{stderr}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            proto: Proto::Ctl,
            cwd: s.dir.path().into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&s.dir.path()))
                .project_id()
                .expect("the project's .rdy"),
        };
        let Attempt::Ready(mut cl) = connect(&RunDir::of_project(s.dir.path()), &hello).await
        else {
            panic!("no daemon")
        };
        let t = Instant::now();
        loop {
            let v = cl.ctl("ping", json!({})).await.unwrap();
            if v["sessions"] == 1 {
                break;
            } // only this ctl session
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "the mcp session was not freed: {v}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

#[test]
fn proxy1_a_proxy_against_a_store_held_by_no_daemon_fails_fast() {
    let s = Served::new();
    // a start timeout far past the deadline below: waiting it out is the defect
    s.set_start_timeout(300);
    let holder = McpClient::start(s.dir.path()); // rdy mcp --no-daemon holds the store
    let t = Instant::now();
    let out = std::process::Command::new(rdy_bin())
        .args(["mcp", "--project"])
        .arg(s.dir.path())
        .envs(s.env_pairs())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("E_DAEMON_START") && err.contains("held by another rdy process"),
        "{err}"
    );
    assert!(
        t.elapsed() < Duration::from_secs(120),
        "waited out the 300 s start timeout: {:?}",
        t.elapsed()
    );
    drop(holder);
    let _ = DEADLINE;
}
