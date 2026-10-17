use crate::brief_text;
use crate::conn::{Conn, Env, call, human_session};
use crate::output::{hash_short, new_keys_text, num, text};
use rdy_daemon::ErrorReply;
use rdy_engine::{E_BAD_INPUT, E_INTERNAL, is_proposal_ref};
use rdy_model::Hash;
use serde_json::{Value, json};
use std::io::Write;

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
/// A person approving, or an agent relaying a person, first reads the version the argument
/// names with the shared resolver (`show`, which takes the approval's own spellings: a full
/// hash, `P-<id>@<n>` or `P-<id>`), with that version's removal card when it carries content
/// removal (Task 8's card, read for that version alone: D-CC90E, D-6B1W1). The card is shown
/// whole before the approval op runs: as text on stdout at once, so a refused or interrupted
/// approval still showed it; as JSON in the reply (`removal_card`), or in the error when the
/// approval is refused. The approval then names that exact version, so a version proposed after
/// the card was read moves it (`E_PROPOSAL_MOVED`) instead of landing unseen. An agent's own
/// approval is refused as before, without the read.
pub(crate) fn approve(
    env: &Env,
    a: ApproveArgs,
    as_json: bool,
) -> Result<(Value, String), ErrorReply> {
    let reads_card = human_session(env) || a.relayed_by.is_some();
    let mut c = Conn::open(env)?;
    let out = approve_on(&mut |op, args| c.op(op, args), reads_card, a, &mut |t| {
        if !as_json {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(t.as_bytes());
            let _ = out.flush();
        }
    });
    c.close();
    out
}

/// Runs one op on the approval's session.
type OpFn<'a> = dyn FnMut(&str, Value) -> Result<Value, ErrorReply> + 'a;

/// The approval on one session: `op` runs an op, `emit` shows text to the person at once. With
/// `reads_card`, the version is read and bound first ([`approve`]); the text returned is the
/// landing lines alone, after the card `emit` showed.
fn approve_on(
    op: &mut OpFn<'_>,
    reads_card: bool,
    a: ApproveArgs,
    emit: &mut dyn FnMut(&str),
) -> Result<(Value, String), ErrorReply> {
    let ApproveArgs {
        proposal,
        relayed_by,
        evidence,
        question,
        session_ref,
        strict,
    } = a;
    let bound = if reads_card && binds(&proposal, relayed_by.is_some()) {
        Some(bound_version(op, &proposal)?)
    } else {
        None
    };
    let card = bound.as_ref().and_then(|b| b.card.clone());
    if let Some(text) = card.as_ref().and_then(|c| c["card"].as_str()) {
        emit(&format!("{text}\n\n"));
    }
    let args = json!({
        "proposal": bound.as_ref().map_or(proposal.as_str(), |b| b.version.as_str()),
        "relayed_by": relayed_by,
        "evidence": evidence,
        "question": question,
        "session_ref": session_ref,
        "strict": strict.then_some(true),
    });
    match op("approve", args) {
        Ok(mut v) => {
            let t = approve_text(&v);
            if let Some(card) = card {
                v["removal_card"] = card;
            }
            Ok((v, t))
        }
        Err(mut e) => {
            if let Some(card) = card {
                e.extra.insert("removal_card".into(), card);
            }
            Err(e)
        }
    }
}

/// Whether the approval of `proposal` reads and binds its version first: when the text has a
/// proposal's shape (`P-<id>[@<n>]` or a full hash). Any other text never resolves, so the
/// approval refuses it as before. A relay must name the exact version: an unversioned relay goes
/// to the approval unchanged, which refuses it before anything is written.
fn binds(proposal: &str, relayed: bool) -> bool {
    let hash = proposal.parse::<Hash>().is_ok();
    let shaped = is_proposal_ref(proposal);
    hash || (shaped && (proposal.contains('@') || !relayed))
}

/// The version an approval binds: its full hash, and its removal card (`ref`, `card`,
/// `card_hash`, `removal`) when it carries one.
struct Bound {
    version: String,
    card: Option<Value>,
}

