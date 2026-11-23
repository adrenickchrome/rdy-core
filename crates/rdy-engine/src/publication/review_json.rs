//! The one JSON form of the publication review (D-RDH1C, D-9BWMV, D-MCH70): the `references`,
//! `disclosure` and `included` members of a check. Chat, the CLI and the adapter write them from
//! the same rows ([`PublicationReview`]) with [`review_json`]; only the projection differs
//! ([`ReviewProjection`]). The report cuts each top-level list to its first rows; the adapter
//! keeps every outer row. Both share the inner limits and the change budget.
//!
//! Every old member stays. A row of the comparison or the reference check keeps its old fields
//! and adds its review fields. The doc arrays follow each side's reading order, not id order.
//! No member here enters a selection hash, a selection identity or a doc digest (D-B2AC3).

use super::baseline::{Baseline, BaselineReviewSide};
use super::diff::DisclosureDiff;
use super::references::{ReferenceReport, ReferenceRow};
use super::report::{doc_row, hash_or_null, reference_row};
use super::review_docs::included;
use super::review_labels::{first_attachments, reference_labels, reference_review};
use super::review_limits::{ReviewProjection, disclosure_review};
use super::review_model::{
    AttachmentLabels, DisclosureReview, Included, OutsideReview, ReferenceLabels, ReferenceReview,
};
use super::review_side::{ReaderReviewSide, ReferenceLookup, ReviewRemovals};
use super::select::{AttachmentEntry, Resolved};
use super::source::PublicationSource;
use crate::Materialized;
use crate::error::*;
use rdy_model::BlockId;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The complete review of one check, before any projection cuts an outer list: the labels of the
/// candidate's reference check, and either the review of a comparison (with the labels of its new
/// references) or, for a first publication, the included docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationReview {
    pub references: ReferenceReview,
    pub new_references: Vec<ReferenceLabels>,
    pub disclosure: Option<DisclosureReview>,
    pub included: Option<Included>,
    /// The labels of a first publication's attachments, in the order of its resolution's
    /// attachment rows; empty with a comparison.
    pub first_attachments: Vec<AttachmentLabels>,
}

impl PublicationReview {
    /// The review of `after`, resolved from `candidate_source` through `source`. With `against`
    /// (the baseline and the comparison), both sides read with the removals of both; without it,
    /// the review lists the included docs. The reference labels read the candidate source with
    /// the same removals applied.
    pub fn build(
        source: &dyn PublicationSource,
        candidate_source: &Materialized,
        after: &Resolved,
        against: Option<(&Resolved, &DisclosureDiff)>,
    ) -> Result<PublicationReview, RdyError> {
        let sides: Vec<&Resolved> = match against {
            Some((before, _)) => vec![before, after],
            None => vec![after],
        };
        let removals = ReviewRemovals::of_sides(source, &sides)?;
        let lookup = ReferenceLookup::new(candidate_source, &removals);
        let after_side = ReaderReviewSide::new(after, &removals);
        let references = reference_review(&after.references, &lookup);
        Ok(match against {
            None => PublicationReview {
                references,
                new_references: vec![],
                disclosure: None,
                included: Some(included(&after_side)),
                first_attachments: first_attachments(&after_side),
            },
            Some((before, diff)) => {
                let before_side = ReaderReviewSide::new(before, &removals);
                PublicationReview {
                    references,
                    new_references: reference_labels(&diff.new_references, &lookup),
                    disclosure: Some(disclosure_review(diff, &before_side, &after_side)),
                    included: None,
                    first_attachments: vec![],
                }
            }
        })
    }

    /// The review of `after` against the portable `baseline` (D-CDQF7, D-W4YAE): the candidate
    /// reads as [`PublicationReview::build`] reads it, with the baseline's removals added; the
    /// baseline reads through its own side view, which has no reader text.
    pub fn build_against_baseline(
        source: &dyn PublicationSource,
        candidate_source: &Materialized,
        after: &Resolved,
        baseline: &Baseline,
        diff: &DisclosureDiff,
    ) -> Result<PublicationReview, RdyError> {
        let removals = ReviewRemovals::of_sides(source, &[after])?.with_baseline(baseline);
        let lookup = ReferenceLookup::new(candidate_source, &removals);
        let after_side = ReaderReviewSide::new(after, &removals);
        let before_side = BaselineReviewSide::new(baseline, &removals);
        Ok(PublicationReview {
            references: reference_review(&after.references, &lookup),
            new_references: reference_labels(&diff.new_references, &lookup),
            disclosure: Some(disclosure_review(diff, &before_side, &after_side)),
            included: None,
            first_attachments: vec![],
        })
    }
}

