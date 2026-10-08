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
