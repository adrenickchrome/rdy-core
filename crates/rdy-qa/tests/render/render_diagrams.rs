//! Diagrams drawn as SVG when the site is built: no library or script, themed by the site's
//! tokens, and the source, alt and caption kept (dia1, D-FY4VF, D-KAGB2).
use rdy_engine::Scope;
use rdy_qa::golden::*;

/// The diagram figures of a page, in order.
fn figures(html: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find("<figure class=\"diagram\"") {
        let end = rest[i..].find("</figure>").unwrap() + i + "</figure>".len();
        out.push(&rest[i..end]);
        rest = &rest[end..];
    }
    out
}

#[test]
fn dia1_a_diagram_page_bundles_mermaid_and_a_plain_page_does_not_load_it() {
    let (_d, r, _ids) = every_type_site(); // has one diagram on all.html
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    // no diagram library ships: the diagram is drawn into the page and its own file
    assert!(!out.path().join("assets/js").exists());
    let all = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    assert!(!all.contains("mermaid.min.js"));
    let fig = figures(&all)[0];
    assert!(
        fig.contains("<span class=\"diagram-svg\" aria-hidden=\"true\"><svg "),
        "{fig}"
    );
    let open = "<p class=\"diagram-open\"><a href=\"";
    let href = &fig[fig.find(open).unwrap() + open.len()..];
    let href = &href[..href.find('"').unwrap()];
    assert!(href.starts_with("assets/diagrams/") && href.ends_with(".svg"));
    assert!(out.path().join(href).exists());
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(!home.contains("mermaid.min.js") && !home.contains("assets/diagrams/"));
    let app = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    assert!(!app.contains("mermaid"));
    // app.js loads nothing from a remote URL (menu.js names the "http:" and "https:" protocols
    // in its same-origin check, which is not a URL)
    assert!(!app.contains("http://") && !app.contains("https://"));
}

/// Review Focus 4.
#[test]
fn dia1_a_project_without_diagrams_ships_no_mermaid() {
    let (_d, r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    assert!(!out.path().join("assets/js").exists());
    assert!(!out.path().join("assets/diagrams").exists());
    assert!(
        !std::fs::read_to_string(out.path().join("assets/app.js"))
            .unwrap()
            .contains("mermaid.initialize")
    );
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        assert!(!h.contains("mermaid"), "{}", p.display());
        assert!(!h.contains("diagram-svg"), "{}", p.display());
    }
}

#[test]
fn dia1_without_scripts_the_source_alt_and_caption_remain() {
    let (_d, r, _ids) = every_type_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let all = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    let fig = figures(&all)[0];
    // the drawing needs no script: native disclosure expands it and a link opens its file
    assert!(
        fig.contains("<details class=\"diagram-canvas\"><summary aria-label=\"Expand diagram\">")
            && fig.contains(">open full size</a>")
            && fig.contains("<figcaption>The flow</figcaption>")
            && fig.contains("<p class=\"visually-hidden\">The flow</p>")
    );
    assert!(fig.contains(
        "<details class=\"diagram-source\"><summary>Diagram source</summary><pre class=\"code\"><code>graph TD"
    ));
    assert!(!fig.contains("<script"));
}

#[test]
fn dia1_a_d2_diagram_shows_its_source_and_caption_only() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", serde_json::json!([{"op":"create","type":"doc","id":"$d","fields":{"title":"D","doc_type":"page"}},{"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}},{"op":"create","type":"diagram","parent":"$s","fields":{"lang":"d2","alt":"Boxes"},"body":"a -> b"}])).unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("d.html")).unwrap();
    // its source as a code block in place, unhighlighted, with the code-copy control
    let fig = figures(&h)[0];
    assert_eq!(
        fig,
        "<figure class=\"diagram\" data-lang=\"d2\"><p class=\"visually-hidden\">Boxes</p><div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">D2</span><button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button></div><pre class=\"code\"><code class=\"language-d2\">a -&gt; b</code></pre></div><figcaption>Boxes</figcaption></figure>"
    );
    assert!(
        !out.path().join("assets/js").exists(),
        "d2 needs no library"
    );
    assert!(
        !out.path().join("assets/diagrams").exists(),
        "nor a drawing"
    );
}
