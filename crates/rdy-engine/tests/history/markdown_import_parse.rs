//! The Markdown converter (D-XR09R, D-19BZD, D-H5MKZ): bounded path-and-byte input to prepared
//! folders, docs and blocks, with the `unmapped` report. Pure: no filesystem, store or transport.
use rdy_engine::markdown_import::{
    self as mi, MarkdownFile, Prepared, PreparedContent, PreparedDoc, Target, Text, UnmappedKind,
};
use rdy_model::{RefMode, ref_spans};
use rdy_schema::{Pack, PackSet};

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn md(path: &str, text: &str) -> MarkdownFile {
    MarkdownFile {
        path: path.to_string(),
        bytes: text.as_bytes().to_vec(),
    }
}

fn convert(files: &[MarkdownFile]) -> Prepared {
    mi::convert(&pack(), &[], files).unwrap()
}

/// A text with each ref written as `{{<doc path>}}` or `{{<doc path>@<block index>}}`.
fn show(p: &Prepared, t: &Text) -> String {
    t.render(&|tg: &Target| match tg.block {
        None => p.docs[tg.doc].path.clone(),
        Some(b) => format!("{}@{b}", p.docs[tg.doc].path),
    })
}

fn doc<'a>(p: &'a Prepared, path: &str) -> &'a PreparedDoc {
    p.docs
        .iter()
        .find(|d| d.path == path)
        .unwrap_or_else(|| panic!("no doc {path}"))
}

/// One line per block: index, type, parent (`-` for the doc) and content.
fn outline(p: &Prepared, path: &str) -> Vec<String> {
    let d = doc(p, path);
    d.blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let parent = b.parent.map_or("-".to_string(), |x| x.to_string());
            let content = match &b.content {
                PreparedContent::Section { title } => show(p, title),
                PreparedContent::Prose { body } => show(p, body),
                PreparedContent::Code { lang, body } => {
                    format!("[{}] {body}", lang.as_deref().unwrap_or(""))
                }
                PreparedContent::Table { columns, rows } => {
                    let cells = |r: &[Text]| r.iter().map(|c| show(p, c)).collect::<Vec<_>>();
                    let mut parts = vec![cells(columns).join(" | ")];
                    parts.extend(rows.iter().map(|r| cells(r).join(" | ")));
                    parts.join(" // ")
                }
            };
            format!("{i} {} <{parent}> {content}", b.content.block_type())
        })
        .collect()
}

fn rows(p: &Prepared) -> Vec<(String, usize, &'static str)> {
    p.unmapped
        .iter()
        .map(|u| (u.path.clone(), u.line, u.kind.as_str()))
        .collect()
}

fn row(path: &str, line: usize, kind: &'static str) -> (String, usize, &'static str) {
    (path.to_string(), line, kind)
}

fn limit_message(bound: &str, limit: usize) -> String {
    format!("Markdown import exceeds {bound}: {limit}")
}

fn assert_limit(r: Result<Prepared, rdy_engine::RdyError>, bound: &str, limit: usize) {
    let e = r.expect_err("a bound refuses");
    assert_eq!(e.code, "E_LIMIT", "{e:?}");
    assert_eq!(e.message, limit_message(bound, limit));
    assert_eq!(e.fix, "split the source into smaller import directories");
}

