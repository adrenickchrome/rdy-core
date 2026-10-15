//! sr6: the change summary pages and the inline diffs (D-W40KZ, D-BMX8P; RULING F6). Each `+N`
//! chip opens a diff summary: `changes.html` for a view or proposal, `changes-since.html` for the
//! landings since the tag. It lists the changed docs, then each changed block with its word diff;
//! every mark is also a word. A review page carries each edited block's diff, hidden until Show
//! diff turns it on.
use rdy_engine::{DiffChange, Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::{Mode, build_site};
use serde_json::json;
use std::path::Path;

const DEL: &str = "<del><span class=\"sr-word\">removed: </span>";
const INS: &str = "<ins><span class=\"sr-word\">added: </span>";

fn review() -> (tempfile::TempDir, Rdy, ReviewIds, tempfile::TempDir) {
    review_site(&Scope::View(REVIEW_VIEW.into()))
}

/// The review project with [`REVIEW_VIEW`] proposed, and the site of that proposal version.
fn proposal_site() -> (tempfile::TempDir, Rdy, tempfile::TempDir) {
    let (d, mut r, _ids) = review_project();
    let p = r
        .propose(&writer(), REVIEW_VIEW, "Rewrite the intro")
        .expect("propose the review view");
    let scope = r
        .read()
        .resolve_scope(&format!("proposal:{}", p.id))
        .expect("the proposal scope");
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &scope, out.path()).expect("render the proposal");
    (d, r, out)
}

/// The text between `open` and the next `close` after it.
fn between<'a>(h: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = h.find(open)? + open.len();
    let len = h[start..].find(close)?;
    Some(&h[start..start + len])
}

fn h1(h: &str) -> &str {
    between(h, "<h1>", "</h1>").expect("an h1")
}

/// The entries of a change summary page, each from its opening tag to its end.
fn entries(h: &str) -> Vec<&str> {
    h.match_indices("<article class=\"chg-entry")
        .map(|(i, _)| {
            let len = h[i..].find("</article>").expect("an entry ends");
            &h[i..i + len]
        })
        .collect()
}

/// The entry of block `id`.
fn entry_of<'a>(h: &'a str, id: &str) -> &'a str {
    let attr = format!("data-rdy-block=\"{id}\"");
    entries(h)
        .into_iter()
        .find(|e| e.contains(&attr))
        .unwrap_or_else(|| panic!("no entry for {id}: {h}"))
}

/// What an entry's diff paragraph holds.
fn diff_of(entry: &str) -> &str {
    between(entry, "<p class=\"chg-diff\" data-rdy=\"content\">", "</p>").expect("a diff paragraph")
}

/// The rows of the first change list (`ul.changes`) on a page: each title with the word or the
/// counts it shows.
fn list_rows(h: &str) -> Vec<(String, String)> {
    let list = between(h, "<ul class=\"changes\">", "</ul>").expect("a change list");
    list.split("<li>")
        .skip(1)
        .map(|li| {
            let title = between(li, "\">", "</").expect("a row title").to_string();
            let tail = li.rsplit_once("\">").map(|(_, t)| t).unwrap_or_default();
            let tail = tail.split("</").next().unwrap_or_default().to_string();
            (title, tail)
        })
        .collect()
}

/// The `+N` of the review chip in the header of a page, if it has one.
fn review_chip_count(h: &str) -> Option<usize> {
    let chip = between(h, "class=\"chip chip-review\"", "</")?;
    let n = chip.rsplit_once(">+")?.1;
    Some(n.parse().expect("a count"))
}

fn read(site: &Path, page: &str) -> String {
    html(site, page)
}

