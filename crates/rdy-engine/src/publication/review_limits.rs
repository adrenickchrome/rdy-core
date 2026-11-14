//! The totals and the bounds of the publication review (D-RDH1C, D-0C4TB, D-BND1J). The totals
//! cover the complete comparison and are worked out before any list is cut. The changed docs
//! share one budget of change rows ([`change_shares`]); decisions stay outside it. One row model
//! ([`DisclosureReview`]) serves two projections ([`ReviewProjection`]): the report of chat and
//! the CLI cuts each top-level list to [`REPORT_ROWS`] rows and holds at most [`REPORT_BYTES`];
//! the adapter keeps every outer row and holds at most [`ADAPTER_REVIEW_BYTES`]. Both measure the
//! compact UTF-8 JSON while they write it and stop at the first byte over the limit, before the
//! full value is allocated ([`compact_len`]).
//!
//! Every block counts once in the totals. An ordinary block that moves to another home counts at
//! its new home only: the removed row its old place lists, in a changed doc, in a removed doc's
//! size or outside docs, leaves the totals. A decision has no `moved` kind, so a decision that
//! moves counts as both removed and added. An added or removed doc or folder counts as one block,
//! and each block shown inside a doc counts too. No total enters a hash (D-B2AC3).

use super::diff::DisclosureDiff;
use super::report::{REPORT_BYTES, REPORT_ROWS};
use super::review_changes::{
    changed_doc_reviews, content, count_row, folder_changes, outside_reviews,
};
use super::review_docs::{doc_reviews, sized_blocks};
use super::review_labels::{attachment_labels, folder_reviews};
use super::review_model::{
    ChangedDocReview, DisclosureReview, DisclosureTotals, DocSize, Included, ReviewChange,
    ReviewCounts,
};
use super::review_side::PublicationReviewSide;
use crate::error::*;
use rdy_model::BlockId;
use serde::Serialize;
use std::io;

/// The most change rows one changed doc lists (D-RDH1C).
pub const CHANGES_MAX: usize = 100;

/// The most change rows all the changed docs list together (D-RDH1C).
pub const CHANGE_BUDGET: usize = 2000;

/// The most bytes the adapter's serialized review may hold (D-RDH1C).
pub const ADAPTER_REVIEW_BYTES: usize = 134_217_728;

/// The limits one surface applies to the one row model (D-RDH1C). Both share the inner limits
/// and the change budget; they differ in their outer lists and their byte limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewProjection {
    /// Chat and the CLI: each top-level list keeps its first [`REPORT_ROWS`] rows, and the
    /// report holds at most [`REPORT_BYTES`].
    Report,
    /// The adapter: every outer row stays, and the review holds at most
    /// [`ADAPTER_REVIEW_BYTES`].
    Adapter,
}

impl ReviewProjection {
    /// The most rows one top-level list keeps: `None` keeps them all.
    pub fn outer_rows(self) -> Option<usize> {
        match self {
            ReviewProjection::Report => Some(REPORT_ROWS),
            ReviewProjection::Adapter => None,
        }
    }

    /// The most bytes the serialized value may hold.
    pub fn max_bytes(self) -> usize {
        match self {
            ReviewProjection::Report => REPORT_BYTES,
            ReviewProjection::Adapter => ADAPTER_REVIEW_BYTES,
        }
    }

    /// The rows a top-level list keeps of `rows`, in their order, and how many it leaves out.
    pub fn list<T>(self, rows: &[T]) -> (&[T], u64) {
        let kept = self
            .outer_rows()
            .map_or(rows.len(), |max| rows.len().min(max));
        (&rows[..kept], (rows.len() - kept) as u64)
    }

    /// `included` with its rows cut as a top-level list: `omitted_rows` adds the rows left out;
    /// the counts and totals still cover every selected doc.
    pub fn included(self, included: &Included) -> Included {
        let (rows, omitted) = self.list(&included.rows);
        Included {
            docs: included.docs,
            folders: included.folders,
            rows: rows.to_vec(),
            omitted_rows: included.omitted_rows + omitted,
            totals: included.totals,
        }
    }

    /// `value` when its compact JSON, with `reserve` bytes kept for what the caller adds, fits
    /// [`ReviewProjection::max_bytes`]; else `E_LIMIT`, and no part of the value. The size check
    /// stops at the first byte over the limit, so it never allocates the serialized value.
    pub fn fit<T: Serialize>(self, value: T, reserve: usize) -> Result<T, RdyError> {
        fit_within(value, self.max_bytes(), reserve)
    }
}

