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
    /// You, the relaying agent, as actor[/run]; run this under your own RDY_ACTOR. Proposes the person's own view (one they own, or work) on their word.
    #[arg(long, requires = "evidence")]
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
}

#[derive(clap::Args)]
pub(crate) struct ApproveArgs {
    proposal: String,
    /// You, the relaying agent, as actor[/run]; run this under your own RDY_ACTOR. Records a relayed approval.
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
    /// You, the relaying agent, as actor[/run]; run this under your own RDY_ACTOR. Records the person's tag, relayed.
    #[arg(long, requires = "evidence")]
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
}

pub(crate) fn propose(env: &Env, a: ProposeArgs) -> Result<(Value, String), ErrorReply> {
    let ProposeArgs {
        title,
        view,
        summary,
        alt,
        chosen,
        relayed_by,
        evidence,
        question,
        session_ref,
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
        json!({
            "title": title,
            "view": view,
            "summary": summary,
            "alternatives": alternatives,
            "relayed_by": relayed_by,
            "evidence": evidence,
            "question": question,
            "session_ref": session_ref,
        }),
    )?;
    let t = propose_text(&v);
    Ok((v, t))
}

/// The propose reply as text: the proposal line, its summary and alternative, the blocking
/// conflicts and the tensions, the consequences part (coverage included) with a `not worked
/// out:` line for each part rdy could not read, the new keys, the warnings, and last the
/// approve command.
fn propose_text(v: &Value) -> String {
    let pref = text(&v["ref"]);
    let mut t = format!("proposal {pref} ({} changes)", num(&v["changes"]));
    t.push_str(&relayed_by_text(v));
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
        // D-WMCFW: each open advised semantic tension not yet accepted (a dismissed one is not
        // advised), those past the cut, and how to accept the advice for the set shown
        let mut advised = false;
        for tn in tensions {
            let j = &tn["judged"];
            if let Some(advice) = j["advice"].as_str()
                && !j["accepted"].is_object()
                && tn["status"].as_str() == Some("open")
            {
                advised = true;
                t.push_str(&format!(
                    "\nadvised to block: {} by {}: {advice}",
                    text(&tn["id"]),
                    text(&j["actor"])
                ));
            }
        }
        let more = num(&v["advised_omitted"]);
        if more > 0 {
            advised = true;
            t.push_str(&format!(
                "\nand {more} more advised to block (rdy show {pref} lists them)"
            ));
        }
        if let Some(d) = v["advised_digest"].as_str()
            && advised
        {
            t.push_str(&format!(
                "\naccept the advice with: rdy resolve --accept {pref} --digest {d}"
            ));
        }
    }
    t.push_str(&brief_text::consequences_text(v));
    t.push_str(&new_keys_text(v));
    // D-2YCVK: the propose report warns (W_UNAPPLIED_DECISION), as approve prints its warnings
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
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
    let t = approve_text(&v);
    Ok((v, t))
}

/// The landing lines of an approval, from its reply: the proposal, its summary, the landing,
/// the relay, the chosen alternative, the consequences, the rebases, new keys and warnings.
pub(crate) fn approve_text(v: &Value) -> String {
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
    t.push_str(&relayed_by_text(v));
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
    t.push_str(&brief_text::consequences_text(v));
    for r in v["rebased"].as_array().into_iter().flatten() {
        if num(&r["conflicts"]) > 0 {
            t.push_str(&format!(
                "\nrebased {}: {} conflict(s)",
                text(&r["view"]),
                num(&r["conflicts"])
            ));
        }
    }
    t.push_str(&new_keys_text(v));
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    t
}

/// `, relayed by <actor>[/<run>]`: the relayer as recorded (the session's actor and run, which
/// may differ from the flag); empty when the reply names none.
fn relayed_by_text(v: &Value) -> String {
    match (
        v["relayed_by"]["actor"].as_str(),
        v["relayed_by"]["run"].as_str(),
    ) {
        (Some(by), Some(run)) => format!(", relayed by {by}/{run}"),
        (Some(by), None) => format!(", relayed by {by}"),
        (None, _) => String::new(),
    }
}

pub(crate) fn tag(env: &Env, a: TagArgs) -> Result<(Value, String), ErrorReply> {
    let TagArgs {
        version,
        landing,
        message,
        relayed_by,
        evidence,
        question,
        session_ref,
    } = a;
    let v = call(
        env,
        "tag",
        json!({
            "version": version,
            "landing": landing,
            "message": message,
            "relayed_by": relayed_by,
            "evidence": evidence,
            "question": question,
            "session_ref": session_ref,
        }),
    )?;
    let t = format!(
        "version {} → landing {}{}",
        text(&v["name"]),
        hash_short(text(&v["landing"])),
        relayed_by_text(&v)
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

    /// Final review I3 (D-2YCVK): the propose report a person reads names each warning, as
    /// `rdy approve` does, before the approve command; `W_UNAPPLIED_DECISION` among them.
    #[test]
    fn cc16_propose_text_prints_the_warnings() {
        let v = json!({
            "ref": "P-1@1", "changes": 1, "tensions": [],
            "warnings": [{"severity": "warning", "code": "W_UNAPPLIED_DECISION", "block": "d-1",
                "message": "decision d-1 lands with no content that applies it",
                "fix": "link the content that carries it with decided_by"}]
        });
        assert_eq!(
            propose_text(&v),
            "proposal P-1@1 (1 changes)\nwarning W_UNAPPLIED_DECISION: decision d-1 lands with no content that applies it\napprove with: rdy approve P-1@1"
        );
    }

    /// D-WMCFW: the propose report names each advised semantic tension not yet accepted, and
    /// the command that accepts the advice; an accepted one is not named again.
    #[test]
    fn se8_propose_text_names_the_advice_and_how_to_accept_it() {
        let v = json!({
            "ref": "P-1@1", "changes": 1, "tensions_omitted": 2,
            "advised_digest": "0123456789ab", "advised_omitted": 2,
            "tensions": [
                {"id": "tn-aaaaaaaaaaaa", "refuses": false, "status": "open",
                    "judged": {"actor": "agent:r", "advice": "it contradicts the gate rule"}},
                {"id": "tn-bbbbbbbbbbbb", "refuses": true, "status": "open",
                    "judged": {"actor": "agent:s", "advice": "x", "accepted": {"actor": "human:qa"}}},
                {"id": "tn-cccccccccccc", "refuses": false, "status": "open", "judged": {"actor": "agent:r"}},
                {"id": "tn-dddddddddddd", "refuses": false, "status": "dismissed",
                    "judged": {"actor": "agent:r", "advice": "y"}}
            ]
        });
        assert_eq!(
            propose_text(&v),
            "proposal P-1@1 (1 changes)\ntensions: 6 (1 would refuse the landing: tn-bbbbbbbbbbbb)\nadvised to block: tn-aaaaaaaaaaaa by agent:r: it contradicts the gate rule\nand 2 more advised to block (rdy show P-1@1 lists them)\naccept the advice with: rdy resolve --accept P-1@1 --digest 0123456789ab\napprove with: rdy approve P-1@1"
        );
        // FR1 finding 3: a dismissed tension is not advised, so nothing asks to accept it
        let v = json!({
            "ref": "P-1@1", "changes": 1,
            "tensions": [{"id": "tn-dddddddddddd", "refuses": false, "status": "dismissed",
                "judged": {"actor": "agent:r", "advice": "y"}}]
        });
        assert_eq!(
            propose_text(&v),
            "proposal P-1@1 (1 changes)\ntensions: 1 (0 would refuse the landing)\napprove with: rdy approve P-1@1"
        );
    }
}
