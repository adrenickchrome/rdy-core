//! The designed rendering of every content type, and the references inside text.
use crate::{highlight, inline};
use rdy_model::{Block, BlockId, FieldValue, Hash, RefMode, RefSpan, entity, ref_spans};
use rdy_schema::Pack;
use std::fmt::Write;

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Where a reference to a block leads, as one page sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// Relative to the page that holds the link (`#anchor` on the same page).
    pub href: String,
    /// The target's one-line label.
    pub label: String,
    /// The hover text (`Doc › Section`, `Doc`, `Glossary`, `Open questions`); empty for none.
    pub title: String,
    /// The target's key, which a reference the author typed as a key shows (FW-F2a). None for a
    /// doc, a folder or a section, whose title shows instead (FW-F2b; see `site::typed_key`).
    pub key: Option<String>,
}

/// What an unresolved reference reads as. In strict mode the author wrote only an id, which a
/// reader may not see, so the marker's own words stand in for it.
pub(crate) const UNRESOLVED: &str = "unresolved reference";

/// A label as HTML ([`inline::label_html`] on the page being written): escaped, its code spans
/// as `<code>` and its references as links. A newline is a `<br>`.
pub fn text_html(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    inline::label_html(text, &inline::Ctx { link, doc_page: "" })
}

/// A label as plain words ([`inline::Form::Label`]), escaped, each reference read as its display
/// text or its target's label (see [`diagram_label`]): for an attribute, a caption or a place a
/// link may not go.
pub fn words_html(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    escape(&inline::plain_refs(
        text,
        RefMode::Strict,
        inline::Form::Label,
        &|span| diagram_label(span, link),
    ))
}

/// `<a>` for a link, with its hover title when it has one.
fn anchor(l: &Link) -> String {
    if l.title.is_empty() {
        format!("<a href=\"{}\">{}</a>", escape(&l.href), escape(&l.label))
    } else {
        format!(
            "<a href=\"{}\" title=\"{}\">{}</a>",
            escape(&l.href),
            escape(&l.title),
            escape(&l.label)
        )
    }
}

/// mer1: escapes a label for Mermaid. Characters Mermaid reads as syntax become `#<code>;` entity
/// codes, which Mermaid decodes before parsing and shows as the characters; line breaks become
/// spaces. `&` is left to the HTML escape.
fn mermaid_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' | '#' | ';' | '<' | '>' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '`' => {
                out.push_str(&format!("#{};", c as u32))
            }
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// The words a reference reads as where no link may go: its display text, the target's key for
/// a key-typed reference whose target still has one, the target's label, else the unresolved
/// marker's words; never the id.
fn diagram_label(span: &RefSpan, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    if let Some(d) = &span.display {
        return d.clone();
    }
    match BlockId::parse(&span.token).ok().and_then(|id| link(&id)) {
        Some(Link { key: Some(k), .. }) if span.key_typed && !k.is_empty() => k,
        Some(l) => l.label,
        None => UNRESOLVED.to_string(),
    }
}

/// Escaped diagram source with each id-shaped `{{id}}` replaced by its label (see
/// [`diagram_label`]), escaped for Mermaid and then for HTML, so the diagram renderer sees plain
/// text. Other `{{…}}` (e.g. Mermaid hexagons) is kept as written.
pub fn diagram_source(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(text, RefMode::IdShaped, &|span| {
        escape(&mermaid_label(&diagram_label(span, link)))
    })
}

/// The diagram's source as a reader reads it under "Diagram source": escaped, its references as
/// labels (see [`diagram_label`]), so no id is shown.
fn readable_source(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(text, RefMode::IdShaped, &|span| {
        escape(&diagram_label(span, link))
    })
}

/// Escapes diagram source between references (turning the rdy escape `\{{` into `{{`) and
/// renders each reference with `r`.
fn with_refs(text: &str, mode: RefMode, r: &dyn Fn(&RefSpan) -> String) -> String {
    let plain = |s: &str| escape(&inline::unescape_braces(s));
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&plain(&text[last..span.range.start]));
        out.push_str(&r(&span));
        last = span.range.end;
    }
    out.push_str(&plain(&text[last..]));
    out
}

