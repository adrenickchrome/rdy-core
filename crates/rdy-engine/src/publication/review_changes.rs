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
//! its old doc lists and counts a `removed` row (D-0C4TB). Decisions count apart from these rows:
//! a decision has no `moved` kind, so one that leaves a doc counts `removed` there and `added` at
//! its new home.
//!
//! The content outside docs takes the part of a doc for moves to and from it
//! ([`outside_reviews`]). Its rows count by their parts with sibling place left out; a version
//! change with no changed part keeps its row, counts nothing and is `record_only` (D-BND1J).
//!
//! Each change row names its section by label and id, and a moved block's row, or an updated
//! row with `place`, the section that held it on the baseline's side (D-EGFYD, D-9PG33). Each
//! changed doc gives its candidate's top folder, its place in the review's reading order of doc
//! rows, and the candidate's outline with the change rows of each section counted by its id,
//! over every row before any cut.
//!
//! Rows follow the review index's "Changes on this page" order: the doc's own row, the lede,
//! then each block in page order, with each removed block before the block it stood before
//! ([`removed_line_places`]). Excerpts and short diffs read the reader text, never the raw body,
//! and keep D-0C4TB's hard limits in Unicode scalar values: 200 for an excerpt and 300 for the
//! pieces of one diff, with the "…" and a code span's backticks inside the limit.

use super::diff::{ChangedFolder, DisclosureDiff};
use super::review_docs::{OUTLINE_MAX, capped, doc_positions, outline_rows, top_folder};
use super::review_labels::{DECISIONS_MAX, decision_row, doc_decisions};
use super::review_model::{
    ChangeRow, ChangedDocReview, DiffOp, DiffPiece, OutsideReview, ReviewChange, ReviewCounts,
};
use super::review_side::{
    BlockParts, PartName, PublicationReviewSide, ShownBlock, backticked, changed_parts,
};
use super::select::AttachmentVariant;
use crate::reader::inline::{Ch, Decoder};
use crate::reader::review::{removed_line_places, visible_type};
use crate::reader::shape::shared_places;
use crate::worddiff::{self, Op, Piece, Unit};
use rdy_model::{BlockId, Hash, decision, tree};
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
/// are the two sides of `diff`, each viewed with the removals of both. The change rows are
/// complete: no change row is cut here. The decision rows keep their first [`DECISIONS_MAX`]
/// and count the rest in `omitted_decisions`. The counts count every row.
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
    let positions = doc_positions(diff, before, after);
    let mut docs: Vec<(Option<usize>, ChangedDocReview)> = diff
        .changed_docs
        .iter()
        .map(|d| {
            let place = after.tree_entry(&d.id).and_then(|t| t.position);
            let review = ChangedDocReview {
                position: positions.get(&d.id).copied().unwrap_or_default(),
                ..sides.doc(&d.id)
            };
            (place, review)
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
pub(super) fn content<'s>(
    side: &'s dyn PublicationReviewSide,
    id: &BlockId,
) -> Option<&'s ShownBlock> {
    side.shown(id)
        .filter(|s| visible_type(&s.block_type) && !tree::is_tree_type(&s.block_type))
}

