//! rdx3: a removed block's read projection passes the read schema's structural checks, while a
//! normal incomplete block, and the same stub on a write path, still fails (D-3M81W).
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash};
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
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn stub(id: &str, ty: &str, parent: Option<&str>, frozen: bool) -> Block {
    Block::redacted(
        BlockId::parse(id).unwrap(),
        ty.into(),
        parent.map(|p| BlockId::parse(p).unwrap()),
        "V".into(),
        RedactedBlockIdentity {
            version: Hash::of(format!("version of {id}").as_bytes()),
            content_hash: Hash::of(format!("content of {id}").as_bytes()),
            frozen,
            request: Hash::of(b"the request"),
        },
    )
}

fn doc() -> Block {
    let mut d = b("d-D0C11", "doc", None);
    for (k, v) in [("name", "plan"), ("title", "Plan"), ("doc_type", "page")] {
        d.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    d
}

fn section() -> Block {
    let mut s = b("s-SECT1", "section", Some("d-D0C11"));
    s.fields
        .insert("title".into(), FieldValue::Str("Goals".into()));
    s
}

fn folder() -> Block {
    let mut f = b("fd-F0DR1", "folder", None);
    f.fields
        .insert("name".into(), FieldValue::Str("notes".into()));
    f
}

fn codes(issues: &[Issue]) -> Vec<&'static str> {
    let mut c: Vec<&'static str> = issues.iter().map(|i| i.code).collect();
    c.sort();
    c
}

#[test]
fn rdx3_stubs_pass_read_schema_but_incomplete_writes_fail() {
    let p = pack();
    let (doc, section, folder) = (doc(), section(), folder());

    // each stub lacks its required fields and has the notice as its body (a doc, a folder and a
    // figure forbid a body; a section, a decision and a value require fields): the read schema
    // skips only those unavailable checks
    let cases: Vec<(Block, Option<&Block>)> = vec![
        (
            stub("d-D0C22", "doc", Some("fd-F0DR1"), false),
            Some(&folder),
        ),
        (stub("fd-F0DR2", "folder", None, false), None),
        (
            stub("s-SECT2", "section", Some("d-D0C11"), false),
            Some(&doc),
        ),
        (
            stub("D-DEC11", "decision", Some("s-SECT1"), true),
            Some(&section),
        ),
        (
            stub("v-VA111", "value", Some("s-SECT1"), false),
            Some(&section),
        ),
        (
            stub("fg-F1G11", "figure", Some("s-SECT1"), false),
            Some(&section),
        ),
        (
            stub("p-PR0S1", "prose", Some("s-SECT1"), false),
            Some(&section),
        ),
    ];
    for (s, parent) in &cases {
        assert_eq!(s.body, CONTENT_REMOVED);
        let read = validate_read_block(&p, s, *parent);
        assert!(read.is_empty(), "{} {}: {read:?}", s.block_type, s.id);
        assert!(validate_prefix(&p, s).is_none(), "{}", s.id);
        // the write schema ignores the marker: a stub never passes as a written block
        let write = validate_block(&p, s, *parent);
        if s.block_type != "prose" {
            assert!(has_errors(&write), "{} passed the write schema", s.id);
        }
        for i in &write {
            assert!(
                ["E_FIELD_REQUIRED", "E_BODY_REQUIRED", "E_BODY_FORBIDDEN"].contains(&i.code),
                "{}: {i:?}",
                s.id
            );
        }
    }
    assert_eq!(
        codes(&validate_block(&p, &cases[2].0, Some(&doc))),
        ["E_FIELD_REQUIRED"],
        "a section stub on a write path misses its title"
    );
    assert_eq!(
        codes(&validate_block(&p, &cases[0].0, Some(&folder))),
        [
            "E_BODY_FORBIDDEN",
            "E_FIELD_REQUIRED",
            "E_FIELD_REQUIRED",
            "E_FIELD_REQUIRED"
        ],
        "a doc stub on a write path misses its name, title and doc type, and has a body"
    );

    // the exact structural checks remain for a stub
    let unknown = stub("p-PR0S2", "no_such_type", Some("s-SECT1"), false);
    assert_eq!(
        codes(&validate_read_block(&p, &unknown, Some(&section))),
        ["E_UNKNOWN_TYPE"]
    );
    let wrong_prefix = stub("D-DEC22", "prose", Some("s-SECT1"), false);
    assert_eq!(
        validate_prefix(&p, &wrong_prefix).map(|i| i.code),
        Some("E_PREFIX")
    );
    let misplaced = stub("p-PR0S3", "prose", Some("fd-F0DR1"), false);
    assert_eq!(
        codes(&validate_read_block(&p, &misplaced, Some(&folder))),
        ["E_CHILD_TYPE"]
    );
    let orphan = stub("p-PR0S4", "prose", None, false);
    assert_eq!(codes(&validate_read_block(&p, &orphan, None)), ["E_ORPHAN"]);
    let lost_parent = stub("p-PR0S5", "prose", Some("s-G0NE1"), false);
    assert_eq!(
        codes(&validate_read_block(&p, &lost_parent, None)),
        ["E_MISSING_PARENT"]
    );
    let mut bad_order = stub("p-PR0S6", "prose", Some("s-SECT1"), false);
    bad_order.order = String::new();
    assert_eq!(
        codes(&validate_read_block(&p, &bad_order, Some(&section))),
        ["E_ORDER_KEY"]
    );

    // a normal incomplete block still fails the read schema: only a projection skips a check
    let mut titleless = section.clone();
    titleless.fields.clear();
    assert_eq!(
        codes(&validate_read_block(&p, &titleless, Some(&doc))),
        ["E_FIELD_REQUIRED"]
    );
    let mut empty_prose = b("p-PR0S7", "prose", Some("s-SECT1"));
    empty_prose.body = String::new();
    assert_eq!(
        codes(&validate_read_block(&p, &empty_prose, Some(&section))),
        ["E_BODY_REQUIRED"]
    );
    let mut doc_with_body = doc.clone();
    doc_with_body.body = CONTENT_REMOVED.into();
    assert_eq!(
        codes(&validate_read_block(&p, &doc_with_body, None)),
        ["E_BODY_FORBIDDEN"],
        "the notice text alone is no marker"
    );
    // a normal block reads exactly as the write schema checks it
    for blk in [&doc, &section, &titleless, &empty_prose, &doc_with_body] {
        let parent = match blk.block_type.as_str() {
            "section" => Some(&doc),
            "prose" => Some(&section),
            _ => None,
        };
        assert_eq!(
            validate_read_block(&p, blk, parent),
            validate_block(&p, blk, parent),
            "{}",
            blk.id
        );
    }
}

