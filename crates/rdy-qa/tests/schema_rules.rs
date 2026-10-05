//! Pack rules: content directly in a doc (direct1), optional lang and alt (opt1), the key field (key1).
use rdy_engine::Scope;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn direct1_a_doc_holds_content_blocks_directly_and_still_no_tree_types() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "Opening words."},
        {"op": "create", "type": "note", "parent": "$d", "body": "Read this first."},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Summary"}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let doc = BlockId::parse(&res.created["$d"]).unwrap();
    let m = r.materialized(&Scope::Canon).unwrap().clone();
    let kinds: Vec<String> = m
        .children(Some(&doc))
        .iter()
        .map(|b| b.block_type.clone())
        .collect();
    assert_eq!(kinds, ["prose", "note", "section"]);
    let e = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "parent": res.created["$d"], "fields": {"title": "Inner", "doc_type": "overview"}}
    ])).unwrap_err();
    assert!(
        format!("{e:?}").contains("E_CHILD_TYPE"),
        "a doc still cannot sit in a doc: {e:?}"
    );
}

#[test]
fn opt1_code_without_lang_and_diagram_without_alt_are_valid() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Sketch", "doc_type": "overview"}},
        {"op": "create", "type": "code", "parent": "$d", "body": ">>> match x:\n...     case 1: pass"},
        {"op": "create", "type": "diagram", "parent": "$d", "fields": {"lang": "mermaid"}, "body": "graph TD\n A --> B"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
}

#[test]
fn key1_a_key_is_valid_on_every_type_and_a_bad_one_is_refused_by_name() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "doc_type": "overview", "key": "SPEC"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Guards", "key": "SPEC-GUARDS"}},
        {"op": "create", "type": "requirement", "parent": "$s", "fields": {"key": "REQ-1", "priority": "must"}, "body": "It must."}
    ])).unwrap();
    for (bad, kind) in [
        (json!("c-WYF81"), "E_BAD_KEY"),
        (json!("FW S1"), "E_BAD_KEY"),
        (json!(7), "E_FIELD_KIND"),
    ] {
        let e = write(&mut r, "work", json!([
            {"op": "create", "type": "prose", "parent": res.created["$s"], "fields": {"key": bad}, "body": "x"}
        ])).unwrap_err();
        assert!(e.issues.iter().any(|i| i.code == kind), "{bad}: {e:?}");
    }
}

#[test]
fn key1_a_pack_cannot_declare_a_field_named_key() {
    let mut set = rdy_schema::PackSet::with_builtin();
    let yaml = "name: k\nversion: '1'\nextends: rdy-standard\nblock_types:\n  prose: { prefix: p, traits: [informative], body: required, fields: { key: { kind: str } } }\n";
    // the refusal may come when the pack is added or when it is resolved
    let e = match set.add_yaml(yaml) {
        Err(e) => e.to_string(),
        Ok(_) => set.resolve("k").unwrap_err().to_string(),
    };
    assert!(e.contains("key") && e.contains("reserved"), "{e}");
}

#[test]
fn dtext1_a_display_text_reference_is_an_ordinary_reference_to_the_engine() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$d", "fields": {"name": "Guard"}, "body": "A condition."},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "See {{$t|the guard rule}}."}
    ])).unwrap();
    let (t, p) = (
        BlockId::parse(&res.created["$t"]).unwrap(),
        BlockId::parse(&res.created["$p"]).unwrap(),
    );
    let b = r.block(&Scope::View("work".into()), &p).unwrap();
    assert_eq!(b.body, format!("See {{{{{t}|the guard rule}}}}."));
    assert!(
        b.pins.contains_key(&t),
        "a display-text reference pins its target"
    );
}

#[test]
fn key1_a_placeholder_keeps_its_key_typed_and_escaped_forms_when_substituted() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$d", "fields": {"name": "Guard"}, "body": "A condition."},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A {{$t|@key}} and B {{$t|\\@x}}."}
    ])).unwrap();
    let (t, p) = (
        BlockId::parse(&res.created["$t"]).unwrap(),
        BlockId::parse(&res.created["$p"]).unwrap(),
    );
    let b = r.block(&Scope::View("work".into()), &p).unwrap();
    assert_eq!(
        b.body,
        format!("A {{{{{t}|@key}}}} and B {{{{{t}|\\@x}}}}.")
    );
}
