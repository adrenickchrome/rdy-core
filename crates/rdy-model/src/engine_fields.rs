//! Engine-owned block fields (p-Z8ZHE, tb-7PFW9, p-SG9AJ, p-RNK9B): metadata rdy writes into a
//! block's existing field map, so no stored record gains a field (D-4JMPH, D-F0904).
//!
//! - [`REAFFIRMED`]: for each target block id, the decisions whose retirement the target was
//!   re-affirmed against (or `deleted`), and the reason.
//! - [`REFLECTS_AT`]: the code state a block's `reflects` globs were last checked at.
//!
//! Both are left out of the content hash, so writing one makes no pin stale, and neither is ever
//! scanned for references, so a reason that names `{{D-x}}` adds no graph edge.
use crate::plan::valid_git_hash;
use crate::{Block, BlockId, FieldValue, Hash};
use std::collections::BTreeMap;
use std::fmt;

/// The field recording re-affirmations, on any block type.
pub const REAFFIRMED: &str = "reaffirmed";

/// The field recording the code state a block reflects, on any block type.
pub const REFLECTS_AT: &str = "reflects_at";

/// Every engine-owned metadata field, in name order.
pub const ENGINE_FIELDS: [&str; 2] = [REAFFIRMED, REFLECTS_AT];

/// The most entries one engine-field map or list holds.
pub const MAX_ENGINE_ENTRIES: usize = 100_000;

/// The most Unicode scalar values one re-affirmation reason holds.
pub const MAX_REASON_CHARS: usize = 2000;

/// The `against` value of a target that was deleted.
pub const DELETED: &str = "deleted";

const AGAINST: &str = "against";
const REASON: &str = "reason";
const FILES: &str = "files";
const COMMITS: &str = "commits";

/// The decision type's id prefix: an `against` entry names a decision.
const DECISION_PREFIX: &str = "D";

/// Whether `name` is an engine-owned metadata field.
pub fn is_engine_field(name: &str) -> bool {
    ENGINE_FIELDS.contains(&name)
}

/// What a target was re-affirmed against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Against {
    /// The retiring decisions, sorted and distinct. Empty records a changed target's reason
    /// beside its refreshed pin (O18).
    Retiring(Vec<BlockId>),
    /// The target was deleted.
    Deleted,
}

/// One target's re-affirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaffirmation {
    pub against: Against,
    /// Trimmed, 1 to [`MAX_REASON_CHARS`] Unicode scalar values.
    pub reason: String,
}

/// The code state a block was checked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectsAt {
    /// The BLAKE3 hash of the reflected files.
    pub files: Hash,
    /// Each repository's commit (40 or 64 lowercase hex digits), or `None` for an unborn one.
    pub commits: BTreeMap<String, Option<String>>,
}

/// A stored engine field that does not have its shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineFieldError {
    pub field: &'static str,
    pub problem: String,
}

impl fmt::Display for EngineFieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "field {:?}: {}", self.field, self.problem)
    }
}

impl std::error::Error for EngineFieldError {}

fn fail<T>(field: &'static str, problem: impl Into<String>) -> Result<T, EngineFieldError> {
    Err(EngineFieldError {
        field,
        problem: problem.into(),
    })
}

/// The field value of `reaffirmed`.
pub fn encode_reaffirmed(targets: &BTreeMap<BlockId, Reaffirmation>) -> FieldValue {
    FieldValue::Map(
        targets
            .iter()
            .map(|(target, r)| {
                let against = match &r.against {
                    Against::Deleted => FieldValue::Str(DELETED.into()),
                    Against::Retiring(ids) => FieldValue::List(
                        ids.iter()
                            .map(|d| FieldValue::Str(d.as_str().into()))
                            .collect(),
                    ),
                };
                let entry = BTreeMap::from([
                    (AGAINST.to_string(), against),
                    (REASON.to_string(), FieldValue::Str(r.reason.clone())),
                ]);
                (target.as_str().to_string(), FieldValue::Map(entry))
            })
            .collect(),
    )
}

/// The field value of `reflects_at`.
pub fn encode_reflects_at(at: &ReflectsAt) -> FieldValue {
    let commits = at
        .commits
        .iter()
        .map(|(repo, c)| {
            let v = match c {
                Some(c) => FieldValue::Str(c.clone()),
                None => FieldValue::Null,
            };
            (repo.clone(), v)
        })
        .collect();
    FieldValue::Map(BTreeMap::from([
        (COMMITS.to_string(), FieldValue::Map(commits)),
        (FILES.to_string(), FieldValue::Str(at.files.to_string())),
    ]))
}

/// The map of `v`, which holds exactly the keys `keys` (in name order).
fn exact<'a>(
    field: &'static str,
    v: &'a FieldValue,
    keys: [&str; 2],
) -> Result<&'a BTreeMap<String, FieldValue>, EngineFieldError> {
    let FieldValue::Map(m) = v else {
        return fail(field, "an entry must be a map");
    };
    if m.len() != keys.len() || !keys.iter().all(|k| m.contains_key(*k)) {
        return fail(
            field,
            format!("an entry holds exactly {} and {}", keys[0], keys[1]),
        );
    }
    Ok(m)
}

