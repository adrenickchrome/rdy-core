//! `rdy sign` and `rdy key show` (D-1Z951, D-QVH4N): the CLI fallback of `rdy_sign` and
//! `rdy_key_show`, through the same daemon ops. Neither contacts a hub.
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

/// The statement kind a later signature makes.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum SignKindArg {
    Approval,
    Relay,
    Tag,
}

impl SignKindArg {
    fn word(self) -> &'static str {
        match self {
            SignKindArg::Approval => "approval",
            SignKindArg::Relay => "relay",
            SignKindArg::Tag => "tag",
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct SignArgs {
    /// The landing or tag to sign: a landing id (a hash or a unique prefix of 6 or more), or tag:<version>.
    target: String,
    /// approval, relay or tag. Default: your own kind for a person (approval or tag), relay for an agent.
    #[arg(long, value_enum)]
    kind: Option<SignKindArg>,
}

#[derive(Subcommand)]
pub(crate) enum KeyCmd {
    /// Show the configured person and machine agent keys, their types and whether your SSH agent holds them.
    Show,
}

/// `rdy sign`: prints `signed <kind> of <subject> with <fingerprint>`, or `already signed`; its
/// JSON is the requested signature's check.
pub(crate) fn sign(env: &Env, a: SignArgs) -> Result<(Value, String), ErrorReply> {
    let SignArgs { target, kind } = a;
    let v = call(
        env,
        "sign",
        json!({"target": target, "kind": kind.map(SignKindArg::word)}),
    )?;
    let check = v["check"].clone();
    let text = if v["already_signed"] == Value::Bool(true) {
        "already signed".to_string()
    } else {
        format!(
            "signed {} of {} with {}",
            check["kind"].as_str().unwrap_or("unknown"),
            v["subject"].as_str().unwrap_or("unknown"),
            check["key"].as_str().unwrap_or("unknown")
        )
    };
    Ok((check, text))
}

/// `rdy key show`: three lines (person, agent, mode), then the configuration warnings in the
/// shared warning form.
pub(crate) fn key(env: &Env, cmd: KeyCmd) -> Result<(Value, String), ErrorReply> {
    let KeyCmd::Show = cmd;
    let v = call(env, "key_show", json!({}))?;
    let mut text = rdy_engine::signing_read::key_show_text(&v);
    for w in rdy_engine::config_levels::config_warnings_for_env(&env.project) {
        text.push('\n');
        text.push_str(&crate::config_warning_line(&w));
    }
    Ok((v, text))
}
