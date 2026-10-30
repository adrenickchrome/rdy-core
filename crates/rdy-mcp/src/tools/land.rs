//! rdy_land (D-ZWP7Q "The agent's landing act", O14): an agent session lands a ready version that
//! the authority policy accepts. The request runs through `rdy_engine::run_land`, the runner the
//! daemon's `land` op shares.
use super::*;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LandParams {
    /// The proposal version to land: P-<id>@<n> as rdy_propose reports it, P-<id> (its latest version) or a version hash.
    pub proposal: String,
}

/// `rdy_land`: lands the version for the session under the authority policy.
pub fn rdy_land(st: &mut McpState, p: LandParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    run_land(&*st.engine, &s, p.proposal)
}
