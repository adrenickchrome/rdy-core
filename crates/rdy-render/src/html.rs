use rdy_model::{Block, BlockId, FieldValue};

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

/// Escapes `text`, then replaces `{{id}}` references with links. Braces survive escaping unchanged.
pub fn text_html(text: &str, link: &dyn Fn(&BlockId) -> Option<(String, String)>) -> String {
    refs_html(&escape(text), link).replace('\n', "<br>")
}

fn refs_html(escaped: &str, link: &dyn Fn(&BlockId) -> Option<(String, String)>) -> String {
    let mut out = String::new();
    let mut rest = escaped;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tok = after[..end].trim();
        match BlockId::parse(tok).ok().and_then(|id| link(&id)) {
            Some((href, label)) => out.push_str(&format!("<a href=\"{}\">{}</a>", escape(&href), escape(&label))),
            None => out.push_str(&format!("<span class=\"ref missing\">{tok}</span>")),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
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
pub fn block_html(b: &Block, depth: usize, link: &dyn Fn(&BlockId) -> Option<(String, String)>, decision_href: &str, status: &str) -> String {
    let body = text_html(&b.body, link);
    let f = |name: &str| escape(b.field_str(name).unwrap_or(""));
    match b.block_type.as_str() {
        "section" => format!("<h{lvl}>{}</h{lvl}>{}", f("title"), if b.body.is_empty() { String::new() } else { format!("<p>{body}</p>") }, lvl = (depth + 1).min(6)),
        "term" | "component" | "actor" | "interface" => format!("<dl class=\"term\"><dt>{}</dt><dd>{body}</dd></dl>", f("name")),
        "decision" => format!(
            "<div class=\"decision\"><a href=\"{}\">{}</a> <span class=\"badge {}\">{status}</span></div>",
            escape(decision_href),
            text_html(b.field_str("statement").unwrap_or(""), link),
            status.replace(' ', "-")
        ),
        "question" => format!("<p><strong>Open question:</strong> {}</p>", text_html(b.field_str("prompt").unwrap_or(""), link)),
        "diagram" => format!("<figure><pre class=\"mermaid\">{}</pre><figcaption>{}</figcaption></figure>", escape(&b.body), f("alt")),
        "code" => format!("<pre><code class=\"language-{}\">{}</code></pre>", f("lang"), escape(&b.body)),
        "table" => {
            let cols = match b.fields.get("columns") { Some(FieldValue::List(c)) => c.clone(), _ => vec![] };
            let rows = match b.fields.get("rows") { Some(FieldValue::List(r)) => r.clone(), _ => vec![] };
            let head: String = cols.iter().map(|c| format!("<th>{}</th>", cell(c))).collect();
            let body_rows: String = rows
                .iter()
                .map(|r| match r {
                    FieldValue::List(cells) => format!("<tr>{}</tr>", cells.iter().map(|c| format!("<td>{}</td>", cell(c))).collect::<String>()),
                    other => format!("<tr><td>{}</td></tr>", cell(other)),
                })
                .collect();
            format!("<table><thead><tr>{head}</tr></thead><tbody>{body_rows}</tbody></table>")
        }
        "flow" => {
            let steps = match b.fields.get("steps") { Some(FieldValue::List(s)) => s.clone(), _ => vec![] };
            let li: String = steps.iter().map(|s| format!("<li>{}</li>", match s { FieldValue::Str(t) => text_html(t, link), o => cell(o) })).collect();
            format!("<h4>{}</h4><ol>{li}</ol>{}", f("name"), if b.body.is_empty() { String::new() } else { format!("<p>{body}</p>") })
        }
        other => format!("<p><span class=\"meta\">{}</span> {body}</p>", escape(other)),
    }
}
