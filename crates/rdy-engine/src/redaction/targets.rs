//! The closed selector grammar, the reason class and the reason (plan Redaction). A selector is
//! exactly one of:
//!
//! - `<id>`: every stored version of a block;
//! - `<id>@<hash>`: one version, by its full 64-digit lowercase hash;
//! - `blob:<hash>`: a blob, by its full manifest hash;
//! - `<alias>:<id>@<hash>`: one delivered version of a cited block (repo links, D-CN4QJ), by the
//!   alias of a live repo link, the block's id in the cited project and the full version hash.
//!   Its stored form `<repo>:<id>@<hash>` names the cited repo by its 32 hex digits.
//!
//! No key, path, glob, short hash, `meta/` ref, tag or landing is a selector. The checks run in
//! the order the plan fixes: target count, then each selector's byte length and syntax in input
//! order, then the reason class, then the reason, then each alias in input order (an alias no
//! live repo link holds is the write path's `E_UNKNOWN_REF`).
use super::RedactInput;
use crate::error::{
    REDACTION_CAUSE_INVALID, REDACTION_CAUSE_METADATA, RdyError, redaction_limit, redaction_reason,
    redaction_target,
};
use rdy_model::redaction::{ReasonClass, RedactTarget};
use rdy_model::reference::{ForeignRef, foreign_ref};
use rdy_model::{BlockId, Hash, RepoId};
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

/// A selector by its syntax: a target, or a cited version named through a repo link's alias,
/// which only the project's live repo links resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    Target(RedactTarget),
    Aliased {
        alias: String,
        id: BlockId,
        version: Hash,
    },
}

/// The selector `text`, by its syntax alone (`E_REDACTION_TARGET`, cause `invalid selector`, or
/// `metadata is not content` for a ref, tag, landing, proposal or view). Whether it names a
/// stored object is checked against the store later, and an alias against the live repo links.
pub fn parse_selector(text: &str) -> Result<Selector, RdyError> {
    let invalid = || redaction_target(&quoted(text), REDACTION_CAUSE_INVALID);
    if METADATA_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return Err(redaction_target(&quoted(text), REDACTION_CAUSE_METADATA));
    }
    if let Some(h) = text.strip_prefix("blob:") {
        return full_hash(h)
            .map(|h| Selector::Target(RedactTarget::Blob(h)))
            .ok_or_else(invalid);
    }
    if let Some((left, h)) = text.split_once('@') {
        let version = full_hash(h).ok_or_else(invalid)?;
        if left.contains(':') {
            // a cited version: the alias or stored form of a foreign reference, by id only
            return match foreign_ref(left) {
                Some(ForeignRef::Stored { repo, id }) => {
                    Ok(Selector::Target(RedactTarget::ForeignVersion {
                        repo,
                        id,
                        version,
                    }))
                }
                Some(ForeignRef::Alias { alias, target }) => match BlockId::parse(target) {
                    Ok(id) => Ok(Selector::Aliased {
                        alias: alias.to_string(),
                        id,
                        version,
                    }),
                    Err(_) => Err(invalid()),
                },
                None => Err(invalid()),
            };
        }
        return match BlockId::parse(left) {
            Ok(id) => Ok(Selector::Target(RedactTarget::Version { id, version })),
            Err(_) => Err(invalid()),
        };
    }
    BlockId::parse(text)
        .map(|id| Selector::Target(RedactTarget::AllVersions(id)))
        .map_err(|_| invalid())
}

/// The selector of `t` in the input grammar, with full hashes. A cited block's version (repo
/// links) shows its repo's 32 hex digits where the input names the link's alias.
pub fn selector_text(t: &RedactTarget) -> String {
    match t {
        RedactTarget::AllVersions(id) => id.to_string(),
        RedactTarget::Version { id, version } => format!("{id}@{version}"),
        RedactTarget::Blob(h) => format!("blob:{h}"),
        RedactTarget::ForeignVersion { repo, id, version } => format!("{repo}:{id}@{version}"),
    }
}

/// The canonical order of normalized selectors: all-versions, then versions, then blobs, then
/// cited versions; then repo bytes; then id bytes; then hash bytes.
pub fn target_order(a: &RedactTarget, b: &RedactTarget) -> Ordering {
    fn key(t: &RedactTarget) -> (u8, Option<&RepoId>, &[u8], Option<&Hash>) {
        match t {
            RedactTarget::AllVersions(id) => (0, None, id.as_str().as_bytes(), None),
            RedactTarget::Version { id, version } => {
                (1, None, id.as_str().as_bytes(), Some(version))
            }
            RedactTarget::Blob(h) => (2, None, &[], Some(h)),
            RedactTarget::ForeignVersion { repo, id, version } => {
                (3, Some(repo), id.as_str().as_bytes(), Some(version))
            }
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
/// syntax in input order, the reason class, the reason (1 to 4096 UTF-8 bytes after trimming),
/// then each alias in input order: `repo_of` gives the repo the live repo link of that alias
/// cites, or its refusal.
pub(crate) fn check_input(
    input: &RedactInput,
    repo_of: &mut dyn FnMut(&str) -> Result<RepoId, RdyError>,
) -> Result<Checked, RdyError> {
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
    let class = parse_reason_class(&input.reason_class)?;
    let reason = input.reason.trim();
    if reason.is_empty() || reason.len() > MAX_REASON_BYTES {
        return Err(redaction_reason());
    }
    let mut targets = targets
        .into_iter()
        .map(|t| match t {
            Selector::Target(t) => Ok(t),
            Selector::Aliased { alias, id, version } => Ok(RedactTarget::ForeignVersion {
                repo: repo_of(&alias)?,
                id,
                version,
            }),
        })
        .collect::<Result<Vec<_>, RdyError>>()?;
    targets.sort_by(target_order);
    targets.dedup();
    Ok(Checked {
        targets,
        class,
        reason: reason.to_string(),
    })
}
