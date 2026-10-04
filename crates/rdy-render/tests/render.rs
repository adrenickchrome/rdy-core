use rdy_engine::*;
use rdy_model::*;
use rdy_render::*;
use serde_json::json;

fn agent() -> Session { Session { actor: "agent:a".into(), run: None, role: Role::Writer } }
fn founder() -> Session { Session { actor: "human:founder".into(), run: None, role: Role::Approver } }

#[test]
fn text_html_escapes_then_links() {
    let t = BlockId::parse("t-MEDGR").unwrap();
    let html = text_html("<b>x</b> & {{t-MEDGR}} {{t-N0PE1}}\nnext", &|id| (id == &t).then(|| ("doc/d-AAAAA.html#t-MEDGR".into(), "Ledger".into())));
    assert_eq!(html, "&lt;b&gt;x&lt;/b&gt; &amp; <a href=\"doc/d-AAAAA.html#t-MEDGR\">Ledger</a> <span class=\"ref missing\">t-N0PE1</span><br>next");
}

#[test]
fn renders_docs_decisions_diagrams_and_assets() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "The <record>."},
        {"op": "create", "type": "claim", "parent": "$s", "body": "Balances live on the {{$t}}. <script>alert(1)</script>"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": "logo"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let d = r.decide(&agent(), "work", DecisionInput { parent: res.created["$s"].clone(), statement: "Ledger is append-only".into(), options: vec!["append-only".into(), "mutable".into()], ..Default::default() }).unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();

    let out = tempfile::tempdir().unwrap();
    let summary = render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.pages, 3); // index, one doc, one decision
    assert_eq!(summary.assets, 1);

    let doc_id = &res.created["$doc"];
    let t_id = &res.created["$t"];
    let did = &d.created["$decision"];
    let doc = std::fs::read_to_string(out.path().join(format!("doc/{doc_id}.html"))).unwrap();
    assert!(doc.contains("<title>Identity"));
    assert!(doc.contains(&format!("href=\"#{t_id}\">Ledger</a>")), "same-doc reference links to the anchor");
    assert!(doc.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!doc.contains("<script>alert(1)"));
    assert!(doc.contains("<pre class=\"mermaid\">graph TD\n  A --&gt; B</pre>"));
    assert!(doc.contains("mermaid"));
    assert!(doc.contains(&format!("src=\"../assets/{blob}\"")) && doc.contains("alt=\"logo\""));
    assert!(doc.contains(&format!("href=\"../decision/{did}.html\"")));

    let dec = std::fs::read_to_string(out.path().join(format!("decision/{did}.html"))).unwrap();
    assert!(dec.contains("Ledger is append-only"));
    assert!(dec.contains("live"));
    assert!(dec.contains("mutable"), "considered options are listed");

    let index = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(index.contains(&format!("doc/{doc_id}.html")));
    assert!(!index.contains("cdn.jsdelivr.net/npm/mermaid"), "no diagram script on pages without diagrams");
    assert!(out.path().join(format!("assets/{blob}")).exists());
}
