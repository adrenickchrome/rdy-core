//! sbrf1, mflt1: rdy_search over MCP. brief: true returns a registered briefing over the page's
//! hits, whose token a write to those blocks can cite (FW-SR2); no hits, no briefing. Filters and
//! totals reach the tool (FW-PR1a, FW-SR3, FW-SR4).
use rdy_engine::Rdy;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// Doc `rules` / section S (stable): claims "A needle is steel.", "A needle is thin.", "A pin is
/// short."; landed. Returns their ids.
fn rules(r: &mut Rdy) -> Vec<String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S", "lifecycle": "stable"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A needle is steel."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "A needle is thin."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "A pin is short."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    ["$a", "$b", "$c"]
        .iter()
        .map(|k| w.created[*k].clone())
        .collect()
}

fn agent(c: &mut McpClient, run: &str) {
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:s", "run": run, "role": "writer"}),
    );
    assert!(s.code().is_none(), "{s:?}");
}

#[test]
fn sbrf1_brief_true_returns_a_briefing_whose_token_a_write_can_cite() {
    let (dir, mut r) = project();
    let ids = rules(&mut r);
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c, "s1");
    let edit = json!([{"op": "update", "id": ids[0], "body": "A needle is iron."}]);
    let e = c.call("rdy_write", json!({"ops": edit.clone()}));
    assert_eq!(
        e.code(),
        Some("E_BRIEF_REQUIRED"),
        "stable content needs a briefing: {e:?}"
    );
    let plain = c.call("rdy_search", json!({"query": "needle"})).ok();
    assert!(plain.get("briefing").is_none(), "{plain}");
    let v = c
        .call("rdy_search", json!({"query": "needle", "brief": true}))
        .ok();
    assert_eq!(v["total"], 2);
    assert_eq!(v["briefing"]["covers"], 2, "{v}");
    let token = v["briefing"]["base"]["token"].as_str().unwrap().to_string();
    let w = c.call("rdy_write", json!({"ops": edit, "brief": token}));
    assert!(w.code().is_none(), "{w:?}");
    let none = c
        .call("rdy_search", json!({"query": "haystack", "brief": true}))
        .ok();
    assert_eq!(none["total"], 0);
    assert!(
        none.get("briefing").is_none(),
        "no hits, no briefing: {none}"
    );
    c.alive();
}

#[test]
fn sbrf1_each_page_briefs_only_its_own_hits() {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let ops: Vec<Value> = (0..40)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("needle {i}")}))
        .collect();
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c, "s2");
    let args = json!({"query": "needle", "scope": "canon", "brief": true, "max_tokens": 600});
    let first = c.call("rdy_search", args.clone()).ok();
    let n = first["matches"].as_array().unwrap().len();
    assert!(first["next"].is_string(), "{first}");
    assert_eq!(first["briefing"]["covers"], n);
    let mut more = args.clone();
    more["page"] = first["next"].clone();
    let second = c.call("rdy_search", more).ok();
    assert_eq!(
        second["briefing"]["covers"],
        second["matches"].as_array().unwrap().len()
    );
    assert_ne!(
        second["briefing"]["base"]["token"],
        first["briefing"]["base"]["token"]
    );
    c.alive();
}

#[test]
fn mflt1_rdy_search_takes_filters_and_starts_with_totals() {
    let (dir, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "code", "parent": "$a", "body": "port = 8080", "fields": {"lang": "sh"}},
        {"op": "create", "type": "prose", "parent": "$a", "body": "port = 8080"},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
        {"op": "create", "type": "code", "parent": "$b", "body": "port = 8080", "fields": {"lang": "sh"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c, "f1");
    let v = c
        .call(
            "rdy_search",
            json!({"query": "port = 8080", "scope": "canon"}),
        )
        .ok();
    assert_eq!(
        v["totals"],
        json!({"docs": [{"path": "a", "hits": 2}, {"path": "b", "hits": 1}], "types": {"code": 2, "prose": 1}})
    );
    assert_eq!(
        v["matches"].as_array().unwrap().len(),
        2,
        "the two code samples collapse: {v}"
    );
    assert_eq!(v["matches"][0]["also"][0]["path"], "b");
    let code = c
        .call(
            "rdy_search",
            json!({"query": "8080", "scope": "canon", "types": ["code"], "path": "b"}),
        )
        .ok();
    assert_eq!(code["total"], 1);
    assert_eq!(
        c.call("rdy_search", json!({"query": "8080", "traits": ["shiny"]}))
            .code(),
        Some("E_BAD_INPUT")
    );
    c.alive();
}
