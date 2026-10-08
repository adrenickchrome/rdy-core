//! rdy_context, rdy_impact and rdy_suspects: one block's neighbourhood.

use super::*;
use rdy_engine::*;
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
        json!({"items": st_.suspects(&scope)?}),
        p.max_tokens,
    ))
}
