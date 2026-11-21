//! wck7 (D-ZRXY0): a briefing shows no module rule row, as it shows no filter rule row. The
//! briefing of a claim that module rules fail and trap on, in canon and in a view, holds no rule
//! text, and the briefing runs no module.
use rdy_engine::check::module_run::module_runs_on_this_thread_for_tests;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::{McpState, rdy_brief, rdy_session};
use rdy_qa::fixtures::*;
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{j_typo, put_j};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;

const FAIL_TEXT: &str = include_str!("../../fixtures/wasm/fail-text.b64");
const TRAP: &str = include_str!("../../fixtures/wasm/trap.b64");

/// Pack `wck7`: a module rule of claims that fails with its own text, one that traps, and a
/// filter rule that every claim fails, all advisory.
fn pack() -> String {
    let m = |name: &str, module: &str| {
        format!(
            "  - name: {name}\n    select: type = claim\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
            module.trim()
        )
    };
    format!(
        "name: wck7\nversion: \"1\"\nextends: rdy-standard\nrules:\n{}{}  - name: d-filter\n    select: type = claim\n    require: type = note\n    message: d-filter message\n    fix: d-filter fix\n",
        m("a-text", FAIL_TEXT),
        m("b-traps", TRAP)
    )
}

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

#[test]
fn wck7_brief_has_no_module_rule_rows() {
    let (d, r) = project();
    drop(r);
    use_pack(
        d.path(),
        "wck7",
        &pack(),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    let mut r = Rdy::open(d.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Landed claims hold."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let (c, s) = (w.created["$c"].clone(), w.created["$s"].clone());
    let o = put_j(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$o", "parent": s, "body": "Open claims hold."}]),
        j_typo(),
    );
    let open = o.created["$o"].clone();
    let e = Arc::new(Local::new(r));
    let mut st = McpState::new(e.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:a", "run": "a", "role": "writer"})),
    )
    .unwrap();
    let runs = module_runs_on_this_thread_for_tests();
    for args in [
        json!({"ids": [c]}),
        json!({"ids": [c, open], "scope": "a"}),
        json!({"path": "notes"}),
        json!({"path": "notes", "scope": "a"}),
    ] {
        let b = rdy_brief(&mut st, p(args.clone())).unwrap().to_string();
        for word in [
            "W_RULE_CHECK",
            "E_RULE_CHECK",
            "could not judge",
            "the module says no",
            "the module trapped",
            "a-text",
            "b-traps",
            "d-filter",
        ] {
            assert!(!b.contains(word), "{word} in the briefing of {args}: {b}");
        }
    }
    assert_eq!(
        module_runs_on_this_thread_for_tests(),
        runs,
        "a briefing runs no module"
    );
}
