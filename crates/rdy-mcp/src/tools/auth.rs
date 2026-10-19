//! `rdy_auth` (D-KQ4CA, n-XZDQD, n-N055T): hub sign-in from chat, with `rdy auth` as its
//! command-line fallback. Both call the engine's credential service directly, which renders and
//! hashes the sign-in card, checks the relay of the person's answer and is the only renewer of
//! access. No daemon op is involved.
//!
//! - **One validation.** [`auth_request`] checks which fields an op takes, in a fixed order, for
//!   both surfaces; [`run_auth`] runs the request against the service.
//! - **The caller stays the caller.** The service checks the relay with the actual session and
//!   the configured person; the session it would act as is never used here, and the MCP session
//!   is never replaced.
//! - **One transport per call.** The HTTPS transport is built on the call's first request,
//!   fenced to its install, and dropped when the call returns, inside the tool's blocking run.
//! - **Nothing secret leaves.** Replies carry no token, device code or complete verification
//!   link. Access leaves only through the command line's machine-token reply.
//! - **Warnings are not lost.** The config's warnings are read first; a failure after that
//!   carries them as its issues ([`with_warnings`]), and a state reply as its warnings
//!   ([`AuthReply::with_warnings`]).
//! - **Sign-out is local first.** [`sign_out`] needs no settings to remove the saved sign-in:
//!   they are read only for the hub sign-out that follows, so settings that do not load leave
//!   `signed_out` with `W_UPSTREAM_SIGNOUT_LOCAL`, never the credential in place.

use super::*;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::config_levels::{config_warnings, user_file_from_env};
use rdy_engine::hub_config::HubConfig;
use rdy_engine::hub_credentials::{
    AuthAnswer, AuthCaller, AuthConfirmation, AuthState, HubCredentials, InstallState,
    require_user_file,
};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_types::{
    HubCall, HubFailure, HubReply, HubTransport, Origin, OriginFence, RequestClaim, SystemHubClock,
};
use rdy_engine::*;
use rdy_schema::Issue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::OnceLock;

/// What `rdy_auth` does.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthOp {
    Start,
    State,
    SignOut,
    Installs,
}

impl AuthOp {
    /// The op's name in errors.
    pub fn name(self) -> &'static str {
        match self {
            AuthOp::Start => "start",
            AuthOp::State => "state",
            AuthOp::SignOut => "sign_out",
            AuthOp::Installs => "installs",
        }
    }
}

/// The person's answer to the sign-in card.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthAnswerParam {
    Confirm,
    Cancel,
}

