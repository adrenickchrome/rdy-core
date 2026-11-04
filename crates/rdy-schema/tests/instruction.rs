//! ain1 (D-4W1ZR, D-8X04F): the engine injects the `instruction` type into every pack, and the
//! doc field `required` that makes a doc's instructions a required ruleset.
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::*;
use std::collections::{BTreeMap, BTreeSet};

fn standard() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn resolve(yaml: &str) -> Result<Pack, SchemaError> {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(yaml).unwrap();
    set.resolve(&name)
}

/// A pack that extends the standard pack and adds a type of its own.
fn extending() -> Pack {
    resolve("name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    body: required\n").unwrap()
}

/// A replacement pack with no section type: its doc holds only its own chapter.
fn bare() -> Pack {
    resolve("name: bare\nversion: \"1\"\nblock_types:\n  doc:\n    prefix: d\n    children: [chapter]\n  chapter:\n    prefix: ch\n    traits: [structural]\n    children: [instruction]\ndoc_types:\n  page: { skeleton: [] }\n").unwrap()
}

/// A pack `t` extending the standard pack that declares `ty` as `def` (a YAML flow mapping).
fn declaring(ty: &str, def: &str) -> Result<Pack, SchemaError> {
    resolve(&format!(
        "name: t\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  {ty}: {def}\n"
    ))
}

fn fd(kind: FieldKind, required: bool) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: vec![],
    }
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

