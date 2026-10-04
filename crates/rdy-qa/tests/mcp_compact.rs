//! Compact rdy_read (read1).
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn read1_compact_shows_whole_fields_and_first_sentences_with_ids() {
    let long = "Refunds over the consumer-credit limit need two approvers and an entry under the consumer-credit audit rules";
    assert!(long.len() > 80);
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Refunds", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The ledger keeps every balance for {{$t}} members. A second sentence follows."},
        {"op": "create", "type": "decision", "id": "$dd", "parent": "$s", "fields": {"statement": long, "status": "proposed"}}
    ]))
    .unwrap();
    let id = |k: &str| res.created[k].clone();
    let mut st = McpState::new(r);
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": id("$d")})).unwrap(),
    )
    .unwrap();
    let text = read["text"].as_str().unwrap();
    assert!(
        text.contains(&format!(
            "{} claim: The ledger keeps every balance for {} members.",
            id("$c"),
            id("$t")
        )),
        "{text}"
    );
    assert!(!text.contains("A second sentence"), "{text}");
    assert!(
        text.contains(&format!("{} decision: {long}", id("$dd"))),
        "the whole statement: {text}"
    );
}
