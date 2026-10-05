//! The 24 leaks of the bar's section 3, pinned (site1..site24).
use rdy_qa::{golden, site::*};

fn canon() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    golden::Ids,
    tempfile::TempDir,
) {
    golden::site_canon()
}

fn has_leak(out: &std::path::Path, pattern: &str, needle: &str) -> bool {
    leaks(out)
        .iter()
        .any(|l| l.pattern == pattern && l.text.contains(needle))
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn runs_of(p: &std::path::Path) -> Vec<Run> {
    visible_runs(&read(p))
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site1_doc_meta_line_shows_no_doc_type_lifecycle_or_id() {
    let (_d, _r, ids, out) = canon();
    let p = golden::html(out.path(), "specification/patterns.html");
    assert!(!has_leak(out.path(), "F1", &ids.spec_doc.to_string()));
    assert!(
        !visible_runs(&p)
            .iter()
            .any(|r| r.label && (r.text == "overview" || r.text == "draft"))
    );
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site2_folder_pages_show_no_type_name_or_id() {
    let (_d, _r, ids, out) = canon();
    let p = golden::html(out.path(), "specification/index.html");
    assert!(
        !p.contains(&ids.spec_folder.to_string())
            || !has_leak(out.path(), "F1", &ids.spec_folder.to_string())
    );
    assert!(
        !visible_runs(&p)
            .iter()
            .any(|r| r.label && (r.text == "folder" || r.text == "doc"))
    );
}

#[test]
fn site3_no_decision_page_exists_and_no_raw_status_label() {
    let (_d, _r, _ids, out) = canon();
    assert!(!out.path().join("decisions").exists() && !out.path().join("_decision").exists());
    for p in golden::pages(out.path()) {
        assert!(
            !runs_of(&p).iter().any(|r| r.label
                && [
                    "live",
                    "proposed",
                    "rejected",
                    "accepted",
                    "superseded",
                    "partially superseded",
                    "revoked"
                ]
                .contains(&r.text.as_str())),
            "{}",
            p.display()
        );
    }
}

#[test]
fn site4_no_raw_edge_kind_is_visible_anywhere() {
    let (_d, _r, _ids, out) = canon();
    for k in [
        "decided_by",
        "supersedes",
        "depends_on",
        "conditional_on",
        "implements",
        "references",
        "(part:",
    ] {
        assert!(
            !golden::pages(out.path())
                .iter()
                .any(|p| runs_of(p).iter().any(|r| r.text.contains(k))),
            "{k}"
        );
    }
}

#[test]
#[ignore = "SITE-BAR: Task 12"]
fn site5_home_versions_show_no_landing_hash_or_the_word_landing() {
    let (_d, _r, _ids, out) = canon();
    assert!(!has_leak(out.path(), "F2", ""));
    let h = golden::html(out.path(), "index.html");
    assert!(
        !visible_runs(&h)
            .iter()
            .any(|r| r.text.to_lowercase().contains("landing"))
    );
    assert!(h.contains("Accepted by the Steering Council"));
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site6_no_block_is_prefixed_with_its_type_name() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        assert!(
            !runs_of(&p).iter().any(|r| r.label
                && [
                    "prose",
                    "claim",
                    "requirement",
                    "constraint",
                    "example",
                    "note",
                    "rationale",
                    "term",
                    "component",
                    "actor",
                    "interface",
                    "flow",
                    "table",
                    "diagram",
                    "figure",
                    "code",
                    "question",
                    "decision"
                ]
                .contains(&r.text.as_str())),
            "{}",
            p.display()
        );
    }
}

#[test]
fn site7_inline_decisions_show_no_status_badge_and_link_to_no_decision_dir() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        let h = read(&p);
        assert!(!h.contains("class=\"badge"), "{}", p.display());
        assert!(
            !attrs(&h, "href")
                .iter()
                .any(|x| x.contains("_decision/") || x.contains("decisions/")),
            "{}",
            p.display()
        );
    }
}

