//! D-NYCMF: the page index of a review render marks each section that holds a change, and a
//! "Changes on this page" group heads it, one entry per change in page order (rvm2).
use rdy_engine::Scope;
use rdy_qa::golden::*;
use rdy_qa::site::{cascaded, parse_css, visible_runs};
use serde_json::json;

fn draft() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    ReviewIds,
    tempfile::TempDir,
) {
    review_site(&Scope::View(REVIEW_VIEW.into()))
}

/// The marker of a changed entry (D-NYCMF): a dot and a lowercase word, one element.
fn mark(word: &str) -> String {
    format!(
        "<span class=\"chg chg-mark chg-{word}\"><span class=\"chg-dot\" aria-hidden=\"true\"></span>{word}</span>"
    )
}

/// The page's index aside, from its opening tag to its end.
fn index(h: &str) -> &str {
    let a = h.find("<aside class=\"index").expect("the index");
    let b = h[a..].find("</aside>").expect("the index ends");
    &h[a..a + b + "</aside>".len()]
}

const GROUP: &str = "<nav class=\"chg toc-changes\" aria-label=\"Changes on this page\">";

/// The "Changes on this page" group of a page.
fn group(h: &str) -> &str {
    let a = h.find(GROUP).expect("the changes group");
    let b = h[a..].find("</nav>").expect("the group ends");
    &h[a..a + b + "</nav>".len()]
}

/// One entry of the group: its word, its label (as visible text) and its href.
#[derive(Debug)]
struct Entry {
    word: String,
    label: String,
    href: String,
    html: String,
}

fn entries(h: &str) -> Vec<Entry> {
    const OPEN: &str = "<li><a class=\"toc-link toc-chg\" href=\"";
    group(h)
        .split(OPEN)
        .skip(1)
        .map(|li| {
            let li = &li[..li.find("</li>").expect("the entry ends")];
            let href = li[..li.find('"').unwrap()].to_string();
            let runs: Vec<String> = visible_runs(&format!("{OPEN}{li}</li>"))
                .into_iter()
                .map(|r| r.text)
                .collect();
            assert_eq!(runs.len(), 2, "{li}");
            Entry {
                word: runs[0].clone(),
                label: runs[1].clone(),
                href,
                html: li.to_string(),
            }
        })
        .collect()
}

fn words(h: &str) -> Vec<String> {
    entries(h).into_iter().map(|e| e.word).collect()
}

/// The canon doc `guide` (its lede, then Alpha, Beta > Beta one, Gamma and Delta), landed, and
/// the view `w` that deletes one prose, edits two and adds two; then the view's site.
fn guide_site() -> (tempfile::TempDir, rdy_engine::Rdy, tempfile::TempDir) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny();
    let prose = |id: &str, parent: &str, body: &str| json!({"op": "create", "type": "prose", "id": id, "parent": parent, "body": body});
    let section = |id: &str, parent: &str, title: &str| json!({"op": "create", "type": "section", "id": id, "parent": parent, "fields": {"title": title}});
    let canon = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
        prose("$open", "$g", "Opening."),
        section("$alpha", "$g", "Alpha"),
        prose("$a1", "$alpha", "A one."),
        prose("$a2", "$alpha", "A two."),
        section("$beta", "$g", "Beta"),
        prose("$b1", "$beta", "B one."),
        section("$beta1", "$beta", "Beta one"),
        prose("$b11", "$beta1", "B eleven."),
        section("$gamma", "$g", "Gamma"),
        prose("$g1", "$gamma", "G one."),
        section("$delta", "$g", "Delta"),
        prose("$d1", "$delta", "D one."),
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| canon.created[k].clone();
    write(&mut r, "w", json!([
        {"op": "delete", "id": id("$a2")},
        {"op": "update", "id": id("$b11"), "body": "B eleven, edited."},
        {"op": "create", "type": "prose", "id": "$g2", "parent": id("$gamma"), "body": "G two."},
        {"op": "update", "id": id("$d1"), "body": "D one, edited."},
        {"op": "create", "type": "prose", "id": "$d2", "parent": id("$delta"), "body": "D two."},
    ]))
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    (d, r, out)
}

#[test]
fn rvm2_the_changes_group_lists_each_change_in_page_order() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "intro.html");
    assert!(h.contains("<aside class=\"index chg\">"), "{}", index(&h));
    let ix = index(&h);
    assert!(!ix.contains("<nav aria-label=\"On this page\">"), "{ix}");
    let got: Vec<(String, String)> = entries(&h).into_iter().map(|e| (e.word, e.label)).collect();
    let want = [
        ("edited", "Where to begin."),
        ("edited", "New wording."),
        ("removed", "Gone soon."),
        ("new", "Brand new paragraph."),
    ]
    .map(|(w, l)| (w.to_string(), l.to_string()));
    assert_eq!(got, want);
    assert_eq!(ix.find("<nav"), ix.find(GROUP), "{ix}");
}

