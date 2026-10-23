//! `rdy auth` (D-KQ4CA, n-XZDQD, n-N055T): the command-line fallback of `rdy_auth`, plus the
//! machine-token door for a credential consumer. It runs before any project lookup, store or
//! daemon: it reads the user config only, and the engine's credential service does the rest
//! through the same request checks and replies as `rdy_auth`.
//!
//! - **Person or relay.** With no `RDY_ACTOR` the person signs in as themselves. An agent answers
//!   the card only by relaying the person's words (`--relayed-by` with `--evidence`); the
//!   service's relay check decides, and nothing here skips it.
//! - **The token door.** `rdy --json auth token` gives a consumer one line with current access,
//!   only to a pipe: never to a terminal, never as text, and never the refresh token.
//! - **The user config's warnings.** With no project, the shared config-warning print does not
//!   run, so this command reads the user config's warnings itself. A state reply carries them in
//!   its warnings, as `rdy_auth` does; they follow the installs list and the token door's line on
//!   stderr; a failure after they are read carries them as its issues.
//! - **Sign-out is local first.** Settings that do not load do not stop the removal of the saved
//!   sign-in: only the hub sign-out after it needs them.
use crate::config_warning_line;
use crate::output::print_error;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::config_levels::user_config_warnings;
use rdy_engine::hub_config::HubConfig;
use rdy_engine::hub_credentials::{AccessGrant, AuthPhase, AuthState};
use rdy_engine::hub_types::{Origin, RequestClaim};
use rdy_engine::{E_BAD_INPUT, RdyError, Session, classify};
use rdy_mcp::tools::{
    AuthAnswerParam, AuthOp, AuthParams, AuthReply, AuthRequest, AuthSurface, auth_person,
    auth_request, auth_user_file, run_auth, sign_out, with_credentials, with_warnings,
};
use rdy_model::Role;
use rdy_schema::Issue;
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

/// The person's answer to the sign-in card.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum AnswerArg {
    Confirm,
    Cancel,
}

#[derive(clap::Args)]
pub(crate) struct SignInArgs {
    /// The install's HTTPS origin.
    #[arg(long)]
    install: String,
    /// This machine's name on the hub: 1 to 64 characters.
    #[arg(long)]
    machine_name: String,
    /// The card hash the first call printed, to answer that card.
    #[arg(long)]
    card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. Needs --card-hash.
    #[arg(long, value_enum)]
    answer: Option<AnswerArg>,
    /// You, the relaying agent, as actor[/run]; run this under your own RDY_ACTOR.
    #[arg(long)]
    relayed_by: Option<String>,
    /// The person's words, quoted.
    #[arg(long)]
    evidence: Option<String>,
    /// The question the person was asked.
    #[arg(long)]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long)]
    session_ref: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct InstallArg {
    /// The install's HTTPS origin.
    #[arg(long)]
    install: String,
}

#[derive(Subcommand)]
pub(crate) enum AuthCmd {
    /// Start a hub sign-in: the first call prints the confirmation card; answer it with --card-hash and --answer. Then open the printed link and enter the code.
    SignIn(SignInArgs),
    /// Show the sign-in state of an install; a pending sign-in is checked once.
    State(InstallArg),
    /// Remove this machine's sign-in for an install and end it on the hub.
    SignOut(InstallArg),
    /// List the saved sign-ins.
    Installs,
    /// For a credential consumer: print current access for an install as one JSON line. Needs --json and piped output.
    Token(InstallArg),
}

/// Where standard output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Terminal,
    Other,
}

impl OutputKind {
    /// The kind of this process's standard output.
    pub(crate) fn of_stdout() -> OutputKind {
        if std::io::stdout().is_terminal() {
            OutputKind::Terminal
        } else {
            OutputKind::Other
        }
    }
}

/// The token door: `E_BAD_INPUT` unless the reply is JSON and goes to a pipe, not a terminal.
pub(crate) fn token_door(as_json: bool, out: OutputKind) -> Result<(), RdyError> {
    if as_json && out == OutputKind::Other {
        return Ok(());
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        "auth token needs --json and a pipe for its output",
        "run this command from the credential consumer with piped output",
    ))
}

