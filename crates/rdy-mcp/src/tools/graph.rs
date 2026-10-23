//! rdy_context, rdy_impact, rdy_suspects and rdy_trace: one block's neighbourhood and its why.

use super::*;
use rdy_engine::trace::{TRACE_DEFAULT_DEPTH, TRACE_DEFAULT_TOKENS, TraceOptions, check_depth};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// How many versions `rdy_context` carries of a block's history.
pub const CONTEXT_HISTORY: usize = 20;

pub fn rdy_context(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let (scope, id) = read_target(&st.view, &st_, &p.id, &p.scope)?;
    let ctx = st_.context(&scope, &id)?;
    let mut v = serde_json::to_value(ctx).expect("context json");
    if let Some(c) = st_.conflict_info(&scope, &id)? {
        v["conflict"] = json!(c);
    }
    // cgr1: inflight is an analysis scope with no history; its context carries `inflight` instead
    if scope != Scope::Inflight {
        let h = st_.history(&scope, &id, CONTEXT_HISTORY)?;
        v["justification"] = json!(h.entries.first().and_then(|e| e.justification.clone()));
        v["history_more"] = json!(h.more);
        v["history"] = json!(h.entries);
    }
    Ok(budget(v, p.max_tokens))
}

pub fn rdy_impact(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    let id = st_.resolve_id(&scope, &p.id)?;
    let items = st_.impact(&scope, &id, p.depth.unwrap_or(3))?;
    let mut v = json!({"items": items});
    let by_area = rdy_engine::by_area(&items);
    if !by_area.is_empty() {
        v["by_area"] = json!(by_area);
    }
    Ok(budget(v, p.max_tokens))
}

pub fn rdy_suspects(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    Ok(budget(
        json!({"items": st_.suspect_details(&scope)?}),
        p.max_tokens,
    ))
}

/// What `rdy_trace` takes (D-1876T). It takes no page, cursor, mutation, output-file or
/// timestamp: a trace is one bounded read of one captured scope.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TraceParams {
    /// A block id or a path. Also `id@version` or a whole ref line `[id@version] …`, read under that version.
    pub id: String,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>", "proposal:P-<id>@<n>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// How many steps of lineage to walk, 1 to 8 (default 3). A value outside that range is refused, a negative one too.
    pub depth: Option<i64>,
    /// Add the recorded evidence: authored options, sealed plans, landing evidence and the whole re-affirmation record.
    pub detail: Option<bool>,
    /// The budget of the reply, at most 100000 (default 1000). A cut drops whole decision rows, deepest first, and names each in frontier.
    pub max_tokens: Option<usize>,
}

/// The fix of a trace root that names no block.
pub const TRACE_UNKNOWN_FIX: &str = "check the id with rdy_search";

/// The options of a trace from a boundary's arguments (D-1876T): the same token limit as every
/// read, then the closed depth range (a signed depth, so -1 is refused as 0 and 9 are), and the
/// defaults of a trace: depth 3, no detail and 1000 tokens, not the 4000 of a generic read.
pub fn trace_options(
    depth: Option<i64>,
    detail: Option<bool>,
    max_tokens: Option<usize>,
) -> Result<TraceOptions, RdyError> {
    check_tokens(max_tokens)?;
    Ok(TraceOptions {
        depth: depth.map_or(Ok(TRACE_DEFAULT_DEPTH), check_depth)?,
        detail: detail.unwrap_or(false),
        max_tokens: max_tokens.unwrap_or(TRACE_DEFAULT_TOKENS),
    })
}

/// A refusal of the root of a trace with the trace's own fix when the root names no block; the
/// scope's `E_NOT_FOUND` and a reference's parse errors are kept as they are.
pub fn trace_root_error(mut e: RdyError) -> RdyError {
    if e.code == E_UNKNOWN_BLOCK {
        e.fix = TRACE_UNKNOWN_FIX.into();
    }
    e
}

/// `rdy_trace`: why a block says what it says, from the recorded decisions and landings of one
/// scope (D-1876T). The engine owns the token cut: the reply is not budgeted again.
pub fn rdy_trace(st: &mut McpState, p: TraceParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let options = trace_options(p.depth, p.detail, p.max_tokens)?;
    let st_ = st.engine.read();
    let (scope, id) = read_target(&st.view, &st_, &p.id, &p.scope).map_err(trace_root_error)?;
    let reply = st_.trace(&scope, &id, options)?;
    Ok(serde_json::to_value(reply).expect("trace json"))
}
