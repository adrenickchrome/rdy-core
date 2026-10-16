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

/// The review project's changed blocks, by rule O2 and the task-5 ruling (worked out by hand):
/// (a) the edited lede, the edited prose and the new prose (the new decision is not visible): 3;
/// (b) the section and the prose of the new doc Fresh: 2; (c) the removed line `Gone soon.`: 1;
/// (d) the section and the prose of the removed doc Old guide: 2; (e) the added doc Fresh and
/// the removed doc Old guide: 2 (no doc or folder moved or changed a field). Total 10.
const REVIEW_BLOCKS: u32 = 10;

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
            "<p class=\"colophon\">Structural pattern matching, <span data-rdy-label>v1.0.0, 2 updates since</span>.</p>"
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
    // the summary of the landings since the tag lists the new landing's change (sr6)
    assert!(
        d.reload.iter().any(|p| p == "changes-since.html"),
        "{:?}",
        d.reload
    );
    for p in &d.reload {
        assert!(
            p == edited[0] || p == "index.html" || p == "changes-since.html",
            "only the edited page, the home page and the since summary reload: {:?}",
            d.reload
        );
    }
    assert_eq!(d.label.as_deref(), Some("v1.0.0, 3 updates since"));
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

/// The ruling on task 5 (D-ZPAVR, D-42YMF, D-W40KZ): a doc or folder that a view adds or removes
/// is one changed block, as any other added or removed block. A view that only adds an empty doc
/// has changed what a reader sees, so its page never says "no changes yet".
#[test]
fn sr5_an_added_empty_doc_is_a_change() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny_with_doc();
    write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "doc", "fields": {"name": "empty", "title": "Empty", "doc_type": "overview"}}]),
    )
    .unwrap();
    let st = r.read();
    let scope = Scope::View("w".into());
    let rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a review");
    assert_eq!(rv.changed_blocks(), 1);
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    assert!(
        site.files.contains_key("empty.html"),
        "guard: the doc has a page"
    );
    assert!(
        site.label.ends_with(" · view: w, 1 change, not landed"),
        "{}",
        site.label
    );
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert!(
        header_of(&home).ends_with(&review_chip("", 1)),
        "{}",
        header_of(&home)
    );
    // the change list names the new doc
    assert!(
        home.contains(
            "<li><a href=\"empty.html\">Empty</a> <span class=\"chg-label chg-new\">New</span></li>"
        ),
        "{home}"
    );
}

