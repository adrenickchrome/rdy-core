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
    assert_eq!(next("Intro").as_deref(), Some("Edited"), "{nav:?}");
    assert_eq!(next("Fresh").as_deref(), Some("New"), "{nav:?}");
    assert!(
        !matches!(next("Keep").as_deref(), Some("Edited" | "New")),
        "{nav:?}"
    );
    assert_eq!(next("Removed").as_deref(), Some("Old guide"), "{nav:?}");
    assert!(h.contains("<li class=\"nav-item nav-removed\">Old guide</li>"));
    assert!(!h.contains("old.html"));
    assert!(h.contains("<li class=\"nav-group chg\"><span class=\"nav-group-label\">Removed</span><ul><li class=\"nav-item nav-removed\">Old guide</li>"));
    // Edited leads to the doc's group on the change summary; the doc's own link stays
    assert!(h.contains(
        ">Intro</a><a class=\"chg chg-label chg-edited\" href=\"changes.html#intro\">Edited</a>"
    ));
    // New and Removed keep their forms
    assert!(h.contains(">Fresh</a><span class=\"chg chg-label chg-new\">New</span>"));
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
            "chg-removed",
            "nav-removed",
            "Changes in this view",
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

/// The target of `href` as a site-relative path, seen from the page at `from`.
fn resolve(from: &str, href: &str) -> String {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for p in href.split('/') {
        match p {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The `(doc link, Edited link)` pairs of the sidebar of a page: each Edited word that follows
/// a doc's link.
fn edited_links(h: &str) -> Vec<(String, Option<String>)> {
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    nav.split("<li class=\"nav-item")
        .skip(1)
        .filter(|li| li.contains("chg-edited"))
        .map(|li| {
            let doc = li.split("<a href=\"").nth(1).unwrap();
            let doc = doc[..doc.find('"').unwrap()].to_string();
            let word = &li[li.find("chg chg-label chg-edited\"").unwrap()..];
            let link = word
                .contains("href=\"")
                .then(|| word.split("href=\"").nth(1).unwrap())
                .filter(|_| word.find("href=\"").unwrap() < word.find(">Edited<").unwrap())
                .map(|l| l[..l.find('"').unwrap()].to_string());
            (doc, link)
        })
        .collect()
}

/// Task 8 of the renderer plan: the word Edited in the sidebar leads to the doc's group on the
/// change summary, exactly as the summary itself names it, with duplicate titles and nested docs,
/// and leads nowhere when an authored doc owns `changes.html`.
#[test]
fn dgm8_edited_marker_links_to_its_summary() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
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
    let id = |k: &str| canon.created[k].clone();
    write(&mut r, "w", json!([
        {"op": "update", "id": id("$p0"), "body": "Top text, edited."},
        {"op": "update", "id": id("$p1"), "body": "First text, edited."},
        {"op": "update", "id": id("$p2"), "body": "Second text, edited."},
        {"op": "create", "type": "doc", "id": "$nw", "fields": {"name": "fresh", "title": "Fresh", "doc_type": "overview"}},
        prose("$pn", "$nw", "New text."),
    ]))
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let summary = html(out.path(), "changes.html");
    let groups: Vec<(String, String)> = summary
        .split("<h2 id=\"")
        .skip(1)
        .map(|g| {
            let anchor = g[..g.find('"').unwrap()].to_string();
            let href = g.split("<a href=\"").nth(1).unwrap();
            (anchor, href[..href.find('"').unwrap()].to_string())
        })
        .collect();
    let anchors: Vec<&String> = groups.iter().map(|g| &g.0).collect();
    assert_eq!(
        anchors
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        anchors.len(),
        "three docs titled Guide get three anchors: {anchors:?}"
    );
    assert_eq!(anchors.iter().filter(|a| a.starts_with("guide")).count(), 3);
    // every page's sidebar: each Edited word leads to the group of the doc it follows
    let mut seen = 0;
    for page in [
        "index.html",
        "guide.html",
        "one/guide.html",
        "two/in/guide.html",
        "calm.html",
        "fresh.html",
    ] {
        let h = html(out.path(), page);
        for (doc, link) in edited_links(&h) {
            let link = link.unwrap_or_else(|| panic!("{page}: Edited after {doc} has no link"));
            let (to, anchor) = link.split_once('#').expect("an anchor");
            assert_eq!(resolve(page, to), "changes.html", "{page}: {link}");
            let group = groups
                .iter()
                .find(|g| g.0 == anchor)
                .unwrap_or_else(|| panic!("{page}: no group {anchor}"));
            assert_eq!(
                resolve("changes.html", &group.1),
                resolve(page, &doc),
                "{page}: {link} leads to another doc's group"
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 3 * 6, "three Edited docs in each of six sidebars");
    // the doc's content link stays beside it, from a nested page too
    let nested = html(out.path(), "two/in/guide.html");
    assert!(
        nested.contains("href=\"guide.html\" aria-current=\"page\">Guide</a><a class=\"chg chg-label chg-edited\" href=\"../../changes.html#guide-"),
        "{nested}"
    );
    // New is a word and Calm has none
    assert!(nested.contains(">Fresh</a><span class=\"chg chg-label chg-new\">New</span>"));
    assert!(!nested.contains("chg-edited\" href=\"../../changes.html#calm"));

    // an authored top-level doc named changes owns the page: no summary, so no link
    write(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$cd", "fields": {"name": "changes", "title": "Release notes", "doc_type": "overview"}},
        prose("$pcd", "$cd", "What changed."),
    ]))
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    assert!(html(out.path(), "changes.html").contains("<h1>Release notes</h1>"));
    let page = html(out.path(), "one/guide.html");
    assert_eq!(edited_links(&page).len(), 3, "{page}");
    for (doc, link) in edited_links(&page) {
        assert_eq!(link, None, "Edited after {doc} leads nowhere");
    }
    assert!(
        page.contains("chg chg-label chg-edited\">Edited</span>"),
        "{page}"
    );
    assert!(!page.contains("changes.html#"), "{page}");
    // the authored doc is an ordinary entry, and a New one
    assert!(
        page.contains(
            "changes.html\">Release notes</a><span class=\"chg chg-label chg-new\">New</span>"
        ),
        "{page}"
    );
}
