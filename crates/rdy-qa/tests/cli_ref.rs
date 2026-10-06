//! `rdy show` takes id@version and whole ref lines (refv1).
use rdy_qa::{fixtures::rdy_bin, golden::*};
use std::process::Command;

fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    // direct mode: no test leaves a detached daemon behind
    let out = Command::new(rdy_bin())
        .arg("--no-daemon")
        .args(args)
        .current_dir(dir)
        .env("RDY_ACTOR", "human:qa")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn refv1_rdy_show_reads_id_at_version_and_a_whole_ref_line() {
    let (dir, mut r, ids) = decided_site();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(&mut r, "work", serde_json::json!([{"op":"update","id":ids.requirement.to_string(),"body":"Ledgers are atomic always."}])).unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let id = ids.requirement.to_string();
    let (ok, out, e) = rdy(dir.path(), &["show", &format!("{id}@v1.0.0"), "--json"]);
    assert!(ok, "{e}");
    assert!(out.contains("when settled"), "{out}");
    let line = format!("[{id}@v1.0.0] Ledgers › Rules: \"Ledgers are atomic when settled.\"");
    let (ok, out, e) = rdy(dir.path(), &["show", &line, "--json"]);
    assert!(ok, "{e}");
    assert!(out.contains("when settled"), "{out}");
    let (ok, out, _) = rdy(dir.path(), &["show", &id, "--json"]);
    assert!(ok && out.contains("atomic always"), "{out}");
    let (ok, _, e) = rdy(
        dir.path(),
        &["show", &format!("{id}@v1.0.0"), "--scope", "canon"],
    );
    assert!(!ok);
    assert!(e.contains("E_BAD_INPUT"), "{e}");
    let (ok, _, e) = rdy(
        dir.path(),
        &["show", &format!("{id}@v1.0.0"), "--scope", "tag:v1.0.0"],
    );
    assert!(ok, "{e}");
}
