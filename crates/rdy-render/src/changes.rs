//! The change summary pages (sr6; D-W40KZ, D-BMX8P, RULING F6), and the word diff of each
//! changed block that a review page carries for Show diff.
//!
//! Two pages, at the root of a scope's site (O1, D-ZPAVR C3):
//! - `changes.html` for a review render (a view or a proposal version): what it changes against
//!   its base;
//! - `changes-since.html` when the version position has a tag and landings since it: what those
//!   landings changed.
//!
//! The rules (closed):
//! 1. **The entries** are exactly the blocks [`Review::changed_blocks`] counts (O2), so the
//!    chip's `+N` and the page agree. Each takes its word and its order from the engine's block
//!    diff ([`rdy_engine::Diff`]): added is New, removed is Removed, updated or moved is Edited.
//!    A counted block the block diff has no entry for (a block carried into another doc with its
//!    parent, or every block of a proposal version made on the empty project, which no scope
//!    names) is read from the two sides: on both it is Edited, only in `to` New, only in `from`
//!    Removed. Those follow the others, in `to`'s reading order, then `from`'s.
//! 2. **The doc list** is the rows of the home page's list ([`home::changes`], O12), each linked
//!    to its section on this page.
//! 3. **The sections.** One per row, in row order. An entry goes in the section of the doc that
//!    holds it (in `to`, else in `from`) when that doc has a row; else, for a block gone from
//!    `to`, in the section of the nearest block above it in `from` that has a row (a removed
//!    folder holds its docs); else in a section of its own doc, or of itself for a folder, after
//!    the rows' sections, in the order of their first entries.
//! 4. **The text** of a block (O4) is what a reader reads in it, as the search index reads it
//!    ([`own_text`]), resolved against the `to` side's site for both sides; a section's title
//!    leads its text, and a doc or folder reads as its title, then its description. A `code`
//!    block diffs by lines. A new block is one added piece, a removed one one removed piece.
//! 5. **The marks** (RULING F6): a removed piece is `<del>` and an added one `<ins>`, each with
//!    a visually hidden word (`removed: `, `added: `) for a screen reader, and each entry shows
//!    its Edited, New or Removed word. Colour is never the only signal.
use crate::escape;
use crate::home::{self, ChangeRow};
use crate::review::Review;
use crate::search::own_text;
use crate::site::{Site, rel, reserved_ids, unique};
use crate::version::{self, ReviewedKind};
use rdy_engine::worddiff::{self, Op, Piece, Unit};
use rdy_engine::{Diff, DiffChange, Materialized};
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Which summary a page is.
#[derive(Clone, Debug)]
pub enum Of {
    /// A view or a proposal version against its base.
    Review(ReviewedKind),
    /// The landings since this tag.
    Since(String),
}

/// One row of the doc list.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub title: String,
    /// The section on this page the row leads to.
    pub anchor: String,
    /// `edited`, `new` or `removed`.
    pub mark: &'static str,
    pub word: &'static str,
    /// For an edited doc: `2 edited · 1 new`, shown in place of the word.
    pub counts: Option<String>,
}

/// One changed block.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    /// The block's id, in an attribute only (never in visible text).
    pub id: String,
    /// `edited`, `new` or `removed`.
    pub mark: &'static str,
    /// The word a reader sees: `Edited`, `New` or `Removed`.
    pub word: &'static str,
    /// The nearest section's title, or `Title` for a doc or folder.
    pub place: String,
    /// The block in place on its page, when it is in this site.
    pub href: Option<String>,
    /// The word diff: `<p class="chg-diff" data-rdy="content">…</p>`.
    pub diff_html: String,
}

/// The entries of one doc (or folder) under its title.
#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub anchor: String,
    pub title: String,
    /// The doc's page, when it is in this site.
    pub href: Option<String>,
    pub entries: Vec<Entry>,
}

/// A change summary page's own content.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub heading: String,
    pub sentence: String,
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
}

/// The two sides a summary compares.
pub struct Sides<'a> {
    pub from: &'a Materialized,
    pub to: &'a Materialized,
    /// The `to` side's site: both sides' text reads against it (O4).
    pub text: &'a Site<'a>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Edited,
    New,
    Removed,
}

