//! refl1: `reflects` is an engine field on content block types: a list of code path globs.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::*;
use std::collections::BTreeMap;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn block(id: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn section() -> Block {
    let mut s = block("s-SECT1", "section", Some("d-D0C11"));
    s.fields
        .insert("title".into(), FieldValue::Str("Errors".into()));
    s
}

fn claim(reflects: FieldValue) -> Block {
    let mut c = block("c-AAAAA", "claim", Some("s-SECT1"));
    c.body = "Errors are listed.".into();
    c.fields.insert(REFLECTS_FIELD.into(), reflects);
    c
}

fn globs(g: &[&str]) -> FieldValue {
    FieldValue::List(g.iter().map(|s| FieldValue::Str(s.to_string())).collect())
}

fn codes(b: &Block) -> Vec<&'static str> {
    validate_block(&pack(), b, Some(&section()))
        .iter()
        .map(|i| i.code)
        .collect()
}

#[test]
fn refl1_reflects_is_accepted_on_content_types_as_a_list_of_path_globs() {
    assert!(codes(&claim(globs(&["crates/*/src/error.rs", "docs/**"]))).is_empty());
    let mut doc = block("d-AAAAA", "doc", None);
    for (k, v) in [
        ("name", "errors"),
        ("title", "Errors"),
        ("doc_type", "overview"),
    ] {
        doc.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    doc.fields
        .insert(REFLECTS_FIELD.into(), globs(&["crates/*/src/error.rs"]));
    assert!(validate_block(&pack(), &doc, None).is_empty());
}

fn with_reflects(ty: &str, prefix: &str) -> Block {
    let mut b = block(&format!("{prefix}-AAAAA"), ty, Some("s-SECT1"));
    b.body = "Text.".into();
    b.fields
        .insert(REFLECTS_FIELD.into(), globs(&["crates/*/src/error.rs"]));
    b
}

#[test]
fn refl1_reflects_is_refused_on_a_decision_and_accepted_on_a_table() {
    let issues = validate_block(&pack(), &with_reflects("decision", "D"), Some(&section()));
    let hit: Vec<_> = issues
        .iter()
        .filter(|i| i.message.contains(REFLECTS_FIELD) || i.fix.contains("Implements"))
        .collect();
    assert_eq!(hit.len(), 1, "{issues:?}");
    assert_eq!(hit[0].code, "E_FIELD_UNKNOWN");
    assert_eq!(hit[0].severity, Severity::Error);
    let text = format!("{} {}", hit[0].message, hit[0].fix);
    assert!(
        text.contains("reflects justifies content") && text.contains("Implements: trailers"),
        "{text}"
    );
    let table = with_reflects("table", "t");
    assert!(
        !validate_block(&pack(), &table, Some(&section()))
            .iter()
            .any(|i| i.message.contains(REFLECTS_FIELD) || i.fix.contains("Implements")),
    );
}

#[test]
fn refl1_reflects_is_refused_on_every_supporting_type() {
    for (ty, prefix) in [("question", "q"), ("rationale", "ra")] {
        let issues = validate_block(&pack(), &with_reflects(ty, prefix), Some(&section()));
        assert!(
            issues
                .iter()
                .any(|i| i.code == "E_FIELD_UNKNOWN" && i.fix.contains("Implements")),
            "{ty}: {issues:?}"
        );
    }
    for ty in ["decision", "question", "rationale"] {
        assert_eq!(pack().class_of(ty), TypeClass::Supporting, "{ty}");
    }
}

#[test]
fn refl1_reflects_must_be_a_list_of_strings() {
    assert_eq!(
        codes(&claim(FieldValue::Str("src/*.rs".into()))),
        vec!["E_FIELD_KIND"]
    );
    assert_eq!(
        codes(&claim(FieldValue::List(vec![FieldValue::Int(1)]))),
        vec!["E_FIELD_KIND"]
    );
}

#[test]
fn refl1_each_glob_must_compile() {
    let issues = validate_block(
        &pack(),
        &claim(globs(&["src/*.rs", "a//b", "/abs", "x y"])),
        Some(&section()),
    );
    let bad: Vec<_> = issues.iter().filter(|i| i.code == "E_BAD_GLOB").collect();
    assert_eq!(bad.len(), 3, "{issues:?}");
    assert!(bad[0].message.contains("a//b"), "{}", bad[0].message);
    assert!(bad.iter().all(|i| i.severity == Severity::Error));
}

#[test]
fn refl1_at_most_max_reflects_globs() {
    let many: Vec<String> = (0..=MAX_REFLECTS_GLOBS)
        .map(|i| format!("src/f{i}.rs"))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(codes(&claim(globs(&refs))), vec!["E_LIMIT"]);
    assert!(codes(&claim(globs(&refs[..MAX_REFLECTS_GLOBS]))).is_empty());
}

#[test]
fn refl1_a_pack_cannot_define_a_reflects_field() {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            r#"
name: team
version: "1"
extends: rdy-standard
block_types:
  risk:
    prefix: rk
    traits: [normative]
    body: required
    fields:
      reflects: { kind: list }
"#,
        )
        .unwrap();
    let e = set.resolve(&name).unwrap_err();
    assert!(e.to_string().contains("reflects is reserved"), "{e}");
}
