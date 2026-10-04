use rdy_engine::Rdy;
use rdy_mcp::tools::*;
use serde_json::{json, Value};

fn state() -> (tempfile::TempDir, McpState) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, McpState::new(rdy))
}

fn start(st: &mut McpState) {
    rdy_session(st, serde_json::from_value(json!({"actor": "agent:designer", "run": "run-7", "role": "writer"})).unwrap()).unwrap();
}

fn seed(st: &mut McpState) -> Value {
    rdy_write(st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]})).unwrap()).unwrap()
}

#[test]
fn tools_require_a_session_and_refuse_approver() {
    let (_d, mut st) = state();
    let e = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_NO_SESSION");
    let e = rdy_session(&mut st, serde_json::from_value(json!({"actor": "agent:x", "role": "approver"})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    assert!(rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).is_ok(), "schema needs no session");
}

#[test]
fn session_view_defaults_to_run() {
    let (_d, mut st) = state();
    start(&mut st);
    assert_eq!(st.view, "run-7");
    seed(&mut st);
    let ov = rdy_overview(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(ov["views"][0]["name"], "run-7");
    assert_eq!(ov["pack"], "rdy-standard");
}

#[test]
fn write_read_context_round_trip() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let c = res["created"]["$c"].as_str().unwrap().to_string();
    let t = res["created"]["$t"].as_str().unwrap().to_string();

    let read = rdy_read(&mut st, serde_json::from_value(json!({"id": doc, "scope": "run-7"})).unwrap()).unwrap();
    let text = read["text"].as_str().unwrap();
    assert!(text.contains(&format!("{t} term: Ledger")));
    assert!(text.contains(&format!("  {c} claim:")), "indented by depth:\n{text}");

    let md = rdy_read(&mut st, serde_json::from_value(json!({"id": doc, "scope": "run-7", "format": "markdown"})).unwrap()).unwrap();
    assert!(md["text"].as_str().unwrap().contains("# Identity"));

    let ctx = rdy_context(&mut st, serde_json::from_value(json!({"id": c, "scope": "run-7"})).unwrap()).unwrap();
    assert_eq!(ctx["block_type"], "claim");
    assert_eq!(ctx["doc"], doc);

    let imp = rdy_impact(&mut st, serde_json::from_value(json!({"id": t, "scope": "run-7"})).unwrap()).unwrap();
    assert_eq!(imp["items"][0]["id"], c);
}

#[test]
fn bad_ops_explain_themselves() {
    let (_d, mut st) = state();
    start(&mut st);
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "create"}]})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("type"), "{}", e.message);
    let j = error_json(&e);
    assert!(!j["fix"].as_str().unwrap().is_empty());
}

#[test]
fn decide_and_propose() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let s = res["created"]["$s"].as_str().unwrap();
    let d = rdy_decide(&mut st, serde_json::from_value(json!({"parent": s, "statement": "Ledger is append-only"})).unwrap()).unwrap();
    assert!(d["created"]["$decision"].as_str().unwrap().starts_with("D-"));
    let p = rdy_propose(&mut st, serde_json::from_value(json!({"title": "terms"})).unwrap()).unwrap();
    assert_eq!(p["changes"], 2);
}

#[test]
fn assets_and_schema() {
    let (_d, mut st) = state();
    start(&mut st);
    let a = rdy_asset(&mut st, serde_json::from_value(json!({"data_base64": "aGVsbG8="})).unwrap()).unwrap();
    assert_eq!(a["hash"].as_str().unwrap().len(), 64);
    let s = rdy_schema(&mut st, serde_json::from_value(json!({"block_type": "term"})).unwrap()).unwrap();
    assert_eq!(s["prefix"], "t");
    assert!(s["fields"]["name"]["required"].as_bool().unwrap());
    assert!(s["example"]["op"] == "create");
}

