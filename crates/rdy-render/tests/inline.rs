//! The inline subset (md1, dtext1): its exact HTML and plain text.
use rdy_model::{BlockId, RefMode};
use rdy_render::Link;
use rdy_render::inline::{self, Ctx, Form};

fn link(id: &BlockId) -> Option<Link> {
    (id.to_string() == "t-AAAAA").then(|| Link {
        href: Some("#guard".into()),
        label: "Guard".into(),
        title: "Spec › Terms".into(),
        ..Default::default()
    })
}
fn spans(src: &str) -> String {
    inline::spans_html(
        src,
        &Ctx {
            link: &link,
            doc_page: "",
            unavailable: inline::UNRESOLVED,
            local_link: None,
        },
    )
}
fn body(src: &str) -> String {
    inline::body_html(
        src,
        &Ctx {
            link: &link,
            doc_page: "",
            unavailable: inline::UNRESOLVED,
            local_link: None,
        },
    )
}
const A: &str = "<a href=\"#guard\" title=\"Spec › Terms\">Guard</a>";
const UNRES: &str =
    "<span class=\"ref unresolved\" title=\"This reference could not be resolved\">";

#[test]
fn md1_spans_render_bold_italic_code_and_links() {
    for (src, want) in [
        (
            "**bold** and *it* and `code`",
            "<strong>bold</strong> and <em>it</em> and <code>code</code>",
        ),
        (
            "**bold *and it* end**",
            "<strong>bold <em>and it</em> end</strong>",
        ),
        (
            "[docs](https://example.invalid/a?b=1&c=2)",
            "<a href=\"https://example.invalid/a?b=1&amp;c=2\" rel=\"noopener noreferrer\">docs</a>",
        ),
        (
            "[Docs](HTTPS://example.invalid/(x))",
            "<a href=\"HTTPS://example.invalid/(x)\" rel=\"noopener noreferrer\">Docs</a>",
        ),
        (
            "[mail](mailto:team)",
            "<a href=\"mailto:team\" rel=\"noopener noreferrer\">mail</a>",
        ),
        (
            "[**bold** link](#x)",
            "<a href=\"#x\"><strong>bold</strong> link</a>",
        ),
        ("``a ` b``", "<code>a ` b</code>"),
        ("line one\nline two", "line one<br>line two"),
    ] {
        assert_eq!(spans(src), want, "{src}");
    }
}

#[test]
fn md1_text_that_is_not_markup_stays_as_written() {
    for (src, want) in [
        ("2 * 3 * 4", "2 * 3 * 4"),
        ("snake_case_name and a_b_", "snake_case_name and a_b_"),
        ("**unclosed", "**unclosed"),
        ("it's ` alone", "it&#39;s ` alone"),
        ("![alt](x.png)", "![alt](x.png)"),
        ("***three***", "***three***"),
        ("\\*not\\* it \\\\", "*not* it \\"),
        ("C:\\Users\\x", "C:\\Users\\x"),
        (
            "<b>hi</b> & \"q\"",
            "&lt;b&gt;hi&lt;/b&gt; &amp; &quot;q&quot;",
        ),
        ("[x](a b)", "[x](a b)"),
        ("[](x)", "[](x)"),
        ("- not a list in a field", "- not a list in a field"),
        ("", ""),
    ] {
        assert_eq!(spans(src), want, "{src}");
    }
}

/// FW-F1a (founder ruling): emphasis uses `*` and `**` only; underscores are always literal.
#[test]
fn md1_underscores_are_always_literal_and_code_spans_keep_them() {
    assert_eq!(spans("__init__ runs"), "__init__ runs");
    assert_eq!(spans("`__init__` runs"), "<code>__init__</code> runs");
    assert_eq!(spans("x.__match_args__"), "x.__match_args__");
    assert_eq!(spans("self__x__"), "self__x__");
    assert_eq!(spans("__bold__ and _it_"), "__bold__ and _it_");
    assert_eq!(
        body("Set `__match_args__` or __match_args__ on the class."),
        "<p>Set <code>__match_args__</code> or __match_args__ on the class.</p>",
        "the golden fixture's __match_args__ renders verbatim, with no <strong>"
    );
}

