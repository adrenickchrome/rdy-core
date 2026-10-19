//! `rdy_sign` and `rdy_key_show` (D-1Z951, D-QVH4N, D-KQ4CA): later signing of a stored landing
//! or tag through the caller's SSH agent, and the configured keys. Neither contacts a hub.
//!
//! `rdy_key_bind`, `rdy_key_retire` and `rdy_key_revoke` (D-XXA9D): thin hub API commands over
//! the key service ([`super::remote`]'s runners). Bind proves possession of the configured key
//! through the caller's agent and returns the hub's confirmation link; retire and revoke change
//! a key's state on the hub. No local signature, signing setting or store record changes.

use super::*;
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::signing_act::run_sign;
use rdy_engine::signing_read::{key_show, signing_config};
use rdy_engine::*;
use rdy_model::Role;
use rdy_model::signature::StatementKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The statement kind a later signature makes.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignKind {
    Approval,
    Relay,
    Tag,
}

impl SignKind {
    /// The statement kind of the engine.
    pub fn statement_kind(self) -> StatementKind {
        match self {
            SignKind::Approval => StatementKind::Approval,
            SignKind::Relay => StatementKind::Relay,
            SignKind::Tag => StatementKind::Tag,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SignParams {
    /// The landing or tag to sign: a landing id (a hash or a unique prefix of 6 or more), or tag:<version>.
    pub target: String,
    /// approval, relay or tag. Default: your own kind for a person (approval or tag), relay for an agent.
    pub kind: Option<SignKind>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyShowParams {}

/// Why a reader session is refused a signature.
pub const READER_CANNOT_SIGN: &str = "role reader cannot sign";
/// How that refusal is fixed.
pub const READER_CANNOT_SIGN_FIX: &str = "start a writer session for this act";

/// `E_PERMISSION` for a reader session that asks for a signature: every surface refuses it
/// before the engine's own subject rules.
pub fn refuse_reader(s: &Session) -> Result<(), RdyError> {
    if s.role == Role::Reader {
        return Err(RdyError::new(
            E_PERMISSION,
            READER_CANNOT_SIGN,
            READER_CANNOT_SIGN_FIX,
        ));
    }
    Ok(())
}

/// `rdy_sign`: the requested signature's check (`run_sign`'s `check`), whether it was made now
/// or was already stored.
pub fn rdy_sign(st: &mut McpState, p: SignParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    refuse_reader(&s)?;
    let kind = p.kind.map(SignKind::statement_kind);
    let signed = run_sign(&*st.engine, &*st.agent, &s, &p.target, kind)?;
    serde_json::to_value(&signed.check).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a signature report did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// `rdy_key_show`: `{key, agent_key, sign}`. Any session may call it.
pub fn rdy_key_show(st: &mut McpState, _p: KeyShowParams) -> Result<Value, RdyError> {
    session(st)?;
    let config = signing_config(&st.engine.root())?;
    key_show(&config, &*st.agent)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyBindParams {
    /// person (signing.key) or agent (signing.agent_key): which configured key to bind.
    pub kind: KeyKindParam,
    /// The key's label on the hub: 1 to 64 characters. Default: "person key" or "agent key".
    pub label: Option<String>,
    /// The remote whose hub binds the key. Default: the share remote, else the only remote.
    pub remote: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyRetireParams {
    /// The key: a public key line or its SHA256 fingerprint.
    pub key: String,
    /// The remote whose hub holds the key. Default: the share remote, else the only remote.
    pub remote: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyRevokeParams {
    /// The key: a public key line or its SHA256 fingerprint.
    pub key: String,
    /// When the key may first have been compromised, as an RFC3339 time in any offset. Default: the hub's request time.
    pub compromised_since: Option<String>,
    /// The remote whose hub holds the key. Default: the share remote, else the only remote.
    pub remote: Option<String>,
}

/// The key commands' caller over MCP: this session, the agent of the process at the other end
/// and the user config of this server's process.
fn key_caller<'a>(st: &'a McpState, s: &'a Session, root: &'a std::path::Path) -> KeyCaller<'a> {
    KeyCaller {
        project: root,
        user_file: user_file_from_env(),
        session: s,
        agent: &*st.agent,
    }
}

/// `rdy_key_bind`: the pending key and the hub's confirmation link. Any session may call it.
pub fn rdy_key_bind(st: &mut McpState, p: KeyBindParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let root = st.engine.root();
    run_key_bind(
        &key_caller(st, &s, &root),
        p.kind,
        p.label.as_deref(),
        p.remote.as_deref(),
    )
}

/// `rdy_key_retire`: the hub's state of the retired key. Any session may call it.
pub fn rdy_key_retire(st: &mut McpState, p: KeyRetireParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let root = st.engine.root();
    run_key_retire(&key_caller(st, &s, &root), &p.key, p.remote.as_deref())
}

/// `rdy_key_revoke`: the hub's state of the revoked key and its times. Any session may call it.
pub fn rdy_key_revoke(st: &mut McpState, p: KeyRevokeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let root = st.engine.root();
    run_key_revoke(
        &key_caller(st, &s, &root),
        &p.key,
        p.compromised_since.as_deref(),
        p.remote.as_deref(),
    )
}