impl From<AuthAnswerParam> for AuthAnswer {
    fn from(a: AuthAnswerParam) -> AuthAnswer {
        match a {
            AuthAnswerParam::Confirm => AuthAnswer::Confirm,
            AuthAnswerParam::Cancel => AuthAnswer::Cancel,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthParams {
    /// start, state, sign_out or installs.
    pub op: AuthOp,
    /// The install's HTTPS origin. start, state and sign_out only.
    pub install: Option<String>,
    /// This machine's name on the hub: 1 to 64 characters. start only.
    pub machine_name: Option<String>,
    /// The card_hash of the card the person answered. start only, with answer.
    pub card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. start only, with card_hash.
    pub answer: Option<AuthAnswerParam>,
    /// The person's words, quoted (1 to 2000 characters). start only.
    pub evidence: Option<String>,
    /// The question you asked the person. start only, with evidence.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp). start only, with evidence.
    pub session_ref: Option<String>,
}

/// A checked `rdy_auth` or `rdy auth` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthRequest {
    /// The card when `confirmation` is none; else the person's answer to it.
    Start {
        install: Origin,
        machine_name: String,
        confirmation: Option<AuthConfirmation>,
    },
    State {
        install: Origin,
    },
    SignOut {
        install: Origin,
    },
    Installs,
}

/// `E_BAD_INPUT` for a field `op` needs.
fn needs(op: AuthOp, field: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("auth {} needs {field}", op.name()),
        format!("pass {field}"),
    )
}

/// `E_BAD_INPUT` for a field `op` does not take.
fn not_used(op: AuthOp, field: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{field} is not used by auth {}", op.name()),
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

/// Checks which fields `p` gives for its op, in the order install, machine_name, card_hash,
/// answer, evidence, question, session_ref, and builds the request. `relayed_by` is the command
/// line's relay flag (`None` over MCP, where the session is the relay); it counts as a relay
/// field of start. start takes install and machine_name; with any of card_hash, answer or a
/// relay field it is the answer to the card, which needs card_hash and answer, and question or
/// session_ref needs evidence. state and sign_out take install only; installs takes nothing.
/// Who may relay is the service's relay check, not this one.
pub fn auth_request(p: AuthParams, relayed_by: Option<String>) -> Result<AuthRequest, RdyError> {
    let op = p.op;
    let answered = p.card_hash.is_some()
        || p.answer.is_some()
        || p.evidence.is_some()
        || p.question.is_some()
        || p.session_ref.is_some()
        || relayed_by.is_some();
    let start = |needed: bool| match (op, needed) {
        (AuthOp::Start, true) => Rule::Needed,
        (AuthOp::Start, false) => Rule::Optional,
        _ => Rule::Unused,
    };
    let install_rule = match op {
        AuthOp::Installs => Rule::Unused,
        _ => Rule::Needed,
    };
    let quoted = p.question.is_some() || p.session_ref.is_some();
    let fields = [
        ("install", p.install.is_some(), install_rule),
        ("machine_name", p.machine_name.is_some(), start(true)),
        ("card_hash", p.card_hash.is_some(), start(answered)),
        ("answer", p.answer.is_some(), start(answered)),
        ("evidence", p.evidence.is_some(), start(quoted)),
        ("question", p.question.is_some(), start(false)),
        ("session_ref", p.session_ref.is_some(), start(false)),
    ];
    for (field, given, rule) in fields {
        match (rule, given) {
            (Rule::Needed, false) => return Err(needs(op, field)),
            (Rule::Unused, true) => return Err(not_used(op, field)),
            _ => {}
        }
    }
    let install = || Origin::parse(p.install.as_deref().unwrap_or_default());
    Ok(match op {
        AuthOp::Start => {
            let confirmation = match (p.card_hash, p.answer) {
                (Some(card_hash), Some(answer)) => Some(AuthConfirmation {
                    card_hash,
                    answer: answer.into(),
                    relay: RelayInput {
                        relayed_by,
                        evidence: p.evidence,
                        question: p.question,
                        session_ref: p.session_ref,
                        kind: None,
                    },
                }),
                _ => None,
            };
            AuthRequest::Start {
                install: install()?,
                machine_name: p.machine_name.unwrap_or_default(),
                confirmation,
            }
        }
        AuthOp::State => AuthRequest::State {
            install: install()?,
        },
        AuthOp::SignOut => AuthRequest::SignOut {
            install: install()?,
        },
        AuthOp::Installs => AuthRequest::Installs,
    })
}

/// Who calls and with which settings: the actual session, the configured person the relay
/// table names, whether the call came over MCP, the project (none at the command line) and the
/// user config with the hub settings read from it.
pub struct AuthSurface<'a> {
    pub session: &'a Session,
    pub person: &'a str,
    pub implicit: bool,
    pub project: Option<&'a Path>,
    pub user_file: &'a Path,
    pub settings: &'a HubConfig,
}

/// The user config of the environment, or `E_CONFIG` when no location is known.
pub fn auth_user_file() -> Result<std::path::PathBuf, RdyError> {
    require_user_file(user_file_from_env())
}

/// `e` with the config's `warnings` as its issues: those first, then each issue `e` had that is
/// not one of them (a hub settings refusal already carries its own share).
pub fn with_warnings(mut e: RdyError, warnings: &[Issue]) -> RdyError {
    let own = std::mem::take(&mut e.issues);
    e.issues = warnings.to_vec();
    e.issues
        .extend(own.into_iter().filter(|i| !warnings.contains(i)));
    e
}

/// The bounded configured human actor: `actor` in `.rdy/local.yaml`, else in the user config,
/// as `settings` read them, else `human:$USER`, else `human:unknown`. With no settings (they do
/// not load, and the caller only signs out), the configured actor is not known: `human:$USER`.
pub fn auth_person(settings: Option<&HubConfig>) -> String {
    let user = std::env::var("USER").ok();
    match settings {
        Some(s) => s.human_actor(user.as_deref()),
        None => format!("human:{}", user.as_deref().unwrap_or("unknown")),
    }
}

/// Where a call's origin fence comes from.
enum FenceOf<'a> {
    /// No install (`installs`): every request is refused.
    None,
    /// Already read from the call's settings.
    Ready(OriginFence),
    /// Read from the settings on the first request (sign-out): settings that do not load fail
    /// that request, never the removal before it.
    Settings {
        project: Option<&'a Path>,
        user_file: &'a Path,
        install: Origin,
    },
}

