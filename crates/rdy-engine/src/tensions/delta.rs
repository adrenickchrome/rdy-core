//! What one write or view op did to the tension set: the tensions it opened and those it
//! resolved, among those with a change side in the view (`on_change`).
use super::info::{InfoCache, row_bytes, rows_bytes};
use super::set::TensionDelta;
use super::{SideRef, Tension, TensionInfo, TensionKind, TensionSeverity, tension_label};
use crate::ReadState;
use crate::brief::BRIEF_DEFAULT_TOKENS;
use crate::error::RdyError;
use rdy_model::Hash;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The `on_change` part of a write or view op reply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OnChange {
    /// Tensions the state after has and the state before had not, by id.
    pub tensions: Vec<TensionInfo>,
    /// Tensions the state before had and the state after has not, by id.
    pub resolved: Vec<ResolvedNote>,
    /// The budget cut every tension's `sides`; `rdy_tensions {involving: <change>}` has them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub detail_cut: bool,
    /// Advisory tensions the budget left out, by kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub collapsed: BTreeMap<String, usize>,
}

/// A tension a write or view op resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedNote {
    /// `tn-<12 hex>`.
    pub id: String,
    /// The full id, 64 hex.
    pub tension: String,
    pub kind: TensionKind,
}

/// Whether `t` has a change side among `changes`.
fn has_change_side(t: &Tension, changes: &BTreeSet<Hash>) -> bool {
    t.sides
        .iter()
        .any(|s| matches!(s, SideRef::Change(c) if changes.contains(c)))
}

/// What the step from `before` to `after` did to the tension set, as seen from `view`: the
/// tensions it opened with a change side in `after`'s view, and those it resolved with a change
/// side in `before`'s view. An all-canon tension has no change side, so it is never listed.
///
/// The reply is cut to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`; four bytes a token) in
/// this order: the sides of every tension go (`detail_cut`), then advisory tensions go from the
/// end, each counted in `collapsed` under its kind. A blocking tension and the resolved list are
/// never cut. The cut is linear: each row is measured once, against a running total. A reply
/// whose rows overflow the budget without their sides builds no sides at all, and an advisory
/// tension the cut collapses is never built (Q15: cut detail, never coverage).
pub fn on_change(
    before: &ReadState,
    after: &ReadState,
    view: &str,
    max_tokens: Option<usize>,
    delta: &TensionDelta,
) -> Result<OnChange, RdyError> {
    let (opened, resolved) = (&delta.opened, &delta.resolved);
    let now: BTreeSet<Hash> = after.included(view).iter().copied().collect();
    let was: BTreeSet<Hash> = before.included(view).iter().copied().collect();
    let listed: Vec<&Tension> = opened
        .iter()
        .map(|t| &**t)
        .filter(|t| has_change_side(t, &now))
        .collect();
    let mut out = OnChange {
        resolved: resolved
            .iter()
            .filter(|t| has_change_side(t, &was))
            .map(|t| ResolvedNote {
                id: tension_label(&t.id),
                tension: t.id.to_string(),
                kind: t.kind,
            })
            .collect(),
        ..OnChange::default()
    };
    let budget = max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS).saturating_mul(4);
    let mut rows = Rows::new(after, &listed);

    // the detail: the rows with their sides, if the bare rows leave room for them
    let frame = json_len(&out);
    if rows.bare_fit(budget.saturating_sub(frame)) {
        let mut cache = InfoCache::default();
        let bare: Vec<usize> = (0..listed.len()).map(|i| rows.bytes(i)).collect();
        let full: Vec<TensionInfo> = listed
            .iter()
            .zip(rows.take_all())
            .map(|(t, mut row)| {
                row.sides = after.sides_info(t, &mut cache);
                row
            })
            .collect();
        let bytes: usize = full.iter().map(row_bytes).sum();
        if frame + rows_bytes(bytes, full.len()) <= budget {
            out.tensions = full;
            return Ok(out);
        }
        rows.reuse(full, bare);
    }
    out.detail_cut = listed.iter().any(|t| !t.sides.is_empty());

    // every blocking row, then the advisory ones from the front while they fit; the rest are
    // counted by kind, unbuilt
    let frame = json_len(&out);
    let advisory: Vec<usize> = (0..listed.len())
        .filter(|&i| listed[i].severity == TensionSeverity::Advisory)
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for &i in &advisory {
        *counts.entry(listed[i].kind.name().to_string()).or_default() += 1;
    }
    let (mut bytes, mut kept) = (0usize, 0usize);
    for (i, t) in listed.iter().enumerate() {
        if t.severity != TensionSeverity::Advisory {
            bytes += rows.bytes(i);
            kept += 1;
        }
    }
    let total = |bytes: usize, kept: usize, counts: &BTreeMap<String, usize>| {
        frame + rows_bytes(bytes, kept) + collapsed_len(counts)
    };
    // first against the widest count map (every advisory row collapsed), then exactly: a row
    // is far wider than what its count adds, so the size grows with each row kept and the
    // exact pass adds at most a row or two
    let widest = collapsed_len(&counts);
    let mut k = 0;
    while k < advisory.len()
        && frame + rows_bytes(bytes + rows.bytes(advisory[k]), kept + 1) + widest <= budget
    {
        bytes += rows.bytes(advisory[k]);
        kept += 1;
        uncount(&mut counts, listed[advisory[k]]);
        k += 1;
    }
    while k < advisory.len() {
        let mut next = counts.clone();
        uncount(&mut next, listed[advisory[k]]);
        let b = bytes + rows.bytes(advisory[k]);
        if total(b, kept + 1, &next) > budget {
            break;
        }
        (bytes, kept, counts) = (b, kept + 1, next);
        k += 1;
    }
    let keep: BTreeSet<usize> = advisory[..k].iter().copied().collect();
    out.tensions = (0..listed.len())
        .filter(|i| listed[*i].severity != TensionSeverity::Advisory || keep.contains(i))
        .map(|i| rows.take(i))
        .collect();
    out.collapsed = counts;
    debug_assert_eq!(json_len(&out), total(bytes, kept, &out.collapsed));
    Ok(out)
}

