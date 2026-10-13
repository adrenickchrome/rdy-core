//! MCP schema docs (doc1) and reader sessions on canon (ses1).
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

#[test]
fn doc1_schema_shows_the_link_shape_and_every_op() {
    let (_d, r) = project();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    let s = rdy_schema(&mut st, p(json!({}))).unwrap();
    assert!(
        s["link_shape"]["shape"]
            .as_str()
            .unwrap()
            .contains("\"target\""),
        "{s}"
    );
    assert_eq!(
        s["link_shape"]["example"]["decided_by"][0]["target"],
        "D-W2HXR"
    );
    for (name, _) in WRITE_OPS {
        assert!(s["ops"][*name].is_string(), "{name}: {}", s["ops"]);
    }
    let t = rdy_schema(&mut st, p(json!({"block_type": "claim"}))).unwrap();
    assert!(
        t["links"]["shape"].as_str().unwrap().contains("\"target\""),
        "{t}"
    );
    assert!(
        t["links"]["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "decided_by"),
        "{t}"
    );
}

#[test]
fn doc1_a_malformed_links_value_gets_a_hint_with_the_shape() {
    let (_d, mut r) = project();
    let s = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ]))
    .unwrap()
    .created["$s"]
    .clone();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "role": "writer", "view": "work"})),
    )
    .unwrap();
    for links in [
        json!(["D-AAAAA"]),
        json!({"decided_by": 7}),
        json!({"decided_by": [{"id": "D-AAAAA"}]}),
    ] {
        let e = rdy_write(&mut st, p(json!({"ops": [{"op": "create", "type": "claim", "parent": s, "body": "x", "links": links}]})))
            .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{links}: {e}");
        assert!(e.fix.contains("\"target\""), "{links}: {e}");
    }
}

#[test]
fn doc1_the_rdy_write_tool_lists_every_op_and_the_link_shape() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let w = tools.iter().find(|t| t["name"] == "rdy_write").unwrap();
    let ops = w["inputSchema"]["properties"]["ops"]["description"]
        .as_str()
        .unwrap();
    for (name, _) in WRITE_OPS {
        assert!(
            ops.contains(&format!("\"op\":\"{name}\"")),
            "{name} missing from: {ops}"
        );
    }
    assert!(ops.contains("\"target\""), "{ops}");
    assert!(w["description"].as_str().unwrap().contains("links"), "{w}");
}

#[test]
fn ses1_a_reader_session_may_read_canon_by_default() {
    let (_d, mut r) = project();
    let doc = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Ledger rules", "doc_type": "glossary"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    land(&mut r, "work").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    let ok = rdy_session(
        &mut st,
        p(json!({"actor": "agent:reader", "role": "reader", "view": "canon"})),
    )
    .unwrap();
    assert_eq!(ok["view"], "canon");
    let ov = rdy_overview(&mut st, p(json!({}))).unwrap();
    assert_eq!(ov["tree"][0]["id"], doc, "{ov}");
    let read = rdy_read(&mut st, p(json!({"id": doc}))).unwrap();
    assert!(
        read["text"].as_str().unwrap().contains("Ledger rules"),
        "{read}"
    );
    for role in ["writer", "reviewer"] {
        let e = rdy_session(
            &mut st,
            p(json!({"actor": "agent:w", "role": role, "view": "canon"})),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_RESERVED_VIEW", "{role}: {e}");
    }
}

#[test]
fn sg10_schema_has_brief_and_consequences_topics() {
    let (_d, r) = project();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    let s = rdy_schema(&mut st, p(json!({}))).unwrap();
    assert_eq!(s["brief"], BRIEF_DOC, "{s}");
    assert_eq!(s["consequences"], CONSEQUENCES_DOC, "{s}");
    assert!(
        EDIT_LOOP_DOC.contains("brief and consequences"),
        "{EDIT_LOOP_DOC}"
    );
}

#[test]
fn sg10_the_readme_names_the_m2_concepts() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    let readme = std::fs::read_to_string(&path).unwrap();
    for word in [
        "rdy_brief",
        "consequences",
        "intent",
        "alternative set",
        "tension",
        "inbox",
        "swarm",
    ] {
        assert!(readme.contains(word), "README.md lacks {word}");
    }
    assert!(!readme.contains("in-flight work"));
}

/// Fix round 1: the README states the rules the code holds.
#[test]
fn sg10_the_readme_states_the_rules_the_code_holds() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    let readme = std::fs::read_to_string(&path).unwrap();
    // Dismissal: anyone who writes may dismiss an advisory tension.
    assert!(readme.contains("Anyone who writes may dismiss an advisory tension."));
    assert!(!readme.contains("A reviewer or a person of another actor than the authors dismisses"));
    // The gate refuses only a landing that brings all sides into canon.
    assert!(readme.contains("stops a landing that would bring all its sides into canon"));
    // No tool approves: the relay goes through the CLI.
    assert!(readme.contains("No tool approves yet"));
    assert!(readme.contains("--relayed-by"));
    assert!(!readme.contains("with the same acts"));
    // Confidence is carried by fact sheets, search hits and briefings only.
    assert!(readme.contains("Fact sheets, search hits and briefings carry a confidence level"));
    assert!(!readme.contains("Each answer rdy gives"));
    // `work` is the human's view; the agent's scope is not "work".
    assert!(!readme.contains("around its work"));
    // The swarm line says what the run checks.
    assert!(readme.contains("eight scripted agents and one person against one daemon"));
    assert!(readme.contains("every stable write cites a briefing"));
    assert!(!readme.contains("every write was briefed"));
    // The working section stays within 45 lines.
    let start = readme.find("## Working with agents").unwrap();
    let end = readme.find("## Daemon").unwrap();
    assert!(readme[start..end].lines().count() <= 46);
}
