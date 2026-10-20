//! The publication check (D-0VCQ5, D-KDFZ3, D-KQ4CA): one profile resolved in one snapshot, with
//! its reference check and, against a baseline of the same profile, its disclosure comparison,
//! projected to one bounded JSON report. The MCP tool, the ctl op and the command line all call
//! [`check`]. It is a source inspection: it writes nothing, authorizes nothing and finds no
//! previous publication; which baseline was published is the hub's record (D-WDKWQ).

use super::diff::{DisclosureDiff, disclosure_diff};
use super::references::ReferenceReport;
use super::select::{Resolved, resolve};
use crate::error::*;
use crate::{ReadState, Scope};
use rdy_model::{Hash, tree};
use serde_json::{Value, json};

/// The most rows listed for one array of the report; counts and hashes cover every row.
pub const REPORT_ROWS: usize = 200;

/// The most bytes the serialized report may hold.
pub const REPORT_BYTES: usize = 8_388_608;

/// One side of the comparison: the scope as the read state resolves it.
struct Side {
    scope: Scope,
    /// `canon` or `landing` (a tag resolves to a landing).
    kind: &'static str,
    /// The resolved landing, or `None` for an empty canon.
    landing: Option<Hash>,
}

impl Side {
    fn json(&self) -> Value {
        json!({"kind": self.kind, "landing": self.landing.map(|h| h.to_string())})
    }
}

fn bad_scope(which: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("Publication check {which} must be canon, tag:<name>, or landing:<hash>."),
        "Use canon, tag:<name>, or landing:<hash>.",
    )
}

/// `text` as one of the three scopes a check reads, resolved through `read`. Absent is canon,
/// whatever view the caller's session holds.
fn side(read: &ReadState, text: Option<&str>, which: &str) -> Result<Side, RdyError> {
    match text.unwrap_or("canon") {
        "canon" => Ok(Side {
            scope: Scope::Canon,
            kind: "canon",
            landing: read.canon_head(),
        }),
        t if t.starts_with("tag:") || t.starts_with("landing:") => match read.resolve_scope(t)? {
            Scope::At(h) => Ok(Side {
                scope: Scope::At(h),
                kind: "landing",
                landing: Some(h),
            }),
            _ => Err(bad_scope(which)),
        },
        _ => Err(bad_scope(which)),
    }
}

/// The first [`REPORT_ROWS`] of `items` as JSON, and how many were left out.
fn listed<T>(items: &[T], row: impl Fn(&T) -> Value) -> (Value, usize) {
    let shown = items.len().min(REPORT_ROWS);
    (
        Value::Array(items[..shown].iter().map(row).collect()),
        items.len() - shown,
    )
}

fn hash_or_null(h: Option<Hash>) -> Value {
    h.map_or(Value::Null, |h| json!(h.to_string()))
}

fn reference_row(r: &super::ReferenceRow) -> Value {
    json!({
        "source": r.source.to_string(),
        "location": r.location,
        "target": r.target,
        "classification": r.classification.as_str(),
        "occurrences": r.occurrences,
    })
}

/// The report's `references` object of `r`: its status, counts, its first [`REPORT_ROWS`] rows
/// and how many rows were left out. Every surface that shows a reference check uses it.
pub fn reference_summary(r: &ReferenceReport) -> Value {
    let (rows, omitted_rows) = listed(&r.rows, reference_row);
    json!({
        "status": r.status.as_str(),
        "excluded": r.excluded_count,
        "missing": r.missing_count,
        "total": r.blocked_count,
        "rows": rows,
        "omitted_rows": omitted_rows,
    })
}

fn doc_row(x: &super::DocEntry) -> Value {
    json!({
        "id": x.id.to_string(),
        "path": x.path,
        "digest": x.digest.to_string(),
        "redacted": x.redacted,
    })
}

fn disclosure_json(d: &DisclosureDiff) -> Value {
    let mut omitted = serde_json::Map::new();
    let mut put = |name: &str, (rows, left): (Value, usize)| {
        omitted.insert(name.to_string(), json!(left));
        rows
    };
    let added_docs = put("added_docs", listed(&d.added_docs, doc_row));
    let removed_docs = put("removed_docs", listed(&d.removed_docs, doc_row));
    let changed_docs = put(
        "changed_docs",
        listed(&d.changed_docs, |x| {
            json!({
                "id": x.id.to_string(),
                "before_path": x.before_path,
                "after_path": x.after_path,
                "before_digest": x.before_digest.to_string(),
                "after_digest": x.after_digest.to_string(),
                "redacted": x.redacted,
            })
        }),
    );
    let changed_folders = put(
        "changed_folders",
        listed(&d.changed_folders, |x| {
            json!({
                "id": x.id.to_string(),
                "before_version": hash_or_null(x.before_version),
                "after_version": hash_or_null(x.after_version),
                "before_path": x.before_path,
                "after_path": x.after_path,
                "redacted": x.redacted,
            })
        }),
    );
    let attachment = |x: &super::AttachmentEntry| {
        json!({
            "block": x.block.to_string(),
            "home_doc": x.home_doc.to_string(),
            "blob": x.blob.to_string(),
            "available": x.available,
        })
    };
    let added_attachments = put(
        "added_attachments",
        listed(&d.added_attachments, attachment),
    );
    let removed_attachments = put(
        "removed_attachments",
        listed(&d.removed_attachments, attachment),
    );
    let changed_attachments = put(
        "changed_attachments",
        listed(&d.changed_attachments, |x| {
            json!({
                "block": x.block.to_string(),
                "before_home_doc": x.before_home_doc.to_string(),
                "after_home_doc": x.after_home_doc.to_string(),
                "before_blob": x.before_blob.to_string(),
                "after_blob": x.after_blob.to_string(),
                "before_available": x.before_available,
                "after_available": x.after_available,
            })
        }),
    );
    let new_references = put("new_references", listed(&d.new_references, reference_row));
    let redactions = put(
        "redactions",
        listed(&d.redactions, |h| json!(h.to_string())),
    );
    json!({
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
        "added_attachments": added_attachments,
        "removed_attachments": removed_attachments,
        "changed_attachments": changed_attachments,
        "new_references": new_references,
        "new_reference_count": d.new_reference_count,
        "redactions": redactions,
        "omitted": omitted,
    })
}

