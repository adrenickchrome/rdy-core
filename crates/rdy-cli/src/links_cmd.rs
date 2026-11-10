//! `rdy links` and `rdy adopt-upstream`, the CLI twins of `rdy_links` and of `rdy_supersede
//! {old, adopted_as}` (repo links, D-DAPR8, D-8VJAZ, D-KQ4CA). The chat path is primary; these
//! send the daemon's `links` and `adopt_upstream` ops with those ops' keys and print the replies.
//! A delivery moves no pin, and an adopt card in `rdy pending` asks the person: this module has
//! no take, export or revoke, no exchange between two projects, and no Later.
use crate::conn::{Env, bound_view, call, human_session};
use crate::edit_cmd::write_text;
use crate::output::{num, text};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct LinksArgs {
    #[command(subcommand)]
    pub(crate) cmd: Option<LinksCmd>,
}

#[derive(Subcommand)]
pub(crate) enum LinksCmd {
    /// What a link delivers: each block with its pin state, and the instructions the link has
    /// not adopted. The chat path is rdy_links with op show.
    Show {
        /// The alias of a repo link (rdy links lists them).
        alias: String,
    },
    /// The pins of canon to cited content, by repo; an alias limits it to that link's repo. The
    /// chat path is rdy_links with op refs.
    Refs {
        /// The alias of a repo link.
        alias: Option<String>,
    },
}

#[derive(clap::Args)]
pub(crate) struct AdoptUpstreamArgs {
    /// The local decision to supersede: an accepted decision in force (id or key).
    old: String,
    /// The cited decision that is its upstream copy: `<alias>:<id>` or `<alias>:<KEY>`.
    #[arg(value_name = "ALIAS:ID")]
    adopted_as: String,
    /// The view to write (default: the session's, `work` for the human).
    #[arg(long)]
    view: Option<String>,
}

/// The args of the `links` op for `a`: only what the command line named.
pub(crate) fn request(a: &LinksArgs) -> Value {
    match &a.cmd {
        None => json!({}),
        Some(LinksCmd::Show { alias }) => json!({"op": "show", "alias": alias}),
        Some(LinksCmd::Refs { alias: None }) => json!({"op": "refs"}),
        Some(LinksCmd::Refs { alias: Some(alias) }) => json!({"op": "refs", "alias": alias}),
    }
}

/// The args of the `adopt_upstream` op for `a`: the view only when it is named.
pub(crate) fn adopt_request(a: &AdoptUpstreamArgs) -> Value {
    let mut args = json!({"old": a.old, "adopted_as": a.adopted_as});
    if let Some(view) = &a.view {
        args["view"] = json!(view);
    }
    args
}

pub(crate) fn links(env: &Env, a: LinksArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "links", request(&a))?;
    let t = match &a.cmd {
        None => list_text(&v),
        Some(LinksCmd::Show { .. }) => show_text(&v),
        Some(LinksCmd::Refs { .. }) => refs_text(&v),
    };
    Ok((v, t))
}

/// `rdy adopt-upstream`: the daemon fetches the briefing itself, as `rdy reaffirm` does. A
/// person's working copy writes to the view it is bound to, as `rdy new` does.
pub(crate) fn adopt_upstream(
    env: &Env,
    mut a: AdoptUpstreamArgs,
) -> Result<(Value, String), ErrorReply> {
    if a.view.is_none() && human_session(env)? {
        a.view = bound_view(env)?;
    }
    let v = call(env, "adopt_upstream", adopt_request(&a))?;
    let t = write_text(&v);
    Ok((v, t))
}

/// The first 12 characters of a hash or repo id.
fn short(s: &str) -> &str {
    s.get(..12).unwrap_or(s)
}

/// `<n> pin(s) behind`.
fn behind_words(n: usize) -> String {
    format!("{n} {} behind", if n == 1 { "pin" } else { "pins" })
}

/// `, tag <tag>` when the link has a delivery.
fn tag_part(row: &Value) -> String {
    row["tag"]
        .as_str()
        .map(|t| format!(", tag {t}"))
        .unwrap_or_default()
}

/// The status line of a link, as `rdy status` prints it: no repo and no profile.
pub(crate) fn status_line(row: &Value) -> String {
    format!(
        "link {}: {}{}, {}",
        text(&row["alias"]),
        text(&row["state"]),
        tag_part(row),
        behind_words(num(&row["pins_behind"]) as usize)
    )
}

/// The line of a link in `rdy links`: its state, last delivery, repo, profile and pins behind.
fn link_line(row: &Value) -> String {
    let title = row["title"]
        .as_str()
        .map(|t| format!(" ({t})"))
        .unwrap_or_default();
    format!(
        "link {}{title}: {}{}, repo {}, profile {}, {}",
        text(&row["alias"]),
        text(&row["state"]),
        tag_part(row),
        short(text(&row["repo"])),
        text(&row["profile"]),
        behind_words(num(&row["pins_behind"]) as usize)
    )
}

fn list_text(v: &Value) -> String {
    let mut lines: Vec<String> = v["links"]
        .as_array()
        .into_iter()
        .flatten()
        .map(link_line)
        .collect();
    for u in v["unlinked"].as_array().into_iter().flatten() {
        lines.push(format!(
            "unlinked repo {}: {}{}, {}",
            short(text(&u["repo"])),
            text(&u["state"]),
            tag_part(u),
            behind_words(num(&u["pins_behind"]) as usize)
        ));
    }
    if lines.is_empty() {
        "no repo links".to_string()
    } else {
        lines.join("\n")
    }
}

fn show_text(v: &Value) -> String {
    let mut lines = vec![link_line(&v["link"])];
    let blocks = v["blocks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if blocks.is_empty() {
        lines.push("  nothing delivered".to_string());
    }
    for b in blocks {
        let mut line = format!(
            "  {} {} {}  {}",
            text(&b["id"]),
            text(&b["block_type"]),
            short(text(&b["version"])),
            text(&b["summary"])
        );
        if let Some(s) = b["status"].as_str() {
            line.push_str(&format!(" [{s}]"));
        }
        if let Some(p) = b["pin"].as_str() {
            line.push_str(&format!(" (pin {p})"));
        }
        lines.push(line);
    }
    for p in v["pending"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  not adopted: instruction {} {} {}  {}",
            text(&p["id"]),
            text(&p["change"]),
            short(text(&p["version"])),
            text(&p["summary"])
        ));
    }
    let omitted = num(&v["omitted"]);
    if omitted > 0 {
        lines.push(format!("  {omitted} more blocks are not shown"));
    }
    lines.join("\n")
}

fn refs_text(v: &Value) -> String {
    let mut lines = vec![];
    for r in v["repos"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{} (repo {})",
            text(&r["alias"]),
            short(text(&r["repo"]))
        ));
        for p in r["pins"].as_array().into_iter().flatten() {
            let held = p["held"].as_str().map_or("none", short);
            let delivered = p["delivered"]
                .as_str()
                .map_or("no delivery selects it".to_string(), |d| {
                    format!("delivered {}", short(d))
                });
            let cited_by: Vec<&str> = p["cited_by"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            lines.push(format!(
                "  {} held {held}, {delivered}, {}; cited by {}",
                text(&p["target"]),
                if p["behind"] == true {
                    "behind"
                } else {
                    "current"
                },
                cited_by.join(", ")
            ));
        }
    }
    let omitted = num(&v["omitted"]);
    if omitted > 0 {
        lines.push(format!("{omitted} more pins are not shown"));
    }
    if lines.is_empty() {
        "no pins to cited content".to_string()
    } else {
        lines.join("\n")
    }
}
