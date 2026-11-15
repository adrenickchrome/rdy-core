//! The decision rows and the labels of the publication review (D-9BWMV, D-0C4TB, D-RDH1C,
//! D-44NMT, D-7M0YF): what names each decision, folder, attachment and reference for a
//! reviewer.
//!
//! A decision row compares only the decision's authored title and statement and its derived
//! status tag, read through the side-view seam ([`PublicationReviewSide`]). A rationale or a
//! pin alone gives no row. A decision has no `moved` kind: one that leaves a doc, or moves
//! between a doc and the content outside docs, gives a `removed` row in its old place and an
//! `added` row in its new one. A move inside one doc gives no row. The excerpt reads the reader
//! statement (`reader::text::decision_statement`); a removed decision reads "Content removed."
//! and has no excerpt.
//!
//! A folder row gives the candidate's title, else the baseline's for a removed folder. An
//! attachment row gives its figure's label and its home doc's title from the side that holds
//! it. A reference row reads its labels from the captured candidate source
//! ([`ReferenceLookup`]): that lookup may name a target outside the selection for the
//! authorizer, and never widens the reader view. A missing target has no label. The report's
//! status, counts and policy stay as they are.

use super::diff::DisclosureDiff;
use super::references::{Classification, ReferenceReport, ReferenceRow};
use super::review_changes::{excerpt, walk};
use super::review_model::{
    AttachmentLabels, DecisionRow, DroppedDecision, DroppedDecisions, FolderReview,
    ReferenceLabels, ReferenceReview, ReviewChange,
};
use super::review_side::{
    BlockParts, DecisionFacts, PublicationReviewSide, ReferenceLookup, changed_parts,
};
use super::select::AttachmentEntry;
use crate::keys::Keys;
use rdy_model::reference::foreign_ref;
use rdy_model::{BlockId, decision, key};
use std::cell::OnceCell;
use std::collections::BTreeSet;

/// The most decision rows one changed doc lists (D-RDH1C). Decisions take no part of the change
/// budget.
pub const DECISIONS_MAX: usize = 50;

// ---- decisions ----------------------------------------------------------------------------

/// The decision rows of changed doc `doc`, complete: each decision the doc holds in the
/// candidate, in its page order, `added` when the doc did not hold it and `updated` when its
/// title, statement or status changed; then each decision the doc held only in the baseline,
/// `removed`, in the baseline's page order.
pub(super) fn doc_decisions(
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
    doc: &BlockId,
) -> Vec<DecisionRow> {
    let held = |side: &dyn PublicationReviewSide, id: &BlockId| {
        side.shown(id)
            .is_some_and(|s| s.block_type == decision::DECISION && s.home_doc.as_ref() == Some(doc))
    };
    let mut rows = vec![];
    for (id, _) in walk(after, doc) {
        if !held(after, id) {
            continue;
        }
        let change = match held(before, id) {
            true => ReviewChange::Updated,
            false => ReviewChange::Added,
        };
        rows.extend(decision_row(before, after, id, change));
    }
    for (id, _) in walk(before, doc) {
        if held(before, id) && !held(after, id) {
            rows.extend(decision_row(before, after, id, ReviewChange::Removed));
        }
    }
    rows
}

/// The row of decision `id` for `change`: none for an `updated` decision whose title,
/// statement and status are unchanged, or for a decision the side it reads does not show. An
/// `added` or `removed` row has an empty `what` and no status for the side that lacks it. A
/// removed row reads the baseline; every other row reads the candidate.
pub(super) fn decision_row(
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
    id: &BlockId,
    change: ReviewChange,
) -> Option<DecisionRow> {
    let (b, a) = (before.decision(id), after.decision(id));
    let what = match change {
        ReviewChange::Updated => {
            let none = BlockParts::default();
            let parts = changed_parts(
                before.parts(id).unwrap_or(&none),
                after.parts(id).unwrap_or(&none),
            );
            if parts.is_empty() {
                return None;
            }
            parts.iter().map(ToString::to_string).collect()
        }
        _ => vec![],
    };
    let shown = match change {
        ReviewChange::Removed => b?,
        _ => a?,
    };
    let status = |side: Option<&DecisionFacts>| side.and_then(|d| d.status.clone());
    Some(DecisionRow {
        id: id.clone(),
        title: shown.title.clone(),
        change,
        status_from: match change {
            ReviewChange::Added => None,
            _ => status(b),
        },
        status_to: match change {
            ReviewChange::Removed => None,
            _ => status(a),
        },
        what,
        excerpt: match shown.redacted {
            true => None,
            false => shown.text.as_deref().and_then(excerpt),
        },
    })
}

// ---- folders and attachments --------------------------------------------------------------