/// A file's media type as a reader names it: `PDF`, `Text`, `Image`, else the subtype upper-cased.
fn human_media(media: &str) -> String {
    let (kind, sub) = media.split_once('/').unwrap_or((media, ""));
    let sub = sub.split(';').next().unwrap_or("").trim();
    match (kind.trim(), sub) {
        (_, "pdf") => "PDF".into(),
        ("text", _) => "Text".into(),
        ("image", _) => "Image".into(),
        (_, "") => "File".into(),
        (_, s) => s.to_uppercase(),
    }
}

/// A size as a reader reads it: `512 bytes`, `12 KB`, `3.4 MB`.
fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    match n {
        1 => "1 byte".into(),
        n if n < KB => format!("{n} bytes"),
        n if n < KB * KB => format!("{} KB", n.div_ceil(KB)),
        n => format!("{:.1} MB", n as f64 / (KB * KB) as f64),
    }
}

/// A figure: its image or a download link, or a sentence when the store lacks the blob.
fn figure_html(b: &Block, ctx: &BlockCtx) -> String {
    let h = b.field_str("blob").unwrap_or("");
    let alt_text = b.field_str("alt").unwrap_or("");
    // the alt attribute, the caption and the download link's text are plain words
    let alt = words_html(alt_text, ctx.link);
    let caption = alt.clone();
    let media = b.field_str("media_type").unwrap_or("");
    let image = media.starts_with("image/");
    match h.parse::<Hash>() {
        Ok(hash) if (ctx.present)(&hash) => {
            let src = escape(&(ctx.asset_href)(&hash, media));
            if image {
                format!(
                    "<figure class=\"figure\"><img src=\"{src}\" alt=\"{alt}\" loading=\"lazy\"><figcaption>{caption}</figcaption></figure>"
                )
            } else {
                let mut meta = human_media(media);
                if let Some(n) = (ctx.blob_len)(&hash) {
                    let _ = write!(meta, ", {}", human_size(n));
                }
                format!(
                    "<p class=\"download\"><a href=\"{src}\" download>{alt}</a> <span class=\"meta\">({})</span></p>",
                    escape(&meta)
                )
            }
        }
        _ => format!(
            "<p class=\"figure-missing\">{caption} ({} not available)</p>",
            if image { "image" } else { "file" }
        ),
    }
}

/// A table cell or column head: as [`cell`], with each string in it read as spans (a map's keys
/// as written).
fn cell_html(v: &FieldValue, ictx: &inline::Ctx) -> String {
    match v {
        FieldValue::Str(s) => inline::spans_html(s, ictx),
        FieldValue::List(items) => items
            .iter()
            .map(|i| cell_html(i, ictx))
            .collect::<Vec<_>>()
            .join(", "),
        FieldValue::Map(m) => m
            .iter()
            .map(|(k, v)| format!("{}: {}", escape(k), cell_html(v, ictx)))
            .collect::<Vec<_>>()
            .join("; "),
        other => cell(other),
    }
}

/// A table cell's value as text: a list's items joined by `, `, a map's `k: v` pairs joined by
/// `; ` (both recursively), a number as written, `Null` as nothing. Escaped.
pub fn cell(v: &FieldValue) -> String {
    escape(&cell_text(v))
}

