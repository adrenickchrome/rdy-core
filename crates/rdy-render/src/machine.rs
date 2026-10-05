//! The machine-readable twins of the pages and `llms.txt` (FW-D5): the file an agent reads in
//! place of a page. Raw ids, hashes and type names are allowed here; the pages never show them.
use crate::inline::Form;
use crate::page::PageContext;
use crate::site::Site;
use rdy_engine::{E_IO, Materialized, RdyError, VersionPosition};
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue, RefMode, decision};
use rdy_schema::Pack;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

/// The schema name every twin carries.
const SCHEMA: &str = "rdy-page/1";

/// What `llms.txt` says when the project has no description.
const DEFAULT_DESCRIPTION: &str = "Design documentation rendered by rdy.";

/// The most characters of a page's first prose that `llms.txt` shows beside its link.
const BLURB_MAX: usize = 160;

/// The twin of one page. The fields are in the order the file shows them: serde writes a
/// struct's fields in declaration order, which a `serde_json::Value` would not.
#[derive(Serialize)]
pub struct PageTwin {
    schema: &'static str,
    site: SiteJson,
    version: VersionJson,
    page: PageJson,
    ids: Vec<String>,
    blocks: Vec<BlockJson>,
    decisions: Vec<DecisionJson>,
    links: LinksJson,
}

#[derive(Serialize)]
struct SiteJson {
    name: String,
    description: Option<String>,
}

#[derive(Serialize)]
struct VersionJson {
    label: String,
    name: Option<String>,
    landing: Option<String>,
    landings_since: u32,
    view: Option<ViewJson>,
    frozen: bool,
}

#[derive(Serialize)]
struct ViewJson {
    name: String,
    pending: u32,
}

#[derive(Serialize)]
struct PageJson {
    kind: &'static str,
    id: Option<String>,
    url: String,
    title: String,
    doc_type: Option<String>,
    lifecycle: Option<String>,
    permalink: Option<String>,
}

#[derive(Serialize)]
struct BlockJson {
    id: String,
    #[serde(rename = "type")]
    block_type: String,
    anchor: String,
    parent: Option<String>,
    title: Option<String>,
    summary: String,
    lifecycle: Option<String>,
    governing: Vec<GoverningJson>,
    suspect: bool,
    presentational: bool,
}

#[derive(Serialize)]
struct GoverningJson {
    id: String,
    status: &'static str,
}

#[derive(Serialize)]
struct DecisionJson {
    id: String,
    statement: String,
    status: &'static str,
    decided_at: Option<String>,
    authority_basis: Option<String>,
    options: Vec<String>,
    supersedes: Vec<String>,
    revokes: Vec<String>,
    affects: Vec<String>,
    governs: Vec<String>,
}

#[derive(Serialize)]
struct LinksJson {
    prev: Option<String>,
    next: Option<String>,
    parent: Option<String>,
    search: &'static str,
    index: &'static str,
}

fn owned(s: Option<&str>) -> Option<String> {
    s.map(String::from)
}

fn id_list(ids: &[BlockId]) -> Vec<String> {
    ids.iter().map(|i| i.to_string()).collect()
}

/// The engine's word for a decision's derived status.
fn status_word(s: &DerivedStatus) -> &'static str {
    match s {
        DerivedStatus::Proposed => "proposed",
        DerivedStatus::Rejected => "rejected",
        DerivedStatus::Live => "live",
        DerivedStatus::PartiallySuperseded { .. } => "partially superseded",
        DerivedStatus::Superseded { .. } => "superseded",
        DerivedStatus::Revoked { .. } => "revoked",
    }
}

fn link_targets(b: &Block, kind: &str) -> Vec<BlockId> {
    b.links
        .get(kind)
        .into_iter()
        .flatten()
        .map(|l| l.target.clone())
        .collect()
}

/// The blocks a decision governs through links: what it affects and what names it as
/// `decided_by` or `implements`.
fn link_governs(g: &Graph, d: &Block) -> Vec<BlockId> {
    let mut out: Vec<BlockId> = link_targets(d, decision::AFFECTS);
    for e in g.incoming(&d.id) {
        if matches!(&e.kind, EdgeKind::Link(n) if n == decision::DECIDED_BY || n == decision::IMPLEMENTS)
            && !out.contains(&e.from)
        {
            out.push(e.from.clone());
        }
    }
    out
}

