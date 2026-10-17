use crate::brief_text;
use crate::conn::{Conn, Env, call, human_session};
use crate::output::{hash_short, new_keys_text, num, text};
use rdy_daemon::ErrorReply;
use rdy_engine::E_BAD_INPUT;
use rdy_engine::pending::CARD_MAX_TOKENS;
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

/// `rdy approve`: the approval op, through `land_with` (no switch of its own skips the person).
/// A person approving, or an agent relaying a person, first reads the needs-you queue on the
/// same session; when the version it names carries content removal (Task 8's card), the reply
/// carries that card as `removal_card` and the text shows it whole before the landing lines
/// (D-CC90E, D-6B1W1). An agent's own approval is refused as before, without the read.
pub(crate) fn approve(env: &Env, a: ApproveArgs) -> Result<(Value, String), ErrorReply> {
    let ApproveArgs {
        proposal,
        relayed_by,
        evidence,
        question,
        session_ref,
        strict,
    } = a;
    let reads_card = human_session(env) || relayed_by.is_some();
    let args = json!({
        "proposal": proposal,
        "relayed_by": relayed_by,
        "evidence": evidence,
        "question": question,
        "session_ref": session_ref,
        "strict": strict.then_some(true),
    });
    let mut c = Conn::open(env)?;
    let out = (|| {
        let card = if reads_card {
            removal_card(&mut c, &proposal)?
        } else {
            None
        };
        Ok::<_, ErrorReply>((c.op("approve", args)?, card))
    })();
    c.close();
    let (mut v, card) = out?;
    let t = approve_text_with(&v, card.as_ref());
    if let Some(card) = card {
        v["removal_card"] = card;
    }
    Ok((v, t))
}

