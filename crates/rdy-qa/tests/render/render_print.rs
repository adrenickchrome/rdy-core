//! ec3 (D-WQSAY, D-EVRNM, D-X61DT): the print model. One model of exactly what a reader sees on
//! the site (the words on the page, nothing a reader cannot see) feeds every print format.
use rdy_engine::{Rdy, Scope};
use rdy_model::Hash;
use rdy_qa::golden::*;
use rdy_render::print::{self, Href, PBlock, PageKind, Print, PrintDoc, Run};
use rdy_render::{Mode, SiteFiles, build_site};
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

/// Ruling task-6: a diagram's words are the same in the print and in llms-full, whatever its
/// shape: drawn (its "open full size" link is a reader control and adds no words), D2, and an
/// invalid, an undrawn and a failed Mermaid (each a code block with its language, then its
/// caption). Authored text that reads "open full size" stays in both.
#[test]
fn dgm6_print_and_llms_read_the_same_diagram_words() {
    let (_d, mut r, _ids) = project();
    let diagram = |lang: &str, alt: &str, body: &str| {
        json!({"op": "create", "type": "diagram", "parent": "$s",
            "fields": {"lang": lang, "alt": alt}, "body": body})
    };
    write(
        &mut r,
        "dgm6",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Diagram words", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Shapes"}},
            diagram("mermaid", "Drawn flow", "flowchart LR\n  A[start] --> B[end]"),
            diagram("d2", "D2 flow", "a -> b: goes"),
            diagram("mermaid", "Invalid flow", "flowchart LR\n A["),
            diagram("mermaid", "Undrawn pie", "pie\n  \"a\" : 1"),
            diagram("mermaid", "Failed flow",
                "flowchart LR\n    A --> B\n    classDef e fill:#f00\" onload=\"alert(1)\n    class A e"),
            {"op": "create", "type": "prose", "parent": "$s", "body": "open full size"}
        ]),
    )
    .expect("the diagrams");
    land(&mut r, "dgm6").expect("land them");
    let p = canon(&r);
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let full = String::from_utf8(site.files["llms-full.txt"].clone()).unwrap();
    let sections = llms_sections(&full);
    let docs = tree_docs(&p);
    assert_eq!(sections.len(), docs.len());
    let (d, section) = docs
        .iter()
        .zip(&sections)
        .find(|(d, _)| d.title == "Diagram words")
        .expect("the doc and its section");
    // the site draws the one flow and shows the other four as code blocks
    let page = String::from_utf8(site.files[&d.page].clone()).unwrap();
    assert_eq!(
        page.matches("<p class=\"diagram-open\">").count(),
        1,
        "{page}"
    );
    assert_eq!(
        page.matches("<div class=\"code-block\">").count(),
        4,
        "{page}"
    );
    assert_eq!(doc_words(d), llms_words(section), "{section}");
    // the control adds no words; the authored line is in both
    let authored = |l: &&str| l.trim() == "open full size";
    assert_eq!(section.lines().filter(authored).count(), 1, "{section}");
    assert!(
        d.blocks
            .iter()
            .any(|b| matches!(b, PBlock::Para { runs, .. } if text(runs) == "open full size")),
        "{:?}",
        d.blocks
    );
    // each fallback reads its language, its source, then its caption, in both
    for (label, alt) in [
        ("D2", "D2 flow"),
        ("Mermaid", "Invalid flow"),
        ("Mermaid", "Undrawn pie"),
        ("Mermaid", "Failed flow"),
    ] {
        let at = section.find(alt).expect(alt);
        let before = &section[..at];
        let lang = before.rfind(label).expect(label);
        assert!(before[lang..].contains("```"), "{alt}: {section}");
    }
    // the print draws none
    assert!(p.blobs.values().all(|b| !b.file.contains("diagrams")));
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
    let e = print::build(&r.read(), &Scope::Inflight).expect_err("refused");
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

