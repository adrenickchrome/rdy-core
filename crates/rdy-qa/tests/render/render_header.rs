//! sr5: the header on every page (D-42YMF, D-BMX8P; RULING F3, F4): the pill shows only the
//! tag, a neutral chip counts the landings since it, an accent chip counts the blocks a view or
//! proposal changes; the colophon and the llms files keep the text label; the live site patches
//! the header in place.
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_render::live::{build_live, diff, page};
use rdy_render::{Mode, SlotKind, build_site, escape};
use serde_json::json;
use std::path::Path;

/// The header's markup on a written page: what the `data-rdy-version` element holds.
fn header_of(html: &str) -> &str {
    const OPEN: &str = "data-rdy-version>";
    let start = html.find(OPEN).expect("a version element") + OPEN.len();
    let len = html[start..]
        .find("</span>\n")
        .expect("the version element ends its line");
    &html[start..start + len]
}

fn read(site: &Path, page: &str) -> String {
    html(site, page)
}

/// The landings chip as the header writes it, linked under `root`.
fn since_chip(root: &str, n: u32, tag: &str) -> String {
    let hover = format!(
        "{n} {} since {tag}",
        if n == 1 { "landing" } else { "landings" }
    );
    format!(
        "<a class=\"chip chip-since\" href=\"{root}changes-since.html\" title=\"{hover}\" aria-label=\"{hover}\">+{n}</a>"
    )
}

/// The review chip as the header writes it, linked under `root`.
fn review_chip(root: &str, n: u32) -> String {
    let hover = format!(
        "{n} {}, not landed",
        if n == 1 { "change" } else { "changes" }
    );
    format!(
        "<a class=\"chip chip-review\" href=\"{root}changes.html\" title=\"{hover}\" aria-label=\"{hover}\">+{n}</a>"
    )
}

#[test]
fn sr5_the_header_shows_the_pill_and_chips() {
    let (_d, _r, _ids, out) = site_canon();
    for (page, root) in [("index.html", ""), ("tutorial/adventure.html", "../")] {
        let h = read(out.path(), page);
        let header = header_of(&h);
        assert_eq!(
            header,
            format!(
                "<a class=\"pill\" href=\"{root}index.html#versions\"><span class=\"pill-text\">v1.0.0</span></a> {}",
                since_chip(root, 2, "v1.0.0")
            ),
            "{page}"
        );
        assert!(!header.contains("chip-review"), "{page}: {header}");
    }
}

/// The review project's changed blocks, by rule O2 (worked out by hand): (a) the edited lede,
/// the edited prose and the new prose (the new decision is not visible): 3; (b) the section and
/// the prose of the new doc Fresh: 2; (c) the removed line `Gone soon.`: 1; (d) the section and
/// the prose of the removed doc Old guide: 2; (e) no doc or folder field changed: 0. Total 8.
const REVIEW_BLOCKS: u32 = 8;

#[test]
fn sr5_a_review_page_has_the_accent_chip_and_no_title_in_the_pill() {
    let scope = Scope::View(REVIEW_VIEW.into());
    let (_d, r, _ids, out) = review_site(&scope);
    let st = r.read();
    let rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a review");
    assert_eq!(rv.changed_blocks(), REVIEW_BLOCKS);
    let since = st.version_position(&scope).unwrap().landings_since;
    for (page, root) in [("index.html", ""), ("intro.html", "")] {
        let h = read(out.path(), page);
        let header = header_of(&h);
        assert!(
            header.starts_with(
                "<a class=\"pill\" href=\"index.html#versions\"><span class=\"pill-text\">v1.0.0</span></a>"
            ),
            "{page}: {header}"
        );
        assert_eq!(
            header.contains("chip-since"),
            since > 0,
            "{page}: a landings chip exactly when the view has landings since the tag: {header}"
        );
        if since > 0 {
            assert!(
                header.contains(&since_chip(root, since, "v1.0.0")),
                "{header}"
            );
        }
        assert!(
            header.ends_with(&review_chip(root, REVIEW_BLOCKS)),
            "{page}: {header}"
        );
        assert!(
            !header.contains(REVIEW_VIEW) && !header.to_lowercase().contains("draft"),
            "{page}: the title leaves the header: {header}"
        );
    }
}