fn cell_text(v: &FieldValue) -> String {
    match v {
        FieldValue::Null => String::new(),
        FieldValue::Bool(b) => b.to_string(),
        FieldValue::Int(i) => i.to_string(),
        FieldValue::Str(s) => s.clone(),
        FieldValue::List(items) => items.iter().map(cell_text).collect::<Vec<_>>().join(", "),
        FieldValue::Map(m) => m
            .iter()
            .map(|(k, v)| format!("{k}: {}", cell_text(v)))
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// Whether a cell holds a number (right-aligned in its column). A word such as `inf` or `NaN`
/// is not one, though it parses as a float.
fn is_number(v: &FieldValue) -> bool {
    match v {
        FieldValue::Int(_) => true,
        FieldValue::Str(s) => {
            let s = s.trim();
            s.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
                && s.parse::<f64>().is_ok_and(f64::is_finite)
        }
        _ => false,
    }
}

/// Everything [`block_html`] reads besides the block: where it sits, how to link and where an
/// asset's file lives. Built by `page.rs`, one per block.
pub struct BlockCtx<'a> {
    /// The block's depth (the doc's children are at 1).
    pub depth: usize,
    pub anchor: &'a str,
    pub link: &'a dyn Fn(&BlockId) -> Option<Link>,
    /// The href of the block's doc's page from the page being written, with no anchor; `""` on
    /// the doc's own page. Relative links in the block's text are read from there.
    pub doc_page: &'a str,
    pub pack: &'a Pack,
    /// Whether the store holds the blob.
    pub present: &'a dyn Fn(&Hash) -> bool,
    /// The page-relative href of the asset file for a blob and its media type.
    pub asset_href: &'a dyn Fn(&Hash, &str) -> String,
    pub blob_len: &'a dyn Fn(&Hash) -> Option<u64>,
    /// The 1-based position of a table among its doc's tables.
    pub table_number: Option<usize>,
    /// The blocks that `describes`-link to this one.
    pub described_by: &'a [Link],
}

/// A type's name as a reader reads it: the type id with `_` as a space and its first letter
/// upper-cased (`open_question` is `Open question`).
fn display_name(block_type: &str) -> String {
    let words = block_type.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The `Also: …` line of a block that has aliases, else nothing.
fn aliases_html(b: &Block) -> String {
    let aliases = entity::aliases(b);
    if aliases.is_empty() {
        return String::new();
    }
    let joined = aliases
        .iter()
        .map(|a| escape(a))
        .collect::<Vec<_>>()
        .join(", ");
    format!("<p class=\"entity-aliases\">Also: {joined}</p>")
}

/// The marker (eyebrow) of a block, or nothing for a presentational type.
fn marker(ctx: &BlockCtx, block_type: &str, words: &str) -> String {
    if ctx.pack.is_presentational(block_type) {
        String::new()
    } else {
        format!("<span class=\"marker\">{}</span>", escape(words))
    }
}

/// The fields that hold a block's primary text, in the order the engine reads them.
const PRIMARY_FIELDS: [&str; 4] = ["statement", "prompt", "name", "title"];

/// Inner HTML for one block. A block the site leaves out (a rationale, a decision) is empty.
pub fn block_html(b: &Block, ctx: &BlockCtx) -> String {
    let (depth, link) = (ctx.depth, ctx.link);
    let ictx = inline::Ctx {
        link,
        doc_page: ctx.doc_page,
    };
    let t = b.block_type.as_str();
    // a title or a name is a label: its code spans and references, which link like the body's
    let f = |name: &str| inline::label_html(b.field_str(name).unwrap_or(""), &ictx);
    let body = || inline::body_html(&b.body, &ictx);
    // a section's heading is one level below its parent section's; a definition or a flow heads
    // a part of the section it sits in, at the level of that section's subsections
    let section_level = (depth + 1).min(6);
    let item_level = depth.clamp(2, 6);
    match t {
        "section" => format!(
            "<h{section_level} class=\"section-title\">{}</h{section_level}>{}",
            f("title"),
            body()
        ),
        "claim" | "constraint" | "requirement" => {
            let mut eyebrow = display_name(t);
            if t == "requirement"
                && let Some(p) = b.field_str("priority").filter(|p| !p.is_empty())
            {
                let _ = write!(eyebrow, " · {p}");
            }
            format!(
                "<div class=\"normative\">{}{}</div>",
                marker(ctx, t, &eyebrow),
                body()
            )
        }
        "example" | "note" => {
            // a note that is `planned` carries the quiet "Planned" marker, not its type's eyebrow
            let planned =
                t == "note" && matches!(b.fields.get("planned"), Some(FieldValue::Bool(true)));
            let eyebrow = if planned {
                "Planned".into()
            } else {
                display_name(t)
            };
            format!(
                "<aside class=\"callout callout-{t}\">{}<div>{}</div></aside>",
                marker(ctx, t, &eyebrow),
                body()
            )
        }
        // supporting text: off the site (FW-D10)
        "rationale" => String::new(),
        "term" => format!(
            "<dl class=\"term\"><dt id=\"{}\">{}</dt><dd>{}{}</dd></dl>",
            escape(ctx.anchor),
            f("name"),
            inline::body_inline_html(&b.body, &ictx),
            aliases_html(b)
        ),
        "component" | "actor" | "interface" | "value" | "entity" => {
            let described = if ctx.described_by.is_empty() {
                String::new()
            } else {
                format!(
                    "<p class=\"described-by\">Described by: {}</p>",
                    ctx.described_by
                        .iter()
                        .map(anchor)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            // an entity's kind is its eyebrow, when it has one
            let eyebrow = match b.field_str("kind").map(str::trim).filter(|k| !k.is_empty()) {
                Some(k) if t == entity::ENTITY => display_name(k),
                _ => display_name(t),
            };
            let value = match entity::value_text(b) {
                Some(v) => format!("<p class=\"entity-value\">{}</p>", escape(&v)),
                None => String::new(),
            };
            format!(
                "<div class=\"entity entity-{t}\">{}<h{item_level} class=\"entity-name\">{}</h{item_level}>{value}{}{}{described}</div>",
                marker(ctx, t, &eyebrow),
                f("name"),
                aliases_html(b),
                body()
            )
        }
        "flow" => {
            let steps: String = match b.fields.get("steps") {
                Some(FieldValue::List(s)) => s
                    .iter()
                    .map(|s| match s {
                        FieldValue::Str(t) => format!("<li>{}</li>", inline::spans_html(t, &ictx)),
                        o => format!("<li>{}</li>", cell_html(o, &ictx)),
                    })
                    .collect(),
                _ => String::new(),
            };
            format!(
                "<div class=\"flow\"><h{item_level} class=\"flow-name\">{}</h{item_level}><ol class=\"steps\">{steps}</ol>{}</div>",
                f("name"),
                body()
            )
        }
        "table" => table_html(b, ctx, &ictx),
        "diagram" => {
            let lang = b.field_str("lang").unwrap_or("");
            // the alt text is plain words: the hidden text and the caption
            let alt = b
                .field_str("alt")
                .filter(|a| !a.trim().is_empty())
                .map(|a| words_html(a, link));
            // only Mermaid has a renderer on the site; any other language shows its source
            let canvas = if lang == "mermaid" {
                format!(
                    "<div class=\"diagram-canvas\" aria-hidden=\"true\"><pre class=\"mermaid\">{}</pre></div>",
                    diagram_source(&b.body, link)
                )
            } else {
                String::new()
            };
            // with no alt text the source below is the text alternative, and there is no caption
            let (hidden, caption) = match &alt {
                Some(a) => (a.as_str(), format!("<figcaption>{a}</figcaption>")),
                None => ("Diagram (source below)", String::new()),
            };
            format!(
                "<figure class=\"diagram\" data-lang=\"{}\"><p class=\"visually-hidden\">{hidden}</p>{canvas}{caption}<details class=\"diagram-source\"><summary>Diagram source</summary><pre class=\"code\"><code>{}</code></pre></details></figure>",
                escape(lang),
                readable_source(&b.body, link)
            )
        }
        "figure" => figure_html(b, ctx),
        "code" => {
            // with no language: the label `Code`, plain text, and no class on `<code>`
            let lang = b.field_str("lang").unwrap_or("").trim();
            let human = match highlight::human_name(lang) {
                n if n.is_empty() || lang.is_empty() => "Code".to_string(),
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
                highlight::html(lang, &b.body)
            )
        }
        "question" => format!(
            "<aside class=\"callout callout-question\">{}<p>{}</p>{}</aside>",
            marker(ctx, t, "Open question"),
            inline::spans_html(b.field_str("prompt").unwrap_or(""), &ictx),
            body()
        ),
        // a decision has no place in the text: the trail carries it
        rdy_model::decision::DECISION => String::new(),
        // prose, and a type with no rendering of its own (presentational or not): plain text,
        // with no type label; its primary field (a statement, prompt, name or title) first, so
        // that a block without a body still shows what it is
        _ => {
            // a statement or a prompt is a sentence (spans); a name or a title is a label
            let primary = PRIMARY_FIELDS
                .iter()
                .find_map(|k| Some(*k).zip(b.field_str(k).filter(|s| !s.trim().is_empty())))
                .map(|(k, p)| {
                    let html = match k {
                        "statement" | "prompt" => inline::spans_html(p, &ictx),
                        _ => inline::label_html(p, &ictx),
                    };
                    format!("<p>{html}</p>")
                })
                .unwrap_or_default();
            format!("{primary}{}", body())
        }
    }
}

/// A table: wrapped so that it scrolls on its own, captioned, numeric columns right-aligned.
fn table_html(b: &Block, ctx: &BlockCtx, ictx: &inline::Ctx) -> String {
    let list = |name: &str| match b.fields.get(name) {
        Some(FieldValue::List(c)) => c.clone(),
        _ => vec![],
    };
    let (cols, rows) = (list("columns"), list("rows"));
    let width = rows
        .iter()
        .map(|r| match r {
            FieldValue::List(c) => c.len(),
            _ => 1,
        })
        .chain([cols.len()])
        .max()
        .unwrap_or(0);
    // a column is numeric when it has body cells and every one of them is a number
    let num: Vec<bool> = (0..width)
        .map(|i| {
            let mut cells = rows.iter().map(|r| match r {
                FieldValue::List(c) => c.get(i),
                other => (i == 0).then_some(other),
            });
            !rows.is_empty() && cells.all(|c| c.is_some_and(is_number))
        })
        .collect();
    let class = |i: usize| {
        if num.get(i).copied().unwrap_or(false) {
            " class=\"num\""
        } else {
            ""
        }
    };
    let head: String = cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("<th scope=\"col\"{}>{}</th>", class(i), cell_html(c, ictx)))
        .collect();
    let body: String = rows
        .iter()
        .map(|r| {
            let cells: String = match r {
                FieldValue::List(cells) => cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("<td{}>{}</td>", class(i), cell_html(c, ictx)))
                    .collect(),
                other => format!("<td{}>{}</td>", class(0), cell_html(other, ictx)),
            };
            format!("<tr>{cells}</tr>")
        })
        .collect();
    let caption = match b.field_str("caption").filter(|c| !c.trim().is_empty()) {
        Some(c) => words_html(c, ctx.link),
        None => format!("Table {}", ctx.table_number.unwrap_or(1)),
    };
    let normative = matches!(b.fields.get("normative"), Some(FieldValue::Bool(true)));
    // the marker sits just before the scrolling wrapper: it stays in view, and the wrapper holds
    // nothing but the table
    format!(
        "{}<div class=\"table-wrap{}\"><table><caption>{caption}</caption><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table></div>",
        if normative {
            marker(ctx, "table", "Normative")
        } else {
            String::new()
        },
        if normative { " normative" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_render_lists_maps_and_numbers_without_debug_output() {
        let s = |t: &str| FieldValue::Str(t.into());
        let items = [
            FieldValue::List(vec![s("a"), s("b")]),
            FieldValue::Map(
                [
                    ("k".to_string(), s("v")),
                    (
                        "n".to_string(),
                        FieldValue::List(vec![FieldValue::Int(1), FieldValue::Int(2)]),
                    ),
                ]
                .into(),
            ),
            FieldValue::Null,
            FieldValue::Int(3),
        ];
        assert_eq!(cell(&items[0]), "a, b");
        assert_eq!(cell(&items[1]), "k: v; n: 1, 2");
        assert_eq!(cell(&items[2]), "");
        assert_eq!(cell(&items[3]), "3");
    }

    #[test]
    fn media_types_and_sizes_read_as_words() {
        assert_eq!(human_media("application/pdf"), "PDF");
        assert_eq!(human_media("text/plain"), "Text");
        assert_eq!(human_media("image/svg+xml"), "Image");
        assert_eq!(human_media("application/zip"), "ZIP");
        assert_eq!(human_size(512), "512 bytes");
        assert_eq!(human_size(12 * 1024), "12 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[test]
    fn numbers_are_recognised_and_words_that_parse_as_floats_are_not() {
        for yes in ["1", "-2.5", "+3", ".5", "1e3"] {
            assert!(is_number(&FieldValue::Str(yes.into())), "{yes}");
        }
        for no in ["inf", "NaN", "", "a1", "infinity"] {
            assert!(!is_number(&FieldValue::Str(no.into())), "{no}");
        }
    }
}
