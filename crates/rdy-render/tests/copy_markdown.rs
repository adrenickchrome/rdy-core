//! Copy's Markdown fragments (D-BRF4P, D-E8P8J): the export's Markdown writer, with link targets
//! handed over as separate slots and no anchor lines or front matter (dgm4).
use rdy_model::Hash;
use rdy_render::print::ir::from_html;
use rdy_render::print::md::{CopyPart, Layout, copy_parts, emit_files};
use rdy_render::print::{Blob, PBlock, PageKind, Print, PrintDoc};
use std::collections::BTreeMap;

/// A print of one doc page holding `blocks`.
fn print_of(page: &str, blocks: Vec<PBlock>, blobs: BTreeMap<Hash, Blob>) -> Print {
    Print {
        title: "Project".into(),
        description: None,
        label: String::new(),
        scope: String::new(),
        tag_redacted: false,
        docs: vec![PrintDoc {
            kind: PageKind::Doc,
            id: "d-1".into(),
            title: "Page".into(),
            page: page.into(),
            depth: page.matches('/').count(),
            blocks,
        }],
        blobs,
        warnings: vec![],
        rdy_dir: std::path::PathBuf::new(),
    }
}

/// The exported Markdown file of the print's one page.
fn export_page(p: &Print) -> String {
    let want = format!(
        "{}.md",
        p.docs[0]
            .page
            .strip_suffix(".html")
            .unwrap_or(&p.docs[0].page)
    );
    let mut found = None;
    emit_files(p, Layout::Plain, false, |path, data| {
        if path == want {
            found = Some(String::from_utf8(data.to_vec()).expect("utf-8"));
        }
        Ok(())
    })
    .expect("export");
    found.expect("the page's file")
}