#[test]
fn budget_truncates_large_lists() {
    let big = json!({"items": (0..2000).map(|i| json!({"id": i, "summary": "x".repeat(40)})).collect::<Vec<_>>()});
    let out = budget(big, Some(500));
    assert_eq!(out["truncated"], true);
    assert!(out["omitted"].as_u64().unwrap() > 0);
    assert!(serde_json::to_string(&out).unwrap().len() / 4 <= 500);
    let text = budget(json!({"text": "é".repeat(10_000)}), Some(500));
    assert_eq!(text["truncated"], true);
    assert!(text["text"].as_str().unwrap().len() <= 2000);
    let small = budget(json!({"a": 1}), Some(500));
    assert!(small.get("truncated").is_none());
}

#[test]
fn budget_bounds_escaped_text() {
    let text = (0..5000).map(|_| "line \"x\"").collect::<Vec<_>>().join("\n");
    let out = budget(json!({"text": text}), Some(500));
    assert_eq!(out["truncated"], true);
    assert!(serde_json::to_string(&out).unwrap().len() / 4 <= 500);
}

#[test]
fn failed_session_leaves_state_unchanged() {
    let (_d, mut st) = state();
    start(&mut st);
    for bad in [json!({"actor": "a", "role": "writer", "view": "canon"}), json!({"actor": "a", "role": "writer", "run": "canon"}), json!({"actor": "a", "role": "boss"})] {
        assert!(rdy_session(&mut st, serde_json::from_value(bad).unwrap()).is_err());
        assert_eq!(st.view, "run-7");
        assert_eq!(st.session.as_ref().unwrap().actor, "agent:designer");
    }
}

#[test]
fn read_rejects_unknown_format_and_yaml_separates() {
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let doc = res["created"]["$doc"].as_str().unwrap().to_string();
    let e = rdy_read(&mut st, serde_json::from_value(json!({"id": doc, "format": "xml"})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    let y = rdy_read(&mut st, serde_json::from_value(json!({"id": doc, "format": "yaml"})).unwrap()).unwrap();
    assert!(y["text"].as_str().unwrap().contains("---\n"));
    let md = rdy_read(&mut st, serde_json::from_value(json!({"id": doc, "format": "markdown"})).unwrap()).unwrap();
    assert!(md["text"].as_str().unwrap().contains("\n\n"));
}

#[test]
fn readers_cannot_write_or_upload() {
    let (_d, mut st) = state();
    rdy_session(&mut st, serde_json::from_value(json!({"actor": "agent:r", "role": "reader"})).unwrap()).unwrap();
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "glossary"}}]})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    let e = rdy_asset(&mut st, serde_json::from_value(json!({"data_base64": "aGVsbG8="})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
}

#[test]
fn justification_mapping_and_conflict() {
    use rdy_model::{Change, EditorialClass, Justification};
    let (_d, mut st) = state();
    start(&mut st);
    let res = seed(&mut st);
    let c = res["created"]["$c"].as_str().unwrap().to_string();
    let d = rdy_decide(&mut st, serde_json::from_value(json!({"parent": res["created"]["$s"], "statement": "S"})).unwrap()).unwrap();
    let did = d["created"]["$decision"].as_str().unwrap().to_string();
    let e = rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "update", "id": c, "body": "X."}], "implements": did, "editorial": {"class": "typo", "reason": "r"}})).unwrap()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    let last = |st: &McpState| {
        let v = st.rdy.store().view("run-7").unwrap();
        st.rdy.store().get::<Change>(v.changes.last().unwrap()).unwrap()
    };
    rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "update", "id": c, "body": "Edited."}], "editorial": {"class": "clarify", "reason": "why"}})).unwrap()).unwrap();
    assert_eq!(last(&st).justification, Some(Justification::Editorial { class: EditorialClass::Clarify, reason: "why".into() }));
    rdy_write(&mut st, serde_json::from_value(json!({"ops": [{"op": "update", "id": c, "body": "Edited again."}], "implements": did})).unwrap()).unwrap();
    assert_eq!(last(&st).justification, Some(Justification::Implements(rdy_model::BlockId::parse(&did).unwrap())));
}
