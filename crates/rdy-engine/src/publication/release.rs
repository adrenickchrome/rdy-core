//! The release comparison of the publication check (D-WJ0SJ): which shown content is `since`
//! a later version than the release being checked, and which retained decision is `built` in
//! one. It is a read of a resolved selection and writes nothing. Planned blocks a `mark` profile
//! shows are not violations: only a later version is. A reference to a hidden target belongs to
//! the reference check, not to a row here.

use super::REPORT_ROWS;
use super::references::CheckStatus;
use super::select::Resolved;
use crate::as_built::Status;
use crate::error::*;
use rdy_model::{FieldValue, decision::DECISION};
use rdy_schema::as_built::{BUILT_FIELD, MAX_VERSION_BYTES, SINCE_FIELD, parse_release};
use semver::Version;
use serde_json::{Value, json};

const RELEASE_FIX: &str = "Use a version such as 1.2.0 or 2.0.0-rc.1; one leading v is allowed, and +build metadata is not.";

/// The release `text` names. Parsed before any selection is resolved or allocated.
pub(super) fn parse(text: &str) -> Result<Version, RdyError> {
    if text.len() > MAX_VERSION_BYTES {
        return Err(RdyError::new(
            E_LIMIT,
            "Release exceeds 256 bytes.",
            "Use at most 256 bytes for release.",
        ));
    }
    parse_release(text).map_err(|_| {
        RdyError::new(
            E_BAD_INPUT,
            "Release must be a release version.",
            RELEASE_FIX,
        )
    })
}

/// One violating block and the field that puts it after the release.
struct Violation {
    id: String,
    field: &'static str,
    version: Version,
}

/// The release comparison of `r` against `release`: the `release` object of the report and its
/// status (`clear` or `blocked`, for either audience).
pub(super) fn report(r: &Resolved, release: &Version) -> (CheckStatus, Value) {
    let mut found: Vec<Violation> = Vec::new();
    // shown content: its effective since, even when the profile shows no pill
    for (id, s) in &r.as_built {
        if s.status != Status::Since {
            continue;
        }
        if let Some(v) = s.since.as_deref().and_then(|t| parse_release(t).ok())
            && v > *release
        {
            found.push(Violation {
                id: id.to_string(),
                field: SINCE_FIELD,
                version: v,
            });
        }
    }
    // every retained decision with an explicit later built version
    for e in &r.entries {
        let Some(b) = r.m.get(&e.id) else { continue };
        if b.block_type != DECISION || b.is_redacted() {
            continue;
        }
        if let Some(FieldValue::Str(t)) = b.fields.get(BUILT_FIELD)
            && let Ok(v) = parse_release(t)
            && v > *release
        {
            found.push(Violation {
                id: e.id.to_string(),
                field: BUILT_FIELD,
                version: v,
            });
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id).then(a.field.cmp(b.field)));
    let blocks_later = found.iter().filter(|v| v.field == SINCE_FIELD).count();
    let decisions_later = found.len() - blocks_later;
    let rows: Vec<Value> = found
        .iter()
        .take(REPORT_ROWS)
        .map(|v| row(v, release))
        .collect();
    let status = if found.is_empty() {
        CheckStatus::Clear
    } else {
        CheckStatus::Blocked
    };
    let object = json!({
        "version": release.to_string(),
        "status": status.as_str(),
        "blocks_later": blocks_later,
        "decisions_later": decisions_later,
        "rows": rows,
        "omitted_rows": found.len().saturating_sub(REPORT_ROWS),
    });
    (status, object)
}

fn row(v: &Violation, release: &Version) -> Value {
    let (message, fix) = if v.field == SINCE_FIELD {
        (
            format!(
                "Block {} is since {}, after release {release}.",
                v.id, v.version
            ),
            "Check built and since at this scope. Correct the records or check the release that ships this content.",
        )
    } else {
        (
            format!(
                "Decision {} is built in {}, after release {release}.",
                v.id, v.version
            ),
            "Check built at this scope. Correct the record or check the release that ships this decision.",
        )
    };
    json!({
        "id": v.id,
        "field": v.field,
        "version": v.version.to_string(),
        "message": message,
        "fix": fix,
    })
}