/// The machine-token reply: the only encoder of access. One JSON object with the install, the
/// access token, its expiry and the cached human actor; never the refresh token.
fn token_line(g: &AccessGrant) -> String {
    json!({
        "install": g.install.as_str(),
        "access_token": g.access_token.expose(),
        "expires_at_ms": g.expires_at_ms,
        "human_actor": g.human_actor,
    })
    .to_string()
}

/// The standalone caller: the bounded configured person (for a sign-out whose settings do not
/// load, the user config's actor read on its own), classified with
/// `RDY_ACTOR` and `RDY_RUN` as a writer.
fn caller(
    settings: Option<&HubConfig>,
    user_file: &std::path::Path,
) -> Result<(Session, String), RdyError> {
    let person = auth_person(settings, user_file)?;
    let actor = std::env::var("RDY_ACTOR").ok();
    let run = std::env::var("RDY_RUN").ok();
    let session = classify(&person, actor.as_deref(), run.as_deref(), Role::Writer)?;
    Ok((session, person))
}

/// The checked request of a person command.
fn request(cmd: AuthCmd) -> Result<AuthRequest, RdyError> {
    let bare = |op, install| AuthParams {
        op,
        install,
        machine_name: None,
        card_hash: None,
        answer: None,
        evidence: None,
        question: None,
        session_ref: None,
    };
    match cmd {
        AuthCmd::SignIn(a) => auth_request(
            AuthParams {
                machine_name: Some(a.machine_name),
                card_hash: a.card_hash,
                answer: a.answer.map(|x| match x {
                    AnswerArg::Confirm => AuthAnswerParam::Confirm,
                    AnswerArg::Cancel => AuthAnswerParam::Cancel,
                }),
                evidence: a.evidence,
                question: a.question,
                session_ref: a.session_ref,
                ..bare(AuthOp::Start, Some(a.install))
            },
            a.relayed_by,
        ),
        AuthCmd::State(a) => auth_request(bare(AuthOp::State, Some(a.install)), None),
        AuthCmd::SignOut(a) => auth_request(bare(AuthOp::SignOut, Some(a.install)), None),
        AuthCmd::Installs => auth_request(bare(AuthOp::Installs, None), None),
        AuthCmd::Token(_) => unreachable!("the token door has its own path"),
    }
}

/// The text of one state.
fn state_text(s: &AuthState) -> String {
    let install = &s.install;
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    match s.state {
        AuthPhase::ConfirmationRequired => format!(
            "{}card hash: {}\nanswer with --answer and --card-hash",
            text(&s.card),
            text(&s.card_hash)
        ),
        AuthPhase::Cancelled => format!("sign-in cancelled for {install}"),
        AuthPhase::Pending => format!(
            "sign-in pending for {install}\ncode: {}\nopen: {}\ncheck again with rdy auth state --install {install}",
            text(&s.user_code),
            text(&s.verification_uri)
        ),
        AuthPhase::SignedIn => format!("signed in to {install} as {}", text(&s.human_actor)),
        AuthPhase::SignedOut => format!("signed out of {install}"),
        AuthPhase::ReauthRequired => format!("sign-in is required for {install}"),
    }
}

