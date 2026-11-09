//! rpl4: the engine type `repo_link` (prefix `rl`), a supporting block directly under a doc. It
//! has no body, key or traits. Its fields are `alias`, `repo`, `profile` and an optional `title`.
//! The schema checks each value's grammar; the engine checks that aliases are unique among live
//! links and that the repo differs from this project's own.
use crate::pack::{BlockTypeDef, BodyRule, FieldDef, FieldKind, TypeClass};
use crate::validate::{Issue, Severity};
use rdy_model::{Block, BlockId, FieldValue};
use std::collections::{BTreeMap, BTreeSet};

/// The block type of a repo link.
pub const REPO_LINK: &str = "repo_link";
/// The id prefix of a repo link.
pub const REPO_LINK_PREFIX: &str = "rl";
/// The prefix of the publication profile a link names.
const PROFILE_PREFIX: &str = crate::publication::PROFILE_PREFIX;
/// The most ASCII characters an alias holds.
pub const MAX_ALIAS_CHARS: usize = 24;
/// The hex digits of a repo id.
pub const REPO_ID_DIGITS: usize = 32;
/// The fix of every repo link refusal.
pub const FIX: &str = "correct the repo_link block: rdy_write / rdy edit";

/// The reasons a repo link can fail the schema's checks, in check order.
pub const REASON_ALIAS: &str = "invalid alias";
pub const REASON_REPO: &str = "invalid repo id";
pub const REASON_PROFILE: &str = "invalid profile id";

fn field(kind: FieldKind, required: bool) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: vec![],
    }
}

/// The closed definition of the repo link type, the same in every pack.
pub(crate) fn type_def() -> BlockTypeDef {
    BlockTypeDef {
        prefix: REPO_LINK_PREFIX.to_string(),
        traits: BTreeSet::new(),
        fields: BTreeMap::from([
            ("alias".to_string(), field(FieldKind::Str, true)),
            ("profile".to_string(), field(FieldKind::Str, true)),
            ("repo".to_string(), field(FieldKind::Str, true)),
            ("title".to_string(), field(FieldKind::Str, false)),
        ]),
        children: vec![],
        body: BodyRule::Forbidden,
        class: Some(TypeClass::Supporting),
        tracking: BTreeSet::new(),
    }
}

/// Whether `s` matches `[a-z][a-z0-9-]{0,23}`.
pub fn is_valid_alias(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=MAX_ALIAS_CHARS).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Whether `s` is 32 lowercase hex digits.
pub fn is_valid_repo_id(s: &str) -> bool {
    s.len() == REPO_ID_DIGITS && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is the id of a publication profile.
pub fn is_valid_profile_id(s: &str) -> bool {
    BlockId::is_valid(s) && s.split_once('-').map(|p| p.0) == Some(PROFILE_PREFIX)
}

/// The refusal text of a repo link with `reason`.
pub fn message(alias: &str, reason: &str) -> String {
    format!("repo link {alias}: {reason}")
}

/// The value checks generic validation cannot make, appended after its issues, one per bad field
/// in alias, repo, profile order. A field that is absent or not a string is skipped: generic
/// validation reports both.
pub(crate) fn check_values(block: &Block, out: &mut Vec<Issue>) {
    if block.block_type != REPO_LINK {
        return;
    }
    let text = |name: &str| match block.fields.get(name) {
        Some(FieldValue::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let shown = text("alias").unwrap_or_else(|| block.id.as_str());
    for (name, valid, reason) in [
        ("alias", is_valid_alias as fn(&str) -> bool, REASON_ALIAS),
        ("repo", is_valid_repo_id, REASON_REPO),
        ("profile", is_valid_profile_id, REASON_PROFILE),
    ] {
        if let Some(v) = text(name)
            && !valid(v)
        {
            out.push(Issue {
                severity: Severity::Error,
                code: "E_VALIDATION",
                block: Some(block.id.clone()),
                message: message(shown, reason),
                fix: FIX.to_string(),
            });
        }
    }
}
