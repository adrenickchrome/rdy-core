//! The `hook` op (FW-HOOK, FW-L3): one `rdy_hook` request (`HookRequest::from_value`), run
//! through the shared check or the engine's local hook host under one run context.
use super::OpContext;
use rdy_engine::hooks::{HOOK_DEADLINE, HookRequest, RunContext};
use rdy_engine::{E_PERMISSION, RdyError};
use serde_json::Value;
use std::time::Instant;

/// The run context of a `hook` op with none of its own: [`HOOK_DEADLINE`] from now.
pub fn default_run() -> RunContext {
    RunContext::new().with_deadline(Instant::now() + HOOK_DEADLINE)
}

/// Runs one hook request under `run`. Readers may check; installing and removing need a
/// session that can write (the person's ordinary local session does). Nothing here reads the
/// session's working directory: every path is absolute or comes from the project.
pub(super) fn hook(cx: &OpContext, args: &Value, run: &RunContext) -> Result<Value, RdyError> {
    let req = HookRequest::from_value(args)?;
    let act = match &req {
        HookRequest::Install(_) => Some("install"),
        HookRequest::Remove(_) => Some("remove"),
        HookRequest::CheckCode(_) | HookRequest::CheckEdit(_) => None,
    };
    if let Some(act) = act
        && !cx.session.role.can_write()
    {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot {act} hooks", cx.session.role),
            "start the session as a writer",
        ));
    }
    rdy_engine::hooks::run(cx.engine, &req, run)
}
