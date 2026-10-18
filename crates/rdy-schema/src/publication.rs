//! Publication profiles (D-KDFZ3): an ordinary versioned block in the source project that names
//! which docs and folders a publication holds and its audience. The engine injects the type into
//! every pack; generic validation checks it, and this module adds only the value checks generic
//! validation cannot make, and the typed reading of a valid profile.

use crate::pack::{BlockTypeDef, BodyRule, FieldDef, FieldKind, Pack, Trait, TypeClass};
use crate::validate::{Issue, Severity, has_errors, validate_block};
use rdy_model::{Block, BlockId, FieldValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The block type of a publication profile.
pub const PROFILE_TYPE: &str = "publication_profile";

/// The id prefix of a publication profile.
pub const PROFILE_PREFIX: &str = "pf";

/// The most bytes a profile's `title` holds.
pub const MAX_TITLE_BYTES: usize = 256;

/// The most bytes a profile's `description` holds.
pub const MAX_DESCRIPTION_BYTES: usize = 4096;

/// The most ids a profile's `select` holds.
pub const MAX_SELECT_IDS: usize = 1024;

/// The most bytes one entry of a profile's `select` holds.
pub const MAX_SELECTOR_BYTES: usize = 64;

/// Who a publication is for. It chooses the fixed check policy (D-0VCQ5); hub access is separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    Org,
    Public,
}

impl Audience {
    /// The field value: `org` or `public`.
    pub fn as_str(self) -> &'static str {
        match self {
            Audience::Org => "org",
            Audience::Public => "public",
        }
    }

    /// The trail setting of a profile that sets none: on for an org, off for the public.
    pub fn default_trail(self) -> bool {
        self == Audience::Org
    }
}

/// The fields of a valid publication profile. `select` keeps the author's order; selection
/// resolution sorts the ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileFields {
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub audience: Audience,
    pub select: Vec<BlockId>,
    pub trail: bool,
}

fn field(kind: FieldKind, required: bool, values: &[&str]) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: values.iter().map(|s| s.to_string()).collect(),
    }
}

/// The closed definition of the profile type, the same in every pack. `key` is the engine field
/// every type has, outside this map.
pub(crate) fn type_def() -> BlockTypeDef {
    BlockTypeDef {
        prefix: PROFILE_PREFIX.to_string(),
        traits: BTreeSet::from([Trait::Structural]),
        fields: BTreeMap::from([
            (
                "audience".to_string(),
                field(FieldKind::Enum, true, &["org", "public"]),
            ),
            ("description".to_string(), field(FieldKind::Str, false, &[])),
            ("select".to_string(), field(FieldKind::List, true, &[])),
            ("title".to_string(), field(FieldKind::Str, true, &[])),
            ("trail".to_string(), field(FieldKind::Bool, false, &[])),
        ]),
        children: vec![],
        body: BodyRule::Forbidden,
        class: Some(TypeClass::Supporting),
        tracking: BTreeSet::new(),
    }
}

fn err(block: &Block, code: &'static str, message: String, fix: String) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        block: Some(block.id.clone()),
        message,
        fix,
    }
}

fn limit(block: &Block, name: &str, max: usize, unit: &str) -> Issue {
    err(
        block,
        "E_LIMIT",
        format!("Publication field {name:?} exceeds its limit."),
        format!("Use at most {max} {unit} in {name:?}."),
    )
}

/// The value checks generic validation cannot make on a profile, appended after its issues in
/// key, title, description, select order, at most one per field. A field with the wrong kind or
/// absent (generic validation reports both) is skipped; only a missing `key` is reported here,
/// since `key` is no declared field.
pub(crate) fn check_values(block: &Block, out: &mut Vec<Issue>) {
    if !block.fields.contains_key("key") {
        out.push(err(
            block,
            "E_FIELD_REQUIRED",
            format!("type {PROFILE_TYPE} requires field \"key\""),
            "add key (str)".to_string(),
        ));
    }
    if let Some(FieldValue::Str(title)) = block.fields.get("title") {
        if title.len() > MAX_TITLE_BYTES {
            out.push(limit(block, "title", MAX_TITLE_BYTES, "bytes"));
        } else if title.trim().is_empty() {
            out.push(err(
                block,
                "E_BAD_INPUT",
                "Publication title must contain text.".to_string(),
                "Set title to a nonblank string.".to_string(),
            ));
        }
    }
    if let Some(FieldValue::Str(description)) = block.fields.get("description")
        && description.len() > MAX_DESCRIPTION_BYTES
    {
        out.push(limit(block, "description", MAX_DESCRIPTION_BYTES, "bytes"));
    }
    if let Some(FieldValue::List(items)) = block.fields.get("select")
        && let Some(issue) = check_select(block, items)
    {
        out.push(issue);
    }
}

