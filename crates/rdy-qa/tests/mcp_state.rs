//! mcp4: tools run against the door; a run-less session gets its own view; overview reports the open.
use rdy_engine::engine::Local;
use rdy_mcp::tools::{self, McpState, ScopeParams, SessionParams, WriteParams};
use rdy_qa::fixtures::project;
use serde_json::json;
use std::sync::Arc;

fn state() -> (tempfile::TempDir, McpState) {
    let (d, r) = project();
    (d, McpState::new(Arc::new(Local::new(r))))
}

fn session(st: &mut McpState, actor: &str, run: Option<&str>) -> serde_json::Value {
    tools::rdy_session(
        st,
        SessionParams {
            actor: actor.into(),
            run: run.map(String::from),
            role: "writer".into(),
            view: None,
        },
    )
    .unwrap()
}

#[test]
fn mcp4_a_runless_session_defaults_to_work_dash_actor_and_a_run_to_the_run() {
    let (_d, mut st) = state();
    let v = session(&mut st, "agent:designer", None);
    assert_eq!(v["view"], "work-agent-designer");
    let v = session(&mut st, "agent:designer", Some("run:7"));
    assert_eq!(v["view"], "run-7");
    assert!(
        !st.session.as_ref().unwrap().is_human(),
        "MCP sessions are never human"
    );
}

#[test]
fn mcp4_a_write_through_the_door_is_visible_to_the_next_read() {
    let (_d, mut st) = state();
    session(&mut st, "agent:a", None);
    let ops = vec![
        json!({"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}),
    ];
    let w = tools::rdy_write(
        &mut st,
        WriteParams {
            view: None,
            ops,
            implements: None,
            editorial: None,
            message: None,
            brief: None,
            max_tokens: None,
        },
    )
    .unwrap();
    assert!(w["change"].is_string(), "{w}");
    let o = tools::rdy_overview(&mut st, ScopeParams::default()).unwrap();
    let views = o["views"].as_array().unwrap();
    assert!(
        views
            .iter()
            .any(|v| v["name"] == "work-agent-a" && v["changes"] == 1),
        "{o}"
    );
}

#[test]
fn mcp4_overview_reports_the_open() {
    let (_d, mut st) = state();
    session(&mut st, "agent:a", None);
    let o = tools::rdy_overview(&mut st, ScopeParams::default()).unwrap();
    let rep = &o["open_report"];
    assert_eq!(rep["index_rebuilt"], false, "{o}");
    assert_eq!(rep["quarantined"], json!([]));
    assert!(rep["caught_up_frames"].is_number());
}

#[test]
fn mcp4_an_agent_write_into_the_humans_view_is_e_view_owner() {
    let (_d, r) = project();
    let local = Arc::new(Local::new(r));
    rdy_engine::engine::write_typed(&*local, |r| {
        let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "fields": {"name": "h", "title": "H", "doc_type": "glossary"}}])).unwrap();
        r.write(&rdy_qa::fixtures::writer(), rdy_engine::WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap();
    let mut st = McpState::new(local);
    session(&mut st, "agent:a", None);
    let ops = vec![
        json!({"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}),
    ];
    let e = tools::rdy_write(
        &mut st,
        WriteParams {
            view: Some("work".into()),
            ops,
            implements: None,
            editorial: None,
            message: None,
            brief: None,
            max_tokens: None,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_VIEW_OWNER);
}

#[test]
fn mcp4_schema_lists_the_daemon_and_owner_errors() {
    let (_d, mut st) = state();
    session(&mut st, "agent:a", None);
    let s = tools::rdy_schema(&mut st, tools::SchemaParams::default())
        .unwrap()
        .to_string();
    for code in [
        "E_VIEW_OWNER",
        "E_DAEMON_START",
        "E_DAEMON_VERSION",
        "E_DAEMON_GONE",
    ] {
        assert!(s.contains(code), "{code} missing from rdy_schema");
    }
}
