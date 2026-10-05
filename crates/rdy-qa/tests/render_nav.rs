//! Left nav, breadcrumbs, toc, previous/next (nav1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

#[test]
fn nav1_groups_follow_top_level_folders_and_mark_the_current_page() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert!(
        nav.contains("<span class=\"nav-group-label\">Beta</span>")
            && nav.contains("<span class=\"nav-group-label\">Alpha</span>")
            && nav.contains("<span class=\"nav-group-label\">Reference</span>"),
        "{nav}"
    );
    assert!(nav.contains("aria-current=\"page\"") && nav.contains(">Three</a>"));
    assert!(
        nav.contains("href=\"../../index.html\">Overview</a>")
            && nav.contains("href=\"../../intro.html\">Intro</a>")
    );
    assert!(
        nav.contains("href=\"../../glossary.html\">Glossary</a>")
            && nav.contains("href=\"../../questions.html\">Open questions</a>")
    );
    assert!(
        nav.contains(">One</a>") && nav.contains(">Two</a>"),
        "a top-level folder always lists its direct children: {nav}"
    );
    assert_eq!(
        nav.matches("class=\"nav-sub\"").count(),
        1,
        "only Gamma, on the path, is expanded: {nav}"
    );
    let one = std::fs::read_to_string(out.path().join("a/one.html")).unwrap();
    let nav1 = &one[one.find("<nav aria-label=\"Pages\">").unwrap()..one.find("</aside>").unwrap()];
    assert!(
        nav1.contains(">Gamma</a>") && !nav1.contains(">Three</a>"),
        "Gamma is a bare entry off the path: {nav1}"
    );
}

#[test]
fn nav1_crumbs_start_with_the_project_name_and_end_with_the_page() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    let crumbs = &h[h.find("<nav class=\"crumbs\"").unwrap()..h.find("</nav>").unwrap()];
    let texts: Vec<&str> = crumbs
        .split('>')
        .filter_map(|s| s.split('<').next())
        .filter(|s| !s.trim().is_empty())
        .collect();
    assert_eq!(texts, ["Tree", "Beta", "Gamma", "Three"]);
    assert!(crumbs.contains("<span aria-current=\"page\">Three</span>"));
}

#[test]
fn nav1_the_toc_appears_at_three_sections_and_follows_depth() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let three = std::fs::read_to_string(out.path().join("b/c/three.html")).unwrap();
    assert!(
        three.contains("<nav aria-label=\"On this page\">")
            && three.contains("<p class=\"toc-label\">On this page</p>")
    );
    assert_eq!(three.matches("class=\"toc-link\"").count(), 5);
    let one = std::fs::read_to_string(out.path().join("a/one.html")).unwrap();
    assert!(
        !one.contains("aria-label=\"On this page\""),
        "one section: no toc"
    );
}

#[test]
fn nav1_previous_and_next_follow_tree_order_on_doc_pages_only() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let two = std::fs::read_to_string(out.path().join("a/two.html")).unwrap();
    assert!(
        two.contains("class=\"pager-prev\" href=\"one.html\"")
            && two.contains("<span class=\"pager-title\">One</span>")
    );
    assert!(two.contains("class=\"pager-next\" href=\"../b/c/three.html\""));
    let intro = std::fs::read_to_string(out.path().join("intro.html")).unwrap();
    assert!(
        intro.contains("class=\"pager-next\" href=\"a/one.html\"") && !intro.contains("pager-prev")
    );
    let folder = std::fs::read_to_string(out.path().join("a/index.html")).unwrap();
    assert!(!folder.contains("aria-label=\"Previous and next\""));
}

/// Review Focus 4.
#[test]
fn nav1_absent_collected_pages_are_not_linked() {
    let (_d, mut r) = tiny_with_doc();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        !h.contains("glossary.html") && !h.contains("questions.html") && !h.contains(">Reference<")
    );
}

#[test]
fn nav1_the_sidebar_caps_each_list_at_50_with_a_more_link() {
    let (_d, mut r) = wide_folder(120); // folder "many" with 120 docs
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("many/doc-60.html")).unwrap();
    let nav = &h[h.find("<nav aria-label=\"Pages\">").unwrap()..h.find("</aside>").unwrap()];
    assert_eq!(
        nav.matches("class=\"nav-item").count(),
        50 + 1,
        "50 siblings plus Overview; the folder is the group label"
    );
    assert!(
        nav.contains("class=\"more\"")
            && nav.contains(">+70 more</a>")
            && nav.contains("href=\"index.html\"")
    );
    assert!(nav.contains("aria-current=\"page\"") && nav.contains(">Doc 60</a>"));
}
