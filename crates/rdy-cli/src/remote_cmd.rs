//! `rdy remote` (D-B8N21, p-38V30, D-KQ4CA): the command-line fallback of `rdy_remote`, through
//! the same daemon op. Remote edits are offline: they read and write settings files only.
//!
//! Through the daemon, the user config (a user-level remote, and the hub credentials beside it)
//! is the daemon process's own; with `--no-daemon`, this process's. After changing
//! `XDG_CONFIG_HOME`, restart the daemon (`rdy serve --stop`).
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

/// How a remote is used.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum ModeArg {
    Share,
    Publish,
}

impl ModeArg {
    fn word(self) -> &'static str {
        match self {
            ModeArg::Share => "share",
            ModeArg::Publish => "publish",
        }
    }
}

/// The config level a remote edit changes.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum LevelArg {
    Project,
    Local,
    User,
}

impl LevelArg {
    fn word(self) -> &'static str {
        match self {
            LevelArg::Project => "project",
            LevelArg::Local => "local",
            LevelArg::User => "user",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RemoteCmd {
    /// List the remotes in effect: name, mode, URL and the config level that sets each one.
    List,
    /// Add a remote at one config level (default: the project config, .rdy.yaml).
    Add {
        /// The remote's name: 1 to 64 letters, digits, '.', '_' or '-', a letter or digit first.
        name: String,
        /// The remote's HTTPS URL.
        url: String,
        /// share (the project's shared line; at most one) or publish.
        #[arg(long, value_enum)]
        mode: ModeArg,
        /// The config level to change: project, local (.rdy/local.yaml) or user.
        #[arg(long, value_enum)]
        level: Option<LevelArg>,
    },
    /// Remove a remote from one config level (default: the project config).
    Rm {
        /// The remote's name.
        name: String,
        /// The config level to change: project, local or user.
        #[arg(long, value_enum)]
        level: Option<LevelArg>,
    },
    /// Change a remote's mode at one config level (default: the project config).
    SetMode {
        /// The remote's name.
        name: String,
        /// share or publish.
        #[arg(value_enum)]
        mode: ModeArg,
        /// The config level to change: project, local or user.
        #[arg(long, value_enum)]
        level: Option<LevelArg>,
    },
}

/// The op's arguments: only the fields the subcommand gives.
fn args(cmd: &RemoteCmd) -> Value {
    let level = |l: &Option<LevelArg>| l.map(LevelArg::word);
    match cmd {
        RemoteCmd::List => json!({"op": "list"}),
        RemoteCmd::Add {
            name,
            url,
            mode,
            level: l,
        } => json!({"op": "add", "name": name, "url": url, "mode": mode.word(),
            "level": level(l)}),
        RemoteCmd::Rm { name, level: l } => json!({"op": "rm", "name": name, "level": level(l)}),
        RemoteCmd::SetMode {
            name,
            mode,
            level: l,
        } => json!({"op": "set_mode", "name": name, "mode": mode.word(), "level": level(l)}),
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// The text of a reply: its lines, then each warning in the shared warning form.
fn text(cmd: &RemoteCmd, v: &Value) -> String {
    let mut out = match cmd {
        RemoteCmd::List => {
            let rows: Vec<String> = v["remotes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|r| {
                    format!(
                        "{} {} {} ({})",
                        s(r, "name"),
                        s(r, "mode"),
                        s(r, "url"),
                        s(r, "level")
                    )
                })
                .collect();
            if rows.is_empty() {
                "no remotes".to_string()
            } else {
                rows.join("\n")
            }
        }
        RemoteCmd::Rm { .. } => {
            let r = &v["remote"];
            format!(
                "removed remote {} from {} config",
                s(r, "name"),
                s(r, "level")
            )
        }
        RemoteCmd::Add { .. } | RemoteCmd::SetMode { .. } => {
            let r = &v["remote"];
            format!("remote {}: {} {}", s(r, "name"), s(r, "mode"), s(r, "url"))
        }
    };
    push_warnings(&mut out, v);
    out
}

/// Appends each warning of reply `v` in the shared warning form of hub settings and sign-in
/// replies (`config_warning_line`): `warning <code>: <message>; <fix>`.
pub(crate) fn push_warnings(out: &mut String, v: &Value) {
    for w in v["warnings"].as_array().into_iter().flatten() {
        out.push('\n');
        out.push_str(&format!(
            "warning {}: {}; {}",
            s(w, "code"),
            s(w, "message"),
            s(w, "fix")
        ));
    }
}

/// `rdy remote <cmd>`: the `remote` op's reply and its text.
pub(crate) fn remote(env: &Env, cmd: RemoteCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "remote", args(&cmd))?;
    let t = text(&cmd, &v);
    Ok((v, t))
}