/// The site's bytes, as one digest: every file (its path and bytes), the label, the header,
/// where the label and header sit, the warnings and the counts.
fn site_digest(s: &SiteFiles) -> String {
    let mut buf: Vec<u8> = vec![];
    let mut put = |b: &[u8]| {
        buf.extend_from_slice(&(b.len() as u64).to_le_bytes());
        buf.extend_from_slice(b);
    };
    for (path, bytes) in &s.files {
        put(path.as_bytes());
        put(bytes);
    }
    put(s.label.as_bytes());
    put(s.header.as_bytes());
    put(format!("{:?}", s.label_at).as_bytes());
    put(format!("{:?}", s.summary.warnings).as_bytes());
    put(format!(
        "{} {} {}",
        s.summary.pages, s.summary.assets, s.summary.redirects
    )
    .as_bytes());
    rdy_model::Hash::of(&buf).to_string()
}

/// The view `reserved`: top-level docs named `changes`, `changes-since` and `glossary`, the
/// three names the site keeps for its own pages.
fn reserved(r: &mut Rdy) -> Scope {
    write(
        r,
        "reserved",
        json!([
            {"op": "create", "type": "doc", "fields": {"name": "changes", "title": "Release notes", "doc_type": "overview"}},
            {"op": "create", "type": "doc", "fields": {"name": "changes-since", "title": "Since notes", "doc_type": "overview"}},
            {"op": "create", "type": "doc", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}}
        ]),
    )
    .expect("docs with the site's reserved names");
    Scope::View("reserved".into())
}

/// Guard (fix round 1, finding 3): the site's bytes equal the bytes the site build gave before
/// the print model's refactor (BASE d29bb4c, with ids seeded with 7). The digests were taken
/// from `build_site` at BASE, twice, before this commit's code existed; they are the oracle. A
/// change to the site's pages moves them: take them again from the build before that change.
/// site.css gained the HA2b Task 6 rules; CSS-only (ruling task-6).
/// The live profiles change (D-CWC91) changed `js/menu.js`, bundled in
/// `assets/app.js`: the menu `LABEL` becomes "Versions, proposals, views and profiles" and the
/// script fetches the pill's href in place of the hard-coded PAGE (with its same-origin http/https
/// check from fix rounds 2827fddd and 2f7023bd). Only `assets/app.js` changed; re-taken.
/// Re-taken after the reference period rule (ruling task-7): an automatic label drops its final period before an authored period; only that text moved.
/// Re-taken for the renderer plan's Task 8 (sidebar folders): the navigation markup of the pages,
/// the sidebar rules of `assets/site.css` and `assets/app.js` (which now bundles `js/sidebar.js`)
/// moved; their content rows keep their text (the gold1_ goldens).
/// Re-taken again in Task 8 fix round 1: only `js/sidebar.js` changed (byte-size bounds and absolute-URL
/// validation of the stored state), so only `assets/app.js` moved.
/// Re-taken for the renderer plan's Task 6 (native diagrams, ruling task-8): the diagram figures
/// of the pages that have one, their removed library script tag, the new `assets/diagrams/` files,
/// `assets/js/mermaid.min.js` (gone), the diagram rules of `assets/site.css`, `assets/app.js`
/// (without `js/mermaid-init.js`) and an `open full size` line per drawing in `llms-full.txt` moved.
#[test]
fn ec3_the_site_build_is_unchanged() {
    const BASE: [(&str, &str); 3] = [
        (
            "canon",
            "a75099e44625fe4568b6b29e0a77c29e0d4e7236fa6beb6ebd9aee3db8a718e4",
        ),
        (
            "view:editing",
            "ef3f931395c7f5fc23ff715622dcd2f1af8956d74f204d2c7fd324ba500a2e65",
        ),
        (
            "view:reserved",
            "d5955717afd1bcbe0c93275856e6abd88b88d16f638a489733a24c11fbd41380",
        ),
    ];
    // the ids the corpus's generated pages and the reserved docs draw come from a seeded stream
    rdy_model::seed_ids_for_tests(7);
    let (_d, mut r, _ids) = project();
    let mut got = vec![];
    for (name, scope) in [
        ("canon", Scope::Canon),
        ("view:editing", Scope::View("editing".into())),
    ] {
        let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
        got.push((name, site_digest(&site)));
    }
    // the reserved names: the site's warnings and the pages it leaves out
    let scope = reserved(&mut r);
    rdy_model::unseed_ids_for_tests();
    let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
    assert_eq!(
        site.summary.warnings.len(),
        3,
        "{:?}",
        site.summary.warnings
    );
    got.push(("view:reserved", site_digest(&site)));
    let got: Vec<(&str, &str)> = got.iter().map(|(n, d)| (*n, d.as_str())).collect();
    assert_eq!(got, BASE);
}

