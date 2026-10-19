//! The `remote` op (D-B8N21, p-38V30): the remotes in effect, or one offline edit at one config
//! level, as `rdy_remote` runs it. The user config is the daemon process's own (in a direct run,
//! the CLI's): a changed `XDG_CONFIG_HOME` reaches the daemon when it restarts.
use super::*;
use rdy_engine::config_levels::user_file_from_env;
use rdy_mcp::tools::{RemoteParams, remote_request, run_remote};

/// `remote`: `{remotes, warnings}`, or `{remote, removed, warnings}` for an edit.
pub(super) fn remote(cx: &OpContext, p: RemoteParams) -> Result<Value, RdyError> {
    let req = remote_request(p)?;
    let found = user_file_from_env();
    run_remote(&cx.engine.root(), found.as_deref(), req)
}
