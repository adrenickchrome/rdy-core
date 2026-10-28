//! ndh5 (D-P3YD5, D-HK5G8): an open question that sits at the project root or in a folder is on
//! the open-questions page, in a group of its own under its folder's title or the project's name,
//! with no "from" line. The site keeps no decision pages, and the review site marks no block that
//! sits outside a doc.
use rdy_engine::{Rdy, Scope};
use rdy_qa::fixtures::{approver, land, write, write_as};
use rdy_qa::golden::{fixture_clock, html, pages};
use rdy_qa::site::page_titles;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

/// The project's name in the fixture: the last group of the open-questions page.
const PROJECT: &str = "Notes";

struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
}

impl World {
    fn id(&self, key: &str) -> &str {
        &self.ids[key]
    }
}

/// A project named [`PROJECT`] with no decision home: doc `guide` (title `Guide`) with section
/// `Rules` and open question `$qd`; folder `area` (title `Area`) with doc `notes` and open
/// question `$qf`; open question `$qr` at the root; a root decision `$dr` that the claim in
/// `guide` links `decided_by`; a root decision `$dn` that governs nothing; a prose in `guide`
/// that references `$qr` and `$dn`. Landed.
fn world() -> World {
    let _clock = fixture_clock();
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        format!("pack: rdy-standard\nname: {PROJECT:?}\n"),
    )
    .expect("config");
    let mut r = Rdy::open(dir.path()).expect("open");
    let w = write_as(&mut r, &approver(), "work", json!([
        {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$rules", "parent": "$guide", "fields": {"title": "Rules"}},
        {"op": "create", "type": "question", "id": "$qd", "parent": "$rules", "fields": {"prompt": "Doc question?"}},
        {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area", "title": "Area"}},
        {"op": "create", "type": "doc", "id": "$notes", "parent": "$area", "fields": {"name": "notes", "title": "Notes page", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$ns", "parent": "$notes", "fields": {"title": "Detail"}},
        {"op": "create", "type": "prose", "id": "$np", "parent": "$ns", "body": "Plain notes."},
        {"op": "create", "type": "question", "id": "$qf", "parent": "$area", "fields": {"prompt": "Folder question?"}},
        {"op": "create", "type": "question", "id": "$qr", "fields": {"prompt": "Root question?"}},
        {"op": "create", "type": "decision", "id": "$dr",
         "fields": {"statement": "Rules are short.", "status": "accepted",
                    "decided_at": "2021-02-08T10:00:00Z", "recorded_at": "2021-02-08T10:00:00Z"}},
        {"op": "create", "type": "decision", "id": "$dn",
         "fields": {"statement": "Nothing hangs on this.", "status": "accepted",
                    "decided_at": "2021-02-08T10:00:00Z", "recorded_at": "2021-02-08T10:00:00Z"}},
        {"op": "create", "type": "claim", "id": "$claim", "parent": "$rules", "body": "Keep every rule short.",
         "links": {"decided_by": [{"target": "$dr"}]}},
        {"op": "create", "type": "prose", "id": "$ref", "parent": "$rules", "body": "See {{$qr}} and {{$dn}}."}
    ]))
    .unwrap_or_else(|e| panic!("the fixture write: {e:?}"));
    land(&mut r, "work").expect("land the fixture");
    World {
        _dir: dir,
        r,
        ids: w
            .created
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
    }
}

fn site(w: &World, scope: &Scope) -> tempfile::TempDir {
    let out = tempfile::tempdir().expect("tempdir");
    rdy_render::render_site(&w.r.read(), scope, out.path()).expect("render the site");
    out
}

/// The `<h2>` texts of a page, in order.
fn h2s(h: &str) -> Vec<String> {
    page_titles(h)
        .into_iter()
        .filter(|(level, _)| *level == 2)
        .map(|(_, t)| t)
        .collect()
}

/// The row of the open question with `prompt`: its `<div class="callout callout-question" …>`.
fn row_of<'a>(h: &'a str, prompt: &str) -> &'a str {
    let at = h
        .find(&format!("</strong> {prompt}</p>"))
        .unwrap_or_else(|| panic!("no row for {prompt}: {h}"));
    let start = h[..at]
        .rfind("<div class=\"callout callout-question\"")
        .expect("a row starts");
    let end = at + h[at..].find("</div>").expect("a row ends") + "</div>".len();
    &h[start..end]
}

/// The anchor of a row.
fn anchor_of(row: &str) -> &str {
    let rest = row.split_once("id=\"").expect("a row anchor").1;
    rest.split_once('"').expect("a closed anchor").0
}

#[test]
fn ndh5_questions_outside_docs_have_their_own_groups() {
    let w = world();
    let out = site(&w, &Scope::Canon);
    let h = html(out.path(), "questions.html");
    assert_eq!(h2s(&h), ["Guide", "Area", PROJECT], "{h}");
    let folder = row_of(&h, "Folder question?");
    let root = row_of(&h, "Root question?");
    let at = |s: &str| h.find(s).expect("in the page");
    assert!(at(folder) > at("<h2>Area</h2>") && at(folder) < at(&format!("<h2>{PROJECT}</h2>")));
    assert!(
        at(root) > at(&format!("<h2>{PROJECT}</h2>")),
        "the last group"
    );
    for (row, prompt) in [(folder, "Folder question?"), (root, "Root question?")] {
        let expect = format!(
            "<div class=\"callout callout-question\" id=\"{}\"><p><strong>Open question:</strong> {prompt}</p></div>",
            anchor_of(row)
        );
        assert_eq!(row, expect);
        assert!(!row.contains("class=\"meta\""), "{row}");
    }
    assert_ne!(anchor_of(folder), anchor_of(root));
}

#[test]
fn ndh5_doc_rows_keep_their_from_line() {
    let w = world();
    let out = site(&w, &Scope::Canon);
    let h = html(out.path(), "questions.html");
    let row = row_of(&h, "Doc question?");
    assert!(
        row.contains("<p class=\"meta\"><a href=\"guide.html#")
            && row.contains("\">from Guide › Rules</a></p>"),
        "{row}"
    );
}

#[test]
fn ndh5_a_link_to_a_root_question_leads_to_the_questions_page() {
    let w = world();
    let out = site(&w, &Scope::Canon);
    let questions = html(out.path(), "questions.html");
    let anchor = anchor_of(row_of(&questions, "Root question?")).to_string();
    let guide = html(out.path(), "guide.html");
    assert!(
        guide.contains(&format!("href=\"questions.html#{anchor}\"")),
        "{guide}"
    );
}

/// The change entries of a `changes.html` page: the ids of the blocks they name.
fn entry_ids(h: &str) -> Vec<String> {
    h.match_indices("<article class=\"chg-entry")
        .map(|(i, _)| {
            let rest = &h[i..];
            let id = rest
                .split_once("data-rdy-block=\"")
                .expect("an entry block")
                .1;
            id.split_once('"').expect("a closed id").0.to_string()
        })
        .collect()
}

/// The sentence under the heading of a change summary.
fn lede(h: &str) -> &str {
    let rest = h.split_once("<p class=\"lede\">").expect("a lede").1;
    rest.split_once("</p>").expect("a closed lede").0
}

#[test]
fn ndh5_a_review_marks_no_block_outside_a_doc() {
    let mut w = world();
    let claim = w.id("$claim").to_string();
    let area = w.id("$area").to_string();
    // the guard: a view that only changes the claim
    write(
        &mut w.r,
        "claim_only",
        json!([{"op": "update", "id": claim, "body": "Keep every rule very short."}]),
    )
    .expect("the claim-only view");
    // the view under test: the same claim change, a question at the root and one in the folder
    write(
        &mut w.r,
        "draft",
        json!([
            {"op": "update", "id": claim, "body": "Keep every rule very short."},
            {"op": "create", "type": "question", "id": "$nr", "fields": {"prompt": "New root question?"}},
            {"op": "create", "type": "question", "id": "$nf", "parent": area, "fields": {"prompt": "New folder question?"}}
        ]),
    )
    .expect("the draft view");
    let base = site(&w, &Scope::View("claim_only".into()));
    let out = site(&w, &Scope::View("draft".into()));

    let changes = html(out.path(), "changes.html");
    assert_eq!(entry_ids(&changes), [claim.clone()], "{changes}");
    assert_eq!(h2s(&changes), ["Guide"], "{changes}");
    assert!(!changes.contains("New root question?") && !changes.contains("New folder question?"));
    assert_eq!(
        lede(&changes),
        lede(&html(base.path(), "changes.html")),
        "the same number of changed blocks"
    );
    assert_eq!(lede(&changes), "1 change in 1 doc, not landed.");

    // only the claim carries a mark on its page, and no mark sits on the other pages
    let guide = html(out.path(), "guide.html");
    assert_eq!(guide.matches("chg-label").count(), 1, "{guide}");
    assert!(guide.contains("chg-label chg-edited"));
    for page in pages(out.path()) {
        let name = page
            .strip_prefix(out.path())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // the permalink of a question leads to the questions page, as it does for a doc's
        if matches!(name.as_str(), "guide.html" | "changes.html") || name.starts_with("go/") {
            continue;
        }
        let h = std::fs::read_to_string(&page).unwrap();
        if h.contains("New root question?") || h.contains("New folder question?") {
            assert_eq!(name, "questions.html", "{name}");
        }
        assert!(
            !h.contains("chg-label") && !h.contains("has-chg") && !h.contains("chg-new"),
            "{name} holds a mark"
        );
    }
    // the home page lists the Guide alone
    let home = html(out.path(), "index.html");
    assert!(home.contains("Changes in this view"), "{home}");
    assert!(
        !home.contains("New root question?") && !home.contains("New folder question?"),
        "{home}"
    );
}

/// Every file under `dir`, recursively, by its path under `dir`.
fn files(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn ndh5_a_site_renders_with_blocks_outside_docs() {
    let w = world();
    let out = site(&w, &Scope::Canon);
    let all = files(out.path());
    // no page is named for a decision, and a decision keeps only its permalink: it leads to the
    // block the decision governs, and a decision that governs nothing has none
    for (name, _) in &all {
        let lower = name.to_lowercase();
        assert!(!lower.contains("decision"), "{name}");
        assert!(!lower.contains(&w.id("$dn").to_lowercase()), "{name}");
        if !name.starts_with("go/") {
            assert!(!lower.contains(&w.id("$dr").to_lowercase()), "{name}");
        }
    }
    let go = all
        .iter()
        .find(|(n, _)| *n == format!("go/{}.html", w.id("$dr")))
        .expect("the permalink of the governing decision");
    assert!(go.1.contains("guide.html#"), "{}", go.1);
    // the reference to a decision that governs nothing reads as its label, with no link
    let guide = html(out.path(), "guide.html");
    let at = guide.find("Nothing hangs on this.").expect("the label");
    let before = &guide[..at];
    assert!(
        !(before.rfind("<a ") > before.rfind("</a>")),
        "the label sits inside a link: {}",
        &guide[at.saturating_sub(200)..at]
    );
    // the trail of the claim shows the root decision's statement
    let claim = w.id("$claim");
    let start = guide
        .find(&format!("data-rdy-block=\"{claim}\""))
        .expect("the claim");
    let own = &guide[start..];
    let own = &own[..own[1..]
        .find("data-rdy-block=\"")
        .map_or(own.len(), |e| e + 1)];
    assert!(own.contains("Ruled: Rules are short."), "{own}");
    // no page, llms file or search index holds the text of a question outside a doc, except the
    // open-questions page and the prose that references the root question (the reference reads
    // as the question's words); a question's permalink under `go/` leads to the questions page, as a
    // doc question's does
    let ref_block = format!("data-rdy-block=\"{}\"", w.id("$ref"));
    for (name, text) in &all {
        if name == "questions.html" || name.starts_with("go/") {
            continue;
        }
        // the prose reads the question's words where it references it (the home page's card,
        // the llms files and the search index quote the prose)
        let mut text = text.replace("See Root question? and Nothing hangs on this.", "");
        if name == "guide.html" {
            let from = text.find(&ref_block).expect("the prose with the reference");
            let to = text[from + 1..]
                .find("data-rdy-block=\"")
                .map_or(text.len(), |e| from + 1 + e);
            text.replace_range(from..to, "");
        }
        assert!(
            !text.contains("Root question?") && !text.contains("Folder question?"),
            "{name} holds a question outside a doc"
        );
    }
}
