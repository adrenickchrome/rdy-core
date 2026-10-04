use rdy_model::{Block, BlockId, FieldValue, Link};
use rdy_schema::*;
use std::collections::BTreeMap;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn b(id: &str, ty: &str, parent: Option<&str>, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn section() -> Block {
    let mut s = b("s-SECT1", "section", Some("d-D0C11"), "");
    s.fields.insert("title".into(), FieldValue::Str("Goals".into()));
    s
}

fn codes(issues: &[Issue]) -> Vec<&'static str> {
    issues.iter().map(|i| i.code).collect()
}

#[test]
fn valid_claim_passes() {
    let c = b("c-AAAAA", "claim", Some("s-SECT1"), "Wallets don't move.");
    assert!(validate_block(&pack(), &c, Some(&section())).is_empty());
}

#[test]
fn type_prefix_and_fields() {
    let p = pack();
    assert_eq!(codes(&validate_block(&p, &b("c-AAAAA", "gizmo", Some("s-SECT1"), "x"), Some(&section()))), vec!["E_UNKNOWN_TYPE"]);
    assert!(codes(&validate_block(&p, &b("t-AAAAA", "claim", Some("s-SECT1"), "x"), Some(&section()))).contains(&"E_PREFIX"));

    let mut term = b("t-AAAAA", "term", Some("s-SECT1"), "A ledger.");
    assert!(codes(&validate_block(&p, &term, Some(&section()))).contains(&"E_FIELD_REQUIRED"));
    term.fields.insert("name".into(), FieldValue::Int(3));
    assert!(codes(&validate_block(&p, &term, Some(&section()))).contains(&"E_FIELD_KIND"));
    term.fields.insert("name".into(), FieldValue::Str("Ledger".into()));
    term.fields.insert("colour".into(), FieldValue::Str("red".into()));
    assert_eq!(codes(&validate_block(&p, &term, Some(&section()))), vec!["E_FIELD_UNKNOWN"]);

    let mut req = b("r-AAAAA", "requirement", Some("s-SECT1"), "x");
    req.fields.insert("priority".into(), FieldValue::Str("urgent".into()));
    let issues = validate_block(&p, &req, Some(&section()));
    assert_eq!(codes(&issues), vec!["E_FIELD_ENUM"]);
    assert!(issues[0].fix.contains("must"), "fix lists allowed values: {}", issues[0].fix);
}

#[test]
fn body_rules_and_links() {
    let p = pack();
    assert_eq!(codes(&validate_block(&p, &b("c-AAAAA", "claim", Some("s-SECT1"), "  "), Some(&section()))), vec!["E_BODY_REQUIRED"]);
    let mut c = b("c-AAAAA", "claim", Some("s-SECT1"), "x");
    c.links.insert("likes".into(), vec![Link { target: BlockId::parse("t-AAAAA").unwrap(), part: None }]);
    assert_eq!(codes(&validate_block(&p, &c, Some(&section()))), vec!["E_LINK_TYPE"]);
}

#[test]
fn tree_rules() {
    let p = pack();
    assert_eq!(codes(&validate_block(&p, &b("c-AAAAA", "claim", None, "x"), None)), vec!["E_ORPHAN"]);
    let mut doc = b("d-AAAAA", "doc", None, "");
    doc.fields.insert("title".into(), FieldValue::Str("Identity".into()));
    doc.fields.insert("doc_type".into(), FieldValue::Str("overview".into()));
    assert!(validate_block(&p, &doc, None).is_empty());
    doc.fields.insert("doc_type".into(), FieldValue::Str("memo".into()));
    assert_eq!(codes(&validate_block(&p, &doc, None)), vec!["E_DOC_TYPE"]);
    // a claim directly under a doc is not allowed (docs hold sections)
    doc.fields.insert("doc_type".into(), FieldValue::Str("overview".into()));
    assert_eq!(codes(&validate_block(&p, &b("c-AAAAA", "claim", Some("d-AAAAA"), "x"), Some(&doc))), vec!["E_CHILD_TYPE"]);
}

#[test]
fn skeleton_gaps_are_warnings() {
    let p = pack();
    let mut doc = b("d-AAAAA", "doc", None, "");
    doc.fields.insert("doc_type".into(), FieldValue::Str("overview".into()));
    let gaps = skeleton_gaps(&p, &doc, &["Goals"]);
    assert_eq!(codes(&gaps), vec!["W_SKELETON_MISSING"]);
    assert!(gaps[0].message.contains("Summary"));
    assert!(!has_errors(&gaps));
}
