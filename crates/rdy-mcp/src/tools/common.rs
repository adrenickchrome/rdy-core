//! Shared state, limits and helpers of every tool family.

use crate::read_page;
use rdy_engine::engine::Engine;
use rdy_engine::signing_crypto::{SigningAgent, UnavailableAgent};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const E_NO_SESSION: &str = "E_NO_SESSION";

/// A daemon of the same version and another build serves this client (D-CCC6E).
pub const W_DAEMON_BUILD: &str = "W_DAEMON_BUILD";

/// A call lacks the current value from its tool's ruleset (D-Y18E1). The call still runs.
pub const W_KUNGFU: &str = "W_KUNGFU";

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

pub use rdy_engine::tool_reads::MAX_TOKENS_LIMIT;

pub(super) fn limit(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_LIMIT, what, fix)
}

pub(crate) use rdy_engine::tool_reads::check_tokens;

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
    /// The sessions connected to this server (ib10): `rdy_session` tells it who this one is.
    /// `RdyServer::new` sets the shared hub; `McpState::new` starts with a hub of its own.
    pub hub: Arc<crate::notify::NotifyHub>,
    /// The caller's SSH agent: the process at the other end of this session. `McpState::new`
    /// starts with none ([`UnavailableAgent`]); `RdyServer::with_agent` sets it.
    pub agent: Arc<dyn SigningAgent>,
    /// D-4PHYQ: the actor and run this connection is bound to: the first valid explicit claim,
    /// by `rdy_session` or `_meta`. Set once, for the connection's life.
    pub bound: Option<crate::claims::Key>,
    /// What the running call's `_meta` states, while the call runs ([`crate::claims::enter`]).
    pub claim: Option<crate::claims::Asked>,
    /// This connection's client is another build of the daemon's version (D-CCC6E); `None` when
    /// it is not, and with no daemon.
    pub build_warning: Option<DaemonBuildWarning>,
    /// The warning this call's reply carries, once taken: its size is reserved in the reply.
    pub build_warning_due: Option<Value>,
    /// The project's local count of kungfu misses (D-Y18E1). `RdyServer::new` sets it; a session
    /// with no recorder counts nothing and works the same.
    pub kungfu_counts: Option<rdy_engine::kungfu_counts::Recorder>,
}

/// What W_DAEMON_BUILD names; the daemon gives it to the session it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonBuildWarning {
    pub version: String,
    pub daemon_build: String,
    pub client_build: String,
}

impl DaemonBuildWarning {
    /// The warning as a reply's `warnings` array carries it.
    pub fn to_json(&self) -> Value {
        json!({
            "code": W_DAEMON_BUILD,
            "message": format!(
                "the running daemon is another build of rdy {} ({}); this client is build {}",
                self.version, self.daemon_build, self.client_build
            ),
            "fix": "close the other clients; the daemon then restarts on this build. Or run rdy serve --stop",
        })
    }

    /// The tokens the warning takes in a reply, with its `warnings` key.
    pub fn tokens(&self) -> usize {
        (self.to_json().to_string().len() + r#","warnings":[]"#.len()).div_ceil(4)
    }
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
            hub: crate::notify::NotifyHub::new(),
            agent: Arc::new(UnavailableAgent),
            bound: None,
            claim: None,
            build_warning: None,
            build_warning_due: None,
            kungfu_counts: None,
        }
    }

    /// This session's id in the hub: the daemon's session id, else the process-unique owner.
    pub fn hub_sid(&self) -> u64 {
        self.sid.unwrap_or(self.lease_owner)
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeParams {
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>", "proposal:P-<id>@<n>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdParams {
    /// A block id or a path. rdy_context also takes `id@version` or a whole ref line `[id@version] …`, and reads under that version.
    pub id: String,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>", "proposal:P-<id>@<n>" or "view:<name>". Defaults to the session view.
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

/// The refusal of a call with no session: no claim, and none bound.
pub(crate) fn no_session() -> RdyError {
    RdyError::new(
        E_NO_SESSION,
        "no session",
        "state rdy/actor and rdy/run in _meta, or call rdy_session",
    )
}

pub(crate) fn session(st: &McpState) -> Result<Session, RdyError> {
    st.session.clone().ok_or_else(no_session)
}

/// The scope a tool reads, resolved on the state the tool reads (`read`), so that a view created
/// by a later publish is never accepted here and then read on a state without it. `view` is the
/// session view `s` resolves against, and the scope when `s` is none.
pub(crate) use rdy_engine::tool_reads::scope_of;

pub(super) use rdy_engine::tool_reads::read_target;
