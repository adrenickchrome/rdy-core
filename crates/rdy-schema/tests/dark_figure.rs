//! rfr2 (D-3DAX4 rule 5, D-6PR3Y rule 7): a figure's optional dark image is a pair of fields,
//! `dark_blob` and `dark_media_type`, set together. A light-only figure validates as before.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::*;
use std::collections::BTreeMap;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

const H: &str = "0000000000000000000000000000000000000000000000000000000000000001";

fn section() -> Block {
    let mut s = Block {
        id: BlockId::parse("s-SECT1").unwrap(),
        block_type: "section".into(),
        parent: Some(BlockId::parse("d-D0C11").unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    };
    s.fields
        .insert("title".into(), FieldValue::Str("Goals".into()));
    s
}

fn figure(fields: &[(&str, FieldValue)]) -> Block {
    let mut f = Block {
        id: BlockId::parse("fg-FGA01").unwrap(),
        block_type: "figure".into(),
        parent: Some(BlockId::parse("s-SECT1").unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    };
    for (k, v) in [
        ("blob", FieldValue::Str(H.into())),
        ("media_type", FieldValue::Str("image/png".into())),
        ("alt", FieldValue::Str("A loop".into())),
    ] {
        f.fields.insert(k.into(), v);
    }
    for (k, v) in fields {
        f.fields.insert((*k).into(), v.clone());
    }
    f
}

fn issues(b: &Block) -> Vec<Issue> {
    validate_block(&pack(), b, Some(&section()))
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

#[test]
fn rfr2_dark_fields_are_an_optional_pair() {
    let p = pack();
    // two optional string fields; the light fields stay required
    let fields = &p.block_types["figure"].fields;
    for name in ["dark_blob", "dark_media_type"] {
        assert_eq!(fields[name].kind, FieldKind::Str, "{name}");
        assert!(!fields[name].required, "{name}");
    }
    for name in ["blob", "media_type", "alt"] {
        assert!(fields[name].required, "{name}");
    }
    assert_eq!(ASSET_BLOB_FIELDS, ["blob", "dark_blob"]);
    assert_eq!(ASSET_MEDIA_TYPE_FIELDS, ["media_type", "dark_media_type"]);

    // a light-only figure and a paired one validate
    assert_eq!(issues(&figure(&[])), vec![]);
    let paired = figure(&[("dark_blob", s(H)), ("dark_media_type", s("image/svg+xml"))]);
    assert_eq!(issues(&paired), vec![]);
    // dark_media_type takes the values media_type takes: any string
    for media in ["image/png", "application/pdf", "text/plain"] {
        let f = figure(&[("dark_blob", s(H)), ("dark_media_type", s(media))]);
        assert_eq!(issues(&f), vec![], "{media}");
    }

    // one field alone: the exact D-6PR3Y message and fix, once
    for (one, v) in [("dark_blob", s(H)), ("dark_media_type", s("image/png"))] {
        let got = issues(&figure(&[(one, v)]));
        assert_eq!(got.len(), 1, "{one}: {got:?}");
        assert_eq!(got[0].code, "E_VALIDATION", "{one}");
        assert_eq!(got[0].severity, Severity::Error, "{one}");
        assert_eq!(
            got[0].message, "fg-FGA01's dark_blob and dark_media_type are set together",
            "{one}"
        );
        assert_eq!(got[0].fix, "set both fields, or neither", "{one}");
        assert_eq!(got[0].block, Some(BlockId::parse("fg-FGA01").unwrap()));
    }

    // types: each is a string
    for (name, v) in [
        ("dark_blob", FieldValue::Int(1)),
        ("dark_media_type", FieldValue::Bool(true)),
        ("dark_blob", FieldValue::List(vec![])),
    ] {
        let other = if name == "dark_blob" {
            ("dark_media_type", s("image/png"))
        } else {
            ("dark_blob", s(H))
        };
        let got = issues(&figure(&[(name, v), other]));
        let codes: Vec<&str> = got.iter().map(|i| i.code).collect();
        assert_eq!(codes, vec!["E_FIELD_KIND"], "{name}: {got:?}");
    }

    // unknown keys: a close typo gets the unknown-field refusal, which lists the dark fields
    let got = issues(&figure(&[("dark_blobs", s(H))]));
    let codes: Vec<&str> = got.iter().map(|i| i.code).collect();
    assert_eq!(codes, vec!["E_FIELD_UNKNOWN"], "{got:?}");
    assert!(got[0].fix.contains("dark_blob"), "{}", got[0].fix);

    // only a figure has them
    for ty in ["prose", "note"] {
        let mut o = figure(&[]);
        o.block_type = ty.into();
        o.id = BlockId::parse(if ty == "prose" { "p-AAAAA" } else { "n-AAAAA" }).unwrap();
        o.fields = BTreeMap::from([("dark_blob".to_string(), s(H))]);
        o.body = "text".into();
        let got = issues(&o);
        let codes: Vec<&str> = got.iter().map(|i| i.code).collect();
        assert_eq!(codes, vec!["E_FIELD_UNKNOWN"], "{ty}: {got:?}");
    }

    // unchanged light-only behaviour: a missing light field is still the required-field
    // refusal, with no pair issue
    let mut no_blob = figure(&[]);
    no_blob.fields.remove("blob");
    let got = issues(&no_blob);
    let codes: Vec<&str> = got.iter().map(|i| i.code).collect();
    assert_eq!(codes, vec!["E_FIELD_REQUIRED"], "{got:?}");

    // the read schema of a stored normal block keeps the pair rule
    let half = figure(&[("dark_blob", s(H))]);
    assert_eq!(
        validate_read_block(&p, &half, Some(&section()))
            .iter()
            .map(|i| i.code)
            .collect::<Vec<_>>(),
        vec!["E_VALIDATION"]
    );
}