impl Change {
    fn mark(self) -> &'static str {
        match self {
            Change::Edited => "edited",
            Change::New => "new",
            Change::Removed => "removed",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Change::Edited => "Edited",
            Change::New => "New",
            Change::Removed => "Removed",
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The summary of `rv` for the page `page` of `site`, and the word diff of each entry by block
/// id (a review page carries an edited block's diff for Show diff). `diff` is the engine's block
/// diff of the same two sides, when a scope names the `from` side.
pub fn summary(
    site: &Site,
    page: &str,
    rv: &Review,
    diff: Option<&Diff>,
    sides: &Sides,
    of: &Of,
) -> (Summary, BTreeMap<BlockId, String>) {
    let changed = rv.changed_set();
    // rule 1: the engine's entries that count, in its order, then the counted blocks it lacks
    let mut order: Vec<(BlockId, Change)> = vec![];
    let mut have: BTreeSet<&BlockId> = BTreeSet::new();
    for e in diff.map(|d| d.entries.as_slice()).unwrap_or_default() {
        if changed.contains(&e.id) && have.insert(&e.id) {
            let change = match e.change {
                DiffChange::Added => Change::New,
                DiffChange::Removed => Change::Removed,
                DiffChange::Updated | DiffChange::Moved => Change::Edited,
            };
            order.push((e.id.clone(), change));
        }
    }
    let missing: Vec<&BlockId> = changed.iter().filter(|id| !have.contains(id)).collect();
    if !missing.is_empty() {
        let (to_at, from_at) = (positions(sides.to), positions(sides.from));
        let mut rest: Vec<((u8, usize), &BlockId, Change)> = missing
            .into_iter()
            .filter_map(|id| {
                let change = match (sides.to.get(id), sides.from.get(id)) {
                    (Some(_), Some(_)) => Change::Edited,
                    (Some(_), None) => Change::New,
                    (None, Some(_)) => Change::Removed,
                    (None, None) => return None,
                };
                let key = match to_at.get(id) {
                    Some(&i) => (0, i),
                    None => (1, from_at.get(id).copied().unwrap_or(usize::MAX)),
                };
                Some((key, id, change))
            })
            .collect();
        rest.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        order.extend(rest.into_iter().map(|(_, id, c)| (id.clone(), c)));
    }

    // rule 2: the rows, each with its section's anchor
    let rows: Vec<ChangeRow> = home::changes(site, rv);
    let mut taken = reserved_ids();
    // the Show changes button's id
    taken.insert("show-changes".into());
    let mut groups: Vec<Group> = vec![];
    let mut group_of: BTreeMap<BlockId, usize> = BTreeMap::new();
    for row in &rows {
        group_of.insert(row.id.clone(), groups.len());
        groups.push(Group {
            anchor: unique(row.title.clone(), &mut taken),
            title: row.title.clone(),
            href: row.href.clone(),
            entries: vec![],
        });
    }
    let out_rows: Vec<Row> = rows
        .iter()
        .zip(&groups)
        .map(|(r, g)| Row {
            title: r.title.clone(),
            anchor: g.anchor.clone(),
            mark: r.mark,
            word: r.word,
            counts: r.counts.clone(),
        })
        .collect();

    // rules 3 to 5: each entry in its section
    let mut extra: Vec<(BlockId, Vec<Entry>)> = vec![];
    let mut inline: BTreeMap<BlockId, String> = BTreeMap::new();
    for (id, change) in order {
        let (old, new) = (sides.from.get(&id), sides.to.get(&id));
        let Some(b) = new.or(old) else { continue };
        let html = diff_html(&pieces(old, new, sides.text));
        inline.insert(id.clone(), html.clone());
        let entry = Entry {
            id: id.to_string(),
            mark: change.mark(),
            word: change.word(),
            place: place(b, sides),
            href: site.url_of(&id).map(|u| u.href_from(page)),
            diff_html: html,
        };
        match home_group(&id, sides, &group_of) {
            Some(g) => groups[g].entries.push(entry),
            None => {
                let key = sides
                    .to
                    .doc_of(&id)
                    .or_else(|| sides.from.doc_of(&id))
                    .map_or_else(|| id.clone(), |d| d.id.clone());
                match extra.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, entries)) => entries.push(entry),
                    None => extra.push((key, vec![entry])),
                }
            }
        }
    }
    for (key, entries) in extra {
        let title = sides
            .to
            .get(&key)
            .or_else(|| sides.from.get(&key))
            .map(|b| page_title(b, sides.text))
            .unwrap_or_default();
        groups.push(Group {
            anchor: unique(title.clone(), &mut taken),
            title,
            href: site.page(&key).map(|p| rel(page, &p)),
            entries,
        });
    }

    let n: usize = groups.iter().map(|g| g.entries.len()).sum();
    let d = out_rows.len();
    let (heading, sentence) = match of {
        Of::Review(kind) => (
            version::changes_heading(*kind).to_string(),
            if n == 0 {
                match kind {
                    ReviewedKind::View => "This view changes nothing yet.".to_string(),
                    ReviewedKind::Proposal => "This proposal changes nothing yet.".to_string(),
                }
            } else {
                format!(
                    "{} in {}, not landed.",
                    plural(n, "change", "changes"),
                    plural(d, "doc", "docs")
                )
            },
        ),
        Of::Since(tag) => (
            format!("Changes since {tag}"),
            format!(
                "{} in {} since {tag}.",
                plural(n, "change", "changes"),
                plural(d, "doc", "docs")
            ),
        ),
    };
    (
        Summary {
            heading,
            sentence,
            rows: out_rows,
            groups,
        },
        inline,
    )
}

