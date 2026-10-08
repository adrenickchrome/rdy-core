//! vrend1: a reference to a value renders the value itself, however it was typed; a value and an
//! entity render as definitions with their name, value, kind and aliases (FW-EN2, refining
//! FW-F2b).
use rdy_engine::{Rdy, Scope};
use rdy_qa::golden::*;
use serde_json::json;

fn render(r: &Rdy) -> (tempfile::TempDir, String, String) {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = html(out.path(), "limits.html");
    let full = std::fs::read_to_string(out.path().join("llms-full.txt")).unwrap();
    (out, h, full)
}

/// Doc `limits`: value Request timeout = 5 ms (key LIM-TIMEOUT); claims that reference it
/// plainly, by key and with display text; entity Ledger service (kind service, aliases ledger and
/// "the books"). Landed. Returns the value's id.
fn limits(r: &mut Rdy) -> String {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "limits", "title": "Limits", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "value", "id": "$v", "parent": "$s", "body": "How long a request may wait.", "fields": {"name": "Request timeout", "value": "5", "unit": "ms", "key": "LIM-TIMEOUT"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Requests time out after {{$v}}."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Typed as a key: {{LIM-TIMEOUT}}."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "{{$v|The timeout}} applies to reads."},
        {"op": "create", "type": "entity", "parent": "$s", "body": "Keeps the books.", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger", "the books"]}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    w.created["$v"].clone()
}

#[test]
fn vrend1_every_reference_to_a_value_reads_as_the_value() {
    let (_d, mut r) = tiny();
    let v = limits(&mut r);
    let (_o, h, full) = render(&r);
    assert_eq!(
        h.matches(">5 ms</a>").count(),
        2,
        "plain and key-typed references: {h}"
    );
    assert!(h.contains(">The timeout</a>"), "display text wins");
    assert!(
        !h.contains(">LIM-TIMEOUT<"),
        "a value never reads as its key"
    );
    assert!(full.contains("Requests time out after 5 ms."), "{full}");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": v, "fields": {"value": "7"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let (_o, h, full) = render(&r);
    assert_eq!(h.matches(">7 ms</a>").count(), 2, "{h}");
    assert!(!h.contains(">5 ms</a>"));
    assert!(full.contains("Requests time out after 7 ms."), "{full}");
}

#[test]
fn vrend1_a_value_and_an_entity_render_as_definitions() {
    let (_d, mut r) = tiny();
    limits(&mut r);
    let (_o, h, _) = render(&r);
    assert!(h.contains("class=\"entity entity-value\""), "{h}");
    assert!(
        h.contains("class=\"entity-name\">Request timeout</h"),
        "{h}"
    );
    assert!(h.contains("<p class=\"entity-value\">5 ms</p>"), "{h}");
    assert!(
        h.contains("id=\"request-timeout\""),
        "the anchor comes from the name: {h}"
    );
    assert!(h.contains("class=\"entity entity-entity\""), "{h}");
    assert!(h.contains("class=\"entity-name\">Ledger service</h"), "{h}");
    assert!(h.contains(">Service<"), "the kind is the eyebrow: {h}");
    assert!(
        h.contains("<p class=\"entity-aliases\">Also: ledger, the books</p>"),
        "{h}"
    );
}
