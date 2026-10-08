//! mint1: `rdy_intent` (post, renew, withdraw, list) and its paragraph of `rdy_schema`. The
//! request itself is run by `rdy_engine::intent::run_intent`, the dispatcher the daemon's
//! `intent` op shares.
use crate::tools::{McpState, budget, check_tokens, session};
use rdy_engine::intent::{IntentRequest, run_intent};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IntentParams {
    /// post, renew, withdraw or list.
    pub op: String,
    /// post: the view the intent is about (default: your own); list: only that view's intents.
    pub view: Option<String>,
    /// post: an area from rdy_schema's areas; list: only intents on that area.
    pub area: Option<String>,
    /// post: block ids, keys or paths.
    pub blocks: Option<Vec<String>>,
    /// post: what you mean to do, in a few words (at most 500 characters).
    pub angle: Option<String>,
    /// post and renew: how long, like 30m, 2h or 7d (default 2h, at most 7d).
    pub ttl: Option<String>,
    /// renew and withdraw: the intent id (in-<12 hex>; a unique prefix works).
    pub intent: Option<String>,
    /// list: only intents that cover this block (id, key or path).
    pub block: Option<String>,
    /// list: only this actor's intents.
    pub actor: Option<String>,
    /// list: also expired and withdrawn intents (default false).
    pub include_expired: Option<bool>,
    /// list: cut the answer to about this many tokens.
    pub max_tokens: Option<usize>,
}

/// `rdy_schema`'s paragraph on intents.
pub const INTENT_DOC: &str = "rdy_intent {op, ...}: post {area?, blocks?, angle, ttl?, view?} (an area from rdy_schema's areas, or block ids, keys or paths; ttl like 30m, 2h or 7d, default 2h, at most 7d), renew {intent, ttl?}, withdraw {intent}, list {area?, block?, actor?, view?, include_expired?}. Ids look like in-<12 hex>; a unique prefix works. Expired intents are hidden unless include_expired. Write tools take intent: <id> to record the intent on the change. Codes: E_INTENT_EXPIRED: renewing or withdrawing an expired intent (post a new one).";

/// `rdy_intent`: builds the request field by field and runs it for the session. A post without
/// `view` is about the session's view.
pub fn rdy_intent(st: &mut McpState, p: IntentParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let max_tokens = p.max_tokens;
    let view = match p.op.as_str() {
        "post" => p.view.or_else(|| Some(st.view.clone())),
        _ => p.view,
    };
    let req = IntentRequest {
        op: p.op,
        view,
        area: p.area,
        blocks: p.blocks,
        angle: p.angle,
        ttl: p.ttl,
        intent: p.intent,
        block: p.block,
        actor: p.actor,
        include_expired: p.include_expired,
    };
    let list = req.op == "list";
    let v = run_intent(&*st.engine, &s, req)?;
    Ok(if list { budget(v, max_tokens) } else { v })
}
