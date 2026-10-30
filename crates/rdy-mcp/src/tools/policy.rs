//! rdy_policy (D-ZWP7Q, O20): starts the authority policy. `init` writes a proposed decision and
//! the policy block from the engine's template, then proposes the view for a person to approve.
//! The request runs through `rdy_engine::policy::init::run_policy`, the runner the daemon's
//! `policy` op shares.
use super::*;
use rdy_engine::policy::init::{PolicyRequest, run_policy};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyParams {
    /// init: starts the authority policy. It is the only op.
    pub op: String,
    /// init: the doc that holds the policy block (path, key or id). Required: a folder or no doc is refused.
    pub doc: Option<String>,
    /// init: the view to write in and propose (default: the session's own view).
    pub view: Option<String>,
}

/// `rdy_policy`: runs the request for the session.
pub fn rdy_policy(st: &mut McpState, p: PolicyParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let req = PolicyRequest {
        op: p.op,
        doc: p.doc,
        view: Some(view),
    };
    run_policy(&*st.engine, &s, req)
}