/// The row section an entry goes in (rule 3), if any.
fn home_group(id: &BlockId, sides: &Sides, group_of: &BTreeMap<BlockId, usize>) -> Option<usize> {
    let doc = sides
        .to
        .doc_of(id)
        .or_else(|| sides.from.doc_of(id))
        .and_then(|d| group_of.get(&d.id));
    if doc.is_some() || sides.to.get(id).is_some() {
        return doc.copied();
    }
    // a block gone from `to`: the nearest block above it in `from` with a row
    let mut cur = sides.from.get(id);
    for _ in 0..=sides.from.len() {
        let b = cur?;
        if let Some(g) = group_of.get(&b.id) {
            return Some(*g);
        }
        cur = b.parent.as_ref().and_then(|p| sides.from.get(p));
    }
    None
}

/// Each block's place in the depth-first reading order of `m`.
fn positions(m: &Materialized) -> BTreeMap<BlockId, usize> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<&Block> = m.children(None).into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        if out.contains_key(&b.id) {
            continue;
        }
        out.insert(b.id.clone(), out.len());
        stack.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    out
}

/// A field's text as a reader reads it, when it has any.
fn field_words(b: &Block, name: &str, site: &Site) -> Option<String> {
    b.field_str(name)
        .filter(|t| !t.trim().is_empty())
        .map(|t| site.words(t))
}

/// A doc's or folder's title as a reader reads it, from the block itself: the site's label
/// cache is keyed by id, and both sides share one. With no title, the site's label.
fn page_title(b: &Block, site: &Site) -> String {
    field_words(b, "title", site).unwrap_or_else(|| site.label(b))
}

/// What a reader reads in a block (rule 4).
fn text(b: &Block, site: &Site) -> String {
    if tree::is_tree_type(&b.block_type) {
        let mut out = page_title(b, site);
        if let Some(d) = b.field_str("description").filter(|d| !d.trim().is_empty()) {
            out.push('\n');
            out.push_str(&site.description_words(d));
        }
        return out;
    }
    let own = own_text(b, site);
    match field_words(b, "title", site) {
        Some(t) if b.block_type == "section" && own.trim().is_empty() => t,
        Some(t) if b.block_type == "section" => format!("{t}\n{own}"),
        _ => own,
    }
}

fn unit(b: &Block) -> Unit {
    if b.block_type == "code" {
        Unit::Lines
    } else {
        Unit::Words
    }
}

/// The word pieces of a block from `old` to `new` (rule 4).
fn pieces(old: Option<&Block>, new: Option<&Block>, site: &Site) -> Vec<Piece> {
    match (old, new) {
        (Some(o), Some(n)) => worddiff::diff(&text(o, site), &text(n, site), unit(n)),
        (None, Some(n)) => worddiff::diff("", &text(n, site), unit(n)),
        (Some(o), None) => worddiff::diff(&text(o, site), "", unit(o)),
        (None, None) => vec![],
    }
}

/// The pieces as the entry's paragraph (rule 5).
pub fn diff_html(pieces: &[Piece]) -> String {
    let mut out = String::from("<p class=\"chg-diff\" data-rdy=\"content\">");
    for p in pieces {
        let text = escape(&p.text);
        match p.op {
            Op::Same => out.push_str(&text),
            Op::Del => out.push_str(&format!(
                "<del><span class=\"sr-word\">removed: </span>{text}</del>"
            )),
            Op::Add => out.push_str(&format!(
                "<ins><span class=\"sr-word\">added: </span>{text}</ins>"
            )),
        }
    }
    out.push_str("</p>");
    out
}

/// Where a block is (rule 5's entry head): `Title` for a doc or folder, else its nearest
/// section's title (itself for a section), else its doc's title. Each block on the way is read
/// as `to` holds it, when it does.
fn place(b: &Block, sides: &Sides) -> String {
    if tree::is_tree_type(&b.block_type) {
        return "Title".into();
    }
    let side = |id: &BlockId| sides.to.get(id).or_else(|| sides.from.get(id));
    let mut cur = side(&b.id);
    for _ in 0..=sides.from.len() + sides.to.len() {
        let Some(c) = cur else { break };
        if c.block_type == "section" || c.block_type == tree::DOC {
            return field_words(c, "title", sides.text).unwrap_or_else(|| {
                if c.block_type == tree::DOC {
                    page_title(c, sides.text)
                } else {
                    "Untitled".into()
                }
            });
        }
        cur = c.parent.as_ref().and_then(side);
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr6_pieces_are_marked_with_their_words() {
        let pieces = worddiff::diff("Old <b> wording.", "New <b> wording.", Unit::Words);
        assert_eq!(
            diff_html(&pieces),
            "<p class=\"chg-diff\" data-rdy=\"content\"><del><span class=\"sr-word\">removed: </span>Old</del><ins><span class=\"sr-word\">added: </span>New</ins> &lt;b&gt; wording.</p>"
        );
        assert_eq!(
            diff_html(&[]),
            "<p class=\"chg-diff\" data-rdy=\"content\"></p>"
        );
    }
}