/// The blocks a decision governs: what `link_governs` says; its parent when that is nothing,
/// as `Site::compute_governed` does, so the page its permalink leads to lists it. Every target is
/// listed here; the permalink and references lead to the one read first (`Site::destination`).
fn governs(g: &Graph, d: &Block) -> Vec<BlockId> {
    let mut out = link_governs(g, d);
    if out.is_empty() {
        out.extend(d.parent.clone());
    }
    out
}

/// Every decision, whatever its status, that governs `id`: one the block names as `decided_by`
/// or `implements`, one that `affects` it, and a decision under it that governs nothing else
/// through links. In id order, as the engine's `governing` is.
fn governing(r: &Read, id: &BlockId) -> BTreeSet<BlockId> {
    let g = r.g;
    let named = g
        .outgoing(id)
        .iter()
        .filter(|e| {
            matches!(&e.kind, EdgeKind::Link(n) if n == decision::DECIDED_BY || n == decision::IMPLEMENTS)
        })
        .map(|e| e.to.clone());
    let affecting = g
        .incoming(id)
        .iter()
        .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS))
        .map(|e| e.from.clone());
    let under =
        r.m.children(Some(id))
            .into_iter()
            .filter(|c| c.block_type == decision::DECISION && link_governs(g, c).is_empty())
            .map(|c| c.id.clone());
    named
        .chain(affecting)
        .chain(under)
        .filter(|d| g.status(d).is_some())
        .collect()
}

/// A running text as plain words on one line (a reference with no display text reads as its
/// target's label, one that leads nowhere as the unresolved marker's words).
fn spans_words(site: &Site, text: &str) -> String {
    let words = site.plain(text, RefMode::Strict, Form::Spans, 0, &|_| {
        crate::html::UNRESOLVED.to_string()
    });
    one_line(&words)
}

fn options_of(d: &Block, site: &Site) -> Vec<String> {
    match d.fields.get("options") {
        Some(FieldValue::List(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(|o| spans_words(site, o))
            .collect(),
        _ => vec![],
    }
}

fn decision_json(d: &Block, g: &Graph, site: &Site) -> DecisionJson {
    DecisionJson {
        id: d.id.to_string(),
        statement: spans_words(site, d.field_str("statement").unwrap_or("")),
        status: g.status(&d.id).map_or("proposed", |s| status_word(&s)),
        decided_at: owned(d.field_str("decided_at")),
        authority_basis: owned(d.field_str("authority_basis")),
        options: options_of(d, site),
        supersedes: id_list(&link_targets(d, decision::SUPERSEDES)),
        revokes: id_list(&link_targets(d, decision::REVOKES)),
        affects: id_list(&link_targets(d, decision::AFFECTS)),
        governs: id_list(&governs(g, d)),
    }
}

/// What the twin reads about the whole project, whichever page it is for.
struct Read<'a> {
    m: &'a Materialized,
    g: &'a Graph,
    suspects: &'a BTreeSet<BlockId>,
    site: &'a Site<'a>,
}

/// A block as the twin lists it. `anchor` is where it sits on the page; `title` what it is
/// called, when it has a name. The decisions that govern it are added to `seen`, once each.
fn block_json(
    r: &Read,
    id: &BlockId,
    anchor: &str,
    title: Option<&str>,
    presentational: bool,
    seen: &mut Vec<BlockId>,
) -> Option<BlockJson> {
    let b = r.m.get(id)?;
    // a presentational block has no trail, so no decision governs it for a reader
    let governing: Vec<GoverningJson> = if presentational {
        vec![]
    } else {
        governing(r, id)
            .into_iter()
            .map(|d| {
                let entry = GoverningJson {
                    id: d.to_string(),
                    status: r.g.status(&d).map_or("proposed", |s| status_word(&s)),
                };
                if !seen.contains(&d) {
                    seen.push(d);
                }
                entry
            })
            .collect()
    };
    Some(BlockJson {
        id: id.to_string(),
        block_type: b.block_type.clone(),
        anchor: anchor.to_string(),
        parent: b.parent.as_ref().map(|p| p.to_string()),
        title: owned(title),
        summary: r.site.summary(b),
        lifecycle: owned(b.field_str("lifecycle")),
        governing,
        suspect: r.suspects.contains(id),
        presentational,
    })
}

/// The folder or doc whose page is `url`.
fn tree_block<'a>(site: &Site<'a>, url: &str) -> Option<&'a Block> {
    site.tree_pages()
        .find(|id| site.page(id).as_deref() == Some(url))
        .and_then(|id| site.m.get(id))
}

