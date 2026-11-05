//! ab1 (D-WJ0SJ): the as-built fields every pack carries, and the version rules of their values.
use rdy_model::decision::RULING_FIELDS;
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::as_built::{self, Built};
use rdy_schema::publication::PROFILE_TYPE;
use rdy_schema::*;
use std::cmp::Ordering;
use std::collections::BTreeMap;

fn pack_of(yaml: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(yaml).unwrap();
    set.resolve(&name)
}

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn bare() -> Pack {
    pack_of("name: bare\nversion: \"1\"\n").unwrap()
}

/// An extending pack with its own content type, its own supporting type and its own tracking.
fn extending() -> Pack {
    pack_of(
        "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    body: required\n  ledger:\n    prefix: lg\n    class: supporting\n    body: required\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      owner: { kind: str }\n    tracking: [owner]\n",
    )
    .unwrap()
}

/// A pack with no parent that declares its own content types, one of them with `planned` and
/// `since` written out exactly as the engine would add them.
fn custom() -> Pack {
    pack_of(
        "name: custom\nversion: \"1\"\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    body: required\n    fields:\n      planned: { kind: bool }\n      since: { kind: str }\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      built: { kind: str }\n",
    )
    .unwrap()
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

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn section() -> Block {
    let mut x = b("s-SECT1", "section", Some("d-D0C11"));
    x.fields.insert("title".into(), s("Goals"));
    x
}

fn note_with(field: &str, v: FieldValue) -> Block {
    let mut n = b("n-N0TE1", "note", Some("s-SECT1"));
    n.body = "text".into();
    n.fields.insert(field.into(), v);
    n
}

fn decision_with(field: &str, v: FieldValue) -> Block {
    let mut d = b("D-DEC11", "decision", None);
    d.fields.insert("statement".into(), s("Ship it."));
    d.fields.insert("status".into(), s("proposed"));
    d.fields.insert(field.into(), v);
    d
}

fn rows(issues: &[Issue]) -> Vec<(&'static str, String, String)> {
    issues
        .iter()
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

fn row(code: &'static str, message: &str, fix: &str) -> (&'static str, String, String) {
    (code, message.to_string(), fix.to_string())
}

fn optional(kind: FieldKind) -> FieldDef {
    FieldDef {
        kind,
        required: false,
        values: vec![],
    }
}

#[test]
fn ab1_fields_are_pack_independent_and_profile_keys_are_distinct() {
    for pack in [bare(), standard(), extending(), custom()] {
        let name = &pack.name;
        // the decision: built is an optional string and a tracking field, nothing else changed
        let decision = &pack.block_types["decision"];
        assert_eq!(decision.fields["built"], optional(FieldKind::Str), "{name}");
        assert!(decision.tracking.contains("built"), "{name}");
        assert!(pack.tracking().is_tracking("decision", "built"), "{name}");
        assert!(!decision.fields.contains_key("planned"), "{name}");
        assert!(!decision.fields.contains_key("since"), "{name}");
        assert!(!RULING_FIELDS.contains(&"built"), "{name}");
        assert_eq!(RULING_FIELDS, ["statement", "options", "status"]);
        // every content type has planned (bool) and since (str); supporting types have neither
        for (ty, def) in &pack.block_types {
            let content =
                pack.class_of(ty) == TypeClass::DocAsset && !rdy_model::tree::is_tree_type(ty);
            if content {
                assert_eq!(
                    def.fields["planned"],
                    optional(FieldKind::Bool),
                    "{name} {ty}"
                );
                assert_eq!(def.fields["since"], optional(FieldKind::Str), "{name} {ty}");
            } else if ty != "doc" && ty != PROFILE_TYPE {
                assert!(!def.fields.contains_key("planned"), "{name} {ty}");
                assert!(!def.fields.contains_key("since"), "{name} {ty}");
            }
            if ty != "decision" {
                assert!(!def.tracking.contains("built"), "{name} {ty}");
            }
        }
        // a doc covers the blocks below it with planned; since does not inherit
        assert_eq!(
            pack.block_types["doc"].fields["planned"],
            optional(FieldKind::Bool),
            "{name}"
        );
        assert!(
            !pack.block_types["doc"].fields.contains_key("since"),
            "{name}"
        );
        assert!(
            !pack.block_types["folder"].fields.contains_key("planned"),
            "{name}"
        );
        // the profile keeps its own enum keys, apart from the content overrides
        let profile = &pack.block_types[PROFILE_TYPE];
        assert_eq!(profile.fields["planned"].kind, FieldKind::Enum, "{name}");
        assert_eq!(profile.fields["planned"].values, ["hide", "mark"], "{name}");
        assert_eq!(profile.fields["since"].kind, FieldKind::Enum, "{name}");
        assert_eq!(profile.fields["since"].values, ["show", "off"], "{name}");
    }
    // the standard pack's note no longer declares planned itself, and still takes it
    assert_eq!(
        standard().block_types["note"].fields["planned"],
        optional(FieldKind::Bool)
    );
    assert!(!STANDARD_YAML.contains("planned"));
    // a child pack's tracking adds to built
    let team = extending();
    let tracking: Vec<&str> = team.block_types["decision"]
        .tracking
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(tracking, ["built", "owner"]);

    // a content planned value is a bool; a profile's hide or mark is no override
    let std = standard();
    assert_eq!(
        validate_block(
            &std,
            &note_with("planned", FieldValue::Bool(true)),
            Some(&section())
        ),
        vec![]
    );
    assert_eq!(
        validate_block(
            &std,
            &note_with("planned", FieldValue::Bool(false)),
            Some(&section())
        ),
        vec![]
    );
    assert_eq!(
        rows(&validate_block(
            &std,
            &note_with("planned", s("hide")),
            Some(&section())
        )),
        vec![row(
            "E_FIELD_KIND",
            "field \"planned\" must be Bool",
            "write planned as a bool value"
        )]
    );

    // unknown keys and close misspellings are refused with the type's field list
    let list = |ty: &str| {
        let mut names: Vec<String> = std.block_types[ty].fields.keys().cloned().collect();
        names.sort();
        names.join(", ")
    };
    for (block, name) in [
        (note_with("sinse", s("1.0.0")), "sinse"),
        (note_with("plannd", FieldValue::Bool(true)), "plannd"),
    ] {
        assert_eq!(
            rows(&validate_block(&std, &block, Some(&section()))),
            vec![row(
                "E_FIELD_UNKNOWN",
                &format!("type note has no field {name:?}"),
                &format!("remove it, or use one of: {}", list("note"))
            )]
        );
    }
    assert_eq!(
        rows(&validate_block(
            &std,
            &decision_with("bilt", s("planned")),
            None
        )),
        vec![row(
            "E_FIELD_UNKNOWN",
            "type decision has no field \"bilt\"",
            &format!("remove it, or use one of: {}", list("decision"))
        )]
    );
    // a decision takes no content override, and a note takes no built
    assert_eq!(
        validate_block(
            &std,
            &decision_with("planned", FieldValue::Bool(true)),
            None
        )
        .len(),
        1
    );
    assert_eq!(
        validate_block(&std, &note_with("built", s("planned")), Some(&section())).len(),
        1
    );
}

#[test]
fn ab1_a_predeclared_field_must_match_exactly() {
    let content = |fields: &str| {
        pack_of(&format!(
            "name: p\nversion: \"1\"\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    fields:\n{fields}"
        ))
    };
    let decision = |fields: &str| {
        pack_of(&format!(
            "name: p\nversion: \"1\"\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n{fields}"
        ))
    };
    let invalid = |m: &str| Err(SchemaError::Invalid(m.to_string()));
    // identical optional declarations are kept
    assert!(content("      planned: { kind: bool }\n      since: { kind: str }\n").is_ok());
    assert!(decision("      built: { kind: str }\n").is_ok());
    // incompatible kinds, required fields and enum constraints are refused, nothing injected
    for (y, why) in [
        (
            "      planned: { kind: str }\n",
            "type memo: field planned must be an optional bool",
        ),
        (
            "      planned: { kind: bool, required: true }\n",
            "type memo: field planned must be an optional bool",
        ),
        (
            "      planned: { kind: enum, values: [hide, mark] }\n",
            "type memo: field planned must be an optional bool",
        ),
        (
            "      since: { kind: int }\n",
            "type memo: field since must be an optional string",
        ),
        (
            "      since: { kind: str, required: true }\n",
            "type memo: field since must be an optional string",
        ),
        (
            "      since: { kind: enum, values: [show, off] }\n",
            "type memo: field since must be an optional string",
        ),
    ] {
        assert_eq!(content(y).map(|_| ()), invalid(why), "{y}");
    }
    for (y, why) in [
        (
            "      built: { kind: bool }\n",
            "type decision: field built must be an optional string",
        ),
        (
            "      built: { kind: str, required: true }\n",
            "type decision: field built must be an optional string",
        ),
        (
            "      built: { kind: enum, values: [planned, none] }\n",
            "type decision: field built must be an optional string",
        ),
    ] {
        assert_eq!(decision(y).map(|_| ()), invalid(why), "{y}");
    }
    // a pack that marks built as tracking itself is no problem either
    let own = pack_of(
        "name: p\nversion: \"1\"\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      built: { kind: str }\n    tracking: [built]\n",
    )
    .unwrap();
    assert_eq!(own.block_types["decision"].tracking.len(), 1);
}

#[test]
fn ab1_versions_use_existing_order_and_exact_errors() {
    // order is numeric, with prerelease before final, and the leading v is optional
    let lt =
        |a: &str, b: &str| assert_eq!(as_built::compare(a, b), Some(Ordering::Less), "{a} {b}");
    lt("1.9.0", "1.10.0");
    lt("1.0.0-rc.1", "1.0.0");
    lt("1.0.0-rc.2", "1.0.0-rc.10");
    lt("v0.9.0", "0.10.0");
    assert_eq!(as_built::compare("v1.2.0", "1.2.0"), Some(Ordering::Equal));
    assert_eq!(as_built::normalize("v1.2.0"), Some("1.2.0".to_string()));
    assert_eq!(
        as_built::normalize("1.2.0-rc.1"),
        Some("1.2.0-rc.1".to_string())
    );
    // no metadata, no short forms, one v at most
    for bad in [
        "",
        "1.2",
        "1",
        "v",
        "vv1.2.0",
        "V1.2.0",
        " 1.2.0",
        "1.2.0 ",
        "1.2.0+build.5",
        "latest",
    ] {
        assert_eq!(as_built::normalize(bad), None, "{bad:?}");
        assert_eq!(as_built::compare(bad, "1.0.0"), None, "{bad:?}");
    }
    assert_eq!(
        as_built::parse_release("1.2.0+b").unwrap_err(),
        "build metadata (+…) is not allowed: it doesn't take part in version order"
    );
    // built reads planned, none or a version
    assert_eq!(as_built::parse_built("planned"), Some(Built::Planned));
    assert_eq!(as_built::parse_built("none"), Some(Built::None));
    assert!(matches!(
        as_built::parse_built("v0.35.0"),
        Some(Built::Version(_))
    ));
    assert_eq!(as_built::parse_built("Planned"), None);
    assert_eq!(as_built::parse_built(""), None);

    let std = standard();
    let built = |v: FieldValue| rows(&validate_block(&std, &decision_with("built", v), None));
    let since = |v: FieldValue| {
        rows(&validate_block(
            &std,
            &note_with("since", v),
            Some(&section()),
        ))
    };
    for ok in ["planned", "none", "0.35.0", "v0.35.0", "1.0.0-rc.1"] {
        assert_eq!(built(s(ok)), vec![], "{ok}");
    }
    for ok in ["0.35.0", "v0.35.0", "1.0.0-rc.1"] {
        assert_eq!(since(s(ok)), vec![], "{ok}");
    }
    let built_bad = row(
        "E_BAD_INPUT",
        "Field \"built\" must be planned, none or a release version.",
        "Write built as planned, none, or a version such as 1.2.0.",
    );
    let since_bad = row(
        "E_BAD_INPUT",
        "Field \"since\" must be a release version.",
        "Write since as a version such as 1.2.0 or v1.2.0-rc.1.",
    );
    for bad in ["", "1.2", "1.2.0+b", "soon", "Planned"] {
        assert_eq!(built(s(bad)), vec![built_bad.clone()], "{bad:?}");
        assert_eq!(since(s(bad)), vec![since_bad.clone()], "{bad:?}");
    }
    // since never takes planned or none
    assert_eq!(since(s("planned")), vec![since_bad.clone()]);
    assert_eq!(since(s("none")), vec![since_bad.clone()]);
    // a wrong kind is the generic kind issue alone
    assert_eq!(
        built(FieldValue::Bool(true)),
        vec![row(
            "E_FIELD_KIND",
            "field \"built\" must be Str",
            "write built as a str value"
        )]
    );
    assert_eq!(
        since(FieldValue::Int(1)),
        vec![row(
            "E_FIELD_KIND",
            "field \"since\" must be Str",
            "write since as a str value"
        )]
    );
    // 255, 256 and 257 bytes: the bound is 256 bytes of spelling, v included
    let spelled = |n: usize| format!("1.0.0-{}", "a".repeat(n - 6));
    for n in [255, 256] {
        let v = spelled(n);
        assert_eq!(v.len(), n);
        assert_eq!(built(s(&v)), vec![], "{n}");
        assert_eq!(since(s(&v)), vec![], "{n}");
        let with_v = format!("v{}", spelled(n - 1));
        assert_eq!(with_v.len(), n);
        assert_eq!(since(s(&with_v)), vec![], "v {n}");
    }
    let long = spelled(257);
    assert_eq!(long.len(), 257);
    assert_eq!(
        built(s(&long)),
        vec![row(
            "E_LIMIT",
            "Field \"built\" exceeds its limit.",
            "Use at most 256 bytes in \"built\"."
        )]
    );
    assert_eq!(
        since(s(&long)),
        vec![row(
            "E_LIMIT",
            "Field \"since\" exceeds its limit.",
            "Use at most 256 bytes in \"since\"."
        )]
    );
    // both overrides together are two valid inputs, never a mutual refusal
    let mut both = note_with("since", s("1.0.0"));
    both.fields.insert("planned".into(), FieldValue::Bool(true));
    assert_eq!(validate_block(&std, &both, Some(&section())), vec![]);
    // a redacted projection skips the value checks (its fields are gone)
    let mut gone = decision_with("built", s("soon"));
    gone.read_identity = None;
    assert_eq!(validate_read_block(&std, &gone, None).len(), 1);
}

#[test]
fn ab1_a_new_decision_gets_built_from_needs_content() {
    let mut planned = b("D-DEC22", "decision", None);
    as_built::default_built(&mut planned);
    assert_eq!(planned.field_str("built"), Some("planned"));
    let mut none = b("D-DEC33", "decision", None);
    none.fields
        .insert("needs_content".into(), FieldValue::Bool(false));
    as_built::default_built(&mut none);
    assert_eq!(none.field_str("built"), Some("none"));
    let mut needs = b("D-DEC44", "decision", None);
    needs
        .fields
        .insert("needs_content".into(), FieldValue::Bool(true));
    as_built::default_built(&mut needs);
    assert_eq!(needs.field_str("built"), Some("planned"));
    let mut explicit = b("D-DEC55", "decision", None);
    explicit
        .fields
        .insert("needs_content".into(), FieldValue::Bool(false));
    explicit.fields.insert("built".into(), s("0.35.0"));
    as_built::default_built(&mut explicit);
    assert_eq!(explicit.field_str("built"), Some("0.35.0"));
    let mut note = b("n-N0TE2", "note", None);
    as_built::default_built(&mut note);
    assert!(!note.fields.contains_key("built"));
}
