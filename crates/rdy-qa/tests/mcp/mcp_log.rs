//! logt1: rdy_log names the proposer and takes limit and since.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::rdy_log;
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};
use std::sync::Arc;

#[test]
fn logt1_rdy_log_takes_limit_and_since_and_names_the_proposer() {
    let (_d, mut r) = project();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let l1 = r.read().canon_head().unwrap().to_string();
    let a = agent("agent:a", Some("ra"));
    r.write(
        &a,
        WriteRequest {
            view: "ra".into(),
            ops: from_value(json!([{"op": "update", "id": res.created["$c"], "body": "Two."}]))
                .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r.propose(&a, "ra", "Agent work").unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    let rows = rdy_log(&mut st, from_value(json!({})).unwrap()).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{rows}");
    assert_eq!(
        rows[0]["proposed_by"],
        json!({"actor": "agent:a", "run": "ra"})
    );
    let rows = rdy_log(&mut st, from_value(json!({"limit": 1})).unwrap()).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    let rows = rdy_log(&mut st, from_value(json!({"since": l1})).unwrap()).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    let rows = rdy_log(&mut st, from_value(json!({"since": "tag:v1.0.0"})).unwrap()).unwrap();
    assert_eq!(rows, json!([]));
    let e = rdy_log(&mut st, from_value(json!({"limit": 0})).unwrap()).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}
