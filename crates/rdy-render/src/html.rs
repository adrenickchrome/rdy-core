use crate::highlight;
use rdy_model::{Block, BlockId, FieldValue, Hash, RefMode, ref_spans};
use rdy_schema::Pack;

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
    /// The hover text; empty for none.
    pub title: String,
}

/// Escapes `text` and replaces `{{id}}` references with links; `\{{` renders as a literal `{{`.
pub fn text_html(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(
        text,
        RefMode::Strict,
        &|tok| match BlockId::parse(tok).ok().and_then(|id| link(&id)) {
            Some(l) if l.title.is_empty() => {
                format!("<a href=\"{}\">{}</a>", escape(&l.href), escape(&l.label))
            }
            Some(l) => format!(
                "<a href=\"{}\" title=\"{}\">{}</a>",
                escape(&l.href),
                escape(&l.title),
                escape(&l.label)
            ),
            None => format!("<span class=\"ref missing\">{}</span>", escape(tok)),
        },
    )
    .replace('\n', "<br>")
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

/// Escaped diagram source with each id-shaped `{{id}}` replaced by the target's label, escaped for
/// Mermaid and then for HTML, so the diagram renderer sees plain text; other `{{…}}` (e.g. Mermaid
/// hexagons) is kept as written.
pub fn diagram_source(text: &str, link: &dyn Fn(&BlockId) -> Option<Link>) -> String {
    with_refs(text, RefMode::IdShaped, &|tok| match BlockId::parse(tok)
        .ok()
        .and_then(|id| link(&id))
    {
        Some(l) => escape(&mermaid_label(&l.label)),
        None => escape(tok),
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

/// A figure: its image or a download link, or a placeholder when the store lacks the blob.
fn figure_html(b: &Block, ctx: &BlockCtx) -> String {
    let h = b.field_str("blob").unwrap_or("");
    let alt = escape(b.field_str("alt").unwrap_or(""));
    match h.parse::<Hash>() {
        Ok(hash) if (ctx.present)(&hash) => {
            let media = b.field_str("media_type").unwrap_or("");
            let src = escape(&(ctx.asset_href)(&hash, media));
            if media.starts_with("image/") {
                format!(
                    "<figure><img src=\"{src}\" alt=\"{alt}\"><figcaption>{alt}</figcaption></figure>"
                )
            } else {
                format!("<p><a href=\"{src}\" download>{alt}</a></p>")
            }
        }
        _ => format!("<p class=\"asset missing\">missing asset: {alt}</p>"),
    }
}

fn cell(v: &FieldValue) -> String {
    match v {
        FieldValue::Null => String::new(),
        FieldValue::Bool(b) => b.to_string(),
        FieldValue::Int(i) => i.to_string(),
        FieldValue::Str(s) => escape(s),
        other => escape(&format!("{other:?}")),
    }
}

/// Everything [`block_html`] reads besides the block: where it sits, how to link and where an
/// asset's file lives. Built by `page.rs`, one per block.
// the fields a later task's rendering reads are filled now so that no call site changes again
#[allow(dead_code)]
pub struct BlockCtx<'a> {
    /// The section depth (the doc's children are at 1).
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

/// Inner HTML for one block.
pub fn block_html(b: &Block, ctx: &BlockCtx) -> String {
    let (depth, link) = (ctx.depth, ctx.link);
    let body = text_html(&b.body, link);
    let f = |name: &str| escape(b.field_str(name).unwrap_or(""));
    match b.block_type.as_str() {
        "section" => format!(
            "<h{lvl}>{}</h{lvl}>{}",
            f("title"),
            if b.body.is_empty() {
                String::new()
            } else {
                format!("<p>{body}</p>")
            },
            lvl = (depth + 1).min(6)
        ),
        "term" | "component" | "actor" | "interface" => format!(
            "<dl class=\"term\"><dt>{}</dt><dd>{body}</dd></dl>",
            f("name")
        ),
        // a decision has no page and no place in the text: the trail and the JSON carry it
        "decision" => String::new(),
        "question" => format!(
            "<p><strong>Open question:</strong> {}</p>",
            text_html(b.field_str("prompt").unwrap_or(""), link)
        ),
        "diagram" => format!(
            "<figure><pre class=\"mermaid\">{}</pre><figcaption>{}</figcaption></figure>",
            diagram_source(&b.body, link),
            f("alt")
        ),
        "code" => format!(
            "<pre><code class=\"language-{}\">{}</code></pre>",
            f("lang"),
            highlight::html(b.field_str("lang").unwrap_or(""), &b.body)
        ),
        "figure" => figure_html(b, ctx),
        "table" => {
            let cols = match b.fields.get("columns") {
                Some(FieldValue::List(c)) => c.clone(),
                _ => vec![],
            };
            let rows = match b.fields.get("rows") {
                Some(FieldValue::List(r)) => r.clone(),
                _ => vec![],
            };
            let head: String = cols
                .iter()
                .map(|c| format!("<th>{}</th>", cell(c)))
                .collect();
            let body_rows: String = rows
                .iter()
                .map(|r| match r {
                    FieldValue::List(cells) => format!(
                        "<tr>{}</tr>",
                        cells
                            .iter()
                            .map(|c| format!("<td>{}</td>", cell(c)))
                            .collect::<String>()
                    ),
                    other => format!("<tr><td>{}</td></tr>", cell(other)),
                })
                .collect();
            format!("<table><thead><tr>{head}</tr></thead><tbody>{body_rows}</tbody></table>")
        }
        "flow" => {
            let steps = match b.fields.get("steps") {
                Some(FieldValue::List(s)) => s.clone(),
                _ => vec![],
            };
            let li: String = steps
                .iter()
                .map(|s| {
                    format!(
                        "<li>{}</li>",
                        match s {
                            FieldValue::Str(t) => text_html(t, link),
                            o => cell(o),
                        }
                    )
                })
                .collect();
            format!(
                "<h4>{}</h4><ol>{li}</ol>{}",
                f("name"),
                if b.body.is_empty() {
                    String::new()
                } else {
                    format!("<p>{body}</p>")
                }
            )
        }
        // a presentational type has no label: it is plain text with no marker
        other if ctx.pack.is_presentational(other) => format!("<p>{body}</p>"),
        other => format!(
            "<p><span class=\"meta\">{}</span> {body}</p>",
            escape(other)
        ),
    }
}
