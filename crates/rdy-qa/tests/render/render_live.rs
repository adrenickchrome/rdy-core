//! ls2: the live layer: revisions, the input key, the diff, and the bytes a page is served as.
use rdy_engine::{Rdy, Scope};
use rdy_render::live::*;
use rdy_render::{SlotKind, escape};
use serde_json::json;

#[test]
fn ls2_a_page_is_served_filled_with_the_script() {
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let html = String::from_utf8(page(&b, "index.html", "canon").unwrap()).unwrap();
    // the header and the label sit in exactly the recorded ranges, moved on by the script tag put
    // in the head
    let base = &b.files["index.html"];
    let shift = html.len() - base.len();
    let kinds: Vec<SlotKind> = b.label_at["index.html"].iter().map(|(k, _)| *k).collect();
    assert_eq!(kinds, vec![SlotKind::Header, SlotKind::Label]);
    for (kind, r) in &b.label_at["index.html"] {
        let want = match kind {
            SlotKind::Label => escape(&b.label),
            SlotKind::Header => b.header.clone(),
        };
        assert_eq!(&html[r.start + shift..r.end + shift], want);
    }
    assert!(html.contains(&format!("<span data-rdy-label>{}</span>", escape(&b.label))));
    let tag = format!(
        "<script src=\"/_rdy/live.js\" data-events=\"/_rdy/events?scope=canon&amp;page=index.html&amp;rev={}&amp;label={}\" defer></script></head>",
        b.revs["index.html"], b.label_rev
    );
    assert!(html.contains(&tag), "{html}");
    let llms = String::from_utf8(page(&b, "llms.txt", "canon").unwrap()).unwrap();
    assert!(llms.contains(&b.label));
    assert_eq!(
        page(&b, "assets/site.css", "canon").unwrap(),
        b.files["assets/site.css"]
    );
    assert!(page(&b, "no/such.html", "canon").is_none());
    let v = build_live(&r.read(), &Scope::View("editing".into()), "/~view/editing/").unwrap();
    let vhtml = String::from_utf8(page(&v, "index.html", "view:editing").unwrap()).unwrap();
    assert!(
        vhtml.contains("scope=view:editing&amp;page=index.html"),
        "{vhtml}"
    );
    assert!(
        vhtml.contains("Show changes"),
        "a view renders in review mode"
    );
}

#[test]
fn ls2_a_proposal_version_renders_in_review_mode() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_d, mut r) = rdy_qa::golden::tiny_with_doc();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "proposed-doc", "title": "Proposed doc", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "What the doc is for."}
        ]),
    )
    .unwrap();
    let p = r
        .propose(&rdy_qa::golden::writer(), "w", "Add a doc")
        .unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let b = build_live(&r.read(), &scope, &format!("/~proposal/{pref}/")).unwrap();
    // the new doc's paragraph is the one changed block (O2); "draft" is only the lifecycle state
    // (D-BMX8P, O9)
    assert!(
        b.label
            .ends_with("· proposal: Add a doc, 1 change, not landed"),
        "{}",
        b.label
    );
    assert!(
        !b.label.contains("P-"),
        "no proposal ref on the site: {}",
        b.label
    );
    assert!(
        b.header.contains(">+1</a>") && b.header.contains("1 change, not landed"),
        "{}",
        b.header
    );
    let home =
        String::from_utf8(page(&b, "index.html", &format!("proposal:{pref}")).unwrap()).unwrap();
    assert!(
        home.contains("Show changes") && home.contains("New"),
        "review mode"
    );
}

#[test]
fn ls2_a_body_edit_changes_only_its_page() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    edit_second_prose(&mut r, "work", "Alpha", "A new second paragraph.", true);
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert_eq!(
        d.reload.into_iter().collect::<Vec<_>>(),
        vec!["alpha.html".to_string()]
    );
    assert_eq!(d.label, None);
}

#[test]
fn ls2_a_tag_changes_the_label_and_the_home_page() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    r.tag(&rdy_qa::golden::approver(), "v1.0.0", None, None)
        .unwrap();
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert_eq!(d.label.as_deref(), Some("v1.0.0"));
    assert!(d.reload.contains("index.html"), "home lists the versions");
    assert!(!d.reload.contains("alpha.html") && !d.reload.contains("beta.html"));
}

