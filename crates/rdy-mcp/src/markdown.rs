//! `rdy_read` markdown (md1): every block's primary content and its id. Folders are path
//! headings; section headings count from their doc (doc `#`, its sections `##`).
use rdy_engine::{Materialized, field_json};
use rdy_model::{Block, FieldValue, decision, tree};

/// How far below its doc `b` sits (the doc itself is 0), or None outside a doc.
pub fn doc_depth(m: &Materialized, b: &Block) -> Option<usize> {
    let mut cur = b;
    for depth in 0..=m.blocks.len() {
        if cur.block_type == tree::DOC {
            return Some(depth);
        }
        if cur.block_type == tree::FOLDER {
            return None;
        }
        cur = m.get(cur.parent.as_ref()?)?;
    }
    None
}

fn text(v: &FieldValue) -> String {
    match v {
        FieldValue::Str(s) => s.clone(),
        other => field_json(other).to_string(),
    }
}

fn list(b: &Block, name: &str) -> Vec<String> {
    match b.fields.get(name) {
        Some(FieldValue::List(items)) => items.iter().map(text).collect(),
        _ => vec![],
    }
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// One block as markdown. `depth` is its depth below its doc ([`doc_depth`]); `status` is a
/// decision's derived status.
pub fn block_markdown(
    b: &Block,
    m: &Materialized,
    depth: Option<usize>,
    status: Option<&str>,
) -> String {
    let f = |name: &str| b.field_str(name).unwrap_or("").to_string();
    let id = b.id.to_string();
    let with_body = |head: String| {
        if b.body.is_empty() {
            head
        } else {
            format!("{head}\n\n{}", b.body)
        }
    };
    match b.block_type.as_str() {
        t if t == tree::FOLDER => {
            let path = m.path_of(&b.id).unwrap_or_else(|| f("name"));
            format!("# {path}/ {{#{id}}}")
        }
        t if t == tree::DOC => format!("# {} {{#{id}}}", f("title")),
        "section" => with_body(format!(
            "{} {} {{#{id}}}",
            "#".repeat((depth.unwrap_or(1) + 1).min(6)),
            f("title")
        )),
        "term" | "component" | "actor" | "interface" => {
            format!("`{id}` **{}** — {}", f("name"), b.body)
        }
        t if t == decision::DECISION => {
            let stored = f("status");
            let mut out = vec![format!(
                "> `{id}` decision [{}]: {}",
                status.unwrap_or(&stored),
                f("statement")
            )];
            out.extend(list(b, "options").into_iter().map(|o| format!("> - {o}")));
            if !b.body.is_empty() {
                out.push(">".into());
                out.extend(b.body.lines().map(|l| format!("> {l}")));
            }
            out.join("\n")
        }
        t if t == decision::QUESTION => with_body(format!("`{id}` **Question:** {}", f("prompt"))),
        "flow" => {
            let steps: Vec<String> = list(b, "steps")
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {s}", i + 1))
                .collect();
            with_body(format!(
                "`{id}` **Flow: {}**\n\n{}",
                f("name"),
                steps.join("\n")
            ))
        }
        "figure" => format!(
            "`{id}` ![{}](rdy-blob:{}) ({})",
            f("alt"),
            f("blob"),
            f("media_type")
        ),
        "diagram" => format!("`{id}` {}\n\n```{}\n{}\n```", f("alt"), f("lang"), b.body),
        "code" => format!("`{id}`\n\n```{}\n{}\n```", f("lang"), b.body),
        "table" => {
            let cols = list(b, "columns");
            let mut out = vec![format!("`{id}`"), String::new()];
            out.push(format!(
                "| {} |",
                cols.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
            ));
            out.push(format!("|{}", " --- |".repeat(cols.len().max(1))));
            if let Some(FieldValue::List(rows)) = b.fields.get("rows") {
                for row in rows {
                    let cells: Vec<String> = match row {
                        FieldValue::List(cs) => cs.iter().map(|v| cell(&text(v))).collect(),
                        other => vec![cell(&text(other))],
                    };
                    out.push(format!("| {} |", cells.join(" | ")));
                }
            }
            out.join("\n")
        }
        _ => format!("`{id}` {}", b.body),
    }
}
