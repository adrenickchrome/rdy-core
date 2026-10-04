use rdy_schema::*;

#[test]
fn standard_pack_resolves_with_engine_types() {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    assert_eq!(pack.name, "rdy-standard");
    let d = &pack.block_types["decision"];
    assert_eq!(d.prefix, "D");
    assert!(d.fields["statement"].required);
    assert_eq!(d.fields["status"].values, vec!["proposed", "accepted", "rejected"]);
    assert!(pack.block_types.contains_key("question"));
    for l in ["supersedes", "refines", "depends_on", "conditional_on", "revokes", "answers", "describes"] {
        assert!(pack.link_types.contains(l), "{l}");
    }
    assert_eq!(pack.type_for_prefix("t"), Some("term"));
    assert!(pack.block_types["claim"].traits.contains(&Trait::Normative));
}

#[test]
fn extending_pack_merges_fields_and_adds_types() {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(r#"
name: team
version: "1"
extends: rdy-standard
block_types:
  term:
    prefix: t
    traits: [defines]
    body: required
    fields:
      aliases: { kind: list }
  risk:
    prefix: rk
    traits: [normative]
    body: required
"#).unwrap();
    let pack = set.resolve(&name).unwrap();
    let term = &pack.block_types["term"];
    assert!(term.fields.contains_key("name"), "parent field kept");
    assert!(term.fields.contains_key("aliases"), "child field added");
    assert!(pack.block_types.contains_key("risk"));
    assert!(pack.doc_types.contains_key("overview"));
}

#[test]
fn pack_cannot_redefine_core_decision_fields() {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(r#"
name: sneaky
version: "1"
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      status: { kind: str }
      mood: { kind: str }
"#).unwrap();
    let pack = set.resolve(&name).unwrap();
    let d = &pack.block_types["decision"];
    assert_eq!(d.fields["status"].kind, FieldKind::Enum, "core field forced back");
    assert!(d.fields.contains_key("mood"), "extra field allowed");
}

#[test]
fn replacement_pack_without_extends_has_no_standard_types() {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml("name: tiny\nversion: \"1\"\nblock_types:\n  doc: { prefix: d, traits: [structural], children: [] }\n").unwrap();
    let pack = set.resolve(&name).unwrap();
    assert!(!pack.block_types.contains_key("claim"));
    assert!(pack.block_types.contains_key("decision"));
}

#[test]
fn errors_are_specific() {
    let mut set = PackSet::with_builtin();
    assert!(matches!(set.resolve("nope"), Err(SchemaError::UnknownPack(_))));
    set.add_yaml("name: a\nversion: \"1\"\nextends: b\n").unwrap();
    set.add_yaml("name: b\nversion: \"1\"\nextends: a\n").unwrap();
    assert!(matches!(set.resolve("a"), Err(SchemaError::Cycle(_))));
    set.add_yaml("name: bad\nversion: \"1\"\nblock_types:\n  doc: { prefix: d, children: [ghost] }\n").unwrap();
    assert!(matches!(set.resolve("bad"), Err(SchemaError::Invalid(m)) if m.contains("ghost")));
    set.add_yaml("name: dup\nversion: \"1\"\nblock_types:\n  x: { prefix: d }\n  y: { prefix: d }\n").unwrap();
    assert!(matches!(set.resolve("dup"), Err(SchemaError::Invalid(m)) if m.contains("prefix")));
    assert!(matches!(set.add_yaml("name: [unclosed"), Err(SchemaError::Yaml(_))));
}
