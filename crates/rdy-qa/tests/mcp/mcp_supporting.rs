//! rf5 (D-Z8P09): over MCP, rdy_write creates a finding or a source with no parent in the
//! decision home, exactly where rdy_decide records a decision, with rdy_decide's refusals when
//! there is no home or a bad one. The home is the briefing subject of such a create.
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    }
}

fn session(c: &mut McpClient) {
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:f", "run": "f1", "role": "writer"}),
    );
    assert!(r.code().is_none(), "{:?}", r.ok());
}

/// A project whose canon holds folder `decisions` with the stable doc `register`; `home` (if
/// any) is written as its decision home. Returns the project and the register's id.
fn project_with_home(home: Option<&str>) -> (tempfile::TempDir, String) {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "decisions", "title": "Decisions"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "register", "title": "Register", "doc_type": "overview", "lifecycle": "stable"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    if let Some(home) = home {
        let cfg = dir.path().join(".rdy.yaml");
        let text =
            std::fs::read_to_string(&cfg).unwrap() + &format!("decisions:\n  home: {home}\n");
        std::fs::write(&cfg, text).unwrap();
    }
    (dir, w.created["$d"].clone())
}

fn finding() -> Value {
    json!({"op": "create", "type": "finding", "id": "$f",
        "fields": {"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days."}]}})
}

fn source() -> Value {
    json!({"op": "create", "type": "source", "id": "$s",
        "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}})
}

/// The code, message and fix of a tool error.
fn refusal(e: &Value) -> (Value, Value, Value) {
    (e["code"].clone(), e["message"].clone(), e["fix"].clone())
}

#[test]
fn rf5_home_and_explicit_parent_match_decide() {
    // no home: rdy_write's refusal is rdy_decide's, word for word
    let (dir, _reg) = project_with_home(None);
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let decide = tool_error(c.call("rdy_decide", json!({"statement": "Retention is short"})));
    assert_eq!(decide["code"], "E_BAD_INPUT", "{decide}");
    assert_eq!(
        decide["message"],
        "no parent given, and the project names no decision home"
    );
    for op in [finding(), source()] {
        let e = tool_error(c.call("rdy_write", json!({"ops": [op]})));
        assert_eq!(refusal(&e), refusal(&decide), "{e}");
    }
    c.alive();
    drop(c);

    // a home that names no block: the same E_CONFIG
    let (dir, _reg) = project_with_home(Some("decisions/nowhere"));
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    let decide = tool_error(c.call("rdy_decide", json!({"statement": "Retention is short"})));
    assert_eq!(decide["code"], "E_CONFIG", "{decide}");
    for op in [finding(), source()] {
        let e = tool_error(c.call("rdy_write", json!({"ops": [op]})));
        assert_eq!(refusal(&e), refusal(&decide), "{e}");
    }
    drop(c);

    // the home: both types go where rdy_decide records, once the stable home is briefed
    let (dir, reg) = project_with_home(Some("decisions/register"));
    let mut c = McpClient::start(dir.path());
    session(&mut c);
    for (op, ty) in [(finding(), "finding"), (source(), "source")] {
        let e = tool_error(c.call("rdy_write", json!({"ops": [op.clone()]})));
        assert_eq!(e["code"], "E_BRIEF_REQUIRED", "{e}");
        assert_eq!(
            e["message"],
            format!("this write touches stable content ({reg}) and cites no briefing"),
            "{ty}"
        );
        let token = e["briefing"]["base"]["token"].as_str().unwrap().to_string();
        let w = c
            .call("rdy_write", json!({"ops": [op], "brief": token}))
            .ok();
        let id = w["created"].as_object().unwrap().values().next().unwrap();
        let ctx = c.call("rdy_context", json!({"id": id})).ok();
        assert_eq!(ctx["parent"], reg, "{ty}: {ctx}");
        assert_eq!(ctx["type"], ty, "{ctx}");
    }
    let b = c.call("rdy_brief", json!({"ids": [reg]})).ok();
    let token = b["base"]["token"].as_str().unwrap().to_string();
    let d = c
        .call(
            "rdy_decide",
            json!({"statement": "Retention follows the release", "brief": token}),
        )
        .ok();
    let did = d["created"]["$decision"].as_str().unwrap().to_string();
    assert_eq!(
        c.call("rdy_context", json!({"id": did})).ok()["parent"],
        reg
    );
    // an explicit parent is kept: a draft doc of the agent's own
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}]}),
        )
        .ok();
    let notes = w["created"]["$n"].as_str().unwrap().to_string();
    let mut op = source();
    op["parent"] = json!(notes);
    let w = c.call("rdy_write", json!({"ops": [op]})).ok();
    let sid = w["created"]["$s"].as_str().unwrap().to_string();
    assert_eq!(
        c.call("rdy_context", json!({"id": sid})).ok()["parent"],
        notes
    );
    c.alive();
}
