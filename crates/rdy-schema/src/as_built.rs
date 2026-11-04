//! As-built status (D-WJ0SJ): the fields that record when rdy ships a piece of content, and the
//! pure helpers that read their values. A decision records its first built release in the
//! tracking field `built`. A content block may override the status derived from its decisions
//! with `planned` (a bool) or `since` (a version). Both fields hold ordinary authored values:
//! nothing here changes how a block is stored.
//!
//! A version is the one grammar rdy already uses for tags: SemVer 2.0.0, one optional leading
//! `v`, no `+build` metadata, ordered by number and prerelease precedence, never by string.

use crate::pack::{FieldDef, FieldKind, Pack, TypeClass};
use crate::validate::{Issue, Severity};
use rdy_model::{Block, FieldValue, tree};
use semver::Version;
use std::cmp::Ordering;

/// The decision field that records the first release that ships the decision.
pub const BUILT_FIELD: &str = "built";

/// The content field that marks a block planned by hand. On a section or doc it covers every
/// block below it.
pub const PLANNED_FIELD: &str = "planned";

/// The content field that sets the release a block first ships in by hand.
pub const SINCE_FIELD: &str = "since";

/// The `built` value of a decision that rdy does not ship yet.
pub const BUILT_PLANNED: &str = "planned";

/// The `built` value of a decision with nothing to build, such as a process ruling.
pub const BUILT_NONE: &str = "none";

/// The most bytes a `built` or `since` value holds, the bound a new version name has.
pub const MAX_VERSION_BYTES: usize = 256;

/// The why of a build-metadata version, the same words a tag name gets.
pub const BUILD_METADATA_WHY: &str =
    "build metadata (+…) is not allowed: it doesn't take part in version order";

/// What a decision's `built` value says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Built {
    /// rdy does not ship it yet.
    Planned,
    /// Nothing to build.
    None,
    /// The first release that ships it.
    Version(Version),
}

/// Parses a release version: one optional leading `v`, then a SemVer 2.0.0 version without build
/// metadata. The error is the reason, with no length bound (a caller bounds new input first).
pub fn parse_release(text: &str) -> Result<Version, String> {
    let bare = text.strip_prefix('v').unwrap_or(text);
    let v = Version::parse(bare).map_err(|e| e.to_string())?;
    if !v.build.is_empty() {
        return Err(BUILD_METADATA_WHY.to_string());
    }
    Ok(v)
}

/// The version `text` names, within the new-input byte bound; `None` for anything else.
pub fn version_of(text: &str) -> Option<Version> {
    if text.len() > MAX_VERSION_BYTES {
        return None;
    }
    parse_release(text).ok()
}

/// `text` spelled as its canonical version, without a leading `v`; `None` when it is no version.
pub fn normalize(text: &str) -> Option<String> {
    version_of(text).map(|v| v.to_string())
}

/// The order of two version spellings by number and prerelease precedence; `None` when either is
/// no version.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    Some(version_of(a)?.cmp(&version_of(b)?))
}

/// What a stored `built` value says; `None` when it is no `planned`, `none` or version.
pub fn parse_built(text: &str) -> Option<Built> {
    match text {
        BUILT_PLANNED => Some(Built::Planned),
        BUILT_NONE => Some(Built::None),
        _ => version_of(text).map(Built::Version),
    }
}

/// Sets the build status a new decision starts with: `none` when it says `needs_content: false`,
/// else `planned`. A block that already has `built`, an explicit value, keeps it; any other type
/// is untouched. Only the authoring create path calls this.
pub fn default_built(block: &mut Block) {
    if block.block_type != rdy_model::decision::DECISION || block.fields.contains_key(BUILT_FIELD) {
        return;
    }
    let value = match block.fields.get("needs_content") {
        Some(FieldValue::Bool(false)) => BUILT_NONE,
        _ => BUILT_PLANNED,
    };
    block
        .fields
        .insert(BUILT_FIELD.to_string(), FieldValue::Str(value.to_string()));
}

/// The definition `built` and `since` take: an optional string.
pub(crate) fn optional_str() -> FieldDef {
    FieldDef {
        kind: FieldKind::Str,
        required: false,
        values: vec![],
    }
}

/// The definition `planned` takes: an optional bool.
pub(crate) fn optional_bool() -> FieldDef {
    FieldDef {
        kind: FieldKind::Bool,
        required: false,
        values: vec![],
    }
}

fn issue(block: &Block, code: &'static str, message: String, fix: String) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        block: Some(block.id.clone()),
        message,
        fix,
    }
}

fn limit(block: &Block, name: &str) -> Issue {
    issue(
        block,
        "E_LIMIT",
        format!("Field {name:?} exceeds its limit."),
        format!("Use at most {MAX_VERSION_BYTES} bytes in {name:?}."),
    )
}

/// Whether `block_type` takes the content override fields `planned` and `since`: a doc-asset
/// type that is no doc or folder. A doc takes `planned` alone.
pub(crate) fn is_content_type(pack: &Pack, block_type: &str) -> bool {
    pack.class_of(block_type) == TypeClass::DocAsset
        && pack.block_types.contains_key(block_type)
        && !tree::is_tree_type(block_type)
}

/// The value checks generic validation cannot make: a decision's `built` and a content block's
/// `since`. A value of the wrong kind is skipped (generic validation reports it). At most one
/// issue per field.
pub(crate) fn check_values(pack: &Pack, block: &Block, out: &mut Vec<Issue>) {
    if block.block_type == rdy_model::decision::DECISION
        && let Some(FieldValue::Str(text)) = block.fields.get(BUILT_FIELD)
    {
        if text.len() > MAX_VERSION_BYTES {
            out.push(limit(block, BUILT_FIELD));
        } else if parse_built(text).is_none() {
            out.push(issue(
                block,
                "E_BAD_INPUT",
                format!("Field {BUILT_FIELD:?} must be planned, none or a release version."),
                "Write built as planned, none, or a version such as 1.2.0.".to_string(),
            ));
        }
    }
    if is_content_type(pack, &block.block_type)
        && let Some(FieldValue::Str(text)) = block.fields.get(SINCE_FIELD)
    {
        if text.len() > MAX_VERSION_BYTES {
            out.push(limit(block, SINCE_FIELD));
        } else if version_of(text).is_none() {
            out.push(issue(
                block,
                "E_BAD_INPUT",
                format!("Field {SINCE_FIELD:?} must be a release version."),
                "Write since as a version such as 1.2.0 or v1.2.0-rc.1.".to_string(),
            ));
        }
    }
}
