//! rfr1 (D-3VVNC): closed content is reader text. A collapsed section is a `<details>` that
//! starts closed, and the leak detector reads it as it reads any other part of the page: a closed
//! fold is no place to hide an id, a status word or a file name, and the heading in its summary
//! and the prose in its body stay visible text.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::{html, pages, tiny};
use rdy_qa::site::{Zone, leaks, visible_runs};
use serde_json::json;

#[test]
fn rfr1_closed_details_are_reader_text() {
    let (_dir, mut r) = tiny();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$i", "parent": "$g", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "parent": "$i", "body": "Lede words."},
            {"op": "create", "type": "section", "id": "$f", "parent": "$g", "fields": {"title": "Closed fold", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$f", "body": "Plain words in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary id r-7KQ2M in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary digest 0123456789abcdef0123 in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary file leak-canary.yaml in the fold."},
            {"op": "create", "type": "section", "id": "$d", "parent": "$f", "fields": {"title": "draft", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Words under a status word."},
            {"op": "create", "type": "section", "id": "$n", "parent": "$g", "fields": {"title": "Clean fold", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$n", "body": "Nothing to find here."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = html(out.path(), "guide.html");
    assert_eq!(
        page.matches("<details class=\"section-fold\">").count(),
        3,
        "three closed folds"
    );

    // the detector reads a closed fold as content
    let found: Vec<(String, &str)> = leaks(out.path())
        .into_iter()
        .filter(|l| l.page == "guide.html")
        .map(|l| (l.text, l.pattern))
        .collect();
    for (text, pattern) in [
        ("r-7KQ2M", "F1"),
        ("0123456789abcdef0123", "F2"),
        ("leak-canary.yaml", "F6"),
    ] {
        assert!(
            found.iter().any(|(t, p)| t.contains(text) && *p == pattern),
            "{pattern} {text} in a closed fold: {found:?}"
        );
    }
    // a status word as a fold's heading is a label the detector reads
    assert!(
        found.iter().any(|(t, p)| t == "draft" && *p == "F8"),
        "{found:?}"
    );
    // and the clean fold adds none of them
    assert!(
        !found
            .iter()
            .any(|(t, _)| t.contains("Nothing") || t.contains("Clean")),
        "{found:?}"
    );

    // the legitimate words stay visible text: the summary's heading and the body, in the content
    let runs = visible_runs(&page);
    for text in [
        "Closed fold",
        "Plain words in the fold.",
        "Clean fold",
        "Nothing to find here.",
    ] {
        let run = runs
            .iter()
            .find(|r| r.text == text)
            .unwrap_or_else(|| panic!("{text}"));
        assert_eq!(run.zone, Zone::Content, "{text}");
    }
    // the heading in a summary is a label; the body words are running text
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap();
    assert!(by("Closed fold").label);
    assert!(by("Closed fold").path.contains(&"summary".to_string()));
    assert!(!by("Plain words in the fold.").label);
    assert!(!pages(out.path()).is_empty());
}
