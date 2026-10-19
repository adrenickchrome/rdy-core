//! The signing ops (D-1Z951, D-QVH4N): `sign`, a later signature of a stored landing or tag
//! through the caller's agent, and `key_show`, the configured keys. Neither contacts a hub.
//!
//! The hub key ops (D-XXA9D): `key_bind`, `key_retire` and `key_revoke`, as the key tools run
//! them. A bind is signed by the caller's agent (`cx.agent`), never the daemon's own. The user
//! config, and so the credentials, are the daemon process's own (in a direct run, the CLI's).
use super::*;
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::signing_act::run_sign;
use rdy_engine::signing_read::signing_config;
use rdy_mcp::tools::{
    KeyBindParams, KeyCaller, KeyRetireParams, KeyRevokeParams, SignKind, refuse_reader,
    run_key_bind, run_key_retire, run_key_revoke,
};
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

/// The key ops' caller: the session, the caller's agent and this process's user config.
fn key_caller<'a>(cx: &'a OpContext<'a>, root: &'a Path) -> KeyCaller<'a> {
    KeyCaller {
        project: root,
        user_file: user_file_from_env(),
        session: cx.session,
        agent: cx.agent,
    }
}

/// `key_bind`: `{key, kind, state, confirm_url, remote, warnings}`.
pub(super) fn key_bind(cx: &OpContext, p: KeyBindParams) -> Result<Value, RdyError> {
    let root = cx.engine.root();
    run_key_bind(
        &key_caller(cx, &root),
        p.kind,
        p.label.as_deref(),
        p.remote.as_deref(),
    )
}

/// `key_retire`: `{key, state, remote, warnings}`.
pub(super) fn key_retire(cx: &OpContext, p: KeyRetireParams) -> Result<Value, RdyError> {
    let root = cx.engine.root();
    run_key_retire(&key_caller(cx, &root), &p.key, p.remote.as_deref())
}

/// `key_revoke`: `{key, state, revoked_at, compromised_from, remote, warnings}`.
pub(super) fn key_revoke(cx: &OpContext, p: KeyRevokeParams) -> Result<Value, RdyError> {
    let root = cx.engine.root();
    run_key_revoke(
        &key_caller(cx, &root),
        &p.key,
        p.compromised_since.as_deref(),
        p.remote.as_deref(),
    )
}
