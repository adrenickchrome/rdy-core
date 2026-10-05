//! DX2 on the golden site: inline Markdown, display-text references, direct content, fallbacks (md1, dtext1, direct1, opt1, key1).
use rdy_qa::golden::*;

#[test]
fn md1_the_golden_long_doc_opens_with_typeset_direct_content() {
    let (_d, _r, _ids, out) = site_canon();
    let h = html(out.path(), "specification/patterns.html");
    let at = |s: &str| {
        h.find(s)
            .unwrap_or_else(|| panic!("patterns.html lacks {s}"))
    };
    assert!(
        at("<strong>specification</strong>") < at("class=\"section-title\""),
        "direct content sits before the first section"
    );
    for want in [
        "of the <code>match</code> statement",
        ">the companion rationale</a>",
        "<a href=\"../tutorial/adventure.html\">the tutorial</a>",
        "<ul><li>the syntax of patterns<ul><li>capture and wildcard patterns</li>",
        "<ol><li>Patterns</li><li>Evaluation</li><li>Guards</li></ol>",
        ">guards</a>, which see the bindings",
        "why the wildcard is an underscore</a>",
    ] {
        at(want);
    }
}

#[test]
fn opt1_the_golden_tutorial_renders_the_fallbacks() {
    let (_d, _r, _ids, out) = site_canon();
    let h = html(out.path(), "tutorial/adventure.html");
    assert!(
        h.contains("<span class=\"lang\">Code</span>")
            && h.contains("&gt;&gt;&gt; match command.split():")
    );
    assert!(h.contains("Diagram (source below)"));
}

/// FW-F2a (a founder ruling that overrides the plan's "keys appear in no rendered file"): a
/// reference the author typed as a key shows the target's current key. The fixture types one,
/// `{{REQ-SOFT-KEYWORDS}}` in the specification's opening note, so that key reads there and
/// nowhere else; the other two keys are referenced with display text and never appear.
#[test]
fn key1_no_golden_site_file_names_a_key_but_the_one_typed_as_a_reference() {
    const TYPED: &str = "REQ-SOFT-KEYWORDS";
    const TYPED_IN: &str = "soft: see REQ-SOFT-KEYWORDS";
    let mut typed_seen = false;
    for (_d, _r, _ids, out) in [site_canon(), site_frozen(), site_view()] {
        let mut stack = vec![out.path().to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if let Ok(t) = std::fs::read_to_string(&p) {
                    for k in KEYS {
                        if k == TYPED {
                            if t.contains(k) {
                                assert!(
                                    t.contains(TYPED_IN),
                                    "{} names {k} outside the typed reference",
                                    p.display()
                                );
                                typed_seen = true;
                            }
                        } else {
                            assert!(!t.contains(k), "{} names {k}", p.display());
                        }
                    }
                }
            }
        }
    }
    assert!(typed_seen, "the key-typed reference shows its key (FW-F2a)");
}

/// Task 4's lede rule counts sections: a doc with one section keeps that section's first
/// paragraph under its heading, even when a note follows the section directly in the doc.
#[test]
fn direct1_a_one_section_doc_with_a_trailing_note_keeps_its_first_paragraph_in_place() {
    use rdy_qa::fixtures::{land, project, write};
    let (_d, mut r) = project();
    write(&mut r, "work", serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "one", "title": "One", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Only"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "The first paragraph."},
        {"op": "create", "type": "note", "parent": "$d", "body": "A trailing note."},
        {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "two", "title": "Two", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s1", "parent": "$e", "fields": {"title": "First"}},
        {"op": "create", "type": "prose", "parent": "$s1", "body": "The opening line."},
        {"op": "create", "type": "section", "parent": "$e", "fields": {"title": "Second"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &rdy_engine::Scope::Canon, out.path()).unwrap();
    let one = html(out.path(), "one.html");
    assert!(!one.contains("class=\"lede\""), "{one}");
    assert!(
        one.find("class=\"section-title\"").unwrap() < one.find("The first paragraph.").unwrap(),
        "the paragraph stays under its heading"
    );
    let two = html(out.path(), "two.html");
    assert!(
        two.contains("<p class=\"lede\"") && two.contains("The opening line."),
        "two sections: the first one's paragraph is the lede"
    );
}
