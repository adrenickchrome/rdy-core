//! `rdy_remote` (D-B8N21, p-38V30, D-KQ4CA) and the hub key commands' shared runners
//! (D-XXA9D, D-1Z951): the remote edits and the key bind, retire and revoke that the MCP tools,
//! the daemon's ops and so the CLI all run. Each surface is a thin wrapper around these, so
//! their checks, texts and replies are the same.
//!
//! - **Remote edits are offline.** [`run_remote`] calls the engine's remote service, which
//!   reads and writes settings files only: no connection, credential or project store.
//! - **Key commands call the key service.** [`run_key_bind`], [`run_key_retire`] and
//!   [`run_key_revoke`] call `KeyService`, which selects the remote, gets access from the
//!   credential service, builds the keybind statement with SIG's builder and has the caller's
//!   agent sign it. Nothing here reads a token, encodes a statement or retries a request.
//! - **One transport per call.** [`KeyTransport`] builds the HTTPS transport on the call's
//!   first request, fenced to the install of the selected remote, and drops it when the call
//!   returns, inside the blocking tool or op run.
//! - **The user config is the caller process's.** Each surface passes the user config of its
//!   own process (`config_levels::user_file_from_env`): through the daemon, the daemon's. A
//!   changed `XDG_CONFIG_HOME` takes effect for the daemon when it restarts.

use super::*;
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::hub_config::{HubConfig, RemoteMode};
use rdy_engine::hub_credentials::require_user_file;
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_keys::KeyService;
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService, user_config_unavailable};
use rdy_engine::hub_types::{HubCall, HubFailure, HubReply, HubTransport, KeyKind, SystemHubClock};
use rdy_engine::signing_crypto::SigningAgent;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What `rdy_remote` does.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOp {
    List,
    Add,
    Rm,
    SetMode,
}

impl RemoteOp {
    /// The op's name in errors.
    pub fn name(self) -> &'static str {
        match self {
            RemoteOp::List => "list",
            RemoteOp::Add => "add",
            RemoteOp::Rm => "rm",
            RemoteOp::SetMode => "set_mode",
        }
    }
}

/// How a remote is used.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteModeParam {
    Share,
    Publish,
}

impl From<RemoteModeParam> for RemoteMode {
    fn from(m: RemoteModeParam) -> RemoteMode {
        match m {
            RemoteModeParam::Share => RemoteMode::Share,
            RemoteModeParam::Publish => RemoteMode::Publish,
        }
    }
}

/// The settings level a remote edit writes.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RemoteLevelParam {
    Project,
    Local,
    User,
}

impl From<RemoteLevelParam> for RemoteLevel {
    fn from(l: RemoteLevelParam) -> RemoteLevel {
        match l {
            RemoteLevelParam::Project => RemoteLevel::Project,
            RemoteLevelParam::Local => RemoteLevel::Local,
            RemoteLevelParam::User => RemoteLevel::User,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteParams {
    /// list, add, rm or set_mode.
    pub op: RemoteOp,
    /// The remote's name: 1 to 64 letters, digits, '.', '_' or '-', a letter or digit first. add, rm and set_mode.
    pub name: Option<String>,
    /// The remote's HTTPS URL. add only.
    pub url: Option<String>,
    /// share (the project's shared line; at most one) or publish. add and set_mode.
    pub mode: Option<RemoteModeParam>,
    /// The config level to change: project (.rdy.yaml, the default), local (.rdy/local.yaml) or user. add, rm and set_mode.
    pub level: Option<RemoteLevelParam>,
}

/// A checked `rdy_remote` or `rdy remote` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteRequest {
    List,
    Add {
        name: String,
        url: String,
        mode: RemoteMode,
        level: RemoteLevel,
    },
    Rm {
        name: String,
        level: RemoteLevel,
    },
    SetMode {
        name: String,
        mode: RemoteMode,
        level: RemoteLevel,
    },
}

/// `E_BAD_INPUT` for a field `op` needs.
fn needs(op: RemoteOp, field: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("remote {} needs {field}", op.name()),
        format!("pass {field}"),
    )
}

/// `E_BAD_INPUT` for a field `op` does not take.
fn not_used(op: RemoteOp, field: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{field} is not used by remote {}", op.name()),
        format!("remove {field}"),
    )
}

/// Whether an op needs, may take or does not take a field.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    Needed,
    Optional,
    Unused,
}

/// Checks which fields `p` gives for its op, in the order name, url, mode, level, and builds
/// the request. list takes nothing; add needs name, url and mode; rm needs name; set_mode needs
/// name and mode; add, rm and set_mode take level (default project). The name and URL
/// themselves are the remote service's to check.
pub fn remote_request(p: RemoteParams) -> Result<RemoteRequest, RdyError> {
    use Rule::{Needed, Optional, Unused};
    let op = p.op;
    let (name, url, mode, level) = match op {
        RemoteOp::List => (Unused, Unused, Unused, Unused),
        RemoteOp::Add => (Needed, Needed, Needed, Optional),
        RemoteOp::Rm => (Needed, Unused, Unused, Optional),
        RemoteOp::SetMode => (Needed, Unused, Needed, Optional),
    };
    let fields = [
        ("name", p.name.is_some(), name),
        ("url", p.url.is_some(), url),
        ("mode", p.mode.is_some(), mode),
        ("level", p.level.is_some(), level),
    ];
    for (field, given, rule) in fields {
        match (rule, given) {
            (Needed, false) => return Err(needs(op, field)),
            (Unused, true) => return Err(not_used(op, field)),
            _ => {}
        }
    }
    let name = p.name.unwrap_or_default();
    let level = p.level.map(RemoteLevel::from).unwrap_or_default();
    // add and set_mode need a mode, checked above; the others never read it
    let mode = p.mode.map(RemoteMode::from).unwrap_or(RemoteMode::Share);
    Ok(match op {
        RemoteOp::List => RemoteRequest::List,
        RemoteOp::Add => RemoteRequest::Add {
            name,
            url: p.url.unwrap_or_default(),
            mode,
            level,
        },
        RemoteOp::Rm => RemoteRequest::Rm { name, level },
        RemoteOp::SetMode => RemoteRequest::SetMode { name, mode, level },
    })
}

