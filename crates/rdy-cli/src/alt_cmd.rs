//! malt1: `rdy alt *`. The commands build one `AltRequest` JSON for the ctl `alt` op; the
//! renderer turns the op's reply into text. `--json` prints the reply as it is.
use crate::conn::{Env, call};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::E_BAD_INPUT;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum AltCmd {
    /// Group two or more open views that answer one question into an alternative set.
    Create {
        /// The question the members answer, in a few words.
        title: String,
        /// A member as label=view; give two or more.
        #[arg(long = "member", required = true)]
        members: Vec<String>,
        /// The question block the members answer (id, key or path).
        #[arg(long)]
        question: Option<String>,
    },
    /// Add a member, as label=view, to an open set.
    Add { set: String, member: String },
    /// Remove a member (its label, else its view) from an open set; a set keeps two members.
    Remove { set: String, member: String },
    /// List the open sets.
    Ls {
        /// Also list chosen and abandoned sets.
        #[arg(long)]
        all: bool,
    },
    /// Each member of a set against canon.
    Compare { set: String },
    /// Close every member view: no member is chosen.
    Abandon {
        set: String,
        /// Why no member is chosen.
        #[arg(long)]
        reason: String,
    },
}

fn bad_member(text: &str) -> ErrorReply {
    ErrorReply::new(
        E_BAD_INPUT,
        format!("member {text:?} is not label=view"),
        "name each member as label=view, like x=wording-a",
    )
}

/// `label=view` as the `{label, view}` JSON a request takes.
fn member_json(text: &str) -> Result<Value, ErrorReply> {
    match text.split_once('=') {
        Some((label, view)) if !label.trim().is_empty() && !view.trim().is_empty() => {
            Ok(json!({"label": label.trim(), "view": view.trim()}))
        }
        _ => Err(bad_member(text)),
    }
}

/// The `AltRequest` JSON for `cmd`.
pub fn args_of(cmd: &AltCmd) -> Result<Value, ErrorReply> {
    Ok(match cmd {
        AltCmd::Create {
            title,
            members,
            question,
        } => json!({
            "op": "create",
            "title": title,
            "question": question,
            "members": members.iter().map(|m| member_json(m)).collect::<Result<Vec<_>, _>>()?,
        }),
        AltCmd::Add { set, member } => {
            let m = member_json(member)?;
            json!({"op": "add", "set": set, "label": m["label"], "view": m["view"]})
        }
        AltCmd::Remove { set, member } => json!({"op": "remove", "set": set, "member": member}),
        AltCmd::Ls { all } => json!({"op": "list", "all": all.then_some(true)}),
        AltCmd::Compare { set } => json!({"op": "compare", "set": set}),
        AltCmd::Abandon { set, reason } => json!({"op": "abandon", "set": set, "reason": reason}),
    })
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// `<as-…> <title> (<status>): <label>=<view>, …`.
fn set_line(s: &Value) -> String {
    let members: Vec<String> = s["members"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| format!("{}={}", text(&m["label"]), text(&m["view"])))
        .collect();
    format!(
        "{} {} ({}): {}",
        text(&s["id"]),
        text(&s["title"]),
        text(&s["status"]),
        members.join(", ")
    )
}

/// The text of the reply to `cmd`.
pub fn render(cmd: &AltCmd, v: &Value) -> String {
    match cmd {
        AltCmd::Ls { .. } => match v["sets"].as_array() {
            Some(rows) if !rows.is_empty() => {
                rows.iter().map(set_line).collect::<Vec<_>>().join("\n")
            }
            _ => "no alternative sets".to_string(),
        },
        AltCmd::Compare { .. } => v["members"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                format!(
                    "{} ({}):\n{}",
                    text(&m["label"]),
                    text(&m["view"]),
                    crate::view_cmd::compare_text(&m["compare"])
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AltCmd::Abandon { .. } => {
            let closed: Vec<&str> = v["closed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!("{}\nclosed {}", set_line(&v["set"]), closed.join(", "))
        }
        _ => set_line(&v["set"]),
    }
}

pub(crate) fn alt(env: &Env, cmd: AltCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "alt", args_of(&cmd)?)?;
    let t = render(&cmd, &v);
    Ok((v, t))
}