/// The three review members of a check as JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewJson {
    pub references: Value,
    pub disclosure: Value,
    pub included: Value,
}

/// The JSON of `review` in `projection`: `references` from `report` and its labels;
/// `disclosure` from `diff` and its review, or null without a comparison; `included`, or null
/// with a comparison. A `review` built for another comparison gives no labels for the rows it
/// lacks.
pub fn review_json(
    projection: ReviewProjection,
    report: &ReferenceReport,
    diff: Option<&DisclosureDiff>,
    review: &PublicationReview,
) -> ReviewJson {
    ReviewJson {
        references: references_json(projection, report, &review.references),
        disclosure: match (diff, &review.disclosure) {
            (Some(d), Some(r)) => disclosure_json(projection, d, r, &review.new_references),
            _ => Value::Null,
        },
        included: match (diff, &review.included) {
            (None, Some(i)) => value(&projection.included(i)),
            _ => Value::Null,
        },
    }
}

/// `x` as JSON. The rows hold string keys only, so this cannot fail; a failure writes null.
fn value<T: Serialize>(x: &T) -> Value {
    serde_json::to_value(x).unwrap_or(Value::Null)
}

/// `base` with each member of `extra` that it lacks.
fn with(mut base: Value, extra: Map<String, Value>) -> Value {
    if let Value::Object(m) = &mut base {
        for (k, v) in extra {
            m.entry(k).or_insert(v);
        }
    }
    base
}

/// The members of `x`, an object.
fn members<T: Serialize>(x: &T) -> Map<String, Value> {
    match value(x) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The named members of `pairs` as a map.
fn object<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// A reference row with its labels.
fn labelled_reference(r: &ReferenceRow, l: Option<&ReferenceLabels>) -> Value {
    match l {
        Some(l) => with(reference_row(r), members(l)),
        None => reference_row(r),
    }
}

/// The kept rows of `rows` with their labels, and how many the projection left out.
fn labelled_rows(
    projection: ReviewProjection,
    rows: &[ReferenceRow],
    labels: &[ReferenceLabels],
) -> (Value, u64) {
    let (kept, omitted) = projection.list(rows);
    let rows = kept
        .iter()
        .enumerate()
        .map(|(i, r)| labelled_reference(r, labels.get(i)))
        .collect();
    (Value::Array(rows), omitted)
}

/// The `references` object: the old summary's members, its rows with their labels, and the
/// dropped decisions with their titles (D-7M0YF).
fn references_json(
    projection: ReviewProjection,
    r: &ReferenceReport,
    labels: &ReferenceReview,
) -> Value {
    let (rows, omitted_rows) = labelled_rows(projection, &r.rows, &labels.rows);
    let (link_rows, omitted_link_rows) =
        labelled_rows(projection, &r.links_outside, &labels.links_outside);
    let dropped = &labels.dropped_decisions;
    let (dropped_rows, omitted_dropped) = projection.list(&dropped.rows);
    json!({
        "status": r.status.as_str(),
        "excluded": r.excluded_count,
        "missing": r.missing_count,
        "total": r.blocked_count,
        "rows": rows,
        "omitted_rows": omitted_rows,
        "links_outside": {
            "count": r.links_outside_count,
            "rows": link_rows,
            "omitted_rows": omitted_link_rows,
        },
        "dropped_decisions": {
            "count": dropped.count,
            "rows": value(&dropped_rows),
            "omitted_rows": dropped.omitted_rows + omitted_dropped,
        },
    })
}

/// The rows of `old` in the order of `review`'s ids, each with its review: first every row the
/// review lists, then any row it lacks, in its old order.
fn in_review_order<'a, T, R>(
    old: &'a [T],
    old_id: impl Fn(&T) -> &BlockId,
    review: &'a [R],
    review_id: impl Fn(&R) -> &BlockId,
) -> Vec<(&'a T, Option<&'a R>)> {
    let by_id: BTreeMap<&BlockId, &T> = old.iter().map(|x| (old_id(x), x)).collect();
    let mut out: Vec<(&T, Option<&R>)> = review
        .iter()
        .filter_map(|r| by_id.get(review_id(r)).map(|x| (*x, Some(r))))
        .collect();
    let listed: std::collections::BTreeSet<&BlockId> = review.iter().map(&review_id).collect();
    out.extend(
        old.iter()
            .filter(|x| !listed.contains(old_id(x)))
            .map(|x| (x, None)),
    );
    out
}

