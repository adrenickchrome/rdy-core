//! mten1: `rdy_tensions` and `rdy_resolve`. The requests are run by
//! `rdy_engine::tensions::api::{run_tensions, run_resolve}`, the dispatchers the daemon's
//! `tensions` and `resolve` ops share; `rdy_schema`'s paragraph is `TENSIONS_DOC`.
use super::*;
use rdy_engine::tensions::api::{
    DismissRequest, ResolveRequest, TensionsRequest, run_resolve, run_tensions,
};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TensionsParams {
    /// inflight (default: every tension), canon (only tensions whose sides are all canon) or a
    /// view name (tensions with a change side in that view).
    pub scope: Option<String>,
    /// Only this kind: edit_collision, fork, premise_shift, area_overlap, duplicate_definition,
    /// unjustified_change, deprecated_reference or value_disagreement.
    pub kind: Option<String>,
    /// Only this severity: blocking or advisory.
    pub severity: Option<String>,
    /// open (default) or dismissed.
    pub status: Option<String>,
    /// Only tensions with a change side in this view (the same as a scope that is a view).
    pub view: Option<String>,
    /// Only tensions that involve a block in this area (one of rdy_schema's areas).
    pub area: Option<String>,
    /// Only tensions that involve this block (id, key or path) or this change (a hex prefix of
    /// at least 6 digits).
    pub involving: Option<String>,
    /// Also list every dismissal review: who dismissed which tension, why, and whether the
    /// dismissal still holds.
    pub reviews: Option<bool>,
    /// Cut the answer to about this many tokens (blocking tensions come first).
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveParams {
    /// The tension to dismiss: tn-<12 hex> from rdy_tensions (a unique prefix works).
    pub tension: String,
    /// Dismiss the tension, with a reason. A tension is otherwise resolved by a change.
    pub dismiss: Option<DismissParams>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DismissParams {
    /// Why the tension needs no action, in a few words (at most 2000 characters).
    pub reason: String,
}

/// `rdy_tensions`: lists tensions, filtered, for the session.
pub fn rdy_tensions(st: &mut McpState, p: TensionsParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    run_tensions(
        &*st.engine,
        &s,
        TensionsRequest {
            scope: p.scope,
            kind: p.kind,
            severity: p.severity,
            status: p.status,
            view: p.view,
            area: p.area,
            involving: p.involving,
            reviews: p.reviews,
            max_tokens: p.max_tokens,
        },
    )
}

/// `rdy_resolve`: dismisses a tension with a reason (a review).
pub fn rdy_resolve(st: &mut McpState, p: ResolveParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    run_resolve(
        &*st.engine,
        &s,
        ResolveRequest {
            tension: p.tension,
            dismiss: p.dismiss.map(|d| DismissRequest { reason: d.reason }),
        },
    )
}
