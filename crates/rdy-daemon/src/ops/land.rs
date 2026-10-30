//! The landing ops: propose, approve, land and tag.
use super::*;
use rdy_engine::consequence::propose_and_report;
use rdy_engine::plan::{PlanAnswerInput, PlanQuery, ProposePlanRequest, check_plan_request};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::{LandOptions, RdyError, Relay, alt};
use rdy_model::RelayEvidence;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProposeArgs {
    title: String,
    view: Option<String>,
    summary: Option<String>,
    max_tokens: Option<usize>,
    alternatives: Option<alt::AltPick>,
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
    plan_answers: Option<Vec<rdy_mcp::tools::PlanAnswerParams>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApproveArgs {
    proposal: String,
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
    strict: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TagArgs {
    version: String,
    landing: Option<String>,
    message: Option<String>,
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
}

/// The relay flags of an op, or None when it names none: the op then runs as today.
fn relay_of(
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
) -> Option<RelayInput> {
    let r = RelayInput {
        relayed_by,
        evidence,
        question,
        session_ref,
        kind: None,
    };
    (r != RelayInput::default()).then_some(r)
}

/// A proposal; with relay flags, the person's proposal of their own view (`propose_as`, O32).
pub(super) fn propose(cx: &OpContext, a: ProposeArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let relay = relay_of(a.relayed_by, a.evidence, a.question, a.session_ref);
    let person = cx.human.to_string();
    let answers: Vec<PlanAnswerInput> = a
        .plan_answers
        .unwrap_or_default()
        .into_iter()
        .map(PlanAnswerInput::from)
        .collect();
    check_plan_request(&PlanQuery::default(), &answers)?;
    propose_and_report(cx.engine, a.max_tokens, move |r| {
        let req = ProposePlanRequest {
            view: &view,
            title: &a.title,
            changes: None,
            summary: a.summary.as_deref(),
            alternatives: a.alternatives.as_ref(),
            plan_answers: &answers,
        };
        match relay {
            None => r.propose_plan(&s, req),
            Some(relay) => r.propose_plan_as(&s, &person, &relay, false, req),
        }
    })
}

pub(super) fn approve(cx: &OpContext, a: ApproveArgs) -> Result<Value, RdyError> {
    // the session kind, then the flags' pairing, then the relaying actor and run, then the
    // quote, question and session_ref limits: the whole relay goes through the table (D-HA7YZ)
    let input = RelayInput {
        relayed_by: a.relayed_by,
        evidence: a.evidence,
        question: a.question,
        session_ref: a.session_ref,
        kind: None,
    };
    let (s, relay) = relay_approver(cx, &input)?;
    // the landing records the words as given and checks them itself (an empty session_ref is
    // refused there, not replaced by the table's default); the relayer is the table's
    let relay = relay.map(|r| Relay {
        by: r.by,
        evidence: RelayEvidence {
            quoted: input.evidence.unwrap_or_default(),
            session_ref: input.session_ref.unwrap_or_else(|| "cli".into()),
            question: input.question.filter(|q| !q.trim().is_empty()),
        },
    });
    // the reply names the relayer as recorded (the session's actor and run), not the flag
    let recorded = relay.as_ref().map(|r| r.by.clone());
    let opts = LandOptions {
        strict: a.strict.unwrap_or(false),
        relay,
        evidence: None,
    };
    // the approval, signed as the signing settings say (D-1Z951): a relayed approval runs as the
    // person, and the asking session (the relaying agent) is who asks for its signatures
    let asking = if opts.relay.is_some() {
        cx.session.clone()
    } else {
        s
    };
    let request = SigningAct::Approve {
        proposal: a.proposal,
        options: opts,
    };
    let mut v = run_signed_act(cx.engine, cx.agent, &asking, cx.human, request)?;
    if let Some(by) = recorded {
        v["relayed_by"] = json!(by);
    }
    Ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LandArgs {
    proposal: String,
}

/// `rdy land` (D-ZWP7Q, O14): an agent session lands a version the authority policy accepts.
/// The request runs through `rdy_engine::run_land`, the runner `rdy_land` shares.
pub(super) fn land(cx: &OpContext, a: LandArgs) -> Result<Value, RdyError> {
    rdy_engine::run_land(cx.engine, cx.session, a.proposal)
}

/// A tag. With no relay flags, as before: a human session tags, an agent session is refused
/// (`AGENT_NOT_APPROVER`, O19). With them, the relayed tag (`tag_as`, D-DGCH7).
pub(super) fn tag(cx: &OpContext, a: TagArgs) -> Result<Value, RdyError> {
    // with no relay flags a human tags as itself and an agent is refused, as before; either way
    // the tag is signed as the signing settings say (D-1Z951)
    let (s, relay) = match relay_of(a.relayed_by, a.evidence, a.question, a.session_ref) {
        None => (approver(cx.session)?, RelayInput::default()),
        Some(relay) => (cx.session.clone(), relay),
    };
    let request = SigningAct::Tag {
        name: a.version,
        landing: a.landing,
        message: a.message,
        relay,
        implicit: false,
    };
    run_signed_act(cx.engine, cx.agent, &s, cx.human, request)
}