/// Every block `side` shows under `root`, depth first in page order, with its depth below
/// `root`. A block is visited once, so a damaged parent chain still ends.
pub(super) fn walk<'s>(
    side: &'s dyn PublicationReviewSide,
    root: &BlockId,
) -> Vec<(&'s BlockId, usize)> {
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

    /// The review row of changed doc `doc`. Its position is 0 until its review places it.
    fn doc(&self, doc: &BlockId) -> ChangedDocReview {
        let (bt, at) = (self.before.tree_entry(doc), self.after.tree_entry(doc));
        let title = at.and_then(|t| t.title.clone());
        let before_title = bt
            .and_then(|t| t.title.clone())
            .filter(|t| title.as_ref() != Some(t));
        let changes = self.changes(doc);
        let mut counts = self.counts(&changes);
        let mut decisions = doc_decisions(self.before, self.after, doc);
        for d in &decisions {
            count_row(
                &mut counts,
                self.before,
                self.after,
                decision::DECISION,
                &d.id,
                d.change,
            );
        }
        let omitted_decisions = decisions.len().saturating_sub(DECISIONS_MAX) as u64;
        decisions.truncate(DECISIONS_MAX);
        // each section counts the rows whose block sits in it, by id, before any row is cut
        let mut outline = outline_rows(self.after, doc);
        for r in &mut outline {
            r.changes = changes
                .iter()
                .filter(|c| c.section_id.as_ref() == Some(&r.id))
                .count() as u64;
        }
        let (outline, omitted_outline) = capped(outline, OUTLINE_MAX);
        let (folder, folder_redacted) = top_folder(self.after, doc);
        ChangedDocReview {
            id: doc.clone(),
            title,
            before_title,
            page: at.and_then(|t| t.page.clone()),
            before_page: bt.and_then(|t| t.page.clone()),
            folder,
            folder_redacted,
            position: 0,
            outline,
            omitted_outline,
            counts,
            record_only: counts == ReviewCounts::default(),
            changes,
            omitted_changes: 0,
            decisions,
            omitted_decisions,
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
            return place.then(|| {
                left_section(
                    row(self.after, a, ReviewChange::Moved, vec![], None),
                    self.before,
                    b,
                )
            });
        }
        let diff = match (
            parts.contains(&PartName::Text),
            self.before.reader_text(id),
            self.after.reader_text(id),
        ) {
            (true, Some(old), Some(new)) => short_diff(old, new, &b.block_type, &a.block_type),
            _ => None,
        };
        let updated = row(
            self.after,
            a,
            ReviewChange::Updated,
            what_of(&parts, place),
            diff,
        );
        Some(match place {
            true => left_section(updated, self.before, b),
            false => updated,
        })
    }

    /// The counts of a doc's rows. Sections and figures count as blocks too; figures count by
    /// attachment image, light and dark each.
    fn counts(&self, rows: &[ChangeRow]) -> ReviewCounts {
        let mut c = ReviewCounts::default();
        for r in rows {
            count_row(
                &mut c,
                self.before,
                self.after,
                &r.block_type,
                &r.id,
                r.change,
            );
        }
        c
    }

    /// The parts of `id` that differ between the sides.
    fn changed(&self, id: &BlockId) -> Vec<PartName> {
        let none = BlockParts::default();
        changed_parts(
            self.before.parts(id).unwrap_or(&none),
            self.after.parts(id).unwrap_or(&none),
        )
    }

    /// The row of a block outside docs that the comparison lists (D-BND1J, D-0C4TB). A decision
    /// compares its title, statement and status, and is `added` or `removed` when it enters or
    /// leaves the content outside docs. Another block a reader sees compares the part table with
    /// sibling place left out; it took a new place only when it moved in from a doc, and leaving
    /// for a doc is `removed`. A version change with no changed part, or of a block no reader
    /// sees, keeps its row with `change` `None`, an empty `what` and `record_only` true. A
    /// decision row with a change carries its decision fields ([`decision_row`]).
    fn outside(&self, id: &BlockId) -> OutsideReview {
        let (b, a) = (self.before.shown(id), self.after.shown(id));
        let (bo, ao) = (
            b.filter(|x| x.home_doc.is_none()),
            a.filter(|x| x.home_doc.is_none()),
        );
        let block_type = a.or(b).map_or_else(String::new, |s| s.block_type.clone());
        let counted = block_type == decision::DECISION
            || (visible_type(&block_type) && !tree::is_tree_type(&block_type));
        let (change, what) = match (bo, ao) {
            _ if !counted => (None, vec![]),
            (Some(_), Some(_)) => {
                let parts = self.changed(id);
                match parts.is_empty() {
                    true => (None, vec![]),
                    false => (Some(ReviewChange::Updated), what_of(&parts, false)),
                }
            }
            (None, Some(_))
                if block_type != decision::DECISION && content(self.before, id).is_some() =>
            {
                let parts = self.changed(id);
                match parts.is_empty() {
                    true => (Some(ReviewChange::Moved), vec![]),
                    false => (Some(ReviewChange::Updated), what_of(&parts, true)),
                }
            }
            (None, Some(_)) => (Some(ReviewChange::Added), vec![]),
            (Some(_), None) => (Some(ReviewChange::Removed), vec![]),
            (None, None) => (None, vec![]),
        };
        // a removed block reads the baseline's label; every other row the candidate's
        let shown = match change {
            Some(ReviewChange::Removed) => b.or(a),
            _ => a.or(b),
        };
        let decision = change
            .filter(|_| block_type == decision::DECISION)
            .and_then(|c| decision_row(self.before, self.after, id, c));
        OutsideReview {
            id: id.clone(),
            block_type,
            label: shown.map_or_else(String::new, |s| s.label.clone()),
            record_only: change.is_none(),
            change,
            what,
            decision,
        }
    }

    /// What happened to folder `f` (D-0C4TB): `added` or `removed` when one side selects it;
    /// else `updated` when its title, name or description changed, `moved` when it took a new
    /// place among its siblings, and `None` when neither changed (a new path alone).
    fn folder(&self, f: &ChangedFolder) -> Option<ReviewChange> {
        match (f.before_version, f.after_version) {
            (None, Some(_)) => Some(ReviewChange::Added),
            (Some(_), None) => Some(ReviewChange::Removed),
            (None, None) => None,
            (Some(_), Some(_)) => {
                let (b, a) = (self.before.shown(&f.id)?, self.after.shown(&f.id)?);
                if !self.changed(&f.id).is_empty() {
                    Some(ReviewChange::Updated)
                } else {
                    self.moved(b, a).then_some(ReviewChange::Moved)
                }
            }
        }
    }
}