#[test]
fn rdx3_a_stub_named_by_its_id_passes_only_the_read_schema() {
    let p = pack();
    let folder = folder();
    let named = |mut s: Block, name: &str| {
        s.fields.insert("name".into(), FieldValue::Str(name.into()));
        s
    };
    // a working copy names a removed doc's file and a removed folder's directory by the id
    for (s, parent) in [
        (
            named(stub("d-D0C22", "doc", Some("fd-F0DR1"), false), "d-D0C22"),
            Some(&folder),
        ),
        (
            named(stub("fd-F0DR2", "folder", None, false), "fd-F0DR2"),
            None,
        ),
    ] {
        let read = validate_read_block(&p, &s, parent);
        assert!(read.is_empty(), "{}: {read:?}", s.id);
        // the write schema keeps the name rule
        assert!(
            validate_block(&p, &s, parent)
                .iter()
                .any(|i| i.code == "E_BAD_NAME"),
            "{}",
            s.id
        );
    }
    // only the stub's own id is that neutral name
    let other = named(stub("d-D0C33", "doc", None, false), "d-D0C44");
    assert_eq!(
        codes(&validate_read_block(&p, &other, None)),
        ["E_BAD_NAME"]
    );
    // a normal doc named by its id keeps the name rule on the read schema too
    let mut written = doc();
    written
        .fields
        .insert("name".into(), FieldValue::Str("d-D0C11".into()));
    assert_eq!(
        codes(&validate_read_block(&p, &written, None)),
        ["E_BAD_NAME"]
    );
}
