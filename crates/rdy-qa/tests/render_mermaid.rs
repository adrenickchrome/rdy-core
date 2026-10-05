//! Diagram labels (mer1).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

/// Includes Review Focus 5: `#` and `;`, Mermaid's own entity syntax, render literally.
#[test]
fn mer1_labels_in_diagram_source_are_escaped_for_mermaid() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger ] \"q\" (x) | y"}, "body": "Balances."},
        {"op": "create", "type": "term", "id": "$u", "parent": "$s", "fields": {"name": "C# ;x"}, "body": "A language."},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow"},
         "body": "graph TD\n  A[{{$t}}] --> B[Done]\n  B --> C[{{$u}}]"}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("tx.html")).unwrap();
    assert!(
        page.contains("A[Ledger #93; #34;q#34; #40;x#41; #124; y] --&gt; B[Done]"),
        "{page}"
    );
    assert!(page.contains("C[C#35; #59;x]"), "{page}");
    assert!(page.contains("assets/js/mermaid.min.js"), "{page}");
}
