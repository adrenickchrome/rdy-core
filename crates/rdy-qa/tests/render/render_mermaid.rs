//! Diagram labels (mer1).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

/// Includes Review Focus 5: `#` and `;`, Mermaid's own entity syntax, render literally.
#[test]
fn mer1_labels_in_diagram_source_are_escaped_for_mermaid() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger ] \"q\" (x) | y"}, "body": "Balances."},
        {"op": "create", "type": "term", "id": "$u", "parent": "$s", "fields": {"name": "C# ;x"}, "body": "A language."},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow"},
         "body": "graph TD\n  A[{{$t}}] --> B[Done]\n  B --> C[{{$u}}]"}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = std::fs::read_to_string(out.path().join("tx.html")).unwrap();
    // the drawn labels show the characters, decoded once; no Mermaid code and no id is shown
    let at = page.find("<span class=\"diagram-svg\"").expect("a drawing");
    let drawn = &page[at..];
    let drawn = &drawn[..drawn.find("</svg>").unwrap()];
    assert!(
        drawn.contains(">Ledger ] &quot;q&quot; (x) | y<"),
        "{drawn}"
    );
    assert!(drawn.contains(">C# ;x<"), "{drawn}");
    assert!(drawn.contains(">Done<"), "{drawn}");
    for code in ["#93;", "#34;", "#40;", "#124;", "#35;", "#59;"] {
        assert!(!page.contains(code), "{code}: {page}");
    }
    let read = r.read();
    let m = read.materialized(&Scope::Canon).unwrap();
    for id in m.ids() {
        assert!(!drawn.contains(&id.to_string()), "{id}");
    }
    // the readable source reads the labels as written
    assert!(
        page.contains("A[Ledger ] &quot;q&quot; (x) | y] --&gt; B[Done]"),
        "{page}"
    );
    assert!(!page.contains("mermaid.min.js"), "{page}");
}