/// The twin of one page.
///
/// `ids` are the block ids the page's markup marks (`data-rdy-block`): a doc page's blocks.
/// The glossary's and the questions page's entries, and a doc's rationale blocks (supporting
/// text, off the page), are in `blocks` but carry no mark in the markup, so they are not in
/// `ids`. Every path is relative to the site's root, as `page.url`.
pub fn page_json(
    ctx: &PageContext,
    m: &Materialized,
    g: &Graph,
    suspects: &BTreeSet<BlockId>,
    pos: &VersionPosition,
    site: &Site,
    pack: &Pack,
) -> PageTwin {
    let r = Read {
        m,
        g,
        suspects,
        site,
    };
    let own = matches!(ctx.kind, "doc" | "folder")
        .then(|| tree_block(site, &ctx.page_url))
        .flatten();
    let mut seen: Vec<BlockId> = vec![];
    let mut ids: Vec<String> = vec![];
    let mut blocks: Vec<BlockJson> = vec![];
    match ctx.kind {
        "doc" => {
            for i in &ctx.items {
                let Ok(id) = BlockId::parse(&i.id) else {
                    continue;
                };
                // a supporting block (a rationale) is listed but carries no mark on the page
                if !i.supporting {
                    ids.push(i.id.clone());
                }
                blocks.extend(block_json(
                    &r,
                    &id,
                    &i.anchor,
                    i.title.as_deref(),
                    i.presentational,
                    &mut seen,
                ));
            }
        }
        "glossary" | "questions" => {
            let (list, anchors) = if ctx.kind == "glossary" {
                (&site.terms, &site.term_anchors)
            } else {
                (&site.open_questions, &site.question_anchors)
            };
            for id in list {
                let Some(b) = m.get(id) else { continue };
                blocks.extend(block_json(
                    &r,
                    id,
                    anchors.get(id).map_or("", String::as_str),
                    b.field_str("name"),
                    pack.is_presentational(&b.block_type),
                    &mut seen,
                ));
            }
        }
        _ => {}
    }
    let decisions: Vec<DecisionJson> = seen
        .iter()
        .filter_map(|d| m.get(d))
        .map(|d| decision_json(d, g, site))
        .collect();

    let (prev, next) = match (ctx.kind, own) {
        ("doc", Some(b)) => {
            let page = |id: Option<BlockId>| id.and_then(|id| site.page(&id));
            let (p, n) = site.prev_next(&b.id);
            (page(p), page(n))
        }
        _ => (None, None),
    };
    let parent = match (ctx.kind, own) {
        ("home", _) => None,
        (_, Some(b)) => Some(
            b.parent
                .as_ref()
                .and_then(|p| site.page(p))
                .unwrap_or_else(|| "index.html".to_string()),
        ),
        _ => Some("index.html".to_string()),
    };
    let (landing, name) = match &pos.version {
        Some(v) => (Some(v.landing.as_str()), Some(v.name.as_str())),
        None => (None, None),
    };
    PageTwin {
        schema: SCHEMA,
        site: SiteJson {
            name: ctx.site_name.clone(),
            description: ctx.site_description.clone(),
        },
        version: VersionJson {
            label: ctx.version.text.clone(),
            name: name.map(String::from),
            landing: landing.map(String::from),
            landings_since: pos.landings_since,
            view: pos.view.as_ref().map(|v| ViewJson {
                name: v.name.clone(),
                pending: v.unlanded_changes,
            }),
            frozen: pos.frozen,
        },
        page: PageJson {
            kind: ctx.kind,
            id: own.map(|b| b.id.to_string()),
            url: ctx.page_url.clone(),
            title: ctx.title.clone(),
            doc_type: own.and_then(|b| b.field_str("doc_type")).map(String::from),
            lifecycle: own.and_then(|b| b.field_str("lifecycle")).map(String::from),
            permalink: own.map(|b| site.go_page(&b.id)),
        },
        ids,
        blocks,
        decisions,
        links: LinksJson {
            prev,
            next,
            parent,
            search: "search-index.json",
            index: "llms.txt",
        },
    }
}

