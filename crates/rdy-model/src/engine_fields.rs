//! Engine-owned block fields (p-Z8ZHE, tb-7PFW9, p-SG9AJ, p-RNK9B): metadata rdy writes into a
//! block's existing field map, so no stored record gains a field (D-4JMPH, D-F0904).
//!
//! - [`REAFFIRMED`]: for each target block id, the decisions whose retirement the target was
//!   re-affirmed against (or `deleted`), and the reason.
//! - [`REFLECTS_AT`]: the code state a block's `reflects` globs were last checked at.
//! - [`ADOPTED_AS`] (repo links, D-8VJAZ): on an accepted decision, the cited decision that
//!   superseded it upstream (its adopted copy, delivered back from its home project) and the
//!   delivered content the adoption checked.
//!
//! Each is left out of the content hash, so writing one makes no pin stale, and none is ever
//! scanned for references, so a reason that names `{{D-x}}` adds no graph edge.
use crate::foreign::{ForeignId, ForeignPin};
use crate::plan::valid_git_hash;
use crate::{Block, BlockId, FieldValue, Hash, RepoId};
use std::collections::BTreeMap;
use std::fmt;

/// The field recording re-affirmations, on any block type.
pub const REAFFIRMED: &str = "reaffirmed";

/// The field recording the code state a block reflects, on any block type.
pub const REFLECTS_AT: &str = "reflects_at";

/// The field recording an upstream adoption, on an accepted decision (repo links, D-8VJAZ).
pub const ADOPTED_AS: &str = crate::block::ADOPTED_AS;

/// Every engine-owned metadata field, in name order.
pub const ENGINE_FIELDS: [&str; 3] = [ADOPTED_AS, REAFFIRMED, REFLECTS_AT];

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
const CONTENT: &str = "content";
const ENCODING: &str = "encoding";
const ID: &str = "id";
const REPO: &str = "repo";

/// The decision type's id prefix: an `against` entry names a decision.
const DECISION_PREFIX: &str = "D";

/// Whether `name` is an engine-owned metadata field.
pub fn is_engine_field(name: &str) -> bool {
    ENGINE_FIELDS.contains(&name)
}

/// The fix of a write input, a file or an edit buffer that sets or unsets engine field `field`:
/// the operation that writes it.
pub fn input_fix(field: &str) -> String {
    if field == ADOPTED_AS {
        format!("remove {ADOPTED_AS}; use rdy_supersede with {ADOPTED_AS}")
    } else {
        format!("remove {field}; use reaffirm or repin with code: true")
    }
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

/// An upstream adoption (repo links, D-8VJAZ): the cited decision that supersedes an accepted
/// local decision, and the pin of the delivered content the adoption checked. Stored as a map of
/// exactly `content` (64 lowercase hex digits), `encoding` (0 to 65535), `id` (a block id) and
/// `repo` (32 lowercase hex digits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedAs {
    pub target: ForeignId,
    pub pin: ForeignPin,
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

/// The field value of `adopted_as`.
pub fn encode_adopted_as(a: &AdoptedAs) -> FieldValue {
    FieldValue::Map(BTreeMap::from([
        (
            CONTENT.to_string(),
            FieldValue::Str(a.pin.content.to_string()),
        ),
        (
            ENCODING.to_string(),
            FieldValue::Int(i64::from(a.pin.encoding)),
        ),
        (ID.to_string(), FieldValue::Str(a.target.id.as_str().into())),
        (REPO.to_string(), FieldValue::Str(a.target.repo.to_string())),
    ]))
}

/// The map of `v`, which holds exactly the keys `keys` (in name order).
fn exact<'a, const N: usize>(
    field: &'static str,
    v: &'a FieldValue,
    keys: [&str; N],
) -> Result<&'a BTreeMap<String, FieldValue>, EngineFieldError> {
    let FieldValue::Map(m) = v else {
        return fail(field, "an entry must be a map");
    };
    if m.len() != keys.len() || !keys.iter().all(|k| m.contains_key(*k)) {
        let (last, rest) = keys.split_last().expect("an entry has keys");
        return fail(
            field,
            format!("an entry holds exactly {} and {last}", rest.join(", ")),
        );
    }
    Ok(m)
}

