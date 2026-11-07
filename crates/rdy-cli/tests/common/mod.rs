//! The helpers of the CLI tests that drive the `rdy` binary directly: a human actor, a home of its
//! own, and a project with one term. `cli.rs` runs on every platform; `cli_unix.rs` holds the two
//! editor-script cases that need Unix file modes.
#![allow(dead_code)]
use serde_json::Value;
use std::process::Command;

/// A user config naming `human:tester` as the human actor, and the environment that points at it.
pub fn human_config() -> (tempfile::TempDir, (&'static str, std::path::PathBuf)) {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join("rdy")).unwrap();
    std::fs::write(cfg.path().join("rdy/config.yaml"), "actor: human:tester\n").unwrap();
    let env = ("XDG_CONFIG_HOME", cfg.path().to_path_buf());
    (cfg, env)
}

/// `rdy --no-daemon <args>` in `dir` as the human `human:tester` (these tests drive the direct
/// path; the daemon path is covered by rdy-qa's cli_daemon).
pub fn rdy_cmd(dir: &std::path::Path, args: &[&str]) -> (tempfile::TempDir, Command) {
    let (cfg, (k, v)) = human_config();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rdy"));
    cmd.arg("--no-daemon")
        .args(args)
        .current_dir(dir)
        .env("RDY_ACTOR", "human:tester")
        .env(k, v)
        // ain: no real home, so no real meta project is read
        .env("HOME", cfg.path())
        .env("USERPROFILE", cfg.path())
        .env("APPDATA", cfg.path());
    (cfg, cmd)
}

pub fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let (_cfg, mut cmd) = rdy_cmd(dir, args);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

pub fn json(dir: &std::path::Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let (ok, out, err) = rdy(dir, &a);
    assert!(ok, "rdy {args:?} failed: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("bad json from {args:?}: {e}\n{out}"))
}

pub fn rdy_env(dir: &std::path::Path, editor: &str, args: &[&str]) -> (bool, String, String) {
    let (_cfg, mut cmd) = rdy_cmd(dir, args);
    let out = cmd.env("EDITOR", editor).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

pub fn project_with_term() -> (tempfile::TempDir, String) {
    use rdy_engine::*;
    use rdy_model::Role;
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let res = r.write(&w, WriteRequest { view: "work".into(), ops: serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Hello", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let id = res.created["$t"].clone();
    drop(r);
    (dir, id)
}
