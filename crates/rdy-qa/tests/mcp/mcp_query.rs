//! dq7: `rdy_query` over MCP: the engine's page, paging, the error envelope and the golden.
use crate::mcp_diff::st_with_work;
use rdy_mcp::tools::*;
use serde_json::{Value, json};

fn query(st: &mut McpState, args: Value) -> Result<Value, rdy_engine::RdyError> {
    rdy_query(st, serde_json::from_value(args).unwrap())
}

#[test]
fn dq7_rdy_query_equals_the_engine_page() {
    let (_d, mut st, _) = st_with_work(5);
    // the session view is the default scope: ten landed claims and five added
    let got = query(&mut st, json!({"filter": "type = claim"})).unwrap();
    let want = {
        let read = st.engine.read();
        serde_json::to_value(
            read.query_page(
                &rdy_engine::Scope::View("work".into()),
                "work",
                "type = claim",
                None,
                rdy_engine::filter::QUERY_DEFAULT_TOKENS,
            )
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(got, want);
    assert_eq!(got["total"], 15);
    let canon = query(&mut st, json!({"filter": "type = claim", "scope": "canon"})).unwrap();
    assert_eq!(canon["total"], 10);
}

#[test]
fn dq7_a_query_cut_by_max_tokens_pages_to_the_whole() {
    let (_d, mut st, _) = st_with_work(60);
    let mut seen = 0;
    let mut v = query(
        &mut st,
        json!({"filter": "type = claim", "max_tokens": 500}),
    )
    .unwrap();
    let mut pages = 1;
    loop {
        seen += v["matches"].as_array().unwrap().len();
        let Some(next) = v["next"].as_str().map(String::from) else {
            break;
        };
        v = query(
            &mut st,
            json!({"filter": "type = claim", "max_tokens": 500, "page": next}),
        )
        .unwrap();
        pages += 1;
        assert!(pages < 100);
    }
    assert!(pages > 2);
    assert_eq!(seen, 70);
}

#[test]
fn dq7_rdy_query_parse_error_envelope() {
    let (_d, mut st, _) = st_with_work(1);
    let e = query(&mut st, json!({"filter": "tpye = claim"})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        "filter: unknown field tpye; did you mean type? at column 1"
    );
    assert_eq!(
        e.fix,
        "read the filter guide in rdy_schema, or run rdy q --help"
    );
}

#[test]
fn dq7_rdy_query_matches_its_golden() {
    crate::mcp_golden::check_goldens("rdy_query--");
}
