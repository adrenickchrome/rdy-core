//! Disclosure review Task 1b: the site's shape (pages, reading order, numbers, sections) and the
//! review's count rule live in `rdy_engine::reader`, and the site reads them from there. Each
//! test builds the shared facts from a state and compares them with the pages, the search index
//! and the review marks that the site actually writes.
use rdy_engine::reader::labels;
use rdy_engine::reader::review::{self, Mark};
use rdy_engine::reader::shape::{self, Shape};
use rdy_engine::reader::text::Reading;
use rdy_engine::{Materialized, Scope};
use rdy_model::{Block, BlockId, tree};
use rdy_qa::golden::*;
use rdy_qa::site::visible_runs;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The words of a reference outside a profile's selection with no display text.
const UNAVAILABLE: &str = "unavailable reference";

/// The reader inputs a shape supplies: its blocks, docs, tree names and numbers. A named
/// profile's side passes its words for a reference that leads nowhere.
struct Reader<'s, 'm> {
    shape: &'s Shape<'m>,
    unavailable: Option<&'static str>,
}

impl Reading for Reader<'_, '_> {
    fn block(&self, id: &BlockId) -> Option<&Block> {
        self.shape.state().get(id)
    }

    fn doc_of(&self, id: &BlockId) -> Option<&Block> {
        self.shape.state().doc_of(id)
    }

    fn tree_name(&self, id: &BlockId) -> Option<String> {
        self.shape.state().name_of(id)
    }

    fn number(&self, b: &Block) -> usize {
        self.shape.number(&b.id).unwrap_or(1)
    }

