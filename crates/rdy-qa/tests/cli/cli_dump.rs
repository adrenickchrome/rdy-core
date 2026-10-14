//! dl7: `rdy dump` and `rdy load`, the CLI twins of `rdy_dump` and `rdy_load` (D-KQ4CA).
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::golden::{FIXTURE_DIR, approver, land};
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::path::Path;

/// A served project holding the golden corpus, landed and tagged `v1.0.0`.
fn golden_served() -> Served {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    r.close().unwrap();
    s
}

/// Runs `rdy serve --stop` in `dir` when dropped, so a daemon a test starts there stops also
/// when an assert fails.
struct StopOnDrop<'a> {
    dir: &'a Path,
    env: Vec<(String, String)>,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        match human_command(self.dir, &self.env, &["serve", "--stop"]).output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!(
                "rdy serve --stop failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => eprintln!("rdy serve --stop did not run: {e}"),
        }
    }
}

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_count(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `dumped {batches} batches and {frames} frames, with {n} project files, to {out} ({bytes} bytes)`.
fn is_the_dump_line(line: &str, out: &str) -> bool {
    let Some(rest) = line.strip_prefix("dumped ") else {
        return false;
    };
    let Some((batches, rest)) = rest.split_once(" batches and ") else {
        return false;
    };
    let Some((frames, rest)) = rest.split_once(" frames, with ") else {
        return false;
    };
    let Some((n, rest)) = rest.split_once(" project files, to ") else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(out).and_then(|r| r.strip_prefix(" (")) else {
        return false;
    };
    let Some(bytes) = rest.strip_suffix(" bytes)") else {
        return false;
    };
    [batches, frames, n, bytes].iter().all(|x| is_count(x))
}

/// `loaded {batches} batches and {frames} frames into {project}: {l} landings, {t} versions, {v} views; fsck ok`.
fn is_the_load_line(line: &str, project: &str) -> bool {
    let Some(rest) = line.strip_prefix("loaded ") else {
        return false;
    };
    let Some((batches, rest)) = rest.split_once(" batches and ") else {
        return false;
    };
    let Some((frames, rest)) = rest.split_once(" frames into ") else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix(project)
        .and_then(|r| r.strip_prefix(": "))
    else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(" views; fsck ok") else {
        return false;
    };
    let Some((l, rest)) = rest.split_once(" landings, ") else {
        return false;
    };
    let Some((t, v)) = rest.split_once(" versions, ") else {
        return false;
    };
    [batches, frames, l, t, v].iter().all(|x| is_count(x))
}

#[test]
fn dl7_rdy_dump_and_rdy_load_round_trip_at_the_cli() {
    let s = golden_served();
    let scratch = short_tempdir();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap();
    let out = s.rdy(&["dump", "--out", file_arg]);
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(is_the_dump_line(text.trim_end(), file_arg), "{text:?}");
    assert!(file.is_file());

    let to = scratch.path().join("new");
    let to_arg = to.to_str().unwrap();
    let out = s.rdy(&["load", file_arg, to_arg]);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert!(is_the_load_line(lines[0], to_arg), "{text:?}");
    assert!(
        lines[1].starts_with("next: ") && lines[1].len() > 6,
        "{text:?}"
    );

    let env = s.env_pairs();
    // fsck starts a daemon in `to`; it stops also when an assert below fails
    let _stop = StopOnDrop {
        dir: &to,
        env: env.clone(),
    };
    let fsck = human_command(&to, &env, &["fsck"]).output().unwrap();
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    let loaded = human_command(&to, &env, &["--json", "tags"])
        .output()
        .unwrap();
    let loaded: Value = serde_json::from_slice(&loaded.stdout).unwrap();
    let (ok, source) = s.json(&["--json", "tags"]);
    assert!(ok, "{source}");
    assert_eq!(loaded, source);
    assert!(
        !source.to_string().is_empty() && source != json!([]),
        "{source}"
    );
    let stop = human_command(&to, &env, &["serve", "--stop"])
        .output()
        .unwrap();
    assert!(stop.status.success());
}

#[test]
fn dl7_rdy_dump_has_no_scope_flag() {
    let s = Served::new();
    let out = s.rdy(&["dump", "--out", "x", "--scope", "canon"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unexpected argument '--scope'"), "{err}");
    assert!(!s.dir.path().join("x").exists());
}

#[test]
fn dl7_cli_and_tool_replies_match() {
    let s = golden_served();
    let scratch = short_tempdir();
    let a = scratch.path().join("a.rdydump");
    let (ok, cli) = s.json(&["--json", "dump", "--out", a.to_str().unwrap()]);
    assert!(ok, "{cli}");
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let b = scratch.path().join("b.rdydump");
    let tool = c.call("rdy_dump", json!({"out": b.to_str().unwrap()})).ok();
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&cli), keys(&tool), "{cli} {tool}");
}

#[test]
fn dl7_a_load_into_dot_rdy_is_refused() {
    let s = golden_served();
    let scratch = short_tempdir();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap();
    stdout(&s.rdy(&["dump", "--out", file_arg]));
    for dir in [".rdy/packs", ".rdy", ".rdy/new"] {
        let to = s.dir.path().join(dir);
        let (ok, e) = s.json(&["--json", "load", file_arg, to.to_str().unwrap()]);
        assert!(!ok, "{dir}: {e}");
        assert_eq!(e["code"], "E_BAD_INPUT", "{dir}: {e}");
        assert_eq!(
            e["message"],
            format!("{} is inside a project's .rdy directory", to.display()),
            "{dir}"
        );
        assert_eq!(
            e["fix"],
            "load into a new directory outside any .rdy, for example a sibling of the project"
        );
    }
    assert!(!s.dir.path().join(".rdy/packs/.rdy.yaml").exists());
    assert!(!s.dir.path().join(".rdy/new").exists());
    let (ok, st) = s.json(&["--json", "status"]);
    assert!(ok, "the project still opens: {st}");
}

#[test]
fn dl7_rdy_load_refuses_a_target_whose_daemon_still_runs() {
    let s = golden_served();
    let scratch = short_tempdir();
    let file = scratch.path().join("p.rdydump");
    let file_arg = file.to_str().unwrap();
    // the dump goes through the daemon, which stays up
    stdout(&s.rdy(&["dump", "--out", file_arg]));
    let pid = s.daemon_pid().expect("the dump started the daemon");
    let dir = s.dir.path();
    let store = dir.join(".rdy/store");
    let aside = scratch.path().join("store-aside");
    std::fs::rename(&store, &aside).unwrap();
    let (ok, e) = s.json(&["--json", "load", file_arg, dir.to_str().unwrap()]);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_LOCKED", "{e}");
    assert_eq!(
        e["message"],
        format!("a daemon still runs for {} (pid {pid})", dir.display())
    );
    assert_eq!(
        e["fix"],
        format!("run rdy serve --stop in {}, then load again", dir.display())
    );
    assert!(!store.exists(), "the load wrote no store");
    // the order the fix of E_STORE_FORMAT gives: stop the daemon, then move the store aside
    std::fs::rename(&aside, &store).unwrap();
    s.stop();
    assert!(s.daemon_pid().is_none(), "the daemon stopped");
    std::fs::rename(&store, &aside).unwrap();
    let text = stdout(&s.rdy(&["load", file_arg, dir.to_str().unwrap()]));
    assert!(text.starts_with("loaded "), "{text:?}");
    let (ok, tags) = s.json(&["--json", "tags"]);
    assert!(ok && tags != json!([]), "{tags}");
}

/// `text` with every run of white space made one space (help text wraps by the terminal).
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn dl7_rdy_load_help_gives_the_in_place_move_in_order() {
    let out = std::process::Command::new(rdy_qa::fixtures::rdy_bin())
        .args(["load", "--help"])
        .output()
        .unwrap();
    let help = stdout(&out);
    assert!(
        one_line(&help).contains(&one_line(rdy_store::format::MOVE_WITH_DUMP)),
        "{help}"
    );
}
