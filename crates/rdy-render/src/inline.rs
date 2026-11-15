//! The inline subset of Markdown (md1) as the site writes it: HTML for every running text on the
//! site, and the display text of references (dtext1).
//!
//! The token model, the parser and the plain reader text live in
//! [`rdy_engine::reader::inline`]; this module writes the same nodes as HTML. Anything that is not
//! markup is text, escaped.
pub use crate::html::UNRESOLVED;
use crate::html::{Link, escape};
pub use rdy_engine::reader::inline::{
    CODE_CLOSE, CODE_ESC, CODE_OPEN, Form, MAX_LOCAL_URL, code_escape, is_one_paragraph, plain,
    plain_refs, plain_refs_coded, plain_refs_every, source_plain, unescape_braces,
};
use rdy_engine::reader::inline::{
    Node, href, next_char, one_period, parse_label_of, parse_spans_of, prune,
};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{BlockId, RefMode, RefSpan};

/// How a text's references and relative links are read on the page being written.
pub struct Ctx<'a> {
    /// Where a reference leads from the page being written.
    pub link: &'a dyn Fn(&BlockId) -> Option<Link>,
    /// The href of the page of the doc that holds the text, from the page being written, with no
    /// anchor; `""` when that is the page being written.
    pub doc_page: &'a str,
    /// What a reference that leads nowhere reads as when the author gave no display text: the
    /// source's [`UNRESOLVED`], or a named profile's [`UNAVAILABLE`].
    pub unavailable: &'a str,
    /// A named profile's filter of authored local links (D-0VCQ5): it takes a local href that
    /// passed the safety checks, as the page being written sees it, and gives the canonical href
    /// of a selected page and an anchor emitted on it, or none. None for a source output, whose
    /// local links keep their existing rules. With it, a reference that leads nowhere is plain
    /// words with no marker, hover or link.
    pub local_link: Option<LocalLink<'a>>,
}

/// A named profile's filter of authored local links ([`Ctx::local_link`]).
pub type LocalLink<'a> = &'a dyn Fn(&str) -> Option<String>;

/// What a reference to a block outside a named profile's selection reads as when the author gave
/// no display text (D-0VCQ5): never the target's label, key or id.
pub use rdy_engine::reader::text::UNAVAILABLE;

/// A body as HTML: paragraphs and lists. A blank body is the empty string.
pub fn body_html(text: &str, ctx: &Ctx) -> String {
    let mut out = String::new();
    for b in parse_body(text) {
        match b {
            Block::Para(p) => {
                out.push_str("<p>");
                out.push_str(&spans_html(&p, ctx));
                out.push_str("</p>");
            }
            Block::List(l) => list_html(&l, ctx, &mut out),
        }
    }
    out
}

/// As [`body_html`], but a body that is exactly one paragraph comes back without `<p>`.
pub fn body_inline_html(text: &str, ctx: &Ctx) -> String {
    match parse_body(text).as_slice() {
        [Block::Para(p)] => spans_html(p, ctx),
        _ => body_html(text, ctx),
    }
}

/// A one-line field as HTML: spans only. A newline is `<br>`, a leading `- ` is text.
pub fn spans_html(text: &str, ctx: &Ctx) -> String {
    let (nodes, refs) = parse_spans_of(text, RefMode::Strict);
    let mut out = String::new();
    render_html(&nodes, &refs, ctx, false, None, &mut out);
    out
}

/// A label as HTML: references and code spans only.
pub fn label_html(text: &str, ctx: &Ctx) -> String {
    let (nodes, refs) = parse_label_of(text, RefMode::Strict);
    let mut out = String::new();
    render_html(&nodes, &refs, ctx, false, None, &mut out);
    out
}