fn report_json(r: &Resolved, scope: &Side, against: Option<(&Side, &DisclosureDiff)>) -> Value {
    let (entries, omitted_entries) = listed(&r.entries, |e| {
        json!({
            "id": e.id.to_string(),
            "version": e.version.to_string(),
            "redaction": hash_or_null(e.redaction),
        })
    });
    let folders = r.m.iter().filter(|b| b.block_type == tree::FOLDER).count();
    let warnings: Vec<Value> = r
        .references
        .warning()
        .map(|w| json!({"code": w.code, "message": w.message, "fix": w.fix}))
        .into_iter()
        .collect();
    json!({
        "profile": {
            "id": r.profile.id.as_ref().map(ToString::to_string),
            "version": hash_or_null(r.profile.version),
            "key": r.profile.key,
            "title": r.profile.title,
            "audience": r.profile.audience.as_str(),
            "trail": r.profile.trail,
        },
        "scope": scope.json(),
        "against": against.map(|(s, _)| s.json()),
        "selection": {
            "hash": r.selection_hash.to_string(),
            "identity": r.selection_identity.to_string(),
            "blocks": r.entries.len(),
            "docs": r.docs.len(),
            "folders": folders,
            "attachments": r.attachments.len(),
            "entries": entries,
            "omitted_entries": omitted_entries,
        },
        "references": reference_summary(&r.references),
        "disclosure": against.map(|(_, d)| disclosure_json(d)),
        "warnings": warnings,
    })
}

/// `report` if it serializes within `max` bytes, else `E_LIMIT`; never a partial object.
fn fit(report: Value, max: usize) -> Result<Value, RdyError> {
    // serializing a `Value` cannot fail: its keys are strings
    let size = serde_json::to_vec(&report).map_or(usize::MAX, |b| b.len());
    if size > max {
        return Err(RdyError::new(
            E_LIMIT,
            format!("Publication report exceeds the limit of {max} bytes."),
            "Reduce the selection or compare smaller profiles.",
        ));
    }
    Ok(report)
}

/// The check of `profile` (a full profile id or a key) in `scope`, with the disclosure
/// comparison against `against` when given. Scopes are `canon`, `tag:<name>` and
/// `landing:<hash>` only; an absent scope is canon. Every scope string resolves through `read`.
/// The profile resolves in the candidate scope first; the baseline then resolves that profile's
/// stable id, with no fallback to another profile or to current canon. A blocked reference check
/// is a result, not an error: the caller reads `references.status`.
pub fn check(
    read: &ReadState,
    profile: &str,
    scope: Option<&str>,
    against: Option<&str>,
) -> Result<Value, RdyError> {
    let candidate = side(read, scope, "scope")?;
    let after = resolve(read, &candidate.scope, profile)?;
    let Some(text) = against else {
        return fit(report_json(&after, &candidate, None), REPORT_BYTES);
    };
    let baseline = side(read, Some(text), "against")?;
    let stable = after
        .profile
        .id
        .as_ref()
        .map_or_else(|| profile.to_string(), ToString::to_string);
    let before = resolve(read, &baseline.scope, &stable)?;
    let diff = disclosure_diff(&before, &after)?;
    fit(
        report_json(&after, &candidate, Some((&baseline, &diff))),
        REPORT_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pub9_the_report_limit_is_eight_mebibytes_and_never_returns_a_partial_object() {
        assert_eq!(REPORT_BYTES, 8_388_608);
        let big = json!({"rows": ["x".repeat(64)]});
        assert!(fit(big.clone(), 1000).is_ok());
        let e = fit(big, 10).unwrap_err();
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(
            e.message,
            "Publication report exceeds the limit of 10 bytes."
        );
        assert_eq!(e.fix, "Reduce the selection or compare smaller profiles.");
        let e = fit(json!({"a": "x".repeat(REPORT_BYTES)}), REPORT_BYTES).unwrap_err();
        assert_eq!(
            e.message,
            "Publication report exceeds the limit of 8388608 bytes."
        );
    }

    #[test]
    fn pub9_listed_keeps_the_first_rows_and_counts_the_rest() {
        for (n, shown, left) in [(199, 199, 0), (200, 200, 0), (201, 200, 1)] {
            let items: Vec<usize> = (0..n).collect();
            let (rows, omitted) = listed(&items, |i| json!(i));
            assert_eq!(rows.as_array().unwrap().len(), shown);
            assert_eq!(omitted, left);
        }
    }
}
