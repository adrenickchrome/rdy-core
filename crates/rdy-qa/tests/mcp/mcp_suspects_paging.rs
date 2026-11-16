//! spg1: `rdy_suspects` pages a cut result as `rdy_query` does: a cut result carries `next`, and
//! `page` reads the rest with the same cursor form and refusals.
use rdy_engine::RdyError;
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// A term and `n` claims that cite it, landed; then the term is edited and landed, so every claim
/// is a suspect in canon. The session has its own view.
fn st_with_suspects(n: usize) -> (tempfile::TempDir, McpState) {
    let (d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim number {i} uses the {{{{$t}}}}.")}));
    }
    let seeded = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": seeded.created["$t"], "body": "Balances, per wallet."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    (d, st)
}

fn suspects(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_suspects(st, serde_json::from_value(args).unwrap())
}

fn blocks(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["block"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn spg1_a_cut_result_pages_to_every_suspect_once() {
    let (_d, mut st) = st_with_suspects(60);
    let whole = suspects(&mut st, json!({"scope": "canon", "max_tokens": 100000})).unwrap();
    assert!(whole.get("next").is_none() && whole.get("truncated").is_none());
    let want = blocks(&whole);
    assert_eq!(want.len(), 60);

    let args = |page: Option<&str>| {
        let mut a = json!({"scope": "canon", "max_tokens": 300});
        if let Some(p) = page {
            a["page"] = json!(p);
        }
        a
    };
    let first = suspects(&mut st, args(None)).unwrap();
    assert_eq!(first["truncated"], true, "{first}");
    assert!(
        first["next"].is_string(),
        "the first page has next: {first}"
    );
    assert!(first["omitted"].as_u64().unwrap() > 0, "{first}");
    let mut got = blocks(&first);
    let mut v = first;
    let mut pages = 1;
    while let Some(next) = v["next"].as_str().map(String::from) {
        v = suspects(&mut st, args(Some(&next))).unwrap();
        got.extend(blocks(&v));
        pages += 1;
        assert!(pages < 100);
    }
    assert!(pages > 2, "{pages}");
    assert_eq!(got, want, "every suspect once, in the one order");
    assert!(v.get("next").is_none(), "the last page has no next: {v}");
    assert_eq!(v["omitted"], 0, "{v}");
}

#[test]
fn spg1_a_foreign_or_stale_page_is_refused_as_a_paged_read_refuses() {
    let (_d, mut st) = st_with_suspects(60);
    let first = suspects(&mut st, json!({"scope": "canon", "max_tokens": 300})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();

    // not a cursor at all
    let e = suspects(&mut st, json!({"scope": "canon", "page": "nope"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "page is not a cursor from rdy_suspects's next");
    assert_eq!(e.fix, "pass next from the previous page, unchanged");

    // a cursor of another read (rdy_query's) is stale
    let q = rdy_query(
        &mut st,
        serde_json::from_value(
            json!({"filter": "type = claim", "scope": "canon", "max_tokens": 300}),
        )
        .unwrap(),
    )
    .unwrap();
    let foreign = q["next"].as_str().unwrap();
    let e = suspects(&mut st, json!({"scope": "canon", "page": foreign})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e:?}");

    // the same cursor in another scope (the session view) is stale
    let e = suspects(&mut st, json!({"max_tokens": 300, "page": next})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e:?}");

    // a cursor past the end reads as one that does not parse
    let (token, _) = next.split_once(':').unwrap();
    let e = suspects(
        &mut st,
        json!({"scope": "canon", "page": format!("{token}:9999")}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
}

#[test]
fn spg1_a_changed_suspect_set_stales_the_cursor() {
    let (_d, mut st) = st_with_suspects(60);
    let first = suspects(&mut st, json!({"max_tokens": 300})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();
    let block = blocks(&first)[0].clone();
    // a repin in the session view clears one suspect there
    rdy_write(
        &mut st,
        serde_json::from_value(json!({"ops": [{"op": "repin", "id": block}]})).unwrap(),
    )
    .unwrap();
    let e = suspects(&mut st, json!({"max_tokens": 300, "page": next})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e:?}");
}

#[test]
fn spg1_a_result_that_fits_has_no_next_and_is_todays_output() {
    let (_d, mut st) = st_with_suspects(3);
    let got = suspects(&mut st, json!({"scope": "canon"})).unwrap();
    let want = {
        let read = st.engine.read();
        json!({"items": read.suspect_details(&rdy_engine::Scope::Canon).unwrap()})
    };
    assert_eq!(
        serde_json::to_string(&got).unwrap(),
        serde_json::to_string(&want).unwrap()
    );
    assert!(got.get("next").is_none() && got.get("truncated").is_none());
}