fn doc() -> Block {
    let mut d = block("d-D0C11", "doc", None);
    for (k, v) in [("name", "rules"), ("title", "Rules"), ("doc_type", "page")] {
        d.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    d
}

fn codes(issues: &[Issue]) -> Vec<(&'static str, String)> {
    issues.iter().map(|i| (i.code, i.message.clone())).collect()
}

#[test]
fn ain1_instruction_is_a_normative_doc_asset_in_every_pack() {
    let core: BTreeMap<String, FieldDef> = [
        ("applies_to", fd(FieldKind::List, true)),
        ("locked", fd(FieldKind::Bool, false)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for p in [standard(), extending(), bare()] {
        let def = &p.block_types["instruction"];
        assert_eq!(def.prefix, "ai", "{}", p.name);
        assert_eq!(p.type_for_prefix("ai"), Some("instruction"), "{}", p.name);
        assert_eq!(p.class_of("instruction"), TypeClass::DocAsset, "{}", p.name);
        assert_eq!(def.traits, BTreeSet::from([Trait::Normative]), "{}", p.name);
        assert_eq!(def.body, BodyRule::Required, "{}", p.name);
        assert!(def.children.is_empty(), "{}", p.name);
        assert!(!def.fields.contains_key("key"), "{}", p.name);
        for (name, want) in &core {
            assert_eq!(&def.fields[name], want, "{} {name}", p.name);
        }
        // locked is optional: no stored value means false
        assert!(!def.fields["locked"].required);
        // a doc holds an instruction directly, after its own children and before the engine's
        // profile and policy; a section holds one only when the pack has sections
        let kids = &p.block_types["doc"].children;
        let at = |t: &str| kids.iter().position(|c| c == t).unwrap();
        assert_eq!(kids.iter().filter(|c| *c == "instruction").count(), 1);
        assert!(at("instruction") < at("publication_profile"), "{kids:?}");
        if let Some(section) = p.block_types.get("section") {
            assert_eq!(
                section.children.last().map(String::as_str),
                Some("instruction")
            );
        }
        // no other type holds one, whatever its pack lists
        for (name, t) in &p.block_types {
            if name != "doc" && name != "section" {
                assert!(
                    !t.children.iter().any(|c| c == "instruction"),
                    "{} {name}",
                    p.name
                );
            }
        }
    }
    // a bare replacement pack with no section: the doc holds it directly
    assert!(!bare().block_types.contains_key("section"));
    assert_eq!(
        bare().block_types["doc"].children,
        vec![
            "chapter",
            "instruction",
            "publication_profile",
            "authority_policy"
        ]
    );

    // a pack may add optional fields, or repeat the core exactly, and nothing else
    for def in [
        "{ prefix: ai }",
        "{ prefix: ai, traits: [normative], body: required }",
        "{ prefix: ai, fields: { applies_to: { kind: list, required: true }, owner: { kind: str } } }",
    ] {
        let p = declaring("instruction", def).unwrap_or_else(|e| panic!("{def}: {e}"));
        let t = &p.block_types["instruction"];
        assert_eq!(t.body, BodyRule::Required, "{def}");
        assert_eq!(t.traits, BTreeSet::from([Trait::Normative]), "{def}");
        assert_eq!(t.class, Some(TypeClass::DocAsset), "{def}");
    }
    let p = declaring(
        "instruction",
        "{ prefix: ai, fields: { owner: { kind: str } } }",
    )
    .unwrap();
    assert_eq!(
        p.block_types["instruction"]
            .fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["applies_to", "locked", "owner"]
    );
    let redefine = |what: &str| {
        SchemaError::Invalid(format!(
            "pack t cannot redefine instruction ({what}); it may only add optional fields"
        ))
    };
    for (def, what) in [
        ("{ prefix: in }", "prefix"),
        ("{ prefix: ai, children: [prose] }", "children"),
        ("{ prefix: ai, body: forbidden }", "body"),
        ("{ prefix: ai, traits: [informative] }", "traits"),
        ("{ prefix: ai, traits: [normative, defines] }", "traits"),
        (
            "{ prefix: ai, fields: { applies_to: { kind: str, required: true } } }",
            "field applies_to",
        ),
        (
            "{ prefix: ai, fields: { locked: { kind: bool, required: true } } }",
            "field locked",
        ),
        (
            "{ prefix: ai, fields: { owner: { kind: str, required: true } } }",
            "field owner",
        ),
    ] {
        assert_eq!(
            declaring("instruction", def).unwrap_err(),
            redefine(what),
            "{def}"
        );
    }
    // another type cannot take the prefix
    let e = declaring("aside", "{ prefix: ai, body: required }").unwrap_err();
    assert!(
        matches!(&e, SchemaError::Invalid(m) if m.contains("prefix \"ai\" used by both")),
        "{e}"
    );
    // the standard pack file itself is unchanged: it declares no instruction
    assert!(!STANDARD_YAML.contains("instruction"));
}

#[test]
fn ain1_required_is_an_optional_bool_on_docs() {
    for p in [standard(), extending(), bare()] {
        assert_eq!(
            p.block_types["doc"].fields["required"],
            fd(FieldKind::Bool, false),
            "{}",
            p.name
        );
        // it is a doc field only: no instruction, section or other type gets it
        for (name, t) in &p.block_types {
            if name != "doc" {
                assert!(!t.fields.contains_key("required"), "{} {name}", p.name);
            }
        }
        // a doc validates with it, or without it, and only a bool is a value
        let mut d = doc();
        assert_eq!(codes(&validate_block(&p, &d, None)), vec![]);
        d.fields.insert("required".into(), FieldValue::Bool(true));
        assert_eq!(codes(&validate_block(&p, &d, None)), vec![]);
        d.fields
            .insert("required".into(), FieldValue::Str("yes".into()));
        assert_eq!(
            codes(&validate_block(&p, &d, None)),
            vec![(
                "E_FIELD_KIND",
                "field \"required\" must be Bool".to_string()
            )]
        );
        // an instruction that sets it is refused as an unknown field
        let mut i = block("ai-AAAAA", "instruction", Some("d-D0C11"));
        i.body = "Run the tests.".into();
        i.fields.insert(
            "applies_to".into(),
            FieldValue::List(vec![FieldValue::Str("all".into())]),
        );
        i.links.insert(
            "decided_by".into(),
            vec![rdy_model::Link {
                target: BlockId::parse("D-AAAAA").unwrap(),
                part: None,
            }],
        );
        assert_eq!(codes(&validate_block(&p, &i, Some(&doc()))), vec![]);
        i.fields.insert("required".into(), FieldValue::Bool(true));
        assert_eq!(
            codes(&validate_block(&p, &i, Some(&doc()))),
            vec![(
                "E_FIELD_UNKNOWN",
                "type instruction has no field \"required\"".to_string()
            )]
        );
    }
    // a pack's own doc field of that name gives way to the core, as other doc core fields do
    let p = resolve("name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  doc:\n    prefix: d\n    fields:\n      required: { kind: str }\n").unwrap();
    assert_eq!(
        p.block_types["doc"].fields["required"],
        fd(FieldKind::Bool, false)
    );
    // the effective pack serializes in full and reads back with the field and the type
    for p in [standard(), extending(), bare()] {
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        let back: Pack = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back, p, "{}", p.name);
        assert_eq!(
            back.block_types["doc"].fields["required"],
            fd(FieldKind::Bool, false)
        );
        assert_eq!(
            back.block_types["instruction"],
            p.block_types["instruction"]
        );
    }
    // no title convention: a doc titled Required is an ordinary doc
    let mut d = doc();
    d.fields
        .insert("title".into(), FieldValue::Str("Required".into()));
    assert_eq!(codes(&validate_block(&standard(), &d, None)), vec![]);
    assert!(!d.fields.contains_key("required"));
}
