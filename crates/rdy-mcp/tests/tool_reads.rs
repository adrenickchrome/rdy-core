//! rpl38: `rdy_read` and `rdy_search` return the reply of the engine's builders
//! (`rdy_engine::tool_reads`), the one the adapter returns too.

use rdy_engine::engine::Local;
use rdy_engine::search::{SearchFilters, SearchQuery};
use rdy_engine::tool_reads;
use rdy_engine::{Rdy, Scope};
use rdy_mcp::search_tools::rdy_search;
use rdy_mcp::tools::*;
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, McpState, Value) {
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(rdy)));
    rdy_session(
        &mut st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-7", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}),
    ];
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Claim {i} uses the {{{{$t}}}} and says ledger text. {}", "x".repeat(200))}));
    }
    let seeded = rdy_write(
        &mut st,
        serde_json::from_value(json!({ "ops": ops })).unwrap(),
    )
    .unwrap();
    (dir, st, seeded)
}

#[test]
fn rpl38_mcp_read_and_search_use_the_engine_reply() {
    let (_d, mut st, seeded) = project();
    let doc = seeded["created"]["$doc"].as_str().unwrap().to_string();
    let state = st.engine.read();

    // an uncut read, in each format, and a pattern read
    for (format, pattern) in [
        ("compact", None),
        ("markdown", None),
        ("yaml", None),
        ("compact", Some("ledger")),
    ] {
        let mut args =
            json!({"id": doc, "scope": "run-7", "format": format, "max_tokens": 100_000});
        if let Some(p) = pattern {
            args["pattern"] = json!(p);
        }
        let got = rdy_read(&mut st, serde_json::from_value(args).unwrap()).unwrap();
        let entries = tool_reads::read_entries(
            &state,
            &doc,
            "run-7",
            &Some("run-7".into()),
            format,
            pattern,
        )
        .unwrap();
        let token = got
            .get("next")
            .map_or_else(String::new, |n| n.as_str().unwrap().to_string());
        assert!(token.is_empty(), "the read is not cut");
        let q = json!({"id": doc});
        let want = tool_reads::read_page(&entries, &entries.token(&q), 0, Some(100_000));
        assert_eq!(got, want, "{format} {pattern:?}");
    }

    // a cut read: same page, same cursor
    let cut = rdy_read(
        &mut st,
        serde_json::from_value(json!({"id": doc, "scope": "run-7", "max_tokens": 500})).unwrap(),
    )
    .unwrap();
    assert_eq!(cut["truncated"], json!(true));
    let next = cut["next"].as_str().unwrap();
    let token = next.split(':').next().unwrap();
    let entries = tool_reads::read_entries(
        &state,
        &doc,
        "run-7",
        &Some("run-7".into()),
        "compact",
        None,
    )
    .unwrap();
    assert_eq!(cut, tool_reads::read_page(&entries, token, 0, Some(500)));

    // search
    let got = rdy_search(
        &mut st,
        serde_json::from_value(json!({"query": "ledger", "scope": "run-7"})).unwrap(),
    )
    .unwrap();
    let want = tool_reads::search_reply(
        &state,
        &Scope::View("run-7".into()),
        &SearchQuery {
            text: "ledger".into(),
            regex: false,
            ignore_case: false,
            filters: SearchFilters::default(),
        },
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(got, want);
}
