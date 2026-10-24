//! dgm7: a lone disagreement's quote is cut at a word boundary as a summary is, and an automatic
//! reference label adds no second period (PL 31, PL 25; D-QWSTV, p-2M4KD).
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, RefMode};
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::{land, project, write};
use rdy_qa::site::unescape;
use rdy_render::inline::{Ctx, Form, UNRESOLVED, plain, spans_html};
use rdy_render::{Link, render_site};
use serde_json::json;

/// A canon whose notes doc holds one claim with `body` (`{C}` stands for the id of a claim in a
/// deprecated section), so one lone disagreement quotes that claim.
fn lone(body: &str) -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    let body = body.replace("{C}", &s.c.to_string());
    write(&mut r, "notes", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Reading"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": body}
    ]))
    .expect("the notes doc");
    land(&mut r, "notes").expect("land the reference");
    write(
        &mut r,
        "retire",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"lifecycle": "deprecated"}}]),
    )
    .expect("deprecate the section");
    land(&mut r, "retire").expect("land the deprecation");
    (d, r)
}

/// The disagreements page of the canon of `r`.
fn page(r: &Rdy) -> String {
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    std::fs::read_to_string(out.path().join("disagreements.html")).unwrap()
}

/// The text of the link to the notes page, without its doc title, as a reader reads it.
fn notes_quote(page: &str) -> String {
    let content = page
        .split_once("<h1>Where the docs disagree</h1>")
        .expect("the page")
        .1;
    let re = regex::Regex::new(r#"<a [^>]*href="notes\.html#[^"]*"[^>]*>([^<]*)</a>"#).unwrap();
    let text = re
        .captures(content)
        .unwrap_or_else(|| panic!("a link to the notes page: {content}"))
        .get(1)
        .unwrap()
        .as_str();
    unescape(text)
        .strip_prefix("Notes › ")
        .unwrap_or_else(|| panic!("the doc title leads: {text}"))
        .to_string()
}

/// The quote of a lone disagreement whose claim reads `body` after a reference that reads `lead`.
fn quote_of(lead: &str, body: &str) -> String {
    let reference = ["{{", "{C}", "|", lead, "}}"].concat();
    let (_d, r) = lone(&format!("{reference} {body}"));
    notes_quote(&page(&r))
}

/// `n` scalars after the reference's display text `ref` and a space: `word` seven times in a
/// row is 5 scalars each with its space.
fn words(word: &str, count: usize, last: &str) -> String {
    let mut out = vec![word; count];
    out.push(last);
    out.join(" ")
}

#[test]
fn dgm7_disagreement_quote_cuts_at_a_word_boundary() {
    // "ref" and 14 words of four letters are 73 scalars
    let head = format!("ref {}", words("word", 13, "word"));
    assert_eq!(head.chars().count(), 73);
    let with = |last: &str| format!("{head} {last}");
    // 79 and 80 scalars stay whole, and get no ellipsis
    let t79 = with("fives");
    assert_eq!(t79.chars().count(), 79);
    assert_eq!(quote_of("ref", &t79[4..]), t79);
    let t80 = with("sixsix");
    assert_eq!(t80.chars().count(), 80);
    assert_eq!(quote_of("ref", &t80[4..]), t80);
    // 81 scalars cut at the last word boundary; the cut word does not stay
    let t81 = with("sevense");
    assert_eq!(t81.chars().count(), 81);
    assert_eq!(quote_of("ref", &t81[4..]), format!("{head}…"));

    // a word longer than the limit, with no boundary before it, keeps 80 scalars
    let long = "z".repeat(100);
    assert_eq!(
        quote_of(&long, "after"),
        format!("{}…", "z".repeat(80)),
        "no boundary in the first 80 scalars"
    );

    // the limit counts scalars, not bytes: 5 scalars of 3 bytes each
    let uni = "日本語日本";
    let head = format!("ref {}", words(uni, 11, uni));
    assert_eq!(head.chars().count(), 3 + 12 * 6);
    assert_eq!(quote_of("ref", &head[4..]), head, "75 scalars stay whole");
    let tail = format!("{head} {uni}");
    assert_eq!(tail.chars().count(), 81);
    assert_eq!(
        quote_of("ref", &tail[4..]),
        format!("{head}…"),
        "81 scalars cut at the last boundary"
    );

    // straight and curly quotes stay exactly as written
    let quoted = "\"alpha\" “beta” 'gamma' ‘delta’ it’s fine words follow here and then some more words after";
    assert_eq!(
        quote_of("ref", quoted),
        "ref \"alpha\" “beta” 'gamma' ‘delta’ it’s fine words follow here and then some…"
    );

    // a quote that is short already gets no ellipsis
    assert_eq!(quote_of("ref", "short text"), "ref short text");
}

#[test]
fn dgm7_grouped_disagreement_rows_keep_the_doc_title_alone() {
    // seven copies of one doc: the row is a group, and each link is the doc title alone
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    rdy_engine::testkit::generate_tree(&proj, 7, 1, 3);
    let mut r = Rdy::open(&proj).unwrap();
    land(&mut r, "work").unwrap();
    let p = page(&r);
    let content = p
        .split_once("<h1>Where the docs disagree</h1>")
        .expect("the page")
        .1;
    let re =
        regex::Regex::new(r#"<a [^>]*href="area-\d/doc-0\.html#[^"]*"[^>]*>([^<]*)</a>"#).unwrap();
    let texts: Vec<String> = re
        .captures_iter(content)
        .map(|c| unescape(c.get(1).unwrap().as_str()))
        .collect();
    let want: Vec<String> = (0..7).map(|i| format!("Area {i} › Doc 0")).collect();
    assert_eq!(texts, want);
    assert!(texts.iter().all(|t| !t.contains('…')), "{texts:?}");
}

/// `text` as HTML with every reference leading to a page and reading `label`.
fn html_of(text: &str, label: &str) -> String {
    let link = |_: &BlockId| {
        Some(Link {
            href: Some("x.html".into()),
            label: label.into(),
            ..Default::default()
        })
    };
    let ctx = Ctx {
        link: &link,
        doc_page: "",
        unavailable: UNRESOLVED,
        local_link: None,
    };
    spans_html(text, &ctx)
}

/// `text` as plain words with every reference reading `label`.
fn plain_of(text: &str, label: &str) -> String {
    plain(text, RefMode::Strict, Form::Spans, &|_| label.to_string())
}

const ID: &str = "p-2M4KD";

#[test]
fn dgm7_automatic_reference_period_is_not_doubled() {
    let cases: [(&str, &str, &str, &str); 5] = [
        // label, text after the reference, exact HTML, exact plain
        (
            "Done.",
            ".",
            "See <a href=\"x.html\">Done</a>.",
            "See Done.",
        ),
        (
            "Done.",
            "..",
            "See <a href=\"x.html\">Done</a>..",
            "See Done..",
        ),
        // exactly one final period goes: an ellipsis of three periods keeps two
        (
            "Wait...",
            ".",
            "See <a href=\"x.html\">Wait..</a>.",
            "See Wait...",
        ),
        // the authored period may start more words
        (
            "Done.",
            ". Next.",
            "See <a href=\"x.html\">Done</a>. Next.",
            "See Done. Next.",
        ),
        // a label that ends no sentence of its own
        ("Done", ".", "See <a href=\"x.html\">Done</a>.", "See Done."),
    ];
    for (label, after, html, plain_words) in cases {
        let text = format!("See {{{{{ID}}}}}{after}");
        assert_eq!(
            html_of(&text, label),
            html,
            "html of {text:?} with {label:?}"
        );
        assert_eq!(
            plain_of(&text, label),
            plain_words,
            "plain of {text:?} with {label:?}"
        );
    }

    // a lone disagreement quotes the same words as the page
    let (_d, r) = lone("Readers rely on {{{C}}}.");
    assert_eq!(
        notes_quote(&page(&r)),
        "Readers rely on C stands alone.",
        "one period after the reference"
    );
}

#[test]
fn dgm7_explicit_labels_and_authored_punctuation_are_preserved() {
    let r = format!("{{{{{ID}}}}}");
    let explicit = format!("{{{{{ID}|Shown.}}}}");
    // an explicit display keeps its period before an authored period
    assert_eq!(
        html_of(&format!("See {explicit}."), "Other."),
        "See <a href=\"x.html\">Shown.</a>."
    );
    assert_eq!(
        plain_of(&format!("See {explicit}."), "Other."),
        "See Shown.."
    );
    // a label ending in ?, ! or … is unchanged
    for label in ["Really?", "Wow!", "End…"] {
        assert_eq!(
            html_of(&format!("See {r}."), label),
            format!("See <a href=\"x.html\">{label}</a>.")
        );
        assert_eq!(
            plain_of(&format!("See {r}."), label),
            format!("See {label}.")
        );
    }
    // no whitespace is crossed, and only a period right after counts
    assert_eq!(
        html_of(&format!("See {r} ."), "Done."),
        "See <a href=\"x.html\">Done.</a> ."
    );
    assert_eq!(plain_of(&format!("See {r} ."), "Done."), "See Done. .");
    assert_eq!(
        plain_of(&format!("See {r}, then."), "Done."),
        "See Done., then."
    );
    // a label at the end of the text, and a reference alone, are whole
    assert_eq!(plain_of(&format!("See {r}"), "Done."), "See Done.");
    assert_eq!(plain_of(&r, "Done."), "Done.");
    // authored prose with no reference, and a code span, are unchanged
    assert_eq!(plain_of("A. . B.. C...", "Done."), "A. . B.. C...");
    assert_eq!(html_of("Plain.. text.", "Done."), "Plain.. text.");
    assert_eq!(
        html_of(&format!("A `x.`. {r}."), "Done."),
        "A <code>x.</code>. <a href=\"x.html\">Done</a>."
    );
}