#[test]
fn md1_unsafe_link_targets_never_become_hrefs() {
    for src in [
        "[x](javascript:alert(1))",
        "[x](JaVaScRiPt:alert(1))",
        "[x](data:text/html,hi)",
        "[x](vbscript:msgbox)",
        "[x](//evil.invalid/a)",
        "[x](file:///etc/passwd)",
        "[x](java\u{1}script:alert(1))",
        "[x](ftp://example.invalid)",
    ] {
        assert_eq!(spans(src), "x", "{src:?}");
    }
    // an entity that would decode to a colon is escaped, so it stays a harmless relative path
    assert_eq!(
        spans("[x](javascript&#58;alert(1))"),
        "<a href=\"javascript&amp;#58;alert(1)\">x</a>"
    );
}

#[test]
fn md1_relative_links_are_rebased_to_the_page_that_shows_the_text() {
    let at = |doc_page: &str, src: &str| {
        inline::spans_html(
            src,
            &Ctx {
                link: &link,
                doc_page,
                unavailable: inline::UNRESOLVED,
                local_link: None,
            },
        )
    };
    assert_eq!(
        at("", "[t](../tutorial/adventure.html)"),
        "<a href=\"../tutorial/adventure.html\">t</a>"
    );
    assert_eq!(
        at(
            "specification/patterns.html",
            "[t](../tutorial/adventure.html)"
        ),
        "<a href=\"tutorial/adventure.html\">t</a>"
    );
    assert_eq!(
        at("specification/patterns.html", "[g](#guards)"),
        "<a href=\"specification/patterns.html#guards\">g</a>"
    );
    assert_eq!(
        at("../specification/patterns.html", "[i](img/a.png)"),
        "<a href=\"../specification/img/a.png\">i</a>"
    );
    assert_eq!(
        at("specification/patterns.html", "[r](/root.html)"),
        "<a href=\"/root.html\">r</a>"
    );
}

#[test]
fn dtext1_references_render_inside_markup_and_display_text_is_the_link_text() {
    assert_eq!(
        spans("**see {{t-AAAAA}}**"),
        format!("<strong>see {A}</strong>")
    );
    assert_eq!(
        spans("{{t-AAAAA|the guard rule}}"),
        "<a href=\"#guard\" title=\"Spec › Terms\">the guard rule</a>"
    );
    assert_eq!(
        spans("{{p-ZZZZZ|old notes}}"),
        format!("{UNRES}old notes</span>")
    );
    assert_eq!(
        spans("{{p-ZZZZZ}}"),
        format!("{UNRES}unresolved reference</span>")
    );
    assert_eq!(spans("`{{t-AAAAA}}`"), format!("<code>{A}</code>"));
    assert_eq!(spans("`\\{{t-AAAAA}}`"), "<code>{{t-AAAAA}}</code>");
    assert_eq!(
        spans("[see {{t-AAAAA}}](#x)"),
        "<a href=\"#x\">see Guard</a>"
    );
    assert_eq!(
        spans("{{t-AAAAA|**not bold**}}"),
        "<a href=\"#guard\" title=\"Spec › Terms\">**not bold**</a>"
    );
    assert_eq!(spans("[x]({{t-AAAAA}})"), format!("[x]({A})"));
}

