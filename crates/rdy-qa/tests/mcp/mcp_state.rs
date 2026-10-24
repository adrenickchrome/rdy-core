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

fn try_session(
    st: &mut McpState,
    actor: &str,
    run: Option<&str>,
) -> Result<serde_json::Value, rdy_engine::RdyError> {
    tools::rdy_session(
        st,
        SessionParams {
            actor: actor.into(),
            run: run.map(String::from),
            role: "writer".into(),
            view: None,
            areas: None,
        },
    )
}

fn session(st: &mut McpState, actor: &str, run: Option<&str>) -> serde_json::Value {
    try_session(st, actor, run).unwrap()
}

#[test]
fn mcp4_a_runless_session_defaults_to_work_dash_actor_and_a_run_to_the_run() {
    let (_d, mut st) = state();
    let v = session(&mut st, "agent:designer", None);
    assert_eq!(v["view"], "work-agent-designer");
    // id3 (D-ME2C0): a run holds no ':'; `run:7` is refused, `run-7` is the run and the view
    let e = try_session(&mut st, "agent:designer", Some("run:7")).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "the run \"run:7\" is not in rdy's run form");
    let v = session(&mut st, "agent:designer", Some("run-7"));
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
        json!({"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}),
    ];
    let w = tools::rdy_write(
        &mut st,
        WriteParams {
            view: None,
            ops,
            implements: None,
            editorial: None,
            reflects: None,
            message: None,
            brief: None,
            max_tokens: None,
            intent: None,
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
        let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "fields": {"name": "h", "title": "H", "doc_type": "page"}}])).unwrap();
        r.write(&rdy_qa::fixtures::writer(), rdy_engine::WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
    })
    .unwrap();
    let mut st = McpState::new(local);
    session(&mut st, "agent:a", None);
    let ops = vec![
        json!({"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}),
    ];
    let e = tools::rdy_write(
        &mut st,
        WriteParams {
            view: Some("work".into()),
            ops,
            implements: None,
            editorial: None,
            reflects: None,
            message: None,
            brief: None,
            max_tokens: None,
            intent: None,
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

/// id3 (D-ME2C0): a refused `rdy_session` (an actor or run outside the form, or a human actor)
/// keeps the session, the view and the inbox subscription it had, and writes nothing. Runless
/// defaults stay distinct for `agent:a/b` and `agent:a-b`.
#[test]
fn id3_mcp_identity_refusal_keeps_the_prior_session() {
    let (_d, mut st) = state();
    let v = session(&mut st, "agent:keep", Some("k1"));
    assert_eq!(v["view"], "k1");
    let prior = (st.session.clone(), st.view.clone());
    let sub = |st: &McpState| {
        st.engine
            .read()
            .reader()
            .inbox("agent:keep", Some("k1"))
            .unwrap()
    };
    let prior_sub = sub(&st);
    assert!(prior_sub.is_some(), "the session subscribed");
    let seq = st.engine.read().seq();
    const ACTOR_FIX: &str =
        "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
    const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";
    let claim = (
        rdy_engine::E_PERMISSION,
        "an agent session cannot claim a human actor".to_string(),
        "claim your own agent: actor, and relay the person's words with rdy_answer",
    );
    for (actor, run, want) in [
        (
            "Agent:X",
            None,
            (
                rdy_engine::E_BAD_INPUT,
                "the actor \"Agent:X\" is not in rdy's actor form".to_string(),
                ACTOR_FIX,
            ),
        ),
        (
            "",
            Some("r1"),
            (
                rdy_engine::E_BAD_INPUT,
                "the actor \"\" is not in rdy's actor form".to_string(),
                ACTOR_FIX,
            ),
        ),
        (
            "agent:x",
            Some("run:7"),
            (
                rdy_engine::E_BAD_INPUT,
                "the run \"run:7\" is not in rdy's run form".to_string(),
                RUN_FIX,
            ),
        ),
        (
            "agent:x",
            Some("reaffirm-1"),
            (
                rdy_engine::E_BAD_INPUT,
                "the run \"reaffirm-1\" is not in rdy's run form".to_string(),
                RUN_FIX,
            ),
        ),
        ("human:qa", None, claim.clone()),
        ("human:someone", Some("r2"), claim.clone()),
    ] {
        let e = try_session(&mut st, actor, run).unwrap_err();
        assert_eq!(
            (e.code, e.message, e.fix.as_str()),
            want,
            "{actor:?} {run:?}"
        );
        assert_eq!(
            (&st.session, &st.view),
            (&prior.0, &prior.1),
            "{actor:?}: the session and view are kept"
        );
        assert_eq!(sub(&st), prior_sub, "{actor:?}: the subscription is kept");
        assert_eq!(
            st.engine.read().seq(),
            seq,
            "{actor:?}: nothing was written"
        );
    }
    // the human claim's run is checked first: a bad run gets the run's text
    let e = try_session(&mut st, "human:qa", Some("bad run")).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_BAD_INPUT, "{e}");
    // the base defaults: a runless agent's default keeps its slash, so these two differ
    assert_eq!(
        session(&mut st, "agent:a/b", None)["view"],
        "work-agent-a/b"
    );
    assert_eq!(
        session(&mut st, "agent:a-b", None)["view"],
        "work-agent-a-b"
    );
    assert_eq!(session(&mut st, "agent:a", Some("b"))["view"], "b");
}
