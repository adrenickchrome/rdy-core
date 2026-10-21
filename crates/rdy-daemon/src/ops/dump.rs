//! The `dump` op: writes the whole project to one dump file, or with `after` the batches since
//! an earlier dump of this store, the twin of `rdy_dump` (D-FR0C4 F4 and F7: any session, no
//! scope; D-2FMRR).
use super::*;
use rdy_engine::RdyError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DumpArgs {
    out: String,
    /// An earlier dump or increment of this store: write the batches after its end.
    after: Option<String>,
}

pub(super) fn dump(cx: &OpContext, a: DumpArgs) -> Result<Value, RdyError> {
    let out = resolve_path(cx.cwd, &a.out);
    let state = cx.engine.read();
    let summary = match &a.after {
        Some(after) => state.dump_after_file(&resolve_path(cx.cwd, after), &out)?,
        None => state.dump(&out)?,
    };
    Ok(to_json(&summary))
}
