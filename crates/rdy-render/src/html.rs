use rdy_model::{Block, BlockId, FieldValue, RefMode, ref_spans};

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

/// Escapes `text` and replaces `{{id}}` references with links; `\{{` renders as a literal `{{`.
pub fn text_html(text: &str, link: &dyn Fn(&BlockId) -> Option<(String, String)>) -> String {
    with_refs(
        text,
        RefMode::Strict,
        &|tok| match BlockId::parse(tok).ok().and_then(|id| link(&id)) {
            Some((href, label)) => format!("<a href=\"{}\">{}</a>", escape(&href), escape(&label)),
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
pub fn diagram_source(text: &str, link: &dyn Fn(&BlockId) -> Option<(String, String)>) -> String {
    with_refs(text, RefMode::IdShaped, &|tok| match BlockId::parse(tok)
        .ok()
        .and_then(|id| link(&id))
    {
        Some((_, label)) => escape(&mermaid_label(&label)),
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

fn cell(v: &FieldValue) -> String {
    match v {
        FieldValue::Null => String::new(),
        FieldValue::Bool(b) => b.to_string(),
        FieldValue::Int(i) => i.to_string(),
        FieldValue::Str(s) => escape(s),
        other => escape(&format!("{other:?}")),
    }
}

/// Inner HTML for one block. `depth` is the section depth (doc = 0).
pub fn block_html(
    b: &Block,
    depth: usize,
    link: &dyn Fn(&BlockId) -> Option<(String, String)>,
    decision_href: &str,
    status: &str,
) -> String {
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
        "decision" => format!(
            "<div class=\"decision\"><a href=\"{}\">{}</a> <span class=\"badge {}\">{status}</span></div>",
            escape(decision_href),
            text_html(b.field_str("statement").unwrap_or(""), link),
            status.replace(' ', "-")
        ),
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
            escape(&b.body)
        ),
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
        other => format!(
            "<p><span class=\"meta\">{}</span> {body}</p>",
            escape(other)
        ),
    }
}
