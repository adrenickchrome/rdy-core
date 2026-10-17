//! The `clone` op: clones the daemon's project into a directory with no store, the twin of
//! `rdy_clone` without `file` (D-9QV85, D-D2NPN; O6: any session). A dump file clones in the
//! caller's process (`rdy clone <file>`), so the op takes none.
//!
//! Platform (D-1RXEA): none here; the engine's clone uses `std::io::pipe` and a thread.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::clone::{CloneSource, clone as clone_into};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CloneArgs {
    dir: String,
    fork: Option<bool>,
}

pub(super) fn clone(cx: &OpContext, a: CloneArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let summary = clone_into(
        CloneSource::State(&st),
        &resolve_path(cx.cwd, &a.dir),
        a.fork.unwrap_or(false),
    )?;
    Ok(to_json(&summary))
}
