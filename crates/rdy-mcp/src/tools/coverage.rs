//! `rdy_coverage`: the coverage report of a scope or a path (FW-CV1, D-KE84J). The daemon's
//! `coverage` op builds its reply with [`coverage_reply`], so both doors cut by the same budget.
use super::*;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageParams {
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// Only the docs at or under this path (a doc, a folder or a block's path).
    pub path: Option<String>,
    /// Cut the answer to about this many tokens (the longest list is shortened).
    pub max_tokens: Option<usize>,
}

/// The report of `scope` (narrowed to `path`) as JSON, cut to `max_tokens`: the longest list is
/// shortened, and the reply says `truncated` and `omitted`.
pub fn coverage_reply(
    st: &ReadState,
    scope: &Scope,
    path: Option<&str>,
    max_tokens: Option<usize>,
) -> Result<Value, RdyError> {
    let report = st.coverage_report(scope, path)?;
    let v = serde_json::to_value(report).expect("coverage report json");
    Ok(budget(v, max_tokens))
}

/// `rdy_coverage`: the report of the scope (default the session's view) or a path.
pub fn rdy_coverage(st: &mut McpState, p: CoverageParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    coverage_reply(&st_, &scope, p.path.as_deref(), p.max_tokens)
}
