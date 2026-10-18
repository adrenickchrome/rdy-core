//! The signing ops (D-1Z951, D-QVH4N): `sign`, a later signature of a stored landing or tag
//! through the caller's agent, and `key_show`, the configured keys. Neither contacts a hub.
use super::*;
use rdy_engine::signing_act::run_sign;
use rdy_engine::signing_read::signing_config;
use rdy_mcp::tools::{SignKind, refuse_reader};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignArgs {
    target: String,
    kind: Option<SignKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeyShowArgs {}

/// `sign`: `{subject, check, already_signed}`. A reader session is refused before the engine's
/// subject rules; the caller's agent signs (`cx.agent`).
pub(super) fn sign(cx: &OpContext, a: SignArgs) -> Result<Value, RdyError> {
    refuse_reader(cx.session)?;
    let kind = a.kind.map(SignKind::statement_kind);
    let r = run_sign(cx.engine, cx.agent, cx.session, &a.target, kind)?;
    Ok(json!({
        "subject": r.subject.to_string(),
        "check": to_json(&r.check),
        "already_signed": r.already_signed,
    }))
}

/// `key_show`: `{key, agent_key, sign}`, asking the caller's agent only when a key is set.
pub(super) fn key_show(cx: &OpContext, _a: KeyShowArgs) -> Result<Value, RdyError> {
    let config = signing_config(&cx.engine.root())?;
    rdy_engine::signing_read::key_show(&config, cx.agent)
}