#[test]
fn site8_no_suspect_badge() {
    // the fixture's views hold no suspect: a prose that cites a superseded decision makes one
    let (_d, mut r, ids) = golden::project();
    rdy_qa::fixtures::write(
        &mut r,
        "editing",
        serde_json::json!([{"op":"create","type":"prose","parent":ids.guards_section.to_string(),
            "body": format!("This text still leans on {{{{{}}}}}.", ids.superseded_622_decision)}]),
    )
    .unwrap();
    let scope = rdy_engine::Scope::View("editing".into());
    assert!(
        !r.suspects(&scope).unwrap().is_empty(),
        "the view needs a suspect for this test to mean anything"
    );
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &scope, out.path()).unwrap();
    for p in golden::pages(out.path()) {
        assert!(
            !runs_of(&p).iter().any(|r| r.text == "suspect"),
            "{}",
            p.display()
        );
    }
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site9_an_unresolved_reference_shows_a_marker_not_the_id() {
    let (_d, _r, _ids, out) = golden::site_view();
    let h = golden::html(out.path(), "specification/patterns.html");
    assert!(!visible_runs(&h).iter().any(|r| r.text.contains("p-ZZZZZ")));
    assert!(h.contains("<span class=\"ref unresolved\" title=\"This reference could not be resolved\">unresolved reference</span>"));
    assert!(
        visible_runs(&h)
            .iter()
            .any(|r| r.text.contains("for the history"))
    );
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site10_nested_table_cells_show_no_debug_output() {
    let (_d, _r, _ids, out) = canon();
    assert!(!has_leak(out.path(), "F10", ""));
    let h = golden::html(out.path(), "specification/patterns.html");
    assert!(
        h.contains("<td>a, b</td>") && h.contains("<td>k: v</td>"),
        "{h}"
    );
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site11_a_dangling_reference_in_a_diagram_shows_no_id() {
    let (_d, _r, _ids, out) = golden::site_view();
    let h = golden::html(out.path(), "specification/evaluation.html");
    assert!(
        !visible_runs(&h)
            .iter()
            .any(|r| r.zone == Zone::Content && r.text.contains("p-ZZZZZ"))
    );
    // the walker skips <pre>, where a diagram's source sits and where a reader sees the id once the
    // diagram is drawn: the source must not hold the id either
    assert!(
        !h.contains("p-ZZZZZ"),
        "the dangling id survives in the page's markup"
    );
}

#[test]
fn site12_a_titleless_doc_is_never_labelled_with_its_id() {
    let (_d, mut r) = golden::generated_tree();
    // a doc requires a title: the empty string is "no title"
    let doc = rdy_qa::fixtures::write(
        &mut r,
        "work",
        serde_json::json!([{"op":"create","type":"doc","id":"$d","fields":{"title":"","name":"untitled-one","doc_type":"glossary"}}]),
    )
    .unwrap()
    .created["$d"]
        .clone();
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &rdy_engine::Scope::Canon, out.path()).unwrap();
    assert!(!has_leak(out.path(), "F1", &doc.to_string()));
    assert!(golden::html(out.path(), "index.html").contains("Untitled one"));
}

#[test]
fn site13_a_missing_link_target_is_never_labelled_with_its_id() {
    let (_d, _r, _ids, out) = canon();
    assert!(!has_leak(out.path(), "F1", ""));
}

#[test]
fn site14_no_breadcrumb_tail_reads_decision_id() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        assert!(
            !runs_of(&p).iter().any(|r| r.text.starts_with("decision ")),
            "{}",
            p.display()
        );
    }
}

#[test]
fn site15_the_site_name_is_the_projects_name_not_rdy() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        let h = read(&p);
        assert!(h.contains("Structural pattern matching"), "{}", p.display());
        assert!(
            !visible_runs(&h).iter().any(|r| r.label && r.text == "rdy"),
            "{}",
            p.display()
        );
        assert!(
            !h.contains("<title>") || !h.contains("· rdy</title>"),
            "{}",
            p.display()
        );
    }
}

#[test]
fn site16_no_underscore_directory_and_no_id_or_hash_page_in_any_url() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        let rel = p.strip_prefix(out.path()).unwrap().to_string_lossy();
        assert!(!rel.split('/').any(|s| s.starts_with('_')), "{rel}");
        for h in attrs(&read(&p), "href") {
            assert!(!h.split('/').any(|s| s.starts_with('_')), "{rel}: {h}");
        }
    }
    assert!(out.path().join("assets").is_dir() && !out.path().join("_assets").exists());
}

#[test]
#[ignore = "SITE-BAR: Task 12"]
fn site17_the_version_label_uses_site_wording() {
    let (_d, _r, _ids, out) = golden::site_view();
    let h = golden::html(out.path(), "tutorial/adventure.html");
    let v = visible_runs(&h)
        .into_iter()
        .find(|r| r.path.iter().any(|t| t == "span") && r.label && r.text.starts_with("v1.0.0"))
        .expect("version pill");
    assert_eq!(
        v.text,
        "v1.0.0, 2 changes since · draft: editing, 2 pending changes"
    );
    assert!(!has_leak(out.path(), "F11", ""));
}

