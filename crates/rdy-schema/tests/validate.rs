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
    s.fields
        .insert("title".into(), FieldValue::Str("Goals".into()));
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
    assert_eq!(
        codes(&validate_block(
            &p,
            &b("c-AAAAA", "gizmo", Some("s-SECT1"), "x"),
            Some(&section())
        )),
        vec!["E_UNKNOWN_TYPE"]
    );
    // R-P1: a retyped block keeps its id, so the prefix is checked only when a write creates one
    assert!(
        !codes(&validate_block(
            &p,
            &b("t-AAAAA", "claim", Some("s-SECT1"), "x"),
            Some(&section())
        ))
        .contains(&"E_PREFIX")
    );

    let mut term = b("t-AAAAA", "term", Some("s-SECT1"), "A ledger.");
    assert!(codes(&validate_block(&p, &term, Some(&section()))).contains(&"E_FIELD_REQUIRED"));
    term.fields.insert("name".into(), FieldValue::Int(3));
    assert!(codes(&validate_block(&p, &term, Some(&section()))).contains(&"E_FIELD_KIND"));
    term.fields
        .insert("name".into(), FieldValue::Str("Ledger".into()));
    term.fields
        .insert("colour".into(), FieldValue::Str("red".into()));
    assert_eq!(
        codes(&validate_block(&p, &term, Some(&section()))),
        vec!["E_FIELD_UNKNOWN"]
    );

    let mut req = b("r-AAAAA", "requirement", Some("s-SECT1"), "x");
    req.fields
        .insert("priority".into(), FieldValue::Str("urgent".into()));
    let issues = validate_block(&p, &req, Some(&section()));
    assert_eq!(codes(&issues), vec!["E_FIELD_ENUM"]);
    assert!(
        issues[0].fix.contains("must"),
        "fix lists allowed values: {}",
        issues[0].fix
    );
}

#[test]
fn body_rules_and_links() {
    let p = pack();
    assert_eq!(
        codes(&validate_block(
            &p,
            &b("c-AAAAA", "claim", Some("s-SECT1"), "  "),
            Some(&section())
        )),
        vec!["E_BODY_REQUIRED"]
    );
    let mut c = b("c-AAAAA", "claim", Some("s-SECT1"), "x");
    c.links.insert(
        "likes".into(),
        vec![Link {
            target: BlockId::parse("t-AAAAA").unwrap(),
            part: None,
        }],
    );
    assert_eq!(
        codes(&validate_block(&p, &c, Some(&section()))),
        vec!["E_LINK_TYPE"]
    );
}

#[test]
fn tree_rules() {
    let p = pack();
    assert_eq!(
        codes(&validate_block(&p, &b("c-AAAAA", "claim", None, "x"), None)),
        vec!["E_ORPHAN"]
    );
    let mut doc = b("d-AAAAA", "doc", None, "");
    doc.fields
        .insert("name".into(), FieldValue::Str("identity".into()));
    doc.fields
        .insert("title".into(), FieldValue::Str("Identity".into()));
    doc.fields
        .insert("doc_type".into(), FieldValue::Str("overview".into()));
    assert!(validate_block(&p, &doc, None).is_empty());
    doc.fields
        .insert("doc_type".into(), FieldValue::Str("memo".into()));
    assert_eq!(codes(&validate_block(&p, &doc, None)), vec!["E_DOC_TYPE"]);
    // a claim directly under a doc is allowed (docs hold every content type)
    doc.fields
        .insert("doc_type".into(), FieldValue::Str("overview".into()));
    assert!(
        validate_block(&p, &b("c-AAAAA", "claim", Some("d-AAAAA"), "x"), Some(&doc)).is_empty()
    );
    // a doc under a doc is not
    let inner = b("d-BBBBB", "doc", Some("d-AAAAA"), "");
    assert!(codes(&validate_block(&p, &inner, Some(&doc))).contains(&"E_CHILD_TYPE"));
}

#[test]
fn skeleton_gaps_are_warnings() {
    let p = pack();
    let mut doc = b("d-AAAAA", "doc", None, "");
    doc.fields
        .insert("doc_type".into(), FieldValue::Str("overview".into()));
    let gaps = skeleton_gaps(&p, &doc, &["Goals"]);
    assert_eq!(codes(&gaps), vec!["W_SKELETON_MISSING"]);
    assert!(gaps[0].message.contains("Summary"));
    assert!(!has_errors(&gaps));
}