/// The ruling on task 5: a removed folder counts one, each doc removed with it counts one, and
/// each block of those docs counts as before.
#[test]
fn sr5_a_removed_folder_counts_the_folder_and_its_docs() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny_with_doc();
    let made = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "guides", "title": "Guides"}},
            {"op": "create", "type": "doc", "id": "$g", "parent": "$f", "fields": {"name": "setup", "title": "Setup", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$g", "body": "How to set up."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| made.created[k].clone();
    write(
        &mut r,
        "w",
        json!([
            {"op": "delete", "id": id("$p")},
            {"op": "delete", "id": id("$g")},
            {"op": "delete", "id": id("$f")}
        ]),
    )
    .unwrap();
    let st = r.read();
    let scope = Scope::View("w".into());
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    // the folder, the doc and its prose
    assert!(
        site.label.ends_with(" · view: w, 3 changes, not landed"),
        "{}",
        site.label
    );
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert!(
        header_of(&home).ends_with(&review_chip("", 3)),
        "{}",
        header_of(&home)
    );
    assert!(
        home.contains(
            "<li><span class=\"changes-gone\">Guides</span> <span class=\"chg-label chg-removed\">Removed</span></li>"
        ),
        "{home}"
    );
}

/// Two landed root docs, `First` then `Last`: (dir, rdy, first, last).
fn two_root_docs() -> (tempfile::TempDir, rdy_engine::Rdy, String, String) {
    let (d, mut r) = tiny();
    let made = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "first", "title": "First", "doc_type": "overview"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "last", "title": "Last", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let (a, b) = (made.created["$a"].clone(), made.created["$b"].clone());
    (d, r, a, b)
}

/// The root docs of `scope`, in order, and `id`'s order key there.
fn roots_and_key(r: &rdy_engine::Rdy, scope: &Scope, id: &str) -> (Vec<String>, String) {
    let m = r.read().materialized(scope).unwrap();
    let roots = m.children(None).iter().map(|b| b.id.to_string()).collect();
    let key = m
        .get(&rdy_model::BlockId::parse(id).unwrap())
        .unwrap()
        .order
        .clone();
    (roots, key)
}

/// The ruling on task 5 (Task 4's rule): a doc counts as moved only when its own structural
/// place changed. The already-last doc appended to its own parent gets a new order key and keeps
/// its place: no change, no chip, no mark.
#[test]
fn sr5_a_doc_given_a_new_key_at_the_same_place_is_no_change() {
    let _clock = fixture_clock();
    let (_d, mut r, a, b) = two_root_docs();
    write(
        &mut r,
        "w",
        json!([{"op": "move", "id": b, "parent": null}]),
    )
    .unwrap();
    let scope = Scope::View("w".into());
    let (canon_roots, old_key) = roots_and_key(&r, &Scope::Canon, &b);
    let (view_roots, new_key) = roots_and_key(&r, &scope, &b);
    assert_ne!(old_key, new_key, "guard: the append gives a new order key");
    assert_eq!(canon_roots, view_roots, "guard: the place is the same");
    assert_eq!(view_roots, vec![a.clone(), b.clone()], "guard");
    let st = r.read();
    let rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a review");
    assert_eq!(rv.changed_blocks(), 0);
    let id = rdy_model::BlockId::parse(&b).unwrap();
    assert_eq!(rv.page_mark(&id), None, "no mark on the doc");
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    assert!(
        site.label.ends_with(" · view: w, no changes yet"),
        "{}",
        site.label
    );
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert!(
        !header_of(&home).contains("chip-review"),
        "{}",
        header_of(&home)
    );
}

/// The ruling on task 5 (Task 4's rule 1): a custom field named `order` is an ordinary field. A
/// doc whose custom `order` alone changes has not moved, and a doc counts only for its place or
/// a field a reader sees (`title`, `name`, `description`).
#[test]
fn sr5_a_custom_order_field_on_a_doc_is_no_move() {
    const PACK: &str = "name: o\nversion: '1'\nextends: rdy-standard\nblock_types:\n  doc:\n    prefix: d\n    traits: [structural]\n    body: forbidden\n    fields:\n      order: { kind: str }\n    children: [section, prose]\n";
    let _clock = fixture_clock();
    let (d, r) = tiny();
    // the store is locked while a handle is open
    drop(r);
    rdy_qa::packs::use_pack(d.path(), "o", PACK, "name: Ordered\n");
    let mut r = rdy_engine::Rdy::open(d.path()).unwrap();
    let made = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "only", "title": "Only", "doc_type": "overview", "order": "old"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let doc = made.created["$d"].clone();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": doc, "fields": {"order": "new"}}]),
    )
    .unwrap();
    let scope = Scope::View("w".into());
    let (canon_roots, old_key) = roots_and_key(&r, &Scope::Canon, &doc);
    let (view_roots, new_key) = roots_and_key(&r, &scope, &doc);
    assert_eq!(
        (canon_roots, old_key),
        (view_roots, new_key),
        "guard: no structural change"
    );
    let st = r.read();
    let rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a review");
    assert_eq!(rv.changed_blocks(), 0);
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    assert!(
        site.label.ends_with(" · view: w, no changes yet"),
        "{}",
        site.label
    );
}

/// The ruling on task 5: a doc that really moves among its siblings counts one. The doc moved in
/// front counts; the doc it passes keeps its key and its place among the others, so it does not.
#[test]
fn sr5_a_reordered_doc_counts_one() {
    let _clock = fixture_clock();
    let (_d, mut r, a, b) = two_root_docs();
    write(
        &mut r,
        "w",
        json!([{"op": "move", "id": b, "parent": null, "before": a}]),
    )
    .unwrap();
    let scope = Scope::View("w".into());
    let (view_roots, _) = roots_and_key(&r, &scope, &b);
    assert_eq!(view_roots, vec![b.clone(), a.clone()], "guard: the reorder");
    let st = r.read();
    let rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a review");
    assert_eq!(rv.changed_blocks(), 1);
    let id = |s: &str| rdy_model::BlockId::parse(s).unwrap();
    assert_eq!(
        rv.page_mark(&id(&b)),
        Some(rdy_render::review::Mark::Edited)
    );
    assert_eq!(rv.page_mark(&id(&a)), None, "the passed doc did not move");
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    assert!(
        site.label.ends_with(" · view: w, 1 change, not landed"),
        "{}",
        site.label
    );
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert!(
        header_of(&home).ends_with(&review_chip("", 1)),
        "{}",
        header_of(&home)
    );
}