#[test]
fn site18_the_home_page_lists_no_decisions() {
    let (_d, _r, _ids, out) = canon();
    let h = golden::html(out.path(), "index.html");
    assert!(
        !visible_runs(&h)
            .iter()
            .any(|r| r.label && r.text == "Decisions")
    );
    assert!(!h.contains("The wildcard is"));
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site19_a_missing_figure_reads_as_a_sentence_not_storage_words() {
    let (_d, mut r) = golden::hostile();
    let res = rdy_qa::fixtures::write(
        &mut r,
        "work",
        serde_json::json!([
            {"op":"create","type":"doc","id":"$d","fields":{"title":"Gone","doc_type":"glossary"}},
            {"op":"create","type":"section","id":"$s","parent":"$d","fields":{"title":"Terms"}}
        ]),
    )
    .unwrap();
    let s = res.created["$s"].clone();
    // `write` refuses a figure whose blob is absent; the gate does not check, so a raw commit lands
    rdy_qa::fixtures::raw_commit(
        &mut r,
        "work",
        vec![rdy_qa::fixtures::raw(
            "fg-0ST01",
            "figure",
            Some(&s),
            "zz",
            &[
                (
                    "blob",
                    "0000000000000000000000000000000000000000000000000000000000000000",
                ),
                ("media_type", "image/png"),
                ("alt", "A lost picture"),
            ],
            "",
        )],
    );
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &rdy_engine::Scope::Canon, out.path()).unwrap();
    let h = golden::html(out.path(), "gone.html");
    assert!(
        visible_runs(&h)
            .iter()
            .any(|r| r.text == "A lost picture (image not available)"),
        "{h}"
    );
    assert!(!has_leak(out.path(), "F9", "") && !has_leak(out.path(), "F2", ""));
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site20_a_flow_heading_sits_at_its_depth() {
    let (_d, _r, _ids, out) = canon();
    let h = golden::html(out.path(), "specification/evaluation.html");
    let levels = page_titles(&h);
    for w in levels.windows(2) {
        assert!(w[1].0 <= w[0].0 + 1, "{levels:?}");
    }
    assert!(
        levels
            .iter()
            .any(|(_, t)| t == "Evaluation of a match statement")
    );
}

#[test]
fn site21_theme_toggle_data_theme_and_no_cdn() {
    let (_d, _r, _ids, out) = canon();
    for p in golden::pages(out.path()) {
        let h = read(&p);
        // a `go/` redirect page is a bare hop: no toggle
        if h.contains("data-rdy-kind=\"redirect\"") {
            continue;
        }
        assert!(
            h.contains("role=\"radiogroup\"") && h.contains("data-theme-choice=\"dark\""),
            "{}",
            p.display()
        );
        assert!(
            !h.contains("cdn.") && !h.contains("https://"),
            "{}",
            p.display()
        );
    }
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site22_tables_are_wrapped_and_content_wraps_long_tokens() {
    let (_d, _r, _ids, out) = canon();
    let h = golden::html(out.path(), "specification/patterns.html");
    assert!(h.contains("<div class=\"table-wrap\"><table"));
    let css = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    assert!(css.contains("overflow-wrap: anywhere"));
}

#[test]
#[ignore = "SITE-BAR: Task post-G3"]
fn site23_toc_pager_search_skip_link_and_landmarks_exist() {
    let (_d, _r, _ids, out) = canon();
    let h = golden::html(out.path(), "specification/patterns.html");
    for s in [
        "class=\"skip\"",
        "<main",
        "aria-label=\"On this page\"",
        "aria-label=\"Previous and next\"",
        "id=\"search\"",
        "prefers-reduced-motion",
        ":focus-visible",
    ] {
        assert!(
            h.contains(s)
                || std::fs::read_to_string(out.path().join("assets/site.css"))
                    .unwrap()
                    .contains(s),
            "{s}"
        );
    }
}

#[test]
#[ignore = "SITE-BAR: Task 8"]
fn site24_blocks_carry_slug_anchors_and_id_anchors() {
    let (_d, _r, ids, out) = canon();
    let h = golden::html(out.path(), "specification/patterns.html");
    assert!(h.contains("id=\"guards\""));
    assert!(h.contains(&format!("id=\"{}\"", ids.guards_section)));
    assert!(h.contains("data-href=\"#guards\""));
}
