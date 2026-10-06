//! dfu1: the M2b demo's daemon rough edges.
use rdy_daemon::client::{HELD_GRACE, connect_backoff, log_tail_since};
use rdy_daemon::proxy::InFlight;
use rdy_daemon::{E_DAEMON_START, RunDir};
use rdy_engine::Rdy;
use rdy_qa::daemon::{Hosted, Served};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::time::Duration;

#[test]
fn dfu1_in_flight_tracking_answers_exactly_the_unanswered_requests() {
    let mut f = InFlight::new();
    f.sent(br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#);
    f.sent(br#"{"jsonrpc":"2.0","id":"two","method":"tools/call","params":{}}"#);
    f.sent(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    f.sent(br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}}"#);
    f.answered(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    f.answered(br#"not json"#);
    assert_eq!(f.len(), 2);
    let gone: Vec<Value> = f
        .gone_lines()
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        gone.iter().map(|v| v["id"].clone()).collect::<Vec<_>>(),
        vec![json!("two"), json!(3)]
    );
    for v in &gone {
        assert_eq!(v["error"]["data"]["code"], "E_DAEMON_GONE");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("E_DAEMON_GONE")
        );
    }
}

#[test]
fn dfu1_a_dead_daemon_answers_every_in_flight_mcp_request() {
    let s = Served::new();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_session", json!({"actor": "agent:k", "role": "writer"}));
    assert!(r.code().is_none());
    let mut ids = vec![];
    for i in 0..20 {
        let id = c.next_id();
        let ops: Vec<Value> = (0..500)
            .map(|k| {
                json!({"op": "create", "type": "doc",
            "fields": {"name": format!("k{i}-{k}"), "title": "K", "doc_type": "glossary"}})
            })
            .collect();
        c.send_raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "rdy_write", "arguments": {"ops": ops}}})
            .to_string(),
        );
        ids.push(id);
    }
    s.kill9();
    for id in ids {
        let v = c.recv(id, Duration::from_secs(20));
        assert!(
            v.get("result").is_some() || v["error"]["data"]["code"] == "E_DAEMON_GONE",
            "request {id}: {v}"
        );
    }
    let (code, stderr) = c.wait_exit(Duration::from_secs(10));
    assert_eq!(code, Some(1), "{stderr}");
}

#[test]
fn dfu1_after_recovery_status_and_fsck_print_no_unclean_warning() {
    let s = Served::new();
    assert!(s.json(&["--json", "new", "doc", "a"]).0);
    s.kill9();
    let out = s.rdy(&["status"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && !text.contains("did not close the store cleanly"),
        "{text}"
    );
    let (_, v) = s.json(&["--json", "status"]);
    assert_eq!(
        v["open_report"]["unclean_shutdown"], true,
        "the JSON still records what the open did"
    );
    let out = s.rdy(&["fsck"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("note: this daemon started after an unclean shutdown and recovered"),
        "{text}"
    );
    assert!(!text.contains("did not close the store cleanly"), "{text}");
}

#[test]
fn dfu1_a_store_held_by_no_daemon_is_refused_without_spawning_and_with_the_specific_fix() {
    let s = Served::new();
    let holder = Rdy::open(s.dir.path()).unwrap();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(!ok);
    assert_eq!(v["code"], E_DAEMON_START, "{v}");
    assert_eq!(
        v["fix"],
        "a --no-daemon rdy holds the store: close it, or drop --no-daemon"
    );
    assert!(v.get("log_tail").is_none(), "{v}");
    assert!(
        !s.run_dir().log_file().exists(),
        "no daemon was spawned to find the lock"
    );
    assert!(HELD_GRACE <= Duration::from_millis(250));
    drop(holder);
    assert!(
        s.json(&["--json", "status"]).0,
        "once the store is free, the daemon starts"
    );
}

#[test]
fn dfu1_start_errors_cite_only_this_starts_log_lines() {
    let dir = rdy_qa::daemon::short_tempdir();
    let run = RunDir::of_project(dir.path());
    std::fs::create_dir_all(run.path()).unwrap();
    std::fs::write(run.log_file(), "old 1\nold 2\n").unwrap();
    let from = std::fs::metadata(run.log_file()).unwrap().len();
    std::fs::write(run.log_file(), "old 1\nold 2\nnew 1\nnew 2\n").unwrap();
    assert_eq!(log_tail_since(&run, from, 20), vec!["new 1", "new 2"]);
    assert_eq!(log_tail_since(&run, from, 1), vec!["new 2"]);
    std::fs::write(run.log_file(), "rotated\n").unwrap();
    assert_eq!(
        log_tail_since(&run, from, 20),
        vec!["rotated"],
        "a shorter file is read from its start"
    );
}

#[test]
fn dfu1_connect_retries_back_off_from_one_millisecond() {
    let got: Vec<u64> = (0..9)
        .map(|a| connect_backoff(a).as_millis() as u64)
        .collect();
    assert_eq!(got, vec![1, 2, 4, 8, 16, 32, 50, 50, 50]);
}

#[test]
fn dfu1_fixing_a_broken_pack_sends_pack_changed() {
    const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
    const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack\n";
    let h = Hosted::new();
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        std::fs::write(h.dir.path().join(".rdy/config.yaml"), "pack: proj\n").unwrap();
        a.ctl("new_doc", json!({"path": "d1", "view": null}))
            .await
            .unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_changed"));
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_BAD).unwrap();
        a.ctl("new_doc", json!({"path": "d2", "view": null}))
            .await
            .unwrap();
        assert!(a.take_notice().unwrap().starts_with("pack_error"));
        std::fs::write(h.dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        a.ctl("new_doc", json!({"path": "d3", "view": null}))
            .await
            .unwrap();
        let n = a
            .take_notice()
            .expect("a notice when the pack is good again");
        assert!(n.starts_with("pack_changed"), "{n}");
        a.ctl("status", json!({})).await.unwrap();
        assert!(a.take_notice().is_none(), "once");
    });
}