/// Whether `s` is `n` lowercase hex digits.
fn lower_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads `adopted_as` back, checking its whole shape.
pub fn decode_adopted_as(v: &FieldValue) -> Result<AdoptedAs, EngineFieldError> {
    const F: &str = ADOPTED_AS;
    let m = exact(F, v, [CONTENT, ENCODING, ID, REPO])?;
    let Some(content) = m[CONTENT]
        .as_str()
        .filter(|s| lower_hex(s, 64))
        .and_then(|s| s.parse::<Hash>().ok())
    else {
        return fail(F, "content is a lowercase BLAKE3 hash");
    };
    let Some(encoding) = (match &m[ENCODING] {
        FieldValue::Int(n) => u16::try_from(*n).ok(),
        _ => None,
    }) else {
        return fail(F, "encoding is a whole number from 0 to 65535");
    };
    let Some(id) = m[ID].as_str().and_then(|s| BlockId::parse(s).ok()) else {
        return fail(F, "id is a block id");
    };
    let Some(repo) = m[REPO]
        .as_str()
        .filter(|s| lower_hex(s, 32))
        .and_then(|s| s.parse::<RepoId>().ok())
    else {
        return fail(F, "repo is 32 lowercase hex digits");
    };
    Ok(AdoptedAs {
        target: ForeignId::new(repo, id),
        pin: ForeignPin { content, encoding },
    })
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
        ADOPTED_AS => Some(decode_adopted_as(v).map(|_| ())),
        REAFFIRMED => Some(decode_reaffirmed(v).map(|_| ())),
        REFLECTS_AT => Some(decode_reflects_at(v).map(|_| ())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adoption() -> AdoptedAs {
        AdoptedAs {
            target: ForeignId::new(RepoId([7; 16]), BlockId::parse("D-PSTRM").unwrap()),
            pin: ForeignPin {
                content: Hash([0xab; 32]),
                encoding: 2,
            },
        }
    }

    /// rpl21: `adopted_as` reads back exactly what rdy wrote, and every other shape is damaged
    /// metadata: a missing or extra key, a value of another kind, uppercase hex, an encoding
    /// past 65535 or below 0.
    #[test]
    fn rpl21_adopted_as_has_a_closed_shape() {
        let a = adoption();
        let v = encode_adopted_as(&a);
        assert_eq!(decode_adopted_as(&v), Ok(a.clone()));
        assert!(check(ADOPTED_AS, &v).is_some_and(|r| r.is_ok()));
        assert!(is_engine_field(ADOPTED_AS));
        assert_eq!(
            input_fix(ADOPTED_AS),
            "remove adopted_as; use rdy_supersede with adopted_as"
        );
        assert_eq!(
            input_fix(REAFFIRMED),
            "remove reaffirmed; use reaffirm or repin with code: true"
        );
        let FieldValue::Map(m) = &v else {
            panic!("{v:?}")
        };
        let with = |k: &str, x: FieldValue| {
            let mut m = m.clone();
            m.insert(k.into(), x);
            FieldValue::Map(m)
        };
        let without = |k: &str| {
            let mut m = m.clone();
            m.remove(k);
            FieldValue::Map(m)
        };
        let bad = [
            FieldValue::Str(format!("{}:D-PSTRM", RepoId([7; 16]))),
            without(CONTENT),
            with("extra", FieldValue::Null),
            with(
                CONTENT,
                FieldValue::Str(Hash([0xab; 32]).to_string().to_uppercase()),
            ),
            with(CONTENT, FieldValue::Str("ab".into())),
            with(ENCODING, FieldValue::Int(65_536)),
            with(ENCODING, FieldValue::Int(-1)),
            with(ENCODING, FieldValue::Str("2".into())),
            with(ID, FieldValue::Str("not an id".into())),
            with(REPO, FieldValue::Str("7".repeat(31))),
            with(REPO, FieldValue::Str("A".repeat(32))),
        ];
        for b in &bad {
            assert!(decode_adopted_as(b).is_err(), "{b:?}");
        }
    }
}
