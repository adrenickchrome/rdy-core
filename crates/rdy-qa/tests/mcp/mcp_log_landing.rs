//! dq7: `rdy_log {landing}` over MCP: one landing's row and its change list, cut to a budget.
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::rdy_log;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// Two landings: the first holds a five-block doc; the second adds `n` claims. Tag `v1.0.0` is
/// the second. Returns the state and both landings.
fn landed(n: usize) -> (tempfile::TempDir, McpState, String, String) {
    let (d, mut r) = project();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "g", "title": "G", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "One."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Two."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Three."}
    ]))
    .unwrap();
    let l1 = land(&mut r, "seed").unwrap();
    let ops: Vec<Value> = (0..n)
        .map(|i| json!({"op": "create", "type": "claim", "parent": seeded.created["$s"], "body": format!("Claim number {i} holds.")}))
        .collect();
    write(&mut r, "more", Value::Array(ops)).unwrap();
    let l2 = land(&mut r, "more").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    (d, st, l1, l2)
}

fn log(st: &mut McpState, args: Value) -> Result<Value, rdy_engine::RdyError> {
    rdy_log(st, serde_json::from_value(args).unwrap())
}

#[test]
fn dq7_rdy_log_landing_gives_the_row_and_the_diff() {
    let (_d, mut st, l1, l2) = landed(3);
    let rows = log(&mut st, json!({})).unwrap();
    let v = log(&mut st, json!({"landing": l2})).unwrap();
    assert_eq!(v["landing"], rows[0], "the row is the log's row");
    assert_eq!(v["diff"]["counts"]["added"], 3, "{v}");
    assert_eq!(v["diff"]["from"]["kind"], "landing");
    for form in [
        l2[..8].to_string(),
        format!("landing:{l2}"),
        "tag:v1.0.0".to_string(),
    ] {
        assert_eq!(log(&mut st, json!({"landing": form})).unwrap(), v, "{form}");
    }
    let first = log(&mut st, json!({"landing": l1})).unwrap();
    assert_eq!(first["diff"]["from"], json!({"kind": "empty"}));
    assert_eq!(first["landing"], rows[1]);
}

#[test]
fn dq7_rdy_log_landing_refuses_since_limit_and_verify() {
    let (_d, mut st, _l1, l2) = landed(1);
    for extra in [
        json!({"since": "tag:v1.0.0"}),
        json!({"limit": 1}),
        json!({"verify": true}),
    ] {
        let mut args = json!({"landing": l2});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let e = log(&mut st, args).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT");
        assert_eq!(e.message, "landing takes no since, limit or verify");
        assert_eq!(
            e.fix,
            "drop since, limit and verify, or drop landing to page the log"
        );
    }
}

#[test]
fn dq7_rdy_log_page_needs_landing() {
    let (_d, mut st, _, _) = landed(1);
    let e = log(&mut st, json!({"page": "x"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "page continues one landing's change list");
    assert_eq!(e.fix, "pass landing with page");
}

#[test]
fn dq7_rdy_log_landing_cuts_the_diff_and_never_the_row() {
    let (_d, mut st, _l1, l2) = landed(60);
    let whole = log(&mut st, json!({"landing": l2, "max_tokens": 100_000})).unwrap();
    assert!(whole["diff"].get("next").is_none(), "{whole}");
    let first = log(&mut st, json!({"landing": l2, "max_tokens": 700})).unwrap();
    assert_eq!(first["landing"], whole["landing"], "the row is whole");
    assert_eq!(first["diff"]["truncated"], true, "{first}");
    assert!(serde_json::to_string(&first).unwrap().len() / 4 <= 700);
    let mut seen: Vec<Value> = first["diff"]["entries"].as_array().unwrap().clone();
    let mut next = first["diff"]["next"].as_str().map(String::from);
    let mut pages = 1;
    while let Some(cursor) = next {
        let v = log(
            &mut st,
            json!({"landing": l2, "max_tokens": 700, "page": cursor}),
        )
        .unwrap();
        assert_eq!(v["landing"], whole["landing"]);
        seen.extend(v["diff"]["entries"].as_array().unwrap().clone());
        next = v["diff"]["next"].as_str().map(String::from);
        pages += 1;
        assert!(pages < 100);
    }
    assert!(pages > 2);
    let ids = |es: &[Value]| es.iter().map(|e| e["id"].clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(&seen),
        ids(whole["diff"]["entries"].as_array().unwrap())
    );
    // a cursor that does not parse names rdy_log
    let e = log(&mut st, json!({"landing": l2, "page": "nonsense"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "page is not a cursor from rdy_log's next");
    assert_eq!(e.fix, "pass next from the previous page, unchanged");
}

#[test]
fn dq7_cli_and_chat_agree() {
    // `rdy --json show <landing>` equals `rdy_log {landing, max_tokens: 100000}`: a five-block landing
    let s = rdy_qa::daemon::Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "g", "title": "G", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "One."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Two."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Three."}
    ]))
    .unwrap();
    let l1 = land(&mut r, "seed").unwrap();
    r.close().unwrap();
    let (ok, cli) = s.json(&["--json", "show", &l1]);
    assert!(ok, "{cli}");
    s.stop();
    let mut st = McpState::new(Arc::new(Local::new(Rdy::open(s.dir.path()).unwrap())));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let chat = log(&mut st, json!({"landing": l1, "max_tokens": 100_000})).unwrap();
    assert_eq!(chat["diff"]["counts"]["added"], 5);
    assert_eq!(cli, chat);
}
