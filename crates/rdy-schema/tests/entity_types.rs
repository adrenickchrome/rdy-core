//! ent1: rdy-standard's entity types (FW-EN2): value and entity, aliases on every type that
//! defines something, and docs and sections may hold both.
use rdy_schema::*;

#[test]
fn ent1_value_and_entity_are_defining_types() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let v = &pack.block_types["value"];
    assert_eq!(v.prefix, "v");
    assert!(v.traits.contains(&Trait::Defines) && v.traits.contains(&Trait::Normative));
    assert!(v.fields["name"].required && v.fields["value"].required);
    assert_eq!(
        (v.fields["value"].kind, v.fields["unit"].kind),
        (FieldKind::Str, FieldKind::Str)
    );
    assert!(!v.fields["unit"].required);
    let e = &pack.block_types["entity"];
    assert_eq!(e.prefix, "en");
    assert_eq!(
        e.traits.iter().copied().collect::<Vec<_>>(),
        vec![Trait::Defines]
    );
    assert!(e.fields["name"].required);
    assert_eq!(
        (e.fields["kind"].kind, e.fields["kind"].required),
        (FieldKind::Str, false)
    );
}

#[test]
fn ent1_every_type_that_defines_something_takes_aliases() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let defining: Vec<&str> = pack
        .block_types
        .iter()
        .filter(|(_, t)| t.traits.contains(&Trait::Defines))
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(
        defining,
        vec!["actor", "component", "entity", "interface", "term", "value"]
    );
    for t in defining {
        let a = &pack.block_types[t].fields["aliases"];
        assert_eq!((a.kind, a.required), (FieldKind::List, false), "{t}");
    }
}

#[test]
fn ent1_docs_and_sections_may_hold_values_and_entities() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    for parent in ["doc", "section"] {
        let c = &pack.block_types[parent].children;
        assert!(
            c.iter().any(|x| x == "value") && c.iter().any(|x| x == "entity"),
            "{parent}: {c:?}"
        );
    }
}

// ---- nb3: the optional `everyday` field on every Defines type (D-YXBXH) ----

mod nb3 {
    use rdy_model::{Block, BlockId, FieldValue};
    use rdy_schema::*;
    use std::collections::BTreeMap;

    /// A pack with one custom Defines type, `gadget`.
    fn pack_with_custom() -> Pack {
        let mut set = PackSet::with_builtin();
        set.add_yaml(
            "name: p\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  gadget:\n    prefix: gg\n    traits: [defines]\n    fields:\n      name: { kind: str, required: true }\n",
        )
        .unwrap();
        set.resolve("p").unwrap()
    }

    fn try_pack(yaml_types: &str) -> Result<Pack, SchemaError> {
        let mut set = PackSet::with_builtin();
        set.add_yaml(&format!(
            "name: p\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n{yaml_types}"
        ))?;
        set.resolve("p")
    }

    fn block(ty: &str, prefix: &str, fields: &[(&str, FieldValue)]) -> Block {
        let mut f: BTreeMap<String, FieldValue> = BTreeMap::new();
        f.insert("name".into(), FieldValue::Str("Thing".into()));
        if ty == "value" {
            f.insert("value".into(), FieldValue::Str("3".into()));
        }
        for (k, v) in fields {
            f.insert((*k).to_string(), v.clone());
        }
        Block {
            id: BlockId::parse(&format!("{prefix}-AAAAA")).unwrap(),
            block_type: ty.into(),
            parent: Some(BlockId::parse("s-SECT1").unwrap()),
            order: "V".into(),
            fields: f,
            body: "Words.".into(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        }
    }

    /// The field issues a block draws: its parent and body rules are not under test here.
    fn field_codes(pack: &Pack, b: &Block) -> Vec<&'static str> {
        validate_block(pack, b, None)
            .iter()
            .map(|i| i.code)
            .filter(|c| c.starts_with("E_FIELD"))
            .collect()
    }

