//! rdy_propose: a proposal for a change.

use super::*;
use rdy_engine::consequence::propose_and_report;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeParams {
    pub view: Option<String>,
    pub title: String,
    /// Propose only these changes (ids from rdy_view show). The set must be closed under dependencies, or it is E_DEPS_MISSING. Omit to propose all the view's changes. A new version of an open proposal replaces the old one.
    pub changes: Option<Vec<String>>,
    /// Your plain-words summary of the proposal, for the person who approves it. Stored with the version; a new version without it keeps the last one.
    pub summary: Option<String>,
    /// The budget for the consequences in the result.
    pub max_tokens: Option<usize>,
    /// A proposal from a member view of an alternative set (rdy_alternatives) names the set and the view's own member: {set, chosen}. Landing it chooses that member and closes the others.
    pub alternatives: Option<crate::alt_tools::AltPickParams>,
}

pub fn rdy_propose(st: &mut McpState, p: ProposeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v))
        .unwrap_or_else(|| st.view.clone());
    let changes = p.changes.clone();
    let alternatives = p.alternatives.map(rdy_engine::alt::AltPick::from);
    propose_and_report(&*st.engine, p.max_tokens, move |r| {
        r.propose_with(
            &s,
            &view,
            &p.title,
            changes.as_deref(),
            p.summary.as_deref(),
            alternatives.as_ref(),
        )
    })
}