/// Every element of `h` whose `id` is `id`, as the offset of its opening tag; an `id-anchor` span
/// (every block has one) is not counted.
fn with_id(h: &str, id: &str) -> Vec<usize> {
    h.match_indices(&format!(" id=\"{id}\""))
        .map(|(i, _)| h[..i].rfind('<').unwrap())
        .filter(|&s| {
            let tag = &h[s..s + h[s..].find('>').unwrap()];
            !tag.contains("class=\"id-anchor\"")
        })
        .collect()
}

fn text(h: &str) -> String {
    visible_runs(h)
        .into_iter()
        .map(|r| r.text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn check_jumps(h: &str, title: &str) {
    let article_end = h.find("</article>").expect("an article");
    let all = entries(h);
    assert!(!all.is_empty());
    for e in &all {
        let id = e.href.strip_prefix('#').expect("a fragment");
        let at = with_id(h, id);
        assert_eq!(at.len(), 1, "{id}: {at:?}");
        let at = at[0];
        let label = e.label.trim_end_matches('…');
        if id == "main" {
            let h1 = &h[h.find("<h1>").unwrap()..h.find("</h1>").unwrap()];
            assert!(text(h1).contains(label), "{h1} {label}");
            assert_eq!(e.label, title);
            continue;
        }
        let range = if id.starts_with("Removed-") {
            assert!(
                h[at..].starts_with(&format!("<details class=\"chg chg-removed\" id=\"{id}\">")),
                "{}",
                &h[at..at + 80]
            );
            &h[at..at + h[at..].find("</details>").unwrap()]
        } else {
            let open_end = at + h[at..].find('>').unwrap();
            let end = h[open_end..]
                .find("data-rdy-block=")
                .map_or(article_end, |i| open_end + i)
                .min(article_end);
            &h[at..end]
        };
        assert!(
            text(range).contains(label),
            "{id}: {label} is not in {range}"
        );
    }
}

#[test]
fn rvm2_each_entry_jumps_to_its_change() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "intro.html");
    check_jumps(&h, "Intro");
    let all = entries(&h);
    let removed: Vec<&Entry> = all.iter().filter(|e| e.word == "removed").collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].href, "#Removed-1");
    assert!(h.contains("<details class=\"chg chg-removed\" id=\"Removed-1\">"));
    let (_d, _r, out) = guide_site();
    check_jumps(&html(out.path(), "guide.html"), "Guide");
}

#[test]
fn rvm2_a_new_doc_lists_itself_once() {
    let (_d, _r, _ids, out) = draft();
    let h = html(out.path(), "fresh.html");
    let all = entries(&h);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        (
            all[0].word.as_str(),
            all[0].label.as_str(),
            all[0].href.as_str()
        ),
        ("new", "Fresh", "#main")
    );
}

#[test]
fn rvm2_section_entries_carry_the_word_of_their_changes() {
    let (_d, _r, out) = guide_site();
    let h = html(out.path(), "guide.html");
    assert!(h.contains("<aside class=\"index\">"), "{}", index(&h));
    let ix = index(&h);
    for (anchor, label, word) in [
        ("alpha", "Alpha", "removed"),
        ("beta", "Beta", "edited"),
        ("beta-one", "Beta one", "edited"),
        ("gamma", "Gamma", "new"),
        ("delta", "Delta", "edited"),
    ] {
        let want = format!(
            "<a class=\"toc-link\" href=\"#{anchor}\">{label} {}</a>",
            mark(word)
        );
        assert!(ix.contains(&want), "{want} is not in {ix}");
    }
    assert_eq!(words(&h), ["removed", "edited", "new", "edited", "new"]);
}

#[test]
fn rvm2_a_lede_from_the_first_section_marks_no_section() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    let prose = |id: &str, parent: &str, body: &str| json!({"op": "create", "type": "prose", "id": id, "parent": parent, "body": body});
    let section = |id: &str, title: &str| json!({"op": "create", "type": "section", "id": id, "parent": "$l", "fields": {"title": title}});
    let canon = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lead", "title": "Lead", "doc_type": "overview"}},
        section("$one", "One"),
        prose("$lt", "$one", "Lead text."),
        prose("$om", "$one", "One more."),
        section("$two", "Two"),
        prose("$t1", "$two", "T one."),
        section("$three", "Three"),
        prose("$r1", "$three", "R one."),
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": canon.created["$lt"].clone(), "body": "Lead text, edited."}]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let h = html(out.path(), "lead.html");
    let ix = index(&h);
    for anchor in ["one", "two", "three"] {
        let at = ix
            .find(&format!("<a class=\"toc-link\" href=\"#{anchor}\">"))
            .unwrap_or_else(|| panic!("no entry #{anchor} in {ix}"));
        let a = &ix[at..at + ix[at..].find("</a>").unwrap()];
        assert!(!a.contains("chg-mark"), "{a}");
    }
    let all = entries(&h);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        (all[0].word.as_str(), all[0].label.as_str()),
        ("edited", "Lead text, edited.")
    );
    // the lede's own element: the edited lede is a `lede-block` with its block's anchor
    let lede = h.find("class=\"lede-block").expect("the edited lede");
    let tag = &h[lede..lede + h[lede..].find('>').unwrap()];
    let id = tag
        .split(" id=\"")
        .nth(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .expect("the lede has an anchor");
    assert_eq!(all[0].href, format!("#{id}"));
}

