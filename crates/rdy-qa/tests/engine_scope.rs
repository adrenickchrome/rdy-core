//! Read scopes naming views that don't exist (scope1).
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::process::Command;

fn seeded() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let doc = write(&mut r, "work", json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Kept", "doc_type": "glossary"}}]))
        .unwrap()
        .created["$d"]
        .clone();
    let landing = land(&mut r, "work").unwrap();
    (d, r, doc, landing)
}

#[test]
fn scope1_an_unknown_view_is_not_found_and_a_hash_gets_the_landing_hint() {
    let (_d, r, _doc, landing) = seeded();
    let e = r.read().resolve_read_scope("nope", None).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    let e = r.read().resolve_read_scope(&landing, None).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert!(e.fix.contains(&format!("landing:{landing}")), "{e}");
    let e = r
        .read()
        .resolve_read_scope(&landing[..12], None)
        .unwrap_err();
    assert!(e.fix.contains("landing:"), "{e}");
    assert_eq!(
        r.read()
            .resolve_read_scope(&format!("landing:{landing}"), None)
            .unwrap(),
        Scope::At(landing.parse().unwrap())
    );
    assert_eq!(
        r.read().resolve_read_scope("work", None).unwrap(),
        Scope::View("work".into()),
        "a view with a record"
    );
    assert_eq!(
        r.read().resolve_read_scope("mine", Some("mine")).unwrap(),
        Scope::View("mine".into()),
        "the session's own view, created lazily"
    );
    assert_eq!(
        r.read().resolve_read_scope("canon", None).unwrap(),
        Scope::Canon
    );
}

/// Includes Review Focus 5: a reader on canon passing a landing hash as scope.
#[test]
fn scope1_mcp_and_cli_reads_refuse_an_unknown_view() {
    // CLI first: it needs the store unlocked
    let (dir, r, doc, landing) = seeded();
    drop(r);
    let out = Command::new(rdy_bin())
        .args(["show", doc.as_str(), "--scope", "nope"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E_NOT_FOUND"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // MCP: a writer's own (unwritten) view reads fine; an unknown view doesn't
    let mut st = McpState::new(Rdy::open(dir.path()).unwrap());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "r1"})).unwrap(),
    )
    .unwrap();
    let e = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "nope"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:reader", "role": "reader", "view": "canon"}))
            .unwrap(),
    )
    .unwrap();
    let e = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": landing})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert!(e.fix.contains("landing:"), "{e}");
}