thread_local! {
    /// Whether the HTML being written is the print model's source ([`with_image_markup`]): a
    /// remote image is then an `img` element the print reader reads, never written to a page.
    static IMAGE_MARKUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f`, with every remote image written as `<img class="remote-image" src alt>` in place of its
/// alt text: the HTML the print model and Copy read (D-3DAX4 rule 3), so the Markdown outputs
/// write the image from the selected blocks alone. A page never holds this HTML: the site shows
/// the alt text and loads nothing.
pub(crate) fn with_image_markup<T>(f: impl FnOnce() -> T) -> T {
    let before = IMAGE_MARKUP.with(|m| m.replace(true));
    let out = f();
    IMAGE_MARKUP.with(|m| m.set(before));
    out
}

thread_local! {
    /// Whether the print model's source marks each reference with the id it names
    /// ([`with_ref_markup`]): the single Markdown file reads them to link a reference within its
    /// doc and to warn of any other (D-ESHXM rule 8, D-6PR3Y rule 6).
    static REF_MARKUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f`, with each reference in the print model's source ([`with_image_markup`]) marked with the
/// token it names (`data-rdy-ref`). A page never holds this markup.
pub(crate) fn with_ref_markup<T>(f: impl FnOnce() -> T) -> T {
    let before = REF_MARKUP.with(|m| m.replace(true));
    let out = f();
    REF_MARKUP.with(|m| m.set(before));
    out
}

/// Whether references are being marked ([`with_ref_markup`]): every block's print source is then
/// rendered, not only one that may hold a remote image.
pub(crate) fn ref_markup() -> bool {
    REF_MARKUP.with(std::cell::Cell::get)
}

/// The attribute that marks a reference in the print model's source, or nothing.
fn ref_mark(span: &RefSpan) -> String {
    if IMAGE_MARKUP.with(std::cell::Cell::get) && ref_markup() {
        format!(" data-rdy-ref=\"{}\"", escape(&span.token))
    } else {
        String::new()
    }
}

/// Whether `text` may hold a remote image: it holds `![`, the opener every one starts with. A
/// cheap test, before the print model's second rendering of a block.
pub(crate) fn may_hold_image(text: &str) -> bool {
    text.contains("![")
}

/// [`href`] of an authored link target, then, for a named profile, its filter: a target longer
/// than [`MAX_LOCAL_URL`], local or external, leads nowhere; a local target leads only to a
/// selected page and an anchor emitted on it, by its canonical href. An external target within
/// the bound keeps its href.
fn local_href(target: &str, ctx: &Ctx) -> Option<(String, bool)> {
    let (h, away) = href(target, ctx.doc_page)?;
    match ctx.local_link {
        Some(_) if target.len() > MAX_LOCAL_URL => None,
        Some(_) if away => Some((h, away)),
        Some(local) => local(&h).map(|h| (h, false)),
        None => Some((h, away)),
    }
}

// ---- output -------------------------------------------------------------------------------

/// What a reference reads as in words: the notice when its target is removed (whatever display
/// text the author gave, D-3M81W), its display text, the target's key when the author typed a
/// key and the target still has one, else the target's label (`unavailable`, the unresolved
/// marker's words, when it leads nowhere, unless the author gave display text).
fn ref_words(span: &RefSpan, link: Option<&Link>, unavailable: &str) -> String {
    if link.is_some_and(|l| l.removed) {
        return CONTENT_REMOVED.to_string();
    }
    if let Some(d) = &span.display {
        return d.clone();
    }
    match link {
        // a decision outside a named profile's selection: no words of its own (D-7M0YF)
        Some(l) if l.dropped => String::new(),
        Some(l) => match (&l.key, span.key_typed) {
            (Some(k), true) if !k.is_empty() => k.clone(),
            _ => l.label.clone(),
        },
        None => unavailable.to_string(),
    }
}

fn target_of(span: &RefSpan, ctx: &Ctx) -> Option<Link> {
    BlockId::parse(&span.token)
        .ok()
        .and_then(|id| (ctx.link)(&id))
}

fn text_html(s: &str) -> String {
    escape(s).replace('\n', "<br>")
}

fn render_html(
    nodes: &[Node],
    refs: &[RefSpan],
    ctx: &Ctx,
    in_link: bool,
    after: Option<char>,
    out: &mut String,
) {
    let nodes = &prune(nodes, refs, &|span| crate::html::drops(span, ctx.link));
    for (i, n) in nodes.iter().enumerate() {
        let next = next_char(nodes, i, after);
        match n {
            Node::Text(t) => out.push_str(&text_html(t)),
            Node::Ref(k) => {
                let span = &refs[*k];
                let link = target_of(span, ctx);
                let words = ref_words(span, link.as_ref(), ctx.unavailable);
                let words = text_html(&one_period(span, words, next));
                // the print model's source marks the reference (never a page)
                let mark = ref_mark(span);
                let plain = |out: &mut String| {
                    if mark.is_empty() || words.is_empty() {
                        out.push_str(&words)
                    } else {
                        out.push_str(&format!("<span{mark}>{words}</span>"))
                    }
                };
                match link {
                    _ if in_link => out.push_str(&words),
                    // a named profile: a reference outside the selection is its words alone
                    None if ctx.local_link.is_some() => plain(out),
                    // the target resolves but has no place on the site: its words, unlinked
                    Some(Link { href: None, .. }) => plain(out),
                    Some(Link {
                        href: Some(h),
                        title,
                        ..
                    }) if title.is_empty() => {
                        out.push_str(&format!("<a href=\"{}\"{mark}>{words}</a>", escape(&h)))
                    }
                    Some(Link {
                        href: Some(h),
                        title,
                        ..
                    }) => out.push_str(&format!(
                        "<a href=\"{}\" title=\"{}\"{mark}>{words}</a>",
                        escape(&h),
                        escape(&title)
                    )),
                    None => out.push_str(&format!(
                        "<span class=\"ref unresolved\" title=\"This reference could not be resolved\"{mark}>{words}</span>"
                    )),
                }
            }
            Node::Code(c) => {
                out.push_str("<code>");
                render_html(c, refs, ctx, in_link, None, out);
                out.push_str("</code>");
            }
            Node::Strong(c) => {
                out.push_str("<strong>");
                render_html(c, refs, ctx, in_link, next, out);
                out.push_str("</strong>");
            }
            Node::Em(c) => {
                out.push_str("<em>");
                render_html(c, refs, ctx, in_link, next, out);
                out.push_str("</em>");
            }
            // the site and every print show the alt text and load nothing (D-3DAX4 rule 2); the
            // print model's source writes the image for the Markdown outputs
            Node::Image { alt, url } => {
                if IMAGE_MARKUP.with(std::cell::Cell::get) {
                    out.push_str(&format!(
                        "<img class=\"remote-image\" src=\"{}\" alt=\"{}\">",
                        escape(url),
                        escape(alt)
                    ));
                } else {
                    out.push_str(&text_html(alt));
                }
            }
            Node::Link { target, children } => match local_href(target, ctx) {
                Some((h, away)) => {
                    out.push_str(&format!(
                        "<a href=\"{}\"{}>",
                        escape(&h),
                        if away {
                            " rel=\"noopener noreferrer\""
                        } else {
                            ""
                        }
                    ));
                    render_html(children, refs, ctx, true, next, out);
                    out.push_str("</a>");
                }
                None => render_html(children, refs, ctx, true, next, out),
            },
        }
    }
}

// ---- bodies -------------------------------------------------------------------------------

use rdy_model::list_parts::{Block, List, Part, parse_body};

/// Fenced code in a list item as a `code` block shows its body (D-7M4TR): its language's label,
/// the code-copy control, and the code highlighted; no reference or markup is read in it.
fn item_code_html(lang: Option<&str>, text: &str) -> String {
    let lang = lang.unwrap_or("").trim();
    let human = match crate::highlight::human_name(lang) {
        _ if lang.is_empty() => "Code".to_string(),
        n if n.is_empty() => "Code".to_string(),
        n => n,
    };
    let class = if lang.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", escape(lang))
    };
    format!(
        "<div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">{}</span><button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button></div><pre class=\"code\"><code{class}>{}</code></pre></div>",
        escape(&human),
        crate::highlight::html(lang, text)
    )
}

fn list_html(l: &List, ctx: &Ctx, out: &mut String) {
    let tag = if l.ordered { "ol" } else { "ul" };
    if l.ordered && l.start != 1 {
        out.push_str(&format!("<ol start=\"{}\">", l.start));
    } else {
        out.push_str(&format!("<{tag}>"));
    }
    for it in &l.items {
        out.push_str("<li>");
        for (i, part) in it.parts.iter().enumerate() {
            match part {
                // the marker line's paragraph is the item's text, as it always was; a later
                // paragraph is a paragraph of its own
                Part::Para(t) if i == 0 => out.push_str(&spans_html(t, ctx)),
                Part::Para(t) => {
                    out.push_str("<p>");
                    out.push_str(&spans_html(t, ctx));
                    out.push_str("</p>");
                }
                Part::Code { lang, text } => out.push_str(&item_code_html(lang.as_deref(), text)),
                Part::List(sub) => list_html(sub, ctx, out),
            }
        }
        out.push_str("</li>");
    }
    out.push_str(&format!("</{tag}>"));
}
