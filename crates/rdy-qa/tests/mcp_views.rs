//! mview1: rdy_view over the real server, through the daemon; rdy_propose versions; conflicts in rdy_read.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::json;

/// `rdy_context`'s key for a block's version hash (`Context.version` at 573c26b).
const VERSION_KEY: &str = "version";

fn seeded() -> (Served, String, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    r.close().unwrap();
    (s, section, claim)
}
fn session(c: &mut McpClient, actor: &str, run: &str) -> String {
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": "writer"}),
    )
    .ok()["view"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn mview1_an_include_into_another_sessions_view_is_e_view_owner_and_a_run_less_agent_works_in_its_own()
 {
    let (s, section, _claim) = seeded();
    let mut x = McpClient::start_proxy(&s);
    let v = x
        .call("rdy_session", json!({"actor": "agent:x", "role": "writer"}))
        .ok();
    assert_eq!(
        v["view"], "work-agent-x",
        "M2b's default, checked in the views area as spec §15.3 asks"
    );
    let mut a = McpClient::start_proxy(&s);
    assert_eq!(session(&mut a, "agent:a", "r1"), "r1");
    let wa = a
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "a"}]}),
        )
        .ok();
    let wx = x
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "x"}]}),
        )
        .ok();
    let e = x.call(
        "rdy_view",
        json!({"op": "include", "view": "r1", "changes": [wx["change"]]}),
    );
    assert_eq!(e.code(), Some("E_VIEW_OWNER"), "{e:?}");
    let ok = x
        .call(
            "rdy_view",
            json!({"op": "include", "changes": [wa["change"]]}),
        )
        .ok();
    assert_eq!(
        (ok["view"].as_str(), ok["changes"].as_u64()),
        (Some("work-agent-x"), Some(2)),
        "x takes a's change into its own view"
    );
}