/// An attachment row's old members; a dark row adds `variant: "dark"` (D-TBDA1).
fn attachment_row(x: &AttachmentEntry) -> Value {
    let mut row = json!({
        "block": x.block.to_string(),
        "home_doc": x.home_doc.to_string(),
        "blob": x.blob.to_string(),
        "available": x.available,
    });
    if let Some(v) = x.variant {
        row["variant"] = json!(v.as_str());
    }
    row
}

/// The labels of an added or removed attachment row.
fn attachment_labels(l: Option<&AttachmentLabels>) -> Map<String, Value> {
    l.map(|l| {
        object([
            ("label", json!(l.label)),
            ("home_doc_title", json!(l.home_doc_title)),
        ])
    })
    .unwrap_or_default()
}

/// A doc row's `changes_total` (D-6T9WJ), where its review gives one. It enters no selection hash,
/// selection identity or doc digest (D-B2AC3).
fn total_member(total: Option<u64>) -> Map<String, Value> {
    total
        .map(|n| object([("changes_total", json!(n))]))
        .unwrap_or_default()
}

/// What a row outside docs adds: its type, label, change, `what` and `record_only`, and a
/// decision's own fields when it has a change.
fn outside_members(o: &OutsideReview) -> Map<String, Value> {
    let mut m = object([
        ("block_type", json!(o.block_type)),
        ("label", json!(o.label)),
        ("change", value(&o.change)),
        ("what", json!(o.what)),
        ("record_only", json!(o.record_only)),
    ]);
    if let Some(d) = &o.decision {
        m.insert("title".into(), json!(d.title));
        m.insert("status_from".into(), json!(d.status_from));
        m.insert("status_to".into(), json!(d.status_to));
        m.insert("excerpt".into(), json!(d.excerpt));
    }
    m
}