/// The HTTPS transport of one call, fenced to its install and built on its first request; it
/// is dropped with the call.
struct CallTransport<'a> {
    fence: FenceOf<'a>,
    built: OnceLock<Result<HttpHubTransport, RdyError>>,
}

impl CallTransport<'_> {
    fn build(&self) -> Result<HttpHubTransport, RdyError> {
        let fence = match &self.fence {
            FenceOf::None => {
                return Err(RdyError::new(
                    E_INTERNAL,
                    "a hub request was made without an install",
                    "report this as a bug in rdy",
                ));
            }
            FenceOf::Ready(f) => f.clone(),
            FenceOf::Settings {
                project,
                user_file,
                install,
            } => HubConfig::load(*project, Some(*user_file))?.fence(install)?,
        };
        HttpHubTransport::new(fence)
    }
}

impl HubTransport for CallTransport<'_> {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        match self.built.get_or_init(|| self.build()) {
            Ok(t) => t.exchange(call),
            Err(e) => Err(HubFailure::Local(e.clone())),
        }
    }
}

/// Runs `f` with the credential service of `project` and `user_file`, whose requests go
/// through `fence` only. The transport lives for this call alone.
fn with_service<R>(
    project: Option<&Path>,
    user_file: &Path,
    fence: FenceOf<'_>,
    f: impl FnOnce(&HubCredentials<'_>) -> Result<R, RdyError>,
) -> Result<R, RdyError> {
    let transport = CallTransport {
        fence,
        built: OnceLock::new(),
    };
    let clock = SystemHubClock;
    let service = HubCredentials::new(project, user_file, &transport, &clock);
    f(&service)
}

/// Runs `f` with the credential service of `surface`, whose requests go to `install` only.
/// The transport lives for this call alone.
pub fn with_credentials<R>(
    surface: &AuthSurface<'_>,
    install: Option<&Origin>,
    f: impl FnOnce(&HubCredentials<'_>) -> Result<R, RdyError>,
) -> Result<R, RdyError> {
    let fence = match install {
        Some(i) => FenceOf::Ready(surface.settings.fence(i)?),
        None => FenceOf::None,
    };
    with_service(surface.project, surface.user_file, fence, f)
}

/// Signs `session` out of `install` with the credential file of `user_file`: the caller's claim
/// is checked, then the service removes the saved sign-in under its lock. No settings are read
/// first: the hub sign-out after the removal reads them (the CA file and the origin fence of
/// `project` and `user_file`), so settings that do not load give `signed_out` with
/// `W_UPSTREAM_SIGNOUT_LOCAL`.
pub fn sign_out(
    session: &Session,
    project: Option<&Path>,
    user_file: &Path,
    install: &Origin,
) -> Result<AuthState, RdyError> {
    let claim = RequestClaim::from_session(session)?;
    let fence = FenceOf::Settings {
        project,
        user_file,
        install: install.clone(),
    };
    with_service(project, user_file, fence, |c| c.sign_out(install, &claim))
}

/// A reply of `rdy_auth` and `rdy auth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthReply {
    State(Box<AuthState>),
    Installs(Vec<InstallState>),
}

impl AuthReply {
    /// The reply of one state.
    pub fn state(s: AuthState) -> AuthReply {
        AuthReply::State(Box::new(s))
    }

    /// The reply with the config's `warnings`, as they were read for this call: a state carries
    /// them first, then each of its own that is not one of them (the order [`with_warnings`]
    /// gives a failure's issues). The installs list keeps its `warnings: []`.
    pub fn with_warnings(self, warnings: &[Issue]) -> AuthReply {
        match self {
            AuthReply::State(mut s) => {
                let own = std::mem::take(&mut s.warnings);
                s.warnings = warnings.to_vec();
                s.warnings
                    .extend(own.into_iter().filter(|i| !warnings.contains(i)));
                AuthReply::State(s)
            }
            other => other,
        }
    }

    /// Whether the reply carries the config's warnings ([`AuthReply::with_warnings`]): a state
    /// does, the installs list does not.
    pub fn carries_warnings(&self) -> bool {
        matches!(self, AuthReply::State(_))
    }

    /// The reply's JSON: the state, or `{installs, warnings: []}`.
    pub fn to_json(&self) -> Result<Value, RdyError> {
        let encode = |e: serde_json::Error| {
            RdyError::new(
                E_INTERNAL,
                format!("a sign-in reply did not serialize: {e}"),
                "report this as a bug in rdy",
            )
        };
        match self {
            AuthReply::State(s) => serde_json::to_value(s).map_err(encode),
            AuthReply::Installs(rows) => {
                let rows = serde_json::to_value(rows).map_err(encode)?;
                Ok(json!({"installs": rows, "warnings": []}))
            }
        }
    }
}

/// Runs a checked request as `surface`'s caller. state and sign_out claim the caller's own
/// session; start passes the caller and the person to the service's card and relay checks.
pub fn run_auth(surface: &AuthSurface<'_>, req: AuthRequest) -> Result<AuthReply, RdyError> {
    match req {
        AuthRequest::Start {
            install,
            machine_name,
            confirmation,
        } => {
            let caller = AuthCaller {
                session: surface.session,
                person: surface.person,
                implicit: surface.implicit,
            };
            with_credentials(surface, Some(&install), |c| {
                c.start(&install, &machine_name, confirmation.as_ref(), caller)
            })
            .map(AuthReply::state)
        }
        AuthRequest::State { install } => {
            let claim = RequestClaim::from_session(surface.session)?;
            with_credentials(surface, Some(&install), |c| c.state(&install, &claim))
                .map(AuthReply::state)
        }
        AuthRequest::SignOut { install } => sign_out(
            surface.session,
            surface.project,
            surface.user_file,
            &install,
        )
        .map(AuthReply::state),
        AuthRequest::Installs => {
            with_credentials(surface, None, |c| c.installs()).map(AuthReply::Installs)
        }
    }
}

/// `rdy_auth`: the session's sign-in act. Any session may call it; the service's relay check
/// decides who may answer the card.
///
/// The config's warnings, as the project and user config are now, are read first: a failure
/// after that carries them as its issues and a state reply as its warnings, so a mistake made
/// during the session is not lost. Sign-out reads no settings before the removal ([`sign_out`]).
pub fn rdy_auth(st: &mut McpState, p: AuthParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let root = st.engine.root();
    let found = user_file_from_env();
    let warnings = config_warnings(&root, found.as_deref());
    let run = || -> Result<Value, RdyError> {
        let req = auth_request(p, None)?;
        let user_file = require_user_file(found)?;
        let reply = if let AuthRequest::SignOut { install } = &req {
            AuthReply::state(sign_out(&s, Some(&root), &user_file, install)?)
        } else {
            let settings = HubConfig::load(Some(&root), Some(&user_file))?;
            let person = auth_person(Some(&settings));
            let surface = AuthSurface {
                session: &s,
                person: &person,
                implicit: true,
                project: Some(&root),
                user_file: &user_file,
                settings: &settings,
            };
            run_auth(&surface, req)?
        };
        reply.with_warnings(&warnings).to_json()
    };
    run().map_err(|e| with_warnings(e, &warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::hub_credentials::AuthPhase;

    fn config_warning(key: &str) -> Issue {
        Issue {
            severity: rdy_schema::Severity::Warning,
            code: "W_CONFIG_UNKNOWN",
            block: None,
            message: format!("unknown key {key}"),
            fix: format!("remove {key}"),
        }
    }

    /// A state reply carries the config's warnings first, then each of its own that is not one
    /// of them; the installs list keeps `warnings: []`.
    #[test]
    fn hsa8_reply_warnings_merge_config_first_without_repeats() {
        let install = Origin::parse("https://hub.test").unwrap();
        let (a, b) = (config_warning("a"), config_warning("b"));
        let own = upstream_signout_local();
        let state = AuthState {
            install,
            state: AuthPhase::SignedOut,
            card: None,
            card_hash: None,
            user_code: None,
            verification_uri: None,
            expires_at_ms: None,
            next_poll_at_ms: None,
            account: None,
            human_actor: None,
            warnings: vec![own.clone(), b.clone()],
        };
        let reply = AuthReply::state(state).with_warnings(&[a.clone(), b.clone()]);
        assert!(reply.carries_warnings());
        let AuthReply::State(s) = &reply else {
            unreachable!()
        };
        assert_eq!(s.warnings, [a.clone(), b.clone(), own]);
        let rows = AuthReply::Installs(Vec::new()).with_warnings(&[a, b]);
        assert!(!rows.carries_warnings());
        assert_eq!(
            rows.to_json().unwrap(),
            json!({"installs": [], "warnings": []})
        );
    }
}
