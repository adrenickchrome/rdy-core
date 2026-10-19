//! `rdy sign` and `rdy key show` (D-1Z951, D-QVH4N): the CLI fallback of `rdy_sign` and
//! `rdy_key_show`, through the same daemon ops. Neither contacts a hub.
//!
//! `rdy key bind | retire | revoke` (D-XXA9D): the CLI fallback of `rdy_key_bind`,
//! `rdy_key_retire` and `rdy_key_revoke`, through the same daemon ops. A bind is signed by this
//! process's SSH agent, through the daemon too. The hub credentials are the daemon process's
//! (with `--no-daemon`, this process's): after changing `XDG_CONFIG_HOME`, restart the daemon
//! (`rdy serve --stop`).
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

/// Whose configured key a bind proves.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum KeyKindArg {
    Person,
    Agent,
}

impl KeyKindArg {
    fn word(self) -> &'static str {
        match self {
            KeyKindArg::Person => "person",
            KeyKindArg::Agent => "agent",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum KeyCmd {
    /// Show the configured person and machine agent keys, their types and whether your SSH agent holds them.
    Show,
    /// Prove to the selected remote's hub that your SSH agent holds the configured person or agent key. Prints the hub's link: the person opens it to confirm the key there; until then it is pending.
    Bind {
        /// person (signing.key) or agent (signing.agent_key).
        #[arg(long, value_enum)]
        kind: KeyKindArg,
        /// The key's label on the hub: 1 to 64 characters (default: "person key" or "agent key").
        #[arg(long)]
        label: Option<String>,
        /// The remote whose hub binds the key (default: the share remote, else the only remote).
        #[arg(long)]
        remote: Option<String>,
    },
    /// Ask the selected remote's hub to retire a key: a public key line or its SHA256 fingerprint. Local signatures and signing settings stay unchanged.
    Retire {
        /// A public key line or its SHA256 fingerprint.
        key: String,
        /// The remote whose hub holds the key (default: the share remote, else the only remote).
        #[arg(long)]
        remote: Option<String>,
    },
    /// Ask the selected remote's hub to revoke a key: a public key line or its SHA256 fingerprint.
    Revoke {
        /// A public key line or its SHA256 fingerprint.
        key: String,
        /// When the key may first have been compromised, as an RFC3339 time (default: the hub's request time).
        #[arg(long)]
        compromised_since: Option<String>,
        /// The remote whose hub holds the key (default: the share remote, else the only remote).
        #[arg(long)]
        remote: Option<String>,
    },
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
/// shared warning form. `rdy key bind | retire | revoke`: the hub's key state, then the
/// reply's warnings in the same form.
pub(crate) fn key(env: &Env, cmd: KeyCmd) -> Result<(Value, String), ErrorReply> {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let (v, mut text) = match cmd {
        KeyCmd::Show => {
            let v = call(env, "key_show", json!({}))?;
            let mut text = rdy_engine::signing_read::key_show_text(&v);
            for w in rdy_engine::config_levels::config_warnings_for_env(&env.project) {
                text.push('\n');
                text.push_str(&crate::config_warning_line(&w));
            }
            return Ok((v, text));
        }
        KeyCmd::Bind {
            kind,
            label,
            remote,
        } => {
            let v = call(
                env,
                "key_bind",
                json!({"kind": kind.word(), "label": label, "remote": remote}),
            )?;
            let text = format!(
                "key {}: {}\nconfirm: {}",
                s(&v, "key"),
                s(&v, "state"),
                s(&v, "confirm_url")
            );
            (v, text)
        }
        KeyCmd::Retire { key, remote } => {
            let v = call(env, "key_retire", json!({"key": key, "remote": remote}))?;
            let text = format!("key {}: {}", s(&v, "key"), s(&v, "state"));
            (v, text)
        }
        KeyCmd::Revoke {
            key,
            compromised_since,
            remote,
        } => {
            let v = call(
                env,
                "key_revoke",
                json!({"key": key, "compromised_since": compromised_since, "remote": remote}),
            )?;
            let text = format!(
                "key {}: {}\nrevoked at: {}\ncompromised from: {}",
                s(&v, "key"),
                s(&v, "state"),
                s(&v, "revoked_at"),
                s(&v, "compromised_from")
            );
            (v, text)
        }
    };
    crate::remote_cmd::push_warnings(&mut text, &v);
    Ok((v, text))
}
