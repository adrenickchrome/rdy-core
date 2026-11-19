//! The `clone` op: clones the daemon's project into a directory with no store, the twin of
//! `rdy_clone` without `file` (D-9QV85, D-D2NPN; O6: any session). A dump file clones in the
//! caller's process (`rdy clone <file>`), so the op takes none. With `url` it clones that hub
//! repo instead, with the session's claim and this process's user config (D-D5J18, D-47HVF): a
//! URL clone never forks.
//!
//! Platform (D-1RXEA): none here; the engine's clone uses `std::io::pipe` and a thread.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::clone::{CloneSource, clone as clone_into, clone_url, url_fork};
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::hub_credentials::require_user_file;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CloneArgs {
    dir: String,
    fork: Option<bool>,
    url: Option<String>,
}

pub(super) fn clone(cx: &OpContext, a: CloneArgs) -> Result<Value, RdyError> {
    if let Some(url) = &a.url {
        if a.fork == Some(true) {
            return Err(url_fork());
        }
        let user_file = require_user_file(user_file_from_env())?;
        let summary = clone_url(cx.session, url, &resolve_path(cx.cwd, &a.dir), &user_file)?;
        return Ok(to_json(&summary));
    }
    let st = cx.engine.read();
    let summary = clone_into(
        CloneSource::State(&st),
        &resolve_path(cx.cwd, &a.dir),
        a.fork.unwrap_or(false),
    )?;
    Ok(to_json(&summary))
}
