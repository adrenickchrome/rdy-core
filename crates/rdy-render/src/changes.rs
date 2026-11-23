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
//! 2. **The doc list.** For a view or proposal, the rows of the home page's list
//!    ([`home::changes`], O12). For the since page, the docs of its entries, read from the two
//!    states it compares and never from the home page's rows: each doc that holds an entry (in
//!    `to`, else in `from`), once, in the order of its first entry, so in the block diff's
//!    order. A doc `to` holds reads as the home page builds its row; a doc only `from` held is
//!    Removed, titled as `from` held it, so a removed folder keeps a row for each of its docs.
//!    A folder is not a doc and has no row; its own entry takes a section of its own (rule 3).
//!    A doc links to its page only when the rendered site has it. Each row links to its
//!    section on this page.
//! 3. **The sections.** One per row, in row order. An entry goes in the section of the doc that
//!    holds it (in `to`, else in `from`) when that doc has a row; else, for a block gone from
//!    `to`, in the section of the nearest block above it in `from` that has a row (a removed
//!    folder holds its docs); else in a section of its own doc, or of itself for a folder, after
//!    the rows' sections, in the order of their first entries.
//! 4. **The text** of a block (O4) is what a reader reads in it, as the search index reads it
//!    ([`own_text_coded`]), resolved against the `to` side's site for both sides; a section's
//!    title leads its text, and a doc or folder reads as its title, then its description. A doc
//!    or folder with no title reads as its name, each side as that side names it (never from
//!    the site's label cache, which is keyed by id). A `code` block diffs by lines. A new block
//!    is one added piece, a removed one one removed piece.
//! 5. **The marks** (RULING F6): a removed piece is `<del>` and an added one `<ins>`, each with
//!    a visually hidden word (`removed: `, `added: `) for a screen reader, and each entry shows
//!    its Edited, New or Removed word. Colour is never the only signal.
//! 6. **The element** (the task-6 ruling): a diff keeps its block's kind of text. A `code`
//!    block's diff is `<pre class="code chg-diff"><code>`, with the language class its page
//!    uses, and the marks inside it. Any other block's is `<p class="chg-diff">`, where each
//!    code span stays a `<code>` element, inside the mark that changes it. A block whose kind
//!    of text changed (a prose made a code block, or the reverse) shows its old text whole as
//!    removed in its old element, then its new text whole as added in its new element. So the
//!    leak scan's code exemption holds for a diff as for its page, and no diff text needs
//!    another.
//! 7. **The coded text.** The text of rule 4 marks each code span with private-use characters,
//!    and keeps each authored one after an escape ([`crate::inline::plain_refs_coded`]). Each
//!    side's text is read back in order, across the pieces, before any HTML is written: no mark
//!    reaches a page, and no authored character is lost.
//! 8. **A type change** (the task-4 ruling, 2) is said in words before the text's diff:
//!    `<p class="chg-type">Type: prose → note</p>`, from the engine's entry (`type`), or, for a
//!    block the block diff has no entry for, from the two sides. Each type name reads with `_`
//!    as a space. It is the block's structure, never a custom field (a field named `type` is
//!    not read here), and the entry stays one Edited entry, moved or not: it counts once.
//! 9. **A table** (D-AXK27): the diff of an edited table is a table, `<div class="table-wrap
//!    chg-diff" data-rdy="content"><table>`, with the new table's columns and rows and the old
//!    table's removed ones in their old places. Columns pair by header text (the k-th column of a
//!    header text with the k-th of it in the other table); rows by a first cell unique in both
//!    tables; each removed column or row stands after the one before it in the old table. Each
//!    cell is the word diff of its old and new text, so the diff's cost cap holds per cell: a
//!    cell of an added row or column is all added, of a removed one all removed, each with the
//!    marks of rule 5. A new table is all added, a removed one all removed, as tables; a table
//!    made another block type (or the reverse) shows each side whole in its own element (rule 6).
use crate::escape;
use crate::home::{self, ChangeRow};
use crate::inline::{Form, code_escape};
use crate::review::{DocChange, Mark, Review};
use crate::search::own_text_coded;
use crate::site::{Site, name_as_title, rel, reserved_ids, unique};
use crate::version::{self, ReviewedKind};
use rdy_engine::worddiff::{self, Op, Piece, Unit};
use rdy_engine::{Diff, DiffChange, Materialized};
use rdy_model::{Block, BlockId, RefMode, tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Which summary a page is.
#[derive(Clone, Debug)]
pub enum Of {
    /// A view or a proposal version against its base.
    Review(ReviewedKind),
    /// The landings since a tag: the tag's name and how many landings (the since chip's count).
    Since { tag: String, landings: u32 },
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
    /// The word diff: `<p class="chg-diff" data-rdy="content">…</p>`, or for a code block
    /// `<pre class="code chg-diff" data-rdy="content"><code>…</code></pre>` (rule 6), or for a
    /// table a table (rule 9), after the type change in words when the type changed (rule 8).
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
    /// A named profile's `from` side site (D-CWC91): the text only `from` holds reads its
    /// references through that side's own selected state, never through the full source. None
    /// for the source, whose sides both read against `text`.
    pub from_text: Option<&'a Site<'a>>,
}

impl<'a> Sides<'a> {
    /// The site a block of side `m` reads its text against: `from_text` for the `from` side when
    /// there is one, else `text` (O4).
    fn site_of(&self, m: &Materialized) -> &'a Site<'a> {
        match self.from_text {
            Some(old) if std::ptr::eq(m, self.from) => old,
            _ => self.text,
        }
    }
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

/// The since page's sentence (F7): the landings the chip counts, then the blocks and docs they
/// changed: `3 landings since v1.0.0 changed 4 blocks in 2 docs.`, or with no changed block
/// `… changed no blocks.`
fn since_sentence(landings: u32, blocks: usize, docs: usize, tag: &str) -> String {
    let lead = format!(
        "{} since {tag} changed",
        plural(landings as usize, "landing", "landings")
    );
    if blocks == 0 {
        format!("{lead} no blocks.")
    } else {
        format!(
            "{lead} {} in {}.",
            plural(blocks, "block", "blocks"),
            plural(docs, "doc", "docs")
        )
    }
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
    // rule 1: the engine's entries that count, in its order, then the counted blocks it lacks;
    // rule 8: each with its type change, from the engine's entry
    let mut order: Vec<(BlockId, Change)> = vec![];
    let mut types: BTreeMap<BlockId, (String, String)> = BTreeMap::new();
    let mut have: BTreeSet<&BlockId> = BTreeSet::new();
    for e in diff.map(|d| d.entries.as_slice()).unwrap_or_default() {
        if changed.contains(&e.id) && have.insert(&e.id) {
            let change = match e.change {
                DiffChange::Added => Change::New,
                DiffChange::Removed => Change::Removed,
                DiffChange::Updated | DiffChange::Moved => Change::Edited,
            };
            if let Some(t) = &e.type_change {
                types.insert(e.id.clone(), (t.from.clone(), t.to.clone()));
            }
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
    let rows: Vec<ChangeRow> = match of {
        Of::Review(_) => home::changes_with(site, rv, sides.from_text),
        Of::Since { .. } => since_rows(site, rv, &order, sides),
    };
    let mut taken = reserved_ids();
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
        // rule 8: the engine's type change, else (no entry for the block) the two sides'
        let retyped = match (types.get(&id), old, new) {
            (Some((from, to)), _, _) => Some((from.as_str(), to.as_str())),
            (None, Some(o), Some(n)) if !have.contains(&id) && o.block_type != n.block_type => {
                Some((o.block_type.as_str(), n.block_type.as_str()))
            }
            _ => None,
        };
        let mut html = retyped
            .map(|(from, to)| type_html(from, to))
            .unwrap_or_default();
        html.push_str(&block_diff(old, new, sides));
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
        let title = side_of(&key, sides)
            .map(|(b, m)| page_title(b, m, sides.site_of(m)))
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
        Of::Since { tag, landings } => (
            format!("Changes since {tag}"),
            since_sentence(*landings, n, d, tag),
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

/// The doc list of the since page (rule 2), from the counted entries and the two states it
/// compares, never from the home page's rows: each doc that holds an entry of `order` (in `to`,
/// else in `from`, as [`home_group`] reads it), once, in the order of its first entry (the block
/// diff's order). A doc `to` holds is a row as the home page builds it (its word, its counts); a
/// doc only `from` held is a Removed row, titled as `from` held it. So a removed folder keeps a
/// Removed row for each of its docs. A doc that only lost a block another doc now holds has no
/// row: that block's entry is in the doc that holds it. A folder is not a doc and has no row. A
/// row links to its doc's page only when the rendered site `site` has it.
fn since_rows(
    site: &Site,
    rv: &Review,
    order: &[(BlockId, Change)],
    sides: &Sides,
) -> Vec<ChangeRow> {
    let mut rows: Vec<ChangeRow> = vec![];
    let mut seen: BTreeSet<&BlockId> = BTreeSet::new();
    for (id, _) in order {
        let Some(doc) = sides.to.doc_of(id).or_else(|| sides.from.doc_of(id)) else {
            continue;
        };
        if !seen.insert(&doc.id) {
            continue;
        }
        let mut row = match sides.to.get(&doc.id).filter(|b| b.block_type == tree::DOC) {
            Some(b) => {
                let edited = DocChange {
                    mark: Mark::Edited,
                    edited: 0,
                    new: 0,
                    removed: 0,
                };
                home::doc_row(sides.text, b, rv.doc(&b.id).unwrap_or(&edited))
            }
            None => home::removed_row(doc, page_title(doc, sides.from, sides.site_of(sides.from))),
        };
        row.href = site.page(&doc.id).map(|p| rel("index.html", &p));
        rows.push(row);
    }
    rows
}

/// A block as `to` holds it, else as `from` held it, with the side that holds it.
fn side_of<'m>(id: &BlockId, sides: &Sides<'m>) -> Option<(&'m Block, &'m Materialized)> {
    sides
        .to
        .get(id)
        .map(|b| (b, sides.to))
        .or_else(|| sides.from.get(id).map(|b| (b, sides.from)))
}

/// Each block's place in the depth-first reading order of `m`.
fn positions(m: &Materialized) -> BTreeMap<BlockId, usize> {
    rdy_engine::reader::shape::reading_positions(m)
}

/// A field's text as a reader reads it, when it has any: as a label, or, `coded`, with each code
/// span between the code marks (rule 6).
fn field_words(b: &Block, name: &str, site: &Site, coded: bool) -> Option<String> {
    b.field_str(name)
        .filter(|t| !t.trim().is_empty())
        .map(|t| match coded {
            true => site.plain_coded(t, RefMode::Strict, Form::Label, &|_| {
                crate::html::UNRESOLVED.to_string()
            }),
            false => site.words(t),
        })
}

/// A doc's or folder's title as a reader reads it, from the block itself and `m`, the side that
/// holds it (rule 4): its title, else its name as words, as [`Site::label`] reads a page. The
/// site's label cache is keyed by id and both sides share one, so it is never read here.
fn title_of(b: &Block, m: &Materialized, site: &Site, coded: bool) -> String {
    // a removed doc or folder is called by the notice, never by its neutral id name
    if b.is_redacted() {
        return rdy_model::redaction::CONTENT_REMOVED.to_string();
    }
    if let Some(t) = field_words(b, "title", site, coded) {
        return t;
    }
    let title = m
        .name_of(&b.id)
        .or_else(|| b.field_str("name").map(String::from))
        .map(|n| name_as_title(&n, &b.id))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled".into());
    if coded { code_escape(&title) } else { title }
}

/// A doc's or folder's title in plain words: a section's heading on this page.
fn page_title(b: &Block, m: &Materialized, site: &Site) -> String {
    title_of(b, m, site, false)
}

/// What a reader reads in a block (rule 4), each code span between the code marks (rule 6).
/// `m` is the side that holds `b`.
fn text(b: &Block, m: &Materialized, site: &Site) -> String {
    if tree::is_tree_type(&b.block_type) {
        let mut out = title_of(b, m, site, true);
        if let Some(d) = b.field_str("description").filter(|d| !d.trim().is_empty()) {
            let words = site.plain_coded(d, RefMode::Strict, Form::Spans, &|_| String::new());
            out.push('\n');
            out.push_str(&words.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        return out;
    }
    let own = own_text_coded(b, site);
    match field_words(b, "title", site, true) {
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

/// The diff of a block from `old` (in `from`) to `new` (in `to`) (rules 4, 6 and 9): a word diff in
/// the block's element when both sides have the same kind of text, else each side whole in its
/// own element. A table's diff, or a new or removed table's, is a table (rule 9).
fn block_diff(old: Option<&Block>, new: Option<&Block>, sides: &Sides) -> String {
    let old_site = sides.site_of(sides.from);
    match (old.filter(|o| is_table(o)), new.filter(|n| is_table(n))) {
        (Some(o), Some(n)) => return table_diff(Some((o, old_site)), Some((n, sides.text))),
        (Some(o), None) if new.is_none() => return table_diff(Some((o, old_site)), None),
        (None, Some(n)) if old.is_none() => return table_diff(None, Some((n, sides.text))),
        _ => {}
    }
    let old = old.map(|o| (text(o, sides.from, old_site), wrap_of(o), o));
    let new = new.map(|n| (text(n, sides.to, sides.text), wrap_of(n), n));
    match (old, new) {
        (Some((ot, ow, o)), Some((nt, nw, n)))
            if ow.same_kind(&nw) && !is_table(o) && !is_table(n) =>
        {
            diff_html(&worddiff::diff(&ot, &nt, unit(n)), &nw)
        }
        (Some((ot, ow, o)), Some((nt, nw, n))) => {
            let mut out = String::new();
            if is_table(o) {
                out.push_str(&table_diff(Some((o, old_site)), None));
            } else if !ot.is_empty() {
                out.push_str(&diff_html(&worddiff::diff(&ot, "", Unit::Lines), &ow));
            }
            if is_table(n) {
                out.push_str(&table_diff(None, Some((n, sides.text))));
            } else if !nt.is_empty() || ot.is_empty() {
                out.push_str(&diff_html(&worddiff::diff("", &nt, Unit::Lines), &nw));
            }
            out
        }
        (None, Some((nt, nw, n))) => diff_html(&worddiff::diff("", &nt, unit(n)), &nw),
        (Some((ot, ow, o)), None) => diff_html(&worddiff::diff(&ot, "", unit(o)), &ow),
        (None, None) => String::new(),
    }
}

/// Whether `b`'s diff is a table (rule 9): a table that is not removed (a removed block reads as
/// the notice).
fn is_table(b: &Block) -> bool {
    b.block_type == "table" && !b.is_redacted()
}

/// A table's cells as a reader reads them (rule 9), each coded (rule 7): its header texts, and
/// each row's cells. A row that is not a list is one cell. A cell reads as the search index reads
/// it ([`own_text_coded`]).
#[derive(Default)]
struct Grid {
    head: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Grid {
    fn of(b: &Block, site: &Site) -> Grid {
        use rdy_model::FieldValue;
        let cell = |v: &FieldValue| match v {
            FieldValue::Str(s) => {
                site.plain_coded(s, RefMode::Strict, Form::Spans, &|_| String::new())
            }
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::Int(i) => i.to_string(),
            _ => String::new(),
        };
        let list = |name: &str| match b.fields.get(name) {
            Some(FieldValue::List(l)) => l.as_slice(),
            _ => &[],
        };
        let head = list("columns").iter().map(cell).collect();
        let rows = list("rows")
            .iter()
            .map(|r| match r {
                FieldValue::List(cells) => cells.iter().map(cell).collect(),
                other => vec![cell(other)],
            })
            .collect();
        Grid { head, rows }
    }

    /// How many columns the table has: its headers, or its widest row.
    fn width(&self) -> usize {
        self.rows
            .iter()
            .map(Vec::len)
            .chain([self.head.len()])
            .max()
            .unwrap_or(0)
    }
}

/// The places of a table diff's columns or rows (rule 9): each new one in its order, paired with
/// its old one, if any (`pair`, new to old), and each old one with no pair after the one that
/// came before it in the old table (before them all when it was first).
fn places(old: usize, pair: &[Option<usize>]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut paired = vec![false; old];
    for &o in pair.iter().flatten() {
        paired[o] = true;
    }
    // each unpaired old one under the nearest paired old one before it (None: the start)
    let mut after: BTreeMap<Option<usize>, Vec<usize>> = BTreeMap::new();
    let mut anchor = None;
    for (o, &p) in paired.iter().enumerate() {
        if p {
            anchor = Some(o);
        } else {
            after.entry(anchor).or_default().push(o);
        }
    }
    let mut out: Vec<(Option<usize>, Option<usize>)> = vec![];
    let removed = |out: &mut Vec<_>, at: Option<usize>| {
        out.extend(
            after
                .get(&at)
                .into_iter()
                .flatten()
                .map(|&o| (Some(o), None)),
        );
    };
    removed(&mut out, None);
    for (n, &o) in pair.iter().enumerate() {
        out.push((o, Some(n)));
        if o.is_some() {
            removed(&mut out, o);
        }
    }
    out
}

/// The diff of a table (rule 9), as a table: `old` and `new` each with the site its text reads
/// against. Columns pair by header text, each one with the same text in the other table in turn;
/// rows by a first cell that is unique in both tables. Each cell is its own word diff, so the
/// diff's cost cap holds for each cell on its own. A new table's cells are all added, and a
/// removed table's all removed.
fn table_diff(old: Option<(&Block, &Site)>, new: Option<(&Block, &Site)>) -> String {
    let og = old.map(|(b, s)| Grid::of(b, s)).unwrap_or_default();
    let ng = new.map(|(b, s)| Grid::of(b, s)).unwrap_or_default();
    let (ow, nw) = (og.width(), ng.width());
    let header = |g: &Grid, i: usize| g.head.get(i).map_or("", String::as_str).to_string();
    // columns: the k-th column of a header text in the new table pairs with the k-th of it in the
    // old one
    let mut free: BTreeMap<String, std::collections::VecDeque<usize>> = BTreeMap::new();
    for i in 0..ow {
        free.entry(header(&og, i)).or_default().push_back(i);
    }
    let col_pair: Vec<Option<usize>> = (0..nw)
        .map(|i| free.get_mut(&header(&ng, i)).and_then(|q| q.pop_front()))
        .collect();
    let cols = places(ow, &col_pair);
    // rows: a first cell that is unique in both tables
    let first = |r: &Vec<String>| r.first().cloned().unwrap_or_default();
    let counts = |g: &Grid| {
        let mut c: BTreeMap<String, usize> = BTreeMap::new();
        for r in &g.rows {
            *c.entry(first(r)).or_default() += 1;
        }
        c
    };
    let (oc, nc) = (counts(&og), counts(&ng));
    let old_at: BTreeMap<String, usize> = og
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (first(r), i))
        .collect();
    let row_pair: Vec<Option<usize>> = ng
        .rows
        .iter()
        .map(|r| {
            let k = first(r);
            (oc.get(&k) == Some(&1) && nc.get(&k) == Some(&1)).then(|| old_at[&k])
        })
        .collect();
    let rows = places(og.rows.len(), &row_pair);

    let mut out =
        String::from("<div class=\"table-wrap chg-diff\" data-rdy=\"content\"><table><thead><tr>");
    row_html(
        &mut out,
        "th scope=\"col\"",
        "th",
        &cols,
        Some(&og.head),
        Some(&ng.head),
    );
    out.push_str("</tr></thead><tbody>");
    for &(or, nr) in &rows {
        out.push_str("<tr>");
        let (or, nr) = (or.map(|r| &og.rows[r]), nr.map(|r| &ng.rows[r]));
        row_html(&mut out, "td", "td", &cols, or, nr);
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

/// One row of a table diff (rule 9): a cell for each place in `cols` where the old row or the
/// new row has one, the word diff of its old text and its new text. As a table's page writes
/// it, a row ends at its last cell; a run of places inside the row that neither side has is one
/// empty cell that spans them, so each later cell keeps its column. So a row's cells are at most
/// twice the cells its two sides have.
fn row_html(
    out: &mut String,
    open: &str,
    close: &str,
    cols: &[(Option<usize>, Option<usize>)],
    old: Option<&Vec<String>>,
    new: Option<&Vec<String>>,
) {
    let (old, new) = (
        old.map_or(&[][..], Vec::as_slice),
        new.map_or(&[][..], Vec::as_slice),
    );
    // every old (new) column is in `cols` once, so the row ends when each side's cells are seen
    let (mut left_old, mut left_new) = (old.len(), new.len());
    let mut gap = 0;
    for &(oi, ni) in cols {
        if left_old == 0 && left_new == 0 {
            break;
        }
        let o = oi.and_then(|i| old.get(i));
        let n = ni.and_then(|i| new.get(i));
        if o.is_none() && n.is_none() {
            gap += 1;
            continue;
        }
        left_old -= usize::from(o.is_some());
        left_new -= usize::from(n.is_some());
        if gap > 0 {
            out.push_str(&format!("<{open} colspan=\"{gap}\"></{close}>"));
            gap = 0;
        }
        out.push_str(&format!("<{open}>"));
        let (o, n) = (o.map_or("", String::as_str), n.map_or("", String::as_str));
        pieces_html(&worddiff::diff(o, n, Unit::Words), out);
        out.push_str(&format!("</{close}>"));
    }
}

/// The element a diff is written in (rule 6): the block's own kind of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// A paragraph, each code span a `<code>` element.
    Para,
    /// A code block's `<pre><code>`, with its language, if it has one.
    Code(Option<String>),
}

impl Wrap {
    /// Both a paragraph, or both a code block (in any language: the diff takes the new one's).
    fn same_kind(&self, other: &Wrap) -> bool {
        matches!(
            (self, other),
            (Wrap::Para, Wrap::Para) | (Wrap::Code(_), Wrap::Code(_))
        )
    }
}

/// The element `b`'s diff is written in: `<pre><code>` for a code block, with the language its
/// page gives it (`html::block_html`), else a paragraph.
fn wrap_of(b: &Block) -> Wrap {
    if b.block_type == "code" {
        let lang = b.field_str("lang").unwrap_or("").trim();
        Wrap::Code((!lang.is_empty()).then(|| lang.to_string()))
    } else {
        Wrap::Para
    }
}

/// A type change in words (rule 8): `<p class="chg-type">Type: prose → note</p>`, each type
/// name with `_` read as a space.
fn type_html(from: &str, to: &str) -> String {
    let words = |t: &str| escape(&t.replace('_', " "));
    format!(
        "<p class=\"chg-type\">Type: {} \u{2192} {}</p>",
        words(from),
        words(to)
    )
}

const DEL_OPEN: &str = "<del><span class=\"sr-word\">removed: </span>";
const INS_OPEN: &str = "<ins><span class=\"sr-word\">added: </span>";

/// The pieces of a coded text (rule 7) as the entry's diff (rules 5 and 6).
pub fn diff_html(pieces: &[Piece], wrap: &Wrap) -> String {
    let mut out = String::new();
    match wrap {
        Wrap::Code(lang) => {
            let class = lang
                .as_deref()
                .map(|l| format!(" class=\"language-{}\"", escape(l)))
                .unwrap_or_default();
            out.push_str(&format!(
                "<pre class=\"code chg-diff\" data-rdy=\"content\"><code{class}>"
            ));
            pieces_html(pieces, &mut out);
            out.push_str("</code></pre>");
        }
        Wrap::Para => {
            out.push_str("<p class=\"chg-diff\" data-rdy=\"content\">");
            pieces_html(pieces, &mut out);
            out.push_str("</p>");
        }
    }
    out
}

/// One character of a side's text as a reader reads it, and the code span it is in; reads one
/// side's coded text back (rule 7) ([`rdy_engine::reader::inline`]).
pub(crate) use rdy_engine::reader::inline::{Ch, Decoder};

/// Characters as runs, escaped, each run of a code span in a `<code>` element.
pub(crate) fn runs_html(out: &mut String, chars: &[Ch]) {
    for run in chars.chunk_by(|a, b| a.1 == b.1) {
        let text: String = run.iter().map(|r| r.0).collect();
        if run[0].1 == 0 {
            out.push_str(&escape(&text));
        } else {
            out.push_str(&format!("<code>{}</code>", escape(&text)));
        }
    }
}

/// Removed or added characters, with their hidden word; nothing when there are none.
fn marked(out: &mut String, open: &str, close: &str, chars: &[Ch]) {
    if chars.is_empty() {
        return;
    }
    out.push_str(open);
    runs_html(out, chars);
    out.push_str(close);
}

/// A diff's pieces (rules 6 and 7). The old text (`Same` and `Del`) and the new text (`Same` and
/// `Add`) are each read back on their own, so each side keeps its code spans and its authored
/// characters. A `Same` run that is code on one side only is shown as removed in the old form
/// and added in the new, so a change between code and plain words is never hidden; and a `Same`
/// piece the two sides read differently is shown whole as removed, then added.
fn pieces_html(pieces: &[Piece], out: &mut String) {
    let (mut old, mut new) = (Decoder::default(), Decoder::default());
    for p in pieces {
        match p.op {
            Op::Del => marked(out, DEL_OPEN, "</del>", &old.read(&p.text)),
            Op::Add => marked(out, INS_OPEN, "</ins>", &new.read(&p.text)),
            Op::Same => {
                let (o, n) = (old.read(&p.text), new.read(&p.text));
                if !o.iter().map(|r| r.0).eq(n.iter().map(|r| r.0)) {
                    marked(out, DEL_OPEN, "</del>", &o);
                    marked(out, INS_OPEN, "</ins>", &n);
                    continue;
                }
                let both: Vec<(Ch, Ch)> = o.into_iter().zip(n).collect();
                for run in both.chunk_by(|a, b| a.0.1 == b.0.1 && a.1.1 == b.1.1) {
                    let (o, n): (Vec<Ch>, Vec<Ch>) = run.iter().copied().unzip();
                    if (o[0].1 == 0) == (n[0].1 == 0) {
                        runs_html(out, &o);
                    } else {
                        marked(out, DEL_OPEN, "</del>", &o);
                        marked(out, INS_OPEN, "</ins>", &n);
                    }
                }
            }
        }
    }
}

/// Where a block is (rule 5's entry head): `Title` for a doc or folder, else its nearest
/// section's title (itself for a section), else its doc's title. Each block on the way is read
/// as `to` holds it, when it does.
fn place(b: &Block, sides: &Sides) -> String {
    if tree::is_tree_type(&b.block_type) {
        return "Title".into();
    }
    let mut cur = side_of(&b.id, sides);
    for _ in 0..=sides.from.len() + sides.to.len() {
        let Some((c, m)) = cur else { break };
        if c.block_type == "section" {
            return field_words(c, "title", sides.site_of(m), false)
                .unwrap_or_else(|| "Untitled".into());
        }
        if c.block_type == tree::DOC {
            return page_title(c, m, sides.site_of(m));
        }
        cur = c.parent.as_ref().and_then(|p| side_of(p, sides));
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
            diff_html(&pieces, &Wrap::Para),
            "<p class=\"chg-diff\" data-rdy=\"content\"><del><span class=\"sr-word\">removed: </span>Old</del><ins><span class=\"sr-word\">added: </span>New</ins> &lt;b&gt; wording.</p>"
        );
        assert_eq!(
            diff_html(&[], &Wrap::Para),
            "<p class=\"chg-diff\" data-rdy=\"content\"></p>"
        );
    }

    const DEL: &str = DEL_OPEN;
    const INS: &str = INS_OPEN;

    fn coded(t: &str) -> String {
        crate::inline::plain_refs_coded(
            t,
            RefMode::Strict,
            Form::Spans,
            &|_| String::new(),
            &|_| false,
        )
    }

    fn para(old: &str, new: &str) -> String {
        let html = diff_html(
            &worddiff::diff(&coded(old), &coded(new), Unit::Words),
            &Wrap::Para,
        );
        html.strip_prefix("<p class=\"chg-diff\" data-rdy=\"content\">")
            .and_then(|h| h.strip_suffix("</p>"))
            .expect("a paragraph")
            .to_string()
    }

    /// Fix round 2 (finding 3): an authored mark character inside a code span is read back as
    /// text in that span; the escape character itself is read back too.
    #[test]
    fn sr6_authored_mark_characters_are_read_back() {
        assert_eq!(
            para("x `a\u{E000}b` y", "x `a\u{E001}b` y"),
            format!(
                "x <code>a</code>{DEL}<code>\u{E000}</code></del>{INS}<code>\u{E001}</code></ins><code>b</code> y"
            )
        );
        assert_eq!(
            para("a\u{E002}b", "a\u{E002}c"),
            format!("a\u{E002}{DEL}b</del>{INS}c</ins>")
        );
        let code = diff_html(
            &worddiff::diff(
                &code_escape("a\u{E000}"),
                &code_escape("a\u{E001}"),
                Unit::Lines,
            ),
            &Wrap::Code(None),
        );
        assert_eq!(
            code,
            format!(
                "<pre class=\"code chg-diff\" data-rdy=\"content\"><code>{DEL}a\u{E000}</del>{INS}a\u{E001}</ins></code></pre>"
            )
        );
    }

    /// F7 (visual review, founder card 2026-10-15): the since page names the landings its chip
    /// counts, then the blocks and docs they changed.
    #[test]
    fn sr17_the_since_sentence_counts_landings_then_blocks_and_docs() {
        assert_eq!(
            since_sentence(3, 1, 1, "v1.0.0"),
            "3 landings since v1.0.0 changed 1 block in 1 doc."
        );
        assert_eq!(
            since_sentence(1, 5, 2, "v1.0.0"),
            "1 landing since v1.0.0 changed 5 blocks in 2 docs."
        );
        assert_eq!(
            since_sentence(2, 0, 0, "v0.4.0"),
            "2 landings since v0.4.0 changed no blocks."
        );
    }

    /// Text that leaves a code span shows as removed in code and added as plain words.
    #[test]
    fn sr6_a_word_out_of_its_code_span_is_marked() {
        assert_eq!(
            para("Run `make` now.", "Run make now."),
            format!("Run {DEL}<code>make</code></del>{INS}make</ins> now.")
        );
    }

    // rule 9 (D-AXK27): a table's diff is a table

    fn table(head: &[&str], rows: &[&[&str]]) -> Block {
        use rdy_model::FieldValue;
        let strs = |c: &[&str]| {
            FieldValue::List(c.iter().map(|t| FieldValue::Str(t.to_string())).collect())
        };
        Block {
            id: BlockId::parse("tb-AAAAA").expect("an id"),
            block_type: "table".into(),
            parent: None,
            order: "a0".into(),
            fields: BTreeMap::from([
                ("columns".to_string(), strs(head)),
                (
                    "rows".to_string(),
                    FieldValue::List(rows.iter().map(|r| strs(r)).collect()),
                ),
            ]),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    fn prose(body: &str) -> Block {
        Block {
            block_type: "prose".into(),
            fields: BTreeMap::new(),
            body: body.into(),
            ..table(&[], &[])
        }
    }

    fn pack() -> rdy_schema::Pack {
        rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .expect("the pack")
    }

    /// `block_diff` of `old` to `new`, both read against one empty state.
    fn diff_of(old: Option<&Block>, new: Option<&Block>) -> String {
        let m = Materialized::default();
        let pack = pack();
        let site = Site::new(&m, &pack, "Project");
        let sides = Sides {
            from: &m,
            to: &m,
            text: &site,
            from_text: None,
        };
        block_diff(old, new, &sides)
    }

    /// A table diff's cells, row by row (the header row first): `[-x-]` removed, `{+x+}` added.
    fn grid(html: &str) -> Vec<Vec<String>> {
        let inner = html
            .strip_prefix("<div class=\"table-wrap chg-diff\" data-rdy=\"content\"><table><thead>")
            .and_then(|h| h.strip_suffix("</tbody></table></div>"))
            .unwrap_or_else(|| panic!("a table diff: {html}"))
            .replace(DEL, "[-")
            .replace("</del>", "-]")
            .replace(INS, "{+")
            .replace("</ins>", "+}")
            .replace("</thead><tbody>", "");
        inner
            .split("<tr>")
            .filter(|r| !r.is_empty())
            .map(|r| {
                let r = r.strip_suffix("</tr>").expect("a row");
                r.split("</th>")
                    .flat_map(|c| c.split("</td>"))
                    .filter(|c| !c.is_empty())
                    .map(|c| {
                        if let Some(span) = c
                            .strip_prefix("<td colspan=\"")
                            .or_else(|| c.strip_prefix("<th scope=\"col\" colspan=\""))
                        {
                            return format!("~{}", span.trim_end_matches("\">"));
                        }
                        c.strip_prefix("<th scope=\"col\">")
                            .or_else(|| c.strip_prefix("<td>"))
                            .expect("a cell")
                            .to_string()
                    })
                    .collect()
            })
            .collect()
    }

    fn cells(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    fn tdiff(old: &Block, new: &Block) -> Vec<Vec<String>> {
        grid(&diff_of(Some(old), Some(new)))
    }

    /// Columns pair by header text: an added column is added in every cell, a removed one
    /// removed, in its old place; a changed header is one removed and one added column.
    #[test]
    fn sr6_table_columns_pair_by_header_text() {
        let old = table(
            &["Name", "Size", "Note"],
            &[&["a", "1", "x"], &["b", "2", "y"]],
        );
        // Size removed, Colour added at the end, Note renamed to Notes
        let new = table(
            &["Name", "Notes", "Colour"],
            &[&["a", "x", "red"], &["b", "y", "blue"]],
        );
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["Name", "[-Size-]", "[-Note-]", "{+Notes+}", "{+Colour+}"],
                &["a", "[-1-]", "[-x-]", "{+x+}", "{+red+}"],
                &["b", "[-2-]", "[-y-]", "{+y+}", "{+blue+}"],
            ])
        );
        // a removed first column stands before them all
        let old = table(&["Id", "Name"], &[&["a", "a"]]);
        let new = table(&["Name"], &[&["a"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[&["[-Id-]", "Name"], &["[-a-]", "a"]])
        );
    }

    /// Rows pair by a first cell unique in both tables, in the new order; a removed row stands
    /// after the row that came before it in the old table.
    #[test]
    fn sr6_table_rows_pair_by_a_unique_first_cell() {
        let old = table(&["K", "V"], &[&["a", "1"], &["b", "2"], &["c", "3"]]);
        let new = table(
            &["K", "V"],
            &[&["z", "0"], &["a", "1"], &["c", "3"], &["d", "4"]],
        );
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["K", "V"],
                &["{+z+}", "{+0+}"],
                &["a", "1"],
                &["[-b-]", "[-2-]"],
                &["c", "3"],
                &["{+d+}", "{+4+}"],
            ])
        );
        // a removed first row stands first
        let new = table(&["K", "V"], &[&["b", "2"], &["c", "3"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[&["K", "V"], &["[-a-]", "[-1-]"], &["b", "2"], &["c", "3"]])
        );
    }

    /// A first cell that is not unique in both tables pairs no row: those rows are removed and
    /// added.
    #[test]
    fn sr6_table_duplicate_first_cells_are_removed_and_added() {
        let old = table(&["K", "V"], &[&["a", "1"], &["a", "2"], &["b", "3"]]);
        let new = table(&["K", "V"], &[&["a", "1"], &["b", "4"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["K", "V"],
                &["[-a-]", "[-1-]"],
                &["[-a-]", "[-2-]"],
                &["{+a+}", "{+1+}"],
                &["b", "[-3-]{+4+}"],
            ])
        );
    }

    /// A cell of a paired row and column shows its own word diff; an unchanged cell its text.
    /// Text is escaped, and a code span stays a `<code>` element.
    #[test]
    fn sr6_table_cells_show_their_own_word_diff() {
        let old = table(
            &["K", "Say"],
            &[&["a", "the old <b> words"], &["b", "same"]],
        );
        let new = table(
            &["K", "Say"],
            &[&["a", "the new <b> words"], &["b", "same"]],
        );
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["K", "Say"],
                &["a", "the [-old-]{+new+} &lt;b&gt; words"],
                &["b", "same"],
            ])
        );
        let old = table(&["K", "Run"], &[&["a", "run `make` now"]]);
        let new = table(&["K", "Run"], &[&["a", "run `make all` now"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["K", "Run"],
                &["a", "run <code>make</code>{+<code> all</code>+} now"]
            ])
        );
    }

    /// The cost cap holds for each cell on its own: a cell past it shows its old text removed
    /// and its new text added, inside its own cell, and the other cells keep their own diff.
    #[test]
    fn sr6_table_a_cell_past_the_cap_stays_in_its_cell() {
        let words = |w: &str| {
            (0..1_500)
                .map(|i| format!("{w}{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let (ow, nw) = (words("old"), words("new"));
        let old = table(&["K", "Long", "Short"], &[&["a", &ow, "one"]]);
        let new = table(&["K", "Long", "Short"], &[&["a", &nw, "two"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["K", "Long", "Short"],
                &["a", &format!("[-{ow}-]{{+{nw}+}}"), "[-one-]{+two+}"],
            ])
        );
    }

    /// A new table shows whole as added and a removed one whole as removed, as tables.
    #[test]
    fn sr6_a_new_or_removed_table_is_a_table() {
        let t = table(&["K", "V"], &[&["a", "1"]]);
        assert_eq!(
            grid(&diff_of(None, Some(&t))),
            cells(&[&["{+K+}", "{+V+}"], &["{+a+}", "{+1+}"]])
        );
        assert_eq!(
            grid(&diff_of(Some(&t), None)),
            cells(&[&["[-K-]", "[-V-]"], &["[-a-]", "[-1-]"]])
        );
    }

    /// A table made a paragraph (or the reverse) shows each side whole in its own element.
    #[test]
    fn sr6_a_table_made_a_paragraph_shows_each_side_whole() {
        let t = table(&["K"], &[&["a"]]);
        let p = prose("Some words.");
        let removed = diff_of(Some(&t), None);
        let added = diff_of(None, Some(&t));
        assert_eq!(
            diff_of(Some(&t), Some(&p)),
            format!(
                "{removed}<p class=\"chg-diff\" data-rdy=\"content\">{INS}Some words.</ins></p>"
            )
        );
        assert_eq!(
            diff_of(Some(&p), Some(&t)),
            format!("<p class=\"chg-diff\" data-rdy=\"content\">{DEL}Some words.</del></p>{added}")
        );
    }

    /// A table of about 53,000 characters with one cell changed shows as a table with one small
    /// cell diff, never one removed and one added run of the whole table.
    #[test]
    fn sr6_a_large_table_with_one_changed_cell_shows_one_cell_diff() {
        let row = |i: usize, last: &str| {
            vec![
                format!("row{i:04}"),
                format!("alpha beta gamma delta {i}"),
                last.to_string(),
            ]
        };
        let rows: Vec<Vec<String>> = (0..1_000)
            .map(|i| row(i, "a quiet line of words"))
            .collect();
        let mut changed = rows.clone();
        changed[500][2] = "a loud line of words".to_string();
        fn as_refs(rs: &[Vec<String>]) -> Vec<Vec<&str>> {
            rs.iter()
                .map(|r| r.iter().map(String::as_str).collect())
                .collect()
        }
        let (o, n) = (as_refs(&rows), as_refs(&changed));
        let o: Vec<&[&str]> = o.iter().map(Vec::as_slice).collect();
        let n: Vec<&[&str]> = n.iter().map(Vec::as_slice).collect();
        let (old, new) = (
            table(&["Key", "Words", "Tone"], &o),
            table(&["Key", "Words", "Tone"], &n),
        );
        let chars: usize = rows.iter().flatten().map(|c| c.chars().count()).sum();
        assert!((50_000..56_000).contains(&chars), "{chars}");
        let started = std::time::Instant::now();
        let html = diff_of(Some(&old), Some(&new));
        let took = started.elapsed();
        eprintln!(
            "53k table diff: {chars} characters, {} bytes of HTML in {took:?}",
            html.len()
        );
        assert_eq!(html.matches("<del>").count(), 1);
        assert_eq!(html.matches("<ins>").count(), 1);
        assert!(html.contains(&format!(
            "<td>a {DEL}quiet</del>{INS}loud</ins> line of words</td>"
        )));
        let g = grid(&html);
        assert_eq!(g.len(), 1_001);
        assert!(g.iter().all(|r| r.len() == 3));
    }

    /// A row has a cell only where its old or new side has one (review F1): a short row ends at
    /// its last cell, and a run of places inside it that neither side has is one spanning cell.
    /// So a ragged table's diff grows with its cells, never with rows times columns.
    #[test]
    fn sr6_a_ragged_table_diff_grows_with_its_cells() {
        let wide: Vec<String> = (0..1_000).map(|i| format!("w{i}")).collect();
        let short: Vec<String> = (0..999).map(|i| format!("s{i}")).collect();
        let mut rows: Vec<Vec<&str>> = vec![wide.iter().map(String::as_str).collect()];
        rows.extend(short.iter().map(|s| vec![s.as_str()]));
        let rows: Vec<&[&str]> = rows.iter().map(Vec::as_slice).collect();
        let old = table(&["K"], &rows);
        let mut new_rows = rows.clone();
        let changed = ["w0", "changed"];
        new_rows[0] = &changed;
        let new = table(&["K"], &new_rows);
        let html = diff_of(Some(&old), Some(&new));
        // 1,999 cells on the old side and 1,001 on the new: about one <td> each, never 1,000,000
        let tds = html.matches("<td").count();
        eprintln!(
            "ragged table diff: {tds} cells, {} bytes of HTML",
            html.len()
        );
        assert!(tds <= 2 * (1_999 + 1_001), "{tds} cells");
        assert!(html.len() < 200_000, "{} bytes", html.len());
        let g = grid(&html);
        // the header stops at its one cell; each short row at its one cell
        assert_eq!(g[0], vec!["K".to_string()]);
        assert!(g[2..].iter().all(|r| r.len() == 1));
        // a gap inside a row is one spanning cell, so later cells keep their column
        let old = table(&["A", "B", "C"], &[&["k", "1", "2"]]);
        let new = table(&["A", "C"], &[&["k"], &["n", "3"]]);
        assert_eq!(
            tdiff(&old, &new),
            cells(&[
                &["A", "[-B-]", "C"],
                &["k", "[-1-]", "[-2-]"],
                &["{+n+}", "~1", "{+3+}"]
            ])
        );
    }
}
