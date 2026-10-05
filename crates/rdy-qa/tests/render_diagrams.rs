//! Bundled Mermaid, loaded only where needed, themed, degrading without JS (dia1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn dia1_a_diagram_page_bundles_mermaid_and_a_plain_page_does_not_load_it() {
    let (_d, mut r, _ids) = every_type_site(); // has one diagram on all.html
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let js = out.path().join("assets/js/mermaid.min.js");
    assert!(js.exists() && js.metadata().unwrap().len() > 3_000_000);
    let all = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    assert!(all.contains("<script src=\"assets/js/mermaid.min.js\" defer></script>"));
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(!home.contains("mermaid.min.js"));
    let app = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    assert!(
        app.contains("securityLevel: \"strict\"")
            && app.contains("rdy:theme")
            && app.contains("prefers-color-scheme: dark")
    );
    assert!(!app.contains("http"));
}

/// Review Focus 4.
#[test]
fn dia1_a_project_without_diagrams_ships_no_mermaid() {
    let (_d, mut r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    assert!(!out.path().join("assets/js").exists());
    assert!(
        !std::fs::read_to_string(out.path().join("assets/app.js"))
            .unwrap()
            .contains("mermaid.initialize")
    );
    for p in pages(out.path()) {
        assert!(
            !std::fs::read_to_string(&p).unwrap().contains("mermaid"),
            "{}",
            p.display()
        );
    }
}

#[test]
fn dia1_without_scripts_the_source_alt_and_caption_remain() {
    let (_d, mut r, _ids) = every_type_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let all = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    let fig = &all[all.find("<figure class=\"diagram\"").unwrap()..all.find("</figure>").unwrap()];
    assert!(
        fig.contains("<pre class=\"mermaid\">graph TD")
            && fig.contains("<figcaption>The flow</figcaption>")
            && fig.contains("<p class=\"visually-hidden\">The flow</p>")
    );
    assert!(fig.contains("<details class=\"diagram-source\">"));
    assert!(!fig.contains("<script"));
}

#[test]
fn dia1_a_d2_diagram_shows_its_source_and_caption_only() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", serde_json::json!([{"op":"create","type":"doc","id":"$d","fields":{"title":"D","doc_type":"glossary"}},{"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},{"op":"create","type":"diagram","parent":"$s","fields":{"lang":"d2","alt":"Boxes"},"body":"a -> b"}])).unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("d.html")).unwrap();
    assert!(
        h.contains("data-lang=\"d2\"")
            && !h.contains("diagram-canvas")
            && h.contains("a -&gt; b")
            && h.contains("<figcaption>Boxes</figcaption>")
    );
    assert!(
        !out.path().join("assets/js/mermaid.min.js").exists(),
        "d2 needs no mermaid"
    );
}
