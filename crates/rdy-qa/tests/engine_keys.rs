//! Keys: short human ids, unique per project, resolved by the engine on write (key1).
use rdy_engine::{E_BAD_INPUT, E_KEY_CLASH, E_UNKNOWN_REF, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A landed doc "Spec" with section "Guards" (key SPEC-GUARDS) holding requirement REQ-1.
fn keyed() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Guards", "key": "SPEC-GUARDS"}},
        {"op": "create", "type": "requirement", "id": "$r", "parent": "$s", "fields": {"key": "REQ-1", "priority": "must"}, "body": "A guard runs after the pattern matches."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    (d, r, res.created["$s"].clone(), res.created["$r"].clone())
}

#[test]
fn key1_a_key_reference_is_stored_as_the_id_with_its_display_text() {
    let (_d, mut r, _s, req) = keyed();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$p", "parent": "SPEC-GUARDS", "body": "See {{REQ-1}} and {{ req-1 | the guard rule }}."}
    ])).unwrap();
    let p = r
        .read()
        .block(&Scope::View("work".into()), &id(&res.created["$p"]))
        .unwrap();
    // FW-F2a: a key typed bare is stored as the id with the key-typed mark
    assert_eq!(
        p.body,
        format!("See {{{{{req}|@key}}}} and {{{{{req}|the guard rule}}}}.")
    );
    assert!(p.pins.contains_key(&id(&req)));
}

#[test]
fn key1_write_resolves_keys_in_op_targets() {
    let (_d, mut r, s, req) = keyed();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$p", "parent": "SPEC-GUARDS", "after": "REQ-1", "body": "Next.",
         "links": {"describes": [{"target": "req-1"}]}}
    ])).unwrap();
    let p = r
        .read()
        .block(&Scope::View("work".into()), &id(&res.created["$p"]))
        .unwrap();
    assert_eq!(p.parent.unwrap().to_string(), s);
    assert_eq!(p.links["describes"][0].target.to_string(), req);
    for bad in ["c-WYF81", "FW S1"] {
        let e = write(&mut r, "work", json!([{"op": "create", "type": "prose", "parent": "SPEC-GUARDS", "fields": {"key": bad}, "body": "x"}])).unwrap_err();
        assert!(
            e.issues.iter().any(|i| i.code == "E_BAD_KEY"),
            "{bad}: {e:?}"
        );
    }
}

#[test]
fn key1_a_key_set_earlier_in_the_same_write_resolves_and_a_later_one_does_not() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "fields": {"key": "INTRO"}, "body": "Opening."},
        {"op": "create", "type": "prose", "parent": "$d", "body": "After {{INTRO}}."}
    ])).unwrap();
    let e = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$e", "fields": {"title": "Other", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$e", "body": "Before {{LATER}}."},
        {"op": "create", "type": "prose", "parent": "$e", "fields": {"key": "LATER"}, "body": "x"}
    ])).unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_REF);
    assert!(
        e.message.contains("LATER") && e.message.contains("neither a block id nor a key"),
        "{}",
        e.message
    );
}

#[test]
fn key1_write_refuses_a_key_already_taken_in_the_view_ignoring_case() {
    let (_d, mut r, s, req) = keyed();
    let e = write(&mut r, "work", json!([{"op": "create", "type": "prose", "parent": s, "fields": {"key": "req-1"}, "body": "x"}])).unwrap_err();
    assert_eq!(e.code, E_KEY_CLASH);
    assert!(
        e.message.contains("REQ-1") && e.message.contains(&req),
        "{}",
        e.message
    );
    // a block keeps its own key under a new spelling
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": req, "fields": {"key": "Req-1"}}]),
    )
    .unwrap();
}

#[test]
fn key1_two_views_each_fine_alone_clash_at_landing() {
    let (_d, mut r, s, _req) = keyed();
    write(&mut r, "a", json!([{"op": "create", "type": "prose", "parent": s, "fields": {"key": "NEW-1"}, "body": "from a"}])).unwrap();
    write(&mut r, "b", json!([{"op": "create", "type": "prose", "parent": s, "fields": {"key": "new-1"}, "body": "from b"}])).unwrap();
    land(&mut r, "a").unwrap();
    let e = land(&mut r, "b").unwrap_err();
    let clash = e
        .issues
        .iter()
        .find(|i| i.code == "E_KEY_CLASH")
        .unwrap_or_else(|| panic!("{e:?}"));
    assert!(
        clash.message.contains("NEW-1") || clash.message.contains("new-1"),
        "{}",
        clash.message
    );
}

#[test]
fn key1_read_surfaces_take_a_key_and_a_key_that_is_also_a_path_is_refused() {
    let (_d, mut r, _s, req) = keyed();
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "REQ-1")
            .unwrap()
            .to_string(),
        req
    );
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "req-1")
            .unwrap()
            .to_string(),
        req
    );
    let t = rdy_model::reference::parse_target("[REQ-1@v1.0.0] Spec › Guards: \"A guard runs\"")
        .unwrap();
    assert_eq!(
        (t.id.as_str(), t.version.as_deref()),
        ("REQ-1", Some("v1.0.0"))
    );
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$i", "fields": {"title": "Intro", "name": "intro", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$i", "fields": {"key": "intro"}, "body": "Hello."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let e = r.read().resolve_id(&Scope::Canon, "intro").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.fix.contains("id"), "{}", e.fix);
}

#[test]
fn key1_diagram_and_code_bodies_never_read_keys() {
    let (_d, mut r, s, _req) = keyed();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "diagram", "id": "$g", "parent": s, "fields": {"lang": "mermaid"}, "body": "graph TD\n A{{REQ-1}} --> B"},
        {"op": "create", "type": "code", "id": "$c", "parent": s, "body": "x = '{{REQ-1}}'"}
    ])).unwrap();
    let scope = Scope::View("work".into());
    assert!(
        r.read()
            .block(&scope, &id(&res.created["$g"]))
            .unwrap()
            .body
            .contains("A{{REQ-1}}")
    );
    assert!(
        r.read()
            .block(&scope, &id(&res.created["$c"]))
            .unwrap()
            .body
            .contains("'{{REQ-1}}'")
    );
}