/// The text of a reply: its lines, then each warning in the shared warning form.
fn reply_text(r: &AuthReply) -> Result<String, RdyError> {
    let (mut out, warnings): (String, &[Issue]) = match r {
        AuthReply::State(s) => (state_text(s), &s.warnings),
        AuthReply::Installs(rows) if rows.is_empty() => ("no saved hub sign-ins".into(), &[]),
        AuthReply::Installs(rows) => {
            let lines: Vec<String> = rows
                .iter()
                .map(|row| {
                    let state = serde_json::to_value(row.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    let actor = row.human_actor.as_deref().unwrap_or("-");
                    format!("{} {state} {actor}", row.install)
                })
                .collect();
            (lines.join("\n"), &[])
        }
    };
    for w in warnings {
        out.push('\n');
        out.push_str(&config_warning_line(w));
    }
    Ok(out)
}

/// One person command: its JSON (the `rdy_auth` reply, with the config's `warnings`) and its
/// text. `settings` is `None` only for sign-out, whose removal needs none.
fn person(
    cmd: AuthCmd,
    settings: Option<&HubConfig>,
    user_file: &std::path::Path,
    warnings: &[Issue],
) -> CmdResult {
    let req = request(cmd)?;
    let (session, person) = caller(settings, user_file)?;
    let reply = match (req, settings) {
        (AuthRequest::SignOut { install }, _) => {
            AuthReply::state(sign_out(&session, None, user_file, &install)?)
        }
        (req, Some(settings)) => {
            let surface = AuthSurface {
                session: &session,
                person: &person,
                implicit: false,
                project: None,
                user_file,
                settings,
            };
            run_auth(&surface, req)?
        }
        (_, None) => {
            return Err(RdyError::new(
                rdy_engine::E_INTERNAL,
                "a sign-in command ran without its settings",
                "report this as a bug in rdy",
            ));
        }
    };
    let reply = reply.with_warnings(warnings);
    Ok((
        reply.to_json()?,
        reply_text(&reply)?,
        reply.carries_warnings(),
    ))
}

/// The machine-token door: current access for `install`, renewed by this process under the
/// credential lock when it is about to expire.
fn token(
    install: &str,
    settings: &HubConfig,
    user_file: &std::path::Path,
) -> Result<String, RdyError> {
    let install = Origin::parse(install)?;
    let (session, person) = caller(Some(settings), user_file)?;
    let claim = RequestClaim::from_session(&session)?;
    let surface = AuthSurface {
        session: &session,
        person: &person,
        implicit: false,
        project: None,
        user_file,
        settings,
    };
    let grant = with_credentials(&surface, Some(&install), |c| c.access(&install, &claim))?;
    Ok(token_line(&grant))
}

/// A person command's JSON, its text, and whether the reply carries the config's warnings.
type CmdResult = Result<(Value, String, bool), RdyError>;

fn fail(e: RdyError, as_json: bool) -> ExitCode {
    print_error(&ErrorReply::from(e), as_json);
    ExitCode::FAILURE
}

/// Each warning on stderr, in the shared form.
fn print_warnings(warnings: &[Issue]) {
    for w in warnings {
        eprintln!("{}", config_warning_line(w));
    }
}

/// `rdy auth <cmd>`: no project, store or daemon. The user config's warnings are read once,
/// right after its location: a state reply carries them in its warnings (its JSON, or after its
/// text); the installs list and the token door's one line are followed by them on stderr; a
/// failure after the read carries them as its issues. Settings that do not load refuse every
/// command but sign-out, which removes the saved sign-in without them.
pub(crate) fn run(cmd: AuthCmd, as_json: bool) -> ExitCode {
    if let AuthCmd::Token(_) = &cmd
        && let Err(e) = token_door(as_json, OutputKind::of_stdout())
    {
        return fail(e, as_json);
    }
    let user_file = match auth_user_file() {
        Ok(f) => f,
        Err(e) => return fail(e, as_json),
    };
    let warnings = user_config_warnings(&user_file);
    let refuse = |e: RdyError| fail(with_warnings(e, &warnings), as_json);
    let settings = match HubConfig::load(None, Some(&user_file)) {
        Ok(s) => Some(s),
        Err(_) if matches!(cmd, AuthCmd::SignOut(_)) => None,
        Err(e) => return refuse(e),
    };
    if let (AuthCmd::Token(a), Some(settings)) = (&cmd, &settings) {
        return match token(&a.install, settings, &user_file) {
            Ok(line) => {
                let mut out = std::io::stdout().lock();
                if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                    return ExitCode::FAILURE;
                }
                print_warnings(&warnings);
                ExitCode::SUCCESS
            }
            Err(e) => refuse(e),
        };
    }
    match person(cmd, settings.as_ref(), &user_file, &warnings) {
        Ok((v, text, carried)) => {
            if as_json {
                println!("{v}");
            } else {
                println!("{text}");
            }
            if !carried {
                print_warnings(&warnings);
            }
            ExitCode::SUCCESS
        }
        Err(e) => refuse(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token door with the output kind injected: only JSON to a pipe passes, with the
    /// shared texts; a terminal is refused even with --json.
    #[test]
    fn hsa8_token_output_requires_json_and_a_pipe() {
        assert!(token_door(true, OutputKind::Other).is_ok());
        for (as_json, out) in [
            (true, OutputKind::Terminal),
            (false, OutputKind::Terminal),
            (false, OutputKind::Other),
        ] {
            let e = token_door(as_json, out).expect_err("refused");
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (
                    E_BAD_INPUT,
                    "auth token needs --json and a pipe for its output",
                    "run this command from the credential consumer with piped output"
                ),
                "{as_json} {out:?}"
            );
        }
    }
}
