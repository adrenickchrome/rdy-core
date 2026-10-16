//! ec3 (D-WQSAY, D-EVRNM, D-X61DT): the print model. One model of exactly what a reader sees on
//! the site (the words on the page, nothing a reader cannot see) feeds every print format.
use rdy_engine::{Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::print::{self, Href, PBlock, PageKind, Print, PrintDoc, Run};
use rdy_render::{Mode, build_site};
use serde_json::json;

fn canon(r: &Rdy) -> Print {
    print::build(&r.read(), &Scope::Canon).expect("print canon")
}

fn view(r: &Rdy) -> Print {
    print::build(&r.read(), &Scope::View("editing".into())).expect("print the view")
}

/// The text of runs, as one line shows them.
fn text(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// Every run of `blocks`, in order.
fn runs_of<'a>(blocks: &'a [PBlock], out: &mut Vec<&'a Run>) {
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => out.extend(runs),
            PBlock::List { items, .. } => items.iter().for_each(|i| runs_of(i, out)),
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    out.extend(cell);
                }
            }
            PBlock::Figure { caption, .. } => out.extend(caption),
            PBlock::Callout { blocks, .. } => runs_of(blocks, out),
            PBlock::Code { .. } => {}
        }
    }
}

/// Every block of `blocks` and the blocks inside them, in order.
fn all_blocks<'a>(blocks: &'a [PBlock], out: &mut Vec<&'a PBlock>) {
    for b in blocks {
        out.push(b);
        match b {
            PBlock::List { items, .. } => items.iter().for_each(|i| all_blocks(i, out)),
            PBlock::Callout { blocks, .. } => all_blocks(blocks, out),
            _ => {}
        }
    }
}

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split_whitespace().map(String::from)
}

/// The words of a doc of the print, in order: its title, then the words of its runs, code text,
/// captions and callout labels.
fn doc_words(d: &PrintDoc) -> Vec<String> {
    fn block(b: &PBlock, out: &mut Vec<String>) {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => {
                out.extend(words(&text(runs)))
            }
            PBlock::List { items, .. } => items.iter().flatten().for_each(|b| block(b, out)),
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    out.extend(words(&text(cell)));
                }
            }
            PBlock::Code { text, .. } => out.extend(words(text)),
            PBlock::Figure { caption, .. } => out.extend(words(&text(caption))),
            PBlock::Callout { label, blocks, .. } => {
                out.extend(words(label));
                blocks.iter().for_each(|b| block(b, out));
            }
        }
    }
    let mut out: Vec<String> = words(&d.title).collect();
    d.blocks.iter().for_each(|b| block(b, &mut out));
    out
}

