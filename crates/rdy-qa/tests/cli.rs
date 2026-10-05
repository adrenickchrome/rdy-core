use rdy_qa::fixtures::*;
use std::process::Command;

fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(rdy_bin())
        .args(args)
        .current_dir(dir)
        .env("RDY_ACTOR", "human:qa")
        .env("EDITOR", "true")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn cli_errors_are_clean_on_every_command() {
    let (dir, r) = project();
    drop(r);
    let d = dir.path();
    let file = d.join("a-file");
    std::fs::write(&file, "x").unwrap();
    let f = file.to_str().unwrap();
    let h64 = "a".repeat(64);
    let bad: Vec<(&str, Vec<&str>)> = vec![
        ("E_UNKNOWN_BLOCK", vec!["show", "c-ZZZZZ"]),
        ("E_UNKNOWN_BLOCK", vec!["show", "no/such/path"]),
        ("E_UNKNOWN_BLOCK", vec!["show", "с-ABCDE"]),
        ("E_UNKNOWN_BLOCK", vec!["impact", "c-ZZZZZ"]),
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
        (
            "E_NO_PROJECT",
            vec!["status", "--project", "/no/such/project"],
        ),
    ];
    for (code, args) in &bad {
        let (ok, out, err) = rdy(d, args);
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
        let (ok, _, err) = rdy(d, &j);
        assert!(!ok);
        let v: serde_json::Value = serde_json::from_str(err.trim())
            .unwrap_or_else(|e| panic!("{args:?}: stderr is not JSON ({e}): {err}"));
        assert_eq!(v["code"].as_str(), Some(*code), "{args:?}: {v}");
    }
    for args in [
        vec!["status"],
        vec!["log"],
        vec!["tags"],
        vec!["ls"],
        vec!["fsck"],
        vec!["pack", "check"],
    ] {
        let mut j = vec!["--json"];
        j.extend(args.iter());
        let (ok, out, err) = rdy(d, &j);
        assert!(ok, "{args:?}: {err}");
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("{args:?}: stdout is not JSON ({e}): {out}"));
    }
}
