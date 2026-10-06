use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use std::process::{Command, Output};

/// `rdy <args>` against the served project as the human; a command that names its own
/// `--project` runs in the project directory with the human's environment and no other flags.
fn rdy(s: &Served, args: &[&str]) -> (bool, String, String) {
    let out: Output = if args.contains(&"--project") {
        Command::new(rdy_bin())
            .args(args)
            .current_dir(s.dir.path())
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(s.env_pairs())
            .output()
            .unwrap()
    } else {
        s.rdy(args)
    };
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn cli_errors_are_clean_on_every_command() {
    let s = Served::new();
    let d = s.dir.path();
    let file = d.join("a-file");
    std::fs::write(&file, "x").unwrap();
    let f = file.to_str().unwrap();
    let h64 = "a".repeat(64);
    let bad: Vec<(&str, Vec<&str>)> = vec![
        ("E_UNKNOWN_BLOCK", vec!["show", "c-ZZZZZ"]),
        ("E_UNKNOWN_BLOCK", vec!["show", "no/such/path"]),
        ("E_UNKNOWN_BLOCK", vec!["show", "с-ABCDE"]),
        ("E_UNKNOWN_BLOCK", vec!["impact", "c-ZZZZZ"]),
        ("E_NOT_FOUND", vec!["suspects", "--scope", "nope"]),
        ("E_BAD_INPUT", vec!["propose", "--title", "x"]),
        ("E_BAD_INPUT", vec!["approve", "zz"]),
        ("E_NOT_FOUND", vec!["approve", &h64]),
        ("E_BAD_TAG_NAME", vec!["tag", "not-a-version"]),
        ("E_NOT_FOUND", vec!["tag", "v1.0.0"]),
        // ren1: an existing file as --out is a bad input, not an I/O failure.
        ("E_BAD_INPUT", vec!["render", "--out", f]),
        ("E_IO", vec!["export", "--out", f]),
        ("E_BAD_INPUT", vec!["import", "/no/such/dir", "--view", "x"]),
        ("E_NOT_FOUND", vec!["check", "/no/such/path"]),
        ("E_BAD_NAME", vec!["new", "folder", "has space"]),
        ("E_BAD_NAME", vec!["new", "doc", "index"]),
        ("E_UNKNOWN_BLOCK", vec!["ls", "nope"]),
        (
            "E_NOT_FOUND",
            vec!["show", "--scope", "tag:v9.9.9", "c-ZZZZZ"],
        ),
        (
            "E_NOT_FOUND",
            vec!["show", "--scope", "landing:zz", "c-ZZZZZ"],
        ),
        ("E_UNKNOWN_BLOCK", vec!["edit", "c-ZZZZZ"]),
        ("E_NOT_FOUND", vec!["view", "show", "nope"]),
        ("E_NOT_FOUND", vec!["view", "fork", "nope", "x"]),
        ("E_BAD_INPUT", vec!["view", "include", "work", "zz"]),
        ("E_NOT_FOUND", vec!["view", "close", "nope"]),
        ("E_NOT_FOUND", vec!["undo", "--view", "nope"]),
        (
            "E_NO_PROJECT",
            vec!["status", "--project", "/no/such/project"],
        ),
    ];
    for (code, args) in &bad {
        let (ok, out, err) = rdy(&s, args);
        assert!(!ok, "{args:?} should fail: {out}");
        assert!(
            !err.contains("panicked") && !out.contains("panicked"),
            "{args:?}: {err}"
        );
        assert!(
            err.contains("error ") || err.contains("E_"),
            "{args:?}: {err}"
        );
        let mut j = vec!["--json"];
        j.extend(args.iter());
        let (ok, _, err) = rdy(&s, &j);
        assert!(!ok);
        let v: serde_json::Value = serde_json::from_str(err.trim())
            .unwrap_or_else(|e| panic!("{args:?}: stderr is not JSON ({e}): {err}"));
        assert_eq!(v["code"].as_str(), Some(*code), "{args:?}: {v}");
    }
    // a clap usage error (no daemon, no JSON): `view compare` needs a scope
    let (ok, out, err) = rdy(&s, &["view", "compare"]);
    assert!(!ok && out.is_empty(), "{out}");
    assert!(err.contains("Usage") && !err.contains("panicked"), "{err}");
    for args in [
        vec!["status"],
        vec!["log"],
        vec!["tags"],
        vec!["suspects"],
        vec!["ls"],
        vec!["fsck"],
        vec!["pack", "check"],
    ] {
        let mut j = vec!["--json"];
        j.extend(args.iter());
        let (ok, out, err) = rdy(&s, &j);
        assert!(ok, "{args:?}: {err}");
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("{args:?}: stdout is not JSON ({e}): {out}"));
    }
}
