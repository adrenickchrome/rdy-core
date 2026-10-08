//! Plain text in search, labels and Copy ref (md1, dtext1, key1); content directly in a doc (direct1).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use serde_json::{Value, json};

fn render(r: &mut rdy_engine::Rdy) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    out
}
fn read_json(p: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}
fn all_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p)
            } else {
                out.push(p)
            }
        }
    }
    out
}

/// A doc "Spec" whose first children are direct content (an opening prose, a note), then section
/// "Guards" holding a keyed requirement, a term and a prose citing both with display text.
fn spec() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    serde_json::Map<String, Value>,
) {
    let (d, mut r) = tiny();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Spec", "name": "spec", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$o", "parent": "$d", "body": "The **match** statement, in `brief`.\n\nA second paragraph."},
        {"op": "create", "type": "note", "id": "$n", "parent": "$d", "body": "Read *this* first."},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "The `case` clause"}},
        {"op": "create", "type": "requirement", "id": "$r", "parent": "$s", "fields": {"key": "QQKEY-77", "priority": "must"}, "body": "A **guard** runs after the pattern matches."},
        {"op": "create", "type": "term", "id": "$t", "parent": "$d", "fields": {"name": "Guard"}, "body": "An `if` clause."},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "See {{QQKEY-77|the guard rule}} and {{$t}}."},
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "rules", "description": "Rules on **guards** in `brief`, per [the docs](https://example.com), {{$t}} and {{QQKEY-77|the guard rule}}."}}
    ])).unwrap();
    r.decide(
        &writer(),
        "work",
        rdy_engine::DecisionInput {
            parent: res.created["$s"].clone(),
            statement: "Guards use **`if`**".into(),
            options: vec!["`if`".into(), "*when*".into()],
            affects: vec![res.created["$r"].clone()],
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let ids = res
        .created
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    (d, r, ids)
}

#[test]
fn md1_search_carries_plain_text() {
    let (_d, mut r, _ids) = spec();
    let out = render(&mut r);
    let idx = read_json(&out.path().join("search-index.json"));
    let texts: Vec<String> = idx["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| format!("{} | {}", e["title"], e["text"]))
        .collect();
    let joined = texts.join("\n");
    assert!(
        !joined.contains('`') && !joined.contains("**") && !joined.contains("QQKEY"),
        "{joined}"
    );
    assert!(
        joined.contains("The case clause") && joined.contains("the guard rule"),
        "{joined}"
    );
}

#[test]
fn dtext1_copy_ref_snippets_and_link_labels_read_display_text_and_no_markup() {
    let (_d, mut r, ids) = spec();
    let out = render(&mut r);
    let h = html(out.path(), "spec.html");
    let refs: Vec<String> = rdy_qa::site::attrs(&h, "data-ref")
        .into_iter()
        .map(rdy_qa::site::unescape)
        .collect();
    let p = refs
        .iter()
        .find(|x| x.starts_with(&format!("[{}]", ids["$p"].as_str().unwrap())))
        .unwrap_or_else(|| panic!("{refs:?}"));
    assert_eq!(
        p,
        &format!(
            "[{}] Spec › The case clause: \"See the guard rule and Guard.\"",
            ids["$p"].as_str().unwrap()
        )
    );
    let r1 = refs
        .iter()
        .find(|x| x.starts_with(&format!("[{}]", ids["$r"].as_str().unwrap())))
        .unwrap();
    assert!(
        r1.ends_with(": \"A guard runs after the pattern matches.\""),
        "{r1}"
    );
    assert!(
        h.contains(">the guard rule</a>"),
        "the display text is the link text"
    );
}

/// The sweep DX2-3 could not do: with labels plain too, no page shows a raw marker.
#[test]
fn md1_no_page_shows_a_raw_marker() {
    let (_d, mut r, _ids) = spec();
    let out = render(&mut r);
    for p in pages(out.path()) {
        for run in rdy_qa::site::visible_runs(&std::fs::read_to_string(&p).unwrap()) {
            use rdy_qa::site::Zone;
            assert!(
                !run.text.contains('`'),
                "{}: backtick in {:?}",
                p.display(),
                run.text
            );
            if matches!(run.zone, Zone::Content | Zone::Trail) && !run.label {
                assert!(
                    !run.text.contains("**"),
                    "{}: raw ** in {:?}",
                    p.display(),
                    run.text
                );
            }
        }
    }
}

#[test]
fn key1_no_rendered_file_holds_a_key() {
    let (_d, mut r, _ids) = spec();
    let out = render(&mut r);
    for f in all_files(out.path()) {
        if let Ok(text) = std::fs::read_to_string(&f) {
            assert!(!text.contains("QQKEY"), "{} holds the key", f.display());
        }
    }
}

/// Review Focus 5.
#[test]
fn direct1_a_doc_without_sections_renders_searches_and_cites_cleanly() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Flat", "name": "flat", "doc_type": "glossary"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "Everything here sits *directly* in the doc."},
        {"op": "create", "type": "term", "parent": "$d", "fields": {"name": "Ledger"}, "body": "Where balances come from."},
        {"op": "create", "type": "question", "parent": "$d", "fields": {"prompt": "Is a flat doc enough?"}},
        {"op": "create", "type": "doc", "id": "$e", "fields": {"title": "Next", "name": "next", "doc_type": "overview"}},
        {"op": "create", "type": "section", "parent": "$e", "fields": {"title": "Summary"}}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "flat.html");
    assert!(
        h.contains("<p class=\"lede\"")
            && h.contains("Everything here sits <em>directly</em> in the doc."),
        "the opening prose is the lede"
    );
    assert_eq!(
        h.matches("sits <em>directly</em>").count(),
        1,
        "the lede is rendered once (data-ref attributes hold plain text)"
    );
    assert!(!h.contains("toc-link"), "no on-page index without sections");
    assert!(h.contains("next.html"), "previous/next still link");
    let gl = html(out.path(), "glossary.html");
    assert!(
        gl.contains("defined in Flat</a>") && !gl.contains("Flat › <"),
        "{gl}"
    );
    let q = html(out.path(), "questions.html");
    assert!(
        q.contains("from Flat</a>") && !q.contains("Flat › <"),
        "{q}"
    );
    let idx = read_json(&out.path().join("search-index.json"));
    let doc = idx["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "doc" && e["title"] == "Flat")
        .unwrap();
    assert!(
        doc["text"]
            .as_str()
            .unwrap()
            .starts_with("Everything here sits directly in the doc."),
        "{doc}"
    );
}

#[test]
fn direct1_content_after_a_section_stays_in_stored_order() {
    let (_d, mut r) = tiny();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Order", "name": "order", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "First words."},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Middle"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Inside the middle."},
        {"op": "create", "type": "note", "parent": "$d", "body": "Closing note."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let h = html(render(&mut r).path(), "order.html");
    let at = |s: &str| h.find(s).unwrap_or_else(|| panic!("{s}"));
    assert!(
        at("First words.") < at("Middle")
            && at("Middle") < at("Inside the middle.")
            && at("Inside the middle.") < at("Closing note.")
    );
}

/// A folder's description is spans: home card, search entry and `llms.txt` read it as plain words.
#[test]
fn md1_a_folder_description_is_plain_on_the_home_card_in_search_and_in_llms_txt() {
    let (_d, mut r, ids) = spec();
    let out = render(&mut r);
    let want = "Rules on guards in brief, per the docs, Guard and the guard rule.";
    let home = html(out.path(), "index.html");
    assert!(home.contains(want), "home card: {want}");
    let idx = read_json(&out.path().join("search-index.json"));
    let f = idx["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "folder")
        .unwrap();
    assert_eq!(f["text"], want);
    let llms = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    assert!(llms.contains(&format!(": {want}")), "{llms}");
    let line = llms
        .lines()
        .find(|l| l.contains("rules/index.html"))
        .unwrap()
        .split_once("): ")
        .unwrap()
        .1
        .to_string();
    for (what, text) in [("search", f["text"].to_string()), ("llms.txt", line)] {
        let id = ids["$t"].as_str().unwrap();
        assert!(
            !text.contains("**")
                && !text.contains('`')
                && !text.contains("](")
                && !text.contains("{{")
                && !text.contains(id)
                && !text.contains("QQKEY"),
            "{what}: {text}"
        );
    }
}

/// A code block's Copy ref quotes its source as written: no Markdown is read out of it.
#[test]
fn md1_a_code_blocks_copy_ref_keeps_its_source_verbatim() {
    let (_d, mut r) = tiny();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Code", "name": "code", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Samples"}},
        {"op": "create", "type": "code", "id": "$c", "parent": "$s", "fields": {"lang": "yaml"}, "body": "- name: x\n1. step `cmd` and **kwargs**"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "code.html");
    let refs: Vec<String> = rdy_qa::site::attrs(&h, "data-ref")
        .into_iter()
        .map(rdy_qa::site::unescape)
        .collect();
    let id = &res.created["$c"];
    let c = refs
        .iter()
        .find(|x| x.starts_with(&format!("[{id}]")))
        .unwrap_or_else(|| panic!("{refs:?}"));
    assert!(c.contains("- name: x 1. step `cmd` and **kwargs**"), "{c}");
}

/// A diagram's Copy ref quotes its source as written, except that each reference in it reads as
/// on the drawn diagram: its display text, else its target's label (a dangling one: site11).
#[test]
fn md1_a_diagrams_copy_ref_reads_its_references_and_shows_no_id() {
    let (_d, mut r) = tiny();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Flow", "name": "flow", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Steps"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Guard"}, "body": "An `if` clause."},
        {"op": "create", "type": "diagram", "id": "$g", "parent": "$s", "fields": {"lang": "mermaid", "alt": "A flow"},
         "body": "flowchart TD\n  A[{{$t}}] --> B[{{$t|the **check**}}] --> D{{hex}}"}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let out = render(&mut r);
    let h = html(out.path(), "flow.html");
    let refs: Vec<String> = rdy_qa::site::attrs(&h, "data-ref")
        .into_iter()
        .map(rdy_qa::site::unescape)
        .collect();
    let id = &res.created["$g"];
    let g = refs
        .iter()
        .find(|x| x.starts_with(&format!("[{id}]")))
        .unwrap_or_else(|| panic!("{refs:?}"));
    let t = res.created["$t"].as_str();
    assert!(
        g.contains("flowchart TD A[Guard] --> B[the **check**] --> D{{hex}}"),
        "{g}"
    );
    assert!(!g.contains(t), "{g}");
}