type Step = Box<dyn Fn(&mut Rdy)>;

#[test]
fn ls2_an_unchanged_key_means_unchanged_pages() {
    // the key is the only reason the site worker builds; a write it misses would leave a page stale
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "draft", "Alpha", "Draft text.", false);
    // final review M8: a proposal version too; a landing moves canon's head under it
    edit_second_prose(&mut r, "prop", "Beta", "Proposed text.", false);
    let p = r
        .propose(&rdy_qa::golden::writer(), "prop", "Proposed")
        .unwrap();
    let proposal = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.r#ref))
        .unwrap();
    let scopes = [Scope::Canon, Scope::View("draft".into()), proposal];
    let mut keys: Vec<_> = scopes
        .iter()
        .map(|s| inputs_key(&r.read(), s, 0).unwrap())
        .collect();
    let mut sites: Vec<_> = scopes
        .iter()
        .map(|s| build_live(&r.read(), s, "/").unwrap())
        .collect();
    let steps: Vec<(&str, Step)> = vec![
        (
            "a commit to draft",
            Box::new(|r| edit_second_prose(r, "draft", "Beta", "Draft two.", false)),
        ),
        (
            "a commit to another view",
            Box::new(|r| {
                rdy_qa::golden::write(
                    r,
                    "other",
                    json!([
                        {"op": "create", "type": "doc", "fields": {"name": "elsewhere", "title": "Elsewhere", "doc_type": "overview"}}
                    ]),
                )
                .unwrap();
            }),
        ),
        (
            "a landing",
            Box::new(|r| edit_second_prose(r, "work", "Beta", "Landed text.", true)),
        ),
        (
            "a tag",
            Box::new(|r| {
                r.tag(&rdy_qa::golden::approver(), "v2.0.0", None, None)
                    .unwrap();
            }),
        ),
    ];
    for (what, step) in steps {
        step(&mut r);
        for (i, s) in scopes.iter().enumerate() {
            let k = inputs_key(&r.read(), s, 0).unwrap();
            let site = build_live(&r.read(), s, "/").unwrap();
            if k == keys[i] {
                assert!(
                    site.files == sites[i].files,
                    "{what}: {s:?}'s key held but its pages moved"
                );
            }
            keys[i] = k;
            sites[i] = site;
        }
    }
    assert_ne!(
        inputs_key(&r.read(), &Scope::Canon, 0).unwrap(),
        inputs_key(&r.read(), &Scope::Canon, 1).unwrap(),
        "pack generation"
    );
}

/// Final review M11: a proposal version based on canon's head is read against canon itself, as
/// a view is (no as-of build of the same state); once canon moves on, against canon at its base.
#[test]
fn ls2_a_proposal_at_canons_head_is_reviewed_against_canon() {
    use rdy_render::review::base_of;
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "prop", "Alpha", "Proposed.", false);
    let p = r
        .propose(&rdy_qa::golden::writer(), "prop", "Proposed")
        .unwrap();
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.r#ref))
        .unwrap();
    assert_eq!(base_of(&r.read(), &scope), Some(Scope::Canon));
    edit_second_prose(&mut r, "work", "Beta", "Landed.", true);
    assert!(
        matches!(base_of(&r.read(), &scope), Some(Scope::At(_))),
        "{:?}",
        base_of(&r.read(), &scope)
    );
}

#[test]
fn ls2_a_commit_to_another_view_keeps_canons_key() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    let canon = inputs_key(&r.read(), &Scope::Canon, 0).unwrap();
    edit_second_prose(&mut r, "draft", "Alpha", "Only in draft.", false);
    assert_eq!(inputs_key(&r.read(), &Scope::Canon, 0).unwrap(), canon);
}