/// The JSON length of `o`.
fn json_len(o: &OnChange) -> usize {
    serde_json::to_string(o)
        .expect("on_change serializes: strings and string-keyed maps only")
        .len()
}

/// The bytes `collapsed` adds to an `OnChange` (none when empty).
fn collapsed_len(counts: &BTreeMap<String, usize>) -> usize {
    if counts.is_empty() {
        return 0;
    }
    ",\"collapsed\":".len()
        + serde_json::to_string(counts)
            .expect("a count map serializes")
            .len()
}

/// Takes `t` out of the collapsed counts: it is kept.
fn uncount(counts: &mut BTreeMap<String, usize>, t: &Tension) {
    let name = t.kind.name();
    if let Some(n) = counts.get_mut(name) {
        *n -= 1;
        if *n == 0 {
            counts.remove(name);
        }
    }
}

/// The bare rows (no sides) of the listed tensions, each built and measured on first use.
struct Rows<'a> {
    st: &'a ReadState,
    listed: &'a [&'a Tension],
    rows: Vec<Option<(TensionInfo, usize)>>,
}

impl<'a> Rows<'a> {
    fn new(st: &'a ReadState, listed: &'a [&'a Tension]) -> Self {
        Rows {
            st,
            listed,
            rows: listed.iter().map(|_| None).collect(),
        }
    }

    fn get(&mut self, i: usize) -> &mut (TensionInfo, usize) {
        let (st, t) = (self.st, self.listed[i]);
        self.rows[i].get_or_insert_with(|| {
            let row = st.tension_info_bare(t);
            let n = row_bytes(&row);
            (row, n)
        })
    }

    /// Row `i`'s JSON length.
    fn bytes(&mut self, i: usize) -> usize {
        self.get(i).1
    }

    /// Row `i`, taken out.
    fn take(&mut self, i: usize) -> TensionInfo {
        self.get(i);
        self.rows[i].take().expect("built above").0
    }

    /// Every row, taken out.
    fn take_all(&mut self) -> Vec<TensionInfo> {
        (0..self.listed.len()).map(|i| self.take(i)).collect()
    }

    /// Whether every row fits `room` bytes bare, building rows from the front only until they
    /// overflow it.
    fn bare_fit(&mut self, room: usize) -> bool {
        let mut used = 0usize;
        for i in 0..self.listed.len() {
            used += self.bytes(i) + usize::from(i > 0);
            if used > room {
                return false;
            }
        }
        true
    }

    /// Takes back the rows `take_all` gave, built with their sides since: the sides go, which
    /// leaves each row as it was bare, at its bare length `bare`.
    fn reuse(&mut self, full: Vec<TensionInfo>, bare: Vec<usize>) {
        for ((slot, mut row), n) in self.rows.iter_mut().zip(full).zip(bare) {
            row.sides.clear();
            *slot = Some((row, n));
        }
    }
}
