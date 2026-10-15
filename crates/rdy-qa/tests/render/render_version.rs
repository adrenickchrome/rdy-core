//! Site wording of the version position, the pill, the colophon, the home page (ver1).
use rdy_engine::*;
use rdy_qa::golden::*;

#[test]
fn ver1_every_page_carries_the_site_worded_version_pill() {
    let (_d, mut r) = tiny_with_doc();
    let render = |r: &mut Rdy, s: &Scope| {
        let o = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), s, o.path()).unwrap();
        o
    };
    let out = render(&mut r, &Scope::Canon);
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        if h.contains("redirect") {
            continue;
        }
        // the pill shows only the version; no tag, no landings chip (F3)
        assert!(
            h.contains("data-rdy-version><a class=\"pill\" href=\"")
                && h.contains("<span class=\"pill-text\">unversioned</span></a></span>"),
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
        home.contains("data-rdy-version><a class=\"pill\" href=\"index.html#versions\"><span class=\"pill-text\">v0.1.0</span></a></span>")
            && home.contains("<dt>v0.1.0 <span class=\"meta\">(current)</span></dt>")
            && home.contains(" · first cut</dd>"),
        "{home}"
    );
    assert!(!home.to_lowercase().contains("landing"));
    let doc = first_doc_id(&r);
    write(
        &mut r,
        "work",
        serde_json::json!([{"op":"update","id":doc,"fields":{"title":"Identity v2"}}]),
    )
    .unwrap();
    let out = render(&mut r, &Scope::View("work".into()));
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    // a review render: the pill shows only the tag, the accent chip counts the changed blocks
    // (the doc's title: one), and the colophon keeps the text label (D-BMX8P, O9)
    assert!(
        h.contains("<span class=\"pill-text\">v0.1.0</span></a> <a class=\"chip chip-review\" href=\"changes.html\" title=\"1 change, not landed\" aria-label=\"1 change, not landed\">+1</a></span>")
            && h.contains("Tiny, <span data-rdy-label>v0.1.0 · view: work, 1 change, not landed</span>."),
        "{h}"
    );
    let p = r.propose(&writer(), "work", "rename").unwrap();
    r.land(&approver(), &p.id).unwrap();
    let out = render(&mut r, &Scope::Canon);
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("<span class=\"pill-text\">v0.1.0</span></a> <a class=\"chip chip-since\" href=\"changes-since.html\" title=\"1 landing since v0.1.0\" aria-label=\"1 landing since v0.1.0\">+1</a></span>")
            && h.contains("Tiny, <span data-rdy-label>v0.1.0, 1 change since</span>."),
        "{h}"
    );
    let v: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    let out = render(&mut r, &Scope::At(v));
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("class=\"version frozen\"")
            && h.contains(
                "<span class=\"pill-text\">v0.1.0</span> <span class=\"snapshot\">snapshot</span></a></span>"
            )
            && h.contains("A snapshot of Tiny at <span data-rdy-label>v0.1.0</span>."),
        "{h}"
    );
    assert_eq!(
        r.read().version_position(&Scope::Canon).unwrap().label,
        "v0.1.0 + 1 landing",
        "the CLI wording is unchanged"
    );
}

/// Review Focus 4.
#[test]
fn ver1_an_unversioned_project_reads_unversioned_everywhere() {
    let (_d, mut r) = tiny_with_doc();
    let doc = first_doc_id(&r);
    write(
        &mut r,
        "work",
        serde_json::json!([{"op":"update","id":doc,"fields":{"title":"Renamed"}}]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("data-rdy-version><a class=\"pill\" href=\"index.html#versions\"><span class=\"pill-text\">unversioned</span></a> <a class=\"chip chip-review\" href=\"changes.html\" title=\"1 change, not landed\" aria-label=\"1 change, not landed\">+1</a></span>"),
        "{h}"
    );
    assert!(
        h.contains("<p class=\"colophon\">Tiny, <span data-rdy-label>unversioned · view: work, 1 change, not landed</span>.</p>")
    );
    assert!(!out.path().join("assets/js").exists() && !h.contains("glossary.html"));
}

#[test]
fn ver1_the_home_page_has_cards_for_top_level_entries_and_no_decisions() {
    let (_d, r) = tree_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
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

/// FF-C: the pill shows at every width, so a frozen snapshot is marked at the top of a phone page
/// too (C7); below 900px it and its chips sit on the header's first row, between the brand and the
/// Menu button (sr7, D-42YMF), on one line that never wraps.
#[test]
fn ver1_the_pill_shows_at_every_width_and_sits_on_row_one_on_a_phone() {
    let (_d, _r, _ids, out) = site_canon();
    let text = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let css = rdy_qa::site::parse_css(&text);
    let at = |p: &str, w: u32| rdy_qa::site::cascaded(&css, ".version", p, w);
    for w in [320, 375, 600, 900, 901, 1024, 1440] {
        assert_eq!(
            at("display", w).as_deref(),
            Some("inline-flex"),
            "width {w}"
        );
        assert_eq!(at("flex-wrap", w).as_deref(), Some("nowrap"), "width {w}");
        for el in [".pill", ".chip"] {
            assert_eq!(
                rdy_qa::site::cascaded(&css, el, "display", w).as_deref(),
                Some("inline-flex"),
                "{el} at width {w}"
            );
        }
    }
    for w in [320, 375, 600, 900] {
        assert_eq!(at("grid-row", w).as_deref(), Some("1"), "width {w}");
    }
}