/// The `disclosure` object: the comparison's old members with their rows' review fields, and
/// the totals over the complete comparison.
fn disclosure_json(
    p: ReviewProjection,
    d: &DisclosureDiff,
    review: &DisclosureReview,
    new_references: &[ReferenceLabels],
) -> Value {
    let mut omitted = Map::new();
    let mut put = |name: &str, rows: Vec<Value>, left: u64| {
        omitted.insert(name.to_string(), json!(left));
        Value::Array(rows)
    };
    let added = in_review_order(&d.added_docs, |x| &x.id, &review.added_docs, |r| &r.id);
    let (kept, left) = p.list(&added);
    let added_docs = put(
        "added_docs",
        kept.iter()
            .map(|(x, r)| {
                let total = total_member(r.map(|r| r.size.changes_total()));
                with(with(doc_row(x), r.map(members).unwrap_or_default()), total)
            })
            .collect(),
        left,
    );
    let removed = in_review_order(&d.removed_docs, |x| &x.id, &review.removed_docs, |r| &r.id);
    let (kept, left) = p.list(&removed);
    let removed_docs = put(
        "removed_docs",
        kept.iter()
            .map(|(x, r)| {
                let total = total_member(r.map(|r| r.size.changes_total()));
                with(with(doc_row(x), r.map(members).unwrap_or_default()), total)
            })
            .collect(),
        left,
    );
    let changed = in_review_order(&d.changed_docs, |x| &x.id, &review.changed_docs, |r| &r.id);
    let (kept, left) = p.list(&changed);
    let changed_docs = put(
        "changed_docs",
        kept.iter()
            .map(|(x, r)| {
                let old = json!({
                    "id": x.id.to_string(),
                    "before_path": x.before_path,
                    "after_path": x.after_path,
                    "before_digest": x.before_digest.to_string(),
                    "after_digest": x.after_digest.to_string(),
                    "redacted": x.redacted,
                });
                let total = total_member(r.map(|r| r.changes_total()));
                with(with(old, r.map(members).unwrap_or_default()), total)
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.changed_folders);
    let changed_folders = put(
        "changed_folders",
        kept.iter()
            .enumerate()
            .map(|(i, x)| {
                let old = json!({
                    "id": x.id.to_string(),
                    "before_version": hash_or_null(x.before_version),
                    "after_version": hash_or_null(x.after_version),
                    "before_path": x.before_path,
                    "after_path": x.after_path,
                    "redacted": x.redacted,
                });
                let extra = review
                    .changed_folders
                    .get(i)
                    .map(|f| {
                        object([
                            ("title", json!(f.title)),
                            ("before_title", json!(f.before_title)),
                        ])
                    })
                    .unwrap_or_default();
                with(old, extra)
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.changed_outside_docs);
    let changed_outside_docs = put(
        "changed_outside_docs",
        kept.iter()
            .enumerate()
            .map(|(i, x)| {
                let old = json!({
                    "id": x.id.to_string(),
                    "before_version": hash_or_null(x.before_version),
                    "after_version": hash_or_null(x.after_version),
                    "redacted": x.redacted,
                });
                let extra = review
                    .changed_outside_docs
                    .get(i)
                    .map(outside_members)
                    .unwrap_or_default();
                with(old, extra)
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.added_attachments);
    let added_attachments = put(
        "added_attachments",
        kept.iter()
            .enumerate()
            .map(|(i, x)| {
                with(
                    attachment_row(x),
                    attachment_labels(review.added_attachments.get(i)),
                )
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.removed_attachments);
    let removed_attachments = put(
        "removed_attachments",
        kept.iter()
            .enumerate()
            .map(|(i, x)| {
                with(
                    attachment_row(x),
                    attachment_labels(review.removed_attachments.get(i)),
                )
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.changed_attachments);
    let changed_attachments = put(
        "changed_attachments",
        kept.iter()
            .enumerate()
            .map(|(i, x)| {
                let mut row = json!({
                    "block": x.block.to_string(),
                    "before_home_doc": x.before_home_doc.to_string(),
                    "after_home_doc": x.after_home_doc.to_string(),
                    "before_blob": x.before_blob.to_string(),
                    "after_blob": x.after_blob.to_string(),
                    "before_available": x.before_available,
                    "after_available": x.after_available,
                });
                if let Some(v) = x.variant {
                    row["variant"] = json!(v.as_str());
                }
                let extra = review
                    .changed_attachments
                    .get(i)
                    .map(|l| {
                        object([
                            ("label", json!(l.label)),
                            ("before_home_doc_title", json!(l.before_home_doc_title)),
                            ("after_home_doc_title", json!(l.after_home_doc_title)),
                        ])
                    })
                    .unwrap_or_default();
                with(row, extra)
            })
            .collect(),
        left,
    );
    let (kept, left) = p.list(&d.new_references);
    let new_references = put(
        "new_references",
        kept.iter()
            .enumerate()
            .map(|(i, r)| labelled_reference(r, new_references.get(i)))
            .collect(),
        left,
    );
    let hashes = |kept: &[rdy_model::Hash]| -> Vec<Value> {
        kept.iter().map(|h| json!(h.to_string())).collect()
    };
    // a baseline comparison keeps each side's requests apart (D-CDQF7): the store records of one
    // side never name the other side's requests
    let redactions = match &d.baseline {
        None => {
            let (kept, left) = p.list(&d.redactions);
            put("redactions", hashes(kept), left)
        }
        Some(b) => {
            let (candidate, c_left) = p.list(&b.candidate_redactions);
            let (baseline, b_left) = p.list(&b.baseline_redactions);
            put("redactions", vec![], c_left + b_left);
            json!({"candidate": hashes(candidate), "baseline": hashes(baseline)})
        }
    };
    let mut out = json!({
        "profile_id": d.profile_id.as_ref().map(ToString::to_string),
        "audience": d.audience.as_str(),
        "before_profile_version": hash_or_null(d.before_profile_version),
        "after_profile_version": hash_or_null(d.after_profile_version),
        "before_selection_hash": d.before_selection_hash.to_string(),
        "after_selection_hash": d.after_selection_hash.to_string(),
        "before_selection_identity": d.before_selection_identity.to_string(),
        "after_selection_identity": d.after_selection_identity.to_string(),
        "profile_changed": d.profile_changed,
        "added_docs": added_docs,
        "removed_docs": removed_docs,
        "changed_docs": changed_docs,
        "changed_folders": changed_folders,
        "changed_outside_docs": changed_outside_docs,
        "added_attachments": added_attachments,
        "removed_attachments": removed_attachments,
        "changed_attachments": changed_attachments,
        "new_references": new_references,
        "new_reference_count": d.new_reference_count,
        "redactions": redactions,
        "newly_hidden_planned": d.newly_hidden_planned.to_json(),
        "omitted": omitted,
        "totals": value(&review.totals),
    });
    if let Some(b) = &d.baseline {
        out["baseline"] = json!({
            "repo_id": b.repo_id.to_string(),
            "tag": b.tag,
            "landing": b.landing.to_string(),
            "baseline_hash": b.baseline_hash.to_string(),
        });
    }
    out
}