/// A text as one line: runs of whitespace become one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of a piece of the renderer's own HTML (the escaped text `text_html` writes): tags
/// dropped, the five entities `escape` writes decoded.
fn plain(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            // `escape` turns every `<` of the author's text into `&lt;`, so a bare one opens a tag
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let decoded = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    one_line(&decoded)
}

/// A title as link text: nothing in it can close the link or start a destination.
fn link_text(title: &str) -> String {
    let spaced: String = title
        .chars()
        .map(|c| match c {
            '[' | ']' | '(' | ')' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    spaced.trim().to_string()
}

/// What a page is about, in a line: a folder's description, a doc's first prose.
fn blurb(p: &PageContext, site: &Site) -> Option<String> {
    let text = match p.kind {
        "folder" => site.description_words(p.body.folder.as_ref()?.description.as_deref()?),
        "doc" => {
            // the doc's first prose is its lede when it opens the doc, else the first prose block
            let html = p.body.doc.as_ref()?.lede.as_deref().or_else(|| {
                p.items
                    .iter()
                    .find(|i| i.block_type == "prose")
                    .map(|i| i.html.as_str())
            })?;
            plain(html)
        }
        _ => return None,
    };
    let cut: String = text.chars().take(BLURB_MAX).collect();
    let cut = cut.trim_end().to_string();
    (!cut.is_empty()).then_some(cut)
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The text of `llms.txt`: `pages` are in tree order.
fn index_text(
    site: &Site,
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version_text: &str,
) -> String {
    let description = description
        .map(one_line)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_string());
    let mut t = format!(
        "# {}\n\n> {description}\n\nVersion: {}. Every page listed below has a JSON twin at the same path with `.json` in place of `.html`; the twin lists the page's block ids, anchors, governing decisions and links. A block's stable permalink is `go/<id>.html`.\n\n## Pages\n\n",
        one_line(site_name),
        one_line(version_text),
    );
    for p in pages.iter().filter(|p| matches!(p.kind, "doc" | "folder")) {
        // a folder's page is `<folder>/index.html`, one level above the pages it holds
        let depth = p.page_url.matches('/').count() - usize::from(p.kind == "folder");
        t.push_str(&"  ".repeat(depth));
        t.push_str(&format!("- [{}]({})", link_text(&p.title), p.page_url));
        if let Some(b) = blurb(p, site) {
            t.push_str(": ");
            t.push_str(&b);
        }
        t.push('\n');
    }
    t.push_str("\n## Reference\n\n");
    for p in pages {
        match p.kind {
            "glossary" => {
                let n = p.body.glossary.as_ref().map_or(0, |g| g.terms.len());
                t.push_str(&format!(
                    "- [Glossary]({}): {}\n",
                    p.page_url,
                    count(n, "term", "terms")
                ));
            }
            "questions" => {
                let n = p.body.questions.as_ref().map_or(0, |q| {
                    q.groups.iter().map(|g| g.questions.len()).sum::<usize>()
                });
                t.push_str(&format!(
                    "- [Open questions]({}): {}\n",
                    p.page_url,
                    count(n, "question", "questions")
                ));
            }
            _ => {}
        }
    }
    t.push_str("- [Search index](search-index.json)\n");
    t
}

/// Writes `llms.txt` under `out`.
pub fn write_index(
    site: &Site,
    out: &Path,
    pages: &[PageContext],
    site_name: &str,
    description: Option<&str>,
    version_text: &str,
) -> Result<(), RdyError> {
    std::fs::write(
        out.join("llms.txt"),
        index_text(site, pages, site_name, description, version_text),
    )
    .map_err(|e| {
        RdyError::new(
            E_IO,
            format!("cannot write llms.txt: {e}"),
            "check the output directory",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_tags_and_decodes_what_escape_wrote() {
        assert_eq!(
            plain("A <a href=\"x\" title=\"t\">link</a> &amp; &lt;b&gt; it&#39;s<br>fine"),
            "A link & <b> it's fine"
        );
    }

    #[test]
    fn a_link_text_cannot_close_the_link_or_open_a_destination() {
        assert_eq!(link_text("a](javascript:x)\nb"), "a  javascript:x  b");
    }

    #[test]
    fn counts_are_singular_for_one() {
        assert_eq!(count(1, "term", "terms"), "1 term");
        assert_eq!(count(0, "term", "terms"), "0 terms");
        assert_eq!(count(2, "term", "terms"), "2 terms");
    }
}
