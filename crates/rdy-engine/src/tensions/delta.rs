//! What one write or view op did to the tension set: the tensions it opened and those it
//! resolved, among those with a change side in the view (`on_change`).
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
/// end, one at a time, each counted in `collapsed` under its kind. A blocking tension and the
/// resolved list are never cut.
pub fn on_change(
    before: &ReadState,
    after: &ReadState,
    view: &str,
    max_tokens: Option<usize>,
) -> Result<OnChange, RdyError> {
    let (opened, resolved) = before.tensions()?.diff(&*after.tensions()?);
    let now: BTreeSet<Hash> = after.included(view).iter().copied().collect();
    let was: BTreeSet<Hash> = before.included(view).iter().copied().collect();
    let mut out = OnChange {
        tensions: opened
            .iter()
            .filter(|t| has_change_side(t, &now))
            .map(|t| after.tension_info(t))
            .collect(),
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
    let size = |o: &OnChange| {
        serde_json::to_string(o)
            .expect("on_change serializes: strings and string-keyed maps only")
            .len()
    };
    if size(&out) > budget && out.tensions.iter().any(|t| !t.sides.is_empty()) {
        for t in &mut out.tensions {
            t.sides.clear();
        }
        out.detail_cut = true;
    }
    while size(&out) > budget {
        let Some(i) = out
            .tensions
            .iter()
            .rposition(|t| t.severity == TensionSeverity::Advisory)
        else {
            break;
        };
        let dropped = out.tensions.remove(i);
        *out.collapsed
            .entry(dropped.kind.name().to_string())
            .or_default() += 1;
    }
    Ok(out)
}