#[test]
fn sr6_a_review_site_has_its_change_summary() {
    let (_d, _r, _ids, out) = review();
    assert!(
        out.path().join("changes.html").exists(),
        "a review render writes its change summary"
    );
    let page = read(out.path(), "changes.html");
    assert_eq!(h1(&page), "Changes in this view");
    let rows = list_rows(&page);
    let titles: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(titles, ["Intro", "Fresh", "Old guide"], "{page}");
    // the same rows as the home page's list (O12)
    let home = read(out.path(), "index.html");
    assert_eq!(
        rows,
        list_rows(&home),
        "the home page and the summary agree"
    );
    // each title leads to its section on this page
    assert!(page.contains("<li><a href=\"#intro\">Intro</a>"), "{page}");
    assert!(
        page.contains("<h2 id=\"intro\"><a href=\"intro.html\">Intro</a></h2>"),
        "{page}"
    );
    assert!(
        page.contains("<h2 id=\"old-guide\">Old guide</h2>"),
        "{page}"
    );
    // the breadcrumbs: the site name, then the heading
    let crumbs = between(&page, "<nav class=\"crumbs\"", "</nav>").expect("crumbs");
    assert!(
        crumbs.contains(">Review</a>")
            && crumbs.contains("<span aria-current=\"page\">Changes in this view</span>"),
        "{crumbs}"
    );
    // ten changed blocks in three docs (REVIEW_BLOCKS in render_header.rs)
    assert!(
        page.contains("<p class=\"lede\">10 changes in 3 docs, not landed.</p>"),
        "{page}"
    );

    let (_d, _r, out) = proposal_site();
    let page = read(out.path(), "changes.html");
    assert_eq!(h1(&page), "Changes in this proposal");
    assert_eq!(list_rows(&page), list_rows(&read(out.path(), "index.html")));
}

#[test]
fn sr6_the_since_page_lists_the_changes_since_the_tag() {
    let (_d, r, _ids, out) = site_canon();
    let page = read(out.path(), "changes-since.html");
    assert_eq!(h1(&page), "Changes since v1.0.0");
    let st = r.read();
    let landing: rdy_model::Hash = st.versions().unwrap()[0].landing.parse().unwrap();
    let diff = st.diff(&Scope::At(landing), &Scope::Canon).unwrap();
    let m = st.materialized(&Scope::Canon).unwrap();
    let mut docs: Vec<String> = vec![];
    for e in &diff.entries {
        let visible = e.block_type != "decision" && e.block_type != "rationale";
        let something = e.change != DiffChange::Updated
            || e.body.is_some()
            || !e.fields.is_empty()
            || e.links.is_some()
            || e.type_change.is_some()
            || e.moved.is_some();
        if !(visible && something) {
            continue;
        }
        let doc = e.doc.as_ref().expect("a canon block in a doc");
        let title = m.get(doc).unwrap().field_str("title").unwrap().to_string();
        if !docs.contains(&title) {
            docs.push(title);
        }
    }
    assert!(
        !docs.is_empty(),
        "guard: canon landed changes after the tag"
    );
    let titles: Vec<String> = list_rows(&page).into_iter().map(|(t, _)| t).collect();
    assert_eq!(titles, docs, "{page}");
    assert!(page.contains(" since v1.0.0.</p>"), "{page}");

    let (_d, _r, _ids, out) = site_frozen();
    assert!(
        !out.path().join("changes-since.html").exists(),
        "a tag's own site has no landings since it"
    );
    assert!(!out.path().join("changes.html").exists());
}

#[test]
fn sr6_an_edited_block_shows_its_words() {
    let (_d, _r, ids, out) = review();
    let page = read(out.path(), "changes.html");
    let e = entry_of(&page, ids.edited.as_str());
    assert_eq!(diff_of(e), format!("{DEL}Old</del>{INS}New</ins> wording."));
    assert!(
        e.contains("<span class=\"chg-label chg-edited\">Edited</span>"),
        "{e}"
    );
    // where it is, and a link to it in place
    assert!(
        e.contains("<span class=\"chg-where\">Summary</span>"),
        "{e}"
    );
    assert!(e.contains(">Read it in place</a>"), "{e}");
    assert!(e.contains("href=\"intro.html#"), "{e}");
}