#[test]
fn mview1_create_fork_include_exclude_show_list_compare_close_and_undo() {
    let (s, section, claim) = seeded();
    let mut a = McpClient::start_proxy(&s);
    session(&mut a, "agent:a", "ra");
    let v = a
        .call(
            "rdy_view",
            json!({"op": "create", "name": "feature", "base": "canon", "pin": false}),
        )
        .ok();
    assert_eq!(
        (
            v["view"].as_str(),
            v["pinned"].as_bool(),
            v["changes"].as_u64()
        ),
        (Some("feature"), Some(false), Some(0))
    );
    let w = a.call("rdy_write", json!({"view": "feature", "ops": [{"op": "create", "type": "claim", "parent": section, "body": "f1"}]})).ok();
    let cf = w["change"].as_str().unwrap().to_string();
    let f = a
        .call(
            "rdy_view",
            json!({"op": "fork", "from": "feature", "name": "feature-2"}),
        )
        .ok();
    assert_eq!(f["changes"], 1);
    let mut b = McpClient::start_proxy(&s);
    session(&mut b, "agent:b", "rb");
    let read = b
        .call("rdy_context", json!({"id": claim, "scope": "canon"}))
        .ok();
    let base = read[VERSION_KEY].as_str().unwrap().to_string();
    let cb = b
        .call(
            "rdy_write",
            json!({"ops": [{"op": "update", "id": claim, "base": base, "body": "b's"}]}),
        )
        .ok()["change"]
        .as_str()
        .unwrap()
        .to_string();
    let inc = b
        .call(
            "rdy_view",
            json!({"op": "include", "changes": [cf.clone()]}),
        )
        .ok();
    assert_eq!(
        (inc["added"].clone(), inc["changes"].as_u64()),
        (json!([cf]), Some(2))
    );
    let ca = a.call("rdy_write", json!({"view": "feature", "ops": [{"op": "update", "id": claim, "base": base, "body": "a's"}]})).ok()["change"].as_str().unwrap().to_string();
    let inc = b
        .call("rdy_view", json!({"op": "include", "changes": [ca]}))
        .ok();
    assert_eq!(inc["conflicts"].as_array().unwrap().len(), 1, "{inc}");
    let rd = b.call("rdy_context", json!({"id": claim})).ok();
    assert_eq!(rd["body"], "one", "reads as the base");
    assert_eq!(rd["conflict"]["sides"].as_array().unwrap().len(), 2, "{rd}");
    let rr = b.call("rdy_read", json!({"id": claim})).ok();
    assert!(
        rr["conflict"]["id"].is_string() && rr["text"].as_str().unwrap().contains("[conflict "),
        "{rr}"
    );
    let show = b.call("rdy_view", json!({"op": "show"})).ok();
    assert_eq!(
        (
            show["changes"].as_array().unwrap().len(),
            show["conflicts"].as_array().unwrap().len()
        ),
        (3, 1)
    );
    let list = a.call("rdy_view", json!({"op": "list"})).ok();
    let names: Vec<&str> = list["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["feature", "feature-2", "rb", "seed"]);
    let cmp = b
        .call("rdy_view", json!({"op": "compare", "b": "canon"}))
        .ok();
    assert_eq!(
        (
            cmp["added"].as_array().unwrap().len(),
            cmp["conflicted"].as_array().unwrap().len()
        ),
        (1, 1)
    );
    let small = b
        .call(
            "rdy_view",
            json!({"op": "compare", "b": "canon", "max_tokens": 40}),
        )
        .ok();
    assert!(
        small.to_string().len() < cmp.to_string().len(),
        "max_tokens cuts"
    );
    let ex = b
        .call(
            "rdy_view",
            json!({"op": "exclude", "changes": [cb], "cascade": false}),
        )
        .ok();
    assert_eq!(ex["removed"].as_array().unwrap().len(), 1);
    assert!(ex["conflicts"].as_array().unwrap().is_empty());
    let u = b.call("rdy_view", json!({"op": "undo"})).ok();
    assert_eq!(
        (u["kind"].as_str(), u["conflicts"].as_array().unwrap().len()),
        (Some("exclude"), 1)
    );
    let e = a.call("rdy_view", json!({"op": "undo", "view": "rb"}));
    assert_eq!(e.code(), Some("E_VIEW_OWNER"));
    let cl = a
        .call(
            "rdy_view",
            json!({"op": "close", "name": "feature-2", "reason": "dup"}),
        )
        .ok();
    assert_eq!(cl["closed"][0]["reason"], "dup");
    assert_eq!(
        a.call("rdy_view", json!({"op": "list"})).ok()["views"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        a.call("rdy_view", json!({"op": "list", "all": true})).ok()["views"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let e = a.call(
        "rdy_view",
        json!({"op": "own", "view": "feature", "none": true}),
    );
    assert_eq!(
        e.code(),
        Some("E_PERMISSION"),
        "MCP sessions are agents: {e:?}"
    );
    let e = a.call("rdy_view", json!({"op": "nope"}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"));
    let e = a.call(
        "rdy_view",
        json!({"op": "exclude", "view": "feature", "changes": [cf]}),
    );
    assert!(e.code().is_none(), "the owner excludes: {e:?}");
}

#[test]
fn mview1_propose_versions_subsets_and_conflicts_over_mcp() {
    let (s, section, claim) = seeded();
    let mut a = McpClient::start_proxy(&s);
    session(&mut a, "agent:a", "ra");
    a.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "one"}]}),
    )
    .ok();
    let p1 = a.call("rdy_propose", json!({"title": "t"})).ok();
    assert_eq!(
        (p1["version"].as_u64(), p1["changes"].as_u64()),
        (Some(1), Some(1))
    );
    assert!(p1["ref"].as_str().unwrap().ends_with("@1"), "{p1}");
    let w2 = a
        .call(
            "rdy_write",
            json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "two"}]}),
        )
        .ok();
    let p2 = a
        .call(
            "rdy_propose",
            json!({"title": "t", "changes": [w2["change"]]}),
        )
        .ok();
    assert_eq!(
        (
            p2["version"].as_u64(),
            p2["changes"].as_u64(),
            p2["stable"].as_str()
        ),
        (Some(2), Some(1), p1["stable"].as_str()),
        "a create has no deps, so a one-change subset is closed"
    );
    let mut b = McpClient::start_proxy(&s);
    session(&mut b, "agent:b", "rb");
    let read = b
        .call("rdy_context", json!({"id": claim, "scope": "canon"}))
        .ok();
    b.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": claim, "base": read[VERSION_KEY], "body": "b's"}]}),
    )
    .ok();
    a.call(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": claim, "base": read[VERSION_KEY], "body": "a's"}]}),
    )
    .ok();
    let cb = b.call("rdy_view", json!({"op": "show"})).ok()["changes"][0]["id"].clone();
    a.call("rdy_view", json!({"op": "include", "changes": [cb]}))
        .ok();
    let p3 = a.call("rdy_propose", json!({"title": "t"})).ok();
    assert_eq!(
        (
            p3["conflicts"].as_array().unwrap().len(),
            p3["blocking"].as_array().unwrap().len()
        ),
        (1, 1)
    );
    let schema = a.call("rdy_schema", json!({})).ok();
    let doc = schema["views"].as_str().unwrap();
    for code in [
        "E_VIEW_OWNER",
        "E_CONFLICT_UNRESOLVED",
        "E_PROPOSAL_MOVED",
        "E_UNDO_BLOCKED",
        "E_UNDO_LANDING",
    ] {
        assert!(doc.contains(code), "{code} missing from the schema doc");
    }
    for code in [
        "E_VIEW_EXISTS",
        "E_VIEW_CLOSED",
        "E_HAS_DEPENDENTS",
        "E_DEPS_MISSING",
        "E_CONFLICT_UNRESOLVED",
        "E_PROPOSAL_MOVED",
        "E_UNDO_BLOCKED",
        "E_UNDO_LANDING",
        "E_NOTHING_TO_UNDO",
        "E_VIEW_OWNER",
    ] {
        assert!(
            schema["errors"][code]["fix"].is_string(),
            "{code} missing from the errors table"
        );
    }
    let names: Vec<String> = a
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"rdy_view".to_string())
            && names.contains(&"rdy_log".to_string())
            && !names.contains(&"rdy_undo".to_string()),
        "undo is an op of rdy_view: {names:?}"
    );
}

