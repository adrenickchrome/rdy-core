//! The changed-doc rows of the publication review (D-RDH1C, D-MCH70, D-0C4TB): for each doc that
//! both sides select and whose digest differs, its titles, pages, counts and change rows. The
//! digest comparison ([`DisclosureDiff::changed_docs`]) stays the gate: a doc gets a row here only
//! when its digest changed, and a row whose counts are all zero is `record_only`.
//!
//! Each row reads both sides through the side-view seam ([`PublicationReviewSide`]), never a
//! store or a full state. A block counts exactly when its `what` is not empty (D-MCH70): each
//! authored part that differs ([`changed_parts`]), and `place` for a counted move. Movement uses
//! the shared-sibling rule of the review chips (p-9M1DK): a new parent, a new home, or a new
//! order key with a new index among the siblings both sides hold. A block that moves to another
//! doc, or between a doc and the content outside docs, gets its one counted row at its new home;
//! its old doc lists and counts a `removed` row (D-0C4TB). Decisions count apart from these rows.
//!
//! Rows follow the review index's "Changes on this page" order: the doc's own row, the lede,
//! then each block in page order, with each removed block before the block it stood before
//! ([`removed_line_places`]). Excerpts and short diffs read the reader text, never the raw body,
//! and keep D-0C4TB's hard limits in Unicode scalar values: 200 for an excerpt and 300 for the
//! pieces of one diff, with the "…" and a code span's backticks inside the limit.

use super::diff::DisclosureDiff;
use super::review_model::{
    ChangeRow, ChangedDocReview, DiffOp, DiffPiece, ReviewChange, ReviewCounts,
};
use super::review_side::{
    BlockParts, PartName, PublicationReviewSide, ShownBlock, backticked, changed_parts,
};
use super::select::AttachmentVariant;
use crate::reader::inline::{Ch, Decoder};
use crate::reader::review::{removed_line_places, visible_type};
use crate::reader::shape::shared_places;
use crate::worddiff::{self, Op, Piece, Unit};
use rdy_model::{BlockId, Hash, tree};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The most Unicode scalar values of an excerpt, the "…" and a code span's backticks included
/// (D-0C4TB).
pub const EXCERPT_MAX: usize = 200;

/// The most Unicode scalar values of all the pieces of one short diff together, the "…" and a
/// code span's backticks included (D-0C4TB).
pub const DIFF_MAX: usize = 300;

/// The most unchanged words a short diff keeps on each side of its first changed run.
const CONTEXT_WORDS: usize = 8;

/// The characters a cut at a word boundary drops from the end, as the site's cut drops them.
const CUT_TRIM: [char; 6] = [' ', ',', ';', ':', '.', '-'];

const ELLIPSIS: char = '\u{2026}';

// ---- changed docs ---------------------------------------------------------------------------

/// The review rows of every changed doc of `diff`, in the candidate's reading order: depth
/// first, siblings by their order keys, a doc with no page in its place. `before` and `after`
/// are the two sides of `diff`, each viewed with the removals of both. The lists are complete:
/// no row is cut here, and no decision row is built here.
pub fn changed_doc_reviews(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> Vec<ChangedDocReview> {
    let sides = Sides {
        before,
        after,
        places: RefCell::new(HashMap::new()),
    };
    let mut docs: Vec<(Option<usize>, ChangedDocReview)> = diff
        .changed_docs
        .iter()
        .map(|d| {
            let place = after.tree_entry(&d.id).and_then(|t| t.position);
            (place, sides.doc(&d.id))
        })
        .collect();
    docs.sort_by(|(p, a), (q, b)| {
        (p.is_none(), p)
            .cmp(&(q.is_none(), q))
            .then_with(|| a.id.cmp(&b.id))
    });
    docs.into_iter().map(|(_, d)| d).collect()
}

/// Each shared sibling's (old, new) index among the siblings both sides hold, by block id.
type SharedPlaces = BTreeMap<BlockId, (usize, usize)>;

/// The two sides of one comparison, with each parent's shared sibling places worked out once.
struct Sides<'s> {
    before: &'s dyn PublicationReviewSide,
    after: &'s dyn PublicationReviewSide,
    places: RefCell<HashMap<Option<BlockId>, SharedPlaces>>,
}

/// The block `id` as `side` shows it on the page: a block a reader sees, never a doc or folder.
fn content<'s>(side: &'s dyn PublicationReviewSide, id: &BlockId) -> Option<&'s ShownBlock> {
    side.shown(id)
        .filter(|s| visible_type(&s.block_type) && !tree::is_tree_type(&s.block_type))
}