/// FW-F2a (founder ruling): a reference the author typed as a key shows the target's current
/// key; once the target has no key it shows the label, like a reference typed as an id.
#[test]
fn key1_a_key_typed_reference_shows_the_targets_current_key() {
    let keyed = |id: &BlockId| {
        link(id).map(|l| Link {
            key: Some("FW-S1".into()),
            ..l
        })
    };
    let s = |src: &str, l: &dyn Fn(&BlockId) -> Option<Link>| {
        inline::spans_html(
            src,
            &Ctx {
                link: l,
                doc_page: "",
                unavailable: inline::UNRESOLVED,
                local_link: None,
            },
        )
    };
    let with_key = "<a href=\"#guard\" title=\"Spec › Terms\">FW-S1</a>";
    assert_eq!(s("{{t-AAAAA|@key}}", &keyed), with_key);
    assert_eq!(s("{{t-AAAAA}}", &keyed), A, "typed as an id: the label");
    assert_eq!(
        s("{{t-AAAAA|@key}}", &link),
        A,
        "the key is gone: the label"
    );
    assert_eq!(
        s("{{t-AAAAA|\\@key}}", &keyed),
        "<a href=\"#guard\" title=\"Spec › Terms\">@key</a>"
    );
    assert_eq!(
        s("[see {{t-AAAAA|@key}}](#x)", &keyed),
        "<a href=\"#x\">see FW-S1</a>"
    );
    assert_eq!(
        s("{{p-ZZZZZ|@key}}", &keyed),
        format!("{UNRES}unresolved reference</span>")
    );
    let p = |src: &str| {
        inline::plain_refs(src, RefMode::Strict, Form::Spans, &|span| match (
            span.key_typed,
            span.token.as_str(),
        ) {
            (true, "t-AAAAA") => "FW-S1".into(),
            (_, "t-AAAAA") => "Guard".into(),
            _ => String::new(),
        })
    };
    assert_eq!(
        p("{{t-AAAAA|@key}} and {{t-AAAAA}} and {{t-AAAAA|text}}"),
        "FW-S1 and Guard and text"
    );
}

#[test]
fn md1_bodies_hold_paragraphs_and_nested_lists() {
    for (src, want) in [
        (
            "Intro line\n- one\n- two\n  - two a\n  - two b\n3. three\n\nAfter.",
            "<p>Intro line</p><ul><li>one</li><li>two<ul><li>two a</li><li>two b</li></ul></li></ul><ol start=\"3\"><li>three</li></ol><p>After.</p>",
        ),
        ("1. a\n2. b", "<ol><li>a</li><li>b</li></ol>"),
        (
            "- item\n  continues here\n- next",
            "<ul><li>item<br>continues here</li><li>next</li></ul>",
        ),
        ("- a\n\n- b", "<ul><li>a</li><li>b</li></ul>"),
        (
            "- a\n\nNot indented.",
            "<ul><li>a</li></ul><p>Not indented.</p>",
        ),
        (
            "-not a list\n2021. That year",
            "<p>-not a list<br>2021. That year</p>",
        ),
        (
            "* star item\n\t- tab nested",
            "<ul><li>star item<ul><li>tab nested</li></ul></li></ul>",
        ),
        (
            "line one\nline two\n\npara two",
            "<p>line one<br>line two</p><p>para two</p>",
        ),
        (
            "- **bold** item with {{t-AAAAA}}",
            "<ul><li><strong>bold</strong> item with <a href=\"#guard\" title=\"Spec › Terms\">Guard</a></li></ul>",
        ),
        ("   \n\n", ""),
    ] {
        assert_eq!(body(src), want, "{src:?}");
    }
    assert_eq!(
        inline::body_inline_html(
            "One *para*.",
            &Ctx {
                link: &link,
                doc_page: "",
                unavailable: inline::UNRESOLVED,
                local_link: None,
            }
        ),
        "One <em>para</em>."
    );
    assert_eq!(
        inline::body_inline_html(
            "A\n\nB",
            &Ctx {
                link: &link,
                doc_page: "",
                unavailable: inline::UNRESOLVED,
                local_link: None,
            }
        ),
        "<p>A</p><p>B</p>"
    );
}

#[test]
fn md1_list_nesting_stops_at_six_levels() {
    let src: String = (0..8)
        .map(|i| format!("{}- level {}\n", "  ".repeat(i), i + 1))
        .collect();
    let h = body(&src);
    assert_eq!(h.matches("<ul>").count(), 6, "{h}");
    assert!(
        h.contains("<li>level 6</li><li>level 7</li><li>level 8</li>"),
        "{h}"
    );
}

