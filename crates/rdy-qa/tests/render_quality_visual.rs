//! The visual review set (vis1). Renders the golden project's three sites into a fixed
//! directory under `target/qa/site/` and writes `review-set.json`: the eleven pages with their
//! URLs and the browser state to reach. The reviewer screenshots this render, so the set is
//! always the gate's own.
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
    ];
    let set: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, (name, site, url, state))| {
            json!({"n": i + 1, "name": name, "site": site, "url": format!("{site}/{url}"), "state": state})
        })
        .collect();
    assert_eq!(set.len(), 11);
    for (_, site, url, _) in &rows {
        let f = root.join(site).join(url);
        assert!(f.is_file(), "the review page {} is missing", f.display());
    }
    // Page 8's anchor is a block of the long doc's JSON twin.
    let json_twin = root.join("canon/specification/patterns.json");
    let twin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&json_twin)
            .unwrap_or_else(|e| panic!("{}: {e}", json_twin.display())),
    )
    .unwrap();
    let text = twin.to_string();
    assert!(
        text.contains(&format!("\"{anchor}\"")),
        "{anchor} is not in {}",
        json_twin.display()
    );
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