    fn unavailable(&self) -> Option<&'static str> {
        self.unavailable
    }

    fn drops(&self, _: &BlockId) -> bool {
        false
    }
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The search index's entries of the kinds the shape orders: (kind, title, page).
fn indexed(index: &Value) -> Vec<(String, String, String)> {
    index["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|e| matches!(e["kind"].as_str(), Some("doc" | "folder" | "section")))
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["title"].as_str().unwrap().to_string(),
                e["page"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The same entries from the shared shape: each folder and doc with a page in tree order, each
/// doc followed by its sections in reading order, each titled by the shared label rule.
fn shaped(shape: &Shape, reader: &Reader) -> Vec<(String, String, String)> {
    let m = shape.state();
    let mut out = vec![];
    for id in shape.tree() {
        let b = m.get(id).unwrap();
        let page = shape.page(id).expect("a tree page");
        out.push((
            b.block_type.clone(),
            labels::label(reader, b, 0),
            page.clone(),
        ));
        if b.block_type == tree::DOC {
            for s in shape.blocks_of(id) {
                if s.block_type == "section" {
                    out.push(("section".into(), labels::label(reader, s, 0), page.clone()));
                }
            }
        }
    }
    out
}

fn search_index(files: &BTreeMap<String, Vec<u8>>) -> Value {
    serde_json::from_slice(files.get("search-index.json").expect("a search index")).unwrap()
}

/// Every file of a rendered site, by its path under `dir`.
fn tree_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// A project with a folder of docs, a doc shell, a folder shell, a top-level `Glossary` doc
/// (the generated glossary owns its page), numbered code blocks, nested sections, a leading
/// question and a question in a section, and a doc whose reference leads to a doc that a
/// profile leaves out.
fn shaped_project() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    BTreeMap<String, BlockId>,
) {
    let _clock = fixture_clock();
    let (d, mut r) = tiny();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$guides", "fields": {"name": "guides", "title": "Guides"}},
        {"op": "create", "type": "doc", "id": "$intro", "parent": "$guides", "fields": {"name": "intro", "title": "Intro to `rdy` reading", "doc_type": "overview"}},
        {"op": "create", "type": "question", "id": "$lead_q", "parent": "$intro", "fields": {"prompt": "Who reads first?"}},
        {"op": "create", "type": "section", "id": "$alpha", "parent": "$intro", "fields": {"title": "Alpha"}},
        {"op": "create", "type": "prose", "id": "$a1", "parent": "$alpha", "body": "Alpha words."},
        {"op": "create", "type": "code", "id": "$c1", "parent": "$alpha", "fields": {"lang": "python"}, "body": "x = 1"},
        {"op": "create", "type": "code", "id": "$c2", "parent": "$alpha", "fields": {"lang": "python"}, "body": "y = 2"},
        {"op": "create", "type": "prose", "id": "$a2", "parent": "$alpha", "body": "Run {{$c2}} next."},
        {"op": "create", "type": "section", "id": "$beta", "parent": "$intro", "fields": {"title": "Beta"}},
        {"op": "create", "type": "section", "id": "$beta1", "parent": "$beta", "fields": {"title": "Beta one"}},
        {"op": "create", "type": "term", "id": "$ledger", "parent": "$beta1", "fields": {"name": "Ledger"}, "body": "Balances by account."},
        {"op": "create", "type": "question", "id": "$deep_q", "parent": "$beta1", "fields": {"prompt": "Which ledger wins?"}},
        {"op": "create", "type": "doc", "id": "$empty", "parent": "$guides", "fields": {"name": "empty", "title": "Empty shell", "doc_type": "overview"}},
        {"op": "create", "type": "folder", "id": "$shell", "fields": {"name": "shell", "title": "Shell folder"}},
        {"op": "create", "type": "doc", "id": "$gloss", "fields": {"name": "Glossary", "title": "Own glossary", "doc_type": "overview"}},
        {"op": "create", "type": "term", "id": "$own", "parent": "$gloss", "fields": {"name": "Own term"}, "body": "Defined in the dropped doc."},
        {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret doc", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$sp", "parent": "$secret", "body": "Secret words."},
        {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$ns", "parent": "$notes", "fields": {"title": "See {{$sp}}"}},
        {"op": "create", "type": "prose", "id": "$np", "parent": "$ns", "body": "Read {{$ledger}} and {{$sp}}."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let ids = w.created.iter().map(|(k, v)| (k.clone(), bid(v))).collect();
    (d, r, ids)
}

#[test]
fn drv1_shared_reader_matches_site_labels_text_pages_and_order() {
    let (_d, mut r, ids) = shaped_project();
    let id = |k: &str| ids[k].clone();

    // the source site
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let files = tree_files(out.path());
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let shape = Shape::build(&m);
    let reader = Reader {
        shape: &shape,
        unavailable: None,
    };

    // pages: each folder and doc with a page has that file, and the search index lists them,
    // and their sections, in the shape's order with the shared labels
    assert_eq!(indexed(&search_index(&files)), shaped(&shape, &reader));
    for p in shape.tree().iter().filter_map(|i| shape.page(i)) {
        assert!(files.contains_key(&p), "{p}: {:?}", files.keys());
    }
    assert_eq!(
        shape.page(&id("$intro")).as_deref(),
        Some("guides/intro.html")
    );
    assert_eq!(
        shape.page(&id("$guides")).as_deref(),
        Some("guides/index.html")
    );
    // shells keep their pages
    assert_eq!(
        shape.page(&id("$empty")).as_deref(),
        Some("guides/empty.html")
    );
    assert_eq!(
        shape.page(&id("$shell")).as_deref(),
        Some("shell/index.html")
    );
    assert!(shape.blocks_of(&id("$empty")).is_empty());

    // the reserved page: the top-level `Glossary` doc has no page of its own, the generated
    // glossary owns `glossary.html`, and the doc keeps its reading place
    assert_eq!(shape.page(&id("$gloss")), None);
    assert_eq!(shape.glossary_docs(), &[id("$gloss")]);
    assert!(shape.page_taken("guides/intro.html"));
    assert!(!shape.page_taken("glossary.html"));
    let glossary = String::from_utf8(files["glossary.html"].clone()).unwrap();
    assert!(glossary.contains("Own term"), "its terms are listed");
    assert!(!files.contains_key("Glossary.html"));
    let reading: Vec<BlockId> = shape.reading().to_vec();
    assert_eq!(
        reading,
        [
            "$guides", "$intro", "$empty", "$shell", "$gloss", "$secret", "$notes"
        ]
        .map(id)
    );
    assert_eq!(shape.position(&id("$gloss")), Some(4));
    // docs with a page, in reading order: the dropped doc is left out, the others keep the
    // reading order's relative places
    let with_page: Vec<&BlockId> = reading.iter().filter(|i| shape.page(i).is_some()).collect();
    let docs: Vec<&BlockId> = with_page
        .into_iter()
        .filter(|i| m.get(i).unwrap().block_type == tree::DOC)
        .collect();
    assert_eq!(docs, shape.docs().iter().collect::<Vec<_>>());

    // numbers: the second code block of its kind reads as `Python code 2`, as the site words a
    // reference to it
    let intro = String::from_utf8(files["guides/intro.html"].clone()).unwrap();
    assert_eq!(shape.number(&id("$c2")), Some(2));
    let c2 = labels::label(&reader, m.get(&id("$c2")).unwrap(), 0);
    assert_eq!(c2, "Python code 2");
    let runs: Vec<String> = visible_runs(&intro).into_iter().map(|r| r.text).collect();
    let at = runs.iter().position(|t| t == "Run").expect("the prose");
    assert_eq!(runs[at..at + 3], ["Run", c2.as_str(), "next."], "{runs:?}");

    // sections: the questions page says where each question is with the shared section rule;
    // the leading question has no section
    assert_eq!(shape::section_of(&m, &id("$lead_q")), None);
    let deep = shape::section_of(&m, &id("$deep_q")).expect("a section");
    assert_eq!(deep.id, id("$beta1"));
    let questions = String::from_utf8(files["questions.html"].clone()).unwrap();
    let intro_title = labels::label(&reader, m.get(&id("$intro")).unwrap(), 0);
    let deep_label = labels::label(&reader, deep, 0);
    assert!(
        questions.contains(&format!("from {intro_title} › {deep_label}</a>")),
        "{questions}"
    );
    assert!(
        questions.contains(&format!("from {intro_title}</a>")),
        "{questions}"
    );

    // reader text: a section's search text is its children's shared field-aware text
    let index = search_index(&files);
    let section_text = |title: &str| -> String {
        index["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "section" && e["title"] == title)
            .and_then(|e| e["text"].as_str())
            .unwrap_or_else(|| panic!("no section {title}"))
            .to_string()
    };
    let own = |k: &str| rdy_engine::reader::text::own_text(&reader, m.get(&id(k)).unwrap());
    assert_eq!(own("$a2"), "Run Python code 2 next.");
    assert_eq!(own("$ledger"), "Ledger: Balances by account.");
    let alpha = section_text("Alpha");
    assert!(
        alpha.contains(&own("$a1")) && alpha.contains(&own("$a2")),
        "{alpha}"
    );
    let beta1 = section_text("Beta one");
    assert!(
        beta1.contains(&own("$ledger")) && beta1.contains(&own("$deep_q")),
        "{beta1}"
    );

    // an excluded target's label: a profile that selects `notes` alone reads the reference to
    // the secret doc's prose as the unavailable words, in its pages and its search index
    let admin = write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin",
            "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )
    .unwrap()
    .created["$admin"]
        .clone();
    write(
        &mut r,
        "admin",
        json!([{"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": "notesonly", "title": "Notes only", "audience": "org",
                "select": [id("$notes").to_string()]}}]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    let st = r.read();
    let resolved = rdy_engine::publication::resolve(&st, &Scope::Canon, "notesonly").unwrap();
    let selected: &Materialized = &resolved.m;
    let profile_shape = Shape::build(selected);
    let profile_reader = Reader {
        shape: &profile_shape,
        unavailable: Some(UNAVAILABLE),
    };
    let s = rdy_render::build_profile_site(&st, "canon", "notesonly").unwrap();
    assert_eq!(
        indexed(&search_index(&s.files)),
        shaped(&profile_shape, &profile_reader)
    );
    let ns = selected.get(&id("$ns")).unwrap();
    assert_eq!(
        labels::label(&profile_reader, ns, 0),
        format!("See {UNAVAILABLE}")
    );
    let notes = String::from_utf8(s.files["notes.html"].clone()).unwrap();
    assert!(
        notes.contains(&format!(">See {UNAVAILABLE}</h2>")),
        "{notes}"
    );
    assert_eq!(profile_shape.page(&id("$secret")), None);
    assert_eq!(profile_shape.position(&id("$secret")), None);

    // an empty profile state: no page, no reading place, no number
    let empty = Materialized::default();
    let none = Shape::build(&empty);
    assert!(none.reading().is_empty() && none.docs().is_empty() && none.tree().is_empty());
    assert!(none.path_clash().is_none());
}

/// The canon doc `guide` (a lede, then Alpha, Beta > Beta one, Gamma and Delta), landed, and the
/// view `w` that deletes a prose, edits two and adds two.
fn guide_view() -> (
    tempfile::TempDir,
    rdy_engine::Rdy,
    BTreeMap<String, BlockId>,
) {
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
    let mut ids: BTreeMap<String, BlockId> = canon
        .created
        .iter()
        .map(|(k, v)| (k.clone(), bid(v)))
        .collect();
    let id = |k: &str| ids[k].to_string();
    let view = write(&mut r, "w", json!([
        {"op": "delete", "id": id("$a2")},
        {"op": "update", "id": id("$b11"), "body": "B eleven, edited."},
        {"op": "create", "type": "prose", "id": "$g2", "parent": id("$gamma"), "body": "G two."},
        {"op": "update", "id": id("$d1"), "body": "D one, edited."},
        {"op": "create", "type": "prose", "id": "$d2", "parent": id("$delta"), "body": "D two."},
    ]))
    .unwrap();
    ids.extend(view.created.iter().map(|(k, v)| (k.clone(), bid(v))));
    (d, r, ids)
}

/// The "Changes on this page" entries of a page: (word, visible label).
fn change_entries(h: &str) -> Vec<(String, String)> {
    const GROUP: &str = "<nav class=\"chg toc-changes\" aria-label=\"Changes on this page\">";
    const OPEN: &str = "<li><a class=\"toc-link toc-chg\" href=\"";
    let a = h.find(GROUP).expect("the changes group");
    let g = &h[a..a + h[a..].find("</nav>").unwrap()];
    g.split(OPEN)
        .skip(1)
        .map(|li| {
            let li = &li[..li.find("</li>").unwrap()];
            let runs: Vec<String> = visible_runs(&format!("{OPEN}{li}</li>"))
                .into_iter()
                .map(|r| r.text)
                .collect();
            (runs[0].clone(), runs[1].clone())
        })
        .collect()
}

/// The word of a section entry in the page index, if it carries one.
fn index_word(h: &str, anchor: &str) -> Option<String> {
    let a = h.find("<aside class=\"index").expect("the index");
    let ix = &h[a..a + h[a..].find("</aside>").unwrap()];
    let at = ix
        .find(&format!("<a class=\"toc-link\" href=\"#{anchor}\">"))
        .unwrap_or_else(|| panic!("no entry #{anchor}"));
    let link = &ix[at..at + ix[at..].find("</a>").unwrap()];
    ["edited", "new", "removed"]
        .into_iter()
        .find(|w| link.contains(&format!("chg-{w}\"")))
        .map(str::to_string)
}

/// The sections above `id` in `m`, nearest first, by the shared section rule.
fn sections_above(m: &Materialized, id: &BlockId) -> Vec<BlockId> {
    let mut out = vec![];
    let mut at = id.clone();
    while let Some(s) = shape::section_of(m, &at) {
        out.push(s.id.clone());
        at = s.id.clone();
    }
    out
}

#[test]
fn drv1_shared_counts_keep_review_markers_and_sections() {
    let (_d, r, ids) = guide_view();
    let id = |k: &str| ids[k].clone();
    let st = r.read();
    let scope = Scope::View("w".into());
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&st, &scope, out.path()).unwrap();
    let base = st.materialized(&Scope::Canon).unwrap();
    let draft = st.materialized(&scope).unwrap();

    // the shared count rule gives the render's own review, block for block
    let rv = review::between_states(&base, &draft);
    let site_rv = rdy_render::review::build(&st, &scope)
        .unwrap()
        .expect("a view has a base");
    assert_eq!(rv.changed_set(), site_rv.changed_set());
    assert_eq!(rv.changed_blocks(), 5);
    let doc = rv.doc(&id("$g")).expect("the doc changed");
    let site_doc = site_rv.doc(&id("$g")).unwrap();
    assert_eq!(
        (doc.mark, doc.edited, doc.new, doc.removed),
        (
            site_doc.mark,
            site_doc.edited,
            site_doc.new,
            site_doc.removed
        )
    );
    assert_eq!(
        doc.counts().as_deref(),
        Some("2 edited · 2 new · 1 removed")
    );
    let home = html(out.path(), "index.html");
    assert!(
        home.contains("2\u{a0}edited · 2\u{a0}new · 1\u{a0}removed"),
        "{home}"
    );
    let changes = html(out.path(), "changes.html");
    assert!(
        changes.contains(&format!("{} changes in 1 doc", rv.changed_blocks())),
        "{changes}"
    );

    // the page's changes, in page order, from the shared facts: each removed line before the
    // block it stands before, then each marked block, with the shared label
    let shape = Shape::build(&draft);
    let old_shape = Shape::build(&base);
    let reader = Reader {
        shape: &shape,
        unavailable: None,
    };
    let old_reader = Reader {
        shape: &old_shape,
        unavailable: None,
    };
    let word = |m: Mark| m.word().to_lowercase();
    let removed = rv.removed_in(&id("$g"));
    let mut want: Vec<(String, String)> = vec![];
    for b in shape.blocks_of(&id("$g")) {
        for line in removed.iter().filter(|l| l.before.as_ref() == Some(&b.id)) {
            want.push((
                "removed".into(),
                labels::plain_label(&labels::label(&old_reader, &line.blocks[0], 0)),
            ));
        }
        if let Some(m) = rv.mark(&b.id) {
            want.push((word(m), labels::plain_label(&labels::label(&reader, b, 0))));
        }
    }
    let page = html(out.path(), "guide.html");
    assert_eq!(change_entries(&page), want);

    // each section's word from the shared section rule: the words of the changes it holds (its
    // removed blocks read on the base side), one word, or `edited` when they differ
    let mut held: BTreeMap<BlockId, String> = BTreeMap::new();
    let mut give = |sections: Vec<BlockId>, w: String| {
        for s in sections {
            let next = match held.get(&s) {
                Some(had) if *had != w => "edited".to_string(),
                _ => w.clone(),
            };
            held.insert(s, next);
        }
    };
    for line in removed {
        give(sections_above(&base, &line.blocks[0].id), "removed".into());
    }
    for b in shape.blocks_of(&id("$g")) {
        if let Some(m) = rv.mark(&b.id) {
            give(sections_above(&draft, &b.id), word(m));
        }
    }
    for (key, anchor) in [
        ("$alpha", "alpha"),
        ("$beta", "beta"),
        ("$beta1", "beta-one"),
        ("$gamma", "gamma"),
        ("$delta", "delta"),
    ] {
        assert_eq!(
            index_word(&page, anchor),
            held.get(&id(key)).cloned(),
            "{key}"
        );
    }
    assert_eq!(held.len(), 5);
}
