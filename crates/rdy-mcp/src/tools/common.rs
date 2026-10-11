//! Shared state, limits and helpers of every tool family.

use crate::read_page;
use rdy_engine::engine::Engine;
use rdy_engine::*;
use rdy_model::BlockId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const E_NO_SESSION: &str = "E_NO_SESSION";

/// The most ops one `rdy_write` may carry.
pub const MAX_OPS_PER_WRITE: usize = 5_000;

/// The longest JSON-RPC request line rdy reads, on every transport: stdio, the proxy and the daemon's MCP socket.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// The largest asset (decoded bytes) `rdy_asset` accepts.
pub const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;

/// Room left in a request line for the JSON-RPC envelope around an asset's base64.
pub const REQUEST_ENVELOPE_MARGIN: usize = 4096;

/// The largest asset (raw bytes) whose base64 fits in one stdio request line with its envelope;
/// over stdio this, not [`MAX_ASSET_BYTES`], is the effective limit.
pub const MAX_ASSET_BYTES_STDIO: usize = (MAX_REQUEST_BYTES - REQUEST_ENVELOPE_MARGIN) / 4 * 3;

/// `rdy_schema`'s note on the request-line limit, the same on both transports.
pub const TRANSPORT_NOTE: &str = "one request is one JSON line of at most 16 MiB (request_bytes); the daemon's socket carries the same line limit as stdio (socket_line_bytes), so asset_bytes_stdio, the largest asset whose base64 fits in one line, binds on both";

/// The largest `max_tokens` any read may ask for.
pub const MAX_TOKENS_LIMIT: usize = 100_000;

pub(super) fn limit(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_LIMIT, what, fix)
}

pub(crate) fn check_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
    match max_tokens {
        Some(n) if n > MAX_TOKENS_LIMIT => Err(limit(
            format!("max_tokens {n} is over the limit of {MAX_TOKENS_LIMIT}"),
            format!("ask for at most {MAX_TOKENS_LIMIT} tokens and page through the rest"),
        )),
        _ => Ok(()),
    }
}

/// Warning codes that are advisory: reported once per doc per MCP session (adv1).
pub const ADVISORY_ONCE: [&str; 1] = ["W_SKELETON_MISSING"];

pub struct McpState {
    /// The one door to the project: reads from the published state, writes on the writer.
    pub engine: Arc<dyn Engine>,
    pub session: Option<Session>,
    pub view: String,
    /// The daemon's id for this connection (`SessionId.0`); `None` for stdio.
    pub sid: Option<u64>,
    /// The pack generation this session has heard of (its `pack_changed` notice).
    pub seen_gen: u64,
    /// The advisories this session has reported, as `(code, block or "", message)` (adv1).
    /// Cleared by a successful `rdy_session`.
    pub reported: std::collections::BTreeSet<(String, String, String)>,
    /// The cut reads this session can page on, by token (rpag1). In memory only.
    pub reads: read_page::ReadRegistry,
    /// Who holds this session's review leases: the daemon's session id (`RdyServer::new` sets it
    /// from `sid`), else a process-unique owner.
    pub lease_owner: u64,
}

impl McpState {
    pub fn new(engine: Arc<dyn Engine>) -> McpState {
        let seen_gen = engine.pack_generation();
        McpState {
            engine,
            session: None,
            view: "work".into(),
            sid: None,
            seen_gen,
            reported: Default::default(),
            reads: Default::default(),
            lease_owner: rdy_engine::semantic::lease::LeaseRegistry::local_owner(),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeParams {
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdParams {
    /// A block id or a path. rdy_context also takes `id@version` or a whole ref line `[id@version] …`, and reads under that version.
    pub id: String,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// rdy_impact only: traversal depth (default 3).
    pub depth: Option<u32>,
    pub max_tokens: Option<usize>,
}

pub fn error_json(e: &RdyError) -> Value {
    serde_json::to_value(e)
        .unwrap_or_else(|_| json!({"code": e.code, "message": e.message, "fix": e.fix}))
}

/// adv1: drop the advisory warnings (`ADVISORY_ONCE`) this session already reported and count
/// them in `warnings_repeated`. The `warnings` array stays, possibly empty.
pub(crate) fn report_once(st: &mut McpState, mut v: Value) -> Value {
    let Some(warnings) = v.get_mut("warnings").and_then(Value::as_array_mut) else {
        return v;
    };
    let mut dropped = 0u64;
    warnings.retain(|w| {
        let code = w["code"].as_str().unwrap_or("");
        if !ADVISORY_ONCE.contains(&code) {
            return true;
        }
        let key = (
            code.to_string(),
            w["block"].as_str().unwrap_or("").to_string(),
            w["message"].as_str().unwrap_or("").to_string(),
        );
        let fresh = st.reported.insert(key);
        if !fresh {
            dropped += 1;
        }
        fresh
    });
    if dropped > 0 {
        v["warnings_repeated"] = json!(dropped);
    }
    v
}

pub(crate) fn session(st: &McpState) -> Result<Session, RdyError> {
    st.session.clone().ok_or_else(|| {
        RdyError::new(
            E_NO_SESSION,
            "no session",
            "call rdy_session first with your actor, run and role",
        )
    })
}

/// The scope a tool reads, resolved on the state the tool reads (`read`), so that a view created
/// by a later publish is never accepted here and then read on a state without it. `view` is the
/// session view `s` resolves against, and the scope when `s` is none.
pub(crate) fn scope_of(
    view: &str,
    read: &ReadState,
    s: &Option<String>,
) -> Result<Scope, RdyError> {
    match s {
        // scope1: an unknown view is not found, unless it is this session's own view
        Some(text) => read.resolve_read_scope(text, Some(view)),
        // ses1: a reader session whose view is canon reads canon by default
        None if view == "canon" => Ok(Scope::Canon),
        None => Ok(Scope::View(view.to_string())),
    }
}

/// refv1: the scope and the block id of a read call whose `id` may be `id@version` or a ref line.
/// A pinned version reads under `tag:<version>`; an explicit scope that differs is refused.
pub(super) fn read_target(
    view: &str,
    read: &ReadState,
    id: &str,
    scope: &Option<String>,
) -> Result<(Scope, BlockId), RdyError> {
    let t = rdy_model::reference::parse_target(id)
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::BAD_REF_FIX))?;
    let pinned = t
        .scope_text(scope.as_deref())
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::PIN_CLASH_FIX))?;
    let scope = match pinned {
        Some(text) => read.resolve_read_scope(&text, Some(view))?,
        None => scope_of(view, read, scope)?,
    };
    let bid = read.resolve_id(&scope, &t.id)?;
    Ok((scope, bid))
}
