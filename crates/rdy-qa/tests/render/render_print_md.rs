//! ec6 (D-K7ZFK, D-WQSAY, O28, O29): the print as a tree of Markdown files, with an MkDocs layout.
use rdy_engine::{E_BAD_INPUT, Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::print::md::{Layout, write_markdown};
use rdy_render::print::{self, Blob, Href, PBlock, PageKind, Print, PrintDoc, Run};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every file under `dir`: its path (`/` separators, relative to `dir`) and bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).expect("read dir") {
            let path = e.expect("entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let rel = path.strip_prefix(base).unwrap();
                let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
                out.insert(rel.join("/"), std::fs::read(&path).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("a Markdown file is UTF-8")
}

/// The print of `scope`, written as a Markdown tree in a new directory.
fn exported(r: &Rdy, scope: &Scope, layout: Layout) -> (tempfile::TempDir, PathBuf, Print) {
    let p = print::build(&r.read(), scope).expect("print");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("md");
    write_markdown(&p, &out, layout).expect("write the tree");
    (dir, out, p)
}

/// The `.md` file of a site page.
fn md_of(page: &str) -> String {
    page.strip_suffix(".html")
        .expect("an html page")
        .to_string()
        + ".md"
}

/// A file split into its front matter (parsed) and its body.
fn split(file: &str) -> (serde_yaml_ng::Mapping, String) {
    let rest = file.strip_prefix("---\n").expect("front matter opens");
    let (front, body) = rest.split_once("\n---\n").expect("front matter closes");
    let map: serde_yaml_ng::Mapping = serde_yaml_ng::from_str(front).expect("front matter is YAML");
    (map, body.to_string())
}

fn scopes() -> [Scope; 2] {
    [Scope::Canon, Scope::View("editing".into())]
}

#[test]
fn ec6_each_doc_is_one_md_file_at_its_site_path() {
    let (_d, r, _ids) = project();
    for scope in scopes() {
        let (_dir, out, p) = exported(&r, &scope, Layout::Plain);
        let files = tree(&out);
        let mut want: Vec<String> = p.docs.iter().map(|d| md_of(&d.page)).collect();
        want.push("index.md".into());
        want.extend(p.blobs.values().map(|b| b.file.clone()));
        want.sort();
        assert_eq!(files.keys().cloned().collect::<Vec<_>>(), want, "{scope:?}");
        for d in &p.docs {
            let file = text_of(&files[&md_of(&d.page)]);
            let (front, body) = split(&file);
            assert_eq!(
                front.get("title").and_then(|t| t.as_str()),
                Some(d.title.as_str()),
                "{}",
                d.page
            );
            if d.kind == PageKind::Glossary {
                assert_eq!(front.len(), 1, "the glossary is no block: it has no id");
                assert_eq!(d.page, "glossary.html");
            } else {
                assert_eq!(
                    front.get("rdy_id").and_then(|t| t.as_str()),
                    Some(d.id.as_str()),
                    "{}",
                    d.page
                );
                assert_eq!(front.len(), 2);
            }
            // O28 (c): the title is a JSON string; (b): the heading follows the front matter
            assert!(
                file.starts_with(&format!(
                    "---\ntitle: {}\n",
                    serde_json::to_string(&d.title).unwrap()
                )),
                "{}",
                d.page
            );
            let heading = body.lines().nth(1).expect("a heading line");
            assert_eq!(body.lines().next(), Some(""), "{}", d.page);
            assert_eq!(
                unescape(heading.strip_prefix("# ").expect("a # heading")),
                d.title,
                "{}",
                d.page
            );
        }
        // a folder is its folder's index.md
        for d in p.docs.iter().filter(|d| d.kind == PageKind::Folder) {
            assert!(d.page.ends_with("/index.html"));
            assert!(files.contains_key(&md_of(&d.page)));
        }
        // O28 (a): the title part is index.md
        let index = text_of(&files["index.md"]);
        let (front, body) = split(&index);
        assert_eq!(front.len(), 1);
        assert_eq!(
            front.get("title").and_then(|t| t.as_str()),
            Some(p.title.as_str())
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], format!("# {}", p.title));
        assert!(
            lines.contains(&format!("Version: {}", p.label).as_str()),
            "{index}"
        );
        assert!(
            lines.contains(&format!("Scope: {}", p.scope).as_str()),
            "{index}"
        );
        assert!(index.contains(&p.description.clone().unwrap()), "{index}");
    }
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

#[test]
fn ec6_refs_become_relative_md_links_with_anchors() {
    let (_d, mut r, ids) = project();
    write(
        &mut r,
        "xref",
        json!([{"op": "create", "type": "prose", "parent": ids.guards_section.to_string(),
            "body": "Read {{s-QJ383}} next."}]),
    )
    .expect("a cross-doc reference");
    land(&mut r, "xref").expect("land it");
    let (_dir, out, p) = exported(&r, &Scope::Canon, Layout::Plain);
    let files = tree(&out);
    let tutorial = p
        .docs
        .iter()
        .find(|d| d.id == ids.tutorial_doc.to_string())
        .unwrap();
    let spec = p
        .docs
        .iter()
        .find(|d| d.id == ids.spec_doc.to_string())
        .unwrap();
    assert_eq!(
        (tutorial.page.as_str(), spec.page.as_str()),
        ("tutorial/adventure.html", "specification/patterns.html")
    );
    let anchor = tutorial
        .blocks
        .iter()
        .find_map(|b| match b {
            PBlock::Heading { anchor, runs, .. }
                if runs.iter().map(|r| r.text.as_str()).collect::<String>()
                    == "Composing patterns" =>
            {
                anchor.clone()
            }
            _ => None,
        })
        .expect("the section's anchor");
    let spec_md = text_of(&files["specification/patterns.md"]);
    let link = format!("[Composing patterns](../tutorial/adventure.md#{anchor})");
    assert!(
        spec_md
            .lines()
            .any(|l| l.contains(&format!("Read {link} next."))),
        "{spec_md}"
    );
    // the anchor line precedes the heading in the target
    let tutorial_md = text_of(&files["tutorial/adventure.md"]);
    let lines: Vec<&str> = tutorial_md.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == format!("<a id=\"{anchor}\"></a>"))
        .expect("the anchor line");
    assert!(
        lines[at..at + 3]
            .iter()
            .any(|l| l.starts_with('#') && l.contains("Composing patterns")),
        "{:?}",
        &lines[at..at + 3]
    );
    // the title part holds no link
    assert!(!text_of(&files["index.md"]).contains("]("));
}

/// Every `](target)` of a file's body, with image targets marked.
fn targets(body: &str) -> Vec<(bool, String)> {
    let re = regex::Regex::new(r"(!?)\[(?:[^\]\\]|\\.)*\]\(([^)\s]*)\)").unwrap();
    re.captures_iter(body)
        .map(|c| (&c[1] == "!", c[2].to_string()))
        .collect()
}

/// `path` joined to `dir`, with `.` and `..` resolved. `None` when it leaves the tree.
fn join(dir: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in path.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[test]
fn ec6_every_link_target_file_and_anchor_exists() {
    let (_d, r, _ids) = project();
    for scope in scopes() {
        let (_dir, out, p) = exported(&r, &scope, Layout::Plain);
        let files = tree(&out);
        let (mut with_anchor, mut images) = (0, 0);
        for (path, bytes) in files.iter().filter(|(p, _)| p.ends_with(".md")) {
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
            for (image, target) in targets(&text_of(bytes)) {
                if target.starts_with("http://")
                    || target.starts_with("https://")
                    || target.starts_with("mailto:")
                {
                    continue;
                }
                let (file, anchor) = match target.split_once('#') {
                    Some((f, a)) => (f, Some(a)),
                    None => (target.as_str(), None),
                };
                let at = join(dir, file).unwrap_or_else(|| panic!("{target} leaves {path}"));
                let dest = files
                    .get(&at)
                    .unwrap_or_else(|| panic!("{path}: no file {at} for {target}"));
                if image {
                    images += 1;
                    assert!(at.starts_with("assets/"), "{at}");
                    continue;
                }
                assert!(at.ends_with(".md"), "{path}: {target}");
                if let Some(a) = anchor {
                    with_anchor += 1;
                    assert!(
                        text_of(dest).contains(&format!("<a id=\"{a}\"></a>")),
                        "{path}: no anchor {a} in {at}"
                    );
                }
            }
        }
        assert!(with_anchor > 10, "{scope:?}: {with_anchor}");
        assert!(images > 0);
        assert!(!p.blobs.is_empty());
    }
}

#[test]
fn ec6_ids_appear_only_in_front_matter() {
    let (_d, r, _ids) = project();
    for scope in scopes() {
        let (_dir, out, p) = exported(&r, &scope, Layout::Plain);
        let files = tree(&out);
        let m = r.read().materialized(&scope).unwrap();
        let ids: Vec<String> = m.ids().map(|id| id.to_string()).collect();
        assert!(ids.len() > 100);
        let mut in_front = 0;
        for (path, bytes) in files.iter().filter(|(p, _)| p.ends_with(".md")) {
            let (front, body) = split(&text_of(bytes));
            for id in &ids {
                assert!(!body.contains(id.as_str()), "{scope:?} {path}: {id}");
            }
            in_front += usize::from(front.contains_key("rdy_id"));
        }
        assert_eq!(
            in_front,
            p.docs
                .iter()
                .filter(|d| d.kind != PageKind::Glossary)
                .count()
        );
        // no file name or asset name holds an id either
        for path in files.keys() {
            for id in &ids {
                assert!(!path.contains(id.as_str()), "{path}");
            }
        }
    }
}

#[test]
fn ec6_a_diagram_is_a_fenced_block_with_its_language() {
    let (_d, r, _ids) = project();
    let (_dir, out, _p) = exported(&r, &Scope::Canon, Layout::Plain);
    let found: Vec<(String, String)> = tree(&out)
        .into_iter()
        .filter(|(p, _)| p.ends_with(".md"))
        .map(|(p, b)| (p, text_of(&b)))
        .filter(|(_, t)| t.contains("CB[Case block] --> P[Pattern]"))
        .collect();
    assert_eq!(found.len(), 1);
    let lines: Vec<&str> = found[0].1.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("CB[Case block] --> P[Pattern]"))
        .unwrap();
    let open = lines[..at]
        .iter()
        .rposition(|l| l.starts_with("```"))
        .expect("an opening fence");
    assert_eq!(lines[open], "```mermaid");
    assert_eq!(lines[open + 1], "flowchart TD");
    let close = lines[at..]
        .iter()
        .position(|l| *l == "```")
        .expect("a close")
        + at;
    assert!(close > at);
    assert_eq!(lines[close + 1], "", "a blank line follows the fence");
}

/// A print of one doc, built by hand.
fn hand(blocks: Vec<PBlock>, blobs: BTreeMap<rdy_model::Hash, Blob>) -> Print {
    Print {
        title: "Hand built".into(),
        description: None,
        label: "v1".into(),
        scope: "canon".into(),
        tag_redacted: false,
        docs: vec![
            PrintDoc {
                kind: PageKind::Folder,
                id: "s-AAAAA".into(),
                title: "Top".into(),
                page: "top/index.html".into(),
                depth: 0,
                blocks: vec![],
            },
            PrintDoc {
                kind: PageKind::Doc,
                id: "s-BBBBB".into(),
                title: "Deep *doc*".into(),
                page: "top/doc.html".into(),
                depth: 1,
                blocks,
            },
        ],
        blobs,
        warnings: vec![],
        // a hand-built print has no project; the Markdown writer does not read it
        rdy_dir: std::path::PathBuf::new(),
    }
}

fn run(text: &str) -> Run {
    Run {
        text: text.into(),
        bold: false,
        italic: false,
        code: false,
        link: None,
    }
}

#[test]
fn ec6_a_fence_is_longer_than_any_backtick_run_inside_it() {
    let code = |text: &str| PBlock::Code {
        anchor: None,
        lang: Some("rust".into()),
        text: text.into(),
    };
    let p = hand(
        vec![
            code("let a = \"```\";\nlet b = \"`\";"),
            code("plain"),
            code("````\nx"),
        ],
        BTreeMap::new(),
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    let md = text_of(&std::fs::read(out.join("top/doc.md")).unwrap());
    assert!(
        md.contains("````rust\nlet a = \"```\";\nlet b = \"`\";\n````\n"),
        "{md}"
    );
    assert!(md.contains("```rust\nplain\n```\n"), "{md}");
    assert!(md.contains("`````rust\n````\nx\n`````\n"), "{md}");
    // the title's Markdown characters are escaped in the heading, not in the front matter
    assert!(md.contains("\n# Deep \\*doc\\*\n"), "{md}");
    assert!(md.starts_with("---\ntitle: \"Deep *doc*\"\nrdy_id: s-BBBBB\n---\n"));
}

#[test]
fn ec6_tables_lists_callouts_and_runs_are_plain_markdown() {
    let mut bold = run("strong");
    bold.bold = true;
    let mut code = run("a`b");
    code.code = true;
    let mut link = run("here");
    link.link = Some(Href::Page {
        page: "top/index.html".into(),
        anchor: None,
    });
    let mut out_link = run("web");
    out_link.link = Some(Href::External("https://example.com/a b".into()));
    let blocks = vec![
        PBlock::Para {
            anchor: Some("intro".into()),
            runs: vec![
                run("- not a list "),
                bold,
                run(" "),
                code,
                run(" "),
                link,
                run(" "),
                out_link,
                run("\nsecond line"),
            ],
        },
        PBlock::Table {
            anchor: None,
            head: vec![vec![run("a|b")], vec![run("c")]],
            rows: vec![vec![vec![run("x\ny")], vec![run("z")]]],
        },
        PBlock::List {
            ordered: true,
            start: 9,
            items: vec![
                vec![
                    PBlock::Para {
                        anchor: None,
                        runs: vec![run("nine")],
                    },
                    PBlock::List {
                        ordered: false,
                        start: 1,
                        items: vec![vec![PBlock::Para {
                            anchor: Some("deep".into()),
                            runs: vec![run("inner")],
                        }]],
                    },
                ],
                vec![PBlock::Para {
                    anchor: None,
                    runs: vec![run("ten")],
                }],
            ],
        },
        PBlock::Callout {
            anchor: None,
            label: "Note".into(),
            blocks: vec![PBlock::Para {
                anchor: None,
                runs: vec![run("careful")],
            }],
        },
    ];
    let p = hand(blocks, BTreeMap::new());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    let md = text_of(&std::fs::read(out.join("top/doc.md")).unwrap());
    let (_, body) = split(&md);
    let want = "\n# Deep \\*doc\\*\n\n\
        <a id=\"intro\"></a>\n\
        \\- not a list **strong** ``a`b`` [here](index.md) [web](https://example.com/a%20b)\\\n\
        second line\n\n\
        | a\\|b | c |\n| --- | --- |\n| x y | z |\n\n\
        9.  nine\n\n    -   <a id=\"deep\"></a>\n        inner\n10. ten\n\n\
        > **Note**\n>\n> careful\n";
    assert_eq!(body, want);
}

#[test]
fn ec6_figures_are_written_under_assets() {
    let (_d, r, _ids) = project();
    let (_dir, out, p) = exported(&r, &Scope::Canon, Layout::Plain);
    let files = tree(&out);
    assert!(!p.blobs.is_empty());
    for (h, b) in &p.blobs {
        assert_eq!(b.file, format!("assets/{h}.png"));
        assert_eq!(files.get(&b.file), Some(&b.bytes), "{h}");
    }
    assert_eq!(
        files.keys().filter(|k| k.starts_with("assets/")).count(),
        p.blobs.len()
    );
    // a figure is the image with a path relative to the page, then its caption
    let mut seen = std::collections::BTreeSet::new();
    for d in &p.docs {
        let md = text_of(&files[&md_of(&d.page)]);
        let up = "../".repeat(d.page.matches('/').count());
        for (image, target) in targets(&md) {
            if image {
                let file = target.strip_prefix(&up).expect("relative to the page");
                assert!(file.starts_with("assets/"), "{target}");
                assert!(p.blobs.values().any(|b| b.file == file), "{target}");
                seen.insert(file.to_string());
            }
        }
    }
    assert_eq!(seen.len(), p.blobs.len(), "every blob is shown");
}

#[test]
fn ec6_mkdocs_layout_writes_mkdocs_yml_with_the_tree_as_nav() {
    let (_d, r, _ids) = project();
    let (_dir, out, p) = exported(&r, &Scope::Canon, Layout::Mkdocs);
    let files = tree(&out);
    // the tree is under docs/, and the only other file is mkdocs.yml
    assert!(
        files
            .keys()
            .all(|k| k == "mkdocs.yml" || k.starts_with("docs/")),
        "{:?}",
        files.keys().collect::<Vec<_>>()
    );
    let (_d2, plain, _p) = exported(&r, &Scope::Canon, Layout::Plain);
    let plain = tree(&plain);
    for (path, bytes) in &plain {
        assert_eq!(files.get(&format!("docs/{path}")), Some(bytes), "{path}");
    }
    assert_eq!(files.len(), plain.len() + 1);
    // the file: site_name, then the nav in tree order
    let yml = text_of(&files["mkdocs.yml"]);
    let json = |s: &str| serde_json::to_string(s).unwrap();
    let mut want = format!("site_name: {}\nnav:\n", json(&p.title));
    want.push_str(&format!("  - {}: index.md\n", json(&p.title)));
    for d in &p.docs {
        let pad = " ".repeat(2 + 4 * d.depth);
        match d.kind {
            PageKind::Folder => {
                let inner = " ".repeat(2 + 4 * (d.depth + 1));
                want.push_str(&format!("{pad}- {}:\n", json(&d.title)));
                want.push_str(&format!(
                    "{inner}- {}: {}\n",
                    json(&d.title),
                    md_of(&d.page)
                ));
            }
            PageKind::Doc => {
                want.push_str(&format!("{pad}- {}: {}\n", json(&d.title), md_of(&d.page)))
            }
            PageKind::Glossary => want.push_str("  - Glossary: glossary.md\n"),
        }
    }
    assert_eq!(yml, want);
    assert!(yml.ends_with("  - Glossary: glossary.md\n"));
    // it is YAML, and every path it names is a file
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yml).expect("YAML");
    assert_eq!(v["site_name"].as_str(), Some(p.title.as_str()));
    fn paths(v: &serde_yaml_ng::Value, out: &mut Vec<String>) {
        match v {
            serde_yaml_ng::Value::Sequence(s) => s.iter().for_each(|i| paths(i, out)),
            serde_yaml_ng::Value::Mapping(m) => m.values().for_each(|i| paths(i, out)),
            serde_yaml_ng::Value::String(s) => out.push(s.clone()),
            _ => {}
        }
    }
    let mut named = vec![];
    paths(&v["nav"], &mut named);
    let named: Vec<_> = named.into_iter().filter(|s| s.ends_with(".md")).collect();
    assert_eq!(named.len(), p.docs.len() + 1);
    for n in named {
        assert!(files.contains_key(&format!("docs/{n}")), "{n}");
    }
    assert!(v["nav"].as_sequence().unwrap().len() >= 3);
}

#[test]
fn ec6_a_non_empty_out_is_refused_and_untouched() {
    let (project_dir, r, _ids) = project();
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let before = |out: &Path| {
        let mut all: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        all.sort();
        (tree_or_file(out), all)
    };
    // a directory with a file
    let out = dir.path().join("full");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep.txt"), b"keep").unwrap();
    let was = before(&out);
    for layout in [Layout::Plain, Layout::Mkdocs] {
        let e = write_markdown(&p, &out, layout).expect_err("refused");
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(e.message, format!("{} is not empty", out.display()));
        assert_eq!(
            e.fix,
            "choose an empty directory (or one that does not exist yet) so no stale files mix into the export"
        );
        assert_eq!(before(&out), was);
    }
    // a file
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"data").unwrap();
    let was = before(&file);
    let e = write_markdown(&p, &file, Layout::Plain).expect_err("a file");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!(
            "{} is a file: a Markdown export writes a directory",
            file.display()
        )
    );
    assert_eq!(e.fix, "pass a directory path such as ./docs-md");
    assert_eq!(before(&file), was);
    // O29: no parent, no file name, inside .rdy
    let lost = dir.path().join("nope").join("md");
    let e = write_markdown(&p, &lost, Layout::Plain).expect_err("no parent");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!("{} does not exist", dir.path().join("nope").display())
    );
    assert_eq!(e.fix, "create the directory first, or pick another path");
    let e = write_markdown(&p, Path::new("/"), Layout::Plain).expect_err("no name");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "/ does not name a directory");
    assert_eq!(e.fix, "pass a directory path such as ./docs-md");
    // the `.rdy` directory of the project the print is of (ec11 fix round 1: O29 is that
    // project's store, not any directory named `.rdy`)
    let inside = project_dir.path().join(".rdy").join("md");
    let e = write_markdown(&p, &inside, Layout::Plain).expect_err("inside .rdy");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!("{} is inside the project .rdy directory", inside.display())
    );
    assert_eq!(e.fix, "export to a path outside .rdy");
    assert!(!inside.exists());
    // nothing was left beside the refused outs
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a-file", "full"]);
    // an empty directory is filled
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let s = write_markdown(&p, &empty, Layout::Plain).expect("an empty out is fine");
    assert_eq!(s.format, "md");
    assert!(empty.join("index.md").is_file());
    assert!(
        !dir.path().join(".empty.rdy-export-partial").exists(),
        "the staging directory is gone after the rename"
    );
    // the summary
    assert_eq!(s.out, empty.display().to_string());
    assert_eq!(
        s.docs,
        p.docs.iter().filter(|d| d.kind == PageKind::Doc).count()
    );
    assert_eq!(
        s.folders,
        p.docs.iter().filter(|d| d.kind == PageKind::Folder).count()
    );
    assert_eq!(s.assets, p.blobs.len());
    assert_eq!(s.pages, None);
    assert_eq!(s.warnings, p.warnings);
    let total: usize = tree(&empty).values().map(Vec::len).sum();
    assert_eq!(s.bytes, total as u64);
}