#[test]
fn sr5_a_tag_page_has_the_lock_and_no_chip() {
    let (_d, _r, _ids, out) = site_frozen();
    for page in ["index.html", "tutorial/adventure.html"] {
        let h = read(out.path(), page);
        let header = header_of(&h);
        assert!(
            header.contains("<svg class=\"lock\"")
                && header.contains("<span class=\"pill-text\">v1.0.0</span>")
                && header.contains("<span class=\"snapshot\">snapshot</span>"),
            "{page}: {header}"
        );
        assert!(!header.contains("chip"), "{page}: {header}");
        assert!(h.contains("class=\"version frozen\""), "{page}");
    }
}

/// RULING F3: no tag, no landings chip.
#[test]
fn sr5_an_unversioned_project_has_no_since_chip() {
    let (_d, mut r) = tiny();
    for (n, name) in [(1, "one"), (2, "two")] {
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": format!("Doc {n}"), "doc_type": "overview"}}]),
        )
        .unwrap();
        land(&mut r, "work").unwrap();
    }
    assert_eq!(
        r.read().version_position(&Scope::Canon).unwrap().version,
        None,
        "guard: no tag"
    );
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    for page in ["index.html", "one.html"] {
        let h = read(out.path(), page);
        assert_eq!(
            header_of(&h),
            "<a class=\"pill\" href=\"index.html#versions\"><span class=\"pill-text\">unversioned</span></a>",
            "{page}"
        );
    }
}

#[test]
fn sr5_the_colophon_and_llms_keep_the_text_label() {
    let (_d, _r, _ids, out) = site_canon();
    assert!(
        read(out.path(), "index.html").contains(
            "<p class=\"colophon\">Structural pattern matching, <span data-rdy-label>v1.0.0, 2 changes since</span>.</p>"
        ),
        "{}",
        read(out.path(), "index.html")
    );
    let (_d, _r, _ids, review) = review_site(&Scope::View(REVIEW_VIEW.into()));
    let llms = read(review.path(), "llms.txt");
    let head = llms
        .lines()
        .find(|l| l.contains("v1.0.0"))
        .expect("a head line");
    assert!(
        head.contains(&format!(
            " · view: {REVIEW_VIEW}, {REVIEW_BLOCKS} changes, not landed"
        )),
        "O9: {head}"
    );
    let mut files = pages(review.path());
    files.push(review.path().join("llms.txt"));
    files.push(review.path().join("llms-full.txt"));
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains("draft:"), "{}", f.display());
    }
}

#[test]
fn sr5_a_live_header_is_one_fragment_for_every_page() {
    let (_d, r, _ids) = project();
    let b = build_live(&r.read(), &Scope::View("editing".into()), "/~view/editing/").unwrap();
    let mut seen = 0;
    for (path, slots) in &b.label_at {
        let headers: Vec<_> = slots
            .iter()
            .filter(|(k, _)| *k == SlotKind::Header)
            .collect();
        if !path.ends_with(".html") {
            assert!(headers.is_empty(), "{path}");
            continue;
        }
        assert_eq!(headers.len(), 1, "{path}: one header");
        assert_eq!(
            &b.files[path][headers[0].1.clone()],
            b.header.as_bytes(),
            "{path}"
        );
        seen += 1;
    }
    assert!(seen > 10, "guard: the pages hold the header ({seen})");
    assert!(
        b.header
            .starts_with("<a class=\"pill\" href=\"/~versions/\" data-menu aria-haspopup=\"dialog\" aria-expanded=\"false\">"),
        "{}",
        b.header
    );
    let hrefs: Vec<&str> = b
        .header
        .split("href=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(hrefs.len(), 3, "the pill and both chips: {}", b.header);
    for h in hrefs {
        assert!(
            h.starts_with("/~view/editing/") || h.starts_with("/~versions/"),
            "{h}"
        );
    }
}

/// Review Focus 3: a landing moves the landings chip on every page, and reloads only the pages
/// whose text changed.
#[test]
fn sr5_a_landing_patches_the_header_and_reloads_only_changed_pages() {
    let (_d, mut r, ids) = project();
    let before = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    assert!(
        before.header.contains(">+2</a>"),
        "guard: {}",
        before.header
    );
    let text = "Conditions that no pattern can express belong in a guard, and only there.";
    write(
        &mut r,
        "later",
        json!([{"op": "update", "id": ids.guards_prose.to_string(), "body": text}]),
    )
    .unwrap();
    land(&mut r, "later").unwrap();
    let after = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let edited: Vec<&String> = after
        .files
        .iter()
        // a page with a revision: a permalink page names its target's text, and never reloads
        .filter(|(p, b)| after.revs.contains_key(*p) && String::from_utf8_lossy(b).contains(text))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        edited.len(),
        1,
        "guard: one page shows the text: {edited:?}"
    );
    let d = diff(&before, &after);
    assert!(d.reload.contains(edited[0]), "{:?}", d.reload);
    for p in &d.reload {
        assert!(
            p == edited[0] || p == "index.html",
            "only the edited page and the home page reload: {:?}",
            d.reload
        );
    }
    assert_eq!(d.label.as_deref(), Some("v1.0.0, 3 changes since"));
    assert!(
        after.header.contains(&since_chip("/", 3, "v1.0.0")),
        "{}",
        after.header
    );
}

