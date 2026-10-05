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

/// The compact read of one doc holding `blocks` (each a create op under the doc's section), as
/// its lines keyed by block id.
fn compact_lines(
    blocks: serde_json::Value,
) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Flows", "doc_type": "flows"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Flows"}}),
    ];
    for b in blocks.as_array().unwrap() {
        let mut op = b.clone();
        op["parent"] = json!("$s");
        ops.push(op);
    }
    let res = write(&mut r, "work", json!(ops)).unwrap();
    let mut st = McpState::new(r);
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": res.created["$d"]})).unwrap(),
    )
    .unwrap();
    let text = read["text"].as_str().unwrap().to_string();
    let lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
    let by_id = lines
        .iter()
        .filter_map(|l| l.split_once(' '))
        .map(|(id, rest)| (id.to_string(), rest.to_string()))
        .collect();
    (lines, by_id)
}

/// A body with no sentence terminator is not shown whole: a diagram reads as its alt text.
#[test]
fn read1_a_diagram_reads_as_its_alt_text() {
    let (_lines, by_id) = compact_lines(json!([
        {"op": "create", "type": "diagram", "id": "$g", "body": "graph TD\n  A-->B\n  B-->C\n  C-->D",
         "fields": {"lang": "mermaid", "alt": "Flow of money"}}
    ]));
    let g = by_id.iter().find(|(k, _)| k.starts_with("dg-")).unwrap().1;
    assert_eq!(g, "diagram: Flow of money", "{by_id:?}");
}

/// Code and Mermaid often contain ". ": a non-prose body is never cut at one, only at its first
/// line, and a long line is capped with an ellipsis.
#[test]
fn read1_a_code_block_reads_as_its_first_line_capped() {
    let long = format!("let s = \"{}\";", "x".repeat(300));
    let (lines, by_id) = compact_lines(json!([
        {"op": "create", "type": "code", "id": "$a", "body": "let n = a. b;\nlet m = 2;", "fields": {"lang": "rust"}},
        {"op": "create", "type": "code", "id": "$b", "body": long, "fields": {"lang": "rust"}}
    ]));
    let codes: Vec<&String> = by_id
        .iter()
        .filter(|(k, _)| k.starts_with("cd-"))
        .map(|(_, v)| v)
        .collect();
    assert_eq!(codes.len(), 2, "{lines:?}");
    assert!(
        codes.iter().any(|c| *c == "code: let n = a. b;"),
        "{codes:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("let m")), "{lines:?}");
    let capped = codes.iter().find(|c| c.contains("xxx")).unwrap();
    let body = capped.strip_prefix("code: ").unwrap();
    assert!(body.chars().count() <= 200, "{}", body.chars().count());
    assert!(body.ends_with('…'), "{body}");
}

/// Prose with no terminator shows its first line, and with one, its first sentence as before.
#[test]
fn read1_prose_without_a_terminator_shows_only_its_first_line() {
    let (lines, by_id) = compact_lines(json!([
        {"op": "create", "type": "claim", "id": "$c", "body": "Refunds go through the ledger\nand a second line follows"}
    ]));
    let c = by_id.iter().find(|(k, _)| k.starts_with("c-")).unwrap().1;
    assert_eq!(c, "claim: Refunds go through the ledger", "{lines:?}");
}
