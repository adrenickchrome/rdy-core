//! cli2: every command is a client of the daemon; direct mode keeps the in-process path; serve
//! starts, detaches and stops; the human comes from the user config and agents stay in their views.
use crate::cli_editor::edit_with;
use rdy_daemon::E_DAEMON_START;
use rdy_engine::{E_LOCKED, E_PERMISSION, E_VIEW_OWNER, Rdy};
use rdy_qa::daemon::Served;
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
    // ec10: the repo line follows the pack line (O34); the daemon line sits between them
    let daemon = format!(
        "pack rdy-standard\ndaemon: rdy {} build {}\nrepo ",
        env!("CARGO_PKG_VERSION"),
        rdy_daemon::BUILD
    );
    assert!(text.starts_with(&daemon), "{text}");
    assert!(
        text.lines()
            .nth(3)
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
    let out = rdy_qa::fixtures::rdy_command()
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
    rdy_qa::daemon::wait_endpoint_gone(&s.run_dir());
    assert!(s.daemon_pid().is_none());
    let r = Rdy::open(s.dir.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown);
}

#[test]
fn cli2_edit_goes_through_the_daemon() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "a", "--title", "Before"]);
    assert!(ok, "{v}");
    let id = created_doc(&v);
    let out = edit_with(&s, None, "replace title: Before\ttitle: After\n", &id);
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

#[test]
fn cli2_a_relative_project_starts_the_daemon() {
    let s = Served::new();
    let parent = s.dir.path().parent().unwrap();
    let name = s.dir.path().file_name().unwrap().to_str().unwrap();
    for project in [name.to_string(), format!("./{name}")] {
        let out = rdy_qa::fixtures::rdy_command()
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
    edit_survives_stop_with_notice(None, None);
}

/// A waiting notice on stderr (the stop writes one after its grace on a loaded machine) must not
/// change what the stop prints on stdout. The notice is injected, so no grace has to run out.
#[test]
fn plc8_stop_waiting_notice_does_not_change_stdout() {
    edit_survives_stop_with_notice(
        Some("waiting for the daemon (pid 12345) to finish its work and close the store"),
        None,
    );
}

/// The editor is a program whose path has a space (in double quotes on Windows, where rdy splits
/// the command line itself) and the daemon stops while it runs: the edit still lands on a fresh
/// session. Where `sh` splits the value, a path cannot have a space, so the directory has none.
#[test]
fn wx26b_edit_survives_daemon_stop_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let place = dir
        .path()
        .join(if cfg!(windows) { "an editor" } else { "editor" });
    std::fs::create_dir(&place).unwrap();
    let exe = place.join(if cfg!(windows) {
        "edit fixture.exe"
    } else {
        "edit-fixture"
    });
    let current = std::env::current_exe().unwrap();
    if std::fs::hard_link(&current, &exe).is_err() {
        std::fs::copy(&current, &exe).unwrap();
    }
    assert_eq!(cfg!(windows), exe.to_str().unwrap().contains(' '));
    edit_survives_stop_with_notice(None, Some(&exe));
}

fn edit_survives_stop_with_notice(notice: Option<&str>, exe: Option<&std::path::Path>) {
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
    let script = format!(
        "replace title: Before\ttitle: After\nstop {}\t{}\t{}\n",
        stop_out.display(),
        stop_err.display(),
        notice.unwrap_or_default()
    );
    let out = edit_with(&s, exe, &script, &id);
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
    let saved = failed_edit_keeps_its_file(&s, &id, None);
    std::fs::remove_file(&saved).unwrap();
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["fields"]["title"], "Before");
}

/// An editor that saves a file the block cannot be read from: the edit is refused, the file is
/// kept, and the refusal names its path in `saved_edit` and in the fix. Returns that path.
fn failed_edit_keeps_its_file(
    s: &Served,
    id: &str,
    exe: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let out = edit_with(s, exe, "write not: [valid\n", id);
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
    saved
}

/// D-FARN9: a failed edit keeps the named recovery file whatever the editor command is. On
/// Windows an editor value that cannot be parsed, or names a program that cannot start, is
/// refused before any edit with the decided text, and the block is not changed.
#[test]
fn wx26b_failed_edit_keeps_the_named_recovery_file() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "a", "--title", "Before"]);
    assert!(ok, "{v}");
    let id = created_doc(&v);
    // the editor program has a space in its path on Windows, so its path is in double quotes
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "my editor.exe"
    } else {
        "my-editor"
    });
    let current = std::env::current_exe().unwrap();
    if std::fs::hard_link(&current, &exe).is_err() {
        std::fs::copy(&current, &exe).unwrap();
    }
    let saved = failed_edit_keeps_its_file(&s, &id, Some(&exe));
    std::fs::remove_file(&saved).unwrap();
    #[cfg(windows)]
    {
        let refuse = |editor: &str| {
            let out = rdy_qa::fixtures::rdy_command()
                .arg("--project")
                .arg(s.dir.path())
                .args(["--json", "edit", &id])
                .env_remove("RDY_ACTOR")
                .env_remove("RDY_RUN")
                .env_remove("RDY_NO_DAEMON")
                .envs(s.env_pairs())
                .env_remove("VISUAL")
                .env("EDITOR", editor)
                .output()
                .unwrap();
            assert!(!out.status.success());
            serde_json::from_slice::<Value>(&out.stderr).unwrap()
        };
        let e = refuse("\"never closed");
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(
            e["message"], "the editor command in %EDITOR% cannot be parsed: \"never closed",
            "{e}"
        );
        assert_eq!(
            e["fix"],
            "set %VISUAL% or %EDITOR% to a program, in double quotes if its path has spaces, with any arguments after it",
            "{e}"
        );
        let missing = s.dir.path().join("no such dir").join("editor.exe");
        let e = refuse(&format!("\"{}\" --wait", missing.display()));
        assert_eq!(e["code"], "E_IO", "{e}");
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{}: ", missing.display())),
            "{e}"
        );
        assert_eq!(e["fix"], "set %VISUAL% or %EDITOR%", "{e}");
    }
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["fields"]["title"], "Before");
}
