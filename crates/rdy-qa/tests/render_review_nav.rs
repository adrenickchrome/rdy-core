//! DX4: the navigation marks changed docs and lists removed ones, the home page lists what the
//! draft changes, and a draft render stays leak-clean with no needs-you queue (rv1; FW-RV2,
//! FW-RV3).
use rdy_engine::{DecisionInput, Scope};
use rdy_qa::golden::*;
use rdy_qa::site::{leaks, visible_runs};

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
    assert_eq!(next("Intro").as_deref(), Some("Edited"), "{nav:?}");
    assert_eq!(next("Fresh").as_deref(), Some("New"), "{nav:?}");
    assert!(
        !matches!(next("Keep").as_deref(), Some("Edited" | "New")),
        "{nav:?}"
    );
    assert_eq!(next("Removed").as_deref(), Some("Old guide"), "{nav:?}");
    assert!(h.contains("<li class=\"nav-item nav-removed\">Old guide</li>"));
    assert!(!h.contains("old.html"));
    assert!(h.contains(">Intro</a><span class=\"chg chg-label chg-edited\">Edited</span>"));
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
    let list = "Changes in this draft | Intro | Edited | 2 edited \u{b7} 1 new \u{b7} 1 removed | Fresh | New | Old guide | Removed";
    assert!(joined.contains(list), "{joined}");
    assert!(joined.find(list).unwrap() < joined.find("Contents").unwrap());
    assert!(h.contains("<section class=\"chg\"><h2 id=\"draft-changes\">Changes in this draft</h2><ul class=\"changes\">"));
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
        let h = std::fs::read_to_string(&p).unwrap();
        for m in [
            "has-chg",
            "chg-label",
            "chg-removed",
            "nav-removed",
            "Changes in this draft",
        ] {
            assert!(!h.contains(m), "{} holds {m}", p.display());
        }
    }
    assert!(html(out.path(), "index.html").contains("id=\"show-changes\""));
}
