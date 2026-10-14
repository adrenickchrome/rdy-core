//! The `dump` op: writes the whole project to one dump file, the twin of `rdy_dump` (D-FR0C4 F4
//! and F7: any session, no scope).
use super::*;
use rdy_engine::RdyError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DumpArgs {
    out: String,
}

pub(super) fn dump(cx: &OpContext, a: DumpArgs) -> Result<Value, RdyError> {
    Ok(to_json(
        &cx.engine.read().dump(&resolve_path(cx.cwd, &a.out))?,
    ))
}
