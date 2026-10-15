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
use crate::escape;
use crate::home::{self, ChangeRow};
use crate::inline::{CODE_CLOSE, CODE_ESC, CODE_OPEN, Form, code_escape};
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
    /// The word diff: `<p class="chg-diff" data-rdy="content">…</p>`, or for a code block
    /// `<pre class="code chg-diff" data-rdy="content"><code>…</code></pre>` (rule 6).
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
    let rows: Vec<ChangeRow> = match of {
        Of::Review(_) => home::changes(site, rv),
        Of::Since(_) => since_rows(site, rv, &order, sides),
    };
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
        let html = block_diff(old, new, sides);
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
            .map(|(b, m)| page_title(b, m, sides.text))
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
            None => home::removed_row(doc, page_title(doc, sides.from, sides.text)),
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

/// The diff of a block from `old` (in `from`) to `new` (in `to`) (rules 4 and 6): a word diff in
/// the block's element when both sides have the same kind of text, else each side whole in its
/// own element.
fn block_diff(old: Option<&Block>, new: Option<&Block>, sides: &Sides) -> String {
    let site = sides.text;
    let old = old.map(|o| (text(o, sides.from, site), wrap_of(o), o));
    let new = new.map(|n| (text(n, sides.to, site), wrap_of(n), n));
    match (old, new) {
        (Some((ot, ow, _)), Some((nt, nw, n))) if ow.same_kind(&nw) => {
            diff_html(&worddiff::diff(&ot, &nt, unit(n)), &nw)
        }
        (Some((ot, ow, _)), Some((nt, nw, _))) => {
            let mut out = String::new();
            if !ot.is_empty() {
                out.push_str(&diff_html(&worddiff::diff(&ot, "", Unit::Lines), &ow));
            }
            if !nt.is_empty() || ot.is_empty() {
                out.push_str(&diff_html(&worddiff::diff("", &nt, Unit::Lines), &nw));
            }
            out
        }
        (None, Some((nt, nw, n))) => diff_html(&worddiff::diff("", &nt, unit(n)), &nw),
        (Some((ot, ow, o)), None) => diff_html(&worddiff::diff(&ot, "", unit(o)), &ow),
        (None, None) => String::new(),
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

/// One character of a side's text as a reader reads it, and the code span it is in: `0` for
/// none, else the span's number on that side (two neighbouring spans stay two elements).
type Ch = (char, u32);

/// Reads one side's coded text back (rule 7), in order, across the pieces of a diff.
#[derive(Default)]
struct Decoder {
    /// The last character was [`CODE_ESC`]: the next one is authored text.
    esc: bool,
    span: u32,
    spans: u32,
}

impl Decoder {
    fn read(&mut self, text: &str) -> Vec<Ch> {
        let mut out = vec![];
        for c in text.chars() {
            if std::mem::take(&mut self.esc) {
                out.push((c, self.span));
                continue;
            }
            match c {
                CODE_ESC => self.esc = true,
                CODE_OPEN => {
                    self.spans += 1;
                    self.span = self.spans;
                }
                CODE_CLOSE => self.span = 0,
                _ => out.push((c, self.span)),
            }
        }
        out
    }
}

/// Characters as runs, escaped, each run of a code span in a `<code>` element.
fn runs_html(out: &mut String, chars: &[Ch]) {
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
            return field_words(c, "title", sides.text, false).unwrap_or_else(|| "Untitled".into());
        }
        if c.block_type == tree::DOC {
            return page_title(c, m, sides.text);
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
        crate::inline::plain_refs_coded(t, RefMode::Strict, Form::Spans, &|_| String::new())
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

    /// Text that leaves a code span shows as removed in code and added as plain words.
    #[test]
    fn sr6_a_word_out_of_its_code_span_is_marked() {
        assert_eq!(
            para("Run `make` now.", "Run make now."),
            format!("Run {DEL}<code>make</code></del>{INS}make</ins> now.")
        );
    }
}
