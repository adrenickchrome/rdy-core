//! sbrf1, mflt1: rdy_search over MCP. brief: true returns a registered briefing over the page's
//! hits, whose token a write to those blocks can cite (FW-SR2); no hits, no briefing. Filters and
//! totals reach the tool (FW-PR1a, FW-SR3, FW-SR4).
use rdy_engine::Rdy;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// `rdy_qa::fixtures::write` with an editorial justification: the fixture writes stable normative content, and an
/// unjustified change to stable content blocks its landing (D-ME2HD).
fn clarified(
    r: &mut rdy_engine::Rdy,
    view: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops)?,
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "seed the test world".into(),
            }),
            message: String::new(),
        },
    )
}

/// Doc `rules` / section S (stable): claims "A needle is steel.", "A needle is thin.", "A pin is
/// short."; landed. Returns their ids.
fn rules(r: &mut Rdy) -> Vec<String> {
    let w = clarified(r, "work", json!([
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
fn sbrf1_a_cut_search_briefing_pages_through_rdy_brief() {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "The needle holds."}
    ]))
    .unwrap();
    let (s, h) = (w.created["$s"].clone(), w.created["$h"].clone());
    let ops: Vec<Value> = (0..60)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("Claim {i} cites {{{{{h}}}}}.")}))
        .collect();
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c, "s3");
    let v = c
        .call(
            "rdy_search",
            json!({"query": "needle", "scope": "canon", "brief": true, "max_tokens": 800}),
        )
        .ok();
    let b = &v["briefing"];
    assert!(b["inflight"].is_object() && b["lineage"].is_array(), "{b}");
    assert_eq!(b["truncated"], true, "{b}");
    let token = b["base"]["token"].clone();
    let mut deps = b["dependents"].as_array().unwrap().len();
    let mut next = b["next"].clone();
    assert!(next.is_string(), "{b}");
    while next.is_string() {
        let p = c
            .call(
                "rdy_brief",
                json!({"page": next, "scope": "canon", "max_tokens": 400}),
            )
            .ok();
        assert_eq!(p["base"]["token"], token);
        deps += p["dependents"].as_array().unwrap().len();
        next = p["next"].clone();
    }
    assert_eq!(deps, 60);
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

#[test]
fn msw17_brief_true_briefs_the_sheet_when_every_hit_is_held() {
    let (dir, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "body": "Keeps the books.", "fields": {"name": "Ledger service"}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let e = w.created["$e"].clone();
    drop(r);
    let mut c = McpClient::start(dir.path());
    agent(&mut c, "s1");
    let v = c
        .call(
            "rdy_search",
            json!({"query": "Ledger service", "brief": true}),
        )
        .ok();
    assert_eq!(v["entities"][0]["id"], e.as_str(), "{v}");
    assert!(
        v["matches"].as_array().is_none_or(|a| a.is_empty()),
        "every hit is held: {v}"
    );
    assert!(
        v["briefing"]["base"]["token"].is_string(),
        "the sheet's blocks are briefed: {v}"
    );
    c.alive();
}
