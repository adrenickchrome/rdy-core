//! The inline subset on the rendered site (md1, dtext1, opt1).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_qa::site::attrs;
use serde_json::json;

fn render(r: &mut rdy_engine::Rdy) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    out
}

#[test]
fn md1_block_bodies_fields_and_cells_render_the_subset() {
    let (_d, mut r) = tiny();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a", "title": "Alpha", "description": "The *first* folder."}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "One", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "The `match` statement"}},
        {"op": "create", "type": "requirement", "id": "$r", "parent": "$s", "fields": {"priority": "must"}, "body": "Guards:\n- run *after* the pattern\n- see no bindings of later cases"},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Guard"}, "body": "An `if` clause; see **{{$r|the rule}}**."},
        {"op": "create", "type": "flow", "parent": "$s", "fields": {"name": "Match", "steps": ["Evaluate the **subject**", "Try each `case`"]}},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["Kind", "Example"], "rows": [["capture", "`x`"]]}},
        {"op": "create", "type": "question", "parent": "$s", "fields": {"prompt": "Should `__match_container__` replace the ABC test?"}}
    ])).unwrap();
    r.decide(
        &writer(),
        "work",
        rdy_engine::DecisionInput {
            parent: res.created["$s"].clone(),
            statement: "The wildcard is `_`".into(),
            affects: vec![res.created["$r"].clone()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let page = html(out.path(), "a/one.html");
    for want in [
        "<h2 class=\"section-title\">The <code>match</code> statement</h2>",
        "<strong>",
        "the rule</a></strong>",
        "<ul><li>run <em>after</em> the pattern</li><li>see no bindings of later cases</li></ul>",
        "<li>Evaluate the <strong>subject</strong></li><li>Try each <code>case</code></li>",
        "<td><code>x</code></td>",
        "The wildcard is <code>_</code>",
    ] {
        assert!(page.contains(want), "a/one.html lacks {want}");
    }
    assert!(html(out.path(), "a/index.html").contains("The <em>first</em> folder."));
    assert!(
        html(out.path(), "questions.html")
            .contains("Should <code>__match_container__</code> replace")
    );
    // no sweep for raw markers here: labels (link text, "defined in", toc) stay raw until DX2-5
}

/// Review Focus 1.
#[test]
fn md1_hostile_markup_never_becomes_markup() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Evil", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "[a](javascript:alert(1)) [b](JAVASCRIPT:alert(2)) [c](data:text/html,<script>alert(3)</script>) **<img src=x onerror=alert(4)>** `<script>alert(5)</script>` [d](\"onmouseover=alert(6))"},
        {"op": "create", "type": "flow", "parent": "$s", "fields": {"name": "F", "steps": ["[e](vbscript:x) *<b>*"]}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "evil.html");
    for href in attrs(&h, "href") {
        let lower = href.to_ascii_lowercase();
        assert!(
            !lower.starts_with("javascript:")
                && !lower.starts_with("data:")
                && !lower.starts_with("vbscript:"),
            "{href}"
        );
    }
    for raw in ["<img src=x", "<script>alert", "\"onmouseover", "<b>"] {
        assert!(!h.contains(raw), "{raw}");
    }
}

/// Review Focus 2.
#[test]
fn md1_relative_links_resolve_from_every_page_that_shows_the_block() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$i", "fields": {"title": "Intro", "name": "intro", "doc_type": "overview"}},
        {"op": "create", "type": "section", "parent": "$i", "fields": {"title": "Welcome"}},
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a", "title": "Alpha"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "One", "name": "one", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "See [the intro](../intro.html) and [terms](#terms)."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let own = html(out.path(), "a/one.html");
    assert!(
        own.contains("<a href=\"../intro.html\">the intro</a>")
            && own.contains("<a href=\"#terms\">terms</a>"),
        "{own}"
    );
    let gl = html(out.path(), "glossary.html");
    assert!(
        gl.contains("<a href=\"intro.html\">the intro</a>")
            && gl.contains("<a href=\"a/one.html#terms\">terms</a>"),
        "{gl}"
    );
    assert!(out.path().join("intro.html").exists());
}

#[test]
fn opt1_code_without_a_language_and_a_diagram_without_alt_render_cleanly() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Sketch", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "code", "parent": "$s", "body": ">>> match point:\n...     case (0, 0): print('Origin')"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid"}, "body": "graph TD\n A --> B"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "sketch.html");
    assert!(
        h.contains("<span class=\"lang\">Code</span>")
            && h.contains("<pre class=\"code\"><code>&gt;&gt;&gt; match point:"),
        "{h}"
    );
    assert!(
        !h.contains("language-\"") && !h.contains("<figcaption></figcaption>"),
        "{h}"
    );
    assert!(
        h.contains("<p class=\"visually-hidden\">Diagram (source below)</p>"),
        "{h}"
    );
}

/// FW-F2a (founder ruling): a key-typed reference (`{{id|@key}}`) shows the target's current key
/// on the site; a reference typed as an id still shows the label.
#[test]
fn key1_a_key_typed_reference_shows_the_targets_key_on_the_site() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Keys", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Guard", "key": "GUARD-1"}, "body": "A clause."},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Typed as a key: {{$t|@key}}. Typed as an id: {{$t}}."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "keys.html");
    assert!(h.contains("Typed as a key: <a href=\"#"), "{h}");
    assert!(
        h.contains(">GUARD-1</a>. Typed as an id: <a href=\"#"),
        "{h}"
    );
    assert!(h.contains(">Guard</a>."), "{h}");
}
