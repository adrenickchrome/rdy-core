//! ls11: the disagreements page shows a repeated disagreement once, with no count (Q1), and folds a
//! long list of pages behind "Show all" (founder card 2026-10-13; FW-SW2, D-QWSTV).
use rdy_engine::{Rdy, Scope};
use rdy_render::render_site;

const SAME: &str = "These pages define the same thing twice.";

/// Seven top-level folders, each with one doc ("Doc 0") whose three claims read the same in
/// every copy: three duplicate-definition tensions, each over the same seven pages. Landed.
fn seven_copies() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    rdy_engine::testkit::generate_tree(&proj, 7, 1, 3);
    let mut r = Rdy::open(&proj).unwrap();
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    (dir, r)
}

/// The canon's disagreements page.
fn page(r: &Rdy) -> String {
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    std::fs::read_to_string(out.path().join("disagreements.html")).unwrap()
}

#[test]
fn ls11_a_repeated_disagreement_shows_once() {
    let (_d, r) = seven_copies();
    let p = page(&r);
    assert_eq!(
        p.matches(SAME).count(),
        1,
        "one entry for three repeats: {p}"
    );
    assert!(
        !p.contains("repeats") && !p.contains("times on these pages"),
        "no count (D-QWSTV): {p}"
    );
    for i in 0..7 {
        assert_eq!(
            p.matches(&format!("href=\"area-{i}/doc-0.html#")).count(),
            1,
            "area-{i} once: {p}"
        );
    }
}

#[test]
fn ls11_a_long_list_folds_behind_show_all() {
    let (_d, r) = seven_copies();
    let p = page(&r);
    // the content only: the navigation links the same folders, without an anchor
    let content = p
        .split_once("<h1>Where the docs disagree</h1>")
        .expect("the page")
        .1;
    let (shown, folded) = content
        .split_once("<details class=\"more-links\">")
        .expect("a fold");
    let folded = folded.split_once("</details>").expect("the fold ends").0;
    assert_eq!(
        shown.matches("/doc-0.html#").count(),
        5,
        "five links in view"
    );
    assert!(
        folded.starts_with("<summary>Show all</summary>"),
        "{folded}"
    );
    assert_eq!(
        folded.matches("/doc-0.html#").count(),
        2,
        "two behind the fold"
    );
}

#[test]
fn ls11_a_lone_disagreement_renders_as_before() {
    // guard: an entry that repeats nowhere and lists five pages or fewer is unchanged
    let (_d, r, _s) = rdy_qa::golden::retired_reference();
    let p = page(&r);
    assert!(
        !p.contains("class=\"repeats\"") && !p.contains("more-links"),
        "{p}"
    );
}

#[test]
fn ls11_the_grouped_page_passes_the_leak_scan() {
    // guard: the grouped page shows no ids
    let (_d, r) = seven_copies();
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    assert!(rdy_qa::site::leaks(out.path()).is_empty());
}
