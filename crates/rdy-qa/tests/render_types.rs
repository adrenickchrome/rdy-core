//! One designed rendering per content type (type1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn type1_each_type_renders_its_markup_and_no_type_label() {
    let (_d, mut r, ids) = every_type_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    assert!(h.contains("<article class=\"page\" data-rdy=\"content\">"));
    assert!(h.contains("<h2 class=\"section-title\">Terms</h2>"));
    assert!(h.contains("<p>Plain words.</p>") && !h.contains("class=\"meta\">prose"));
    assert!(h.contains("<div class=\"normative\"><span class=\"marker\">Requirement · must</span><p>It must.</p></div>"));
    assert!(
        h.contains("<span class=\"marker\">Claim</span>")
            && h.contains("<span class=\"marker\">Constraint</span>")
    );
    assert!(h.contains(
        "<aside class=\"callout callout-example\"><span class=\"marker\">Example</span>"
    ));
    assert!(h.contains("<aside class=\"callout callout-note\"><span class=\"marker\">Note</span>"));
    assert!(!h.contains("Because reasons."), "rationale is off the site");
    assert!(h.contains("<dl class=\"term\"><dt id=\"ledger\">Ledger</dt><dd>Balances.</dd></dl>"));
    assert!(h.contains("<div class=\"entity entity-component\"><span class=\"marker\">Component</span><h3 class=\"entity-name\">Core</h3>"));
    assert!(h.contains("<div class=\"flow\"><h3 class=\"flow-name\">Settle</h3><ol class=\"steps\"><li>One</li><li>Two</li></ol>"));
    assert!(h.contains("<div class=\"table-wrap\"><table><caption>Table 1</caption><thead><tr><th scope=\"col\">Name</th><th scope=\"col\" class=\"num\">Count</th></tr></thead>"));
    assert!(
        h.contains("<td>a, b</td>") && h.contains("<td>k: v</td>"),
        "{h}"
    );
    assert!(
        h.contains("<figure class=\"diagram\" data-lang=\"mermaid\">")
            && h.contains("<details class=\"diagram-source\"><summary>Diagram source</summary>")
    );
    assert!(
        h.contains("<figure class=\"figure\"><img src=\"../assets/")
            || h.contains("<figure class=\"figure\"><img src=\"assets/")
    );
    assert!(h.contains("<span class=\"lang\">Python</span>") && h.contains("<button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button>"));
    assert!(h.contains(
        "<aside class=\"callout callout-question\"><span class=\"marker\">Open question</span>"
    ));
    assert!(!h.contains("class=\"decision\"") && !h.contains("class=\"badge"));
    assert!(
        h.contains(&format!("data-rdy-block=\"{}\"", ids.requirement))
            && h.contains(&format!(
                "<span id=\"{}\" class=\"id-anchor\"",
                ids.requirement
            ))
    );
}

#[test]
fn type1_references_link_with_hover_titles_and_unresolved_ones_keep_the_authors_words() {
    let (_d, mut r, _ids) = every_type_site();
    let out = tempfile::tempdir().unwrap();
    // the dangling prose cannot land: it lives in the unlanded view `dangling`
    rdy_render::render_site(&mut r, &Scope::View("dangling".into()), out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    assert!(
        h.contains("<a href=\"#ledger\" title=\"All › Terms\">Ledger</a>"),
        "{h}"
    );
    assert!(h.contains("See <a href=\"#ledger\" title=\"All › Terms\">Ledger</a> and <span class=\"ref unresolved\" title=\"This reference could not be resolved\">unresolved reference</span>."), "{h}");
    assert!(
        !h.contains("p-ZZZZZ"),
        "the id reaches neither the text nor an attribute: {h}"
    );
}

#[test]
fn type1_every_marker_and_eyebrow_starts_with_a_capital() {
    let (_d, mut r, _ids) = every_type_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("all.html")).unwrap();
    for (i, _) in h.match_indices("class=\"marker\">") {
        let t = &h[i + 15..];
        let c = t.chars().next().unwrap();
        assert!(c.is_uppercase(), "marker {:?}", &t[..t.find('<').unwrap()]);
    }
    for (i, _) in h.match_indices("class=\"eyebrow\">") {
        let t = &h[i + 16..];
        assert!(
            t.chars().next().unwrap().is_uppercase(),
            "eyebrow {:?}",
            &t[..t.find('<').unwrap()]
        );
    }
}

/// Review Focus 2.
#[test]
fn type1_anchors_are_unique_non_empty_and_never_ids() {
    let (_d, mut r) = tiny();
    let res = write(&mut r, "work", serde_json::json!([
        {"op":"create","type":"doc","id":"$d","fields":{"title":"Σημασιολογία","doc_type":"glossary"}},
        {"op":"create","type":"section","id":"$s1","parent":"$d","fields":{"title":"Terms"}},
        {"op":"create","type":"section","id":"$s2","parent":"$d","fields":{"title":"Terms"}},
        {"op":"create","type":"section","id":"$s3","parent":"$d","fields":{"title":"Ωμέγα"}},
        {"op":"create","type":"prose","id":"$p","parent":"$s1","body":"d-7KQ2M is how an id looks."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let page = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.extension().is_some_and(|x| x == "html") && p.file_name().unwrap() != "index.html"
        })
        .unwrap();
    let h = std::fs::read_to_string(page).unwrap();
    assert!(
        h.contains("id=\"terms\"") && h.contains("id=\"terms-2\"") && h.contains("id=\"part\"")
    );
    assert!(!h.contains("id=\"d-7kq2m\"") && !h.contains("id=\"d-7KQ2M-is-how\""));
    let _ = res;
}

/// Review Focus 5.
#[test]
fn type1_hostile_text_is_escaped_in_hover_titles_labels_and_captions() {
    let (_d, mut r) = hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        for raw in [
            "<script>alert(1)",
            "onerror=alert(2)>",
            "\"'><img",
            "title=\"<",
            "<span class=\"lang\"><",
        ] {
            assert!(!h.contains(raw), "{}: {raw}", p.display());
        }
    }
}