#[test]
fn ls2_a_dismissal_moves_canons_key_and_its_pages() {
    // the disagreements list is an input of canon's key: a dismissal moves no head
    use rdy_engine::tensions::{TensionKind, tension_label};
    let (_d, mut r, _s) = rdy_qa::golden::retired_reference();
    let key = inputs_key(&r.read(), &Scope::Canon, 0).unwrap();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    assert!(
        before.files.contains_key("disagreements.html"),
        "guard: canon disagrees"
    );
    let t = r
        .read()
        .tensions()
        .unwrap()
        .iter()
        .find(|t| t.kind == TensionKind::DeprecatedReference)
        .cloned()
        .expect("guard: the canon holds the tension");
    r.dismiss_tension(
        &rdy_qa::golden::writer(),
        &tension_label(&t.id),
        "fine as it is",
    )
    .unwrap();
    assert_ne!(
        inputs_key(&r.read(), &Scope::Canon, 0).unwrap(),
        key,
        "a dismissal moves the key"
    );
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let d = diff(&before, &after);
    assert!(
        d.reload.contains("disagreements.html") && d.reload.contains("index.html"),
        "{:?}",
        d.reload
    );
}

/// `Tiny` with two landed top-level docs, Alpha and Beta, each with a section holding two prose
/// blocks.
fn two_docs() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = rdy_qa::golden::tiny();
    for t in ["Alpha", "Beta"] {
        rdy_qa::golden::write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": t.to_lowercase(), "title": t, "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} first.")},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("{t} second.")}
        ])).unwrap();
    }
    rdy_qa::golden::land(&mut r, "work").unwrap();
    (d, r)
}

/// Writes an edit of the second prose block of the doc titled `title` into `view`, and lands
/// the view when `land`.
fn edit_second_prose(r: &mut Rdy, view: &str, title: &str, body: &str, land: bool) {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let doc = m
        .iter()
        .find(|b| b.field_str("title") == Some(title))
        .unwrap()
        .id
        .clone();
    let sec = m.children(Some(&doc))[0].id.clone();
    let second = m.children(Some(&sec))[1].id.clone();
    rdy_qa::golden::write(
        r,
        view,
        json!([
            {"op": "update", "id": second.to_string(), "body": body}
        ]),
    )
    .unwrap();
    if land {
        rdy_qa::golden::land(r, view).unwrap();
    }
}

#[test]
fn ls2_a_permalink_page_has_no_revision_and_no_script() {
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, r) = two_docs();
    let b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let links: Vec<_> = b.files.keys().filter(|p| p.starts_with("go/")).collect();
    assert!(!links.is_empty(), "guard: the site has permalink pages");
    for p in links {
        assert!(!b.revs.contains_key(p), "{p}");
        assert_eq!(page(&b, p, "canon").unwrap(), b.files[p], "{p}");
    }
    assert!(b.files.keys().filter(|p| p.ends_with(".html")).count() > b.revs.len());
}

#[test]
fn ls2_a_new_label_goes_only_into_the_recorded_ranges() {
    // content can hold the label's text; only the places the build wrote it are changed
    let _clock = rdy_qa::golden::fixture_clock();
    let (_dir, mut r) = two_docs();
    edit_second_prose(&mut r, "work", "Alpha", "It reads unversioned, here.", true);
    let mut b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    assert!(b.label.contains("unversioned"), "guard: {}", b.label);
    let old = b.label.clone();
    b.label = "v9 <&>, 3 changes since".into();
    let html = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    // the colophon holds the label; the header holds the pill and its chips (sr5)
    assert_eq!(html.matches(&escape(&b.label)).count(), 1, "{html}");
    assert!(
        html.contains("It reads unversioned, here."),
        "the text in content is kept"
    );
    assert!(!html.contains(&format!("<span data-rdy-label>{old}</span>")));
    let llms = String::from_utf8(page(&b, "llms.txt", "canon").unwrap()).unwrap();
    assert!(llms.contains(&b.label), "the llms files take the label raw");
    // final review M4: the llms files take the label on one line, as the static build writes it
    // (a proposal title or a view name can hold a run of spaces or a line break)
    b.label = "Two  spaces\nand a newline".into();
    let html = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    assert_eq!(html.matches(&escape(&b.label)).count(), 1, "{html}");
    for f in ["llms.txt", "llms-full.txt"] {
        let text = String::from_utf8(page(&b, f, "canon").unwrap()).unwrap();
        assert!(text.contains("Two spaces and a newline"), "{f}: {text}");
        assert!(!text.contains("Two  spaces"), "{f}: {text}");
        assert!(!text.contains("\nand a newline"), "{f}: {text}");
    }
}
