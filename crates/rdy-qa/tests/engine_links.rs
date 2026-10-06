//! Link updates merge per kind (link1).
use rdy_engine::*;
use rdy_model::{Block, BlockId};
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn targets(b: &Block, kind: &str) -> Vec<String> {
    b.links
        .get(kind)
        .into_iter()
        .flatten()
        .map(|l| l.target.to_string())
        .collect()
}

fn setup() -> (tempfile::TempDir, Rdy, serde_json::Value) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Refunds", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "term", "id": "$u", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds a balance."},
        {"op": "create", "type": "decision", "id": "$dd", "parent": "$s",
         "fields": {"statement": "Approvers sign refunds", "status": "proposed"},
         "links": {"answers": [{"target": "$q"}], "affects": [{"target": "$t"}]}}
    ]))
    .unwrap();
    (d, r, serde_json::to_value(&res.created).unwrap())
}

/// Includes Review Focus 1: `links: {}` beside a field change changes no link.
#[test]
fn link1_update_replaces_only_the_kinds_it_names() {
    let (_d, mut r, ids) = setup();
    let id = |k: &str| ids[k].as_str().unwrap().to_string();
    let scope = Scope::View("work".into());
    let dd = BlockId::parse(&id("$dd")).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$dd"), "links": {"affects": [{"target": id("$u")}]}}]),
    )
    .unwrap();
    let b = r.read().block(&scope, &dd).unwrap();
    assert_eq!(
        targets(&b, "answers"),
        vec![id("$q")],
        "an unnamed kind is kept"
    );
    assert_eq!(
        targets(&b, "affects"),
        vec![id("$u")],
        "a named kind is replaced"
    );
    assert!(
        !b.pins.contains_key(&BlockId::parse(&id("$t")).unwrap()),
        "the dropped target's pin goes too"
    );
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$dd"),
        "fields": {"statement": "Approvers sign every refund"}, "links": {}}]),
    )
    .unwrap();
    let b = r.read().block(&scope, &dd).unwrap();
    assert_eq!(
        (targets(&b, "answers"), targets(&b, "affects")),
        (vec![id("$q")], vec![id("$u")])
    );
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id("$dd"), "links": {"answers": []}}]),
    )
    .unwrap();
    let b = r.read().block(&scope, &dd).unwrap();
    assert!(
        !b.links.contains_key("answers"),
        "\"answers\": [] removes the kind"
    );
    assert_eq!(targets(&b, "affects"), vec![id("$u")]);
    check(&r, &scope).unwrap();
}

/// A guard for `rdy edit` (passes before the change; fails if write.rs merges without the
/// edit_block change): removing a kind in the editor still removes it.
#[test]
fn link1_rdy_edit_removing_a_link_kind_still_removes_it() {
    let (_d, mut r, ids) = setup();
    let scope = Scope::View("work".into());
    let dd = BlockId::parse(ids["$dd"].as_str().unwrap()).unwrap();
    let mut edited = r.read().block(&scope, &dd).unwrap();
    edited.links.remove("answers");
    r.edit_block(&writer(), "work", edited, None).unwrap();
    let b = r.read().block(&scope, &dd).unwrap();
    assert!(!b.links.contains_key("answers"), "{:?}", b.links);
    assert_eq!(
        targets(&b, "affects"),
        vec![ids["$t"].as_str().unwrap().to_string()]
    );
}

#[test]
fn link1_schema_explains_link_updates() {
    let (_d, r) = project();
    let mut st =
        rdy_mcp::tools::McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    let s =
        rdy_mcp::tools::rdy_schema(&mut st, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(
        s["link_shape"]["update"]
            .as_str()
            .unwrap()
            .contains("only the link kinds it names"),
        "{s}"
    );
}