/// A proposal version made before the first landing is based on the empty project. After an
/// unrelated first landing no scope names that base, and the page shows no review marks (the
/// task-8 ruling), but the version keeps its name and its count: its blocks are counted against
/// the empty base it was made on, never as zero.
#[test]
fn sr5_a_pre_landing_proposal_keeps_its_name_and_count_after_a_first_landing() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "proposed", "title": "Proposed doc", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Before anything lands."}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "Initial proposal", None).unwrap();
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .unwrap();
    // the doc and its prose
    let want = " · proposal: Initial proposal, 2 changes, not landed";
    let before = build_site(&r.read(), &scope, Mode::Static).unwrap();
    assert!(before.label.ends_with(want), "{}", before.label);
    write(
        &mut r,
        "x",
        json!([{"op": "create", "type": "doc", "fields": {"name": "landed", "title": "Landed doc", "doc_type": "overview"}}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    assert_eq!(
        rdy_render::review::base_of(&r.read(), &scope),
        None,
        "guard: no scope names the empty base"
    );
    let after = build_site(&r.read(), &scope, Mode::Static).unwrap();
    assert_eq!(after.label, before.label);
    let page = String::from_utf8_lossy(&after.files["proposed.html"]).to_string();
    assert!(
        header_of(&page).ends_with(&review_chip("", 2)),
        "{}",
        header_of(&page)
    );
    assert!(!page.contains("chg-label"), "no review marks: {page}");
}

/// A tag name can be long (sr2 bounds it, not the header). Its pill shrinks and cuts its text
/// with an ellipsis; the chips keep their size; the row stays one line inside the page at every
/// phone width, with Show changes beside it or not (site quality bar §2.7).
#[test]
fn sr5_a_long_tag_shrinks_the_pill_and_keeps_the_chips() {
    let (_d, _r, out) = long_tag_view();
    let home = read(out.path(), "index.html");
    assert_eq!(
        header_of(&home),
        format!(
            "<a class=\"pill\" href=\"index.html#versions\"><span class=\"pill-text\">{LONG_TAG}</span></a> {} {}",
            since_chip("", 1, LONG_TAG),
            review_chip("", 1)
        ),
        "guard: the pill and both chips"
    );
    let text = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let css = rdy_qa::site::parse_css(&text);
    let at = |sel: &str, p: &str, w: u32| rdy_qa::site::cascaded(&css, sel, p, w);
    for w in [320, 360, 375, 600, 900, 901, 1024, 1440] {
        // the row is bounded by its grid area, and may shrink below its text
        assert_eq!(at(".version", "min-width", w).as_deref(), Some("0"), "{w}");
        assert_eq!(
            at(".version", "max-width", w).as_deref(),
            Some("100%"),
            "{w}"
        );
        // the pill gives way; its text is cut with an ellipsis, never overflows
        assert_eq!(at(".pill", "flex", w).as_deref(), Some("0 1 auto"), "{w}");
        for (p, v) in [
            ("min-width", "0"),
            ("overflow", "hidden"),
            ("text-overflow", "ellipsis"),
            ("white-space", "nowrap"),
        ] {
            assert_eq!(
                at(".pill-text", p, w).as_deref(),
                Some(v),
                ".pill-text {p} at {w}"
            );
        }
        // the chips never shrink
        assert_eq!(at(".chip", "flex", w).as_deref(), Some("none"), "{w}");
    }
    // a phone keeps the 40 px floor; a wide screen lets the pill shrink to its text's cut
    for w in [320, 360, 900] {
        assert_eq!(at(".pill", "min-width", w).as_deref(), Some("40px"), "{w}");
    }
    for w in [901, 1440] {
        assert_eq!(at(".pill", "min-width", w).as_deref(), Some("0"), "{w}");
    }
}

/// A tag 48 characters long.
const LONG_TAG: &str = "v1.0.0-release-candidate-feature-preview-october";

/// `Tiny` with doc Identity, tagged [`LONG_TAG`], then one landing that renames the doc, then a
/// view `w` that renames it again; the site of `w`.
fn long_tag_view() -> (tempfile::TempDir, rdy_engine::Rdy, tempfile::TempDir) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny_with_doc();
    r.tag(&approver(), LONG_TAG, None, None).unwrap();
    let doc = first_doc_id(&r).to_string();
    write(
        &mut r,
        "later",
        json!([{"op": "update", "id": doc, "fields": {"title": "Identity, landed"}}]),
    )
    .unwrap();
    land(&mut r, "later").unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": doc, "fields": {"title": "Identity, in review"}}]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    (d, r, out)
}