/// Building a print leaves the site's bytes as they were.
#[test]
fn ec3_a_print_leaves_the_site_build_as_it_was() {
    let (_d, r, _ids) = project();
    for scope in [Scope::Canon, Scope::View("editing".into())] {
        let before = build_site(&r.read(), &scope, Mode::Static).unwrap();
        print::build(&r.read(), &scope).unwrap();
        let after = build_site(&r.read(), &scope, Mode::Static).unwrap();
        assert_eq!(site_digest(&before), site_digest(&after), "{scope:?}");
    }
}

/// Fix round 1 (finding 4): the print keeps the site build's warnings for its scope, and prints
/// no change summary page.
#[test]
fn ec3_print_keeps_the_site_builds_warnings() {
    let (_d, mut r, _ids) = project();
    let scope = reserved(&mut r);
    for scope in [Scope::Canon, scope] {
        let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
        let p = print::build(&r.read(), &scope).unwrap();
        assert_eq!(p.warnings, site.summary.warnings, "{scope:?}");
        if scope != Scope::Canon {
            assert_eq!(p.warnings.len(), 3, "guard: {:?}", p.warnings);
        }
        // a doc named like a summary page prints as that doc
        for page in ["changes.html", "changes-since.html"] {
            for d in p.docs.iter().filter(|d| d.page == page) {
                assert!(matches!(d.kind, PageKind::Doc), "{page}");
                assert!(["Release notes", "Since notes"].contains(&d.title.as_str()));
            }
        }
        assert!(p.docs.iter().all(|d| d.title != "Our words"), "{scope:?}");
    }
}

/// Fix round 1 (finding 1): a figure the print leaves out (beneath a doc the site gives no page)
/// does not change the file or the media type of a printed figure that shows the same blob.
#[test]
fn ec3_a_figure_the_print_leaves_out_changes_no_printed_blob() {
    let (_d, mut r, ids) = project();
    let blob = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&ids.image_figure)
        .unwrap()
        .field_str("blob")
        .unwrap()
        .to_string();
    write(
        &mut r,
        "blob-alias",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Pictures"}},
            {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "The same bytes"}}
        ]),
    )
    .expect("a figure beneath a doc with no page");
    let scope = Scope::View("blob-alias".into());
    let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
    assert!(
        !site.files.contains_key(&format!("assets/{blob}.bin")),
        "guard: the site leaves the figure out"
    );
    let p = print::build(&r.read(), &scope).unwrap();
    let h: Hash = blob.parse().unwrap();
    let printed = &p.blobs[&h];
    assert_eq!(
        (printed.media_type.as_str(), printed.file.as_str()),
        ("image/png", format!("assets/{blob}.png").as_str())
    );
    // every blob of the print is a file of the site, with its bytes
    for (h, b) in &p.blobs {
        assert_eq!(site.files.get(&b.file), Some(&b.bytes), "{h}");
    }
}

