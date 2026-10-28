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

const NO_STORE_FIX: &str =
    "move .rdy.yaml aside, run rdy init, then copy your settings back into the new .rdy.yaml";

/// `rdy <args>` run in `cwd` with no --project flag, as human:qa, with daemons allowed (stdin
/// is closed, so `rdy mcp` ends at once).
fn rdy_daemon_in(cwd: &Path, args: &[&str]) -> std::process::Output {
    let (_cfg, env) = human_env("human:qa");
    Command::new(rdy_bin())
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .output()
        .expect("run rdy")
}

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
    assert!(!err.contains("E_DAEMON_START"), "{err}");
    // a client creates .rdy/run before it spawns a daemon (`RunDir::ensure`); init creates none
    assert!(!d.path().join(".rdy/run").exists(), "no daemon was started");
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
    assert_eq!(e.fix, "fix code_repos in .rdy.yaml, e.g. code_repos: [app]");
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

/// Finding 1 (review of mfb1): `rdy mcp` checks for a project before it starts a daemon, as
/// every other command does. An old layout gets the E_NO_PROJECT text naming .rdy.yaml, and a
/// directory that is no project gets "not an rdy project"; neither gets a .rdy/run directory.
#[test]
fn mfb1_mcp_in_an_old_layout_names_rdy_yaml_and_starts_no_daemon() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::remove_file(d.path().join(PROJECT_FILE)).unwrap();
    std::fs::write(d.path().join(".rdy/config.yaml"), "pack: rdy-standard\n").unwrap();
    let out = rdy_daemon_in(d.path(), &["mcp"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E_NO_PROJECT"), "{err}");
    assert!(
        err.contains("has no .rdy.yaml, the project config"),
        "{err}"
    );
    assert!(err.contains(NO_RDY_YAML_FIX), "{err}");
    assert!(!err.contains("E_DAEMON_START"), "{err}");
    assert!(!d.path().join(".rdy/run").exists(), "no daemon was started");

    let bare = tempfile::tempdir().unwrap();
    let out = rdy_daemon_in(bare.path(), &["mcp"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E_NO_PROJECT"), "{err}");
    assert!(err.contains("is not an rdy project"), "{err}");
    assert!(!err.contains("E_DAEMON_START"), "{err}");
    assert!(!bare.path().join(".rdy").exists(), "rdy mcp created .rdy");
}

/// Ruling (task 1, finding 2): a directory with .rdy.yaml and no store gets one text from the
/// CLI's project check, the engine's open and rdy init, which leaves .rdy.yaml as it is. With a
/// stray .rdy/run directory the text is the same, and no daemon is started.
#[test]
fn mfb1_rdy_yaml_without_a_store_says_so() {
    const CFG: &str = "pack: rdy-standard\nname: Kept settings\nviews:\n  idle_days: 4\n";
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(PROJECT_FILE), CFG).unwrap();
    let text = format!("{} has .rdy.yaml but no store", d.path().display());
    let e = Rdy::open(d.path()).err().expect("no store");
    assert_eq!(e.code, E_NO_PROJECT, "{e}");
    assert_eq!(e.message, text);
    assert_eq!(e.fix, NO_STORE_FIX);
    let e = Rdy::init(d.path(), "rdy-standard")
        .err()
        .expect("init keeps .rdy.yaml");
    assert_eq!(e.code, E_NO_PROJECT, "{e}");
    assert_eq!(e.message, text);
    assert_eq!(e.fix, NO_STORE_FIX);
    assert!(
        !d.path().join(".rdy").exists(),
        "a refused init creates nothing"
    );

    let stray = tempfile::tempdir().unwrap();
    std::fs::write(stray.path().join(PROJECT_FILE), CFG).unwrap();
    std::fs::create_dir_all(stray.path().join(".rdy/run")).unwrap();
    // final review I2: a clone with .rdy/ ignored has no custom pack (it lives in .rdy/packs/);
    // the person still gets the store text, never a pack error about a correct pack line
    const CUSTOM: &str = "pack: house-style\nname: Kept settings\n";
    let custom = tempfile::tempdir().unwrap();
    std::fs::write(custom.path().join(PROJECT_FILE), CUSTOM).unwrap();
    for (dir, cfg) in [
        (d.path(), CFG),
        (stray.path(), CFG),
        (custom.path(), CUSTOM),
    ] {
        let runs = [
            ("status", rdy_daemon_in(dir, &["--json", "status"])),
            ("no-daemon status", rdy_in(dir, &["--json", "status"])),
            ("init", rdy_in(dir, &["--json", "init"])),
            (
                "check",
                rdy_in(dir, &["--json", "check", "a.md", "--pack", "rdy-standard"]),
            ),
        ];
        for (what, out) in runs {
            assert!(!out.status.success(), "{what}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("E_NO_PROJECT"), "{what}: {err}");
            assert!(err.contains("has .rdy.yaml but no store"), "{what}: {err}");
            assert!(err.contains(NO_STORE_FIX), "{what}: {err}");
            assert!(!err.contains("E_DAEMON_START"), "{what}: {err}");
        }
        assert_eq!(
            std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap(),
            cfg,
            "rdy init never touches .rdy.yaml"
        );
        assert!(!dir.join(".rdy/store").exists(), "no store was created");
        assert!(!dir.join(".rdy/run/rdy.sock").exists());
        assert!(
            !dir.join(".rdy/run/daemon.log").exists(),
            "no daemon was started"
        );
    }
    assert!(!d.path().join(".rdy").exists(), "nothing created .rdy");
}

/// Every file under `dir` with its bytes, sorted by path.
fn snapshot(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = vec![];
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push((
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

/// Ruling (final review I1): `rdy init` never adopts an existing store and never guesses a
/// pack. A directory with a store and no .rdy.yaml (the old layout) is refused, and init writes
/// nothing.
#[test]
fn mfb1_init_refuses_a_store_without_rdy_yaml() {
    const OLD: &str = "pack: rdy-standard\nname: My Project\ndecisions:\n  home: wallets/tx\n";
    const FIX: &str = "write .rdy.yaml with this project's settings (pack, name, decisions.home); rdy init does not adopt an existing store";
    let d = tempfile::tempdir().unwrap();
    {
        let mut r = Rdy::init(d.path(), "rdy-standard").unwrap();
        rdy_qa::edit_loop::spec_doc(&mut r);
        r.close().unwrap();
    }
    std::fs::remove_file(d.path().join(PROJECT_FILE)).unwrap();
    std::fs::write(d.path().join(".rdy/config.yaml"), OLD).unwrap();
    let store = snapshot(&d.path().join(".rdy/store"));
    let text = format!("{} has an rdy store but no .rdy.yaml", d.path().display());
    let e = Rdy::init(d.path(), "rdy-standard")
        .err()
        .expect("init does not adopt a store");
    assert_eq!(e.code, E_ALREADY_EXISTS, "{e}");
    assert_eq!(e.message, text);
    assert_eq!(e.fix, FIX);
    let out = rdy_in(d.path(), &["--json", "init"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E_ALREADY_EXISTS"), "{err}");
    assert!(err.contains("has an rdy store but no .rdy.yaml"), "{err}");
    assert!(err.contains(FIX), "{err}");
    assert!(
        !d.path().join(PROJECT_FILE).exists(),
        "init wrote no .rdy.yaml"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join(".rdy/config.yaml")).unwrap(),
        OLD
    );
    assert_eq!(
        snapshot(&d.path().join(".rdy/store")),
        store,
        "the store is untouched"
    );
}

/// rg6: a pack whose version is empty is refused when it loads, in its file, so no record of
/// the project ever names it (an assessment keeps a pack name and version of 1 to 256 bytes).
#[test]
fn rg6_rdy_status_refuses_a_pack_with_an_empty_version() {
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    let pack = d.path().join(".rdy/packs/team.yaml");
    std::fs::create_dir_all(pack.parent().unwrap()).unwrap();
    std::fs::write(&pack, "name: team\nversion: ''\nextends: rdy-standard\n").unwrap();
    std::fs::write(d.path().join(PROJECT_FILE), "pack: team\n").unwrap();
    let out = rdy_in(d.path(), &["--json", "status"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E_CONFIG"), "{err}");
    assert!(
        err.contains(".rdy/packs/team.yaml: invalid pack: the pack version must be 1 to 256 bytes"),
        "{err}"
    );
    assert!(err.contains("fix the pack file"), "{err}");
}
