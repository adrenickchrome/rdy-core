//! ctl2, the read half: each op reads one snapshot; paths resolve against the session cwd;
//! status reports the open and quarantine files; errors carry the M1 shape.
use rdy_daemon::{ErrorReply, OPS, OpContext, dispatch, quarantine};
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::{E_BAD_INPUT, Rdy, RdyError, Session, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, Local) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, Local::new(r))
}

fn human() -> Session {
    Session::human("human:t".into(), Role::Writer)
}

fn doc(e: &Local, name: &str) -> Value {
    let name = name.to_string();
    write_typed(e, move |r| {
        let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}])).unwrap();
        r.write(&human(), WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap()
}

fn cx<'a>(e: &'a Local, s: &'a Session, cwd: &'a std::path::Path) -> OpContext<'a> {
    OpContext {
        engine: e,
        session: s,
        cwd,
    }
}

#[test]
fn ctl2_ops_are_named_and_an_unknown_op_names_them() {
    assert_eq!(OPS.len(), 20);
    assert_eq!(OPS[0], "session");
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "frobnicate", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(
        err.fix.contains("status") && err.fix.contains("approve"),
        "{err}"
    );
}

#[test]
fn ctl2_status_reports_views_owners_seq_open_report_and_quarantine() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    let v = dispatch(&cx(&e, &s, d.path()), "status", json!({})).unwrap();
    assert_eq!(v["landings"], 0);
    let work = &v["views"][0];
    assert_eq!(work["name"], "work");
    assert_eq!(work["changes"], 1);
    assert_eq!(work["owner"]["actor"], "human:t");
    assert_eq!(work["owner"]["run"], "cli");
    assert!(v["seq"]["offset"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["open_report"]["index_rebuilt"], false);
    assert_eq!(v["quarantine"], json!([]));
    // a quarantine file appears in the listing
    std::fs::write(
        d.path().join(".rdy/store/segments/seg-000000.log.torn-123"),
        b"x",
    )
    .unwrap();
    std::fs::write(d.path().join(".rdy/store/index.redb.damaged-1"), b"x").unwrap();
    std::fs::write(d.path().join(".rdy/store/unrelated"), b"x").unwrap();
    assert_eq!(
        quarantine(d.path()),
        vec![
            "index.redb.damaged-1".to_string(),
            "segments/seg-000000.log.torn-123".to_string()
        ]
    );
    let v = dispatch(&cx(&e, &s, d.path()), "status", json!({})).unwrap();
    assert_eq!(v["quarantine"].as_array().unwrap().len(), 2);
}

#[test]
fn ctl2_show_ls_impact_tags_log_read_canon_by_default_and_a_view_on_request() {
    let (d, e) = project();
    let s = human();
    let w = doc(&e, "a");
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let c = cx(&e, &s, d.path());
    let err = dispatch(&c, "show", json!({"id": id})).unwrap_err();
    assert!(
        err.code == rdy_engine::E_UNKNOWN_BLOCK || err.code == rdy_engine::E_NOT_FOUND,
        "canon has no such block yet: {err}"
    );
    let v = dispatch(&c, "show", json!({"id": id, "scope": "work"})).unwrap();
    assert_eq!(v["id"], id);
    let v = dispatch(&c, "ls", json!({"scope": "work"})).unwrap();
    assert_eq!(v[0]["name"], "a");
    let v = dispatch(&c, "ls", json!({})).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0, "canon is empty");
    let v = dispatch(&c, "impact", json!({"id": id, "scope": "work"})).unwrap();
    assert!(v.is_array());
    let v = dispatch(&c, "tags", json!({})).unwrap();
    assert_eq!(v["versions"], json!([]));
    let v = dispatch(&c, "log", json!({})).unwrap();
    assert_eq!(v, json!([]));
    let err = dispatch(&c, "ls", json!({"scope": "work", "bogus": 1})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(err.message.starts_with("ls:"), "{err}");
}

#[test]
fn ctl2_block_returns_yaml_for_the_editor() {
    let (d, e) = project();
    let s = human();
    let w = doc(&e, "a");
    let id = w["created"]["$d"].as_str().unwrap().to_string();
    let v = dispatch(
        &cx(&e, &s, d.path()),
        "block",
        json!({"id": id, "view": "work"}),
    )
    .unwrap();
    assert_eq!(v["id"], id);
    assert!(v["yaml"].as_str().unwrap().contains("title: D"), "{v}");
    assert!(v["version"].as_str().unwrap().len() >= 12);
}

#[test]
fn ctl2_render_export_and_check_resolve_relative_paths_against_the_session_cwd() {
    let (d, e) = project();
    let s = human();
    doc(&e, "a");
    write_typed(&e, |r| {
        r.propose(&human(), "work", "p")
            .and_then(|p| r.land_report(&Session::human("human:t".into(), Role::Approver), &p.id))
    })
    .unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let c = cx(&e, &s, cwd.path());
    let v = dispatch(&c, "export", json!({"out": "exp"})).unwrap();
    assert_eq!(v["docs"], 1);
    assert!(cwd.path().join("exp").is_dir(), "export went to cwd/exp");
    let v = dispatch(&c, "render", json!({"out": "site"})).unwrap();
    assert!(v["pages"].as_u64().unwrap() >= 1, "{v}");
    assert!(cwd.path().join("site").is_dir());
    let v = dispatch(&c, "check", json!({"path": "exp"})).unwrap();
    assert_eq!(v, json!([]), "a fresh export lints clean");
    let abs = d.path().join("exp2");
    dispatch(&c, "export", json!({"out": abs.to_string_lossy()})).unwrap();
    assert!(abs.is_dir(), "an absolute path is used as given");
}

#[test]
fn ctl2_an_rdy_error_becomes_an_error_reply_with_issues_and_corrected() {
    let mut e = RdyError::new(E_BAD_INPUT, "m", "f");
    e.issues.push(rdy_schema::Issue {
        severity: rdy_schema::Severity::Error,
        code: "X",
        block: None,
        message: "i".into(),
        fix: "fx".into(),
    });
    let e = e.with_corrected(json!({"a": 1}));
    let r: ErrorReply = e.into();
    assert_eq!(
        (r.code.as_str(), r.message.as_str(), r.fix.as_str()),
        ("E_BAD_INPUT", "m", "f")
    );
    assert_eq!(r.extra["issues"][0]["code"], "X");
    assert_eq!(r.extra["corrected"]["a"], 1);
    let r2: ErrorReply = RdyError::new(E_BAD_INPUT, "m", "f").into();
    assert!(!r2.extra.contains_key("issues"));
}

#[test]
fn ctl2_the_session_op_is_answered_by_the_host_not_by_dispatch() {
    let (d, e) = project();
    let s = human();
    let err = dispatch(&cx(&e, &s, d.path()), "session", json!({})).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(
        err.message,
        "the session op is answered by the daemon connection, not by dispatch"
    );
}