    #[test]
    fn nb3_every_defines_type_accepts_optional_everyday() {
        let pack = pack_with_custom();
        let defining: Vec<(String, String)> = pack
            .block_types
            .iter()
            .filter(|(_, t)| t.traits.contains(&Trait::Defines))
            .map(|(n, t)| (n.clone(), t.prefix.clone()))
            .collect();
        let names: Vec<&str> = defining.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "actor",
                "component",
                "entity",
                "gadget",
                "interface",
                "term",
                "value"
            ]
        );
        for (name, prefix) in &defining {
            let f = &pack.block_types[name].fields["everyday"];
            assert_eq!(
                (f.kind, f.required, f.values.is_empty()),
                (FieldKind::Bool, false, true),
                "{name}"
            );
            // absent, false and true all pass; no default is stored
            let absent = block(name, prefix, &[]);
            assert!(field_codes(&pack, &absent).is_empty(), "{name} absent");
            assert!(!absent.fields.contains_key("everyday"), "{name}");
            for v in [false, true] {
                let b = block(name, prefix, &[("everyday", FieldValue::Bool(v))]);
                assert!(field_codes(&pack, &b).is_empty(), "{name} {v}");
                assert_eq!(b.fields["everyday"], FieldValue::Bool(v));
            }
        }
        // a type that does not define anything gets no field
        for name in ["claim", "section", "doc", "decision"] {
            assert!(
                !pack.block_types[name].fields.contains_key("everyday"),
                "{name}"
            );
        }
    }

    #[test]
    fn nb3_everyday_uses_the_existing_field_validator() {
        let pack = pack_with_custom();
        // a wrong type is refused by the generic kind check
        let wrong = block("term", "t", &[("everyday", FieldValue::Str("yes".into()))]);
        assert_eq!(field_codes(&pack, &wrong), vec!["E_FIELD_KIND"]);
        // a typo is refused as an unknown field, and the fix lists everyday
        let typo = block("term", "t", &[("everydy", FieldValue::Bool(true))]);
        let issues = validate_block(&pack, &typo, None);
        let unknown: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.code == "E_FIELD_UNKNOWN")
            .collect();
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].fix.contains("everyday"), "{}", unknown[0].fix);
        // a non-Defines type refuses it
        let claim = block("claim", "c", &[("everyday", FieldValue::Bool(true))]);
        assert!(field_codes(&pack, &claim).contains(&"E_FIELD_UNKNOWN"));
        // a field change alters the content hash
        let none = block("term", "t", &[]);
        let off = block("term", "t", &[("everyday", FieldValue::Bool(false))]);
        let on = block("term", "t", &[("everyday", FieldValue::Bool(true))]);
        let hashes = [none.content_hash(), off.content_hash(), on.content_hash()];
        assert!(hashes[0] != hashes[1] && hashes[1] != hashes[2] && hashes[0] != hashes[2]);
        // an identical predeclared optional bool is accepted
        let ok = try_pack(
            "  gadget:\n    prefix: gg\n    traits: [defines]\n    fields:\n      name: { kind: str, required: true }\n      everyday: { kind: bool }\n",
        )
        .unwrap();
        let f = &ok.block_types["gadget"].fields["everyday"];
        assert_eq!((f.kind, f.required), (FieldKind::Bool, false));
        // a conflicting declaration is refused with the exact text
        for field in [
            "everyday: { kind: str }",
            "everyday: { kind: bool, required: true }",
            "everyday: { kind: enum, values: [a] }",
        ] {
            let bad = try_pack(&format!(
                "  gadget:\n    prefix: gg\n    traits: [defines]\n    fields:\n      name: {{ kind: str, required: true }}\n      {field}\n"
            ));
            match bad {
                Err(SchemaError::Invalid(m)) => assert_eq!(
                    m, "type gadget: field everyday must be an optional bool",
                    "{field}"
                ),
                other => panic!("{field}: expected SchemaError::Invalid, got {other:?}"),
            }
        }
        // a non-Defines type may declare an unrelated everyday field
        let other = try_pack(
            "  note2:\n    prefix: nn\n    traits: [informative]\n    fields:\n      everyday: { kind: str }\n",
        )
        .unwrap();
        assert_eq!(
            other.block_types["note2"].fields["everyday"].kind,
            FieldKind::Str
        );
    }
}
