//! Markdown import (D-XR09R) keeps external links clickable: every CommonMark form of an
//! external link the converter prepares renders as an anchor through the inline subset (md1).
use rdy_engine::markdown_import::{self as mi, MarkdownFile, PreparedContent};
use rdy_render::inline::{self, Ctx};
use rdy_schema::PackSet;

/// The prose of `text` converted as one Markdown file, each block rendered as a body.
fn rendered(text: &str) -> Vec<String> {
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let file = MarkdownFile {
        path: "ext.md".into(),
        bytes: text.as_bytes().to_vec(),
    };
    let p = mi::convert(&pack, &[], &[file]).unwrap();
    assert_eq!(p.unmapped.len(), 0, "no unmapped rows");
    let ctx = Ctx {
        link: &|_| None,
        doc_page: "",
        unavailable: inline::UNRESOLVED,
        local_link: None,
    };
    p.docs[0]
        .blocks
        .iter()
        .filter_map(|b| match &b.content {
            PreparedContent::Prose { body } => {
                Some(inline::body_html(&body.render(&|_| unreachable!()), &ctx))
            }
            _ => None,
        })
        .collect()
}

fn a(href: &str, words: &str) -> String {
    format!("<a href=\"{href}\" rel=\"noopener noreferrer\">{words}</a>")
}

#[test]
fn m4bc2_imported_external_links_render_as_anchors() {
    let cases = [
        // the plain inline link already rendered
        (
            "[site](https://example.com)\n",
            a("https://example.com", "site"),
        ),
        (
            "[site][r]\n\n[r]: https://example.com\n",
            a("https://example.com", "site"),
        ),
        (
            "[site][]\n\n[site]: https://example.com\n",
            a("https://example.com", "site"),
        ),
        (
            "[site]\n\n[site]: https://example.com 'T'\n",
            a("https://example.com", "site"),
        ),
        (
            "[site](https://example.com \"Site title\")\n",
            a("https://example.com", "site"),
        ),
        (
            "<https://example.com>\n",
            a("https://example.com", "https://example.com"),
        ),
        (
            "<https://example.com/a_b*c>\n",
            a("https://example.com/a_b*c", "https://example.com/a_b*c"),
        ),
        ("<a@b.c>\n", a("mailto:a@b.c", "a@b.c")),
        ("<mailto:a@b.c>\n", a("mailto:a@b.c", "mailto:a@b.c")),
        (
            "[angle](<https://example.com/a>)\n",
            a("https://example.com/a", "angle"),
        ),
        (
            "[paren](https://example.com/p_(x))\n",
            a("https://example.com/p_(x)", "paren"),
        ),
        (
            "[*two*\nlines](https://example.com/2)\n",
            a("https://example.com/2", "<em>two</em> lines"),
        ),
        (
            "[](https://example.com/e)\n",
            a("https://example.com/e", "https://example.com/e"),
        ),
        // a `!` right before is kept as text, so the link never reads as an image
        (
            "!<https://example.com>\n",
            format!("!{}", a("https://example.com", "https://example.com")),
        ),
        (
            "\\!<https://example.com>\n",
            format!("!{}", a("https://example.com", "https://example.com")),
        ),
    ];
    for (src, link) in cases {
        assert_eq!(rendered(src), vec![format!("<p>{link}</p>")], "{src:?}");
    }
}
