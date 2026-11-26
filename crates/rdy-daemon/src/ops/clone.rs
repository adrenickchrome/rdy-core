//! The `clone` op: clones the daemon's project into a directory with no store, the twin of
//! `rdy_clone` without `file` (D-9QV85, D-D2NPN; O6: any session). A dump file clones in the
//! caller's process (`rdy clone <file>`), so the op takes none. With `url` it clones that hub
//! repo instead, with the session's claim and this process's user config (D-D5J18, D-47HVF): a
//! URL clone never forks. With `at` and `fork` it forks the project at that tag (D-99CWC):
//! `at` needs `fork`, checked first, then the source form.
//!
//! Platform (D-1RXEA): none here; the engine's clone uses `std::io::pipe` and a thread.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::clone::{CloneSource, at_needs_fork, clone_at, clone_url, url_fork};
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::history_fetch::PAUSED_NOTE;
use rdy_engine::hub_credentials::require_user_file;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CloneArgs {
    dir: String,
    fork: Option<bool>,
    url: Option<String>,
    at: Option<String>,
}

pub(super) fn clone(cx: &OpContext, a: CloneArgs) -> Result<Value, RdyError> {
    let fork = a.fork.unwrap_or(false);
    if a.at.is_some() && !fork {
        return Err(at_needs_fork());
    }
    if let Some(url) = &a.url {
        if fork {
            return Err(url_fork());
        }
        let user_file = require_user_file(user_file_from_env())?;
        let dir = resolve_path(cx.cwd, &a.dir);
        let mut summary = clone_url(cx.session, url, &dir, &user_file)?;
        // the history goes on in the new project's own daemon, between calls (O17)
        if summary.history_pending() {
            let project = std::path::PathBuf::from(&summary.project);
            match cx.engine.continue_clone_history(&project, cx.session) {
                Ok(true) => {}
                Ok(false) => summary.notes.push(PAUSED_NOTE.to_string()),
                Err(e) => {
                    summary.warn(&e);
                    summary.notes.push(PAUSED_NOTE.to_string());
                }
            }
        }
        return Ok(to_json(&summary));
    }
    let st = cx.engine.read();
    let summary = clone_at(
        CloneSource::State(&st),
        &resolve_path(cx.cwd, &a.dir),
        fork,
        a.at.as_deref(),
    )?;
    Ok(to_json(&summary))
}