/// Every block `side` shows under `root`, depth first in page order, with its depth below
/// `root`. A block is visited once, so a damaged parent chain still ends.
fn walk<'s>(side: &'s dyn PublicationReviewSide, root: &BlockId) -> Vec<(&'s BlockId, usize)> {
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    let mut stack: Vec<(&BlockId, usize)> = side
        .children(Some(root))
        .iter()
        .rev()
        .map(|c| (c, 1))
        .collect();
    while let Some((id, depth)) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        out.push((id, depth));
        stack.extend(side.children(Some(id)).iter().rev().map(|c| (c, depth + 1)));
    }
    out
}

/// `what` for `parts` that differ, with `place` for a counted move: `title`, `text`, `type`,
/// `links`, `place`, `status`, then each `field:<name>` by name (D-RDH1C).
fn what_of(parts: &[PartName], place: bool) -> Vec<String> {
    let mut out = vec![];
    let mut place = place;
    for p in parts {
        if place && matches!(p, PartName::Status | PartName::Field(_)) {
            out.push("place".to_string());
            place = false;
        }
        out.push(p.to_string());
    }
    if place {
        out.push("place".to_string());
    }
    out
}

impl Sides<'_> {
    /// Whether a block both sides show took a new place (p-9M1DK, D-0C4TB): a new home doc, a
    /// new parent, or a new order key and a new index among the siblings both sides hold under
    /// its parent. A new key at the same place moves nothing.
    fn moved(&self, b: &ShownBlock, a: &ShownBlock) -> bool {
        if b.home_doc != a.home_doc || b.parent != a.parent {
            return true;
        }
        if b.order == a.order {
            return false;
        }
        let mut places = self.places.borrow_mut();
        let at = places.entry(a.parent.clone()).or_insert_with(|| {
            let old: Vec<&BlockId> = self.before.children(a.parent.as_ref()).iter().collect();
            let new: Vec<&BlockId> = self.after.children(a.parent.as_ref()).iter().collect();
            shared_places(&old, &new)
        });
        at.get(&a.id).is_some_and(|(x, y)| x != y)
    }

    /// The review row of changed doc `doc`.
    fn doc(&self, doc: &BlockId) -> ChangedDocReview {
        let (bt, at) = (self.before.tree_entry(doc), self.after.tree_entry(doc));
        let title = at.and_then(|t| t.title.clone());
        let before_title = bt
            .and_then(|t| t.title.clone())
            .filter(|t| title.as_ref() != Some(t));
        let changes = self.changes(doc);
        let counts = self.counts(&changes);
        ChangedDocReview {
            id: doc.clone(),
            title,
            before_title,
            page: at.and_then(|t| t.page.clone()),
            before_page: bt.and_then(|t| t.page.clone()),
            counts,
            record_only: counts == ReviewCounts::default(),
            changes,
            omitted_changes: 0,
            decisions: vec![],
            omitted_decisions: 0,
        }
    }

    /// The change rows of `doc` in page order: its own row, the lede's, then each block's, each
    /// removed block before the block it stood before, and the rest of the removed blocks last.
    fn changes(&self, doc: &BlockId) -> Vec<ChangeRow> {
        let (before, after) = (self.before, self.after);
        let lede = after.tree_entry(doc).and_then(|t| t.lede.as_ref());
        let items: Vec<&BlockId> = walk(after, doc)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| Some(*id) != lede)
            .collect();
        let item_set: BTreeSet<&BlockId> = items.iter().copied().collect();

        // the removed blocks: gone from the doc's page, each placed as a removed line places it
        let stays =
            |id: &BlockId| content(after, id).is_some_and(|a| a.home_doc.as_ref() == Some(doc));
        let gone = |id: &BlockId| content(before, id).is_some() && !stays(id);
        let order = walk(before, doc);
        let mut before_item: HashMap<&BlockId, Vec<ChangeRow>> = HashMap::new();
        let mut at_end = vec![];
        for (stood_before, ids) in removed_line_places(&order, &gone, &stays, &gone) {
            let rows = ids.into_iter().filter_map(|id| {
                let b = content(before, id)?;
                Some(row(before, b, ReviewChange::Removed, vec![], None))
            });
            // a line before the lede stands before the first block after it, as on the page
            let target = match stood_before {
                Some(id) if Some(id) == lede => items.first().copied(),
                Some(id) => item_set.get(id).copied(),
                None => None,
            };
            match target {
                Some(id) => before_item.entry(id).or_default().extend(rows),
                None => at_end.extend(rows),
            }
        }

        let mut out: Vec<ChangeRow> = self.compared(doc).into_iter().collect();
        if let Some(l) = lede
            && let Some(r) = self.candidate(l)
        {
            out.push(r);
        }
        for id in items {
            out.extend(before_item.remove(id).unwrap_or_default());
            out.extend(self.candidate(id));
        }
        out.extend(at_end);
        out
    }

    /// The row of a block the candidate shows on the page: `added` when the baseline does not
    /// show it, else its compared row.
    fn candidate(&self, id: &BlockId) -> Option<ChangeRow> {
        let a = content(self.after, id)?;
        match content(self.before, id) {
            None => Some(row(self.after, a, ReviewChange::Added, vec![], None)),
            Some(_) => self.compared(id),
        }
    }

    /// The row of a block both sides show, from its parts and its place: none when nothing a
    /// reader sees changed, `moved` for a change of place alone, else `updated`.
    fn compared(&self, id: &BlockId) -> Option<ChangeRow> {
        let (b, a) = (self.before.shown(id)?, self.after.shown(id)?);
        let none = BlockParts::default();
        let parts = changed_parts(
            self.before.parts(id).unwrap_or(&none),
            self.after.parts(id).unwrap_or(&none),
        );
        let place = self.moved(b, a);
        if parts.is_empty() {
            return place.then(|| row(self.after, a, ReviewChange::Moved, vec![], None));
        }
        let diff = match (
            parts.contains(&PartName::Text),
            self.before.reader_text(id),
            self.after.reader_text(id),
        ) {
            (true, Some(old), Some(new)) => short_diff(old, new, &b.block_type, &a.block_type),
            _ => None,
        };
        Some(row(
            self.after,
            a,
            ReviewChange::Updated,
            what_of(&parts, place),
            diff,
        ))
    }

    /// The counts of a doc's rows. Sections and figures count as blocks too; figures count by
    /// attachment image, light and dark each.
    fn counts(&self, rows: &[ChangeRow]) -> ReviewCounts {
        let mut c = ReviewCounts::default();
        for r in rows {
            match r.change {
                ReviewChange::Added => c.added += 1,
                ReviewChange::Removed => c.removed += 1,
                ReviewChange::Updated => c.updated += 1,
                ReviewChange::Moved => c.moved += 1,
            }
            match (r.block_type.as_str(), r.change) {
                ("section", ReviewChange::Added) => c.sections_added += 1,
                ("section", ReviewChange::Removed) => c.sections_removed += 1,
                ("figure", ReviewChange::Added) => c.figures_added += images(self.after, &r.id),
                ("figure", ReviewChange::Removed) => {
                    c.figures_removed += images(self.before, &r.id)
                }
                ("figure", ReviewChange::Updated) => {
                    let (old, new) = (image_map(self.before, &r.id), image_map(self.after, &r.id));
                    for v in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
                        match (old.get(v), new.get(v)) {
                            (Some(o), Some(n)) if o != n => c.figures_changed += 1,
                            (None, Some(_)) => c.figures_added += 1,
                            (Some(_), None) => c.figures_removed += 1,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        c
    }
}

/// Each attachment image of figure `block` on `side`, by variant: its blob and availability.
fn image_map(
    side: &dyn PublicationReviewSide,
    block: &BlockId,
) -> BTreeMap<Option<AttachmentVariant>, (Hash, bool)> {
    side.selection()
        .attachments
        .iter()
        .filter(|a| a.block == *block)
        .map(|a| (a.variant, (a.blob, a.available)))
        .collect()
}

/// How many attachment images figure `block` has on `side`.
fn images(side: &dyn PublicationReviewSide, block: &BlockId) -> u64 {
    image_map(side, block).len() as u64
}

/// One change row of block `s`, read on `side`: the candidate's, or the baseline's for a removed
/// block. A removed version reads "Content removed." and has no excerpt and no diff.
fn row(
    side: &dyn PublicationReviewSide,
    s: &ShownBlock,
    change: ReviewChange,
    what: Vec<String>,
    diff: Option<Vec<DiffPiece>>,
) -> ChangeRow {
    ChangeRow {
        id: s.id.clone(),
        block_type: s.block_type.clone(),
        change,
        label: s.label.clone(),
        section: s
            .section
            .as_ref()
            .and_then(|x| side.shown(x))
            .map(|x| x.label.clone()),
        what,
        excerpt: match s.redacted {
            true => None,
            false => side.reader_text(&s.id).and_then(excerpt),
        },
        diff: diff.filter(|_| !s.redacted),
        redacted: s.redacted,
    }
}

// ---- text -----------------------------------------------------------------------------------

/// `chars` with each whitespace run read as one space, which keeps the code span of the first
/// whitespace it stands for. With `trim`, no space stays at either end.
fn collapse(chars: &[Ch], trim: bool) -> Vec<Ch> {
    let mut out: Vec<Ch> = Vec::with_capacity(chars.len());
    let mut space: Option<u32> = None;
    for &(c, span) in chars {
        if c.is_whitespace() {
            space.get_or_insert(span);
            continue;
        }
        if let Some(s) = space.take()
            && (!trim || !out.is_empty())
        {
            out.push((' ', s));
        }
        out.push((c, span));
    }
    if let Some(s) = space
        && !trim
    {
        out.push((' ', s));
    }
    out
}

/// The scalar values of `chars[..i]` as plain text, for each `i`: each character, and two
/// backticks for each code span run.
fn rendered_lengths(chars: &[Ch]) -> Vec<usize> {
    let mut len = Vec::with_capacity(chars.len() + 1);
    len.push(0);
    for (i, &(_, span)) in chars.iter().enumerate() {
        let opens = span != 0 && (i == 0 || chars[i - 1].1 != span);
        len.push(len[i] + 1 + if opens { 2 } else { 0 });
    }
    len
}

/// Collapsed `words` as plain text of at most `max` scalar values (D-0C4TB): whole when they fit;
/// else cut at the last word boundary that fits with "…" inside the limit, dropping the
/// characters the site's cut drops at the end; else, when the first word alone passes the limit,
/// cut inside it. A cut inside a code span closes the span with a backtick before the "…".
fn hard_cut(words: &[Ch], max: usize) -> String {
    let len = rendered_lengths(words);
    if len[words.len()] <= max {
        return backticked(words);
    }
    if max == 0 {
        return String::new();
    }
    // the last boundary whose kept text fits beside the "…"; the kept end never shrinks
    let mut end = None;
    let mut kept = 0;
    for (i, &(c, _)) in words.iter().enumerate() {
        if c == ' ' && i > 0 {
            if len[kept] + 1 > max {
                break;
            }
            end = Some(kept);
        }
        if !CUT_TRIM.contains(&c) {
            kept = i + 1;
        }
    }
    let end = end.unwrap_or_else(|| (0..=words.len()).rev().find(|&j| len[j] < max).unwrap_or(0));
    let mut out = backticked(&words[..end]);
    out.push(ELLIPSIS);
    out
}

/// The excerpt of a block's coded reader text (p-8Q2FB): plain text, whitespace collapsed, each
/// code span in backticks, at most [`EXCERPT_MAX`] scalar values. `None` when the text holds no
/// word.
pub fn excerpt(coded: &str) -> Option<String> {
    let words = collapse(&Decoder::default().read(coded), true);
    (!words.is_empty()).then(|| hard_cut(&words, EXCERPT_MAX))
}

/// The short diff of two coded reader texts (p-FYNX8): the site's word diff, each side read back
/// with its code spans, then the first changed run with up to eight unchanged words on each side,
/// at most [`DIFF_MAX`] scalar values in all. A code block diffs by lines; a block that changes
/// between code and other text shows each side whole. `None` when the texts are equal.
pub fn short_diff(old: &str, new: &str, old_type: &str, new_type: &str) -> Option<Vec<DiffPiece>> {
    if old == new {
        return None;
    }
    let code = |t: &str| t == "code";
    let pieces = if code(old_type) == code(new_type) {
        let unit = if code(new_type) {
            Unit::Lines
        } else {
            Unit::Words
        };
        worddiff::diff(old, new, unit)
    } else {
        let mut p = worddiff::diff(old, "", Unit::Lines);
        p.extend(worddiff::diff("", new, Unit::Lines));
        p
    };
    let read = read_back(&pieces);
    let first = read.iter().position(|(op, _)| *op != DiffOp::Same)?;
    let last = read[first..]
        .iter()
        .position(|(op, _)| *op == DiffOp::Same)
        .map_or(read.len(), |i| first + i);
    let mut window: Vec<(DiffOp, Vec<Ch>)> = vec![];
    if first > 0 {
        window.push((DiffOp::Same, last_words(&read[first - 1].1)));
    }
    window.extend(read[first..last].iter().cloned());
    if let Some((_, same)) = read.get(last) {
        window.push((DiffOp::Same, first_words(same)));
    }
    let mut left = DIFF_MAX;
    let mut out = vec![];
    for (op, chars) in window {
        let chars = collapse(&chars, false);
        if chars.is_empty() {
            continue;
        }
        let n = rendered_lengths(&chars)[chars.len()];
        if n <= left {
            left -= n;
            out.push(DiffPiece {
                op,
                text: backticked(&chars),
            });
            continue;
        }
        if left > 0 {
            out.push(DiffPiece {
                op,
                text: cut_piece(&chars, left),
            });
        }
        break;
    }
    (!out.is_empty()).then_some(out)
}

/// A collapsed piece cut to `max` scalar values by the excerpt's rule, keeping a leading space.
fn cut_piece(chars: &[Ch], max: usize) -> String {
    match chars.first() {
        Some(&(' ', 0)) if max > 1 => format!(" {}", hard_cut(&collapse(chars, true), max - 1)),
        _ => hard_cut(&collapse(chars, true), max),
    }
}

/// The word diff's pieces, each side read back on its own, as the site reads them: the old text
/// (`same` and `del`) and the new text (`same` and `add`) keep their code spans. A `same` run that
/// is code on one side only, or that the sides read differently, reads as removed then added.
/// Neighbours of one operation join.
fn read_back(pieces: &[Piece]) -> Vec<(DiffOp, Vec<Ch>)> {
    fn push(out: &mut Vec<(DiffOp, Vec<Ch>)>, op: DiffOp, chars: Vec<Ch>) {
        if chars.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((last, c)) if *last == op => c.extend(chars),
            _ => out.push((op, chars)),
        }
    }
    let (mut old, mut new) = (Decoder::default(), Decoder::default());
    let mut out = vec![];
    for p in pieces {
        match p.op {
            Op::Del => push(&mut out, DiffOp::Del, old.read(&p.text)),
            Op::Add => push(&mut out, DiffOp::Add, new.read(&p.text)),
            Op::Same => {
                let (o, n) = (old.read(&p.text), new.read(&p.text));
                if !o.iter().map(|c| c.0).eq(n.iter().map(|c| c.0)) {
                    push(&mut out, DiffOp::Del, o);
                    push(&mut out, DiffOp::Add, n);
                    continue;
                }
                let both: Vec<(Ch, Ch)> = o.into_iter().zip(n).collect();
                for run in both.chunk_by(|a, b| a.0.1 == b.0.1 && a.1.1 == b.1.1) {
                    let (o, n): (Vec<Ch>, Vec<Ch>) = run.iter().copied().unzip();
                    if (o[0].1 == 0) == (n[0].1 == 0) {
                        push(&mut out, DiffOp::Same, o);
                    } else {
                        push(&mut out, DiffOp::Del, o);
                        push(&mut out, DiffOp::Add, n);
                    }
                }
            }
        }
    }
    out
}

/// The start of each word of `chars`: a character that is no whitespace and follows whitespace
/// or the start.
fn word_starts(chars: &[Ch]) -> Vec<usize> {
    (0..chars.len())
        .filter(|&i| !chars[i].0.is_whitespace() && (i == 0 || chars[i - 1].0.is_whitespace()))
        .collect()
}

/// The last [`CONTEXT_WORDS`] words of `chars`, with the whitespace after them.
fn last_words(chars: &[Ch]) -> Vec<Ch> {
    let starts = word_starts(chars);
    let from = match starts.len().checked_sub(CONTEXT_WORDS) {
        Some(k) if k > 0 => starts[k],
        _ => 0,
    };
    chars[from..].to_vec()
}

/// The first [`CONTEXT_WORDS`] words of `chars`, with the whitespace before them.
fn first_words(chars: &[Ch]) -> Vec<Ch> {
    let starts = word_starts(chars);
    let to = match starts.get(CONTEXT_WORDS) {
        Some(&next) => chars[..next]
            .iter()
            .rposition(|c| !c.0.is_whitespace())
            .map_or(0, |i| i + 1),
        None => chars.len(),
    };
    chars[..to].to_vec()
}