fn tree_or_file(p: &Path) -> BTreeMap<String, Vec<u8>> {
    if p.is_dir() {
        tree(p)
    } else {
        BTreeMap::from([(String::new(), std::fs::read(p).unwrap())])
    }
}

#[test]
fn ec6_two_exports_are_the_same_bytes() {
    let (_d, r, _ids) = project();
    for scope in scopes() {
        for layout in [Layout::Plain, Layout::Mkdocs] {
            let (_a, one, _) = exported(&r, &scope, layout);
            let (_b, two, _) = exported(&r, &scope, layout);
            assert_eq!(tree(&one), tree(&two), "{scope:?}");
        }
    }
}

/// The refusal of an occupied staging path, and that the path is left as it was.
fn assert_staging_refused(p: &Print, out: &Path, staging: &Path, layout: Layout) {
    let e = write_markdown(p, out, layout).expect_err("an occupied staging path is refused");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!(
            "{} already exists: a Markdown export may be running, or an earlier one stopped",
            staging.display()
        )
    );
    assert_eq!(e.fix, "if no export is running, remove it and export again");
}

#[test]
fn ec6_an_occupied_staging_path_is_refused_and_never_deleted() {
    let (_d, r, _ids) = project();
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    for layout in [Layout::Plain, Layout::Mkdocs] {
        // out does not exist; the staging path is a user's file
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("md");
        let staging = dir.path().join(".md.rdy-export-partial");
        std::fs::write(&staging, b"the user's data").unwrap();
        assert_staging_refused(&p, &out, &staging, layout);
        assert_eq!(std::fs::read(&staging).unwrap(), b"the user's data");
        assert!(!out.exists());
        // out does not exist; the staging path is a directory that holds files (a competing export)
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("md");
        let staging = dir.path().join(".md.rdy-export-partial");
        std::fs::create_dir_all(staging.join("sub")).unwrap();
        std::fs::write(staging.join("sub").join("busy"), b"in use").unwrap();
        let was = tree(&staging);
        assert_staging_refused(&p, &out, &staging, layout);
        assert_eq!(tree(&staging), was);
        assert!(!out.exists());
        // out is an empty directory: it stays, and the staging path stays
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("md");
        std::fs::create_dir(&out).unwrap();
        let staging = dir.path().join(".md.rdy-export-partial");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("busy"), b"in use").unwrap();
        let was = tree(&staging);
        assert_staging_refused(&p, &out, &staging, layout);
        assert_eq!(tree(&staging), was);
        assert!(out.is_dir() && std::fs::read_dir(&out).unwrap().next().is_none());
        // once the user removes it, the export goes through
        std::fs::remove_dir_all(&staging).unwrap();
        write_markdown(&p, &out, layout).expect("exports");
        assert!(!staging.exists());
    }
}

