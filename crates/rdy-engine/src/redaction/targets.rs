//! The closed selector grammar, the reason class and the reason (plan Redaction). A selector is
//! exactly one of:
//!
//! - `<id>`: every stored version of a block;
//! - `<id>@<hash>`: one version, by its full 64-digit lowercase hash;
//! - `blob:<hash>`: a blob, by its full manifest hash.
//!
//! No key, path, glob, short hash, `meta/` ref, tag or landing is a selector. The checks run in
//! the order the plan fixes: target count, then each selector's byte length and syntax in input
//! order, then the reason class, then the reason.
use super::RedactInput;
use crate::error::{
    REDACTION_CAUSE_INVALID, REDACTION_CAUSE_METADATA, RdyError, redaction_limit, redaction_reason,
    redaction_target,
};
use rdy_model::redaction::{ReasonClass, RedactTarget};
use rdy_model::{BlockId, Hash};
use rdy_store::redaction::{MAX_TARGET_BYTES, MAX_TARGETS, limit};
use std::cmp::Ordering;

/// The most UTF-8 bytes of a reason, after trimming its outer whitespace (inclusive).
pub const MAX_REASON_BYTES: usize = 4096;

/// The prefixes that name a metadata record rather than content: refs, tags, landings,
/// proposals and views.
const METADATA_PREFIXES: [&str; 7] = [
    "meta/",
    "tag:",
    "landing:",
    "proposal:",
    "proposal/",
    "view:",
    "view/",
];

/// `text` as an error names an input that is no selector: its JSON string encoding, so it stays
/// on one line.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// A full hash: exactly 64 lowercase hexadecimal digits.
fn full_hash(text: &str) -> Option<Hash> {
    text.parse().ok()
}

/// The selector `text`, by its syntax alone (`E_REDACTION_TARGET`, cause `invalid selector`, or
/// `metadata is not content` for a ref, tag, landing, proposal or view). Whether it names a
/// stored object is checked against the store later.
pub fn parse_selector(text: &str) -> Result<RedactTarget, RdyError> {
    let invalid = || redaction_target(&quoted(text), REDACTION_CAUSE_INVALID);
    if METADATA_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return Err(redaction_target(&quoted(text), REDACTION_CAUSE_METADATA));
    }
    if let Some(h) = text.strip_prefix("blob:") {
        return full_hash(h).map(RedactTarget::Blob).ok_or_else(invalid);
    }
    if let Some((id, h)) = text.split_once('@') {
        return match (BlockId::parse(id), full_hash(h)) {
            (Ok(id), Some(version)) => Ok(RedactTarget::Version { id, version }),
            _ => Err(invalid()),
        };
    }
    BlockId::parse(text)
        .map(RedactTarget::AllVersions)
        .map_err(|_| invalid())
}

/// The selector of `t` in the input grammar, with full hashes.
pub fn selector_text(t: &RedactTarget) -> String {
    match t {
        RedactTarget::AllVersions(id) => id.to_string(),
        RedactTarget::Version { id, version } => format!("{id}@{version}"),
        RedactTarget::Blob(h) => format!("blob:{h}"),
    }
}

/// The canonical order of normalized selectors: all-versions, then versions, then blobs; then
/// id bytes; then hash bytes.
pub fn target_order(a: &RedactTarget, b: &RedactTarget) -> Ordering {
    fn key(t: &RedactTarget) -> (u8, &[u8], Option<&Hash>) {
        match t {
            RedactTarget::AllVersions(id) => (0, id.as_str().as_bytes(), None),
            RedactTarget::Version { id, version } => (1, id.as_str().as_bytes(), Some(version)),
            RedactTarget::Blob(h) => (2, &[], Some(h)),
        }
    }
    key(a).cmp(&key(b))
}

/// The reason class whose wire name is `text`; any other text is `E_REDACTION_REASON`.
pub fn parse_reason_class(text: &str) -> Result<ReasonClass, RdyError> {
    match text {
        "secret" => Ok(ReasonClass::Secret),
        "personal" => Ok(ReasonClass::Personal),
        "legal" => Ok(ReasonClass::Legal),
        "retraction" => Ok(ReasonClass::Retraction),
        _ => Err(redaction_reason()),
    }
}

/// The wire name of `c`.
pub fn reason_class_name(c: ReasonClass) -> &'static str {
    match c {
        ReasonClass::Secret => "secret",
        ReasonClass::Personal => "personal",
        ReasonClass::Legal => "legal",
        ReasonClass::Retraction => "retraction",
    }
}

/// The input, checked and normalized.
#[derive(Clone, Debug)]
pub(crate) struct Checked {
    /// The selectors, deduplicated and in canonical order.
    pub(crate) targets: Vec<RedactTarget>,
    pub(crate) class: ReasonClass,
    /// The reason with its outer whitespace trimmed.
    pub(crate) reason: String,
}

/// Checks `input` in the fixed order: the target count (none is `E_REDACTION_TARGET` with
/// target `[]`; more than 256 is `E_REDACTION_LIMIT`), each selector's byte length (256) and
/// syntax in input order, the reason class, then the reason (1 to 4096 UTF-8 bytes after
/// trimming).
pub(crate) fn check_input(input: &RedactInput) -> Result<Checked, RdyError> {
    if input.targets.is_empty() {
        return Err(redaction_target("[]", REDACTION_CAUSE_INVALID));
    }
    if input.targets.len() > MAX_TARGETS {
        return Err(redaction_limit(limit::TARGETS));
    }
    let mut targets = Vec::with_capacity(input.targets.len());
    for text in &input.targets {
        if text.len() > MAX_TARGET_BYTES {
            return Err(redaction_limit(limit::TARGET_BYTES));
        }
        targets.push(parse_selector(text)?);
    }
    targets.sort_by(target_order);
    targets.dedup();
    let class = parse_reason_class(&input.reason_class)?;
    let reason = input.reason.trim();
    if reason.is_empty() || reason.len() > MAX_REASON_BYTES {
        return Err(redaction_reason());
    }
    Ok(Checked {
        targets,
        class,
        reason: reason.to_string(),
    })
}
