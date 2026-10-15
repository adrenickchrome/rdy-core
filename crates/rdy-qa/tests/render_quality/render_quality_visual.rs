//! The visual review set (vis1). Renders the golden project's three sites and the review project's view into a fixed
//! directory under `target/qa/site/`, fills four more from the live server (canon's also holds the
//! versions page and the tag's site), and writes `review-set.json`: the twenty-six pages with their
//! URLs and the browser state to reach. The reviewer screenshots this render, so the set is
//! always the gate's own.
use crate::render_quality_live::{
    CHANGED_DOC, disagree_live, golden_live, grouped_live, longest_doc, versions_page,
};
use rdy_engine::Scope;
use rdy_qa::golden;
use serde_json::json;
use std::path::{Path, PathBuf};

fn out_root() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/qa/site")).to_path_buf()
}

/// Replaces `dst` with a copy of the directory `src`.
fn replace_dir(src: &Path, dst: &Path) {
    if dst.exists() {
        std::fs::remove_dir_all(dst).unwrap();
    }
    copy_dir(src, dst);
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

#[test]
fn vis1_the_review_set_renders_and_the_manifest_matches() {
    let root = out_root();
    std::fs::create_dir_all(&root).unwrap();
    let (_d, _r, ids, canon) = golden::site_canon();
    replace_dir(canon.path(), &root.join("canon"));
    let (_d2, _r2, _i2, frozen) = golden::site_frozen();
    replace_dir(frozen.path(), &root.join("frozen"));
    let (_d3, _r3, _i3, view) = golden::site_view();
    replace_dir(view.path(), &root.join("view"));
    let (_d4, r4, _i4) = golden::review_project();
    let review = tempfile::tempdir().unwrap();
    rdy_render::render_site(
        &r4.read(),
        &rdy_engine::Scope::View(golden::REVIEW_VIEW.into()),
        review.path(),
    )
    .unwrap();
    replace_dir(review.path(), &root.join("review"));
    let (_d5, _r5, disagree) = golden::site_disagree();
    replace_dir(disagree.path(), &root.join("disagree"));
    // the live rows: the pages as the server sends them (every file, so the pages have their
    // styles), canon and the `editing` view of the golden project and the disagreement project
    let g = golden_live();
    let live_canon = g.live.fetch(&g.canon, "", false);
    replace_dir(live_canon.path(), &root.join("live-canon"));
    // the versions page and the tag's site, as the server sends them, beside canon's pages
    let versions = root.join("live-canon/~versions");
    std::fs::create_dir_all(&versions).unwrap();
    std::fs::write(versions.join("index.html"), versions_page(&g)).unwrap();
    let live_tag = g.live.fetch(&g.tag, &g.tag_prefix, false);
    replace_dir(
        live_tag.path(),
        &root.join("live-canon").join(&g.tag_prefix),
    );
    let live_view = g.live.fetch(&g.view, &g.view_prefix, false);
    replace_dir(live_view.path(), &root.join("live-view"));
    let (dh, _stat) = disagree_live();
    let live_disagree = dh.fetch(&Scope::Canon, "", false);
    replace_dir(live_disagree.path(), &root.join("live-disagree"));
    // a disagreement repeated over seven pages: the page folds its list behind "Show all"
    let (gh, _gstat) = grouped_live();
    let live_grouped = gh.fetch(&Scope::Canon, "", false);
    replace_dir(live_grouped.path(), &root.join("live-grouped"));
    let long_doc = longest_doc(g.static_canon.path());

    let anchor = ids.soft_keywords_requirement.to_string();
    let guards = ids.guards_section.to_string();
    // (name, site, url, state)
    let rows: Vec<(&str, &str, &str, String)> = vec![
        ("home", "canon", "index.html", "none".into()),
        ("folder", "canon", "specification/index.html", "none".into()),
        (
            "long-doc",
            "canon",
            "specification/patterns.html",
            format!("scroll to #{guards}"),
        ),
        (
            "tutorial",
            "canon",
            "tutorial/adventure.html",
            "scroll to the first code block".into(),
        ),
        ("glossary", "canon", "glossary.html", "none".into()),
        (
            "diagram-figure",
            "canon",
            "specification/evaluation.html",
            "scroll to the first figure.diagram, after the diagram renders".into(),
        ),
        (
            "search",
            "canon",
            "specification/patterns.html",
            "focus #search, type `guard`, wait for #results".into(),
        ),
        (
            "toolbar-open",
            "canon",
            "specification/patterns.html",
            format!(
                "open #{anchor}: click the summary of details.why; hover the block to show the toolbar (Link, Copy ref, and Cite after selecting a phrase); at 360px, tap the block's text instead of hovering"
            ),
        ),
        (
            "snapshot",
            "frozen",
            "specification/patterns.html",
            "none".into(),
        ),
        (
            "opening",
            "canon",
            "specification/patterns.html",
            "top of the page, no scroll: the direct content before the first section".into(),
        ),
        (
            "sketches",
            "canon",
            "tutorial/adventure.html",
            "scroll to the `Quick sketches` section".into(),
        ),
        (
            "review-home",
            "review",
            "index.html",
            "none: the Changes in this view list above Contents".into(),
        ),
        (
            "review-page",
            "review",
            "intro.html",
            "none; then open the Removed line; then press Show changes (the markers hide, the button reads not pressed)".into(),
        ),
        (
            "disagreements",
            "disagree",
            "disagreements.html",
            "none: one entry, the sentence above two links".into(),
        ),
        ("live-home", "live-canon", "index.html", "none".into()),
        (
            "live-doc",
            "live-canon",
            long_doc.as_str(),
            "none: the longest doc, served".into(),
        ),
        (
            "live-view-doc",
            "live-view",
            CHANGED_DOC,
            "none: the changed doc in review mode; then press Show changes".into(),
        ),
        (
            "live-glossary",
            "live-canon",
            "glossary.html",
            "none: the grouped glossary".into(),
        ),
        (
            "live-disagreements",
            "live-disagree",
            "disagreements.html",
            "none: one entry, the sentence above two links".into(),
        ),
        (
            "live-disagreements-grouped",
            "live-grouped",
            "disagreements.html",
            "closed: the entry's list shows the first links above `Show all`; then open the `Show all` fold (the rest of the pages list); then close it".into(),
        ),
        (
            "since",
            "canon",
            "changes-since.html",
            "none".into(),
        ),
        (
            "review-changes",
            "review",
            "changes.html",
            "none".into(),
        ),
        (
            "review-doc-diff",
            "review",
            "intro.html",
            "click #show-diff".into(),
        ),
        (
            "live-versions",
            "live-canon",
            "~versions/index.html",
            "none".into(),
        ),
        (
            "live-menu-open",
            "live-canon",
            "index.html",
            "click .pill".into(),
        ),
        (
            "live-tag",
            "live-canon",
            "~tag/v1.0.0/index.html",
            "none".into(),
        ),
    ];
    let set: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, (name, site, url, state))| {
            json!({"n": i + 1, "name": name, "site": site, "url": format!("{site}/{url}"), "state": state})
        })
        .collect();
    assert_eq!(set.len(), 26);
    for (_, site, url, _) in &rows {
        let f = root.join(site).join(url);
        assert!(f.is_file(), "the review page {} is missing", f.display());
    }
    // Page 8's anchor is a block on page 8.
    let page8 = std::fs::read_to_string(root.join("canon/specification/patterns.html")).unwrap();
    assert!(
        page8.contains(&format!("id=\"{anchor}\"")),
        "the anchor {anchor} is not an element of page 8"
    );
    std::fs::write(
        root.join("review-set.json"),
        serde_json::to_string_pretty(&json!({"pages": set, "captures": {"widths": [1280, 360], "themes": ["light", "dark"], "extra": "page 1 with the toggle on Auto and the colour scheme emulated dark and light"}})).unwrap(),
    )
    .unwrap();
}