/// `value` when its compact JSON, with `reserve` bytes kept for what the caller adds, fits `max`
/// bytes; else `E_LIMIT` naming `max`, and no part of the value.
pub fn fit_within<T: Serialize>(value: T, max: usize, reserve: usize) -> Result<T, RdyError> {
    match max
        .checked_sub(reserve)
        .and_then(|room| compact_len(&value, room))
    {
        Some(_) => Ok(value),
        None => Err(RdyError::new(
            E_LIMIT,
            format!("Publication report exceeds the limit of {max} bytes."),
            "Reduce the selection or compare smaller profiles.",
        )),
    }
}

/// A sink that counts the bytes written to it and refuses the write that passes `max`.
struct Bounded {
    len: usize,
    max: usize,
}

impl io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.len.checked_add(buf.len()).filter(|n| *n <= self.max) {
            Some(n) => {
                self.len = n;
                Ok(buf.len())
            }
            None => Err(io::Error::other("over the limit")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The size of `value` as compact UTF-8 JSON, escapes included, when it is at most `max` bytes;
/// `None` when it is larger or does not serialize. Serialization stops at the first byte over
/// `max`, and no byte is kept.
pub fn compact_len<T: Serialize + ?Sized>(value: &T, max: usize) -> Option<usize> {
    let mut sink = Bounded { len: 0, max };
    serde_json::to_writer(&mut sink, value)
        .ok()
        .map(|()| sink.len)
}

/// Each changed doc's share of the change budget, from its complete row count, in site order
/// (D-RDH1C). When every doc's rows fit, up to [`CHANGES_MAX`] each, each doc keeps them.
/// Otherwise each doc keeps up to the largest even share `k` from 0 through [`CHANGES_MAX`] for
/// which the shares fit [`CHANGE_BUDGET`], and the spare rows go one at a time to the docs with
/// rows left, in site order.
pub fn change_shares(rows: &[usize]) -> Vec<usize> {
    let capped: Vec<usize> = rows.iter().map(|r| (*r).min(CHANGES_MAX)).collect();
    let used = |k: usize| capped.iter().map(|c| (*c).min(k)).sum::<usize>();
    if used(CHANGES_MAX) <= CHANGE_BUDGET {
        return capped;
    }
    let k = (0..CHANGES_MAX)
        .rev()
        .find(|k| used(*k) <= CHANGE_BUDGET)
        .unwrap_or(0);
    let mut shares: Vec<usize> = capped.iter().map(|c| (*c).min(k)).collect();
    // fewer spare rows than docs with rows left, since the share k + 1 does not fit
    let mut spare = CHANGE_BUDGET - used(k);
    for (share, c) in shares.iter_mut().zip(&capped) {
        if spare == 0 {
            break;
        }
        if *c > *share {
            *share += 1;
            spare -= 1;
        }
    }
    shares
}

/// The review of `diff`, whose sides `before` and `after` are viewed with the removals of both:
/// the totals over the complete comparison, then every changed, added and removed doc row and
/// every row outside docs. Each changed doc keeps its share of the change budget, in page order,
/// and counts the rest in `omitted_changes`; its counts stay complete. No outer list is cut
/// here: a projection cuts them ([`ReviewProjection::list`]).
pub fn disclosure_review(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> DisclosureReview {
    let mut changed_docs = changed_doc_reviews(diff, before, after);
    let docs = doc_reviews(Some((diff, before)), after);
    let changed_outside_docs = outside_reviews(diff, before, after);
    let mut t = Totals {
        before,
        after,
        counts: ReviewCounts::default(),
        moved_away: ReviewCounts::default(),
    };
    for d in &changed_docs {
        t.add(&d.counts);
        for r in &d.changes {
            t.removed_row(&r.block_type, &r.id, r.change);
        }
    }
    for o in &changed_outside_docs {
        if let Some(change) = o.change {
            count_row(&mut t.counts, before, after, &o.block_type, &o.id, change);
            t.removed_row(&o.block_type, &o.id, change);
        }
    }
    for change in folder_changes(diff, before, after).into_iter().flatten() {
        t.unit(change);
    }
    for d in &docs.added_docs {
        t.unit(ReviewChange::Added);
        t.size(ReviewChange::Added, &d.size);
    }
    for d in &docs.removed_docs {
        t.unit(ReviewChange::Removed);
        t.size(ReviewChange::Removed, &d.size);
        for id in sized_blocks(before, &d.id) {
            if let Some(b) = before.shown(&id) {
                t.removed_row(&b.block_type, &id, ReviewChange::Removed);
            }
        }
    }
    let totals = t.totals(diff, &changed_docs);
    share_changes(&mut changed_docs);
    let attachments = attachment_labels(diff, before, after);
    DisclosureReview {
        totals,
        changed_docs,
        added_docs: docs.added_docs,
        removed_docs: docs.removed_docs,
        changed_outside_docs,
        changed_folders: folder_reviews(diff, before, after),
        added_attachments: attachments.added,
        removed_attachments: attachments.removed,
        changed_attachments: attachments.changed,
    }
}

/// Cuts each changed doc's change rows to its share of the change budget, in site order, and
/// adds the rows it withholds to `omitted_changes`.
fn share_changes(docs: &mut [ChangedDocReview]) {
    let rows: Vec<usize> = docs.iter().map(|d| d.changes.len()).collect();
    for (d, share) in docs.iter_mut().zip(change_shares(&rows)) {
        d.omitted_changes += (d.changes.len() - share) as u64;
        d.changes.truncate(share);
    }
}

/// The totals of one comparison while they are summed: every row's counts, and apart the counts
/// of the removed rows of ordinary blocks that moved to another home, which leave the totals.
struct Totals<'s> {
    before: &'s dyn PublicationReviewSide,
    after: &'s dyn PublicationReviewSide,
    counts: ReviewCounts,
    moved_away: ReviewCounts,
}

impl Totals<'_> {
    /// Adds the counts of one changed doc.
    fn add(&mut self, c: &ReviewCounts) {
        let t = &mut self.counts;
        t.added += c.added;
        t.removed += c.removed;
        t.updated += c.updated;
        t.moved += c.moved;
        t.sections_added += c.sections_added;
        t.sections_removed += c.sections_removed;
        t.figures_added += c.figures_added;
        t.figures_removed += c.figures_removed;
        t.figures_changed += c.figures_changed;
        t.decisions_added += c.decisions_added;
        t.decisions_removed += c.decisions_removed;
        t.decisions_updated += c.decisions_updated;
    }

    /// Notes a counted row of block `id` as a move when it is a removed ordinary block that the
    /// candidate still shows a reader: the block counts at its new home.
    fn removed_row(&mut self, block_type: &str, id: &BlockId, change: ReviewChange) {
        if change == ReviewChange::Removed && content(self.after, id).is_some() {
            count_row(
                &mut self.moved_away,
                self.before,
                self.after,
                block_type,
                id,
                change,
            );
        }
    }

    /// Counts one doc or folder as one block of `change`.
    fn unit(&mut self, change: ReviewChange) {
        let t = &mut self.counts;
        match change {
            ReviewChange::Added => t.added += 1,
            ReviewChange::Removed => t.removed += 1,
            ReviewChange::Updated => t.updated += 1,
            ReviewChange::Moved => t.moved += 1,
        }
    }

    /// Counts the contents of an added or removed doc.
    fn size(&mut self, change: ReviewChange, s: &DocSize) {
        let t = &mut self.counts;
        match change {
            ReviewChange::Added => {
                t.added += s.blocks;
                t.sections_added += s.sections;
                t.figures_added += s.figures;
                t.decisions_added += s.decisions;
            }
            _ => {
                t.removed += s.blocks;
                t.sections_removed += s.sections;
                t.figures_removed += s.figures;
                t.decisions_removed += s.decisions;
            }
        }
    }

    /// The totals, each moved block counted once.
    fn totals(&self, diff: &DisclosureDiff, changed: &[ChangedDocReview]) -> DisclosureTotals {
        let (c, m) = (&self.counts, &self.moved_away);
        DisclosureTotals {
            docs_changed: changed.len() as u64,
            docs_added: diff.added_docs.len() as u64,
            docs_removed: diff.removed_docs.len() as u64,
            docs_record_only: changed.iter().filter(|d| d.record_only).count() as u64,
            blocks_added: c.added,
            blocks_removed: c.removed.saturating_sub(m.removed),
            blocks_updated: c.updated,
            blocks_moved: c.moved,
            sections_added: c.sections_added,
            sections_removed: c.sections_removed.saturating_sub(m.sections_removed),
            figures_added: c.figures_added,
            figures_removed: c.figures_removed.saturating_sub(m.figures_removed),
            figures_changed: c.figures_changed,
            decisions_added: c.decisions_added,
            decisions_removed: c.decisions_removed,
            decisions_updated: c.decisions_updated,
            new_references: diff.new_reference_count,
        }
    }
}
