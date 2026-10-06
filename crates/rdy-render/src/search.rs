//! The search index: `search-index.json`, one entry per doc, section, term, question and folder,
//! read by the client in `theme/rdy-standard/js/search.js`.
use crate::inline::Form;
use crate::page::PageContext;
use crate::site::Site;
use rdy_engine::{E_IO, Materialized, RdyError};
use rdy_model::{Block, BlockId, FieldValue, RefMode, decision, tree};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The most characters of an entry's own text the index keeps.
const TEXT_MAX: usize = 400;

/// The most characters of a question's prompt its entry's title keeps (the text holds the rest).
const TITLE_MAX: usize = 120;

/// One searchable place on the site.
#[derive(Clone, Debug, Serialize)]
struct Entry {
    kind: &'static str,
    /// Site-relative page url.
    page: String,
    anchor: Option<String>,
    /// The title of the doc (or folder) the entry is on.
    doc: String,
    title: String,
    text: String,
}

#[derive(Default)]
pub struct Index {
    /// Entries with the tree position of their page; pages are built in no particular order.
    entries: Vec<(usize, Entry)>,
    /// A page's position in a depth-first walk of the tree, filled by the first page.
    rank: Option<BTreeMap<String, (usize, BlockId)>>,
}

/// The tree's folders and docs by page url, with each one's position in a depth-first walk in
/// sibling order.
fn tree_rank(m: &Materialized, site: &Site) -> BTreeMap<String, (usize, BlockId)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<(&Block, usize)> =
        m.children(None).into_iter().rev().map(|b| (b, 0)).collect();
    while let Some((b, level)) = stack.pop() {
        if !tree::is_tree_type(&b.block_type) || level > tree::MAX_FOLDER_DEPTH {
            continue;
        }
        let Some(page) = site.page(&b.id) else {
            continue;
        };
        let n = out.len();
        if out.insert(page, (n, b.id.clone())).is_some() {
            continue;
        }
        stack.extend(
            m.children(Some(&b.id))
                .into_iter()
                .rev()
                .map(|c| (c, level + 1)),
        );
    }
    out
}

/// `s` with whitespace collapsed and cut to `max` characters.
fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() >= max {
            break;
        }
    }
    out.chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// `text` as a reader sees it ([`Site::plain`]): Markdown flattened, a reference as its display
/// text or its target's label (nothing when it leads nowhere), `\{{` a literal `{{`.
fn plain(text: &str, mode: RefMode, form: Form, site: &Site) -> String {
    site.plain(text, mode, form, 0, &|_| String::new())
}

/// A field's words: a statement or prompt reads as running text, any other as a label.
fn field(b: &Block, name: &str, site: &Site) -> String {
    let form = match name {
        "statement" | "prompt" => Form::Spans,
        _ => Form::Label,
    };
    plain(b.field_str(name).unwrap_or(""), RefMode::Strict, form, site)
}

fn cell(v: &FieldValue, site: &Site) -> String {
    match v {
        FieldValue::Str(s) => plain(s, RefMode::Strict, Form::Spans, site),
        FieldValue::Bool(b) => b.to_string(),
        FieldValue::Int(i) => i.to_string(),
        _ => String::new(),
    }
}

