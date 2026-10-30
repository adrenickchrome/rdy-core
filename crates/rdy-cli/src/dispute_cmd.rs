//! ap5: `rdy dispute open | resolve | list` (D-ZWP7Q "Disputes", n-J783T). The commands build one
//! request JSON for the ctl `dispute` op; the renderer turns the op's reply into text. `--json`
//! prints the reply as it is.
use crate::conn::{Env, call};
use crate::output::hash_short;
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum DisputeCmd {
    /// Open a dispute on a landing. A person's act: an agent session relays it with the
    /// person's words (--relayed-by and --evidence).
    Open {
        /// The landing: a hash or a prefix of 6 or more.
        landing: String,
        /// Why the landing is wrong, 1 to 2000 characters.
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        relay: RelayFlags,
    },
    /// Resolve a dispute as upheld, dismissed or withdrawn. Only the person who opened it
    /// withdraws it. A person's act: an agent session relays it with the person's words
    /// (--relayed-by and --evidence).
    Resolve {
        /// The dispute: its id or a prefix of 6 or more.
        dispute: String,
        /// upheld, dismissed or withdrawn.
        #[arg(long)]
        outcome: String,
        /// With --outcome upheld: the landing that corrects the disputed one.
        #[arg(long, value_name = "LANDING")]
        correction: Option<String>,
        #[command(flatten)]
        relay: RelayFlags,
    },
    /// List the disputes with their state, oldest first.
    List {
        /// Only this landing's disputes.
        #[arg(long)]
        landing: Option<String>,
        /// Only the open disputes.
        #[arg(long)]
        open: bool,
    },
}

/// The relay flags of a person's act, as `rdy view own` has them.
#[derive(clap::Args)]
pub struct RelayFlags {
    /// The person's words, quoted, when an agent relays the act. Requires --relayed-by.
    #[arg(long, value_name = "WORDS", requires = "relayed_by")]
    evidence: Option<String>,
    /// The agent that relays the person's words: your own actor. The run comes from your
    /// session. Only an agent session relays. Requires --evidence.
    #[arg(long, value_name = "ACTOR", requires = "evidence")]
    relayed_by: Option<String>,
    /// The question the person was asked.
    #[arg(long, value_name = "TEXT", requires = "evidence")]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long = "session", value_name = "REF", requires = "evidence")]
    session_ref: Option<String>,
}

/// `args` with the relay flags of `r` that are set.
fn with_relay(mut args: Value, r: &RelayFlags) -> Value {
    for (k, v) in [
        ("evidence", &r.evidence),
        ("relayed_by", &r.relayed_by),
        ("question", &r.question),
        ("session_ref", &r.session_ref),
    ] {
        if let Some(v) = v {
            args[k] = json!(v);
        }
    }
    args
}

/// The `dispute` op's JSON for `cmd`.
pub fn args_of(cmd: &DisputeCmd) -> Value {
    match cmd {
        DisputeCmd::Open {
            landing,
            reason,
            relay,
        } => with_relay(
            json!({"op": "open", "landing": landing, "reason": reason}),
            relay,
        ),
        DisputeCmd::Resolve {
            dispute,
            outcome,
            correction,
            relay,
        } => {
            let mut args = json!({"op": "resolve", "dispute": dispute, "outcome": outcome});
            if let Some(c) = correction {
                args["correction"] = json!(c);
            }
            with_relay(args, relay)
        }
        DisputeCmd::List { landing, open } => {
            let mut args = json!({"op": "list"});
            if let Some(l) = landing {
                args["landing"] = json!(l);
            }
            if *open {
                args["open"] = json!(true);
            }
            args
        }
    }
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// `<id short> <state> landing <short>: <reason>`, then `  resolved <outcome> by <actor>[,
/// correction <short>]` when the dispute is resolved.
fn row_lines(d: &Value) -> String {
    let mut out = format!(
        "{} {} landing {}: {}",
        hash_short(text(&d["id"])),
        text(&d["state"]),
        hash_short(text(&d["landing"])),
        text(&d["reason"])
    );
    let r = &d["resolution"];
    if r.is_object() {
        out.push_str(&format!(
            "\n  resolved {} by {}",
            text(&r["outcome"]),
            text(&r["by"]["actor"])
        ));
        if let Some(c) = r["correction"].as_str() {
            out.push_str(&format!(", correction {}", hash_short(c)));
        }
    }
    out
}

/// The text of the reply to `cmd`: the dispute's lines for open and resolve; for list one row per
/// dispute, then `<n> more` when rows were omitted.
pub fn render(cmd: &DisputeCmd, v: &Value) -> String {
    match cmd {
        DisputeCmd::List { .. } => {
            let mut lines: Vec<String> = v["disputes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(row_lines)
                .collect();
            let omitted = v["omitted"].as_u64().unwrap_or(0);
            if omitted > 0 {
                lines.push(format!("{omitted} more"));
            }
            lines.join("\n")
        }
        _ => row_lines(&v["dispute"]),
    }
}

pub(crate) fn dispute(env: &Env, cmd: DisputeCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "dispute", args_of(&cmd))?;
    let t = render(&cmd, &v);
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ap5_list_text_names_each_row_and_the_omitted_count() {
        let l = "a".repeat(64);
        let v = json!({"disputes": [
            {"id": "1".repeat(64), "landing": l, "state": "upheld", "reason": "r1",
             "resolution": {"id": "2".repeat(64), "outcome": "upheld",
                            "by": {"actor": "human:a", "run": null}, "correction": "3".repeat(64)}},
            {"id": "4".repeat(64), "landing": l, "state": "withdrawn", "reason": "r2",
             "resolution": {"id": "5".repeat(64), "outcome": "withdrawn",
                            "by": {"actor": "human:b", "run": null}, "correction": null}},
            {"id": "6".repeat(64), "landing": l, "state": "open", "reason": "r3", "resolution": null}
        ], "omitted": 7});
        let list = DisputeCmd::List {
            landing: None,
            open: false,
        };
        assert_eq!(
            render(&list, &v),
            "111111111111 upheld landing aaaaaaaaaaaa: r1\n  resolved upheld by human:a, correction 333333333333\n\
             444444444444 withdrawn landing aaaaaaaaaaaa: r2\n  resolved withdrawn by human:b\n\
             666666666666 open landing aaaaaaaaaaaa: r3\n\
             7 more"
        );
        assert_eq!(render(&list, &json!({"disputes": [], "omitted": 0})), "");
    }
}
