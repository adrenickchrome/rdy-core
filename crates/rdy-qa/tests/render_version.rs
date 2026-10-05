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
    let v: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    let out = render(&mut r, &Scope::At(v));
    let h = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    assert!(
        h.contains("class=\"version frozen\"")
            && h.contains("v0.1.0 <span class=\"snapshot\">snapshot</span></span>")
            && h.contains("A snapshot of Tiny at v0.1.0."),
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
    let doc = first_doc_id(&mut r);
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

/// Whether a block's `@media` prelude applies at a viewport `w` px wide (only the `min-width` and
/// `max-width` forms the stylesheet uses).
fn media_applies(media: &Option<String>, w: u32) -> bool {
    let Some(m) = media else { return true };
    let m = m.replace(' ', "");
    let re = regex::Regex::new(r"\((min|max)-width:(\d+)px\)").unwrap();
    let conds: Vec<_> = re.captures_iter(&m).collect();
    if conds.is_empty() {
        return false; // prefers-color-scheme, print and the like: not a width rule
    }
    conds.iter().all(|c| {
        let n: u32 = c[2].parse().unwrap();
        if &c[1] == "min" { w >= n } else { w <= n }
    })
}

/// The pill is hidden on a phone (Task 5 keeps the head grid narrow): the version section's
/// `display` must not win the cascade back below 900px. Resolves the cascade by source order for a
/// bare `.version` selector at each width.
#[test]
fn ver1_the_pill_stays_hidden_below_900px_and_shows_above() {
    let (_d, _r, _ids, out) = site_canon();
    let text = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let css = rdy_qa::site::parse_css(&text);
    let display_at = |w: u32| -> Option<String> {
        let mut d = None;
        for b in &css.blocks {
            if b.selector.split(',').any(|s| s.trim() == ".version") && media_applies(&b.media, w) {
                for (p, v) in &b.decls {
                    if p == "display" {
                        d = Some(v.clone());
                    }
                }
            }
        }
        d
    };
    for w in [320, 375, 600, 900] {
        assert_eq!(display_at(w).as_deref(), Some("none"), "width {w}");
    }
    for w in [901, 1024, 1440] {
        assert_ne!(display_at(w).as_deref(), Some("none"), "width {w}");
    }
}
