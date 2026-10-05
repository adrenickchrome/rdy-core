//! The designed rendering of every content type, and the references inside text.
use crate::highlight;
use rdy_model::{Block, BlockId, FieldValue, Hash, RefMode, ref_spans};
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
}

/// What an unresolved reference reads as. In strict mode the author wrote only an id, which a
/// reader may not see, so the marker's own words stand in for it.
const UNRESOLVED: &str = "unresolved reference";

/// Escapes `text` and replaces `{{id}}` references with links; `\{{` renders as a literal `{{`.
/// A reference that leads nowhere is a marker that names no id. A newline is a `<br>`.
pub fn text_html(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(
        text,
        RefMode::Strict,
        &|tok| match BlockId::parse(tok).ok().and_then(|id| link(&id)) {
            Some(l) => anchor(&l),
            None => format!(
                "<span class=\"ref unresolved\" title=\"This reference could not be resolved\">{UNRESOLVED}</span>"
            ),
        },
    )
    .replace('\n', "<br>")
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

/// The text's paragraphs (separated by blank lines), each as a `<p>`; nothing for empty text.
fn paragraphs(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    split_paragraphs(text)
        .iter()
        .map(|p| format!("<p>{}</p>", text_html(p, link)))
        .collect()
}

/// The text inline when it is one paragraph, as `<p>`s when it is several.
fn inline_or_paragraphs(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    match split_paragraphs(text).as_slice() {
        [] => String::new(),
        [one] => text_html(one, link),
        _ => paragraphs(text, link),
    }
}

fn split_paragraphs(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Vec<&str> = vec![];
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
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

/// The label a diagram shows for the reference `tok`: the target's label, else the unresolved
/// marker's words, never the id.
fn diagram_label(tok: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    BlockId::parse(tok)
        .ok()
        .and_then(|id| link(&id))
        .map_or_else(|| UNRESOLVED.to_string(), |l| l.label)
}

/// Escaped diagram source with each id-shaped `{{id}}` replaced by its label (see
/// [`diagram_label`]), escaped for Mermaid and then for HTML, so the diagram renderer sees plain
/// text. Other `{{…}}` (e.g. Mermaid hexagons) is kept as written.
pub fn diagram_source(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(text, RefMode::IdShaped, &|tok| {
        escape(&mermaid_label(&diagram_label(tok, link)))
    })
}

/// The diagram's source as a reader reads it under "Diagram source": escaped, its references as
/// labels (see [`diagram_label`]), so no id is shown.
fn readable_source(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(text, RefMode::IdShaped, &|tok| {
        escape(&diagram_label(tok, link))
    })
}

/// Escapes the text between references (turning `\{{` into `{{`) and renders each reference with `r`.
fn with_refs(text: &str, mode: RefMode, r: &dyn Fn(&str) -> String) -> String {
    let plain = |s: &str| escape(&s.replace("\\{{", "{{"));
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&plain(&text[last..span.range.start]));
        out.push_str(&r(&span.token));
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
    let alt = escape(b.field_str("alt").unwrap_or(""));
    let media = b.field_str("media_type").unwrap_or("");
    let image = media.starts_with("image/");
    match h.parse::<Hash>() {
        Ok(hash) if (ctx.present)(&hash) => {
            let src = escape(&(ctx.asset_href)(&hash, media));
            if image {
                format!(
                    "<figure class=\"figure\"><img src=\"{src}\" alt=\"{alt}\" loading=\"lazy\"><figcaption>{alt}</figcaption></figure>"
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
            "<p class=\"figure-missing\">{alt} ({} not available)</p>",
            if image { "image" } else { "file" }
        ),
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

/// The marker (eyebrow) of a block, or nothing for a presentational type.
fn marker(ctx: &BlockCtx, block_type: &str, words: &str) -> String {
    if ctx.pack.is_presentational(block_type) {
        String::new()
    } else {
        format!("<span class=\"marker\">{}</span>", escape(words))
    }
}

/// Inner HTML for one block. A block the site leaves out (a rationale, a decision) is empty.
pub fn block_html(b: &Block, ctx: &BlockCtx) -> String {
    let (depth, link) = (ctx.depth, ctx.link);
    let t = b.block_type.as_str();
    let f = |name: &str| escape(b.field_str(name).unwrap_or(""));
    // a section's heading is one level below its parent section's; a definition or a flow heads
    // a part of the section it sits in, at the level of that section's subsections
    let section_level = (depth + 1).min(6);
    let item_level = depth.clamp(2, 6);
    match t {
        "section" => format!(
            "<h{section_level} class=\"section-title\">{}</h{section_level}>{}",
            f("title"),
            paragraphs(&b.body, link)
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
                paragraphs(&b.body, link)
            )
        }
        "example" | "note" => format!(
            "<aside class=\"callout callout-{t}\">{}<div>{}</div></aside>",
            marker(ctx, t, &display_name(t)),
            paragraphs(&b.body, link)
        ),
        // supporting text: it stays in the page JSON, not on the page
        "rationale" => String::new(),
        "term" => format!(
            "<dl class=\"term\"><dt id=\"{}\">{}</dt><dd>{}</dd></dl>",
            escape(ctx.anchor),
            f("name"),
            inline_or_paragraphs(&b.body, link)
        ),
        "component" | "actor" | "interface" => {
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
            format!(
                "<div class=\"entity entity-{t}\">{}<h{item_level} class=\"entity-name\">{}</h{item_level}>{}{described}</div>",
                marker(ctx, t, &display_name(t)),
                f("name"),
                paragraphs(&b.body, link)
            )
        }
        "flow" => {
            let steps: String = match b.fields.get("steps") {
                Some(FieldValue::List(s)) => s
                    .iter()
                    .map(|s| match s {
                        FieldValue::Str(t) => format!("<li>{}</li>", text_html(t, link)),
                        o => format!("<li>{}</li>", cell(o)),
                    })
                    .collect(),
                _ => String::new(),
            };
            format!(
                "<div class=\"flow\"><h{item_level} class=\"flow-name\">{}</h{item_level}><ol class=\"steps\">{steps}</ol>{}</div>",
                f("name"),
                paragraphs(&b.body, link)
            )
        }
        "table" => table_html(b, ctx),
        "diagram" => {
            let lang = b.field_str("lang").unwrap_or("");
            let alt = f("alt");
            // only Mermaid has a renderer on the site; any other language shows its source
            let canvas = if lang == "mermaid" {
                format!(
                    "<div class=\"diagram-canvas\" aria-hidden=\"true\"><pre class=\"mermaid\">{}</pre></div>",
                    diagram_source(&b.body, link)
                )
            } else {
                String::new()
            };
            format!(
                "<figure class=\"diagram\" data-lang=\"{}\"><p class=\"visually-hidden\">{alt}</p>{canvas}<figcaption>{alt}</figcaption><details class=\"diagram-source\"><summary>Diagram source</summary><pre class=\"code\"><code>{}</code></pre></details></figure>",
                escape(lang),
                readable_source(&b.body, link)
            )
        }
        "figure" => figure_html(b, ctx),
        "code" => {
            let lang = b.field_str("lang").unwrap_or("");
            let human = match highlight::human_name(lang) {
                n if n.is_empty() => "Code".to_string(),
                n => n,
            };
            format!(
                "<div class=\"code-block\"><div class=\"code-head\"><span class=\"lang\">{}</span><button type=\"button\" class=\"tool copy-code\" aria-label=\"Copy code\">Copy</button></div><pre class=\"code\"><code class=\"language-{}\">{}</code></pre></div>",
                escape(&human),
                escape(lang),
                highlight::html(lang, &b.body)
            )
        }
        "question" => format!(
            "<aside class=\"callout callout-question\">{}<p>{}</p>{}</aside>",
            marker(ctx, t, "Open question"),
            text_html(b.field_str("prompt").unwrap_or(""), link),
            paragraphs(&b.body, link)
        ),
        // a decision has no place in the text: the trail and the JSON carry it
        rdy_model::decision::DECISION => String::new(),
        // prose, and a type with no rendering of its own (presentational or not): plain text,
        // with no type label
        _ => paragraphs(&b.body, link),
    }
}

/// A table: wrapped so that it scrolls on its own, captioned, numeric columns right-aligned.
fn table_html(b: &Block, ctx: &BlockCtx) -> String {
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
        .map(|(i, c)| format!("<th scope=\"col\"{}>{}</th>", class(i), cell(c)))
        .collect();
    let body: String = rows
        .iter()
        .map(|r| {
            let cells: String = match r {
                FieldValue::List(cells) => cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("<td{}>{}</td>", class(i), cell(c)))
                    .collect(),
                other => format!("<td{}>{}</td>", class(0), cell(other)),
            };
            format!("<tr>{cells}</tr>")
        })
        .collect();
    let caption = match b.field_str("caption").filter(|c| !c.trim().is_empty()) {
        Some(c) => text_html(c, ctx.link),
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
