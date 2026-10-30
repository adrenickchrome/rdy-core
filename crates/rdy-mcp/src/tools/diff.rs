//! rdy_diff: two scopes compared block by block, older to newer (D-HZYK0).

use super::*;
use rdy_engine::diff_request::{DIFF_DEFAULT_TOKENS, DiffRequest};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// What `rdy_diff` takes. The engine owns the token cut, so the reply is not budgeted again.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffParams {
    /// The older scope: canon, a view name, view:<name>, tag:<version>, landing:<id>, proposal:P-<id>@<n> or inflight. Alone, rdy supplies the other side.
    pub from: String,
    /// The newer scope, in the same forms.
    pub to: Option<String>,
    /// Limit both sides to one doc or folder: an id, a key or a path.
    pub doc: Option<String>,
    /// Add field values and word-level body diffs (default false).
    pub detail: Option<bool>,
    /// The budget of the reply, at most 100000 (default 4000).
    pub max_tokens: Option<usize>,
    /// next from the previous page, with the same request.
    pub page: Option<String>,
}

/// `rdy_diff`: the page of the diff that fits the budget, read with the session's view.
pub fn rdy_diff(st: &mut McpState, p: DiffParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let req = DiffRequest {
        from: p.from,
        to: p.to,
        doc: p.doc,
        detail: p.detail.unwrap_or(false),
    };
    let reply = st_.diff_page(
        &req,
        Some(&st.view),
        p.page.as_deref(),
        p.max_tokens.unwrap_or(DIFF_DEFAULT_TOKENS),
    )?;
    Ok(serde_json::to_value(reply).expect("diff json"))
}
