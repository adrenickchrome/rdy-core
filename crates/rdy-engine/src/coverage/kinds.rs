//! The open entity kinds of a scope (FW-EN4): how many entity blocks carry each `kind` value,
//! and the groups of kinds that differ only in spelling.
use crate::Materialized;
use rdy_model::Block;
use rdy_model::entity::{ENTITY, KIND};
use serde::Serialize;
use std::collections::BTreeMap;

/// One open `kind` value and how many entity blocks carry it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub entities: usize,
}

/// A kind's normal form: lowercase, without spaces, `-` and `_`, and without one final `s` when
/// more than 3 characters remain.
fn normal_kind(kind: &str) -> String {
    let mut n: String = kind
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect();
    if n.ends_with('s') && n.chars().count() > 4 {
        n.pop();
    }
    n
}

/// The kinds of `blocks`, by kind, and the groups of kinds that differ only in case, spacing,
/// `-`, `_` or a final `s`.
pub(crate) fn kinds_over<'a>(
    blocks: impl Iterator<Item = &'a Block>,
) -> (Vec<KindCount>, Vec<Vec<String>>) {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for b in blocks {
        if b.block_type == ENTITY
            && let Some(k) = b.field_str(KIND).map(str::trim).filter(|k| !k.is_empty())
        {
            *kinds.entry(k.to_string()).or_default() += 1;
        }
    }
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for k in kinds.keys() {
        groups.entry(normal_kind(k)).or_default().push(k.clone());
    }
    // the kinds are visited in byte order, so each group is already sorted
    let mut similar: Vec<Vec<String>> = groups.into_values().filter(|g| g.len() > 1).collect();
    similar.sort();
    let counts = kinds
        .into_iter()
        .map(|(kind, entities)| KindCount { kind, entities })
        .collect();
    (counts, similar)
}

thread_local! {
    static KINDS_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: this thread's count of whole-scope kind counts (`kinds_of` calls). A write
/// reply's coverage change must leave it unchanged.
#[doc(hidden)]
pub fn kinds_builds() -> u64 {
    KINDS_BUILDS.with(std::cell::Cell::get)
}

/// The kinds of every block of `m` and the groups of similar kinds. Counted by
/// [`kinds_builds`].
pub(crate) fn kinds_of(m: &Materialized) -> (Vec<KindCount>, Vec<Vec<String>>) {
    KINDS_BUILDS.with(|c| c.set(c.get() + 1));
    kinds_over(m.iter())
}
