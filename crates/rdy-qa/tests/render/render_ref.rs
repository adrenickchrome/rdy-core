//! FW-D15: the Copy ref payload, version pinning on frozen pages, the Cite control, and no id in visible text (ref1).
use rdy_engine::Scope;
use rdy_qa::{golden::*, site::*};

fn attr<'a>(h: &'a str, id: &str, name: &str) -> &'a str {
    let i = h.find(&format!("data-rdy-block=\"{id}\"")).unwrap();
    let s = &h[i..];
    let j = s.find(&format!(" {name}=\"")).unwrap() + name.len() + 3;
    let k = s[j..].find('"').unwrap();
    &s[j..j + k]
}

#[test]
fn ref1_every_block_carries_a_ref_in_the_grammar_and_headings_get_the_button() {
    let (_d, r, ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = html(out.path(), "ledgers.html");
    assert_eq!(
        unescape(attr(&h, &ids.requirement.to_string(), "data-ref")),
        format!(
            "[{}] Ledgers › Rules: \"Ledgers are atomic when settled.\"",
            ids.requirement
        )
    );
    assert_eq!(
        unescape(attr(&h, &ids.requirement.to_string(), "data-ref-head")),
        format!("[{}] Ledgers › Rules", ids.requirement)
    );
    assert!(h.contains("class=\"tool copy-ref\" aria-label=\"Copy a reference to this block for an agent chat\">Copy ref</button>"));
    assert!(h.contains("class=\"tool copy-ref\" aria-label=\"Copy a reference to this section for an agent chat\">Copy ref</button>"), "sections have the toolbar too");
    // the third control, Copy (D-BRF4P), after Copy ref on blocks and sections
    assert!(h.contains("class=\"tool copy-ref\" aria-label=\"Copy a reference to this block for an agent chat\">Copy ref</button><button type=\"button\" class=\"tool copy-text\""), "{h}");
    assert!(
        h.contains("aria-label=\"Copy this block as Markdown\">Copy</button>"),
        "{h}"
    );
    assert!(
        h.contains("aria-label=\"Copy this section as Markdown\">Copy</button>"),
        "{h}"
    );
    // the page title carries its doc's ref and the same toolbar: Link to the page, Copy ref, Copy
    let doc = ids.doc.to_string();
    assert_eq!(
        unescape(attr(&h, &doc, "data-ref-head")),
        format!("[{doc}] Ledgers")
    );
    assert!(unescape(attr(&h, &doc, "data-ref")).starts_with(&format!("[{doc}] Ledgers")));
    assert!(h.contains("class=\"tool copy-link\" data-href=\"\" aria-label=\"Copy link to this page\">Link</button><button type=\"button\" class=\"tool copy-ref\" aria-label=\"Copy a reference to this page for an agent chat\">Copy ref</button><button type=\"button\" class=\"tool copy-text\""), "{h}");
    assert_eq!(
        h.matches("aria-label=\"Copy this page as Markdown\">Copy</button>")
            .count(),
        1,
        "{h}"
    );
    assert_eq!(h.matches("contextmenu").count(), 0);
    let js = std::fs::read_to_string(out.path().join("assets/app.js")).unwrap();
    // the theme toggle's arrow keys do call it (a keyboard event); the copy script never does
    let copy = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/copy.js"
    ))
    .unwrap();
    assert!(
        !js.contains("contextmenu") && !copy.contains("preventDefault"),
        "never override the context menu"
    );
    assert!(
        js.contains("selectionchange") && js.contains("class=\"tool cite\"")
            || js.contains("tool cite")
    );
}

#[test]
fn ref1_snippets_cut_at_a_word_boundary_with_an_ellipsis_and_quotes_are_straightened() {
    let (_d, mut r) = tiny();
    let res = write(&mut r, "work", serde_json::json!([
        {"op":"create","type":"doc","id":"$d","fields":{"title":"Household \"ledger\"","doc_type":"page"}},
        {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Components"}},
        {"op":"create","type":"claim","id":"$c","parent":"$s","body":"The Balance store keeps no balance column; every balance is derived from the ledger at read time, which is the whole point of the design."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let h = html(out.path(), "household-ledger.html");
    let c = res.created["$c"].to_string();
    let r#ref = unescape(attr(&h, &c, "data-ref"));
    assert_eq!(
        r#ref,
        format!(
            "[{c}] Household 'ledger' › Components: \"The Balance store keeps no balance column; every balance is derived from the…\""
        ),
        "{}",
        r#ref
    );
    assert!(r#ref.len() <= c.len() + 150);
}

#[test]
fn ref1_frozen_pages_pin_the_version_and_between_versions_they_do_not() {
    let (_d, mut r, ids) = decided_site();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let v: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::At(v), out.path()).unwrap();
    let h = html(out.path(), "ledgers.html");
    assert!(
        unescape(attr(&h, &ids.requirement.to_string(), "data-ref"))
            .starts_with(&format!("[{}@v1.0.0] Ledgers › Rules: ", ids.requirement))
    );
    write(&mut r, "work", serde_json::json!([{"op":"update","id":ids.doc.to_string(),"fields":{"title":"Ledgers again"}}])).unwrap();
    let landing = land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::At(landing.parse().unwrap()), out.path()).unwrap();
    let h = html(out.path(), "ledgers.html");
    assert!(
        unescape(attr(&h, &ids.requirement.to_string(), "data-ref"))
            .starts_with(&format!("[{}] Ledgers again › Rules: ", ids.requirement)),
        "between versions: no pin"
    );
    let canon = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, canon.path()).unwrap();
    assert!(
        !unescape(attr(
            &html(canon.path(), "ledgers.html"),
            &ids.requirement.to_string(),
            "data-ref"
        ))
        .contains('@'),
        "canon is never pinned"
    );
}

/// The payload half of the test below: it stands on its own because the scan half waits for the
/// other tasks' leak fixes.
#[test]
fn ref1_the_payload_is_never_visible_text() {
    let (_d, _r, ids, out) = site_frozen();
    let h = html(out.path(), "specification/patterns.html");
    assert!(h.contains("data-ref=\"[") && h.contains("@v1.0.0] "));
    assert!(!visible_runs(&h).iter().any(|r| {
        r.text.contains(&ids.soft_keywords_requirement.to_string()) || r.text.contains("@v1.0.0")
    }));
    assert!(
        !leaks(out.path()).iter().any(|l| l.text.contains("Copy ref")
            || l.text.contains("Cite")
            || l.text.contains("@v1.0.0")),
        "nothing the toolbar adds is a leak"
    );
}

#[test]
fn ref1_the_payload_is_never_visible_text_and_the_scan_stays_clean() {
    let (_d, _r, ids, out) = site_frozen();
    let h = html(out.path(), "specification/patterns.html");
    assert!(h.contains("data-ref=\"[") && h.contains("@v1.0.0] "));
    assert!(!visible_runs(&h).iter().any(|r| {
        r.text.contains(&ids.soft_keywords_requirement.to_string()) || r.text.contains("@v1.0.0")
    }));
    assert!(leaks(out.path()).is_empty());
}
