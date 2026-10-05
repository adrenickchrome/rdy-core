//! cli2: every command is a client of the daemon; direct mode keeps the in-process path; serve
//! starts, detaches and stops; the human comes from the user config and agents stay in their views.
use rdy_daemon::E_DAEMON_START;
use rdy_engine::{E_LOCKED, E_PERMISSION, E_VIEW_OWNER, Rdy};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::rdy_bin;
use serde_json::Value;
use std::time::{Duration, Instant};

#[test]
fn cli2_a_command_starts_the_daemon_and_later_commands_reuse_it() {
    let s = Served::new();
    assert!(s.daemon_pid().is_none());
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let pid = s.daemon_pid().expect("a daemon was started");
    let (ok, v) = s.json(&["--json", "new", "doc", "a", "--title", "A"]);
    assert!(ok, "{v}");
    assert_eq!(
        s.daemon_pid(),
        Some(pid),
        "the same daemon served the second command"
    );
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok);
    assert_eq!(v["views"][0]["name"], "work");
    assert_eq!(v["views"][0]["changes"], 1);
    assert_eq!(v["views"][0]["owner"]["actor"], "human:qa");
}

#[test]
fn cli2_text_output_is_unchanged_and_status_reports_the_open() {
    let s = Served::new();
    let out = s.rdy(&["new", "doc", "wallets/tx", "--title", "Tx"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("created doc wallets/tx (d-"), "{text}");
    let out = s.rdy(&["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("pack rdy-standard\ncanon: "), "{text}");
    assert!(text.contains("\nviews:\n  work: "), "{text}");
    assert!(!text.contains("quarantine"), "{text}");
    s.stop();
    // a quarantine file by name only (the real ones are made by Store::open; a planted one must
    // not disturb the next open, so the index-damage pattern is used, which open never parses)
    std::fs::write(s.dir.path().join(".rdy/store/index.redb.damaged-7"), b"x").unwrap();
    let out = s.rdy(&["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warning: quarantine: index.redb.damaged-7"),
        "{text}"
    );
    let out = s.rdy(&["fsck"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("ok: "), "{text}");
    assert!(
        text.contains("warning: quarantine: index.redb.damaged-7"),
        "{text}"
    );
}

#[test]
fn cli2_no_daemon_opens_the_store_directly_and_is_locked_while_a_daemon_runs() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "--no-daemon", "new", "doc", "a"]);
    assert!(ok, "{v}");
    assert!(s.daemon_pid().is_none(), "direct mode starts no daemon");
    s.json(&["--json", "status"]);
    assert!(s.daemon_pid().is_some());
    let (ok, v) = s.json(&["--json", "--no-daemon", "status"]);
    assert!(!ok);
    assert_eq!(v["code"], E_LOCKED, "{v}");
    assert!(
        v["fix"].as_str().unwrap().contains("rdy serve --stop"),
        "{v}"
    );
    let out = std::process::Command::new(rdy_bin())
        .args(["--project"])
        .arg(s.dir.path())
        .args(["--json", "status"])
        .env("RDY_NO_DAEMON", "1")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    assert!(!out.status.success(), "RDY_NO_DAEMON=1 is --no-daemon");
}

#[test]
fn cli2_a_daemon_cannot_start_while_no_daemon_holds_the_store() {
    let s = Served::new();
    // hold the store from this process; a ReadState must not outlive this guard
    let holder = Rdy::open(s.dir.path()).unwrap();
    let t = Instant::now();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(!ok);
    assert_eq!(v["code"], E_DAEMON_START, "{v}");
    let tail = v["log_tail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tail.contains("already running: the store at"), "{tail}");
    assert!(
        t.elapsed() < Duration::from_secs(29),
        "the client did not wait out the start timeout: {:?}",
        t.elapsed()
    );
    drop(holder);
    let (ok, _) = s.json(&["--json", "status"]);
    assert!(ok, "once the store is free, the daemon starts");
}

#[test]
fn cli2_an_agent_actor_defaults_to_its_own_view_and_cannot_take_the_humans() {
    let s = Served::new();
    s.json(&["--json", "new", "doc", "h"]);
    let (ok, v) = s.json_as("agent:x", None, &["--json", "new", "doc", "a"]);
    assert!(ok, "{v}");
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok);
    let views: Vec<&str> = v["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert!(views.contains(&"work-agent-x"), "{views:?}");
    let (ok, v) = s.json_as(
        "agent:x",
        None,
        &["--json", "new", "doc", "b", "--view", "work"],
    );
    assert!(!ok);
    assert_eq!(v["code"], E_VIEW_OWNER, "{v}");
    assert!(v["fix"].as_str().unwrap().contains("work-agent-x"), "{v}");
    let (ok, v) = s.json_as(
        "agent:x",
        Some("r9"),
        &["--json", "new", "doc", "c", "--view", "work-agent-x"],
    );
    assert!(!ok, "{v}");
    assert_eq!(
        v["code"], E_VIEW_OWNER,
        "another run of the same actor is another session: {v}"
    );
    let (ok, p) = s.json_as("agent:x", None, &["--json", "propose", "--title", "t"]);
    assert!(ok, "{p}");
    let (ok, v) = s.json_as(
        "agent:x",
        None,
        &["--json", "approve", p["id"].as_str().unwrap()],
    );
    assert!(!ok);
    assert_eq!(v["code"], E_PERMISSION, "{v}");
    let (ok, _) = s.json(&["--json", "approve", p["id"].as_str().unwrap()]);
    assert!(ok, "the human approves");
}

#[test]
fn cli2_serve_foreground_detach_and_stop() {
    let s = Served::new();
    let out = s.rdy(&["serve", "--stop"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "no daemon running"
    );
    let out = s.rdy(&["serve", "--detach"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = Instant::now();
    while s.daemon_pid().is_none() {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    let (ok, _) = s.json(&["--json", "status"]);
    assert!(ok);
    let out = s.rdy(&["serve"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a second serve is already running"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("already running"));
    let out = s.rdy(&["serve", "--stop"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "stopped");
    assert!(!s.run_dir().socket().exists());
    assert!(s.daemon_pid().is_none());
    let r = Rdy::open(s.dir.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown);
}

#[test]
fn cli2_edit_goes_through_the_daemon() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "a", "--title", "Before"]);
    assert!(ok, "{v}");
    let id = v["created"]
        .as_object()
        .unwrap()
        .values()
        .find(|x| x.as_str().unwrap().starts_with("d-"))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let script = s.dir.path().join("ed.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nsed -i.bak 's/title: Before/title: After/' \"$1\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = std::process::Command::new(rdy_bin())
        .args(["--project"])
        .arg(s.dir.path())
        .args(["--json", "edit", &id])
        .env("EDITOR", &script)
        .envs(s.env_pairs())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok);
    assert_eq!(v["fields"]["title"], "After");
}

#[test]
fn cli2_check_inside_a_project_reads_canon_through_the_daemon() {
    let s = Served::new();
    s.json(&["--json", "new", "doc", "a"]);
    let (ok, p) = s.json(&["--json", "propose", "--title", "t"]);
    assert!(ok, "{p}");
    s.json(&["--json", "approve", p["id"].as_str().unwrap()]);
    let (ok, _) = s.json(&["--json", "export", "--out", "exp"]);
    assert!(ok);
    let out = s.rdy(&["check", "exp"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    let _: Value = Value::Null;
}