#[test]
fn m4bc2_structure_code_lists_and_tables_map_without_inference() {
    let setup = "Intro before any heading.\n\
        \n\
        # Setup Guide\n\
        \n\
        Para one with *emphasis* and `code`.\n\
        \n\
        ### Skipped level\n\
        \n\
        - item one\n  - nested item\n    1. deep\n- item two\n\
        \n\
        ## Second\n\
        \n\
        ```rust\nfn main() {}\n```\n\
        \n    indented code\n\
        \n\
        | Name | Value |\n|------|-------|\n| `a`  | **b** |\n| c    |\n\
        \n\
        > quoted\n> still quoted\n\
        \n\
        Decision: we accepted X. Author: Alice\n";
    let files = [
        md("guide/setup.md", setup),
        md("crlf.md", "# Café ☕\r\n\r\nLine one\r\nline two\r\n"),
        md("guide/deep/x.md", "just text\n"),
        md("guide/Über Uns.md", "über\n"),
        md("guide/notes.md", "n1\n"),
        md("guide/notes.markdown", "n2\n"),
        md("cr.md", "# Old Mac\rline one\rline two\r"),
    ];
    let p = mi::convert(&pack(), &["empty".to_string()], &files).unwrap();
    assert_eq!(
        outline(&p, "guide/setup.md"),
        vec![
            "0 prose <-> Intro before any heading.",
            "1 section <-> Setup Guide",
            "2 prose <1> Para one with *emphasis* and `code`.",
            // a skipped level needs no synthetic section
            "3 section <1> Skipped level",
            "4 prose <3> - item one\n  - nested item\n    1. deep\n- item two",
            "5 section <1> Second",
            "6 code <5> [rust] fn main() {}",
            "7 code <5> [] indented code",
            "8 table <5> Name | Value // `a` | **b** // c | ",
            "9 prose <5> > quoted\n> still quoted",
            // no decision, status or author is inferred from the words
            "10 prose <5> Decision: we accepted X. Author: Alice",
        ]
    );
    let setup_doc = doc(&p, "guide/setup.md");
    assert_eq!(show(&p, &setup_doc.title), "Setup Guide");
    assert_eq!(setup_doc.name, "setup");
    assert_eq!(setup_doc.doc_type, "overview");
    // CR LF reads as LF; Unicode stays as written
    assert_eq!(
        outline(&p, "crlf.md"),
        vec!["0 section <-> Café ☕", "1 prose <0> Line one\nline two"]
    );
    assert_eq!(show(&p, &doc(&p, "crlf.md").title), "Café ☕");
    // a lone CR is a line break too
    assert_eq!(
        outline(&p, "cr.md"),
        vec!["0 section <-> Old Mac", "1 prose <0> line one\nline two"]
    );
    // the title falls back to the file name
    assert_eq!(show(&p, &doc(&p, "guide/deep/x.md").title), "x");
    let uber = doc(&p, "guide/Über Uns.md");
    assert_eq!(show(&p, &uber.title), "Über Uns");
    assert_eq!(uber.name, "ber-uns");
    // names are unique under one parent, ignoring case, in path order
    assert_eq!(doc(&p, "guide/notes.markdown").name, "notes");
    assert_eq!(doc(&p, "guide/notes.md").name, "notes-2");
    // folders become folders, the empty one too
    let folders: Vec<(&str, Option<usize>, &str)> = p
        .folders
        .iter()
        .map(|f| (f.path.as_str(), f.parent, f.name.as_str()))
        .collect();
    assert_eq!(
        folders,
        vec![
            ("empty", None, "empty"),
            ("guide", None, "guide"),
            ("guide/deep", Some(1), "deep")
        ]
    );
    assert_eq!(setup_doc.parent, Some(1));
    assert_eq!(doc(&p, "crlf.md").parent, None);
    assert_eq!(doc(&p, "guide/deep/x.md").parent, Some(2));
    // only mapped types, and nothing to report
    for d in &p.docs {
        for b in &d.blocks {
            assert!(
                ["section", "prose", "code", "table"].contains(&b.content.block_type()),
                "{}",
                b.content.block_type()
            );
        }
    }
    assert!(p.unmapped.is_empty(), "{:?}", p.unmapped);
    // 3 folders, 7 docs, and their blocks
    let content: usize = p.docs.iter().map(|d| d.blocks.len()).sum();
    assert_eq!(p.block_count(), 3 + 7 + content);
    // bytes that are not UTF-8 refuse as the export import's unreadable file does
    let e = mi::convert(
        &pack(),
        &[],
        &[MarkdownFile {
            path: "bad.md".into(),
            bytes: vec![b'#', b' ', 0xff, 0xfe],
        }],
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT");
    assert_eq!(
        e.message,
        "bad.md:1:1: cannot read the file: stream did not contain valid UTF-8"
    );
    assert_eq!(e.fix, "check the file");
}

#[test]
fn m4bc2_front_matter_problems_stay_as_text_with_a_row() {
    let files = [
        md(
            "good.md",
            "---\ntitle: Real Title\ndoc_type: page\n---\n# Heading\n\nText.\n",
        ),
        md("close.md", "---\ntitel: Oops\n---\n# From Heading\n"),
        md("dup.md", "---\ntitle: A\ntitle: B\n---\nBody only.\n"),
        md("badyaml.md", "---\ntitle: [unclosed\n---\n## Sub\n"),
        md("badvalue.md", "---\ntitle: 42\ndoc_type: page\n---\ntext\n"),
        md("seq.md", "---\n- a\n---\n"),
        md("crlf.md", "---\r\ntitle: Win\r\n---\r\nBody\r\n"),
        md("emptyfm.md", "---\n---\nBody\n"),
        md("open.md", "---\ntitle: x\n"),
        md("brace.md", "---\ntitle: Use {{x}}\n---\n"),
    ];
    let p = convert(&files);
    let good = doc(&p, "good.md");
    assert_eq!(show(&p, &good.title), "Real Title");
    assert_eq!(good.doc_type, "page");
    assert_eq!(
        outline(&p, "good.md"),
        vec!["0 section <-> Heading", "1 prose <0> Text."]
    );
    // a close key is no key: the front matter stays as text, the title comes from the heading
    let close = doc(&p, "close.md");
    assert_eq!(show(&p, &close.title), "From Heading");
    assert_eq!(close.doc_type, "overview");
    assert_eq!(
        outline(&p, "close.md"),
        vec![
            "0 code <-> [yaml] ---\ntitel: Oops\n---",
            "1 section <-> From Heading"
        ]
    );
    // no heading: the title is the file name
    assert_eq!(show(&p, &doc(&p, "dup.md").title), "dup");
    assert_eq!(
        outline(&p, "dup.md"),
        vec![
            "0 code <-> [yaml] ---\ntitle: A\ntitle: B\n---",
            "1 prose <-> Body only."
        ]
    );
    assert_eq!(show(&p, &doc(&p, "badyaml.md").title), "Sub");
    // a bad value voids the whole front matter, its valid doc_type too
    let badvalue = doc(&p, "badvalue.md");
    assert_eq!(show(&p, &badvalue.title), "badvalue");
    assert_eq!(badvalue.doc_type, "overview");
    assert_eq!(
        outline(&p, "seq.md"),
        vec!["0 code <-> [yaml] ---\n- a\n---"]
    );
    assert_eq!(show(&p, &doc(&p, "crlf.md").title), "Win");
    assert_eq!(outline(&p, "crlf.md"), vec!["0 prose <-> Body"]);
    assert_eq!(show(&p, &doc(&p, "emptyfm.md").title), "emptyfm");
    assert_eq!(outline(&p, "emptyfm.md"), vec!["0 prose <-> Body"]);
    // no closing line: no front matter, only Markdown
    assert_eq!(show(&p, &doc(&p, "open.md").title), "open");
    assert_eq!(
        outline(&p, "open.md"),
        vec!["0 prose <-> ---", "1 prose <-> title: x"]
    );
    // a title is text: its braces bind to nothing
    assert_eq!(show(&p, &doc(&p, "brace.md").title), "Use \\{{x}}");
    assert_eq!(
        rows(&p),
        vec![
            row("badvalue.md", 2, "front_matter"),
            row("badyaml.md", 3, "front_matter"),
            row("close.md", 2, "front_matter"),
            row("dup.md", 2, "front_matter"),
            row("seq.md", 1, "front_matter"),
        ]
    );
    assert!(
        p.unmapped
            .iter()
            .all(|u| u.kind == UnmappedKind::FrontMatter)
    );
}

/// External links in every CommonMark form rdy's renderer can show as a link.
const EXTERNAL_LINKS: &str = "# Ext\n\
    \n\
    [site][r] and [again][] and [r] and [top][]\n\
    \n\
    [titled](https://example.com/t \"Site title\") and <https://auto.example/a_b*c>\n\
    \n\
    <a@b.c> and [angle](<https://example.com/a>) and [paren](https://example.com/p_(x))\n\
    \n\
    [*two*\n\
    lines](https://example.com/2) [](https://example.com/empty) [ftp](ftp://example.com/f)\n\
    \n\
    [r]: https://example.com\n\
    [again]: https://example.com/again 'T'\n\
    [top]: #ext\n";

const EXTERNAL_LINKS_OUTLINE: [&str; 5] = [
    "0 section <-> Ext",
    // a definition every use of which became a link is dropped; a collapsed reference
    // (`[top][]`) is replaced with its `[]`
    "1 prose <0> [site](https://example.com) and [again](https://example.com/again) and [r](https://example.com) and {{ext.md@0|top}}",
    // a title is dropped (rdy links have none); an autolink's text is literal
    "2 prose <0> [titled](https://example.com/t) and [https://auto.example/a_b\\*c](https://auto.example/a_b*c)",
    // an email autolink is a mailto link; parentheses in a target are escaped
    "3 prose <0> [a@b.c](mailto:a@b.c) and [angle](https://example.com/a) and [paren](https://example.com/p_\\(x\\))",
    // link text on one line; an empty text shows the URL; a scheme rdy never links keeps its
    // source text
    "4 prose <0> [*two* lines](https://example.com/2) [https://example.com/empty](https://example.com/empty) [ftp](ftp://example.com/f)",
];

#[test]
fn m4bc2_links_resolve_across_files_and_duplicate_headings() {
    let index = "# Home\n\
        \n\
        [setup](guide/setup.md)\n\
        \n\
        [install](guide/setup.md#install) and [again](guide/setup.md#install-1)\n\
        \n\
        [café](guide/setup.md#caf%C3%A9) and [notes](guide/my%20notes.md)\n\
        \n\
        [here](#local-heading) and [top](#home) and [self](index.md)\n\
        \n\
        [ext](https://example.com/x.md) <https://auto.example> [mail](mailto:a@b.c) [proto](//cdn.example/x.md)\n\
        \n\
        [missing](nope.md) [excluded](drafts/hidden.md)\n\
        \n\
        [escape](../outside.md) [pdf](spec.pdf)\n\
        \n\
        [badfrag](guide/setup.md#nope) [enc](guide%2Fsetup.md)\n\
        \n\
        [drive](C:/x.md) [nul](a%00.md) [dir](guide/) [query](guide/setup.md?x=1)\n\
        \n\
        [root dir](/a/b/) [root index](/c/d) [root frag](/a/b.md#heading)\n\
        \n\
        [root missing](/missing/) [root two](/x/y) [root](/)\n\
        \n\
        ## Local Heading\n\
        \n\
        [ref style][r] and [@at](guide/setup.md)\n\
        \n\
        [r]: guide/setup.md#install\n\
        [unused]: https://example.com\n";
    let setup = "# Setup\n\
        \n\
        ## Install\n\
        \n\
        ## Install\n\
        \n\
        ## Café\n\
        \n\
        [back](../index.md) [up](../../index.md)\n";
    let files = [
        md("index.md", index),
        md("guide/setup.md", setup),
        md("guide/my notes.md", "Notes\n"),
        md("a/b.md", "# Heading\n"),
        md("c/d/index.md", "D\n"),
        md("x/y.md", "Y\n"),
        md("x/y/index.md", "YI\n"),
        md("ext.md", EXTERNAL_LINKS),
    ];
    let p = convert(&files);
    assert_eq!(
        outline(&p, "index.md"),
        vec![
            "0 section <-> Home",
            "1 prose <0> {{guide/setup.md|setup}}",
            // duplicate headings get numeric suffixes in their doc
            "2 prose <0> {{guide/setup.md@1|install}} and {{guide/setup.md@2|again}}",
            // a percent-encoded fragment and path decode once
            "3 prose <0> {{guide/setup.md@3|café}} and {{guide/my notes.md|notes}}",
            "4 prose <0> {{index.md@12|here}} and {{index.md@0|top}} and {{index.md|self}}",
            // external links stay links, in the inline form rdy reads; an autolink shows its
            // URL; a protocol-relative target, which rdy never links, keeps its source text
            "5 prose <0> [ext](https://example.com/x.md) [https://auto.example](https://auto.example) [mail](mailto:a@b.c) [proto](//cdn.example/x.md)",
            // missing, excluded, escaping, non-Markdown, unknown fragment, encoded separator,
            // drive, NUL, directory and query targets stay as their source text
            "6 prose <0> [missing](nope.md) [excluded](drafts/hidden.md)",
            "7 prose <0> [escape](../outside.md) [pdf](spec.pdf)",
            "8 prose <0> [badfrag](guide/setup.md#nope) [enc](guide%2Fsetup.md)",
            "9 prose <0> [drive](C:/x.md) [nul](a%00.md) [dir](guide/) [query](guide/setup.md?x=1)",
            // root links resolve from the source root (D-H5MKZ)
            "10 prose <0> {{a/b.md|root dir}} {{c/d/index.md|root index}} {{a/b.md@0|root frag}}",
            // no match, or two matches (x/y.md and x/y/index.md), stay as text; `/` is index.md
            "11 prose <0> [root missing](/missing/) [root two](/x/y) {{index.md|root}}",
            "12 section <0> Local Heading",
            // a reference link resolves through its definition; a leading @ is escaped
            "13 prose <12> {{guide/setup.md@1|ref style}} and {{guide/setup.md|\\@at}}",
            // a definition every use of which became a ref is dropped; an unused one stays
            "14 prose <12> [unused]: https://example.com",
        ]
    );
    assert_eq!(
        outline(&p, "guide/setup.md"),
        vec![
            "0 section <-> Setup",
            "1 section <0> Install",
            "2 section <0> Install",
            "3 section <0> Café",
            // `..` resolves inside the root only
            "4 prose <3> {{index.md|back}} [up](../../index.md)",
        ]
    );
    // every external link rdy can show becomes the inline form its renderer reads: reference
    // links, titled links, autolinks and email autolinks too (rdy-render's
    // m4bc2_imported_external_links_render_as_anchors renders these blocks)
    assert_eq!(outline(&p, "ext.md"), EXTERNAL_LINKS_OUTLINE);
    assert_eq!(
        rows(&p),
        vec![
            row("guide/setup.md", 9, "link"),
            row("index.md", 13, "link"),
            row("index.md", 15, "link"),
            row("index.md", 17, "link"),
            row("index.md", 19, "link"),
            row("index.md", 23, "link"),
        ]
    );
}

#[test]
fn m4bc2_unsupported_spans_and_literal_refs_keep_their_text() {
    let u = "# Title {{x}}\n\
        \n\
        Text with <span>html</span> inline.\n\
        \n\
        ![diagram](img/d.png)\n\
        \n\
        <div>\nblock html\n</div>\n\
        \n\
        ~~struck~~ text\n\
        \n\
        - [x] done\n\
        \n\
        Footnote ref[^n].\n\
        \n\
        [^n]: The note.\n\
        \n\
        > [!WARNING]\n> careful\n\
        \n\
        Literal {{D-12345}} and {{alias:D-12345}} and {{KEY|words}} and \\{{kept}} and `{{code}}`.\n\
        \n\
        | a | b |\n|---|---|\n| {{c}} | 2 | extra |\n\
        \n\
        ```\n{{raw}}\n```\n";
    let p = convert(&[md("u.md", u)]);
    assert_eq!(
        outline(&p, "u.md"),
        vec![
            "0 section <-> Title \\{{x}}",
            "1 prose <0> Text with <span>html</span> inline.",
            "2 prose <0> ![diagram](img/d.png)",
            "3 prose <0> <div>\nblock html\n</div>",
            "4 prose <0> ~~struck~~ text",
            "5 prose <0> - [x] done",
            "6 prose <0> Footnote ref[^n].",
            "7 prose <0> [^n]: The note.",
            "8 prose <0> > [!WARNING]\n> careful",
            // source braces never bind: not an id, a cited `alias:id` nor a key
            "9 prose <0> Literal \\{{D-12345}} and \\{{alias:D-12345}} and \\{{KEY|words}} and \\{{kept}} and `\\{{code}}`.",
            // a row with more cells than its header would lose one: the table stays as text
            "10 prose <0> | a | b |\n|---|---|\n| \\{{c}} | 2 | extra |",
            // code reads no references, so it keeps its braces as they are
            "11 code <0> [] {{raw}}",
        ]
    );
    assert_eq!(
        rows(&p),
        vec![
            row("u.md", 3, "html"),
            row("u.md", 5, "image"),
            row("u.md", 7, "html"),
            row("u.md", 11, "extension"),
            row("u.md", 13, "extension"),
            row("u.md", 15, "extension"),
            row("u.md", 17, "extension"),
            row("u.md", 19, "extension"),
            row("u.md", 26, "extension"),
        ]
    );
    // no prose body or title holds a reference rdy would read
    for b in &doc(&p, "u.md").blocks {
        let text = match &b.content {
            PreparedContent::Section { title } => show(&p, title),
            PreparedContent::Prose { body } => show(&p, body),
            _ => continue,
        };
        assert!(ref_spans(&text, RefMode::Strict).is_empty(), "{text}");
    }
    assert_eq!(UnmappedKind::NonMarkdownFile.as_str(), "non_markdown_file");
    assert_eq!(
        serde_json::to_value(&p.unmapped[0]).unwrap(),
        serde_json::json!({"path": "u.md", "line": 3, "kind": "html"})
    );
}

#[test]
fn m4bc2_parser_bounds_refuse_before_unbounded_growth() {
    let one = |path: &str, text: String| mi::convert(&pack(), &[], &[md(path, &text)]);
    // parser nesting: n block quotes hold one paragraph at depth n + 1
    assert!(one("q.md", format!("{} x\n", ">".repeat(127))).is_ok());
    assert_limit(
        one("q.md", format!("{} x\n", ">".repeat(128))),
        "Parser nesting",
        128,
    );
    // hostile depth refuses without exhausting the stack
    assert_limit(
        one("q.md", format!("{} x\n", ">".repeat(100_000))),
        "Parser nesting",
        128,
    );
    let lists: String = (0..500)
        .map(|i| format!("{}- x\n", "  ".repeat(i)))
        .collect();
    assert_limit(one("l.md", lists), "Parser nesting", 128);
    assert_limit(
        one("m.md", format!("{}x\n", "- ".repeat(50_000))),
        "Parser nesting",
        128,
    );
    let stars = "*".repeat(20_000);
    assert_limit(
        one("e.md", format!("{stars}x{stars}\n")),
        "Parser nesting",
        128,
    );
    let brackets = format!("{}x{}\n", "[".repeat(50_000), "](a.md)".repeat(50_000));
    assert!(one("b.md", brackets).is_ok());
    // generated blocks: the doc and its paragraphs
    assert!(one("p.md", "a\n\n".repeat(99_999)).is_ok());
    assert_limit(
        one("p.md", "a\n\n".repeat(100_000)),
        "Generated blocks",
        100_000,
    );
    // headings are counted before any block is built
    assert_limit(
        one("h.md", "# h\n\n".repeat(100_000)),
        "Generated blocks",
        100_000,
    );
    // unmapped rows
    let p = one("i.md", "![](x.png)\n\n".repeat(10_000)).unwrap();
    assert_eq!(p.unmapped.len(), 10_000);
    assert_limit(
        one("i.md", "![](x.png)\n\n".repeat(10_001)),
        "Unmapped rows",
        10_000,
    );
    // front matter per file: the bytes between the two fence lines
    let fm = |n: usize| format!("---\ntitle: {}\n---\n", "x".repeat(n - 8));
    let p = one("f.md", fm(65_536)).unwrap();
    assert_eq!(show(&p, &p.docs[0].title).len(), 65_528);
    assert_limit(one("f.md", fm(65_537)), "Front matter per file", 65_536);
    // relative path bytes
    let long = format!("{}.md", "a".repeat(4_093));
    assert!(one(&long, "x\n".into()).is_ok());
    let longer = format!("{}.md", "a".repeat(4_094));
    assert_limit(one(&longer, "x\n".into()), "Relative path bytes", 4_096);
    // folder depth, from a file's path or a directory's
    let deep = |n: usize| format!("{}x.md", "d/".repeat(n));
    assert!(one(&deep(32), "x\n".into()).is_ok());
    assert_limit(one(&deep(33), "x\n".into()), "Folder depth", 32);
    let dir = vec!["d/".repeat(33).trim_end_matches('/').to_string()];
    assert_limit(mi::convert(&pack(), &dir, &[]), "Folder depth", 32);
    // a path that is not a plain relative path refuses as an unsafe source path
    for bad in [
        "/abs.md",
        "a/../b.md",
        "a//b.md",
        "./a.md",
        "a\\b.md",
        "C:/a.md",
        "",
    ] {
        let e = one(bad, "x\n".into()).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad}");
        assert_eq!(
            e.message,
            format!("Markdown source path is not a contained regular file or directory: {bad}")
        );
        assert_eq!(e.fix, "copy regular files into a private source directory");
    }
}

#[test]
fn m4bc2_unknown_doc_type_refuses_before_any_append() {
    let pack = pack();
    let known = pack
        .doc_types
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    for text in [
        "---\ntitle: A\ndoc_type: decsion\n---\nBody\n",
        // other front-matter problems do not hide an unknown type
        "---\ntags: x\ndoc_type: decsion\n---\nBody\n",
    ] {
        let files = [md("notes/a.md", text), md("notes/b.md", "fine\n")];
        let e = mi::convert(&pack, &[], &files).unwrap_err();
        assert_eq!(e.code, "E_DOC_TYPE");
        assert_eq!(e.message, "notes/a.md:3:1: unknown doc_type \"decsion\"");
        assert_eq!(e.fix, format!("use one of: {known}"));
    }
    // a known type is taken as written
    let p = mi::convert(&pack, &[], &[md("a.md", "---\ndoc_type: flows\n---\n")]).unwrap();
    assert_eq!(p.docs[0].doc_type, "flows");
}