/// Reads the version `proposal` names (`show`, the shared resolver) and its removal card. A
/// version whose card cannot be read is refused with that error: the approval never runs
/// without the card it must show.
fn bound_version(op: &mut OpFn<'_>, proposal: &str) -> Result<Bound, ErrorReply> {
    let v = op("show", json!({"id": proposal}))?;
    let p = &v["proposal"];
    if let Some(e) = p.get("removal_card_error") {
        return Err(ErrorReply::new(
            text(&e["code"]),
            text(&e["message"]),
            text(&e["fix"]),
        ));
    }
    let version = text(&p["id"]);
    if version.parse::<Hash>().is_err() {
        return Err(ErrorReply::new(
            E_INTERNAL,
            format!("the show of {proposal} names no version hash"),
            "report this as a bug in rdy",
        ));
    }
    Ok(Bound {
        version: version.to_string(),
        card: p.get("removal_card").cloned(),
    })
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

    /// What a person reads on stdout for reply `v`: the removal card shown before the approval, when
    /// there is one, then the landing lines ([`approve_text`]).
    fn approve_text_with(v: &Value, card: Option<&Value>) -> String {
        let lines = approve_text(v);
        match card.and_then(|c| c["card"].as_str()) {
            Some(card) => format!("{card}\n\n{lines}"),
            None => lines,
        }
    }

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

    /// `rdy approve <proposal>` args for a person.
    fn args(proposal: &str, strict: bool) -> ApproveArgs {
        ApproveArgs {
            proposal: proposal.into(),
            relayed_by: None,
            evidence: None,
            question: None,
            session_ref: None,
            strict,
        }
    }

    /// rdx13 fix round 1 (findings 2 and 4): the whole removal card is shown before the approval
    /// op runs, and a refused approval keeps it (the error carries it as `removal_card`). The op
    /// here observes what was shown at the moment it runs: the mutation boundary. The card is
    /// read for the named version alone: any other op (the queue) panics.
    #[test]
    fn rdx13_the_card_is_shown_before_the_approval_runs() {
        let (item, v) = removal_item();
        let version = "ab".repeat(32);
        let shown_card = json!({
            "ref": item.r#ref, "card": item.card, "card_hash": item.card_hash,
            "removal": v["removal"],
        });
        let show = json!({"proposal": {
            "ref": item.r#ref, "id": version, "state": "open", "removal_card": shown_card,
        }});
        let mut at_approval: Option<(Vec<String>, Value)> = None;
        let shown = std::cell::RefCell::new(vec![]);
        let err = approve_on(
            &mut |op, a| match op {
                "show" => Ok(show.clone()),
                "approve" => {
                    at_approval = Some((shown.borrow().clone(), a));
                    Err(ErrorReply::new(
                        "E_PROPOSAL_MOVED",
                        "canon moved",
                        "approve without --strict",
                    ))
                }
                other => panic!("unexpected op {other}"),
            },
            true,
            args("P-0123456789ab", true),
            &mut |t| shown.borrow_mut().push(t.to_string()),
        )
        .unwrap_err();
        let (seen, sent) = at_approval.expect("the approval ran");
        assert_eq!(
            seen,
            vec![format!("{}\n\n", item.card)],
            "the card was shown before the op"
        );
        assert_eq!(*shown.borrow(), seen, "and nothing after it");
        assert_eq!(sent["proposal"], version.as_str(), "the exact version");
        assert_eq!(sent["strict"], true);
        assert_eq!(err.code, "E_PROPOSAL_MOVED");
        assert_eq!(err.extra.get("removal_card"), Some(&shown_card));
        // an approval that runs prints the landing lines after the card it showed
        let reply = json!({
            "approved": item.r#ref, "title": "Remove the code", "changes": 1,
            "landing": "cd".repeat(32)
        });
        let shown = std::cell::RefCell::new(vec![]);
        let (v2, t) = approve_on(
            &mut |op, _| match op {
                "show" => Ok(show.clone()),
                "approve" => Ok(reply.clone()),
                other => panic!("unexpected op {other}"),
            },
            true,
            args("P-0123456789ab@1", false),
            &mut |t| shown.borrow_mut().push(t.to_string()),
        )
        .unwrap();
        assert_eq!(t, approve_text(&reply));
        assert_eq!(
            format!("{}{t}", shown.borrow().concat()),
            approve_text_with(&reply, Some(&shown_card))
        );
        assert_eq!(v2["removal_card"], shown_card);
    }

    /// A new project in `dir` whose view `work` holds a doc with one prose block; returns the
    /// block's id.
    fn project_with_prose(dir: &std::path::Path) -> String {
        let mut r = rdy_engine::Rdy::init(dir, "rdy-standard").unwrap();
        let ops = rdy_engine::parse_ops(vec![
            json!({"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}}),
            json!({"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}}),
            json!({"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The code is ZEBRA-7731."}),
        ])
        .unwrap();
        let w = r
            .write(
                &rdy_engine::Session::human("human:qa".into(), rdy_model::Role::Writer),
                rdy_engine::WriteRequest {
                    view: "work".into(),
                    ops,
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        r.close().unwrap();
        w.created["$p"].clone()
    }

    /// rdx13 fix round 1 (finding 3): an approval is bound to the exact version whose card was
    /// shown. Another writer records a second request and proposes version 2 between the card's
    /// read and the approval: `rdy approve P-<id>` (no version) shows version 1's card, approves
    /// version 1 and is refused because it moved; nothing lands.
    #[test]
    fn rdx13_an_approval_binds_the_version_whose_card_was_shown() {
        let dir = tempfile::tempdir().unwrap();
        let id = project_with_prose(dir.path());
        let local = rdy_engine::engine::Local::open(dir.path()).unwrap();
        let session = rdy_engine::Session::human("human:qa".into(), rdy_model::Role::Writer);
        let cwd = dir.path().to_path_buf();
        let cx = rdy_daemon::OpContext {
            engine: &local,
            session: &session,
            human: "human:qa",
            cwd: &cwd,
        };
        let run = |op: &str, a: Value| rdy_daemon::dispatch(&cx, op, a).map_err(ErrorReply::from);
        let redact = |reason: &str| {
            run(
                "redact",
                json!({"targets": [id], "reason_class": "secret", "reason": reason}),
            )
            .unwrap()
        };
        let propose = || {
            run(
                "propose",
                json!({"title": "Remove", "view": "work", "summary": "Removes the code."}),
            )
            .unwrap()
        };
        redact("first");
        let p1 = propose();
        let stable = p1["ref"]
            .as_str()
            .unwrap()
            .split('@')
            .next()
            .unwrap()
            .to_string();
        use rdy_engine::engine::Engine as _;
        let landings = || local.read().log_page(None, None).unwrap().len();
        let before = landings();
        let shown = std::cell::RefCell::new(vec![]);
        let mut moved = false;
        let err = approve_on(
            &mut |op, a| {
                let out = run(op, a);
                if op != "approve" && !moved {
                    // the other writer, between the card's read and the approval
                    moved = true;
                    redact("second");
                    let p2 = propose();
                    assert!(p2["ref"].as_str().unwrap().ends_with("@2"), "{p2}");
                }
                out
            },
            true,
            args(&stable, false),
            &mut |t| shown.borrow_mut().push(t.to_string()),
        )
        .unwrap_err();
        assert_eq!(err.code, "E_PROPOSAL_MOVED", "{err:?}");
        let shown = shown.borrow().concat();
        assert_eq!(
            shown.matches("  Request: ").count(),
            1,
            "version 1's card: {shown}"
        );
        assert!(shown.contains("  Reason: first"), "{shown}");
        assert!(!shown.contains("  Reason: second"), "{shown}");
        assert_eq!(err.extra["removal_card"]["ref"], p1["ref"], "{err:?}");
        assert_eq!(landings(), before, "nothing landed");
        local.close().unwrap();
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
