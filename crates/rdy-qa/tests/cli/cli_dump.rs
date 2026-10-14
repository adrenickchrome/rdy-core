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