/// The removal card of the version `proposal` names, as the queue holds it now (`ref`, `card`,
/// `card_hash` and `removal`), read at the card's own budget so no mandatory line is cut; `None`
/// when the queue holds no removal card for it.
fn removal_card(c: &mut Conn, proposal: &str) -> Result<Option<Value>, ErrorReply> {
    let q = c.op("pending", json!({"max_tokens": CARD_MAX_TOKENS}))?;
    Ok(q["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["removal"].is_array() && names_item(proposal, text(&i["ref"])))
        .map(|i| {
            json!({
                "ref": i["ref"],
                "card": i["card"],
                "card_hash": i["card_hash"],
                "removal": i["removal"],
            })
        }))
}

/// Whether `named` (what `rdy approve` was given) names the queue item `item` (`P-<id>@<n>`):
/// the same ref, or the same stable id (a prefix of at least 4 hex digits, either way round)
/// with no version, which the queue lists at its latest. A version hash names no item.
pub(crate) fn names_item(named: &str, item: &str) -> bool {
    let split = |r: &str| -> Option<(String, Option<String>)> {
        let rest = r.strip_prefix("P-")?;
        let (id, n) = match rest.split_once('@') {
            Some((id, n)) => (id, Some(n.to_string())),
            None => (rest, None),
        };
        (id.len() >= 4 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| (id.to_ascii_lowercase(), n))
    };
    let (Some((nid, nn)), Some((iid, Some(inn)))) = (split(named), split(item)) else {
        return false;
    };
    (nid.starts_with(&iid) || iid.starts_with(&nid)) && nn.is_none_or(|n| n == inn)
}

/// The approval text of reply `v`: the removal card the person approved, when there is one,
/// then the landing lines ([`approve_text`]).
pub(crate) fn approve_text_with(v: &Value, card: Option<&Value>) -> String {
    let lines = approve_text(v);
    match card.and_then(|c| c["card"].as_str()) {
        Some(card) => format!("{card}\n\n{lines}"),
        None => lines,
    }
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
    use rdy_engine::pending::{
        CARD_MAX_TOKENS, Item, ItemKind, ItemOption, RemovalCard, card_hash, render_checked,
    };
    use rdy_model::Hash;
    use rdy_model::redaction::ReasonClass;
    use std::collections::BTreeMap;

    /// The queue's approve item of a version that carries one secret removal request, with its
    /// card rendered whole and its digest, as the `pending` op serializes it.
    fn removal_item() -> (Item, Value) {
        let report = Hash([7; 32]);
        let mut item = Item {
            r#ref: "P-0123456789ab@1".into(),
            kind: ItemKind::Approve,
            title: "Remove the code".into(),
            why: "ready to land: 1 changes".into(),
            blocking: false,
            summary: Some("Removes the leaked code.".into()),
            summary_by: Some("agent:a".into()),
            facts: vec![],
            members: vec![],
            options: vec![ItemOption {
                id: "approve".into(),
                label: "Approve P-0123456789ab@1".into(),
                consequences: vec![
                    "Lands the proposal in canon.".into(),
                    "Approved content removal cannot be undone.".into(),
                ],
                recommended: Some("Removes the leaked code.".into()),
            }],
            raised_by: "agent:a".into(),
            raised_at: "2026-10-17T00:00:00Z".into(),
            age: "today".into(),
            batch: None,
            exception: None,
            card: String::new(),
            card_hash: String::new(),
            removal: Some(vec![RemovalCard {
                request: Hash([9; 32]),
                report,
                versions: 2,
                blobs: 0,
                shared_blobs: 0,
                payloads: 3,
                bytes: 640,
                reason_class: ReasonClass::Secret,
                reason: "a leaked code".into(),
            }]),
            removal_reports: BTreeMap::from([(report, vec![])]),
        };
        item.card = render_checked(&item, CARD_MAX_TOKENS).unwrap();
        item.card_hash = card_hash(&item).to_string();
        let v = serde_json::to_value(&item).unwrap();
        (item, v)
    }

    /// rdx13 (D-CC90E, Task 8): a person's direct `rdy approve` of a version that carries a
    /// removal request prints that version's whole removal card, every mandatory line, before
    /// the landing lines; an ordinary approval prints what it always printed.
    #[test]
    fn rdx13_direct_approval_text_displays_the_complete_card() {
        let (item, v) = removal_item();
        let reply = json!({
            "approved": "P-0123456789ab@1", "title": "Remove the code", "changes": 1,
            "landing": "ab".repeat(32)
        });
        let plain = approve_text(&reply);
        assert_eq!(approve_text_with(&reply, None), plain);
        let t = approve_text_with(&reply, Some(&v));
        assert_eq!(t, format!("{}\n\n{plain}", item.card));
        let request = "09".repeat(32);
        let report = "07".repeat(32);
        for line in [
            "Content removal".to_string(),
            format!("  Request: {request}"),
            format!("  Report: {report}"),
            "  Versions: 2; blobs: 0; shared blobs: 0.".to_string(),
            "  Payloads: 3; bytes: 640.".to_string(),
            "  Reason class: secret.".to_string(),
            "  Reason: a leaked code".to_string(),
            format!("  {}", rdy_engine::W_REDACTION_COPIES_MESSAGE),
            format!("  {}", rdy_engine::W_REDACTION_QUOTES_MESSAGE),
            format!("  {}", rdy_engine::W_REDACTION_REUSE_MESSAGE),
            format!("  {}", rdy_engine::W_REDACTION_SECRET_MESSAGE),
            "Approved content removal cannot be undone.".to_string(),
        ] {
            assert!(
                t.lines().any(|l| l.trim_end() == line || l.contains(&line)),
                "{line}\n---\n{t}"
            );
        }
        assert!(t.ends_with("approving P-0123456789ab@1: Remove the code (1 change(s))\nlanded abababababab (P-0123456789ab@1)"), "{t}");
    }

    /// rdx13: the queue item a `rdy approve <proposal>` names: its exact ref, or its stable id
    /// (any prefix of at least 4 hex digits) at its latest version; a version hash names none.
    #[test]
    fn rdx13_the_queue_item_matches_the_named_version() {
        let item = "P-0123456789ab@2";
        for named in [
            "P-0123456789ab@2",
            "P-0123456789ab",
            "P-0123@2",
            "P-0123",
            "P-0123456789abcdef@2",
            "P-0123456789AB@2",
        ] {
            assert!(names_item(named, item), "{named}");
        }
        for named in [
            "P-0123456789ab@1",
            "P-0124",
            "P-012",
            "P-0123456789ab@",
            "0123456789ab",
            "P-xyz1@2",
            "p-0123456789ab@2",
        ] {
            assert!(!names_item(named, item), "{named}");
        }
    }

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