#[cfg(unix)]
#[test]
fn ec6_a_staging_path_that_cannot_be_created_leaves_the_directory_alone() {
    let (_d, r, _ids) = project();
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    std::fs::write(dir.path().join("neighbour"), b"mine").unwrap();
    rdy_qa::platform::set_mode(dir.path(), 0o555).unwrap();
    let writable = std::fs::File::create(dir.path().join("probe")).is_ok();
    let result = write_markdown(&p, &out, Layout::Plain);
    rdy_qa::platform::set_mode(dir.path(), 0o755).unwrap();
    if writable {
        // a user the mode does not bind (root): nothing to test; RDY_QA_REQUIRE_ALL=1 fails it
        rdy_qa::skip::skip(
            "ec6_a_staging_path_that_cannot_be_created_leaves_the_directory_alone",
            rdy_qa::skip::Why::Prerequisite,
            "a read-only directory takes writes here (running as root?)",
        );
        return;
    }
    let e = result.expect_err("the staging directory cannot be created");
    assert_eq!(e.code, rdy_engine::E_IO);
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["neighbour"]);
}

/// The file of the doc "Deep *doc*" in a hand-built print.
fn doc_md(p: &Print) -> String {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(p, &out, Layout::Plain).unwrap();
    text_of(&std::fs::read(out.join("top/doc.md")).unwrap())
}

