//! DX4: the navigation marks changed docs and lists removed ones, the home page lists what the
//! draft changes, and a draft render stays leak-clean with no needs-you queue (rv1; FW-RV2,
//! FW-RV3).
use rdy_engine::{DecisionInput, Scope};
use rdy_qa::golden::*;
use rdy_qa::site::{leaks, visible_runs};
use serde_json::json;

fn draft() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    ReviewIds,
    tempfile::TempDir,
) {
    review_site(&Scope::View(REVIEW_VIEW.into()))
}

#[test]
fn rv1_the_navigation_marks_edited_and_new_docs_and_lists_removed_ones() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "keep.html");
    let nav: Vec<String> = visible_runs(&h)
        .into_iter()
        .filter(|r| r.path.iter().any(|t| t == "aside"))
        .map(|r| r.text)
        .collect();
    let next = |label: &str| {
        nav.iter()
            .position(|t| t == label)
            .and_then(|i| nav.get(i + 1))
            .cloned()
    };
    assert_eq!(next("Intro").as_deref(), Some("edited"), "{nav:?}");
    assert_eq!(next("Fresh").as_deref(), Some("new"), "{nav:?}");
    assert!(
        !matches!(next("Keep").as_deref(), Some("edited" | "new")),
        "{nav:?}"
    );
    assert_eq!(next("Removed").as_deref(), Some("Old guide"), "{nav:?}");
    assert!(h.contains("<li class=\"nav-item nav-removed\">Old guide</li>"));
    assert!(!h.contains("old.html"));
    assert!(h.contains("<li class=\"nav-group chg\"><span class=\"nav-group-label\">Removed</span><ul><li class=\"nav-item nav-removed\">Old guide</li>"));
    // one row per changed doc: the marker sits inside the row's link (rvm1)
    assert!(h.contains(&format!(
        "<li class=\"nav-item\"><a href=\"intro.html\">Intro {}</a></li>",
        mark("edited")
    )));
    assert!(h.contains(&format!(
        "<li class=\"nav-item\"><a href=\"fresh.html\">Fresh {}</a></li>",
        mark("new")
    )));
}

#[test]
fn rv1_the_home_page_lists_the_drafts_changed_docs_with_plain_counts() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "index.html");
    let joined = visible_runs(&h)
        .into_iter()
        .map(|r| r.text)
        .collect::<Vec<_>>()
        .join(" | ");
    let list = "Changes in this view | Intro | 2\u{a0}edited \u{b7} 1\u{a0}new \u{b7} 1\u{a0}removed | Fresh | New | Old guide | Removed";
    assert!(joined.contains(list), "{joined}");
    assert!(joined.find(list).unwrap() < joined.find("Contents").unwrap());
    assert!(h.contains("<section class=\"chg\"><h2 id=\"review-changes\">Changes in this view</h2><ul class=\"changes\">"));
    assert!(h.contains("<span class=\"changes-gone\">Old guide</span>"));
}

#[test]
fn rv1_a_draft_render_is_leak_clean_and_shows_no_needs_you_queue() {
    let (_d, _r, _ids, out) = draft();
    let found = leaks(out.path());
    assert!(found.is_empty(), "{found:?}");
    for page in ["index.html", "intro.html", "keep.html", "fresh.html"] {
        let text = visible_runs(&html(out.path(), page))
            .into_iter()
            .map(|r| r.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        for queue in [
            "approve",
            "approval",
            "proposal",
            "waiting",
            "needs you",
            "queue",
        ] {
            assert!(!text.contains(queue), "{page} shows {queue}");
        }
    }
}

#[test]
fn rv1_a_draft_with_nothing_a_reader_sees_changed_shows_no_marker() {
    let (_d, mut r, ids) = review_project();
    r.decide(
        &writer(),
        "decided",
        DecisionInput {
            parent: ids.summary.to_string(),
            statement: "Keep the summary short".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("decided".into()), out.path()).unwrap();
    for p in pages(out.path()) {
        // the summary of the landings since the tag shows those landings' changes (sr6), and
        // the change summary of the view is the page the heading names (sr6, sr7)
        if p.ends_with("changes-since.html") || p.ends_with("changes.html") {
            continue;
        }
        let h = std::fs::read_to_string(&p).unwrap();
        for m in [
            "has-chg",
            "chg-label",
            "chg-mark",
            "chg-removed",
            "nav-removed",
            "Changes in this view",
            "toc-changes",
            "id=\"Removed-",
        ] {
            assert!(!h.contains(m), "{} holds {m}", p.display());
        }
    }
    assert!(html(out.path(), "index.html").contains("id=\"show-changes\""));
}

/// ls13 (visual review F4, rule 2.5): an edited doc's row says it once (its counts, no
/// "Edited" before "2 edited"), and each number stays on the line of its word.
#[test]
fn ls13_the_home_row_of_an_edited_doc_says_it_once_and_keeps_numbers_with_words() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "index.html");
    let list = h
        .split_once("<ul class=\"changes\">")
        .expect("the list")
        .1
        .split_once("</ul>")
        .unwrap()
        .0;
    let intro = list
        .split("<li>")
        .find(|li| li.contains(">Intro</a>"))
        .expect("the Intro row");
    assert!(!intro.contains(">Edited<"), "{intro}");
    assert!(
        intro.contains("<span class=\"changes-count\">2\u{a0}edited \u{b7} 1\u{a0}new \u{b7} 1\u{a0}removed</span>"),
        "{intro}"
    );
    // a new doc and a removed page keep their word
    assert!(
        list.contains(">New</span>") && list.contains(">Removed</span>"),
        "{list}"
    );
}