#[test]
fn rvm2_an_entry_label_is_escaped_and_cut() {
    let (_d, mut r, ids) = review_project();
    let _clock = fixture_clock();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": ids.summary.to_string(), "fields": {"title": "<b>Bold</b> & \"quoted\" heading that runs past the forty characters"}}]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let h = html(out.path(), "intro.html");
    let g = group(&h);
    assert!(!g.contains("<b>"), "{g}");
    let all = entries(&h);
    let e = all
        .iter()
        .find(|e| e.html.contains("&lt;b&gt;Bold&lt;/b&gt;"))
        .unwrap_or_else(|| panic!("no escaped entry in {g}"));
    assert!(e.html.ends_with("…</a>"), "{}", e.html);
    assert!(e.label.chars().count() <= 41, "{}", e.label);
}

/// Ruling task-2: a change's label keeps a code span as `<code>`, as the block, the change
/// summary and the home card show it (ruling final-round1), cut at 40 characters as `home::cut`
/// cuts it: a span the cut passes stays whole; a span the cut falls in ends there.
#[test]
fn rvm2_a_label_keeps_its_code_spans_as_code() {
    let _clock = fixture_clock();
    let (_d, mut r) = tiny();
    let made = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Reading"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Open `before.json` first, then read every other file in the folder."},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$s", "body": "Run `cargo test -p rdy-render --lib rvm1_ now` today."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": made.created["$p"], "body": "Open `after.json` first, then read every other file in the folder."},
            {"op": "update", "id": made.created["$q"], "body": "Run `cargo test -p rdy-render --lib rvm2_ now` today."}
        ]),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::View("w".into()), out.path()).unwrap();
    let h = html(out.path(), "guide.html");
    let g = group(&h);
    let edited = mark("edited");
    for want in [
        format!("\">{edited} Open <code>after.json</code> first, then read every…</a></li>"),
        format!("\">{edited} Run <code>cargo test -p rdy-render --lib rvm2_</code>…</a></li>"),
    ] {
        assert!(g.contains(&want), "{want}\n{g}");
    }
}

#[test]
fn rvm2_show_changes_hides_the_markers_and_without_script_they_show() {
    let (_d, _r, _ids, out) = draft();
    let intro = html(out.path(), "intro.html");
    assert!(intro.contains(GROUP) && index(&intro).contains("chg-mark"));
    let (_g, _gr, gout) = guide_site();
    let guide = html(gout.path(), "guide.html");
    let hidden = regex::Regex::new(r"<[^>]*\shidden[\s>=/]").unwrap();
    for h in [&intro, &guide] {
        let ix = index(h);
        assert!(ix.contains(GROUP), "{ix}");
        for m in ix.split("class=\"").skip(1) {
            let classes: Vec<&str> = m[..m.find('"').unwrap()].split(' ').collect();
            if classes.contains(&"chg-mark") || classes.contains(&"toc-changes") {
                assert!(classes.contains(&"chg"), "{classes:?}");
            }
        }
        assert!(!hidden.is_match(ix), "{ix}");
    }
    let css = parse_css(&std::fs::read_to_string(out.path().join("assets/site.css")).unwrap());
    for w in [360, 1280] {
        assert_ne!(
            cascaded(&css, ".chg-mark", "display", w).as_deref(),
            Some("none"),
            "{w}"
        );
    }
}

#[test]
fn rvm2_canon_and_tags_show_no_index_marker() {
    let (_d, r, _ids) = review_project();
    let landing: rdy_model::Hash = r.read().versions().unwrap()[0].landing.parse().unwrap();
    for scope in [Scope::Canon, Scope::At(landing)] {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), &scope, out.path()).unwrap();
        for p in pages(out.path()) {
            let h = std::fs::read_to_string(&p).unwrap();
            for m in [
                "toc-changes",
                "Changes on this page",
                "chg-mark",
                "id=\"Removed-",
            ] {
                assert!(!h.contains(m), "{scope:?} {} holds {m}", p.display());
            }
        }
    }
}

#[test]
fn rvm2_llms_search_and_copy_carry_no_index_marker() {
    let (_d, _r, _ids, out) = draft();
    for f in ["llms.txt", "llms-full.txt", "search-index.json"] {
        let t = std::fs::read_to_string(out.path().join(f)).unwrap();
        for m in ["Changes on this page", "chg-mark"] {
            assert!(!t.contains(m), "{f} holds {m}");
        }
    }
    const COPY: &str = "<template data-copy-page hidden>";
    let mut copies = 0;
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        for (i, _) in h.match_indices(COPY) {
            let t = &h[i..i + h[i..].find("</template>").unwrap()];
            for m in ["Changes on this page", "chg-mark"] {
                assert!(!t.contains(m), "{} Copy holds {m}", p.display());
            }
            copies += 1;
        }
    }
    assert!(copies > 0);
}
