//! msrch1: `rdy_search`, the find-text tool. The search itself is `ReadState::search`.
use crate::tools::{McpState, check_tokens, scope_of, session};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// The text to find; with regex: true, a pattern (Rust regex syntax, at most 1024 bytes).
    pub query: String,
    /// Treat query as a regular expression (default false: literal text).
    pub regex: Option<bool>,
    /// Match regardless of case (default false).
    pub ignore_case: Option<bool>,
    /// "canon", a view name, "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// The budget for this page of matches (default 4000).
    pub max_tokens: Option<usize>,
    /// Continue a cut search: its next cursor, with the same query, flags and scope.
    pub page: Option<String>,
}

pub fn rdy_search(st: &mut McpState, p: SearchParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    let r = st_.search(
        &scope,
        &SearchQuery {
            text: p.query,
            regex: p.regex.unwrap_or(false),
            ignore_case: p.ignore_case.unwrap_or(false),
            ..Default::default()
        },
        p.page.as_deref(),
        p.max_tokens,
    )?;
    serde_json::to_value(&r).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a search result did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}