/// `s` with each Markdown backslash escape resolved.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&n) = chars.peek()
            && n.is_ascii_punctuation()
        {
            out.push(n);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// The words of one section of `llms-full.txt`, less this closed list (O27): the leading `#`s of
/// a heading line, a list marker `-` or `N.` at line start, table pipes and the separator row,
/// code fence lines, Markdown backslash escapes, and the `summary` words of each diagram's
/// `details`.
fn llms_words(section: &str) -> Vec<String> {
    let lines: Vec<&str> = section.lines().collect();
    let is_fence = |l: &str| l.len() >= 3 && l.chars().all(|c| c == '`');
    let mut out = vec![];
    let mut fence: Option<&str> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(f) = fence {
            if *line == f {
                fence = None;
            } else {
                out.extend(words(line));
            }
            continue;
        }
        let t = line.trim_start();
        if is_fence(t) {
            fence = Some(t);
            continue;
        }
        let row = t.starts_with('|');
        if row && t.split_whitespace().all(|w| w == "|" || w == "---") {
            continue;
        }
        // the summary line of a diagram's `details`: the next line with words is its fence
        let next = lines[i + 1..].iter().find(|n| !n.trim().is_empty());
        if t == "Diagram source" && next.is_some_and(|n| is_fence(n.trim_start())) {
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let t = if hashes > 0 && t[hashes..].starts_with(' ') {
            &t[hashes + 1..]
        } else if let Some(rest) = t.strip_prefix("- ") {
            rest
        } else {
            let digits = t.chars().take_while(char::is_ascii_digit).count();
            match t[digits..].strip_prefix(". ") {
                Some(rest) if digits > 0 => rest,
                _ => t,
            }
        };
        out.extend(
            t.split_whitespace()
                .filter(|w| !(row && *w == "|"))
                .map(unescape),
        );
    }
    out
}

/// The sections of `llms-full.txt` after its head: one per folder and doc, in tree order. A
/// section starts at a `# ` line outside a code fence.
fn llms_sections(full: &str) -> Vec<String> {
    let mut sections: Vec<String> = vec![];
    let mut fence: Option<&str> = None;
    for line in full.lines() {
        let t = line.trim_start();
        match fence {
            Some(f) if t == f => fence = None,
            None if t.len() >= 3 && t.chars().all(|c| c == '`') => fence = Some(t),
            _ => {}
        }
        if (fence.is_none() && line.starts_with("# ")) || sections.is_empty() {
            sections.push(String::new());
        }
        let cur = sections.last_mut().expect("a section");
        cur.push_str(line);
        cur.push('\n');
    }
    sections.remove(0);
    sections
}

fn tree_docs(p: &Print) -> Vec<&PrintDoc> {
    p.docs
        .iter()
        .filter(|d| !matches!(d.kind, PageKind::Glossary))
        .collect()
}

#[test]
fn ec3_print_holds_every_folder_and_doc_in_tree_order_then_the_glossary() {
    let (_d, r, ids) = project();
    let p = canon(&r);
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    // the site's own list of pages, in tree order: `llms.txt`'s "Pages"
    let llms = String::from_utf8(site.files["llms.txt"].clone()).unwrap();
    let pages = llms
        .split("## Pages\n\n")
        .nth(1)
        .unwrap()
        .split("\n\n## Reference")
        .next()
        .unwrap();
    let line = regex::Regex::new(r"^( *)- \[[^\]]*\]\(([^)]*)\)").unwrap();
    let expected: Vec<(usize, String)> = pages
        .lines()
        .map(|l| {
            let c = line
                .captures(l)
                .unwrap_or_else(|| panic!("a page line: {l}"));
            (c[1].len() / 2, c[2].to_string())
        })
        .collect();
    assert!(expected.len() > 60, "the corpus and its appendix");
    let got: Vec<(usize, String)> = tree_docs(&p)
        .iter()
        .map(|d| (d.depth, d.page.clone()))
        .collect();
    assert_eq!(got, expected);
    for d in tree_docs(&p) {
        assert_eq!(
            matches!(d.kind, PageKind::Folder),
            d.page.ends_with("/index.html"),
            "{}",
            d.page
        );
    }
    let last = p.docs.last().unwrap();
    assert!(matches!(last.kind, PageKind::Glossary));
    assert_eq!(
        (last.page.as_str(), last.title.as_str()),
        ("glossary.html", "Glossary")
    );
    assert_eq!(
        p.docs
            .iter()
            .filter(|d| matches!(d.kind, PageKind::Glossary))
            .count(),
        1
    );
    // the glossary lists the terms the site's glossary lists
    let mut glossary = vec![];
    runs_of(&last.blocks, &mut glossary);
    assert!(
        glossary.iter().any(|r| r.bold && r.text == "Guard"),
        "{glossary:?}"
    );
    // a folder: its title and its description; its children follow
    let spec = p
        .docs
        .iter()
        .find(|d| d.id == ids.spec_folder.to_string())
        .unwrap();
    assert!(matches!(spec.kind, PageKind::Folder));
    assert_eq!(spec.page, "specification/index.html");
    // no page a print leaves out
    for left_out in ["index.html", "questions.html", "disagreements.html"] {
        assert!(p.docs.iter().all(|d| d.page != left_out), "{left_out}");
    }
    // the title part
    assert_eq!(p.title, "Structural pattern matching");
    assert_eq!(p.description, r.read().project_description());
    assert_eq!(p.label, site.label);
    assert_eq!(p.scope, "canon");
    assert_eq!(view(&r).scope, "view:editing");
}

#[test]
fn ec3_print_has_the_words_of_llms_full() {
    let (_d, r, _ids) = project();
    for scope in [Scope::Canon, Scope::View("editing".into())] {
        let p = print::build(&r.read(), &scope).unwrap();
        let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
        let full = String::from_utf8(site.files["llms-full.txt"].clone()).unwrap();
        let sections = llms_sections(&full);
        let docs = tree_docs(&p);
        assert_eq!(sections.len(), docs.len(), "{scope:?}");
        for (d, section) in docs.iter().zip(&sections) {
            assert_eq!(doc_words(d), llms_words(section), "{scope:?} {}", d.page);
        }
    }
}

#[test]
fn ec3_print_shows_no_block_id() {
    let (_d, r, _ids) = project();
    for scope in [Scope::Canon, Scope::View("editing".into())] {
        let p = print::build(&r.read(), &scope).unwrap();
        let m = r.read().materialized(&scope).unwrap();
        let ids: Vec<String> = m.ids().map(|id| id.to_string()).collect();
        assert!(ids.len() > 100);
        let mut seen: Vec<String> = vec![];
        for d in &p.docs {
            let mut runs = vec![];
            runs_of(&d.blocks, &mut runs);
            seen.extend(runs.iter().map(|r| r.text.clone()));
            let mut blocks = vec![];
            all_blocks(&d.blocks, &mut blocks);
            for b in blocks {
                match b {
                    PBlock::Code { text, anchor, .. } => {
                        seen.push(text.clone());
                        seen.extend(anchor.clone());
                    }
                    PBlock::Callout { label, anchor, .. } => {
                        seen.push(label.clone());
                        seen.extend(anchor.clone());
                    }
                    PBlock::Figure { alt, anchor, .. } => {
                        seen.push(alt.clone());
                        seen.extend(anchor.clone());
                    }
                    PBlock::Heading { anchor, .. }
                    | PBlock::Para { anchor, .. }
                    | PBlock::Table { anchor, .. } => seen.extend(anchor.clone()),
                    PBlock::List { .. } => {}
                }
            }
            seen.push(d.title.clone());
        }
        for id in &ids {
            if let Some(s) = seen.iter().find(|s| s.contains(id.as_str())) {
                panic!("{scope:?}: {id} shows in {s:?}");
            }
        }
    }
}

#[test]
fn ec3_a_link_to_another_doc_is_a_page_href_with_its_anchor() {
    let (_d, mut r, ids) = project();
    let composing = "s-QJ383";
    write(
        &mut r,
        "xref",
        json!([{"op": "create", "type": "prose", "parent": ids.guards_section.to_string(),
            "body": format!("Read {{{{{composing}}}}} next.")}]),
    )
    .expect("a cross-doc reference");
    land(&mut r, "xref").expect("land it");
    let p = canon(&r);
    let tutorial = p
        .docs
        .iter()
        .find(|d| d.id == ids.tutorial_doc.to_string())
        .unwrap();
    let anchor = tutorial
        .blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Heading { anchor, runs, .. } if text(runs) == "Composing patterns" => {
                anchor.clone()
            }
            _ => None,
        })
        .expect("the section's heading has its anchor");
    let spec = p
        .docs
        .iter()
        .find(|d| d.id == ids.spec_doc.to_string())
        .unwrap();
    let para = spec
        .blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Para { runs, .. } if text(runs) == "Read Composing patterns next." => {
                Some(runs)
            }
            _ => None,
        })
        .expect("the new paragraph");
    let link = para.iter().find_map(|r| r.link.clone()).expect("a link");
    assert_eq!(
        link,
        Href::Page {
            page: tutorial.page.clone(),
            anchor: Some(anchor),
        }
    );
    assert_ne!(tutorial.page, spec.page);
    // every page link leads to a page the print holds
    for d in &p.docs {
        let mut runs = vec![];
        runs_of(&d.blocks, &mut runs);
        for r in runs {
            if let Some(Href::Page { page, .. }) = &r.link {
                assert!(
                    p.docs.iter().any(|d| d.page == *page),
                    "{page} from {}",
                    d.page
                );
            }
        }
    }
}

