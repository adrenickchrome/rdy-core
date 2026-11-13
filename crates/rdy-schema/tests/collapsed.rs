use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::*;
use std::collections::BTreeMap;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn b(id: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: if ty == "section" { "" } else { "text" }.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn section(fields: &[(&str, FieldValue)]) -> Block {
    let mut s = b("s-SECT1", "section", Some("d-D0C11"));
    s.fields
        .insert("title".into(), FieldValue::Str("Goals".into()));
    for (k, v) in fields {
        s.fields.insert((*k).into(), v.clone());
    }
    s
}

fn doc() -> Block {
    b("d-D0C11", "doc", None)
}

fn codes(issues: &[Issue]) -> Vec<&'static str> {
    issues.iter().map(|i| i.code).collect()
}

#[test]
fn rfr1_collapsed_is_optional_section_content() {
    let p = pack();
    // boolean only, and optional
    let def = &p.block_types["section"].fields["collapsed"];
    assert_eq!(def.kind, FieldKind::Bool);
    assert!(!def.required);
    for v in [true, false] {
        let s = section(&[("collapsed", FieldValue::Bool(v))]);
        let issues = validate_block(&p, &s, Some(&doc()));
        assert!(issues.is_empty(), "collapsed {v}: {issues:?}");
    }
    assert!(validate_block(&p, &section(&[]), Some(&doc())).is_empty());

    // wrong type
    for v in [
        FieldValue::Str("true".into()),
        FieldValue::Int(1),
        FieldValue::List(vec![]),
    ] {
        let s = section(&[("collapsed", v)]);
        assert_eq!(
            codes(&validate_block(&p, &s, Some(&doc()))),
            vec!["E_FIELD_KIND"]
        );
    }

    // wrong block type: only a section has it
    for ty in ["prose", "claim", "note"] {
        let mut o = b("p-AAAAA", ty, Some("s-SECT1"));
        o.fields.insert("collapsed".into(), FieldValue::Bool(true));
        let issues = validate_block(&p, &o, Some(&section(&[])));
        assert_eq!(codes(&issues), vec!["E_FIELD_UNKNOWN"], "{ty}");
        assert!(!issues[0].fix.contains("collapsed"), "{ty}");
    }

    // a typo gets the field list as its hint
    let s = section(&[("colapsed", FieldValue::Bool(true))]);
    let issues = validate_block(&p, &s, Some(&doc()));
    assert_eq!(codes(&issues), vec!["E_FIELD_UNKNOWN"]);
    assert!(issues[0].fix.contains("collapsed"), "{}", issues[0].fix);

    // content: absent leaves the hash alone, and a toggle changes it
    let plain = section(&[]).content_hash();
    assert_eq!(plain, section(&[]).content_hash());
    let on = section(&[("collapsed", FieldValue::Bool(true))]).content_hash();
    let off = section(&[("collapsed", FieldValue::Bool(false))]).content_hash();
    assert_ne!(plain, on);
    assert_ne!(on, off, "toggled");
    assert_ne!(plain, off, "an explicit false is not an absent field");
}
