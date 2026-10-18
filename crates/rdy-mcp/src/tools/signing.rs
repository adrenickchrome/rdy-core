//! `rdy_sign` and `rdy_key_show` (D-1Z951, D-QVH4N, D-KQ4CA): later signing of a stored landing
//! or tag through the caller's SSH agent, and the configured keys. Neither contacts a hub.

use super::*;
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
