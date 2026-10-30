//! The diff and query ops: two scopes compared block by block, and the blocks a filter selects.
use super::*;
use rdy_engine::{DiffRequest, RdyError};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DiffArgs {
    from: String,
    to: Option<String>,
    doc: Option<String>,
    detail: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryArgs {
    filter: String,
    scope: Option<String>,
}

/// dq7: the whole diff of the request, read with the session's default view (D-HZYK0).
pub(super) fn diff(cx: &OpContext, a: DiffArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let req = DiffRequest {
        from: a.from,
        to: a.to,
        doc: a.doc,
        detail: a.detail.unwrap_or(false),
    };
    Ok(to_json(
        &st.diff_reply(&req, Some(&cx.session.default_view()))?,
    ))
}

/// dq7: every block the filter selects in the scope (canon by default), in reading order.
pub(super) fn query(cx: &OpContext, a: QueryArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.query(&scope, &a.filter)?))
}
