//! Site wording of the version position, the pill, the colophon, the home page (ver1).
use rdy_engine::*;
use rdy_qa::golden::*;

#[test]
fn ver1_every_page_carries_the_site_worded_version_pill() {
    let (_d, mut r) = tiny_with_doc();
    let render = |r: &mut Rdy, s: &Scope| {
        let o = tempfile::tempdir().unwrap();
        rdy_render::render_site(r, s, o.path()).unwrap();
        o
    };
    let out = render(&mut r, &Scope::Canon);
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        if h.contains("redirect") {
            continue;
        }
        assert!(
            h.contains("data-rdy-version>unversioned</span>"),
            "{}",
            p.display()
        );
    }
    assert!(
        std::fs::read_to_string(out.path().join("index.html"))
            .unwrap()
            .contains("<p>No versions yet.</p>")
    );
    r.tag(&approver(), "v0.1.0", None, Some("first cut".into()))
        .unwrap();
    let out = render(&mut r, &Scope::Canon);
    let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        home.contains("data-rdy-version>v0.1.0</span>")
            && home.contains("<dt>v0.1.0 <span class=\"meta\">(current)</span></dt>")
            && home.contains(" · first cut</dd>"),
        "{home}"
    );
    assert!(!home.to_lowercase().contains("landing"));
    let doc = first_doc_id(&mut r);
    write(
        &mut r,
        "work",
        serde_json::json!([{"op":"update","id":doc,"fields":{"title":"Identity v2"}}]),
    )
    .unwrap();
    let out = render(&mut r, &Scope::View("work".into()));
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("data-rdy-version>v0.1.0 · draft: work, 1 pending change</span>"),
        "{h}"
    );
    let p = r.propose(&writer(), "work", "rename").unwrap();
    r.land(&approver(), &p.id).unwrap();
    let out = render(&mut r, &Scope::Canon);
    assert!(
        std::fs::read_to_string(out.path().join("index.html"))
            .unwrap()
            .contains("data-rdy-version>v0.1.0, 1 change since</span>")
    );
    let v: rdy_model::Hash = r.versions().unwrap()[0].landing.parse().unwrap();
    let out = render(&mut r, &Scope::At(v));
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("class=\"version frozen\"")
            && h.contains("v0.1.0 <span class=\"snapshot\">snapshot</span></span>")
            && h.contains("A snapshot of Tiny at v0.1.0."),
        "{h}"
    );
    assert_eq!(
        r.version_position(&Scope::Canon).unwrap().label,
        "v0.1.0 + 1 landing",
        "the CLI wording is unchanged"
    );
}

/// Review Focus 4.
#[test]
fn ver1_an_unversioned_project_reads_unversioned_everywhere() {
    let (_d, mut r) = tiny_with_doc();
    let doc = first_doc_id(&mut r);
    write(
        &mut r,
        "work",
        serde_json::json!([{"op":"update","id":doc,"fields":{"title":"Renamed"}}]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::View("work".into()), out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("data-rdy-version>unversioned · draft: work, 1 pending change</span>"),
        "{h}"
    );
    assert!(
        h.contains("<p class=\"colophon\">Tiny, unversioned · draft: work, 1 pending change.</p>")
    );
    assert!(!out.path().join("assets/js").exists() && !h.contains("glossary.html"));
}

#[test]
fn ver1_the_home_page_has_cards_for_top_level_entries_and_no_decisions() {
    let (_d, mut r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(h.contains("<div class=\"cards\">"));
    for (t, href) in [
        ("Intro", "intro.html"),
        ("Alpha", "a/index.html"),
        ("Beta", "b/index.html"),
        ("Glossary", "glossary.html"),
        ("Open questions", "questions.html"),
    ] {
        assert!(
            h.contains(&format!(
                "<a class=\"card\" href=\"{href}\"><strong>{t}</strong>"
            )),
            "{t}: {h}"
        );
    }
    assert!(!h.contains("Decisions"));
}
