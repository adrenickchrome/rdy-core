//! ls7: `rdy_site`. The reply is built by `rdy_engine::engine::site_reply`, which the daemon's
//! `site_url` op shares.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::engine::site_reply;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SiteParams {
    /// canon (default), a version as tag:<version>, a view name or view:<name>, or proposal:P-x@n (P-x follows its latest version). A view or proposal renders in review mode and updates as it changes.
    pub scope: Option<String>,
}

/// `rdy_site`: the link to the live docs site for `scope`, for the person to open. A session is
/// required, as for `rdy_inbox`.
pub fn rdy_site(st: &mut McpState, p: SiteParams) -> Result<Value, RdyError> {
    session(st)?;
    site_reply(&*st.engine, p.scope.as_deref())
}
