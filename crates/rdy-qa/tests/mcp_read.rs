//! rdy_read markdown (md1).
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::json;

/// Includes Review Focus 4: a table cell holding `|`, and a decision body of several lines.
#[test]
fn md1_markdown_carries_every_blocks_content_and_id() {
    let (_d, mut r) = project();
    let blob = r
        .store_mut()
        .put_blob(b"\x89PNG chart")
        .unwrap()
        .to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Wallets", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "section", "id": "$sub", "parent": "$s", "fields": {"title": "Details"}},
        {"op": "create", "type": "term", "parent": "$sub", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "question", "parent": "$sub", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "flow", "parent": "$sub", "fields": {"name": "Checkout", "steps": ["Open wallet", "Pay"]}},
        {"op": "create", "type": "table", "parent": "$sub", "fields": {"columns": ["Term", "Meaning"], "rows": [["Ledger", "Balances | net"]]}},
        {"op": "create", "type": "figure", "parent": "$sub", "fields": {"blob": blob, "media_type": "image/png", "alt": "chart"}},
        {"op": "create", "type": "figure", "parent": "$sub", "fields": {"blob": blob, "media_type": "image/png", "alt": "a]b\\c"}},
        {"op": "create", "type": "figure", "parent": "$sub", "fields": {"blob": blob, "media_type": "image/png", "alt": "x[y\nz"}},
        {"op": "create", "type": "code", "parent": "$sub", "fields": {"lang": "r`u st\nx"}, "body": "fn main() {}"},
        {"op": "create", "type": "code", "parent": "$sub", "fields": {"lang": "md"}, "body": "before\n```\nnot a fence end\n````\nafter"}
    ]))
    .unwrap();
    let sub = res.created["$sub"].clone();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: sub.clone(),
            statement: "Refunds need approval".into(),
            options: vec!["Approver signs".into(), "Auto-approve".into()],
            body: "Line one.\nLine two.".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut st = McpState::new(r);
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    let read = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": "area", "format": "markdown", "max_tokens": 50000}))
            .unwrap(),
    )
    .unwrap();
    let text = read["text"].as_str().unwrap();
    let id = |k: &str| res.created[k].to_string();
    for want in [
        format!("# area/ {{#{}}}", id("$f")),
        format!("# Wallets {{#{}}}", id("$d")),
        format!("## Terms {{#{}}}", id("$s")),
        format!("### Details {{#{}}}", id("$sub")),
        "**Ledger** — Balances.".to_string(),
        "**Question:** Who approves refunds?".to_string(),
        "**Flow: Checkout**".to_string(),
        "1. Open wallet\n2. Pay".to_string(),
        "| Term | Meaning |".to_string(),
        "| Ledger | Balances \\| net |".to_string(),
        format!("![chart](rdy-blob:{blob}) (image/png)"),
        "decision [proposed]: Refunds need approval".to_string(),
        "> - Approver signs\n> - Auto-approve".to_string(),
        "> Line one.\n> Line two.".to_string(),
    ] {
        assert!(text.contains(&want), "missing {want:?} in:\n{text}");
    }
    // a code body holding ``` lines stays inside a longer fence
    assert!(
        text.contains("`````md\nbefore\n```\nnot a fence end\n````\nafter\n`````"),
        "code fence in:\n{text}"
    );
    // `]` and `\` in alt text cannot end the image syntax early
    assert!(
        text.contains(&format!("![a\\]b\\\\c](rdy-blob:{blob}) (image/png)")),
        "alt escape in:\n{text}"
    );
    // `[` is escaped and a newline becomes a space
    assert!(
        text.contains(&format!("![x\\[y z](rdy-blob:{blob}) (image/png)")),
        "alt [ and newline in:\n{text}"
    );
    // a lang with a backtick or whitespace is cut down to a valid info string
    assert!(
        text.contains("```ru\nfn main() {}\n```"),
        "lang in:\n{text}"
    );
    let ids: Vec<String> = st
        .rdy
        .materialized(&Scope::View("work".into()))
        .unwrap()
        .blocks
        .keys()
        .map(|k| k.to_string())
        .collect();
    for b in ids {
        assert!(text.contains(&b), "block {b} has no id in:\n{text}");
    }
}