/// The export's body: what follows the front matter and the `# <title>` line, with each anchor
/// line (and the blank line a non-heading block puts after it) removed.
fn export_body(md: &str) -> String {
    let rest = md.strip_prefix("---\n").expect("front matter");
    let (_, rest) = rest.split_once("\n---\n\n").expect("front matter end");
    let (title, rest) = rest.split_once('\n').expect("title line");
    assert!(title.starts_with("# "), "{title}");
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let mut out = vec![];
    let mut lines = rest.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("<a id=\"") && line.ends_with("\"></a>") {
            if lines.peek() == Some(&"") {
                lines.next();
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// The parts joined as they are, a slot as its raw value.
fn joined(parts: &[CopyPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            CopyPart::Text(t) | CopyPart::Url(t) => t.as_str(),
        })
        .collect()
}

fn text(s: &str) -> CopyPart {
    CopyPart::Text(s.into())
}

fn url(s: &str) -> CopyPart {
    CopyPart::Url(s.into())
}

/// The parts as comparable pairs.
fn pairs(parts: &[CopyPart]) -> Vec<(&'static str, String)> {
    parts
        .iter()
        .map(|p| match p {
            CopyPart::Text(t) => ("text", t.clone()),
            CopyPart::Url(u) => ("url", u.clone()),
        })
        .collect()
}

/// No two text parts are next to each other.
fn assert_coalesced(parts: &[CopyPart]) {
    for w in parts.windows(2) {
        assert!(
            !matches!(w, [CopyPart::Text(_), CopyPart::Text(_)]),
            "adjacent text parts: {parts:?}"
        );
    }
}

#[test]
fn dgm4_fragments_use_the_export_writer() {
    let page = "guide/intro.html";
    let html = "<h2 id=\"the-case-clause\">The <code>case</code> clause</h2>\
        <p id=\"lede\">Some <strong>bold</strong>, <em>italic</em>, <strong><em>both</em></strong> \
        and <code>a `tick` here</code>.<br>Next line</p>\
        <ol start=\"3\"><li>three<ul><li>inner <em>one</em></li><li><p>para in item</p>\
        <p>second para</p></li></ul></li><li>four</li></ol>\
        <div class=\"table-wrap\"><table id=\"the-table\"><thead><tr><th>A | pipe</th><th>B</th>\
        </tr></thead><tbody><tr><td>a<br>b</td><td><code>x|y</code></td></tr><tr><td>c</td></tr>\
        </tbody></table></div>\
        <pre id=\"code-1\"><code class=\"language-rust\">let s = \"```\";\nfn main() {}</code></pre>\
        <aside class=\"callout callout-note\"><span class=\"marker\">Note</span><div><p>Mind \
        <em>it</em>.</p><ul><li>one</li></ul></div></aside>\
        <h3>Last</h3><p>===</p><p>~~~ not a fence</p>";
    let blocks = from_html(html, page);
    let p = print_of(page, blocks.clone(), BTreeMap::new());
    let exported = export_page(&p);
    // the fixture has anchors, so removing them is part of the comparison
    assert!(
        exported.matches("<a id=\"").count() >= 4,
        "the fixture writes anchor lines: {exported}"
    );
    let parts = copy_parts(&blocks, page, &p.blobs).expect("copy");
    assert_coalesced(&parts);
    assert!(
        parts.iter().all(|p| matches!(p, CopyPart::Text(_))),
        "{parts:?}"
    );
    let copied = joined(&parts);
    assert_eq!(copied, export_body(&exported));
    assert!(!copied.contains("<a id="), "{copied}");
    assert!(!copied.contains("---\n"), "{copied}");
    assert!(!copied.contains("rdy_id"), "{copied}");
    // the writer's own forms
    for want in [
        "## The `case` clause\n",
        "Some **bold**, *italic*, ***both*** and ``a `tick` here``.\\\nNext line",
        "3.  three\n\n    -   inner *one*\n    -   para in item\n\n        second para\n4.  four",
        "| A \\| pipe | B |\n| --- | --- |\n| a b | `x\\|y` |\n| c |  |",
        "````rust\nlet s = \"```\";\nfn main() {}\n````",
        "> **Note**\n>\n> Mind *it*.\n>\n> -   one",
        "&#61;==",
        "&#126;~~ not a fence",
    ] {
        assert!(copied.contains(want), "{want:?} in\n{copied}");
    }
    // one blank line between blocks, and one line feed at the end
    assert!(copied.ends_with("fence\n"), "{copied:?}");
    assert!(!copied.ends_with("\n\n"), "{copied:?}");
    assert!(!copied.contains("\n\n\n"), "{copied:?}");
    // an empty range copies nothing
    assert!(copy_parts(&[], page, &p.blobs).expect("copy").is_empty());
}

#[test]
fn dgm4_urls_are_separate_slots() {
    let hash = "667893735fd6ddea275b7aa47f9bf71622da7924b3304e8764ab068f69e6dd7c";
    let h: Hash = hash.parse().expect("hash");
    let mut blobs = BTreeMap::new();
    blobs.insert(
        h,
        Blob {
            media_type: "image/png".into(),
            bytes: vec![1, 2, 3],
            file: format!("assets/{hash}.png"),
        },
    );
    let html = format!(
        "<p>See <a href=\"../spec/rules.html#the-case-clause\">the rules</a>, \
         <a href=\"https://example.invalid/a b?x=(1)&amp;y=2\">out</a> and \
         <a href=\"#local-slug\">here</a>.</p>\
         <figure class=\"figure\"><img src=\"../assets/{hash}.png\" alt=\"A [pic]\">\
         <figcaption>The cap</figcaption></figure>\
         <div class=\"table-wrap\"><table><thead><tr><th>Link</th></tr></thead><tbody><tr>\
         <td><a href=\"https://example.invalid/a|b\">p|q</a></td></tr></tbody></table></div>"
    );
    let page = "guide/intro.html";
    let blocks = from_html(&html, page);
    let parts = copy_parts(&blocks, page, &blobs).expect("copy");
    assert_coalesced(&parts);
    assert_eq!(
        pairs(&parts),
        pairs(&[
            text("See [the rules]("),
            url("../spec/rules.html#the-case-clause"),
            text("), [out]("),
            url("https://example.invalid/a b?x=(1)&y=2"),
            text(") and [here]("),
            url("intro.html#local-slug"),
            text(").\n\n![A \\[pic\\]]("),
            url(&format!("../assets/{hash}.png")),
            text(")\n\nThe cap\n\n| Link |\n| --- |\n| [p\\|q]("),
            // a slot in a table cell writes its `|` so the cell holds it
            url("https://example.invalid/a%7Cb"),
            text(") |\n"),
        ])
    );
    // the export writes the same blocks with .md targets, escaped in place
    let exported = export_page(&print_of(page, blocks.clone(), blobs.clone()));
    assert!(
        exported.contains("[the rules](../spec/rules.md#the-case-clause)"),
        "{exported}"
    );
    assert!(
        exported.contains("[out](https://example.invalid/a%20b?x=%281%29&y=2)"),
        "{exported}"
    );
    assert!(
        exported.contains("[p\\|q](https://example.invalid/a\\|b)"),
        "{exported}"
    );

    // a page under a frozen prefix: every slot is relative to it, so the reader's page URL keeps
    // the prefix, and no slot names a .md file, an id or a root path
    let frozen = "tag/v1/guide/intro.html";
    let blocks = from_html(
        "<p><a href=\"../spec/rules.html#the-case-clause\">rules</a> and \
         <a href=\"../index.html\">home</a></p>",
        frozen,
    );
    let parts = copy_parts(&blocks, frozen, &BTreeMap::new()).expect("copy");
    assert_eq!(
        pairs(&parts),
        pairs(&[
            text("[rules]("),
            url("../spec/rules.html#the-case-clause"),
            text(") and [home]("),
            url("../index.html"),
            text(")\n"),
        ])
    );
    for p in &parts {
        if let CopyPart::Url(u) = p {
            assert!(!u.starts_with('/'), "{u}");
            assert!(!u.contains(".md"), "{u}");
            assert!(!u.contains("tag/v1"), "{u}");
        }
    }
}

#[test]
fn dgm4_source_diagrams_and_removed_notices_copy_as_text() {
    let page = "guide/intro.html";
    let html = "<figure class=\"diagram\" data-lang=\"mermaid\" id=\"flow\">\
        <p class=\"visually-hidden\">Hidden alt</p>\
        <div class=\"diagram-canvas\" aria-hidden=\"true\"><svg viewBox=\"0 0 10 10\">\
        <text>Drawn label</text></svg></div>\
        <div class=\"diagram-tools\"><button type=\"button\">Expand</button></div>\
        <figcaption>The flow</figcaption><details class=\"diagram-source\">\
        <summary>Diagram source</summary><pre class=\"code\"><code>flowchart TD\n  A --&gt; B\
        </code></pre></details></figure>\
        <figure class=\"diagram\" data-lang=\"d2\"><div class=\"diagram-canvas\" \
        aria-hidden=\"true\"><svg><text>x</text></svg></div><details class=\"diagram-source\">\
        <summary>Diagram source</summary><pre class=\"code\"><code>a -&gt; b: `go`</code></pre>\
        </details></figure>\
        <p class=\"redacted\">Content removed.</p>";
    let blocks = from_html(html, page);
    let parts = copy_parts(&blocks, page, &BTreeMap::new()).expect("copy");
    assert_eq!(
        pairs(&parts),
        pairs(&[text(
            "The flow\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\n\
             ```d2\na -> b: `go`\n```\n\nContent removed.\n"
        )])
    );
    let copied = joined(&parts);
    for gone in [
        "svg",
        "Drawn label",
        "Expand",
        "Hidden alt",
        "Diagram source",
    ] {
        assert!(!copied.contains(gone), "{gone} in {copied}");
    }
}

#[test]
fn dgm4_delimiters_entities_and_unicode_survive() {
    let page = "a.html";
    let html = "<p>Use *stars*, _under_, [brackets](x.html), &lt;tag&gt;, &amp;copy; and \
        © \\ back ✓ 日本語 🎉</p>\
        <p># not a heading</p><p>1. not a list</p>\
        <h2>Ends with #</h2>\
        <p>{\"kind\":\"url\",\"value\":\"x\"} and ](../b.html)</p>\
        <p><code>a ``b`` c</code></p>\
        <pre><code>``` inner\n&amp;copy; &lt;b&gt; 日本</code></pre>";
    let blocks = from_html(html, page);
    let p = print_of(page, blocks.clone(), BTreeMap::new());
    let parts = copy_parts(&blocks, page, &p.blobs).expect("copy");
    // text that resembles a slot or a link is literal text
    assert_eq!(parts.len(), 1, "{parts:?}");
    let CopyPart::Text(copied) = &parts[0] else {
        panic!("{parts:?}");
    };
    assert_eq!(copied, &export_body(&export_page(&p)));
    for want in [
        "Use \\*stars\\*, \\_under\\_, \\[brackets\\](x.html), \\<tag\\>, &amp;copy; and © \\\\ back ✓ 日本語 🎉",
        "\\# not a heading",
        "1\\. not a list",
        "## Ends with \\#",
        "{\"kind\":\"url\",\"value\":\"x\"} and \\](../b.html)",
        "```a ``b`` c```",
        "````\n``` inner\n&copy; <b> 日本\n````",
    ] {
        assert!(copied.contains(want), "{want:?} in\n{copied}");
    }
}

#[test]
fn dgm4_payload_is_linear_and_bounded() {
    let page = "guide/intro.html";
    // each block's words are unique, so a second copy of a block would show
    let fixture = |n: usize| -> (Vec<PBlock>, Vec<String>) {
        let words: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "Paragraph number {i:05} says plain words about the design, \
                     and the reader copies them."
                )
            })
            .collect();
        let html: String = words.iter().map(|w| format!("<p>{w}</p>")).collect();
        (from_html(&html, page), words)
    };
    let measure = |n: usize| -> (usize, usize) {
        let (blocks, words) = fixture(n);
        assert_eq!(blocks.len(), n);
        let parts = copy_parts(&blocks, page, &BTreeMap::new()).expect("copy");
        assert_eq!(parts.len(), 1, "plain prose is one text part");
        let payload = serde_json::to_string(&parts).expect("json");
        for w in &words {
            assert_eq!(payload.matches(w.as_str()).count(), 1, "one copy of {w}");
        }
        let text_bytes: usize = words.iter().map(String::len).sum();
        assert!(
            payload.len() <= 2 * text_bytes + 65536,
            "{n} blocks: {} payload bytes for {text_bytes} text bytes",
            payload.len()
        );
        (payload.len(), text_bytes)
    };
    let (small_payload, small_text) = measure(1);
    let (large_payload, large_text) = measure(1000);
    // the payload grows with the text and no faster: the growth between the two fixtures is
    // within twice the growth of their text
    let payload_growth = large_payload - small_payload;
    let text_growth = large_text - small_text;
    assert!(
        payload_growth <= 2 * text_growth,
        "payload grew {payload_growth} bytes for {text_growth} text bytes"
    );
    assert!(payload_growth >= text_growth);
}

