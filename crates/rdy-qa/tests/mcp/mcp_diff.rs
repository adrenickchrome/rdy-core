//! dq7: `rdy_diff` over MCP: the engine's page, the budget, parameter errors, the schema paragraph,
//! stale cursors and the golden.
use rdy_engine::diff_request::{DIFF_DEFAULT_TOKENS, DIFF_DOC, DiffRequest};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};

/// The description of `rdy_diff` (O22).
const DIFF_DESCRIPTION: &str = "Compare two scopes block by block, older to newer. With only from, rdy supplies the other side: a view diffs from its base landing, tag: or landing: diffs to canon, canon diffs from its previous landing, and proposal:P-x@n diffs from the version's base. inflight needs both sides. doc limits both sides to one doc or folder; a block that moved in or out shows as added or removed, with moved naming the other side; the blocks under it show as added or removed without moved. Compact by default: ids, paths, change kinds, summaries and changed field names. detail: true adds field values and word-level body diffs. max_tokens defaults to 4000. A cut drops body pieces first, then field values, then entries from the end; counts are never cut, omitted counts what left, and next, passed as page, reads the rest. rdy_schema's diff documents the shape.";
/// The description of `rdy_query` (O22).
const QUERY_DESCRIPTION: &str = "Filter blocks with an expression, such as type = decision and status = live. Returns matches {id, block_type, path, summary} in reading order, and total. scope defaults to your session's view. max_tokens defaults to 4000; a cut result carries next: pass it as page with the same filter and scope. rdy_schema's filter gives the grammar. A parse error names the column. rdy_search finds text; rdy_query selects by fields and links.";

/// A project with a landed doc of ten claims and a view `work` that adds `n` claims and edits one.
/// The session writes to `work`.
pub(crate) fn st_with_work(n: usize) -> (tempfile::TempDir, McpState, Vec<String>) {
    let (d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}),
    ];
    for i in 0..10 {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim number {i} holds.")}));
    }
    let seeded = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let mut ops = vec![
        json!({"op": "update", "id": seeded.created["$c3"], "body": "Claim number 3 changed."}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$n{i}"), "parent": seeded.created["$s"], "body": format!("Added claim number {i} holds.")}));
    }
    let w = write(&mut r, "work", Value::Array(ops)).unwrap();
    let mut ids: Vec<String> = (0..n)
        .map(|i| w.created[&format!("$n{i}")].clone())
        .collect();
    ids.insert(0, seeded.created["$c3"].clone());
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    (d, st, ids)
}

fn diff(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_diff(st, serde_json::from_value(args).unwrap())
}

#[test]
fn dq7_the_tool_list_names_rdy_diff_and_rdy_query() {
    let (dir, r) = project();
    drop::<Rdy>(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let find = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .clone()
    };
    assert_eq!(find("rdy_diff")["description"], DIFF_DESCRIPTION);
    assert_eq!(find("rdy_query")["description"], QUERY_DESCRIPTION);
    let log = find("rdy_log");
    assert!(
        log["description"].as_str().unwrap().ends_with(" Pass landing (a landing id of at least 6 hex characters, landing:<id> or tag:<version>) to read one landing: its row and its change list, a compact diff from its previous landing. max_tokens cuts the change list, and page continues it."),
        "{log}"
    );
    let props = log["inputSchema"]["properties"].as_object().unwrap();
    assert!(
        props.contains_key("landing") && props.contains_key("page"),
        "{log}"
    );
}

