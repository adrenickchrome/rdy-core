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
            && x["follow_up"] == "confirm"
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
            .any(|x| x["block"] == sp.a.to_string() && x["follow_up"] == "confirm"),
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
        dir.path().join(".rdy.yaml"),
        "pack: rdy-standard\nname: Edit loop\ndescription: One line about it.\n",
    )
    .unwrap();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:o");
    let o = c.call("rdy_overview", json!({})).ok();
    // ec10: the project also names its repo id, 32 hex digits (O34)
    assert_eq!(o["project"]["name"], "Edit loop");
    assert_eq!(o["project"]["description"], "One line about it.");
    let id = o["project"]["repo_id"].as_str().unwrap();
    assert!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
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
    // rf5: the supporting types, in the instructions and the text guide
    assert!(
        i.contains("Finding: record what surfaced with rdy_write, type finding, statement and 1 to 20 evidence entries; cite it from a decision with depends_on"),
        "{i}"
    );
    assert!(
        sch["text"].as_str().unwrap().contains("A finding records a statement and evidence. Each evidence entry is {quote, session_ref?, question?} or {source: \"{{S-…}}\"}. Each string has 1 to 2000 characters. A source names a release, commit or spec with title, kind and locator. Both types live only in the store. With no parent, they use the decision home, or the project root when the project names none."),
        "{sch}"
    );
    assert!(
        i.contains("Brief before editing") && i.contains("must cite a briefing"),
        "{i}"
    );
    // rf12 (D-1876T): the Why sentence follows the Finding sentence, and the briefing guide
    // places the new sections and says where trace differs
    assert!(
        i.contains("cite it from a decision with depends_on. Why: to learn why a block says what it says, call rdy_trace."),
        "{i}"
    );
    let brief = sch["brief"].as_str().unwrap();
    assert!(
        brief.ends_with("Governs includes proposed decisions and decisions that affect a described entity; through names that entity. Sources follow suspects. Findings follow related_decisions. Trace explains why. A briefing guards a write and stays one level deep."),
        "{brief}"
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

// ---- plb1: the parent a create needs briefed, over MCP (PL 58) ----

/// The token of a briefing of `id`, made over MCP.
fn brief_token(c: &mut McpClient, id: &str) -> String {
    c.call("rdy_brief", json!({"ids": [id]})).ok()["base"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Checks a refused create under `parent` (a new `ty`) whose top message ends in `tail`, and
/// returns the token of the fresh briefing the error carries.
fn refused_create(e: &Value, parent: &str, ty: &str, tail: &str) -> String {
    assert_eq!(e["code"], "E_BRIEF_REQUIRED", "{e}");
    assert_eq!(
        e["message"],
        format!(
            "the cited briefing does not cover {parent} (the parent of the new {ty}), which this write touches on stable content{tail}"
        ),
        "{e}"
    );
    assert_eq!(
        e["issues"],
        json!([{"severity": "error", "code": "E_BRIEF_REQUIRED", "block": parent,
            "message": format!("not covered by the cited briefing: the write creates a {ty} under it"),
            "fix": "brief this parent; a briefing of a parent covers its new children"}]),
        "{e}"
    );
    let t = e["briefing"]["base"]["token"].as_str().unwrap().to_string();
    assert_eq!(
        e["fix"],
        format!(
            "retry with brief: \"{t}\"; this error's briefing covers exactly the blocks the write touches"
        ),
        "{e}"
    );
    assert_eq!(e["briefing"]["covers"], 1, "only the parent: {e}");
    t
}

#[test]
fn plb1_parent_diagnostics_cross_the_mcp_boundary() {
    let (dir, mut r) = project();
    let sp = stable_spec(&mut r);
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:p");
    let (sec, old) = (sp.section.to_string(), sp.decision.to_string());
    let hint =
        format!(". The briefing covers {old}, but a new decision needs a briefing of its parent.");
    let ta = brief_token(&mut c, &sp.a.to_string());
    let td = brief_token(&mut c, &old);

    let ops = json!([{"op": "create", "type": "prose", "parent": sec, "body": "New prose."}]);
    let e = tool_error(c.call("rdy_write", json!({"view": "pw", "brief": ta, "ops": ops})));
    let t = refused_create(&e, &sec, "prose", "");
    c.call("rdy_write", json!({"view": "pw", "brief": t, "ops": ops}))
        .ok();

    let decide =
        |brief: &str| json!({"view": "pd", "parent": sec, "statement": "Prefer C", "brief": brief});
    let e = tool_error(c.call("rdy_decide", decide(&td)));
    let t = refused_create(&e, &sec, "decision", "");
    c.call("rdy_decide", decide(&t)).ok();

    let supersede = |view: &str, brief: &str| {
        json!({"old": old, "decision": {"view": view, "parent": sec,
            "statement": "Keep A, firmly", "brief": brief}})
    };
    let e = tool_error(c.call("rdy_supersede", supersede("ps", &td)));
    let t = refused_create(&e, &sec, "decision", &hint);
    c.call("rdy_supersede", supersede("ps", &t)).ok();

    let revoke = |view: &str, brief: &str| {
        json!({"target": old, "reason": "no longer true", "parent": sec,
            "brief": brief, "view": view})
    };
    let e = tool_error(c.call("rdy_revoke", revoke("pr", &td)));
    let t = refused_create(&e, &sec, "decision", &hint);
    c.call("rdy_revoke", revoke("pr", &t)).ok();

    // a briefing of A shows the decision that governs A as context, but does not cover it
    let b = c.call("rdy_brief", json!({"ids": [sp.a.to_string()]})).ok();
    assert_eq!(b["base"]["token"], ta.as_str(), "nothing moved in canon");
    assert!(
        b["governs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["decision"] == old),
        "{b}"
    );
    let e = tool_error(c.call("rdy_supersede", supersede("pc", &ta)));
    let t = refused_create(&e, &sec, "decision", "");
    c.call("rdy_supersede", supersede("pc", &t)).ok();
    let e = tool_error(c.call("rdy_revoke", revoke("pc2", &ta)));
    let t = refused_create(&e, &sec, "decision", "");
    c.call("rdy_revoke", revoke("pc2", &t)).ok();
}

#[test]
fn plb1_schema_explains_the_decision_parent() {
    let (dir, r) = project();
    r.close().unwrap();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:s");
    let sch = c.call("rdy_schema", json!({})).ok();
    let tools = c.tools();
    // the four tools' parameter schemas, as digests of their JSON
    let digests: Vec<(String, String)> = ["rdy_write", "rdy_decide", "rdy_supersede", "rdy_revoke"]
        .iter()
        .map(|name| {
            let t = tools.iter().find(|t| t["name"] == *name).unwrap();
            let bytes = serde_json::to_vec(&t["inputSchema"]).unwrap();
            (name.to_string(), rdy_model::Hash::of(&bytes).to_string())
        })
        .collect();
    let edit_loop = sch["edit_loop"].as_str().unwrap();
    assert!(
        edit_loop.contains(
            "brief the block, or its parent to create under it. rdy_decide, rdy_supersede and rdy_revoke create a decision under a parent. Brief that parent; a briefing of the old decision does not cover it. A briefing is current"
        ),
        "{edit_loop}"
    );
    assert_eq!(
        sch["errors"]["E_BRIEF_REQUIRED"],
        json!({"when": "a write touches stable content and cites no briefing, or one that does not cover the block",
            "fix": "retry with the token of the briefing the error carries\non a readiness issue: exclude the change (rdy_view exclude), rdy_brief the block, and write it again"})
    );
    // rf5: a finding or a source with no parent uses the decision home too, and the decision
    // guidance carries the delegated and time fixes
    assert!(
        sch["text"]
            .as_str()
            .unwrap()
            .contains("With no parent, they use the decision home, or the project root when the project names none."),
        "{sch}"
    );
    let rule = sch["decisions"]["rule"].as_str().unwrap();
    for fix in [
        "name who ruled and under whose mandate in attested_by",
        "write an RFC 3339 UTC time, such as 2026-10-21T15:10:58Z, or a date, such as 2026-10-05",
    ] {
        assert!(rule.contains(fix), "{rule}");
    }
    // the digests at the base of this change (b97ad25). rdy_write's is re-taken when the schema's
    // op list gained the reaffirm op and repin's `code` key: only the description of its `ops`
    // parameter changed (351830b6). It is re-taken again when rdy_write gained the strict
    // `reflects: {sources?: [string], code?: true}` parameter and its guidance text (D-4WC6R):
    // only the new `reflects` parameter changed (28ea7eb0). The other three are re-taken when their
    // `parent` descriptions named a folder and the project root (D-P3YD5; the commit "feat(engine):
    // with no parent and no decision home, a decision sits at the project root"). rdy_write's is
    // re-taken once more when its move text named every top-level block, as ruling final-round1
    // item 2 gives it word for word: "a move without parent goes to the top level (docs, folders,
    // decisions, questions, findings and sources)".
    let base: Vec<(String, String)> = [
        (
            "rdy_write",
            "0bf3a56e9c5e9a352b12f0697e912d5be11aa4fb25df70c5a9e8d6cf30be806f",
        ),
        (
            "rdy_decide",
            "0b3befbb6412d1b97e7388dea302c8206bb1f65a8b28514ae2051968a427911b",
        ),
        (
            "rdy_supersede",
            "4892eba948ff86127fc2e909375e328ccffd14b54bfbc340ae7247d85450bd57",
        ),
        (
            "rdy_revoke",
            "cc79f5c823c89a3ae821e3d425790657ba50b9ccfba0962a234f23e33d30d60d",
        ),
    ]
    .iter()
    .map(|(n, d)| (n.to_string(), d.to_string()))
    .collect();
    assert_eq!(digests, base, "the parameter schemas are unchanged");
}