#[test]
fn sr6_new_and_removed_blocks_show_whole() {
    let (_d, _r, ids, out) = review();
    let page = read(out.path(), "changes.html");
    let added = entry_of(&page, ids.added.as_str());
    assert_eq!(diff_of(added), format!("{INS}Brand new paragraph.</ins>"));
    assert!(added.contains(">New</span>"), "{added}");
    let gone = entry_of(&page, ids.gone.as_str());
    assert_eq!(diff_of(gone), format!("{DEL}Gone soon.</del>"));
    assert!(gone.contains(">Removed</span>"), "{gone}");
    assert!(
        !gone.contains("Read it in place"),
        "a removed block is not in this site: {gone}"
    );
    // the removed doc's prose, and the removed doc itself
    let prose = entries(&page)
        .into_iter()
        .find(|e| e.contains("Old guide text."))
        .expect("the removed doc's prose");
    assert_eq!(diff_of(prose), format!("{DEL}Old guide text.</del>"));
    let doc = entry_of(&page, ids.old.as_str());
    assert_eq!(diff_of(doc), format!("{DEL}Old guide</del>"));
    assert!(
        doc.contains("<span class=\"chg-where\">Title</span>"),
        "{doc}"
    );
    let fresh = entry_of(&page, ids.fresh.as_str());
    assert_eq!(diff_of(fresh), format!("{INS}Fresh</ins>"));
}

#[test]
fn sr6_every_mark_is_also_a_word() {
    let (_d, _r, _ids, out) = review();
    let page = read(out.path(), "changes.html");
    let dels: Vec<_> = page.match_indices("<del>").collect();
    let inss: Vec<_> = page.match_indices("<ins>").collect();
    assert!(!dels.is_empty() && !inss.is_empty(), "guard: {page}");
    for (i, _) in dels {
        assert!(page[i..].starts_with(DEL), "{}", &page[i..]);
    }
    for (i, _) in inss {
        assert!(page[i..].starts_with(INS), "{}", &page[i..]);
    }
    let all = entries(&page);
    assert_eq!(all.len(), 10, "{page}");
    for e in all {
        let labelled = [("edited", "Edited"), ("new", "New"), ("removed", "Removed")]
            .iter()
            .any(|(mark, word)| {
                e.contains(&format!(
                    "<span class=\"chg-label chg-{mark}\">{word}</span>"
                ))
            });
        assert!(labelled, "an entry shows its word: {e}");
    }
    // the hidden words are visually hidden, never removed from the page
    let css = std::fs::read_to_string(out.path().join("assets/site.css")).unwrap();
    let rule = between(&css, ".sr-word {", "}").expect("a .sr-word rule");
    assert!(rule.contains("clip"), "{rule}");
    assert!(!rule.contains("display: none"), "{rule}");
    let del = between(&css, "\ndel {", "}").expect("a del rule");
    assert!(del.contains("line-through"), "{del}");
    let ins = between(&css, "\nins {", "}").expect("an ins rule");
    assert!(ins.contains("underline"), "{ins}");
}

#[test]
fn sr6_the_review_chip_count_is_the_entry_count() {
    let (_d, _r, _ids, out) = review();
    let n = entries(&read(out.path(), "changes.html")).len();
    let pages = pages(out.path());
    let mut seen = 0;
    for p in &pages {
        let h = std::fs::read_to_string(p).unwrap();
        if let Some(chip) = review_chip_count(&h) {
            assert_eq!(chip, n, "{}", p.display());
            seen += 1;
        }
    }
    assert!(seen >= 5, "guard: the review pages carry the chip ({seen})");

    let (_d, _r, out) = proposal_site();
    let n = entries(&read(out.path(), "changes.html")).len();
    assert!(n > 0, "guard: the proposal changes blocks");
    for p in pages_of(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        if let Some(chip) = review_chip_count(&h) {
            assert_eq!(chip, n, "{}", p.display());
        }
    }
}

fn pages_of(site: &Path) -> Vec<std::path::PathBuf> {
    pages(site)
        .into_iter()
        .filter(|p| !p.to_string_lossy().contains("/go/"))
        .collect()
}

#[test]
fn sr6_a_review_page_carries_hidden_inline_diffs() {
    let (_d, _r, _ids, out) = review();
    let intro = read(out.path(), "intro.html");
    let inline = "<div class=\"chg chg-diff-inline\" data-diff hidden>";
    assert_eq!(
        intro.matches(inline).count(),
        2,
        "the lede and the edited prose: {intro}"
    );
    assert!(
        intro.contains(&format!(
            "{inline}<p class=\"chg-diff\" data-rdy=\"content\">{DEL}Old</del>{INS}New</ins> wording.</p></div>"
        )),
        "{intro}"
    );
    // the edited block's own text is wrapped, so Show diff can hide it
    assert!(
        intro.contains("<div class=\"block-text\"><p>New wording.</p></div>"),
        "{intro}"
    );
    // a new block keeps its markup
    assert_eq!(
        intro.matches("<div class=\"block-text\">").count(),
        1,
        "{intro}"
    );

    let (_d, r, _ids) = review_project();
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    for (path, bytes) in &site.files {
        if path.ends_with(".html") {
            let h = String::from_utf8_lossy(bytes);
            assert!(!h.contains("data-diff"), "{path}");
            assert!(!h.contains("block-text"), "{path}");
        }
    }
}