fn para(s: &str) -> PBlock {
    PBlock::Para {
        anchor: None,
        runs: if s.is_empty() {
            vec![]
        } else {
            vec![rdy_render::print::Run {
                text: s.into(),
                bold: false,
                italic: false,
                code: false,
                link: None,
            }]
        },
    }
}

/// A block that writes no words adds no blank line to a fragment, at the top level or inside a
/// list item or a callout (fix round 1). The export keeps its bytes.
#[test]
fn dgm4_empty_blocks_add_no_blank_lines() {
    let page = "page.html";
    let copy = |blocks: &[PBlock]| -> String {
        let parts = copy_parts(blocks, page, &BTreeMap::new()).expect("copy");
        assert_coalesced(&parts);
        joined(&parts)
    };
    // a term with an empty name and a list body, as the site page shows it
    let term = from_html(
        "<p>Before</p><dl class=\"term\"><dt></dt><dd><ul><li>example</li></ul></dd></dl>\
         <p>After</p>",
        page,
    );
    assert_eq!(copy(&term), "Before\n\n-   example\n\nAfter\n");
    // between and after nonempty blocks
    assert_eq!(copy(&[para("A"), para(""), para("B")]), "A\n\nB\n");
    assert_eq!(copy(&[para("A"), para("")]), "A\n");
    assert_eq!(copy(&[para(""), para("A")]), "A\n");
    assert_eq!(copy(&[para(""), para(" ")]), "");
    // nested sequences: a list item and a callout
    let list = PBlock::List {
        ordered: false,
        start: 1,
        items: vec![vec![para(""), para("x"), para(""), para("y")]],
    };
    assert_eq!(copy(std::slice::from_ref(&list)), "-   x\n\n    y\n");
    let callout = PBlock::Callout {
        anchor: None,
        label: "Note".into(),
        blocks: vec![para("a"), para(""), para("b"), para("")],
    };
    assert_eq!(
        copy(std::slice::from_ref(&callout)),
        "> **Note**\n>\n> a\n>\n> b\n"
    );
    // the export writes these blocks as it did before
    let p = print_of(
        page,
        vec![para("A"), para(""), list, callout, para("")],
        BTreeMap::new(),
    );
    assert_eq!(
        export_body(&export_page(&p)),
        "A\n\n\n\n-   \n\n    x\n\n\n\n    y\n\n> **Note**\n>\n> a\n>\n>\n>\n> b\n>\n>\n\n\n"
    );
}