/// The marker of a changed row (D-NYCMF): a dot and a lowercase word, one element.
fn mark(word: &str) -> String {
    format!(
        "<span class=\"chg chg-mark chg-{word}\"><span class=\"chg-dot\" aria-hidden=\"true\"></span>{word}</span>"
    )
}

/// The Pages navigation of a page.
fn pages_nav(h: &str) -> &str {
    let a = h.find("<nav aria-label=\"Pages\">").expect("the Pages nav");
    let b = h[a..].find("</nav>").expect("the nav ends");
    &h[a..a + b + "</nav>".len()]
}

/// Three docs titled Guide at three depths, one calm doc, and a landed canon (the fixture of the
/// change summary's anchors). Returns the project and the canon write's result.
fn guides() -> (tempfile::TempDir, rdy_engine::Rdy, rdy_engine::WriteResult) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny();
    let guide = |id: &str, parent: Option<&str>| {
        let mut d = json!({"op": "create", "type": "doc", "id": id, "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}});
        if let Some(p) = parent {
            d["parent"] = p.into();
        }
        d
    };
    let prose = |id: &str, parent: &str, body: &str| json!({"op": "create", "type": "prose", "id": id, "parent": parent, "body": body});
    let canon = write(&mut r, "work", json!([
        guide("$g0", None),
        prose("$p0", "$g0", "Top text."),
        {"op": "create", "type": "folder", "id": "$one", "fields": {"name": "one", "title": "One"}},
        guide("$g1", Some("$one")),
        prose("$p1", "$g1", "First text."),
        {"op": "create", "type": "folder", "id": "$two", "fields": {"name": "two", "title": "Two"}},
        {"op": "create", "type": "folder", "id": "$in", "parent": "$two", "fields": {"name": "in", "title": "Inner"}},
        guide("$g2", Some("$in")),
        prose("$p2", "$g2", "Second text."),
        {"op": "create", "type": "doc", "id": "$calm", "fields": {"name": "calm", "title": "Calm", "doc_type": "overview"}},
        prose("$pc", "$calm", "Stays."),
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    (d, r, canon)
}

#[test]
fn rvm1_a_changed_doc_row_carries_one_marker_and_no_link() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "keep.html");
    let nav = pages_nav(&h);
    assert!(
        nav.contains(&format!(
            "<li class=\"nav-item\"><a href=\"intro.html\">Intro {}</a></li>",
            mark("edited")
        )),
        "{nav}"
    );
    assert!(
        nav.contains(&format!(
            "<li class=\"nav-item\"><a href=\"fresh.html\">Fresh {}</a></li>",
            mark("new")
        )),
        "{nav}"
    );
    for gone in ["chg-label", "<a class=\"chg", "changes.html"] {
        assert!(!nav.contains(gone), "the nav holds {gone}: {nav}");
    }
    let mut marks = 0;
    for m in nav.split("class=\"chg chg-mark").skip(1) {
        // the marker ends at its second closing span: the dot's, then its own
        let end = m
            .match_indices("</span>")
            .nth(1)
            .expect("the marker ends")
            .0;
        let element = &m[..end];
        assert!(!element.contains("<a"), "{element}");
        marks += 1;
    }
    assert_eq!(marks, 2, "{nav}");
}

#[test]
fn rvm1_only_the_current_row_is_highlighted() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "intro.html");
    let nav = pages_nav(&h);
    assert_eq!(nav.matches("is-active").count(), 1, "{nav}");
    assert!(
        nav.contains(
            "<li class=\"nav-item is-active\"><a href=\"intro.html\" aria-current=\"page\">Intro <span class=\"chg chg-mark chg-edited\">"
        ),
        "{nav}"
    );
    let text = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let css = rdy_qa::site::parse_css(&text);
    let at = |sel: &str, p: &str, w: u32| rdy_qa::site::cascaded(&css, sel, p, w);
    for w in [360, 1280] {
        let m = |p: &str| at(".chg-mark", p, w);
        assert_eq!(m("color").as_deref(), Some("var(--ink-soft)"), "{w}");
        assert_eq!(m("text-transform").as_deref(), Some("none"), "{w}");
        assert_eq!(m("white-space").as_deref(), Some("nowrap"), "{w}");
        assert_eq!(m("background"), None, "{w}");
        assert_eq!(
            at(".chg-dot", "background", w).as_deref(),
            Some("var(--accent)"),
            "{w}"
        );
        assert_eq!(
            at(".chg-dot", "border-radius", w).as_deref(),
            Some("50%"),
            "{w}"
        );
        for p in ["text-decoration", "color", "display", "user-select"] {
            assert_eq!(at("a.chg-label", p, w), None, "{w} {p}");
        }
    }
    assert!(!text.contains("a.chg-label"));
}