#[test]
fn sr6_change_pages_are_not_in_search_or_llms() {
    let (_d, _r, _ids, review_out) = review();
    let (_d2, _r2, _ids2, canon_out) = site_canon();
    assert!(review_out.path().join("changes.html").exists(), "guard");
    assert!(
        canon_out.path().join("changes-since.html").exists(),
        "guard"
    );
    for out in [review_out.path(), canon_out.path()] {
        for file in ["search-index.json", "llms.txt", "llms-full.txt"] {
            let text = std::fs::read_to_string(out.join(file)).unwrap();
            for page in ["changes.html", "changes-since.html"] {
                assert!(!text.contains(page), "{file} names {page}");
            }
            assert!(!text.contains("Changes in this view"), "{file}");
            assert!(!text.contains("Changes since"), "{file}");
        }
        for p in pages(out) {
            let h = std::fs::read_to_string(&p).unwrap();
            if let Some(nav) = between(&h, "<nav aria-label=\"Pages\">", "</nav>") {
                assert!(!nav.contains("changes"), "{}: {nav}", p.display());
            }
        }
        assert!(!out.join("go/changes.html").exists());
    }
}

#[test]
fn sr6_a_doc_named_changes_keeps_its_page() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny_with_doc();
    write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "changes", "title": "Release notes", "doc_type": "overview"}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "What changed."}
        ]),
    )
    .unwrap();
    let scope = Scope::View("w".into());
    let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
    let page = String::from_utf8_lossy(&site.files["changes.html"]).to_string();
    assert!(
        page.contains("<h1>Release notes</h1>"),
        "the doc keeps its page: {page}"
    );
    assert!(!page.contains("Changes in this view"), "{page}");
    assert!(
        page.contains("<span class=\"chip chip-review\""),
        "the chip has no link: {page}"
    );
    assert!(!page.contains("class=\"chip chip-review\" href"), "{page}");
    assert!(
        site.summary.warnings.contains(
            &"the top-level doc named changes has the page changes.html: the change summary of this view or proposal is left out. Rename the doc to publish it."
                .to_string()
        ),
        "{:?}",
        site.summary.warnings
    );

    // the same for the summary of the landings since a tag
    let _clock = fixture_clock();
    let (_d, mut r) = tiny_with_doc();
    write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "doc", "fields": {"name": "changes-since", "title": "Since", "doc_type": "overview"}}]),
    )
    .unwrap();
    land(&mut r, "w").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "x",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "overview"}}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let page = String::from_utf8_lossy(&site.files["changes-since.html"]).to_string();
    assert!(page.contains("<h1>Since</h1>"), "{page}");
    assert!(page.contains("<span class=\"chip chip-since\""), "{page}");
    assert!(
        site.summary.warnings.contains(
            &"the top-level doc named changes-since has the page changes-since.html: the summary of changes since v1.0.0 is left out. Rename the doc to publish it."
                .to_string()
        ),
        "{:?}",
        site.summary.warnings
    );
}

/// A copy of the site at `from` in a new directory, with every `hidden` attribute removed from
/// its pages, so that the scan reads what Show diff and the buttons would show.
fn unhidden_copy(from: &Path) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    let mut stack = vec![from.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let to = out.path().join(p.strip_prefix(from).unwrap());
            if p.is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "html") {
                let h = std::fs::read_to_string(&p).unwrap();
                let h = h.replace(" hidden>", ">").replace(" hidden ", " ");
                std::fs::write(&to, h).unwrap();
            } else {
                std::fs::copy(&p, &to).unwrap();
            }
        }
    }
    out
}