#[test]
fn md1_labels_take_code_spans_only() {
    let l = |s: &str| {
        inline::label_html(
            s,
            &Ctx {
                link: &link,
                doc_page: "",
                unavailable: inline::UNRESOLVED,
                local_link: None,
            },
        )
    };
    assert_eq!(
        l("The `match` statement"),
        "The <code>match</code> statement"
    );
    assert_eq!(
        l("__match_args__ and **x** and [a](b)"),
        "__match_args__ and **x** and [a](b)"
    );
    assert_eq!(l("See {{t-AAAAA}}"), format!("See {A}"));
}

#[test]
fn md1_plain_text_drops_markup_and_keeps_the_words() {
    let p = |s: &str, f: Form| {
        inline::plain(s, RefMode::Strict, f, &|tok: &str| {
            if tok == "t-AAAAA" {
                "Guard".into()
            } else {
                String::new()
            }
        })
    };
    assert_eq!(
        p(
            "**bold** and [docs](https://example.invalid) and `c` and {{t-AAAAA}} and {{t-AAAAA|the rule}}",
            Form::Spans
        ),
        "bold and docs and c and Guard and the rule"
    );
    assert_eq!(
        p("Intro\n- one\n  - one a\n- two\n\nEnd", Form::Body),
        "Intro\n\none\none a\ntwo\n\nEnd"
    );
    assert_eq!(p("`__x__` **y**", Form::Label), "__x__ **y**");
    assert_eq!(
        p("\\{{literal}} \\* {{p-ZZZZZ|old}}", Form::Spans),
        "{{literal}} * old"
    );
}

/// Hostile input: deep emphasis, many brackets and backticks render in bounded depth and time.
#[test]
fn md1_deep_or_unbalanced_markup_renders_without_overflowing() {
    let deep: String = (0..4_000)
        .map(|i| if i % 2 == 0 { "*a " } else { "**a " })
        .collect::<String>()
        + &(0..4_000)
            .rev()
            .map(|i| if i % 2 == 0 { "a* " } else { "a** " })
            .collect::<String>();
    let h = spans(&deep);
    assert_eq!(h.matches("<em>").count(), h.matches("</em>").count());
    assert!(
        h.matches("<em>").count() + h.matches("<strong>").count() <= 16,
        "nesting is capped"
    );
    let brackets = "[".repeat(1_000) + &"`".repeat(3) + &"x`".repeat(1_000) + &"*".repeat(1_000);
    let h = body(&brackets);
    assert!(h.starts_with("<p>[[[") && h.ends_with("</p>"));
}

#[test]
fn msw12_a_backtick_in_a_link_target_does_not_hide_a_later_link() {
    assert_eq!(
        spans("[a](x`y) [b](z) `"),
        "<a href=\"x`y\">a</a> <a href=\"z\">b</a> `"
    );
}

#[test]
fn msw12_a_backtick_in_a_nested_link_target_does_not_hide_a_later_link() {
    assert_eq!(
        spans("[x [a](b`c) y [d](e) `"),
        "[x <a href=\"b`c\">a</a> y <a href=\"e\">d</a> `"
    );
    assert_eq!(
        spans("[a [b](c`d) [e](f) `"),
        "[a <a href=\"c`d\">b</a> <a href=\"f\">e</a> `"
    );
    assert_eq!(
        spans("[[a](b`c)] [d](e) `"),
        "[<a href=\"b`c\">a</a>] <a href=\"e\">d</a> `"
    );
    // accepted divergence from 4c9ec60 (ledger "Group 4 rulings"): the outer link wins
    assert_eq!(spans("[x [a](b]c) y](z)"), "<a href=\"z\">x [a](b]c) y</a>");
}

#[test]
fn msw12_hostile_bodies_render_in_linear_time() {
    let open = "[".repeat(2_000_000);
    assert_eq!(spans(&open), open);
    let nested = "[x [a](b".repeat(250_000);
    assert!(spans(&nested).starts_with("[x [a](b"));
    let gap = format!("- a{}- b", "\n".repeat(2_000_000));
    assert_eq!(body(&gap), "<ul><li>a</li><li>b</li></ul>");
}