#[test]
fn missing_parent_is_an_error() {
    let p = pack();
    let issues = validate_block(&p, &b("c-AAAAA", "claim", Some("s-SECT1"), "x"), None);
    assert_eq!(codes(&issues), vec!["E_MISSING_PARENT"]);
    assert_eq!(issues[0].severity, Severity::Error);
    assert!(issues[0].message.contains("s-SECT1"));
}

#[test]
fn hints_keep_the_field_name_case() {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml("name: cased\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  risk:\n    prefix: rk\n    fields:\n      dueDate: { kind: Str, required: true }\n".replace("Str", "str").as_str()).unwrap();
    let p = set.resolve(&name).unwrap();
    let mut risk = b("rk-AAAAA", "risk", Some("s-SECT1"), "");
    let missing = validate_block(&p, &risk, Some(&section()));
    assert_eq!(missing[0].fix, "add dueDate (str)");
    risk.fields.insert("dueDate".into(), FieldValue::Int(1));
    let kind = validate_block(&p, &risk, Some(&section()));
    assert_eq!(kind[0].fix, "write dueDate as a str value");
}

fn named(id: &str, ty: &str, parent: Option<&str>, name: &str) -> Block {
    let mut x = b(id, ty, parent, "");
    x.fields.insert("name".into(), FieldValue::Str(name.into()));
    if ty == "doc" {
        x.fields.insert("title".into(), FieldValue::Str("T".into()));
        x.fields
            .insert("doc_type".into(), FieldValue::Str("overview".into()));
    }
    x
}

#[test]
fn folders_hold_docs_and_folders_only() {
    let p = pack();
    let folder = named("fd-AAAAA", "folder", None, "wallets");
    assert!(
        validate_block(&p, &folder, None).is_empty(),
        "a folder may be top level"
    );
    let doc = named("d-AAAAA", "doc", Some("fd-AAAAA"), "transactions");
    assert!(
        validate_block(&p, &doc, Some(&folder)).is_empty(),
        "a doc may be in a folder"
    );
    let sub = named("fd-BBBBB", "folder", Some("fd-AAAAA"), "cards");
    assert!(validate_block(&p, &sub, Some(&folder)).is_empty());
    assert_eq!(
        codes(&validate_block(
            &p,
            &named("d-BBBBB", "doc", Some("s-SECT1"), "x"),
            Some(&section())
        )),
        vec!["E_CHILD_TYPE"]
    );
    assert_eq!(
        codes(&validate_block(
            &p,
            &named("fd-CCCCC", "folder", Some("d-AAAAA"), "x"),
            Some(&doc)
        )),
        vec!["E_CHILD_TYPE"]
    );
    let mut top_section = section();
    top_section.parent = None;
    assert_eq!(
        codes(&validate_block(&p, &top_section, None)),
        vec!["E_ORPHAN"]
    );
}

#[test]
fn doc_and_folder_names_are_checked() {
    let p = pack();
    let i = validate_block(&p, &named("fd-AAAAA", "folder", None, "has space"), None);
    assert_eq!(codes(&i), vec!["E_BAD_NAME"]);
    assert!(i[0].fix.contains("has-space"), "{:?}", i[0]);
    let mut doc = named("d-AAAAA", "doc", None, "x");
    doc.fields.remove("name");
    assert_eq!(
        codes(&validate_block(&p, &doc, None)),
        vec!["E_FIELD_REQUIRED"]
    );
}

#[test]
fn docs_and_folders_sit_only_in_folders_whatever_the_pack_says() {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: loose\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  section:\n    prefix: s\n    children: [doc, folder, prose]\n",
        )
        .unwrap();
    let p = set.resolve(&name).unwrap();
    assert!(
        p.block_types["section"]
            .children
            .contains(&"doc".to_string()),
        "the pack lists doc under section"
    );
    for x in [
        named("d-BBBBB", "doc", Some("s-SECT1"), "x"),
        named("fd-BBBBB", "folder", Some("s-SECT1"), "x"),
    ] {
        assert_eq!(
            codes(&validate_block(&p, &x, Some(&section()))),
            vec!["E_CHILD_TYPE"],
            "{}",
            x.block_type
        );
    }
}
