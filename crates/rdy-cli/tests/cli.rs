use serde_json::Value;
use std::process::Command;

fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rdy")).args(args).current_dir(dir).env("RDY_ACTOR", "human:tester").output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

fn json(dir: &std::path::Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let (ok, out, err) = rdy(dir, &a);
    assert!(ok, "rdy {args:?} failed: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("bad json from {args:?}: {e}\n{out}"))
}

#[test]
fn init_status_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, _, err) = rdy(dir.path(), &["status"]);
    assert!(!ok);
    assert!(err.contains("E_NO_PROJECT") && err.contains("rdy init"), "{err}");
    json(dir.path(), &["init"]);
    let st = json(dir.path(), &["status"]);
    assert_eq!(st["pack"], "rdy-standard");
    assert_eq!(st["landings"], 0);
    let sub = dir.path().join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(json(&sub, &["status"])["pack"], "rdy-standard", "project found by walking up");
}

#[test]
fn import_propose_approve_render_fsck() {
    // build an export with the library, then drive the rest through the CLI
    let src = tempfile::tempdir().unwrap();
    let export = tempfile::tempdir().unwrap();
    {
        use rdy_engine::*;
        use rdy_model::Role;
        let mut r = Rdy::init(src.path(), "rdy-standard").unwrap();
        let w = Session { actor: "agent:a".into(), run: None, role: Role::Writer };
        r.write(&w, WriteRequest { view: "work".into(), ops: serde_json::from_value(serde_json::json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Hello", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}
        ])).unwrap(), justification: None, message: String::new() }).unwrap();
        r.export(&Scope::View("work".into()), export.path()).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    json(dir.path(), &["init"]);
    let ex = export.path().to_str().unwrap();
    // import needs an approver; the CLI import runs as approver because it can carry accepted decisions
    json(dir.path(), &["import", ex, "--view", "migrate"]);
    let p = json(dir.path(), &["propose", "--view", "migrate", "--title", "migrate"]);
    let pid = p["id"].as_str().unwrap().to_string();
    let landed = json(dir.path(), &["approve", &pid]);
    assert_eq!(landed["landing"].as_str().unwrap().len(), 64);
    let log = json(dir.path(), &["log"]);
    assert_eq!(log[0]["approved_by"], "human:tester");
    let site = dir.path().join("site");
    let r = json(dir.path(), &["render", "--out", site.to_str().unwrap()]);
    assert_eq!(r["pages"], 2);
    assert!(site.join("index.html").exists());
    let f = json(dir.path(), &["fsck"]);
    assert_eq!(f["landings"], 1);
    assert!(f["frames"].as_u64().unwrap() > 0);
}
