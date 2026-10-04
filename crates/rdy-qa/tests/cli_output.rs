//! The human boundary's output: edit (edit1), show (show1), impact (imp1).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn edit1_an_edit_that_changes_pins_is_refused_and_writes_nothing() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]))
    .unwrap();
    let scope = Scope::View("work".into());
    let c = r
        .block(&scope, &BlockId::parse(&res.created["$c"]).unwrap())
        .unwrap();
    let t = BlockId::parse(&res.created["$t"]).unwrap();
    let before = r.store().view("work").unwrap().changes.len();
    let mut only_pins = c.clone();
    only_pins.pins.insert(t.clone(), rdy_model::Hash([7; 32]));
    let mut pins_and_body = only_pins.clone();
    pins_and_body.body = "Uses the ledger.".into();
    for edited in [only_pins, pins_and_body] {
        let e = r.edit_block(&writer(), "work", edited).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert!(e.fix.contains("pins are set by rdy"), "{e}");
    }
    assert_eq!(
        r.store().view("work").unwrap().changes.len(),
        before,
        "no change committed"
    );
}