/// The rows outside docs of `diff` (D-BND1J, D-0C4TB), one for each version change the
/// comparison lists, in its id order. `before` and `after` are its two sides, each viewed with
/// the removals of both. A block that moves between a doc and the content outside docs gets its
/// one counted row at its new place: here for a move out of a doc, in the doc for a move in. A
/// decision has no `moved` kind: its move gives a removed row and an added row.
pub fn outside_reviews(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> Vec<OutsideReview> {
    let sides = Sides {
        before,
        after,
        places: RefCell::new(HashMap::new()),
    };
    diff.changed_outside_docs
        .iter()
        .map(|o| sides.outside(&o.id))
        .collect()
}

/// What happened to each changed folder of `diff`, in its id order (D-0C4TB): see
/// [`Sides::folder`].
pub(super) fn folder_changes(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> Vec<Option<ReviewChange>> {
    let sides = Sides {
        before,
        after,
        places: RefCell::new(HashMap::new()),
    };
    diff.changed_folders
        .iter()
        .map(|f| sides.folder(f))
        .collect()
}

/// Adds one row of block `id` with type `block_type` to `c`. A decision counts in the decision
/// counts only. Another block counts once by its kind; a section counts as a section too, and a
/// figure by attachment image, light and dark each: an updated figure counts each image whose
/// blob or availability changed, and each image it gained or lost.
pub(super) fn count_row(
    c: &mut ReviewCounts,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
    block_type: &str,
    id: &BlockId,
    change: ReviewChange,
) {
    if block_type == decision::DECISION {
        match change {
            ReviewChange::Added => c.decisions_added += 1,
            ReviewChange::Removed => c.decisions_removed += 1,
            ReviewChange::Updated => c.decisions_updated += 1,
            ReviewChange::Moved => {}
        }
        return;
    }
    match change {
        ReviewChange::Added => c.added += 1,
        ReviewChange::Removed => c.removed += 1,
        ReviewChange::Updated => c.updated += 1,
        ReviewChange::Moved => c.moved += 1,
    }
    match (block_type, change) {
        ("section", ReviewChange::Added) => c.sections_added += 1,
        ("section", ReviewChange::Removed) => c.sections_removed += 1,
        ("figure", ReviewChange::Added) => c.figures_added += images(after, id),
        ("figure", ReviewChange::Removed) => c.figures_removed += images(before, id),
        ("figure", ReviewChange::Updated) => {
            let (old, new) = (image_map(before, id), image_map(after, id));
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

/// The nearest section of block `s` on `side`, as that side shows it: `None` before the first
/// section.
fn section_of<'s>(side: &'s dyn PublicationReviewSide, s: &ShownBlock) -> Option<&'s ShownBlock> {
    s.section.as_ref().and_then(|x| side.shown(x))
}

/// `r` with the section that held its block `b` on the baseline `before` (D-EGFYD, D-9PG33): its
/// label and id, `None` before the first section. A removed section reads "Content removed." and
/// keeps its id.
fn left_section(mut r: ChangeRow, before: &dyn PublicationReviewSide, b: &ShownBlock) -> ChangeRow {
    let old = section_of(before, b);
    r.before_section = old.map(|x| x.label.clone());
    r.before_section_id = old.map(|x| x.id.clone());
    r
}

/// One change row of block `s`, read on `side`: the candidate's, or the baseline's for a removed
/// block. A removed version reads "Content removed." and has no excerpt and no diff. The row
/// names the section that held the block before only when it moved ([`left_section`]).
fn row(
    side: &dyn PublicationReviewSide,
    s: &ShownBlock,
    change: ReviewChange,
    what: Vec<String>,
    diff: Option<Vec<DiffPiece>>,
) -> ChangeRow {
    let section = section_of(side, s);
    ChangeRow {
        id: s.id.clone(),
        block_type: s.block_type.clone(),
        change,
        label: s.label.clone(),
        section: section.map(|x| x.label.clone()),
        section_id: section.map(|x| x.id.clone()),
        before_section: None,
        before_section_id: None,
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
