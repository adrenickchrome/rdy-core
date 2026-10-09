//! msrch1: rdy_search over MCP: matches by scope (session view by default), paging through `page`
//! and `next`, and rdy errors for bad input.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{put, upd};
use serde_json::{Value, json};

fn seed(r: &mut Rdy) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let ops: Vec<Value> = (0..40)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("needle {i}")}))
        .collect();
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
}

#[test]
fn msrch1_rdy_search_pages_and_refuses_bad_input() {
    let (dir, mut r) = project();
    seed(&mut r);
    drop(r);
    let mut c = McpClient::start(dir.path());
    let s = c.call(
        "rdy_session",
        json!({"actor": "agent:q", "run": "q1", "role": "writer"}),
    );
    assert!(s.code().is_none(), "{s:?}");
    let first = c
        .call(
            "rdy_search",
            json!({"query": "needle", "scope": "canon", "max_tokens": 300}),
        )
        .ok();
    assert_eq!(first["total"], 40);
    assert_eq!(first["matches"][0]["path"], "many");
    assert_eq!(
        first["matches"][0]["lines"][0],
        json!({"field": "body", "line": 1, "text": "needle 0"})
    );
    let mut seen = first["matches"].as_array().unwrap().len();
    let mut next = first["next"].as_str().map(String::from);
    while let Some(cursor) = next {
        let p = c
            .call(
                "rdy_search",
                json!({"query": "needle", "scope": "canon", "max_tokens": 300, "page": cursor}),
            )
            .ok();
        seen += p["matches"].as_array().unwrap().len();
        next = p["next"].as_str().map(String::from);
        assert!(seen <= 40);
    }
    assert_eq!(seen, 40);
    let own = c.call("rdy_search", json!({"query": "needle"})).ok();
    assert_eq!(
        own["total"], 40,
        "the session's view (no head yet) reads canon"
    );
    assert_eq!(
        c.call("rdy_search", json!({"query": "(", "regex": true}))
            .code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        c.call("rdy_search", json!({"query": "needle", "page": "nonsense"}))
            .code(),
        Some("E_BAD_INPUT")
    );
    assert_eq!(
        c.call("rdy_search", json!({"query": "x".repeat(2000)}))
            .code(),
        Some("E_LIMIT")
    );
    c.alive();
}

#[test]
fn tsrch1_rdy_search_takes_the_tension_filter() {
    let s = Served::new();
    let (a, c) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let sp = spec_doc(&mut r);
        put(&mut r, "a", upd(&sp.a, "Shared word: alpha."));
        put(&mut r, "b", upd(&sp.a, "Shared word: beta."));
        put(&mut r, "c", upd(&sp.c, "Shared word: gamma."));
        r.close().unwrap();
        (sp.a.to_string(), sp.c.to_string())
    };
    let mut m = McpClient::start_proxy(&s);
    m.call(
        "rdy_session",
        json!({"actor": "agent:q", "run": "q1", "role": "writer"}),
    )
    .ok();
    let ids = |m: &mut McpClient, scope: &str, tension: bool| -> Vec<String> {
        let v = m
            .call(
                "rdy_search",
                json!({"query": "Shared word", "scope": scope, "tension": tension}),
            )
            .ok();
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&mut m, "a", false), vec![a.clone()]);
    assert_eq!(
        ids(&mut m, "a", true),
        [a],
        "a's block is in an open tension"
    );
    assert_eq!(ids(&mut m, "c", false), [c]);
    assert!(
        ids(&mut m, "c", true).is_empty(),
        "c's block is in no open tension"
    );
    m.alive();
}