#[test]
fn rvm1_the_change_summary_gives_each_doc_its_own_anchor() {
    let (_d, mut r, canon) = guides();
    let _clock = fixture_clock();
    let id = |k: &str| canon.created[k].clone();
    write(&mut r, "w", json!([
        {"op": "update", "id": id("$p0"), "body": "Top text, edited."},
        {"op": "update", "id": id("$p1"), "body": "First text, edited."},
        {"op": "update", "id": id("$p2"), "body": "Second text, edited."},
        {"op": "create", "type": "doc", "id": "$nw", "fields": {"name": "fresh", "title": "Fresh", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$pn", "parent": "$nw", "body": "New text."},
    ]))
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let summary = html(out.path(), "changes.html");
    let anchors: Vec<String> = summary
        .split("<h2 id=\"")
        .skip(1)
        .map(|g| g[..g.find('"').unwrap()].to_string())
        .collect();
    assert_eq!(
        anchors
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        anchors.len(),
        "three docs titled Guide get three anchors: {anchors:?}"
    );
    assert_eq!(anchors.iter().filter(|a| a.starts_with("guide")).count(), 3);
}

#[test]
fn rvm1_folder_and_group_markers_sit_inside_their_row() {
    let (_d, mut r, canon) = guides();
    let _clock = fixture_clock();
    write(&mut r, "w", json!([
        {"op": "create", "type": "folder", "id": "$add", "fields": {"name": "added", "title": "Added"}},
        {"op": "create", "type": "doc", "id": "$a1", "parent": "$add", "fields": {"name": "a1", "title": "A1", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$pa", "parent": "$a1", "body": "A text."},
        {"op": "create", "type": "folder", "id": "$nest", "parent": canon.created["$two"].clone(), "fields": {"name": "nested", "title": "Nested"}},
        {"op": "create", "type": "doc", "id": "$n1", "parent": "$nest", "fields": {"name": "n1", "title": "N1", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$pn1", "parent": "$n1", "body": "N text."},
    ]))
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let h = html(out.path(), "two/in/guide.html");
    let nav = pages_nav(&h);
    assert!(
        nav.contains(&format!(
            "<span class=\"nav-group-label\">Added {}</span>",
            mark("new")
        )),
        "{nav}"
    );
    // the top-level folder has a page: its label and marker sit in the fold's summary
    assert!(
        nav.contains(&format!(
            "<summary><span class=\"nav-group-label\">Added {}</span></summary>",
            mark("new")
        )),
        "{nav}"
    );
    assert!(
        nav.contains(&format!("<summary>Nested {}</summary>", mark("new"))),
        "{nav}"
    );
}

#[test]
fn rvm1_the_not_found_page_keeps_markers_and_no_summary_link() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "404.html");
    let nav = pages_nav(&h);
    assert!(
        nav.contains(&format!("Intro {}</a>", mark("edited"))),
        "{nav}"
    );
    assert!(!nav.contains("changes.html#"), "{nav}");
}

#[test]
fn rvm1_canon_rows_keep_their_bytes() {
    let (_d, r, _ids) = review_project();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        for m in ["chg-mark", "chg-dot"] {
            assert!(!h.contains(m), "{} holds {m}", p.display());
        }
    }
    let h = html(out.path(), "keep.html");
    assert_eq!(
        pages_nav(&h),
        "<nav aria-label=\"Pages\"><ul class=\"nav-root\"><li class=\"nav-group\"><ul><li class=\"nav-item\"><a href=\"index.html\">Home</a></li><li class=\"nav-item\"><a href=\"intro.html\">Intro</a></li><li class=\"nav-item is-active\"><a href=\"keep.html\" aria-current=\"page\">Keep</a></li><li class=\"nav-item\"><a href=\"old.html\">Old guide</a></li></ul></li></ul></nav>"
    );
}