/// One folder review for each changed folder of `diff`, in its order (D-0C4TB): the
/// candidate's title, else the baseline's for a removed folder, and `before_title` when the
/// baseline's title differs. A title is `None` only when a removal removed it.
pub(super) fn folder_reviews(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> Vec<FolderReview> {
    diff.changed_folders
        .iter()
        .map(|f| {
            let old = f.before_version.and_then(|_| doc_title(before, &f.id));
            match f.after_version {
                None => FolderReview {
                    id: f.id.clone(),
                    title: old,
                    before_title: None,
                },
                Some(_) => {
                    let new = doc_title(after, &f.id);
                    FolderReview {
                        id: f.id.clone(),
                        before_title: old.filter(|t| new.as_ref() != Some(t)),
                        title: new,
                    }
                }
            }
        })
        .collect()
}

/// The label of figure `block` on `side`, for its attachment rows: its attachment label, else
/// its change label.
pub(super) fn figure_label(side: &dyn PublicationReviewSide, block: &BlockId) -> String {
    side.attachment_label(block)
        .or_else(|| side.shown(block).map(|b| b.label.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// The title of doc or folder `id` on `side`; `None` when a removal removed it or the side
/// does not select it.
fn doc_title(side: &dyn PublicationReviewSide, id: &BlockId) -> Option<String> {
    side.tree_entry(id).and_then(|t| t.title.clone())
}

/// The labels of attachment row `a`, read on `side`.
fn attachment(side: &dyn PublicationReviewSide, a: &AttachmentEntry) -> AttachmentLabels {
    AttachmentLabels {
        label: figure_label(side, &a.block),
        home_doc_title: doc_title(side, &a.home_doc),
        before_home_doc_title: None,
        after_home_doc_title: None,
    }
}

/// The labels of the attachment rows of `diff`: one for each added, removed and changed row,
/// in its order. An added row reads the candidate and a removed row the baseline. A changed
/// row gives the candidate's figure label and both home doc titles.
pub(super) struct AttachmentReview {
    pub added: Vec<AttachmentLabels>,
    pub removed: Vec<AttachmentLabels>,
    pub changed: Vec<AttachmentLabels>,
}

/// The [`AttachmentReview`] of `diff`, whose sides are `before` and `after`.
pub(super) fn attachment_labels(
    diff: &DisclosureDiff,
    before: &dyn PublicationReviewSide,
    after: &dyn PublicationReviewSide,
) -> AttachmentReview {
    AttachmentReview {
        added: diff
            .added_attachments
            .iter()
            .map(|a| attachment(after, a))
            .collect(),
        removed: diff
            .removed_attachments
            .iter()
            .map(|a| attachment(before, a))
            .collect(),
        changed: diff
            .changed_attachments
            .iter()
            .map(|c| AttachmentLabels {
                label: figure_label(after, &c.block),
                home_doc_title: None,
                before_home_doc_title: doc_title(before, &c.before_home_doc),
                after_home_doc_title: doc_title(after, &c.after_home_doc),
            })
            .collect(),
    }
}

/// The labels of a first publication's attachments: one for each attachment row `side`
/// selects, in its order.
pub fn first_attachments(side: &dyn PublicationReviewSide) -> Vec<AttachmentLabels> {
    side.selection()
        .attachments
        .iter()
        .map(|a| attachment(side, a))
        .collect()
}

// ---- references ---------------------------------------------------------------------------

/// The labels of each reference row in `rows`, in their order, read from the candidate source
/// (D-44NMT): the source's label, its doc's title (`None` outside docs or when a removal removed
/// it), whether a removal removed that title (D-62KA3), and the target's label. A missing
/// target, a cited block of another repo and a key that names no single block have no target
/// label.
pub fn reference_labels(rows: &[ReferenceRow], lookup: &ReferenceLookup) -> Vec<ReferenceLabels> {
    let keys: OnceCell<Keys> = OnceCell::new();
    rows.iter()
        .map(|r| ReferenceLabels {
            source_label: lookup.label(&r.source).unwrap_or_default(),
            source_doc_title: lookup.doc_title(&r.source),
            source_doc_redacted: lookup.doc_redacted(&r.source),
            target_label: target_label(r, lookup, &keys),
        })
        .collect()
}

/// The label of the block that `row`'s target names in the candidate source.
fn target_label(
    row: &ReferenceRow,
    lookup: &ReferenceLookup,
    keys: &OnceCell<Keys>,
) -> Option<String> {
    if row.classification == Classification::Missing {
        return None;
    }
    if let Ok(id) = BlockId::parse(&row.target) {
        return lookup.label(&id);
    }
    if foreign_ref(&row.target).is_some() || key::problem(&row.target).is_some() {
        return None;
    }
    let keys = keys.get_or_init(|| Keys::of(lookup.state().iter()));
    match keys.holders(&row.target) {
        [(_, id)] => lookup.label(id),
        _ => None,
    }
}

/// The dropped decisions `ids`, in id order, each with its title (D-7M0YF, D-0C4TB): its title,
/// else the change label over its statement, and "Content removed." for a removed one. `count`
/// counts every one (D-YKZNK). No row is cut here.
pub fn dropped_decisions(ids: &BTreeSet<BlockId>, lookup: &ReferenceLookup) -> DroppedDecisions {
    DroppedDecisions {
        count: ids.len() as u64,
        rows: ids
            .iter()
            .map(|id| DroppedDecision {
                id: id.clone(),
                title: lookup
                    .decision_title(id)
                    .unwrap_or_else(|| "Untitled".to_string()),
            })
            .collect(),
        omitted_rows: 0,
    }
}

/// The labels of `report`, read from the candidate source: its reference rows, its link rows
/// and its dropped decisions. No row is cut here.
pub fn reference_review(report: &ReferenceReport, lookup: &ReferenceLookup) -> ReferenceReview {
    ReferenceReview {
        rows: reference_labels(&report.rows, lookup),
        links_outside: reference_labels(&report.links_outside, lookup),
        dropped_decisions: dropped_decisions(&report.dropped_decisions, lookup),
    }
}
