//! rf1: the two supporting types the engine injects into every pack (D-Z8P09, D-4WC6R). A
//! finding (`F-`) states what an agent found and holds its evidence: evidence is a field of a
//! finding, not a type. A source (`S-`) names a release, commit or spec that a finding's evidence
//! or a block's `reflects` link cites. Packs may add optional fields to either, nothing else.
use crate::pack::{FieldDef, FieldKind, Trait};
use crate::validate::{Issue, err};
use rdy_model::{Block, BlockId, FieldValue};
use std::collections::BTreeMap;

/// The block type of a finding.
pub const FINDING: &str = "finding";
/// The id prefix of a finding.
pub const FINDING_PREFIX: &str = "F";
/// The block type of a source.
pub const SOURCE: &str = "source";
/// The id prefix of a source.
pub const SOURCE_PREFIX: &str = "S";
/// The link kind from a block to the source it reflects: an engine link kind in every pack.
pub const REFLECTS_LINK: &str = "reflects";
/// The finding field holding its evidence entries.
pub const EVIDENCE_FIELD: &str = "evidence";
/// The kinds of thing a source names, in order.
pub const SOURCE_KINDS: [&str; 3] = ["release", "commit", "spec"];

/// The fewest entries one finding's evidence holds.
pub const MIN_EVIDENCE_ENTRIES: usize = 1;
/// The most entries one finding's evidence holds.
pub const MAX_EVIDENCE_ENTRIES: usize = 20;
/// The most Unicode scalar values one evidence entry value holds (and at least one).
pub const MAX_EVIDENCE_VALUE_CHARS: usize = 2000;

/// The keys of a quote entry: `quote` is required, the other two optional.
pub const QUOTE_ENTRY_KEYS: [&str; 3] = ["quote", "session_ref", "question"];
/// The one key of a source entry.
pub const SOURCE_ENTRY_KEY: &str = "source";
/// Every key an evidence entry may hold, for a did-you-mean hint on an unknown one.
pub const EVIDENCE_KEYS: [&str; 4] = ["question", "quote", "session_ref", "source"];

/// The message of evidence of the wrong shape (TASK-507 (4)).
pub const EVIDENCE_MESSAGE: &str = "field \"evidence\" must be a list of 1 to 20 evidence entries";
/// The fix of evidence of the wrong shape (TASK-507 (4), quoted by D-Z8P09).
pub const EVIDENCE_FIX: &str =
    "write evidence as a list of {quote, session_ref?, question?} or {source: \"{{S-…}}\"} entries";

/// Whether a pack may give a finding or a source trait `t`: never normative, defines or
/// presentational.
pub(crate) fn trait_allowed(t: Trait) -> bool {
    !matches!(t, Trait::Normative | Trait::Defines | Trait::Presentational)
}

fn f(kind: FieldKind, required: bool, values: &[&str]) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: values.iter().map(|s| s.to_string()).collect(),
    }
}

/// The core fields of a finding.
pub(crate) fn finding_core() -> BTreeMap<String, FieldDef> {
    [
        ("statement", f(FieldKind::Str, true, &[])),
        (EVIDENCE_FIELD, f(FieldKind::List, true, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The core fields of a source.
pub(crate) fn source_core() -> BTreeMap<String, FieldDef> {
    [
        ("title", f(FieldKind::Str, true, &[])),
        ("kind", f(FieldKind::Enum, true, &SOURCE_KINDS)),
        ("locator", f(FieldKind::Str, true, &[])),
        ("version", f(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The shape check of a finding's `evidence`: one `E_FIELD_KIND` issue when the field is present
/// and is not a list of 1 to 20 entries, each exactly `{quote, session_ref?, question?}` or
/// `{source}`, every value a string of 1 to 2000 Unicode scalar values, and a source value
/// exactly one `{{S-…}}` reference to a full source id. Empty for another type or a missing field
/// (the required-field check reports that).
pub fn validate_evidence(block: &Block) -> Vec<Issue> {
    if block.block_type != FINDING {
        return vec![];
    }
    match block.fields.get(EVIDENCE_FIELD) {
        Some(v) if !evidence_ok(v) => vec![err(
            block,
            "E_FIELD_KIND",
            EVIDENCE_MESSAGE.to_string(),
            EVIDENCE_FIX.to_string(),
        )],
        _ => vec![],
    }
}

/// The first key, by entry order then key order, of an evidence map entry that no entry shape
/// has, with its entry's index. Shape is not checked: a mixed entry has no unknown key, and a
/// value that is not a list, or an entry that is not a map, has none.
pub fn first_unknown_evidence_key(evidence: &FieldValue) -> Option<(usize, &str)> {
    let FieldValue::List(entries) = evidence else {
        return None;
    };
    entries.iter().enumerate().find_map(|(i, e)| match e {
        FieldValue::Map(m) => m
            .keys()
            .find(|k| !EVIDENCE_KEYS.contains(&k.as_str()))
            .map(|k| (i, k.as_str())),
        _ => None,
    })
}

fn evidence_ok(v: &FieldValue) -> bool {
    let FieldValue::List(entries) = v else {
        return false;
    };
    (MIN_EVIDENCE_ENTRIES..=MAX_EVIDENCE_ENTRIES).contains(&entries.len())
        && entries.iter().all(entry_ok)
}

fn entry_ok(e: &FieldValue) -> bool {
    let FieldValue::Map(m) = e else {
        return false;
    };
    let values_ok = m.values().all(|v| {
        v.as_str().is_some_and(|s| {
            (1..=MAX_EVIDENCE_VALUE_CHARS)
                .contains(&s.chars().take(MAX_EVIDENCE_VALUE_CHARS + 1).count())
        })
    });
    if !values_ok {
        return false;
    }
    if let Some(src) = m.get(SOURCE_ENTRY_KEY) {
        return m.len() == 1 && src.as_str().is_some_and(is_source_ref);
    }
    m.contains_key("quote") && m.keys().all(|k| QUOTE_ENTRY_KEYS.contains(&k.as_str()))
}

/// Whether `s` is exactly `{{S-xxxxx}}`: one reference to a full source id, with no label, mark,
/// space or other text.
fn is_source_ref(s: &str) -> bool {
    s.strip_prefix("{{")
        .and_then(|r| r.strip_suffix("}}"))
        .is_some_and(|id| {
            BlockId::is_valid(id) && id.split_once('-').map(|p| p.0) == Some(SOURCE_PREFIX)
        })
}
