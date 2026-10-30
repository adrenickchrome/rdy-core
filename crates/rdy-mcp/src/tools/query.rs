//! rdy_query: the blocks a filter selects (D-HZYK0).

use super::*;
use rdy_engine::filter::QUERY_DEFAULT_TOKENS;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// What `rdy_query` takes. The engine owns the token cut, so the reply is not budgeted again.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    /// The filter expression; rdy_schema's filter gives the grammar.
    pub filter: String,
    /// "canon", a view name, "view:<name>", "tag:<version>", "landing:<id>", "proposal:P-<id>@<n>" or "inflight". Defaults to the session view.
    pub scope: Option<String>,
    /// The budget of the reply, at most 100000 (default 4000).
    pub max_tokens: Option<usize>,
    /// next from the previous page, with the same filter and scope.
    pub page: Option<String>,
}

/// `rdy_query`: the page of matches that fits the budget, in the scope's reading order.
pub fn rdy_query(st: &mut McpState, p: QueryParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    let scope_text = p.scope.clone().unwrap_or_else(|| st.view.clone());
    let reply = st_.query_page(
        &scope,
        &scope_text,
        &p.filter,
        p.page.as_deref(),
        p.max_tokens.unwrap_or(QUERY_DEFAULT_TOKENS),
    )?;
    Ok(serde_json::to_value(reply).expect("query json"))
}