#[test]
fn eff2_rdy_decide_takes_effective_from_and_rdy_context_shows_it() {
    let (s, _section, _claim) = seeded();
    let mut a = McpClient::start_proxy(&s);
    session(&mut a, "agent:a", "r1");
    let w = a.call("rdy_write", json!({"ops": [
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]})).ok();
    let d = a.call("rdy_decide", json!({"parent": w["created"]["$s"], "statement": "S", "effective_from": "2026-09-01T00:00:00Z"})).ok();
    let ctx = a
        .call("rdy_context", json!({"id": d["created"]["$decision"]}))
        .ok();
    assert_eq!(ctx["effective_from"], "2026-09-01T00:00:00Z", "{ctx}");
}

#[test]
fn logj1_rdy_log_lists_landings_with_their_approval_fields() {
    let (s, _section, _claim) = seeded();
    let mut a = McpClient::start_proxy(&s);
    session(&mut a, "agent:a", "r1");
    let rows = a.call("rdy_log", json!({})).ok();
    let top = &rows[0];
    assert_eq!(top["assurance"], "local", "{rows}");
    assert!(top["approved"].as_str().unwrap().starts_with("P-"), "{top}");
    assert_eq!(top["rebased_from"], json!([]));
    assert!(
        top["relayed_by"].is_null() && top["evidence"].is_null() && top["relay_evidence"].is_null()
    );
}