/// The text a reader can find in `b` itself (not in its children), with its own whitespace.
fn own_text(b: &Block, site: &Site) -> String {
    let body = || plain(&b.body, RefMode::for_body(&b.block_type), Form::Body, site);
    let list = |name: &str| match b.fields.get(name) {
        Some(FieldValue::List(l)) => l.clone(),
        _ => vec![],
    };
    match b.block_type.as_str() {
        // a decision has no place in the text
        decision::DECISION => String::new(),
        "term" | "component" | "actor" | "interface" => {
            format!("{}: {}", field(b, "name", site), body())
        }
        "question" => field(b, "prompt", site),
        "diagram" | "figure" => field(b, "alt", site),
        "code" => b.body.clone(),
        "table" => {
            let rows = list("rows").into_iter().map(|r| match r {
                FieldValue::List(cells) => cells
                    .iter()
                    .map(|c| cell(c, site))
                    .collect::<Vec<_>>()
                    .join(" "),
                other => cell(&other, site),
            });
            list("columns")
                .iter()
                .map(|c| cell(c, site))
                .chain(rows)
                .collect::<Vec<_>>()
                .join(" ")
        }
        "flow" => {
            let steps = list("steps").into_iter().map(|s| match s {
                FieldValue::Str(t) => plain(&t, RefMode::Strict, Form::Spans, site),
                other => cell(&other, site),
            });
            std::iter::once(field(b, "name", site))
                .chain(steps)
                .chain(std::iter::once(body()))
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => body(),
    }
}

/// A section's own words: what its direct children that are not sections say. A rationale is
/// off the page (FW-D10), so it is off the index too; a doc's direct words are read the same way.
fn section_text(sec: &Block, m: &Materialized, site: &Site) -> String {
    let mut out = plain(&sec.body, RefMode::Strict, Form::Body, site);
    for c in m.children(Some(&sec.id)) {
        if c.block_type == "section" || c.block_type == crate::site::RATIONALE {
            continue;
        }
        out.push(' ');
        out.push_str(&own_text(c, site));
        if out.chars().count() > TEXT_MAX * 2 {
            break;
        }
    }
    clip(&out, TEXT_MAX)
}

/// The text of the first prose block under `doc`, in reading order.
fn first_prose(doc: &BlockId, m: &Materialized, site: &Site) -> String {
    let mut stack: Vec<&Block> = m.children(Some(doc)).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        if b.block_type == "prose" {
            return clip(&plain(&b.body, RefMode::Strict, Form::Body, site), TEXT_MAX);
        }
        stack.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    String::new()
}

/// Adds the page's entries to the index.
pub fn collect(index: &mut Index, page: &PageContext, m: &Materialized, site: &Site) {
    if page.kind != "doc" && page.kind != "folder" {
        return;
    }
    let Some((rank, id)) = index
        .rank
        .get_or_insert_with(|| tree_rank(m, site))
        .get(&page.page_url)
        .cloned()
    else {
        return;
    };
    let mut add = |entry: Entry| index.entries.push((rank, entry));
    let entry = |kind, anchor: Option<String>, title: String, text: String| Entry {
        kind,
        page: page.page_url.clone(),
        anchor: anchor.filter(|a| !a.is_empty()),
        doc: page.title.clone(),
        title,
        text,
    };
    if let Some(folder) = &page.body.folder {
        let text = clip(
            &site.description_words(folder.description.as_deref().unwrap_or("")),
            TEXT_MAX,
        );
        add(entry("folder", None, folder.title.clone(), text));
        return;
    }
    let Some(doc) = &page.body.doc else { return };
    // what the doc says directly (outside any section), else the first prose under it
    let direct = m
        .get(&id)
        .map(|d| section_text(d, m, site))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| first_prose(&id, m, site));
    add(entry("doc", None, doc.title.clone(), direct));
    for item in &page.items {
        let Some(b) = BlockId::parse(&item.id).ok().and_then(|id| m.get(&id)) else {
            continue;
        };
        match b.block_type.as_str() {
            "section" => {
                let title = item.title.clone().unwrap_or_else(|| site.label(b));
                add(entry(
                    "section",
                    Some(item.anchor.clone()),
                    title,
                    section_text(b, m, site),
                ));
            }
            "term" => {
                let title = item.title.clone().unwrap_or_else(|| site.label(b));
                let text = clip(&plain(&b.body, RefMode::Strict, Form::Body, site), TEXT_MAX);
                add(entry("term", Some(item.anchor.clone()), title, text));
            }
            "question" => {
                let prompt = clip(&field(b, "prompt", site), TEXT_MAX);
                let url = site.url_of(&b.id);
                let mut e = entry(
                    "question",
                    url.as_ref().and_then(|u| u.anchor.clone()),
                    clip(&prompt, TITLE_MAX),
                    prompt,
                );
                if let Some(u) = url {
                    e.page = u.page;
                }
                add(e);
            }
            _ => {}
        }
    }
}

/// Writes `search-index.json` under `out`, the entries in tree order.
pub fn write(index: &Index, site_name: &str, out: &Path) -> Result<(), RdyError> {
    let mut entries: Vec<&(usize, Entry)> = index.entries.iter().collect();
    entries.sort_by_key(|(rank, _)| *rank);
    let json = serde_json::json!({
        "version": 1,
        "site": site_name,
        "entries": entries.iter().map(|(_, e)| e).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string(&json).expect("json");
    std::fs::write(out.join("search-index.json"), text).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("cannot write the search index: {e}"),
            "check the output directory",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_collapses_whitespace_and_cuts_by_characters() {
        assert_eq!(clip("  a \n b\t c ", 400), "a b c");
        let long = "é".repeat(500);
        assert_eq!(clip(&long, 400).chars().count(), 400);
        assert_eq!(
            clip(&format!("{} {}", "x".repeat(399), "yy"), 400),
            "x".repeat(399)
        );
    }
}