/// What a CommonMark reader shows of an ATX heading line: the text, without the opening hashes
/// and the closing sequence.
fn atx_text(line: &str) -> String {
    let rest = line
        .trim_start_matches('#')
        .strip_prefix(' ')
        .expect("a # line");
    let t = rest.trim_end();
    let body = t.trim_end_matches('#');
    let text = if body.is_empty() {
        ""
    } else if body.ends_with(' ') && body.len() < t.len() {
        body.trim_end()
    } else {
        t
    };
    unescape(text)
}

/// The lines that a setext reader would take as an underline of the line before them.
fn setext_underlines(md: &str) -> Vec<String> {
    let lines: Vec<&str> = md.lines().collect();
    (1..lines.len())
        .filter(|&i| {
            let t = lines[i].trim();
            !lines[i - 1].trim().is_empty()
                && !t.is_empty()
                && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'))
        })
        .map(|i| lines[i].to_string())
        .collect()
}

#[test]
fn ec6_literal_entities_stay_literal() {
    let mut code = run("&copy;");
    code.code = true;
    let mut link = run("&lt;b&gt;");
    link.link = Some(Href::External("https://example.com/?a=1&copy;=2".into()));
    let mut p = hand(
        vec![
            PBlock::Para {
                anchor: None,
                runs: vec![
                    run("&copy; and &#169; and &#xA9; and &amp; but AT&T and 5 & 6 "),
                    code,
                    run(" "),
                    link,
                ],
            },
            PBlock::Figure {
                anchor: None,
                blob: None,
                alt: String::new(),
                caption: vec![run("&nbsp;caption")],
            },
            PBlock::Table {
                anchor: None,
                head: vec![vec![run("&reg;")]],
                rows: vec![vec![vec![run("&amp;")]]],
            },
        ],
        BTreeMap::new(),
    );
    p.description = Some("Uses &trade; and\n&#38;.".into());
    let md = doc_md(&p);
    // each entity-like text opens with `&amp;`, which every reader shows as `&`
    assert!(
        md.contains(
            "&amp;copy; and &amp;#169; and &amp;#xA9; and &amp;amp; but AT&T and 5 & 6 `&copy;` [&amp;lt;b&amp;gt;](https://example.com/?a=1&amp;copy;=2)"
        ),
        "{md}"
    );
    assert!(md.contains("&amp;nbsp;caption"), "{md}");
    assert!(md.contains("| &amp;reg; |"), "{md}");
    assert!(md.contains("| &amp;amp; |"), "{md}");
    // in a code span the text is verbatim
    assert!(md.contains("`&copy;`"), "{md}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    let index = text_of(&std::fs::read(out.join("index.md")).unwrap());
    assert!(
        index.contains("Uses &amp;trade; and\n&amp;#38;."),
        "{index}"
    );
}

#[test]
fn ec6_text_never_becomes_a_setext_heading_or_a_fence() {
    let mut p = hand(
        vec![
            PBlock::Para {
                anchor: None,
                runs: vec![run("a\nb\n===")],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("c\n  ==  ")],
            },
            PBlock::Para {
                anchor: None,
                runs: vec![run("= x\n~~~\nlast")],
            },
            PBlock::Para {
                anchor: Some("a".into()),
                runs: vec![run("===")],
            },
            PBlock::Figure {
                anchor: None,
                blob: None,
                alt: String::new(),
                caption: vec![run("cap\n===")],
            },
            PBlock::List {
                ordered: false,
                start: 1,
                items: vec![vec![PBlock::Para {
                    anchor: None,
                    runs: vec![run("item\n===")],
                }]],
            },
        ],
        BTreeMap::new(),
    );
    p.description = Some("first\n===\n\nsecond\n=".into());
    let md = doc_md(&p);
    assert_eq!(
        setext_underlines(&split(&md).1),
        Vec::<String>::new(),
        "{md}"
    );
    // the text of each line still reads as written: `&#61;` is `=`
    assert!(md.contains("a\\\nb\\\n&#61;==\n"), "{md}");
    assert!(md.contains("c\\\n  &#61;=  \n"), "{md}");
    assert!(
        md.contains("= x\\\n&#126;~~\\\nlast\n"),
        "a line with other text is no underline: {md}"
    );
    assert!(md.contains("<a id=\"a\"></a>\n&#61;=="), "{md}");
    assert!(md.contains("cap\\\n&#61;=="), "{md}");
    assert!(md.contains("-   item\\\n    &#61;=="), "{md}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    let index = text_of(&std::fs::read(out.join("index.md")).unwrap());
    assert_eq!(
        setext_underlines(&split(&index).1),
        Vec::<String>::new(),
        "{index}"
    );
    assert!(index.contains("first\n&#61;=="), "{index}");
    assert!(index.contains("second\n&#61;\n"), "{index}");
}

#[test]
fn ec6_a_trailing_hash_in_a_title_is_text_not_a_closing_sequence() {
    let mut p = hand(
        vec![
            PBlock::Heading {
                level: 2,
                anchor: None,
                runs: vec![run("Part #")],
            },
            PBlock::Heading {
                level: 3,
                anchor: Some("h".into()),
                runs: vec![run("#")],
            },
        ],
        BTreeMap::new(),
    );
    p.title = "Release #".into();
    p.docs[0].title = "Top ##".into();
    p.docs[1].title = "Deep *doc* #".into();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("md");
    write_markdown(&p, &out, Layout::Plain).unwrap();
    let heading = |file: &str, level: usize| {
        let md = text_of(&std::fs::read(out.join(file)).unwrap());
        let prefix = format!("{} ", "#".repeat(level));
        md.lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("a level {level} heading in {md}"))
            .to_string()
    };
    assert_eq!(atx_text(&heading("index.md", 1)), "Release #");
    assert_eq!(atx_text(&heading("top/index.md", 1)), "Top ##");
    assert_eq!(atx_text(&heading("top/doc.md", 1)), "Deep *doc* #");
    assert_eq!(atx_text(&heading("top/doc.md", 2)), "Part #");
    assert_eq!(atx_text(&heading("top/doc.md", 3)), "#");
    // the project title in `index.md` is protected on the page itself
    let index = text_of(&std::fs::read(out.join("index.md")).unwrap());
    assert!(index.contains("\n# Release \\#\n"), "{index}");
}
