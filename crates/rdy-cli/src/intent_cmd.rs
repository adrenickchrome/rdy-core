//! mint1: `rdy intent *`. The commands build one `IntentRequest` JSON for the ctl `intent` op;
//! the renderer turns the op's reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum IntentCmd {
    /// Say what you mean to change: an area or blocks, with an expiry. It never locks anything.
    #[command(group(
        clap::ArgGroup::new("target")
            .required(true)
            .multiple(true)
            .args(["area", "blocks"])
    ))]
    Post {
        /// An area from the pack (rdy pack check lists them).
        #[arg(long)]
        area: Option<String>,
        /// Block ids, keys or paths.
        #[arg(long, num_args = 1..)]
        blocks: Vec<String>,
        /// What you mean to do, in a few words.
        #[arg(long)]
        angle: String,
        /// How long it holds: 30m, 2h or 7d (default 2h, at most 7d).
        #[arg(long)]
        ttl: Option<String>,
        /// The view the intent is about (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
    /// Hold an intent for longer: it expires ttl from now.
    Renew {
        id: String,
        #[arg(long)]
        ttl: Option<String>,
    },
    /// End an intent now.
    Withdraw { id: String },
    /// List the open intents.
    List {
        #[arg(long)]
        area: Option<String>,
        /// Only intents that cover this block (id, key or path).
        #[arg(long)]
        block: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        view: Option<String>,
        /// Also list expired and withdrawn intents.
        #[arg(long)]
        include_expired: bool,
    },
}

/// The `IntentRequest` JSON for `cmd`.
pub fn args_of(cmd: &IntentCmd) -> Value {
    match cmd {
        IntentCmd::Post {
            area,
            blocks,
            angle,
            ttl,
            view,
        } => json!({
            "op": "post",
            "area": area,
            "blocks": (!blocks.is_empty()).then_some(blocks),
            "angle": angle,
            "ttl": ttl,
            "view": view,
        }),
        IntentCmd::Renew { id, ttl } => json!({"op": "renew", "intent": id, "ttl": ttl}),
        IntentCmd::Withdraw { id } => json!({"op": "withdraw", "intent": id}),
        IntentCmd::List {
            area,
            block,
            actor,
            view,
            include_expired,
        } => json!({
            "op": "list",
            "area": area,
            "block": block,
            "actor": actor,
            "view": view,
            "include_expired": include_expired.then_some(true),
        }),
    }
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// `intent <id> <status> until <expires_at>: <angle>`.
fn one_line(i: &Value) -> String {
    format!(
        "intent {} {} until {}: {}",
        text(&i["id"]),
        text(&i["status"]),
        text(&i["expires_at"]),
        text(&i["angle"])
    )
}

/// `<id>  <status>  <actor>[ (run <run>)]  <view>  <area or -> <n> block(s)  until <expires_at>  <angle>`.
fn list_line(i: &Value) -> String {
    let who = rdy_engine::identity::actor_run_text(text(&i["actor"]), i["run"].as_str());
    format!(
        "{}  {}  {}  {}  {} {} block(s)  until {}  {}",
        text(&i["id"]),
        text(&i["status"]),
        who,
        text(&i["view"]),
        i["area"].as_str().unwrap_or("-"),
        i["blocks"].as_array().map_or(0, Vec::len),
        text(&i["expires_at"]),
        text(&i["angle"])
    )
}

/// The text of the reply to `cmd`.
pub fn render(cmd: &IntentCmd, v: &Value) -> String {
    match cmd {
        IntentCmd::List { .. } => match v["intents"].as_array() {
            Some(rows) if !rows.is_empty() => {
                rows.iter().map(list_line).collect::<Vec<_>>().join("\n")
            }
            _ => "no intents".to_string(),
        },
        _ => one_line(&v["intent"]),
    }
}

pub(crate) fn intent(env: &Env, cmd: IntentCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "intent", args_of(&cmd))?;
    let t = render(&cmd, &v);
    Ok((v, t))
}
