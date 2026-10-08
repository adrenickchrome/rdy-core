//! DX4: changed blocks carry a labelled bar, a removed block is a collapsed Removed line, and the
//! machine files carry the draft's text without markers (rv1; FW-RV2).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use serde_json::json;

fn draft() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    ReviewIds,
    tempfile::TempDir,
) {
    review_site(&Scope::View(REVIEW_VIEW.into()))
}

/// The page's article, where a doc's own text sits.
fn article(h: &str) -> &str {
    let start = h.find("<article").expect("an article");
    &h[start..start + h[start..].find("</article>").expect("a closed article")]
}

/// The markup from the opening tag of the block that holds `text` up to `text`, where it is read
/// as the block's content (a block's `data-ref` attribute quotes its text before the content).
fn block_with<'a>(h: &'a str, text: &str) -> &'a str {
    let at = h
        .match_indices(text)
        .map(|(i, _)| i)
        .find(|&i| h[..i].rfind('<') < h[..i].rfind('>'))
        .unwrap_or_else(|| panic!("{text} is not on the page"));
    let start = h[..at]
        .rfind("<div class=\"block ")
        .unwrap_or_else(|| panic!("{text} is in no block"));
    &h[start..at]
}

const EDITED: &str = "<span class=\"chg chg-label chg-edited\">Edited</span>";
const NEW: &str = "<span class=\"chg chg-label chg-new\">New</span>";
const REMOVED: &str =
    "<details class=\"chg chg-removed\"><summary>Removed</summary><div class=\"chg-old\">";

#[test]
fn rv1_a_draft_page_bars_edited_and_new_blocks_and_marks_the_edited_lede() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "intro.html");
    let a = article(&h);
    let edited = block_with(a, "New wording.");
    assert!(
        edited.starts_with("<div class=\"block block-prose has-chg chg-edited\""),
        "{edited}"
    );
    assert!(edited.contains(EDITED), "{edited}");
    let added = block_with(a, "Brand new paragraph.");
    assert!(
        added.starts_with("<div class=\"block block-prose has-chg chg-new\""),
        "{added}"
    );
    assert!(added.contains(NEW), "{added}");
    for same in ["Kept as it was.", "Stays after."] {
        let b = block_with(a, same);
        assert!(b.starts_with("<div class=\"block block-prose\" "), "{b}");
        assert!(!b.contains("chg"), "{b}");
    }
    assert!(a.contains(&format!("{EDITED}<p class=\"lede has-chg chg-edited\"")));
    assert!(a.contains("Where to begin.") && !a.contains("Where to start."));
    // the doc's own block did not change: its header carries no marker
    assert!(a.contains("<header class=\"page-head\">"));
    assert_eq!(a.matches("chg-label").count(), 3, "lede, edited, new");
}

#[test]
fn rv1_a_removed_block_is_a_collapsed_removed_line_where_it_was() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "intro.html");
    let a = article(&h);
    assert_eq!(a.matches(REMOVED).count(), 1);
    let at = a.find(REMOVED).unwrap();
    let line = &a[at..at + a[at..].find("</details>").unwrap()];
    assert!(line.contains("Gone soon."), "{line}");
    assert!(
        !line.contains(" id=\"") && !line.contains("data-rdy-block"),
        "{line}"
    );
    // it sits after the block before it and before the block that stays after it
    assert!(a.find("New wording.").unwrap() < at);
    assert!(at + line.len() < a.find(block_with(a, "Stays after.")).unwrap());
}

#[test]
fn rv1_a_new_doc_page_is_marked_new_once() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "fresh.html");
    let a = article(&h);
    assert!(
        a.contains("<header class=\"page-head has-chg chg-new\">"),
        "{a}"
    );
    assert!(a.contains(&format!("</h1>{NEW}")), "{a}");
    assert_eq!(
        a.matches("has-chg").count(),
        1,
        "the blocks carry no bar of their own"
    );
    assert_eq!(a.matches("chg-label").count(), 1);
    assert!(a.contains("Fresh text."));
}

#[test]
fn rv1_an_unchanged_doc_page_carries_no_marker() {
    let (_d, _r, _ids, out) = draft();
    let a = article(&html(out.path(), "keep.html")).to_string();
    assert!(!a.contains("chg"), "{a}");
    assert!(!out.path().join("old.html").exists());
}

#[test]
fn rv1_a_removed_blocks_old_text_is_escaped_and_carries_no_id_or_heading() {
    let _clock = fixture_clock();
    let (_d, mut r) = rdy_qa::fixtures::project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "page", "title": "Page", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Stays."},
        {"op": "create", "type": "prose", "id": "$evil", "parent": "$s", "body": format!("{EVIL} and {{{{$p}}}}")},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances are derived."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "cut",
        json!([
            {"op": "delete", "id": res.created["$evil"]},
            {"op": "delete", "id": res.created["$t"]}
        ]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("cut".into()), out.path()).unwrap();
    let h = html(out.path(), "page.html");
    let lines: Vec<&str> = h
        .match_indices(REMOVED)
        .map(|(i, _)| &h[i..i + h[i..].find("</details>").unwrap()])
        .collect();
    assert_eq!(lines.len(), 2, "{h}");
    let heading = regex::Regex::new(r"<h[1-6][\s>]").unwrap();
    for line in &lines {
        assert!(!line.contains(" id=\""), "{line}");
        assert!(!heading.is_match(line), "{line}");
        assert!(
            !line.contains("<script>") && !line.contains("<img"),
            "{line}"
        );
    }
    assert!(
        lines[0].contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("Ledger") && lines[1].contains("Balances are derived."),
        "{}",
        lines[1]
    );
}

#[test]
fn rv1_llms_and_search_carry_the_drafts_text_without_markers_or_removed_text() {
    let (_d, _r, _ids, out) = draft();
    let read = |f: &str| std::fs::read_to_string(out.path().join(f)).unwrap();
    for f in ["llms.txt", "llms-full.txt", "search-index.json"] {
        let t = read(f);
        for gone in [
            "Edited",
            "Removed",
            "Show changes",
            "Changes in this draft",
            "Gone soon",
            "Old guide",
            "Old wording",
            "Where to start",
        ] {
            assert!(!t.contains(gone), "{f} holds {gone}");
        }
    }
    let full = read("llms-full.txt");
    for kept in [
        "Where to begin.",
        "New wording.",
        "Brand new paragraph.",
        "Stays after.",
        "Fresh text.",
    ] {
        assert!(full.contains(kept), "llms-full.txt lacks {kept}");
    }
    assert!(read("search-index.json").contains("Brand new paragraph"));
}