#[test]
fn sr6_change_pages_pass_the_leak_scan() {
    let (_d, _r, _ids, review_out) = review();
    let (_d2, _r2, _ids2, canon_out) = site_canon();
    for (out, page) in [
        (review_out.path(), "changes.html"),
        (canon_out.path(), "changes-since.html"),
    ] {
        let copy = unhidden_copy(out);
        assert!(copy.path().join(page).exists(), "the scan reads {page}");
        let h = std::fs::read_to_string(copy.path().join("index.html")).unwrap();
        assert!(!h.contains(" hidden>"), "guard: the copy shows everything");
        let leaks: Vec<String> = rdy_qa::site::leaks(copy.path())
            .into_iter()
            .map(|l| format!("{} {} {:?}", l.page, l.pattern, l.text))
            .collect();
        assert!(leaks.is_empty(), "{leaks:#?}");
    }
}

/// Rule 1 of the summary: a counted block that the block diff has no entry for is read from the
/// two sides. A section carried into another doc takes its prose with it: the prose counts (a
/// removed line of its old doc) though its own parent did not change.
#[test]
fn sr6_a_block_carried_with_its_parent_is_an_entry() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    let made = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "Alpha", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$a", "fields": {"title": "Moving"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Carried along."},
            {"op": "create", "type": "section", "id": "$t", "parent": "$a", "fields": {"title": "Staying"}},
            {"op": "create", "type": "prose", "parent": "$t", "body": "Stays here."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "Beta", "doc_type": "overview"}},
            {"op": "create", "type": "section", "parent": "$b", "fields": {"title": "Home"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| made.created[k].clone();
    write(
        &mut r,
        "w",
        json!([{"op": "move", "id": id("$s"), "parent": id("$b")}]),
    )
    .unwrap();
    let st = r.read();
    let scope = Scope::View("w".into());
    let base = rdy_render::review::base_of(&st, &scope).unwrap();
    let diff = st.diff(&base, &scope).unwrap();
    assert!(
        diff.entries.iter().all(|e| e.id.to_string() != id("$p")),
        "guard: the block diff has no entry for the carried prose"
    );
    let rv = rdy_render::review::build(&st, &scope).unwrap().unwrap();
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    let page = String::from_utf8_lossy(&site.files["changes.html"]).to_string();
    let all = entries(&page);
    assert_eq!(all.len() as u32, rv.changed_blocks(), "{page}");
    let carried = entry_of(&page, &id("$p"));
    assert!(carried.contains(">Edited</span>"), "{carried}");
    assert_eq!(diff_of(carried), "Carried along.");
    assert!(carried.contains("href=\"b.html#"), "{carried}");
}

/// A proposal version made on the empty project, read once canon has landed: no scope names its
/// base, so its summary reads it against the empty project, and every block it holds is New.
#[test]
fn sr6_a_proposal_made_before_the_first_landing_lists_every_block_as_new() {
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
    let p = r.propose(&writer(), "w", "Initial proposal").unwrap();
    write(
        &mut r,
        "x",
        json!([{"op": "create", "type": "doc", "fields": {"name": "landed", "title": "Landed doc", "doc_type": "overview"}}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    let st = r.read();
    let scope = st.resolve_scope(&format!("proposal:{}", p.id)).unwrap();
    assert_eq!(
        rdy_render::review::base_of(&st, &scope),
        None,
        "guard: no scope names the empty base"
    );
    let site = build_site(&st, &scope, Mode::Static).unwrap();
    let page = String::from_utf8_lossy(&site.files["changes.html"]).to_string();
    assert_eq!(h1(&page), "Changes in this proposal");
    let all = entries(&page);
    assert_eq!(all.len(), 2, "{page}");
    assert_eq!(diff_of(all[0]), format!("{INS}Proposed doc</ins>"));
    assert_eq!(
        diff_of(all[1]),
        format!("{INS}Before anything lands.</ins>")
    );
    let home = String::from_utf8_lossy(&site.files["index.html"]).to_string();
    assert_eq!(review_chip_count(&home), Some(2));
    // its pages show no review marks, so they carry no inline diff
    let doc = String::from_utf8_lossy(&site.files["proposed.html"]).to_string();
    assert!(!doc.contains("data-diff"), "{doc}");
}