#[test]
fn dq7_rdy_diff_equals_the_engine_page() {
    let (_d, mut st, ids) = st_with_work(5);
    let got = diff(&mut st, json!({"from": "work"})).unwrap();
    let want = {
        let read = st.engine.read();
        serde_json::to_value(
            read.diff_page(
                &DiffRequest {
                    from: "work".into(),
                    ..Default::default()
                },
                Some("work"),
                None,
                DIFF_DEFAULT_TOKENS,
            )
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(got, want);
    assert_eq!(
        got["counts"],
        json!({"added": 5, "removed": 0, "updated": 1, "moved": 0})
    );
    assert_eq!(got["entries"].as_array().unwrap().len(), 6);
    assert!(ids.iter().all(|id| {
        got["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == id.as_str())
    }));
    // `view:` spells the same scope, and the session view is the one-argument default's `to`
    assert_eq!(diff(&mut st, json!({"from": "view:work"})).unwrap(), got);
}

#[test]
fn dq7_a_diff_cut_by_max_tokens_pages_to_the_whole() {
    let (_d, mut st, ids) = st_with_work(60);
    let mut seen: Vec<String> = vec![];
    let mut v = diff(&mut st, json!({"from": "work", "max_tokens": 600})).unwrap();
    let mut pages = 1;
    loop {
        assert_eq!(v.get("next").is_some(), v["truncated"] == true, "{v}");
        assert!(serde_json::to_string(&v).unwrap().len() / 4 <= 600);
        seen.extend(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_string()),
        );
        let Some(next) = v["next"].as_str().map(String::from) else {
            break;
        };
        v = diff(
            &mut st,
            json!({"from": "work", "max_tokens": 600, "page": next}),
        )
        .unwrap();
        pages += 1;
        assert!(pages < 100);
    }
    assert!(pages > 2);
    let mut want = ids.clone();
    want.sort();
    seen.sort();
    assert_eq!(seen, want);
}

#[test]
fn dq7_max_tokens_over_the_limit_is_refused() {
    let (_d, mut st, _) = st_with_work(1);
    let e = diff(&mut st, json!({"from": "work", "max_tokens": 100_001})).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(e.message, "max_tokens 100001 is over the limit of 100000");
    assert_eq!(
        e.fix,
        "ask for at most 100000 tokens and page through the rest"
    );
    let e = rdy_query(
        &mut st,
        serde_json::from_value(json!({"filter": "type = claim", "max_tokens": 100_001})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "max_tokens 100001 is over the limit of 100000")
    );
    let e = rdy_mcp::view_tools::rdy_log(
        &mut st,
        serde_json::from_value(json!({"landing": "canon", "max_tokens": 100_001})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "max_tokens 100001 is over the limit of 100000")
    );
    // the limit itself passes
    diff(&mut st, json!({"from": "work", "max_tokens": 100_000})).unwrap();
}

#[test]
fn dq7_an_unknown_parameter_names_the_nearest() {
    let (dir, r) = project();
    drop::<Rdy>(r);
    let mut c = McpClient::start(dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:q", "run": "q1", "role": "writer"}),
    )
    .ok();
    for (tool, args, near) in [
        (
            "rdy_diff",
            json!({"from": "canon", "detial": true}),
            "detail",
        ),
        ("rdy_query", json!({"filtr": "type = claim"}), "filter"),
    ] {
        let Reply::ToolError(e) = c.call(tool, args) else {
            panic!("{tool}: an unknown parameter is an isError reply")
        };
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{tool}: unknown parameter \"")),
            "{e}"
        );
        assert!(
            e["fix"]
                .as_str()
                .unwrap()
                .starts_with(&format!("did you mean \"{near}\"?")),
            "{e}"
        );
    }
}

#[test]
fn dq7_rdy_schema_documents_the_diff_and_the_filter() {
    let (_d, mut st, _) = st_with_work(1);
    let v = rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert_eq!(v["diff"], DIFF_DOC);
    assert_eq!(v["filter"], rdy_schema::filter::FILTER_GUIDE);
}

#[test]
fn dq7_a_stale_page_carries_the_new_fix() {
    let (_d, mut st, ids) = st_with_work(60);
    let first = diff(&mut st, json!({"from": "work", "max_tokens": 600})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();
    let target = ids[0].clone();
    st.engine
        .write(Box::new(move |r| {
            write(
                r,
                "work",
                json!([{"op": "update", "id": target, "body": "Changed again."}]),
            )?;
            Ok(Value::Null)
        }))
        .unwrap();
    let e = diff(
        &mut st,
        json!({"from": "work", "max_tokens": 600, "page": next}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e}");
    assert_eq!(
        e.fix,
        "the read, briefing, search, diff or query moved; start again without a cursor"
    );
}

#[test]
fn dq7_a_reader_session_reads_both() {
    let (_d, writer, _) = st_with_work(2);
    let mut st = McpState::new(writer.engine.clone());
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:reader", "role": "reviewer"})).unwrap(),
    )
    .unwrap();
    // a reader's view is canon: the one-argument default reads the view it names
    let v = diff(&mut st, json!({"from": "work"})).unwrap();
    assert_eq!(v["counts"]["added"], 2, "{v}");
    let q = rdy_query(
        &mut st,
        serde_json::from_value(json!({"filter": "type = claim"})).unwrap(),
    )
    .unwrap();
    assert_eq!(q["total"], 10, "{q}");
}

#[test]
fn dq7_rdy_diff_matches_its_golden() {
    crate::mcp_golden::check_goldens("rdy_diff--");
}