/// A doc's text can hold the header's text; only the recorded ranges are filled.
#[test]
fn sr5_the_header_slot_goes_only_into_its_ranges() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    let lookalike = "<span class=\"pill-text\">unversioned</span>";
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Part"}},
            {"op": "create", "type": "prose", "parent": "$s", "body": format!("The header reads {lookalike} here.")}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let mut b = build_live(&r.read(), &Scope::Canon, "/").unwrap();
    let old = b.header.clone();
    let base = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    assert!(
        base.contains(&escape(lookalike)),
        "guard: the doc holds the text"
    );
    b.header = "<a class=\"pill\" href=\"/~versions/\">v9 &lt;&gt;</a>".into();
    let html = String::from_utf8(page(&b, "alpha.html", "canon").unwrap()).unwrap();
    assert_eq!(html.matches(&b.header).count(), 1, "{html}");
    assert!(!html.contains(&old), "the old header is gone");
    assert!(
        html.contains(&format!("The header reads {} here.", escape(lookalike))),
        "the text in content is kept"
    );
    // nothing else moved: the page is the old one with the header swapped
    assert_eq!(html, base.replacen(&old, &b.header, 1));
}

#[test]
fn sr5_the_review_chip_counts_blocks_not_writes() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny_with_doc();
    let doc = first_doc_id(&r);
    let terms = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&doc))[0]
        .id
        .clone();
    let made = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$a", "parent": terms.to_string(), "body": "One."},
            {"op": "create", "type": "prose", "id": "$b", "parent": terms.to_string(), "body": "Two."},
            {"op": "create", "type": "prose", "id": "$c", "parent": terms.to_string(), "body": "Three."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| made.created[k].clone();
    write(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": id("$a"), "body": "One, edited."},
            {"op": "update", "id": id("$b"), "body": "Two, edited."}
        ]),
    )
    .unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": id("$c"), "body": "Three, edited."}]),
    )
    .unwrap();
    let st = r.read();
    let site = build_site(&st, &Scope::View("w".into()), Mode::Static).unwrap();
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert!(
        header_of(&home).ends_with(&review_chip("", 3)),
        "{}",
        header_of(&home)
    );
    assert!(
        site.label.ends_with(" · view: w, 3 changes, not landed"),
        "{}",
        site.label
    );
}

/// A static header is rendered per relative root; a live build writes one header.
#[test]
fn sr5_static_headers_follow_the_page_root() {
    let (_d, r, _ids) = project();
    let st = r.read();
    let site = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    assert_eq!(site.header, "", "a static build has no one header");
    let page = String::from_utf8_lossy(&site.files["tutorial/adventure.html"]).to_string();
    assert!(
        header_of(&page).contains("href=\"../index.html#versions\"")
            && header_of(&page).contains("href=\"../changes-since.html\""),
        "{}",
        header_of(&page)
    );
}
