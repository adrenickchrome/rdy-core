#![cfg(unix)]
// Unix only: it sets file modes. FW-XP1 Phase 4 ports it.
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
    // ec10: the repo line follows the pack line (O34)
    assert!(text.starts_with("pack rdy-standard\nrepo "), "{text}");
    assert!(
        text.lines()
            .nth(2)
            .is_some_and(|l| l.starts_with("canon: ")),
        "{text}"
    );
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
    // a start timeout far past the deadline below: waiting it out is the defect, and a loaded
    // machine never comes near it
    s.set_start_timeout(300);
    // hold the store from this process; a ReadState must not outlive this guard
    let holder = Rdy::open(s.dir.path()).unwrap();
    let t = Instant::now();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(!ok);
    assert_eq!(v["code"], E_DAEMON_START, "{v}");
    assert!(v["fix"].as_str().unwrap().contains("--no-daemon"), "{v}");
    assert!(
        t.elapsed() < Duration::from_secs(120),
        "the client waited out the 300 s start timeout: {:?}",
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
    let (ok, p) = s.json_as(
        "agent:x",
        None,
        &[
            "--json",
            "propose",
            "--title",
            "t",
            "--summary",
            "test summary",
        ],
    );
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
        assert!(t.elapsed() < Duration::from_secs(30), "no daemon pid");
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
    // no CLI path writes a format-3 tree (O12): the tree comes from canon as the oracle reads it
    // (O13): rdy dump through the daemon, a load into a temp directory, the engine's export there
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("p.rdydump");
    let (ok, d) = s.json(&["--json", "dump", "--out", file.to_str().unwrap()]);
    assert!(ok, "{d}");
    let loaded = tmp.path().join("project");
    rdy_engine::load::load(&file, &loaded).unwrap();
    let r = rdy_engine::Rdy::open(&loaded).unwrap();
    r.read()
        .export(&rdy_engine::Scope::Canon, &s.dir.path().join("exp"))
        .unwrap();
    r.close().unwrap();
    let out = s.rdy(&["check", "exp"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    let _: Value = Value::Null;
}

/// The id of the doc a `new doc` reply created.
fn created_doc(v: &Value) -> String {
    v["created"]
        .as_object()
        .unwrap()
        .values()
        .find_map(|x| x.as_str().filter(|s| s.starts_with("d-")))
        .unwrap()
        .to_string()
}

/// Writes an executable editor script into the project.
fn editor_script(s: &Served, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = s.dir.path().join("ed.sh");
    std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// `rdy --project <dir> --json edit <id>` with `EDITOR=script`, as the human.
fn edit_with(s: &Served, script: &std::path::Path, id: &str) -> std::process::Output {
    std::process::Command::new(rdy_bin())
        .args(["--project"])
        .arg(s.dir.path())
        .args(["--json", "edit", id])
        .env("EDITOR", script)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap()
}

#[test]
fn cli2_a_relative_project_starts_the_daemon() {
    let s = Served::new();
    let parent = s.dir.path().parent().unwrap();
    let name = s.dir.path().file_name().unwrap().to_str().unwrap();
    for project in [name.to_string(), format!("./{name}")] {
        let out = std::process::Command::new(rdy_bin())
            .args(["--project", &project, "--json", "status"])
            .current_dir(parent)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(s.env_pairs())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "--project {project}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(s.daemon_pid().is_some(), "--project {project}");
    }
}

#[test]
fn cli2_edit_survives_the_daemon_stopping_while_the_editor_runs() {
    edit_survives_stop_with_notice(None);
}

/// A waiting notice on stderr (the stop writes one after its grace on a loaded machine) must not
/// change what the stop prints on stdout. The notice is injected, so no grace has to run out.
#[test]
fn plc8_stop_waiting_notice_does_not_change_stdout() {
    edit_survives_stop_with_notice(Some(
        "waiting for the daemon (pid 12345) to finish its work and close the store",
    ));
}

fn edit_survives_stop_with_notice(notice: Option<&str>) {
    let s = Served::new();
    // made directly, so the edit below is the command that spawns the daemon (and so its parent)
    let (ok, v) = s.json(&[
        "--json",
        "--no-daemon",
        "new",
        "doc",
        "a",
        "--title",
        "Before",
    ]);
    assert!(ok, "{v}");
    let id = created_doc(&v);
    let stop_out = s.dir.path().join("stop.out");
    let stop_err = s.dir.path().join("stop.err");
    let notice_cmd = notice
        .map(|n| format!("printf '%s\\n' '{n}' >&2; "))
        .unwrap_or_default();
    let script = editor_script(
        &s,
        &format!(
            "sed -i.bak 's/title: Before/title: After/' \"$1\"\n{{ {}'{}' --project '{}' serve --stop; }} > '{}' 2> '{}'\necho \"exit=$?\" >> '{}'\n",
            notice_cmd,
            rdy_bin().display(),
            s.dir.path().display(),
            stop_out.display(),
            stop_err.display(),
            stop_out.display()
        ),
    );
    let out = edit_with(&s, &script, &id);
    let stopped = std::fs::read_to_string(&stop_out).unwrap();
    let stop_stderr = std::fs::read_to_string(&stop_err).unwrap_or_default();
    // stdout alone is asserted; a waiting notice may appear on stderr in either case
    assert!(
        stopped.starts_with("stopped\n") && stopped == "stopped\nexit=0\n",
        "the daemon the edit spawned stopped (it is reaped, not a zombie): stdout {stopped:?}, stderr {stop_stderr:?}"
    );
    if let Some(n) = notice {
        assert!(
            stop_stderr.starts_with(&format!("{n}\n")),
            "the injected notice leads stderr: {stop_stderr:?}"
        );
    }
    // the stop drained every connection rather than waiting out its grace: the daemon logs
    // when the grace ran out (read from its log, not a clock, so a loaded machine cannot fail it)
    let log = std::fs::read_to_string(s.run_dir().log_file()).unwrap_or_default();
    assert!(log.contains("exiting (stopped)"), "the daemon log: {log}");
    assert!(
        !log.contains("still open after"),
        "the stop waited out its grace: {log}"
    );
    assert!(
        out.status.success(),
        "the edit is applied on a fresh session: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["fields"]["title"], "After");
}

#[test]
fn cli2_a_failed_edit_keeps_the_edited_file_and_names_it() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "a", "--title", "Before"]);
    assert!(ok, "{v}");
    let id = created_doc(&v);
    let script = editor_script(&s, "printf 'not: [valid\\n' > \"$1\"\n");
    let out = edit_with(&s, &script, &id);
    assert!(!out.status.success());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    let saved = std::path::PathBuf::from(err["saved_edit"].as_str().expect("saved_edit"));
    assert!(
        err["fix"]
            .as_str()
            .unwrap()
            .contains(&format!("your edit is saved at {}", saved.display())),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "not: [valid\n");
    std::fs::remove_file(&saved).unwrap();
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["fields"]["title"], "Before");
}

/// Final review fix round 1 (minor 12): SIGINT (Ctrl-C) and SIGTERM stop a foreground
/// `rdy serve` the way `rdy serve --stop` does: the run files go, the store closes cleanly and
/// the process exits 0.
#[test]
fn cli2_sigint_and_sigterm_stop_a_foreground_serve_cleanly() {
    for sig in ["-INT", "-TERM"] {
        let s = Served::new();
        let mut child = s.spawn(&["serve"]);
        let t = Instant::now();
        while s.daemon_pid() != Some(child.id()) {
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "{sig}: the foreground daemon did not write its pid"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = std::process::Command::new("kill")
            .args([sig, &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let t = Instant::now();
        let exit = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if t.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                panic!("{sig}: rdy serve did not exit within 30 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let out = child.wait_with_output().unwrap();
        assert!(
            exit.success(),
            "{sig}: {exit:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!s.run_dir().socket().exists(), "{sig}");
        assert!(!s.run_dir().pid_file().exists(), "{sig}");
        let r = Rdy::open(s.dir.path()).unwrap();
        assert!(!r.open_report().unclean_shutdown, "{sig}");
        drop(r);
    }
}
