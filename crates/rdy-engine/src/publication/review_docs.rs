//! The doc summaries of the publication review (D-RDH1C, D-0C4TB, D-MCH70, D-TBDA1): what an
//! added doc, a removed doc and each doc of a first publication show a reviewer. Each summary
//! reads its own side through the side-view seam ([`PublicationReviewSide`]): an added or
//! included doc reads the candidate, and a removed doc reads the baseline.
//!
//! A doc's size counts each block its side shows below it, at any depth (D-0C4TB). Sections and
//! figures count as blocks too. The doc itself and its decisions do not. Figures count by
//! attachment image, light and dark each. The outline, the decision titles and the figure rows
//! keep at most [`OUTLINE_MAX`], [`DECISION_TITLES_MAX`] and [`FIGURES_MAX`] entries, and count
//! each entry they leave out. Rows follow their side's reading order; a doc with no page keeps
//! its row and its place.
//!
//! A first publication (no previous publication) lists every selected doc in the added-doc form
//! ([`Included`]). An empty previous selection is still a comparison: it has added rows and no
//! included list.
//!
//! Each doc row gives its top folder's title and whether a removal removed it (D-5WDC2,
//! D-62KA3), and its place in one reading order of doc rows ([`doc_positions`], D-EGFYD). Each
//! outline row gives its section's id and label, the blocks whose nearest section it is, and the
//! changes in it (D-EGFYD, D-9PG33): in an added or included doc each block is added, so
//! `changes` equals `blocks`.

use super::diff::DisclosureDiff;
use super::review_changes::excerpt;
use super::review_labels::figure_label;
use super::review_model::{
    DocSize, DocSummary, FigureEntry, Included, IncludedTotals, OutlineRow, RemovedDocReview,
};
use super::review_side::PublicationReviewSide;
use crate::reader::review::visible_type;
use rdy_model::{BlockId, decision, tree};
use std::collections::{BTreeMap, BTreeSet};

/// The most outline entries an added or included doc lists (D-RDH1C).
pub const OUTLINE_MAX: usize = 30;

/// The most decision titles an added or included doc lists (D-RDH1C).
pub const DECISION_TITLES_MAX: usize = 50;

/// The most attachment-image rows an added or included doc lists, light and dark each
/// (D-RDH1C).
pub const FIGURES_MAX: usize = 50;

/// The doc summaries of one review. `included` is set only for a first publication. `added_docs`
/// and `removed_docs` are set only for a comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocReviews {
    pub included: Option<Included>,
    pub added_docs: Vec<DocSummary>,
    pub removed_docs: Vec<RemovedDocReview>,
}

/// The doc summaries of a review of `after`. With `against` (the comparison and its baseline
/// side), the added docs read the candidate and the removed docs read the baseline, and there is
/// no included list, even when the baseline selects nothing. Without it, the review is a first
/// publication: every selected doc is an included row.
pub fn doc_reviews(
    against: Option<(&DisclosureDiff, &dyn PublicationReviewSide)>,
    after: &dyn PublicationReviewSide,
) -> DocReviews {
    match against {
        None => DocReviews {
            included: Some(included(after)),
            added_docs: vec![],
            removed_docs: vec![],
        },
        Some((diff, before)) => {
            let positions = doc_positions(diff, before, after);
            let place = |id: &BlockId| positions.get(id).copied().unwrap_or_default();
            DocReviews {
                included: None,
                added_docs: in_reading_order(after, diff.added_docs.iter().map(|d| &d.id))
                    .into_iter()
                    .map(|id| DocSummary {
                        position: place(id),
                        ..doc_summary(after, id)
                    })
                    .collect(),
                removed_docs: in_reading_order(before, diff.removed_docs.iter().map(|d| &d.id))
                    .into_iter()
                    .map(|id| RemovedDocReview {
                        position: place(id),
                        ..removed_doc(before, id)
                    })
                    .collect(),
            }
        }
    }
}

/// The place of each changed, added and removed doc of `diff` in one reading order (D-EGFYD),
/// counted from 0 over those rows only. The order is the candidate's page order, with each
/// removed doc directly after the doc before it in the baseline's page order that the candidate
/// still holds; a removed doc with no such doc comes first, and removed docs that share a place
/// keep the baseline's order. A row whose doc a side's reading order lacks comes last, by id.
/// Every row counts, also the rows a cut leaves out.
pub fn doc_positions(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> BTreeMap<BlockId, u64> {
    let docs = |side: &'_ dyn PublicationReviewSide| -> Vec<BlockId> {
        side.reading()
            .iter()
            .filter(|id| {
                side.tree_entry(id)
                    .is_some_and(|e| e.block_type == tree::DOC)
            })
            .cloned()
            .collect()
    };
    let after_docs = docs(after);
    let held: BTreeSet<&BlockId> = after_docs.iter().collect();
    let removed: BTreeSet<&BlockId> = diff.removed_docs.iter().map(|d| &d.id).collect();
    // each removed doc under the candidate doc it follows, in the baseline's order
    let mut follows: BTreeMap<Option<BlockId>, Vec<BlockId>> = BTreeMap::new();
    let mut anchor: Option<BlockId> = None;
    for id in docs(before) {
        if removed.contains(&id) {
            follows.entry(anchor.clone()).or_default().push(id);
        } else if held.contains(&id) {
            anchor = Some(id);
        }
    }
    let mut order: Vec<BlockId> = follows.remove(&None).unwrap_or_default();
    for id in after_docs.iter() {
        order.push(id.clone());
        order.extend(follows.remove(&Some(id.clone())).unwrap_or_default());
    }
    let rows: BTreeSet<&BlockId> = diff
        .changed_docs
        .iter()
        .map(|d| &d.id)
        .chain(diff.added_docs.iter().map(|d| &d.id))
        .chain(removed.iter().copied())
        .collect();
    let mut placed: Vec<&BlockId> = order.iter().filter(|id| rows.contains(id)).collect();
    let listed: BTreeSet<&BlockId> = placed.iter().copied().collect();
    placed.extend(rows.iter().copied().filter(|id| !listed.contains(id)));
    placed
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as u64))
        .collect()
}