#[test]
fn ec3_a_diagram_prints_as_its_source() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let mut found = 0;
    for d in &p.docs {
        let mut blocks = vec![];
        all_blocks(&d.blocks, &mut blocks);
        for (i, b) in blocks.iter().enumerate() {
            if let PBlock::Code {
                lang, text: src, ..
            } = b
                && src.contains("CB[Case block] --> P[Pattern]")
            {
                found += 1;
                assert_eq!(lang.as_deref(), Some("mermaid"));
                assert!(src.starts_with("flowchart TD\n"), "{src}");
                assert!(!src.contains("t-XTQDJ"), "a reference reads as its label");
                // its caption (the alt text) comes just before it
                assert!(
                    matches!(blocks[i - 1], PBlock::Para { runs, .. }
                        if text(runs) == "The grammar of a pattern as a tree"),
                    "{:?}",
                    blocks[i - 1]
                );
            }
            // a diagram is no picture, and its "Diagram source" summary is not printed
            if let PBlock::Figure { blob, .. } = b {
                assert!(blob.is_some(), "a figure prints its image: {b:?}");
            }
        }
        let mut runs = vec![];
        runs_of(&d.blocks, &mut runs);
        assert!(runs.iter().all(|r| !r.text.contains("Diagram source")));
    }
    assert_eq!(found, 1);
    // the figure with an image keeps its blob, which the print holds
    let figures: Vec<_> = p
        .docs
        .iter()
        .flat_map(|d| {
            let mut b = vec![];
            all_blocks(&d.blocks, &mut b);
            b.into_iter()
                .filter_map(|b| match b {
                    PBlock::Figure { blob, .. } => *blob,
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!figures.is_empty());
    for h in figures {
        let blob = &p.blobs[&h];
        assert_eq!(blob.media_type, "image/png");
        assert_eq!(blob.file, format!("assets/{h}.png"));
        assert_eq!(blob.bytes, r.read().reader().get_blob(&h).unwrap());
    }
}

#[test]
fn ec3_a_view_scope_prints_without_review_marks() {
    let (_d, mut r, ids) = project();
    // a removed block, so the view's site shows a removed line
    write(
        &mut r,
        "editing",
        json!([{"op": "delete", "id": ids.opening_note.to_string()}]),
    )
    .expect("delete the note");
    let scope = Scope::View("editing".into());
    let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
    let tutorial = String::from_utf8(site.files["tutorial/adventure.html"].clone()).unwrap();
    assert!(tutorial.contains("chg-label"), "the site marks the edit");
    assert!(tutorial.contains("data-diff"), "the site holds its diff");
    let spec = String::from_utf8(site.files["specification/patterns.html"].clone()).unwrap();
    assert!(
        spec.contains("chg-removed"),
        "the site shows the removed line"
    );
    let p = print::build(&r.read(), &scope).unwrap();
    let mut all = String::new();
    for d in &p.docs {
        let mut runs = vec![];
        runs_of(&d.blocks, &mut runs);
        for r in &runs {
            let t = r.text.trim();
            assert!(
                !["Edited", "New", "Removed", "Show diff", "Show changes"].contains(&t),
                "a mark word in {}: {t}",
                d.page
            );
        }
        all.push_str(&text(&runs.into_iter().cloned().collect::<Vec<_>>()));
        all.push('\n');
    }
    // the view's text, and not the text it replaced or removed
    assert!(all.contains("They compose."), "the view's edit");
    assert!(!all.contains("which are strings, numbers"), "no diff");
    assert!(
        !all.contains("are *soft*") && !all.contains("are soft: see"),
        "no removed line"
    );
    assert!(all.contains("A note added while the overview is being edited."));
}

#[test]
fn ec3_inflight_is_refused() {
    let (_d, r, _ids) = project();
    let e = print::build(&r.read(), &Scope::Inflight)
        .err()
        .expect("refused");
    let want = rdy_engine::refuse_inflight_export(&Scope::Inflight).unwrap_err();
    assert_eq!(
        (e.code, e.message, e.fix),
        (want.code, want.message, want.fix)
    );
}

#[test]
fn ec3_two_builds_of_one_state_are_equal() {
    let (_d, r, _ids) = project();
    assert_eq!(canon(&r), canon(&r));
    assert_eq!(view(&r), view(&r));
}

/// Guard: building a print leaves the site's bytes as they were.
#[test]
fn ec3_the_site_build_is_unchanged() {
    let (_d, r, _ids) = project();
    for scope in [Scope::Canon, Scope::View("editing".into())] {
        let before = build_site(&r.read(), &scope, Mode::Static).unwrap();
        print::build(&r.read(), &scope).unwrap();
        let after = build_site(&r.read(), &scope, Mode::Static).unwrap();
        assert!(before.files == after.files, "{scope:?}: the same files");
        assert_eq!(before.label, after.label);
        assert_eq!(before.header, after.header);
        assert_eq!(before.label_at, after.label_at);
        assert_eq!(before.summary.warnings, after.summary.warnings);
        assert_eq!(
            (
                before.summary.pages,
                before.summary.assets,
                before.summary.redirects
            ),
            (
                after.summary.pages,
                after.summary.assets,
                after.summary.redirects
            )
        );
    }
}
