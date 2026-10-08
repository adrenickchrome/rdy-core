//! mbrf1: the edit loop over MCP, through the daemon and in-process.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};

fn session(c: &mut McpClient, actor: &str) {
    let r = c.call("rdy_session", json!({"actor": actor, "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
}

fn served_stable() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = stable_spec(&mut r);
    r.close().unwrap();
    (s, spec)
}

fn upd(id: &impl ToString, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

#[test]
fn mbrf1_rdy_brief_then_a_stable_write_citing_its_token_lands_with_consequences() {
    let (s, sp) = served_stable();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:m");
    let b = c.call("rdy_brief", json!({"ids": [sp.a.to_string()]})).ok();
    assert!(
        b["governs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["decision"] == sp.decision.to_string()),
        "{b}"
    );
    let token = b["base"]["token"].as_str().unwrap().to_string();
    let w = c
        .call(
            "rdy_write",
            json!({"brief": token, "ops": upd(&sp.a, "A, by the agent.")}),
        )
        .ok();
    let cons = w["consequences"].as_array().unwrap();
    assert!(
        cons.iter().any(|x| x["block"] == sp.b.to_string()
            && x["kind"] == "suspect"
            && x["follow_up"] == "update"
            && x["id"].as_str().unwrap().starts_with("cq-")),
        "{w}"
    );
}

#[test]
fn mbrf1_a_stable_write_without_brief_is_refused_with_a_briefing_whose_token_lands_the_retry() {
    let (s, sp) = served_stable();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:n");
    let e = tool_error(c.call("rdy_write", json!({"ops": upd(&sp.a, "A, unbriefed.")})));
    assert_eq!(e["code"], "E_BRIEF_REQUIRED", "{e}");
    let token = e["briefing"]["base"]["token"].as_str().unwrap().to_string();
    assert!(e["fix"].as_str().unwrap().contains(&token), "{e}");
    assert_eq!(e["briefing"]["covers"], 1);
    let w = c
        .call(
            "rdy_write",
            json!({"brief": token, "ops": upd(&sp.a, "A, unbriefed.")}),
        )
        .ok();
    assert!(w["consequences"].is_array());
    let draft = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc",
        "fields": {"name": "scratch", "title": "Scratch", "doc_type": "overview"}}]}),
        )
        .ok();
    assert_eq!(
        draft["consequences"],
        json!([]),
        "always present, empty when there are none"
    );
}

#[test]
fn mbrf1_decide_supersede_and_revoke_take_brief_and_report_consequences() {
    let (dir, mut r) = project();
    let sp = stable_spec(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:d");
    let sec = sp.section.to_string();
    assert_eq!(
        c.call(
            "rdy_decide",
            json!({"parent": sec, "statement": "Prefer C"})
        )
        .code(),
        Some("E_BRIEF_REQUIRED")
    );
    let t = c.call("rdy_brief", json!({"ids": [sec]})).ok()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let d = c
        .call(
            "rdy_decide",
            json!({"parent": sec, "statement": "Prefer C", "brief": t}),
        )
        .ok();
    assert!(d["consequences"].is_array(), "{d}");
    let sup = c
        .call(
            "rdy_supersede",
            json!({"old": sp.decision.to_string(),
        "decision": {"parent": sec, "statement": "Keep A, firmly", "brief": t}}),
        )
        .ok();
    assert!(
        sup["consequences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["block"] == sp.a.to_string() && x["follow_up"] == "update"),
        "{sup}"
    );
    let rv = c
        .call(
            "rdy_revoke",
            json!({"target": sp.decision.to_string(), "reason": "no longer true",
        "parent": sec, "brief": t, "view": "rv"}),
        )
        .ok();
    assert!(rv["consequences"].is_array(), "{rv}");
}

#[test]
fn mbrf1_a_token_from_before_a_daemon_restart_is_stale_and_the_errors_token_lands() {
    let (s, sp) = served_stable();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:r");
    let t = c.call("rdy_brief", json!({"ids": [sp.a.to_string()]})).ok()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    drop(c);
    s.stop();
    let mut c = McpClient::start_proxy(&s);
    session(&mut c, "agent:r");
    let e = tool_error(c.call(
        "rdy_write",
        json!({"brief": t, "ops": upd(&sp.a, "A, after the restart.")}),
    ));
    assert_eq!(e["code"], "E_BRIEF_STALE", "{e}");
    assert!(e["message"].as_str().unwrap().contains("unknown"), "{e}");
    let t2 = e["briefing"]["base"]["token"].as_str().unwrap().to_string();
    assert_eq!(
        t2, t,
        "nothing moved, so the fresh briefing has the same token, now registered"
    );
    c.call(
        "rdy_write",
        json!({"brief": t2, "ops": upd(&sp.a, "A, after the restart.")}),
    )
    .ok();
}

#[test]
fn mbrf1_the_overview_names_the_project_and_the_schema_and_tools_document_the_edit_loop() {
    let (dir, r) = project();
    r.close().unwrap();
    std::fs::write(
        dir.path().join(".rdy/config.yaml"),
        "pack: rdy-standard\nname: Edit loop\ndescription: One line about it.\n",
    )
    .unwrap();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:o");
    let o = c.call("rdy_overview", json!({})).ok();
    assert_eq!(
        o["project"],
        json!({"name": "Edit loop", "description": "One line about it."})
    );
    let sch = c.call("rdy_schema", json!({})).ok();
    for code in ["E_BRIEF_REQUIRED", "E_BRIEF_STALE", "W_BRIEF_STALE"] {
        assert!(sch["errors"][code]["fix"].is_string(), "{code}: {sch}");
    }
    assert!(
        sch["edit_loop"]
            .as_str()
            .unwrap()
            .contains("brief: <token>")
    );
    let tools = c.tools();
    assert!(tools.iter().any(|t| t["name"] == "rdy_brief"));
    for name in ["rdy_write", "rdy_decide", "rdy_revoke"] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        let props = &t["inputSchema"]["properties"];
        assert!(
            props["brief"].is_object() && props["max_tokens"].is_object(),
            "{name}: {t}"
        );
    }
    let i = rdy_mcp::server::INSTRUCTIONS;
    assert!(
        i.contains("Read and write through rdy's tools, not the working-copy files"),
        "{i}"
    );
    assert!(
        i.contains("Brief before editing") && i.contains("must cite a briefing"),
        "{i}"
    );
}

#[test]
fn mbrf1_rdy_brief_needs_a_session_and_honours_max_tokens() {
    let (dir, mut r) = project();
    let sp = spec_doc(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    assert_eq!(
        c.call("rdy_brief", json!({"ids": [sp.a.to_string()]}))
            .code(),
        Some("E_NO_SESSION")
    );
    session(&mut c, "agent:t");
    let b = c
        .call("rdy_brief", json!({"path": "spec", "max_tokens": 50}))
        .ok();
    assert_eq!(b["truncated"], true, "{b}");
    assert_eq!(b["covers"], 6);
    assert!(b["base"]["token"].is_string());
    assert_eq!(
        c.call("rdy_brief", json!({"path": "spec", "max_tokens": 100_001}))
            .code(),
        Some("E_LIMIT")
    );
}
