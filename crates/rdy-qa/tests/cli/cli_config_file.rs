//! mfb1: the project config is .rdy.yaml at the project root (FW-X5, D-C5955); rdy reads no
//! other project file (D-W7RBV). A directory with .rdy/ and no .rdy.yaml is E_NO_PROJECT naming
//! the file. rdy init writes .rdy.yaml and no .gitignore (founder ruling).
use rdy_engine::config_levels::{PROJECT_FILE, effective};
use rdy_engine::*;
use rdy_qa::fixtures::{human_env, rdy_bin};
use rdy_qa::wc::{json, ok, put, seeded};
use std::path::Path;
use std::process::Command;

const NO_RDY_YAML_FIX: &str = "create .rdy.yaml in the project directory with the line `pack: rdy-standard` (or the pack this project uses)";

/// `rdy <args>` run in `cwd` with no --project flag, as human:qa, with no daemon.
fn rdy_in(cwd: &Path, args: &[&str]) -> std::process::Output {
    let (_cfg, env) = human_env("human:qa");
    Command::new(rdy_bin())
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env("RDY_NO_DAEMON", "1")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .output()
        .expect("run rdy")
}

#[test]
fn mfb1_init_writes_rdy_yaml_at_the_root() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    assert_eq!(PROJECT_FILE, ".rdy.yaml");
    assert_eq!(
        std::fs::read_to_string(d.path().join(".rdy.yaml")).unwrap(),
        "pack: rdy-standard\n"
    );
    assert!(!d.path().join(".rdy/config.yaml").exists());
    // founder ruling: rdy writes no gitignore
    assert!(!d.path().join(".rdy/.gitignore").exists());
    let e = Rdy::init(d.path(), "rdy-standard")
        .err()
        .expect("a second init is refused");
    assert_eq!(e.code, E_ALREADY_EXISTS);
}

#[test]
fn mfb1_a_project_without_rdy_yaml_is_e_no_project_naming_the_file() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::remove_file(d.path().join(".rdy.yaml")).unwrap();
    // the old layout is never read
    std::fs::write(d.path().join(".rdy/config.yaml"), "pack: rdy-standard\n").unwrap();
    let e = Rdy::open(d.path()).err().expect("no project config");
    assert_eq!(e.code, E_NO_PROJECT);
    assert_eq!(
        e.message,
        format!(
            "{} has no .rdy.yaml, the project config",
            d.path().display()
        )
    );
    assert_eq!(e.fix, NO_RDY_YAML_FIX);
    let e = effective(d.path(), None).expect_err("the levels refuse too");
    assert_eq!(e.code, E_NO_PROJECT);
    assert_eq!(e.fix, NO_RDY_YAML_FIX);
}

#[test]
fn mfb1_a_directory_without_rdy_is_still_not_a_project() {
    let d = tempfile::tempdir().unwrap();
    let e = Rdy::open(d.path()).err().expect("not a project");
    assert_eq!(e.code, E_NO_PROJECT);
    assert_eq!(
        e.message,
        format!("{} is not an rdy project", d.path().display())
    );
    assert_eq!(e.fix, "run rdy init in the project directory");
}

#[test]
fn mfb1_the_cli_names_the_missing_rdy_yaml_before_starting_a_daemon() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::remove_file(d.path().join(".rdy.yaml")).unwrap();
    let sub = d.path().join("guides/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let mut cmd = Command::new(rdy_bin());
    let (_cfg, env) = human_env("human:qa");
    let out = cmd
        .args(["--json", "status"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(&sub)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E_NO_PROJECT"), "{err}");
    assert!(
        err.contains("has no .rdy.yaml, the project config"),
        "{err}"
    );
    assert!(
        !d.path().join(".rdy/run/rdy.sock").exists(),
        "no daemon was started"
    );
}

#[test]
fn mfb1_the_cli_finds_the_project_from_a_subdirectory_and_reads_rdy_yaml() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let mut t = std::fs::read_to_string(d.path().join(PROJECT_FILE)).unwrap();
    t.push_str("views:\n  idle_days: 3\n");
    std::fs::write(d.path().join(PROJECT_FILE), t).unwrap();
    let sub = d.path().join("guides/deep");
    std::fs::create_dir_all(&sub).unwrap();
    let out = rdy_in(&sub, &["--json", "config", "--show-origin"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = |k: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == k)
            .unwrap_or_else(|| panic!("{k}: {rows}"))
            .clone()
    };
    assert_eq!(row("views.idle_days")["value"], 3);
    assert!(
        row("views.idle_days")["file"]
            .as_str()
            .is_some_and(|f| f.ends_with(".rdy.yaml")),
        "{rows}"
    );
    assert!(
        row("pack")["file"]
            .as_str()
            .is_some_and(|f| f.ends_with(".rdy.yaml")),
        "{rows}"
    );
}

#[test]
fn mfb1_code_repos_and_decisions_home_are_read_from_rdy_yaml() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let mut t = std::fs::read_to_string(d.path().join(PROJECT_FILE)).unwrap();
    t.push_str("code_repos: 7\n");
    std::fs::write(d.path().join(PROJECT_FILE), t).unwrap();
    let e = code_index::code_repos(d.path()).expect_err("a wrong type");
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(
        e.fix,
        "fix code_repos in .rdy.yaml, e.g. code_repos: [../app]"
    );
}

/// guard: dot entries were never docs; this pins it for the files the move puts at the root.
#[test]
fn mfb1_rdy_yaml_and_rdyignore_are_never_docs_or_untracked() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(&s, ".rdyignore", "tmp/\n");
    assert!(s.dir.path().join(PROJECT_FILE).exists());
    let (okk, v) = json(&s, &["status"]);
    assert!(okk, "{v}");
    let untracked = v["working_copy"]["untracked"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        untracked.iter().all(|u| {
            let p = u["path"].as_str().unwrap_or("");
            p != ".rdy.yaml" && p != ".rdyignore"
        }),
        "{v}"
    );
    let text = ok(&s, &["status"]);
    assert!(!text.contains(".rdy.yaml"), "{text}");
}

/// orchestrator addition: `rdy check` in a directory with .rdy/ and no .rdy.yaml names the
/// missing file, with or without --pack; a directory with no .rdy/ keeps its text.
#[test]
fn mfb1_check_in_an_old_layout_names_rdy_yaml() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::remove_file(d.path().join(PROJECT_FILE)).unwrap();
    std::fs::write(d.path().join(".rdy/config.yaml"), "pack: rdy-standard\n").unwrap();
    std::fs::write(d.path().join("a.md"), "# A\n").unwrap();
    for args in [
        &["--json", "check", "a.md"][..],
        &["--json", "check", "a.md", "--pack", "rdy-standard"][..],
    ] {
        let out = rdy_in(d.path(), args);
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("E_NO_PROJECT"), "{args:?}: {err}");
        assert!(
            err.contains("has no .rdy.yaml, the project config"),
            "{args:?}: {err}"
        );
        assert!(err.contains(NO_RDY_YAML_FIX), "{args:?}: {err}");
    }
    let bare = tempfile::tempdir().unwrap();
    std::fs::write(bare.path().join("a.md"), "# A\n").unwrap();
    let out = rdy_in(bare.path(), &["--json", "check", "a.md"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not inside an rdy project"), "{err}");
}