fn to_reply<T: serde::Serialize>(what: &str, v: &T) -> Result<Value, RdyError> {
    serde_json::to_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a {what} reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// Runs a checked remote request on the project at `project` with the user config
/// `user_file` (`E_CONFIG` when there is none). list is `{remotes, warnings}`; an edit is
/// `{remote: {name, url, mode, level, install}, removed, warnings}`.
pub fn run_remote(
    project: &Path,
    user_file: Option<&Path>,
    req: RemoteRequest,
) -> Result<Value, RdyError> {
    let user_file = user_file.ok_or_else(user_config_unavailable)?;
    let service = RemoteService::new(project, user_file);
    match req {
        RemoteRequest::List => to_reply("remote", &service.list()?),
        RemoteRequest::Add {
            name,
            url,
            mode,
            level,
        } => to_reply("remote", &service.add(&name, &url, mode, level)?),
        RemoteRequest::Rm { name, level } => to_reply("remote", &service.remove(&name, level)?),
        RemoteRequest::SetMode { name, mode, level } => {
            to_reply("remote", &service.set_mode(&name, mode, level)?)
        }
    }
}

/// `rdy_remote`: the remotes in effect, or one edit at one config level. Any session may call
/// it; nothing is sent to a hub.
pub fn rdy_remote(st: &mut McpState, p: RemoteParams) -> Result<Value, RdyError> {
    session(st)?;
    let req = remote_request(p)?;
    let found = user_file_from_env();
    run_remote(&st.engine.root(), found.as_deref(), req)
}

// ---------------------------------------------------------------------------------------------
// Key commands
// ---------------------------------------------------------------------------------------------

/// Whose configured key a bind proves.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyKindParam {
    Person,
    Agent,
}

impl From<KeyKindParam> for KeyKind {
    fn from(k: KeyKindParam) -> KeyKind {
        match k {
            KeyKindParam::Person => KeyKind::Person,
            KeyKindParam::Agent => KeyKind::Agent,
        }
    }
}

/// The HTTPS transport of one key command, fenced to the install of its first request (the
/// selected remote's, which the key service checks against the same fence first) and built
/// then; it is dropped with the call.
struct KeyTransport<'a> {
    project: &'a Path,
    user_file: &'a Path,
    built: OnceLock<Result<HttpHubTransport, RdyError>>,
}

impl HubTransport for KeyTransport<'_> {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        let built = self.built.get_or_init(|| {
            let settings = HubConfig::load(Some(self.project), Some(self.user_file))?;
            HttpHubTransport::new(settings.fence(&call.install)?)
        });
        match built {
            Ok(t) => t.exchange(call),
            Err(e) => Err(HubFailure::Local(e.clone())),
        }
    }
}

/// Who runs a key command: the project, the user config of this process, the caller's
/// session and the caller's own SSH agent.
pub struct KeyCaller<'a> {
    pub project: &'a Path,
    pub user_file: Option<PathBuf>,
    pub session: &'a Session,
    pub agent: &'a dyn SigningAgent,
}

/// Runs `f` with the key service of `c`; the transport lives for this call alone.
fn with_keys<R>(
    c: &KeyCaller<'_>,
    f: impl FnOnce(&KeyService<'_>) -> Result<R, RdyError>,
) -> Result<R, RdyError> {
    let user_file = require_user_file(c.user_file.clone())?;
    let transport = KeyTransport {
        project: c.project,
        user_file: &user_file,
        built: OnceLock::new(),
    };
    let clock = SystemHubClock;
    let service = KeyService::new(c.project, &user_file, &transport, &clock, c.agent);
    f(&service)
}

/// Binds the configured key of `kind`: `{key, kind, state, confirm_url, remote, warnings}`.
/// The state is pending until a person confirms the key on the hub at `confirm_url`.
pub fn run_key_bind(
    c: &KeyCaller<'_>,
    kind: KeyKindParam,
    label: Option<&str>,
    remote: Option<&str>,
) -> Result<Value, RdyError> {
    let reply = with_keys(c, |k| k.bind(kind.into(), label, remote, c.session))?;
    to_reply("key bind", &reply)
}

/// Retires a key on the hub: `{key, state, remote, warnings}`.
pub fn run_key_retire(
    c: &KeyCaller<'_>,
    key: &str,
    remote: Option<&str>,
) -> Result<Value, RdyError> {
    let reply = with_keys(c, |k| k.retire(key, remote, c.session))?;
    to_reply("key retire", &reply)
}

/// Revokes a key on the hub: `{key, state, revoked_at, compromised_from, remote, warnings}`.
/// With no `compromised_since`, none is sent and the hub chooses its request time.
pub fn run_key_revoke(
    c: &KeyCaller<'_>,
    key: &str,
    compromised_since: Option<&str>,
    remote: Option<&str>,
) -> Result<Value, RdyError> {
    let reply = with_keys(c, |k| k.revoke(key, compromised_since, remote, c.session))?;
    to_reply("key revoke", &reply)
}