/// A first publication's review of `side`: every selected doc in the added-doc form, in reading
/// order, and totals that sum the rows' sizes (D-0C4TB). `docs` and `folders` count the selected
/// docs and folders and equal their totals (D-MCH70). No row is cut here.
pub fn included(side: &dyn PublicationReviewSide) -> Included {
    let of_type = |t: &'static str| {
        side.reading()
            .iter()
            .filter(move |id| side.tree_entry(id).is_some_and(|e| e.block_type == t))
    };
    let rows: Vec<DocSummary> = of_type(tree::DOC)
        .enumerate()
        .map(|(i, id)| DocSummary {
            position: i as u64,
            ..doc_summary(side, id)
        })
        .collect();
    let mut totals = IncludedTotals {
        docs: rows.len() as u64,
        folders: of_type(tree::FOLDER).count() as u64,
        ..IncludedTotals::default()
    };
    for r in &rows {
        totals.sections += r.size.sections;
        totals.blocks += r.size.blocks;
        totals.decisions += r.size.decisions;
        totals.figures += r.size.figures;
    }
    Included {
        docs: totals.docs,
        folders: totals.folders,
        rows,
        omitted_rows: 0,
        totals,
    }
}

/// `ids` in `side`'s reading order: depth first, siblings by their order keys, a doc with no
/// page in its place. An id the reading order lacks comes last, by id.
fn in_reading_order<'a>(
    side: &dyn PublicationReviewSide,
    ids: impl Iterator<Item = &'a BlockId>,
) -> Vec<&'a BlockId> {
    let place: BTreeMap<&BlockId, usize> = side
        .reading()
        .iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let mut ids: Vec<&BlockId> = ids.collect();
    ids.sort_by_key(|id| (place.get(id).copied().unwrap_or(usize::MAX), *id));
    ids
}

/// A removed doc as the baseline `side` shows it: its title, page, top folder and size. Its
/// position is 0 until its review places it ([`doc_positions`]).
fn removed_doc(side: &dyn PublicationReviewSide, doc: &BlockId) -> RemovedDocReview {
    let entry = side.tree_entry(doc);
    let (folder, folder_redacted) = top_folder(side, doc);
    RemovedDocReview {
        id: doc.clone(),
        title: entry.and_then(|e| e.title.clone()),
        before_page: entry.and_then(|e| e.page.clone()),
        folder,
        folder_redacted,
        position: 0,
        size: Contents::of(side, doc).size(),
    }
}

/// What `doc` shows on `side` in the added-doc form: its title, page and top folder, its size,
/// its lede, and its outline, decision titles and figure rows, each cut to its limit with the
/// rest counted. Each outline row counts each of its blocks as a change. Its position is 0 until
/// its review places it ([`doc_positions`]).
pub fn doc_summary(side: &dyn PublicationReviewSide, doc: &BlockId) -> DocSummary {
    let entry = side.tree_entry(doc);
    let c = Contents::of(side, doc);
    let size = c.size();
    let added = c
        .outline
        .into_iter()
        .map(|r| OutlineRow {
            changes: r.blocks,
            ..r
        })
        .collect();
    let (outline, omitted_outline) = capped(added, OUTLINE_MAX);
    let (decision_titles, omitted_decision_titles) = capped(c.decisions, DECISION_TITLES_MAX);
    let (figures, omitted_figures) = capped(c.figures, FIGURES_MAX);
    let (folder, folder_redacted) = top_folder(side, doc);
    DocSummary {
        id: doc.clone(),
        title: entry.and_then(|e| e.title.clone()),
        page: entry.and_then(|e| e.page.clone()),
        folder,
        folder_redacted,
        position: 0,
        size,
        lede: c.lede,
        outline,
        omitted_outline,
        decision_titles,
        omitted_decision_titles,
        figures,
        omitted_figures,
    }
}

/// The first `max` of `all`, and how many it leaves out.
pub(super) fn capped<T>(mut all: Vec<T>, max: usize) -> (Vec<T>, u64) {
    let omitted = all.len().saturating_sub(max) as u64;
    all.truncate(max);
    (all, omitted)
}

