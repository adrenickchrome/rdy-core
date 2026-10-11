use crate::brief_text;
use crate::conn::{Env, call};
use crate::output::{hash_short, new_keys_text, num, text};
use rdy_daemon::ErrorReply;
use rdy_engine::E_BAD_INPUT;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct ProposeArgs {
    #[arg(long)]
    title: String,
    /// The view to propose (default: the session's, `work` for the human).
    #[arg(long)]
    view: Option<String>,
    /// Your plain-words summary for the approver.
    #[arg(long)]
    summary: Option<String>,
    /// The alternative set (as-<id>) the view is a member of; goes with --chosen.
    #[arg(long)]
    alt: Option<String>,
    /// The view's own member of that set (its label); goes with --alt.
    #[arg(long)]
    chosen: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ApproveArgs {
    proposal: String,
    /// The agent that relays the human's approval, as actor[/run]. Records a relayed approval.
    #[arg(long, requires = "evidence")]
    relayed_by: Option<String>,
    /// The human's words, quoted. Requires --relayed-by and the exact version.
    #[arg(long, requires = "relayed_by")]
    evidence: Option<String>,
    /// The question the human was asked.
    #[arg(long, requires = "relayed_by")]
    question: Option<String>,
    /// Where the human said it (default: cli).
    #[arg(long = "session", requires = "relayed_by")]
    session_ref: Option<String>,
    /// Refuse the approval when canon has moved since the version was proposed.
    #[arg(long)]
    strict: bool,
}

#[derive(clap::Args)]
pub(crate) struct TagArgs {
    version: String,
    #[arg(long)]
    landing: Option<String>,
    #[arg(short = 'm', long)]
    message: Option<String>,
}

pub(crate) fn propose(env: &Env, a: ProposeArgs) -> Result<(Value, String), ErrorReply> {
    let ProposeArgs {
        title,
        view,
        summary,
        alt,
        chosen,
    } = a;
    let alternatives = match (alt, chosen) {
        (Some(set), Some(chosen)) => Some(json!({"set": set, "chosen": chosen})),
        (None, None) => None,
        _ => {
            return Err(ErrorReply::new(
                E_BAD_INPUT,
                "--alt and --chosen go together",
                "pass both: --alt <set> --chosen <member>, or neither",
            ));
        }
    };
    let v = call(
        env,
        "propose",
        json!({"title": title, "view": view, "summary": summary, "alternatives": alternatives}),
    )?;
    let t = propose_text(&v);
    Ok((v, t))
}

/// The propose reply as text: the proposal line, its summary and alternative, the blocking
/// conflicts and the tensions, the consequences part (coverage included) with a `not worked
/// out:` line for each part rdy could not read, the new keys, and last the approve command.
fn propose_text(v: &Value) -> String {
    let pref = text(&v["ref"]);
    let mut t = format!("proposal {pref} ({} changes)", num(&v["changes"]));
    if let Some(s) = v["summary"].as_str() {
        t.push_str(&format!("\nsummary: {s}"));
    }
    if let Some(member) = v["alternative"]["member"].as_str() {
        t.push_str(&format!(
            "\nchooses {member} of {}",
            text(&v["alternative"]["set"])
        ));
    }
    let blocking = v["blocking"].as_array().map_or(0, Vec::len);
    if blocking > 0 {
        t.push_str(&format!(
            "\nblocking: {blocking} conflict(s) on blocks this proposal writes"
        ));
    }
    let tensions = v["tensions"].as_array().map_or(&[][..], Vec::as_slice);
    if !tensions.is_empty() {
        let refusing: Vec<&str> = tensions
            .iter()
            .filter(|t| t["refuses"].as_bool() == Some(true))
            .map(|t| text(&t["id"]))
            .collect();
        t.push_str(&format!(
            "\ntensions: {} ({} would refuse the landing{})",
            tensions.len() as u64 + num(&v["tensions_omitted"]),
            refusing.len(),
            if refusing.is_empty() {
                String::new()
            } else {
                format!(": {}", refusing.join(", "))
            }
        ));
    }
    t.push_str(&brief_text::consequences_text(v));
    t.push_str(&new_keys_text(v));
    t.push_str(&format!("\napprove with: rdy approve {pref}"));
    t
}

pub(crate) fn approve(env: &Env, a: ApproveArgs) -> Result<(Value, String), ErrorReply> {
    let ApproveArgs {
        proposal,
        relayed_by,
        evidence,
        question,
        session_ref,
        strict,
    } = a;
    let v = call(
        env,
        "approve",
        json!({
            "proposal": proposal,
            "relayed_by": relayed_by,
            "evidence": evidence,
            "question": question,
            "session_ref": session_ref,
            "strict": strict.then_some(true),
        }),
    )?;
    let approved = text(&v["approved"]);
    let mut t = format!(
        "approving {approved}: {} ({} change(s))",
        text(&v["title"]),
        num(&v["changes"]),
    );
    if let Some(s) = v["summary"].as_str() {
        t.push_str(&format!("\nsummary: {s}"));
    }
    t.push_str(&format!(
        "\nlanded {} ({approved})",
        hash_short(text(&v["landing"]))
    ));
    if let Some(by) = relayed_by {
        t.push_str(&format!(", relayed by {by}"));
    }
    if let Some(member) = v["chosen"]["member"].as_str() {
        t.push_str(&format!(
            "\nchose {member} of {}",
            text(&v["chosen"]["set"])
        ));
        let closed: Vec<&str> = v["closed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !closed.is_empty() {
            t.push_str(&format!("; closed {}", closed.join(", ")));
        }
    }
    t.push_str(&brief_text::consequences_text(&v));
    for r in v["rebased"].as_array().into_iter().flatten() {
        if num(&r["conflicts"]) > 0 {
            t.push_str(&format!(
                "\nrebased {}: {} conflict(s)",
                text(&r["view"]),
                num(&r["conflicts"])
            ));
        }
    }
    t.push_str(&new_keys_text(&v));
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    Ok((v, t))
}

pub(crate) fn tag(env: &Env, a: TagArgs) -> Result<(Value, String), ErrorReply> {
    let TagArgs {
        version,
        landing,
        message,
    } = a;
    let v = call(
        env,
        "tag",
        json!({"version": version, "landing": landing, "message": message}),
    )?;
    let t = format!(
        "version {} → landing {}",
        text(&v["name"]),
        hash_short(text(&v["landing"]))
    );
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A propose reply that could not read its tensions or its coverage says so, as a write
    /// reply does (M2f-b), instead of printing nothing for them.
    #[test]
    fn cc16_propose_text_names_what_was_not_worked_out() {
        let v = json!({
            "ref": "P-1@1", "changes": 1, "tensions": [],
            "tensions_error": "E_IO: tensions busy",
            "coverage_error": "E_IO: tensions busy"
        });
        assert_eq!(
            propose_text(&v),
            "proposal P-1@1 (1 changes)\nnot worked out: the tensions (E_IO: tensions busy)\nnot worked out: the coverage change (E_IO: tensions busy)\napprove with: rdy approve P-1@1"
        );
    }
}
