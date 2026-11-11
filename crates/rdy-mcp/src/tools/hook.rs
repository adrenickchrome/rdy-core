//! rdy_hook (FW-HOOK D-NNQ0A, D-GHR0Y; FW-L3 D-54CKQ refined by D-DC5D1): the two local warning
//! hooks, checked, installed or removed through the same engine request the daemon's `hook` op
//! and `rdy hook` run. The engine parses the request ([`rdy_engine::hooks::HookRequest`]) and
//! runs it under one run context; this file publishes the closed input schema, applies the
//! session and role rules, and passes the request on. A check warns and never blocks, approves,
//! proposes or lands anything.

use super::*;
use rdy_engine::hooks::{HOOK_DEADLINE, HookRequest, RunContext};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::time::Instant;

/// The tool's description, on every server that lists it.
pub const HOOK_DOC: &str = "Check, install or remove one of rdy's two local warning hooks. op check, kind code: pass repo, checkout, head, paths and message of a commit; it warns when the staged code paths are anchored to a decision and the message has no Implements value. op check, kind edit: pass paths (native absolute); it warns when an agent edited rdy working-copy files directly. op install or remove, kind code: pass repo, a linked code repo. op install or remove, kind edit: pass harness claude-code and optionally worktree. A reply is {kind, checked, warnings} for a check, and {kind, files, warnings} for an install or remove; checked false means the check was incomplete, never clean. A hook warns and changes nothing in the project: it never approves, proposes or lands. Any session may check; install and remove need a writer session on a host that holds the checkout.";

/// rdy_hook's input, as the schema shows it: one closed object with four forms. Check with kind
/// code needs repo, checkout, head, paths and message. Check with kind edit needs paths. Install
/// and remove with kind code need repo. Install and remove with kind edit need harness and take
/// worktree. A field of another form is refused.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookArgs {
    /// The action: check, install or remove.
    #[schemars(extend("enum" = ["check", "install", "remove"]))]
    pub op: String,
    /// Which hook: code (the Git commit-msg hook) or edit (the agent's direct-edit hook).
    #[schemars(extend("enum" = ["code", "edit"]))]
    pub kind: String,
    /// A code_repos name with a local code_paths checkout; not a repo link alias. Kind code only.
    pub repo: Option<String>,
    /// Check, kind code: the native absolute root of your Git worktree, at most 4096 bytes.
    #[schemars(length(max = 4096))]
    pub checkout: Option<String>,
    /// Check, kind code: the worktree's full HEAD commit hash (40 or 64 lowercase hexadecimal characters), or null for an unborn HEAD.
    #[schemars(length(max = 64))]
    pub head: Option<String>,
    /// Check: at most 256 paths of at most 4096 bytes each. Kind code: repo-relative logical paths, deleted paths and both sides of renames included. Kind edit: native absolute paths.
    #[schemars(length(max = 256))]
    pub paths: Option<Vec<String>>,
    /// Check, kind code: the commit message, at most 65536 bytes.
    #[schemars(length(max = 65536))]
    pub message: Option<String>,
    /// Install and remove, kind edit: the agent harness, claude-code.
    #[schemars(extend("enum" = ["claude-code"]))]
    pub harness: Option<String>,
    /// Install and remove, kind edit: a registered rdy worktree. Without it the main project root is the target.
    pub worktree: Option<String>,
}

/// rdy_hook's arguments as sent. The input schema is [`HookArgs`]'s; the engine's one parser
/// reads them, so every refusal is the same on the server, the daemon and the CLI.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct HookParams(pub Map<String, Value>);

impl JsonSchema for HookParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        HookArgs::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        HookArgs::json_schema(generator)
    }
}

/// `rdy_hook`: one check, install or remove, under [`HOOK_DEADLINE`] from now. Any session may
/// check; install and remove need a session that can write, and a host that has local hooks
/// (an embedding without one refuses with `E_BAD_INPUT`).
pub fn rdy_hook(st: &mut McpState, p: HookParams) -> Result<Value, RdyError> {
    let req = HookRequest::from_value(&Value::Object(p.0))?;
    let s = session(st)?;
    let act = match &req {
        HookRequest::Install(_) => Some("install"),
        HookRequest::Remove(_) => Some("remove"),
        HookRequest::CheckCode(_) | HookRequest::CheckEdit(_) => None,
    };
    if let Some(act) = act
        && !s.role.can_write()
    {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot {act} hooks", s.role),
            "start the session as a writer",
        ));
    }
    let run = RunContext::new().with_deadline(Instant::now() + HOOK_DEADLINE);
    rdy_engine::hooks::run(&*st.engine, &req, &run)
}