/// The title of the topmost folder above `doc` on `side`, and whether a removal removed it
/// (D-62KA3): `(None, false)` for a root doc, `(None, true)` when a removal removed that
/// folder's title.
pub(super) fn top_folder(
    side: &dyn PublicationReviewSide,
    doc: &BlockId,
) -> (Option<String>, bool) {
    let mut top = None;
    let mut cur = side.tree_entry(doc).and_then(|e| e.parent.as_ref());
    // each step climbs one selected ancestor, so a damaged parent chain still ends
    for _ in 0..side.reading().len() {
        let Some(e) = cur.and_then(|p| side.tree_entry(p)) else {
            break;
        };
        if e.block_type == tree::FOLDER {
            top = Some(e);
        }
        cur = e.parent.as_ref();
    }
    match top {
        Some(e) => (e.title.clone(), e.redacted),
        None => (None, false),
    }
}

/// The complete outline of `doc` on `side`, in page order, before any cut: one row for each
/// section, with the blocks whose nearest section it is counted and `changes` at 0.
pub(super) fn outline_rows(side: &dyn PublicationReviewSide, doc: &BlockId) -> Vec<OutlineRow> {
    Contents::of(side, doc).outline
}

/// The blocks a doc's size counts on `side` ([`DocSize::blocks`]): each block it shows below
/// it, at any depth, in page order. Sections and figures are among them; decisions are not.
pub(super) fn sized_blocks(side: &dyn PublicationReviewSide, doc: &BlockId) -> Vec<BlockId> {
    Contents::of(side, doc).blocks
}

/// Everything `doc` shows below it on one side, in page order: the full lists, before any cut.
struct Contents {
    /// The blocks the size counts.
    blocks: Vec<BlockId>,
    sections: u64,
    /// One row for each section, with its blocks counted and no change counted yet.
    outline: Vec<OutlineRow>,
    decisions: Vec<String>,
    figures: Vec<FigureEntry>,
    lede: Option<String>,
}

impl Contents {
    /// Every block `side` shows below `doc`, depth first in page order. A walk goes no deeper
    /// than a block off the page (a decision's rationale is no content) or another doc or
    /// folder, and visits each block once.
    fn of(side: &dyn PublicationReviewSide, doc: &BlockId) -> Contents {
        let mut c = Contents {
            blocks: vec![],
            sections: 0,
            outline: vec![],
            decisions: vec![],
            figures: vec![],
            lede: None,
        };
        let mut texts: Vec<&BlockId> = vec![];
        let mut figures: Vec<&BlockId> = vec![];
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&BlockId> = side.children(Some(doc)).iter().rev().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(b) = side.shown(id) else { continue };
            let ty = b.block_type.as_str();
            if ty == decision::DECISION {
                if let Some(d) = side.decision(id) {
                    c.decisions.push(d.title.clone());
                }
                continue;
            }
            if tree::is_tree_type(ty) || !visible_type(ty) {
                continue;
            }
            c.blocks.push(id.clone());
            match ty {
                "section" => {
                    c.sections += 1;
                    c.outline.push(OutlineRow {
                        id: id.clone(),
                        label: b.label.clone(),
                        blocks: 0,
                        changes: 0,
                    });
                }
                "figure" => {
                    figures.push(id);
                    texts.push(id);
                }
                _ => texts.push(id),
            }
            stack.extend(side.children(Some(id)).iter().rev());
        }
        // each block counts in the row of its nearest section; a nested section counts in its
        // parent's row, and a block before the first section in none
        let rows: BTreeMap<BlockId, usize> = c
            .outline
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id.clone(), i))
            .collect();
        for id in &c.blocks {
            let section = side.shown(id).and_then(|b| b.section.as_ref());
            if let Some(i) = section.and_then(|s| rows.get(s)) {
                c.outline[*i].blocks += 1;
            }
        }
        // the page's lede first, else the first block a reader reads after the title
        let lede = side.tree_entry(doc).and_then(|e| e.lede.as_ref());
        c.lede = lede
            .into_iter()
            .chain(texts)
            .find_map(|id| side.reader_text(id).and_then(excerpt));
        // each image of each figure, in page order, light first
        let mut images: Vec<_> = side
            .selection()
            .attachments
            .iter()
            .filter(|a| a.home_doc == *doc)
            .collect();
        let place: BTreeMap<&BlockId, usize> =
            figures.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        images.sort_by_key(|a| {
            (
                place.get(&a.block).copied().unwrap_or(usize::MAX),
                &a.block,
                a.variant,
            )
        });
        c.figures = images
            .into_iter()
            .map(|a| FigureEntry {
                label: figure_label(side, &a.block),
                variant: a.variant,
            })
            .collect();
        c
    }

    /// The size these contents give: decisions apart, figures by image.
    fn size(&self) -> DocSize {
        DocSize {
            sections: self.sections,
            blocks: self.blocks.len() as u64,
            decisions: self.decisions.len() as u64,
            figures: self.figures.len() as u64,
        }
    }
}
