//! `rdy pending` and `rdy answer`, the CLI twins of `rdy_pending` and `rdy_answer` (D-KQ4CA,
//! D-JV6Z8). The chat path is primary; these are its fallback, with the same queue and the same
//! answers. A human session answers `local`; an agent session answers only by relaying the
//! person's words (`--relayed-by` with `--evidence`). The CLI has no Later (F4).
use crate::conn::{Env, call};
use crate::land_cmd::approve_text;
use crate::output::{num, text};
use rdy_daemon::ErrorReply;
use rdy_engine::E_BAD_INPUT;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct PendingArgs {
    /// Cut each card to this many tokens (default 2000). The card hash does not change.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
    /// The chat session whose Later answers hide cards (an agent's; a person has none).
    #[arg(long = "session")]
    session: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct AnswerArgs {
    /// The item ref and the option id (`rdy answer P-x@1 approve`); with --all-recommended, the
    /// item refs.
    #[arg(required = true, num_args = 1..)]
    items: Vec<String>,
    /// Answer each item with its recommended option.
    #[arg(long)]
    all_recommended: bool,
    /// The card hash `rdy pending` printed for the card; once for each item with --all-recommended.
    #[arg(long = "card")]
    card: Vec<String>,
    /// The person's note.
    #[arg(long)]
    note: Option<String>,
    /// A briefing token, for a question whose decision goes under stable content.
    #[arg(long)]
    brief: Option<String>,
    /// The agent that relays the person's words: your own actor. The run comes from your session. Records the person's answer, relayed.
    #[arg(long, value_name = "ACTOR", requires = "evidence")]
    relayed_by: Option<String>,
    /// The person's words, quoted. Requires --relayed-by.
    #[arg(long, requires = "relayed_by")]
    evidence: Option<String>,
    /// The question the person was asked.
    #[arg(long, requires = "relayed_by")]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long = "session", requires = "relayed_by")]
    session_ref: Option<String>,
    /// How you got the answer: card (the person saw the card; default) or text (their own words).
    #[arg(long, value_parser = ["card", "text"], requires = "relayed_by")]
    kind: Option<String>,
}

pub(crate) fn pending(env: &Env, a: PendingArgs) -> Result<(Value, String), ErrorReply> {
    let v = call(
        env,
        "pending",
        json!({"max_tokens": a.max_tokens, "session_ref": a.session}),
    )?;
    let t = pending_text(&v);
    Ok((v, t))
}

/// The queue as text: each card, one blank line between them; then a line for each issue of each
/// version that is not ready; then what the queue left out, and `<n> snoozed`.
fn pending_text(v: &Value) -> String {
    let cards: Vec<&str> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| text(&i["card"]))
        .collect();
    let mut lines: Vec<String> = vec![];
    for n in v["not_ready"].as_array().into_iter().flatten() {
        for i in n["issues"].as_array().into_iter().flatten() {
            lines.push(format!(
                "not ready: {}: {} (fix: {})",
                text(&n["ref"]),
                text(&i["message"]),
                text(&i["fix"])
            ));
        }
    }
    let omitted = num(&v["omitted"]);
    if omitted > 0 {
        lines.push(format!(
            "{omitted} more items wait: answer these first, then run rdy pending again"
        ));
    }
    lines.push(format!("{} snoozed", num(&v["snoozed"])));
    let tail = lines.join("\n");
    if cards.is_empty() {
        tail
    } else {
        format!("{}\n\n{tail}", cards.join("\n\n"))
    }
}

fn bad(what: &str, fix: &str) -> ErrorReply {
    ErrorReply::new(E_BAD_INPUT, what, fix)
}

pub(crate) fn answer(env: &Env, a: AnswerArgs) -> Result<(Value, String), ErrorReply> {
    let AnswerArgs {
        items,
        all_recommended,
        card,
        note,
        brief,
        relayed_by,
        evidence,
        question,
        session_ref,
        kind,
    } = a;
    let mut args = json!({
        "note": note,
        "brief": brief,
        "relayed_by": relayed_by,
        "evidence": evidence,
        "question": question,
        "session_ref": session_ref,
        "kind": kind,
    });
    if all_recommended {
        if !card.is_empty() && card.len() != items.len() {
            return Err(bad(
                "--card goes once for each item, or not at all",
                "pass --card <hash> for every ref, in the order of the refs",
            ));
        }
        args["all_recommended"] = items
            .iter()
            .enumerate()
            .map(|(n, r)| json!({"ref": r, "card_hash": card.get(n)}))
            .collect();
    } else {
        let [r#ref, option] = items.as_slice() else {
            return Err(bad(
                "an answer names its item and its option",
                "run rdy answer <ref> <option>, or rdy answer --all-recommended <ref>...",
            ));
        };
        if card.len() > 1 {
            return Err(bad(
                "an answer takes one --card",
                "pass the hash of the card the person was shown",
            ));
        }
        args["ref"] = json!(r#ref);
        args["option"] = json!(option);
        args["card_hash"] = json!(card.first());
    }
    let v = call(env, "answer", args)?;
    let t = match v.as_array() {
        Some(done) => done
            .iter()
            .map(answered_text)
            .collect::<Vec<_>>()
            .join("\n"),
        None => answered_text(&v),
    };
    Ok((v, t))
}

/// `answered <ref>: <label>` and, for an approval, the landing lines of `rdy approve`.
fn answered_text(v: &Value) -> String {
    let mut t = format!("answered {}: {}", text(&v["ref"]), text(&v["label"]));
    if v["landing"].is_object() {
        let mut landing = v["landing"].clone();
        landing["relayed_by"] = v["relayed_by"].clone();
        t.push('\n');
        t.push_str(&approve_text(&landing));
    }
    t
}
