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

use super::diff::DisclosureDiff;
use super::review_changes::excerpt;
use super::review_labels::figure_label;
use super::review_model::{
    DocSize, DocSummary, FigureEntry, Included, IncludedTotals, RemovedDocReview,
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
        Some((diff, before)) => DocReviews {
            included: None,
            added_docs: in_reading_order(after, diff.added_docs.iter().map(|d| &d.id))
                .into_iter()
                .map(|id| doc_summary(after, id))
                .collect(),
            removed_docs: in_reading_order(before, diff.removed_docs.iter().map(|d| &d.id))
                .into_iter()
                .map(|id| removed_doc(before, id))
                .collect(),
        },
    }
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
    let rows: Vec<DocSummary> = of_type(tree::DOC).map(|id| doc_summary(side, id)).collect();
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

/// A removed doc as the baseline `side` shows it: its title, page and size.
fn removed_doc(side: &dyn PublicationReviewSide, doc: &BlockId) -> RemovedDocReview {
    let entry = side.tree_entry(doc);
    RemovedDocReview {
        id: doc.clone(),
        title: entry.and_then(|e| e.title.clone()),
        before_page: entry.and_then(|e| e.page.clone()),
        size: Contents::of(side, doc).size(),
    }
}

/// What `doc` shows on `side` in the added-doc form: its title, page and top folder, its size,
/// its lede, and its outline, decision titles and figure rows, each cut to its limit with the
/// rest counted.
pub fn doc_summary(side: &dyn PublicationReviewSide, doc: &BlockId) -> DocSummary {
    let entry = side.tree_entry(doc);
    let c = Contents::of(side, doc);
    let size = c.size();
    let (outline, omitted_outline) = capped(c.outline, OUTLINE_MAX);
    let (decision_titles, omitted_decision_titles) = capped(c.decisions, DECISION_TITLES_MAX);
    let (figures, omitted_figures) = capped(c.figures, FIGURES_MAX);
    DocSummary {
        id: doc.clone(),
        title: entry.and_then(|e| e.title.clone()),
        page: entry.and_then(|e| e.page.clone()),
        folder: top_folder(side, doc),
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
fn capped<T>(mut all: Vec<T>, max: usize) -> (Vec<T>, u64) {
    let omitted = all.len().saturating_sub(max) as u64;
    all.truncate(max);
    (all, omitted)
}

/// The title of the topmost folder above `doc` on `side`: `None` for a root doc, or when a
/// removal removed that folder's title.
fn top_folder(side: &dyn PublicationReviewSide, doc: &BlockId) -> Option<String> {
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
    top.and_then(|e| e.title.clone())
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
    outline: Vec<String>,
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
                    c.outline.push(b.label.clone());
                }
                "figure" => {
                    figures.push(id);
                    texts.push(id);
                }
                _ => texts.push(id),
            }
            stack.extend(side.children(Some(id)).iter().rev());
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