/// Fix round 2: a download on a printed page shows no figure, so it names no printed blob, even
/// when it holds the bytes of an image on the same page (its file, `.bin`, comes first by path).
#[test]
fn ec3_a_download_beside_its_image_changes_no_printed_blob() {
    let (_d, mut r, ids) = project();
    let figure = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .get(&ids.image_figure)
        .unwrap()
        .clone();
    let blob = figure.field_str("blob").unwrap().to_string();
    let parent = figure.parent.expect("the figure has a parent").to_string();
    write(
        &mut r,
        "blob-download",
        json!([
            {"op": "create", "type": "figure", "parent": parent, "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "The same bytes"}}
        ]),
    )
    .expect("a download of the image's bytes beside it");
    let scope = Scope::View("blob-download".into());
    let site = build_site(&r.read(), &scope, Mode::Static).unwrap();
    let (bin, png) = (format!("assets/{blob}.bin"), format!("assets/{blob}.png"));
    let p = print::build(&r.read(), &scope).unwrap();
    // guard: one printed page shows the image and links the download of its bytes
    let both = p.docs.iter().find(|d| {
        let html = String::from_utf8_lossy(&site.files[&d.page]).into_owned();
        html.contains(&format!("{blob}.bin\" download")) && html.contains(&format!("{blob}.png\""))
    });
    assert!(
        both.is_some(),
        "guard: the image and the download share a page"
    );
    assert!(
        site.files.contains_key(&bin) && site.files.contains_key(&png),
        "guard: the site writes both files"
    );
    let h: Hash = blob.parse().unwrap();
    let printed = &p.blobs[&h];
    assert_eq!(
        (printed.media_type.as_str(), printed.file.as_str()),
        ("image/png", png.as_str())
    );
    for (h, b) in &p.blobs {
        assert_eq!(site.files.get(&b.file), Some(&b.bytes), "{h}");
    }
}

/// Every page link of the print leads to a page it holds and, with an anchor, to an anchor a
/// block of that page has.
fn assert_links_resolve(p: &Print) {
    let anchors: std::collections::BTreeMap<&str, Vec<&str>> = p
        .docs
        .iter()
        .map(|d| {
            let mut blocks = vec![];
            all_blocks(&d.blocks, &mut blocks);
            let a = blocks
                .into_iter()
                .filter_map(|b| match b {
                    PBlock::Heading { anchor, .. }
                    | PBlock::Para { anchor, .. }
                    | PBlock::Table { anchor, .. }
                    | PBlock::Code { anchor, .. }
                    | PBlock::Figure { anchor, .. }
                    | PBlock::Callout { anchor, .. } => anchor.as_deref(),
                    PBlock::List { .. } => None,
                })
                .collect();
            (d.page.as_str(), a)
        })
        .collect();
    let mut with_anchor = 0;
    for d in &p.docs {
        let mut runs = vec![];
        runs_of(&d.blocks, &mut runs);
        for r in runs {
            if let Some(Href::Page { page, anchor }) = &r.link {
                let on = anchors
                    .get(page.as_str())
                    .unwrap_or_else(|| panic!("{page} from {}", d.page));
                if let Some(a) = anchor {
                    assert!(on.contains(&a.as_str()), "{page}#{a} from {}", d.page);
                    with_anchor += 1;
                }
            }
        }
    }
    assert!(
        with_anchor > 10,
        "the corpus links to anchors: {with_anchor}"
    );
}

/// Fix round 1 (finding 2): an authored link to a block's id (which the site's page holds as the
/// block's id anchor) goes to the anchor the print gives that block.
#[test]
fn ec3_a_link_to_a_block_id_goes_to_its_printed_anchor() {
    let (_d, mut r, ids) = project();
    let guards = ids.guards_section.to_string();
    write(
        &mut r,
        "xref",
        json!([{"op": "create", "type": "prose", "parent": guards,
            "body": format!("[Read guards](#{guards})")}]),
    )
    .expect("a link to a block id");
    land(&mut r, "xref").expect("land it");
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let html = String::from_utf8(site.files["specification/patterns.html"].clone()).unwrap();
    assert!(
        html.contains(&format!("href=\"#{guards}\"")) && html.contains(&format!("id=\"{guards}\"")),
        "guard: the site's link works through the block's id anchor"
    );
    let p = canon(&r);
    let spec = p
        .docs
        .iter()
        .find(|d| d.id == ids.spec_doc.to_string())
        .unwrap();
    let anchor = spec
        .blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Heading { anchor, runs, .. } if text(runs) == "Guards" => anchor.clone(),
            _ => None,
        })
        .expect("the section's heading has its anchor");
    let link = spec
        .blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Para { runs, .. } if text(runs) == "Read guards" => runs[0].link.clone(),
            _ => None,
        })
        .expect("the new paragraph");
    assert_eq!(
        link,
        Href::Page {
            page: spec.page.clone(),
            anchor: Some(anchor),
        }
    );
    assert_links_resolve(&p);
    assert_links_resolve(&view(&r));
}
