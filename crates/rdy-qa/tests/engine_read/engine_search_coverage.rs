//! cc14: search hits and fact sheets carry coverage and confidence (FW-SR1, FW-SR4, FW-CV2);
//! `coverage_below` keeps low-scoring hits and `sort: coverage` lists the lowest first.
use rdy_engine::search::SearchFilters;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Doc `ops` (overview) holds term Ledger, claim c1 (names Ledger twice, unlinked, no decision),
/// claim c2 (describes Ledger, decided_by d1), decisions d1, d2 and d3, question q. Doc `lone`
/// holds one prose block. Landed. The ids by placeholder.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d3", "parent": "$s", "fields": {"statement": "Reviews happen on Fridays", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    w.created
}

fn query(text: &str, filters: Value) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        filters: serde_json::from_value::<SearchFilters>(filters).unwrap(),
        ..Default::default()
    }
}

fn search(r: &Rdy, text: &str, filters: Value) -> Value {
    let res = r
        .read()
        .search(&Scope::Canon, &query(text, filters), None, Some(4000))
        .unwrap();
    serde_json::to_value(res).unwrap()
}

fn search_err(r: &Rdy, text: &str, filters: Value) -> RdyError {
    r.read()
        .search(&Scope::Canon, &query(text, filters), None, Some(4000))
        .unwrap_err()
}

#[test]
fn cc14_hits_carry_coverage_issues_and_confidence() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let c = world(&mut r);
    let res = search(&r, "append-only", json!({}));
    let hit = &res["matches"][0];
    assert_eq!(hit["id"], c["$c1"]);
    assert!(hit["coverage"].as_u64().unwrap() < 100);
    assert_eq!(hit["issues"], 3);
    assert_eq!(hit["confidence"], "low", "text with no entity behind it");
}

#[test]
fn cc14_a_fact_sheet_carries_confidence_and_reasons() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let res = search(&r, "Ledger", json!({}));
    assert_eq!(res["entities"][0]["confidence"], "medium");
    assert_eq!(res["entities"][0]["reasons"][0]["code"], "untied_content");
}

#[test]
fn cc14_coverage_below_and_sort_coverage() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let all = search(&r, "the", json!({}));
    let low = search(&r, "the", json!({"coverage_below": 60}));
    assert!(
        low["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["coverage"].as_u64().unwrap() < 60)
    );
    let sorted = search(&r, "the", json!({"sort": "coverage"}));
    let scores: Vec<u64> = sorted["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["coverage"].as_u64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(sorted["total"], all["total"], "a sort keeps every hit");
    assert!(
        search_err(&r, "the", json!({"sort": "score"}))
            .message
            .contains("sort takes coverage")
    );
    assert!(
        search_err(&r, "the", json!({"coverage_below": 101}))
            .message
            .contains("0 to 100")
    );
}

#[test]
fn cc14_a_sort_moves_the_lowest_first_and_a_bound_of_zero_keeps_nothing() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let plain = search(&r, "e", json!({}));
    let sorted = search(&r, "e", json!({"sort": "coverage"}));
    let ids = |v: &Value| -> Vec<String> {
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap().to_string())
            .collect()
    };
    let scores = |v: &Value| -> Vec<u64> {
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["coverage"].as_u64().unwrap())
            .collect()
    };
    let (lo, hi) = (scores(&sorted)[0], *scores(&sorted).last().unwrap());
    assert!(lo < hi, "the world has hits of different scores");
    let mut want = ids(&plain);
    want.sort_by_key(|id| {
        plain["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == *id)
            .map(|h| h["coverage"].as_u64().unwrap())
    });
    assert_eq!(
        ids(&sorted),
        want,
        "a stable sort keeps the default order between ties"
    );
    assert_eq!(search(&r, "e", json!({"coverage_below": 0}))["total"], 0);
}

#[test]
fn cc14_a_past_landing_carries_no_coverage_and_refuses_the_filter() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let c = world(&mut r);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": c["$empty"], "body": "Later text."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let tag = r.read().resolve_read_scope("tag:v1.0.0", None).unwrap();
    let res = r
        .read()
        .search(&tag, &query("append-only", json!({})), None, Some(4000))
        .unwrap();
    let v = serde_json::to_value(res).unwrap();
    assert_eq!(v["total"], 1);
    assert!(v["matches"][0].get("coverage").is_none());
    let e = r
        .read()
        .search(
            &tag,
            &query("append-only", json!({"coverage_below": 50})),
            None,
            Some(4000),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
}

#[test]
fn cc14_the_cli_and_mcp_take_coverage_below_and_sort() {
    let s = rdy_qa::daemon::Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        world(&mut r);
        r.close().unwrap();
    }
    let (ok, v) = s.json(&["--json", "search", "e", "--sort", "coverage"]);
    assert!(ok, "{v}");
    let scores: Vec<u64> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["coverage"].as_u64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] <= w[1]), "{scores:?}");
    let (ok, v) = s.json(&["--json", "search", "e", "--coverage-below", "0"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 0);
    let (ok, v) = s.json(&["--json", "search", "e", "--sort", "score"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    let out = s.rdy(&["search", "Ledger"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("  confidence medium"), "{text}");
    assert!(text.contains("  coverage "), "{text}");
    s.stop();

    let (dir, mut r) = rdy_qa::fixtures::project();
    world(&mut r);
    drop(r);
    let mut c = rdy_qa::mcp::McpClient::start(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:m", "run": "m1", "role": "writer"})
        )
        .code()
        .is_none()
    );
    let v = c
        .call(
            "rdy_search",
            json!({"query": "e", "scope": "canon", "sort": "coverage", "coverage_below": 101}),
        )
        .code()
        .map(str::to_string);
    assert_eq!(v.as_deref(), Some("E_BAD_INPUT"));
    let v = c
        .call(
            "rdy_search",
            json!({"query": "append-only", "scope": "canon", "coverage_below": 100}),
        )
        .ok();
    assert_eq!(v["total"], 1, "{v}");
    c.alive();
}
