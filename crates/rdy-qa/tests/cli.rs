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
    let bad: Vec<Vec<&str>> = vec![
        vec!["show", "c-ZZZZZ"],
        vec!["show", "no/such/path"],
        vec!["show", "с-ABCDE"],
        vec!["impact", "c-ZZZZZ"],
        vec!["propose", "--title", "x"],
        vec!["approve", "zz"],
        vec!["approve", &h64],
        vec!["tag", "not-a-version"],
        vec!["tag", "v1.0.0"],
        vec!["render", "--out", f],
        vec!["export", "--out", f],
        vec!["import", "/no/such/dir", "--view", "x"],
        vec!["check", "/no/such/path"],
        vec!["new", "folder", "has space"],
        vec!["new", "doc", "index"],
        vec!["ls", "nope"],
        vec!["show", "--scope", "tag:v9.9.9", "c-ZZZZZ"],
        vec!["show", "--scope", "landing:zz", "c-ZZZZZ"],
        vec!["edit", "c-ZZZZZ"],
        vec!["status", "--project", "/no/such/project"],
    ];
    for args in &bad {
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
        assert!(
            v["code"].as_str().is_some_and(|c| c.starts_with("E_")),
            "{args:?}: {v}"
        );
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