/// The first failing stage of a `select` list: count, string entries, entry lengths, then full
/// distinct ids.
fn check_select(block: &Block, items: &[FieldValue]) -> Option<Issue> {
    if items.is_empty() {
        return Some(err(
            block,
            "E_BAD_INPUT",
            "Publication select must contain at least one id.".to_string(),
            "Add a doc or folder id to select.".to_string(),
        ));
    }
    if items.len() > MAX_SELECT_IDS {
        return Some(limit(block, "select", MAX_SELECT_IDS, "ids"));
    }
    let Some(texts) = items
        .iter()
        .map(FieldValue::as_str)
        .collect::<Option<Vec<&str>>>()
    else {
        return Some(err(
            block,
            "E_FIELD_KIND",
            "field \"select\" must be a list of strings".to_string(),
            "write select as a list of strings".to_string(),
        ));
    };
    if texts.iter().any(|t| t.len() > MAX_SELECTOR_BYTES) {
        return Some(limit(block, "select", MAX_SELECTOR_BYTES, "bytes per id"));
    }
    let mut seen = BTreeSet::new();
    let distinct_ids = texts
        .iter()
        .all(|t| BlockId::parse(t).is_ok_and(|id| seen.insert(id)));
    if !distinct_ids {
        return Some(err(
            block,
            "E_BAD_INPUT",
            "Publication selectors must be distinct full block ids.".to_string(),
            "Use each doc or folder id once in select.".to_string(),
        ));
    }
    None
}

fn not_a_profile(block: &Block) -> Vec<Issue> {
    vec![err(
        block,
        "E_BAD_INPUT",
        format!("Block {} is not a publication profile.", block.id),
        "Use a publication profile id or key.".to_string(),
    )]
}

/// The fields of `block`, a publication profile under `parent`, once it passes the write schema
/// ([`validate_block`], which makes the profile value checks too); else every issue it found.
/// A block of another type is refused with E_BAD_INPUT.
pub fn parse(
    pack: &Pack,
    block: &Block,
    parent: Option<&Block>,
) -> Result<ProfileFields, Vec<Issue>> {
    if block.block_type != PROFILE_TYPE {
        return Err(not_a_profile(block));
    }
    let issues = validate_block(pack, block, parent);
    if has_errors(&issues) {
        return Err(issues);
    }
    // The schema checked each value above, and no pack can redefine the type, so no mismatch
    // can occur here; one would refuse the block rather than read it.
    let text = |name: &str| block.field_str(name).map(str::to_string);
    let audience = match block.field_str("audience") {
        Some("org") => Audience::Org,
        Some("public") => Audience::Public,
        _ => return Err(not_a_profile(block)),
    };
    let select = match block.fields.get("select") {
        Some(FieldValue::List(items)) => items
            .iter()
            .map(|v| v.as_str().and_then(|t| BlockId::parse(t).ok()))
            .collect::<Option<Vec<BlockId>>>(),
        _ => None,
    };
    let trail = match block.fields.get("trail") {
        None => audience.default_trail(),
        Some(FieldValue::Bool(b)) => *b,
        Some(_) => return Err(not_a_profile(block)),
    };
    let (Some(key), Some(title), Some(select)) = (text("key"), text("title"), select) else {
        return Err(not_a_profile(block));
    };
    Ok(ProfileFields {
        key,
        title,
        description: text("description"),
        audience,
        select,
        trail,
    })
}