/// Reads `reaffirmed` back, checking its whole shape.
pub fn decode_reaffirmed(
    v: &FieldValue,
) -> Result<BTreeMap<BlockId, Reaffirmation>, EngineFieldError> {
    const F: &str = REAFFIRMED;
    let FieldValue::Map(targets) = v else {
        return fail(F, "must be a map from target id to its re-affirmation");
    };
    if targets.len() > MAX_ENGINE_ENTRIES {
        return fail(F, format!("holds more than {MAX_ENGINE_ENTRIES} targets"));
    }
    let mut out = BTreeMap::new();
    for (target, entry) in targets {
        let Ok(target_id) = BlockId::parse(target) else {
            return fail(F, format!("{target:?} is not a block id"));
        };
        let m = exact(F, entry, [AGAINST, REASON])?;
        let against = match &m[AGAINST] {
            FieldValue::Str(s) if s == DELETED => Against::Deleted,
            FieldValue::List(items) => {
                if items.len() > MAX_ENGINE_ENTRIES {
                    return fail(
                        F,
                        format!("against holds more than {MAX_ENGINE_ENTRIES} decisions"),
                    );
                }
                let mut ids: Vec<BlockId> = Vec::with_capacity(items.len());
                for item in items {
                    let Some(d) = item
                        .as_str()
                        .and_then(|s| BlockId::parse(s).ok())
                        .filter(|d| d.prefix() == DECISION_PREFIX)
                    else {
                        return fail(F, "against lists decision ids");
                    };
                    if ids.last().is_some_and(|last| *last >= d) {
                        return fail(F, "against is sorted with no repeats");
                    }
                    ids.push(d);
                }
                Against::Retiring(ids)
            }
            _ => return fail(F, "against is a list of decision ids or \"deleted\""),
        };
        let Some(reason) = m[REASON].as_str() else {
            return fail(F, "reason is text");
        };
        let chars = reason.chars().count();
        if reason.trim() != reason || chars == 0 || chars > MAX_REASON_CHARS {
            return fail(
                F,
                format!("reason is trimmed text of 1 to {MAX_REASON_CHARS} characters"),
            );
        }
        out.insert(
            target_id,
            Reaffirmation {
                against,
                reason: reason.to_string(),
            },
        );
    }
    Ok(out)
}

/// The causes a version re-affirms, read from the block delta between `before` (the block as
/// it stood, if any) and `after`: each cause whose re-affirmation `after` records and `before`
/// does not record the same way, or whose pin `after` moved (a second re-affirmation of a cause
/// with the same reason writes the same entry, and only its pin moves; a frozen decision's pins
/// move only by a re-affirmation). Each comes with the reason `after` records, in cause-id
/// order. A field that does not read as a re-affirmation map counts as none. Plan computation,
/// its code-evidence closes and the pending queue all read re-affirmation through this one rule.
pub fn reaffirmed_causes(before: Option<&Block>, after: &Block) -> Vec<(BlockId, String)> {
    let read = |b: Option<&Block>| {
        b.and_then(|b| b.fields.get(REAFFIRMED))
            .and_then(|v| decode_reaffirmed(v).ok())
            .unwrap_or_default()
    };
    let old = read(before);
    let pin = |b: Option<&Block>, t: &BlockId| b.and_then(|b| b.pins.get(t).copied());
    read(Some(after))
        .into_iter()
        .filter(|(t, r)| old.get(t) != Some(r) || pin(before, t) != pin(Some(after), t))
        .map(|(t, r)| (t, r.reason))
        .collect()
}

/// Reads `reflects_at` back, checking its whole shape.
pub fn decode_reflects_at(v: &FieldValue) -> Result<ReflectsAt, EngineFieldError> {
    const F: &str = REFLECTS_AT;
    let m = exact(F, v, [COMMITS, FILES])?;
    let Some(files) = m[FILES].as_str().and_then(|s| s.parse::<Hash>().ok()) else {
        return fail(F, "files is a lowercase BLAKE3 hash");
    };
    let FieldValue::Map(commits) = &m[COMMITS] else {
        return fail(F, "commits is a map from repository name to commit");
    };
    if commits.len() > MAX_ENGINE_ENTRIES {
        return fail(
            F,
            format!("commits holds more than {MAX_ENGINE_ENTRIES} repositories"),
        );
    }
    let mut out = BTreeMap::new();
    for (repo, c) in commits {
        let c = match c {
            FieldValue::Null => None,
            FieldValue::Str(s) if valid_git_hash(s) => Some(s.clone()),
            _ => {
                return fail(
                    F,
                    "a commit is a lowercase Git hash, or null for an unborn repository",
                );
            }
        };
        out.insert(repo.clone(), c);
    }
    Ok(ReflectsAt {
        files,
        commits: out,
    })
}

/// Checks the stored shape of engine field `name`: `None` for any other field.
pub fn check(name: &str, v: &FieldValue) -> Option<Result<(), EngineFieldError>> {
    match name {
        REAFFIRMED => Some(decode_reaffirmed(v).map(|_| ())),
        REFLECTS_AT => Some(decode_reflects_at(v).map(|_| ())),
        _ => None,
    }
}
