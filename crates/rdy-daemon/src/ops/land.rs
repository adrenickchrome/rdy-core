//! The landing ops: propose, approve and tag.
use super::*;
use rdy_engine::consequence::{land_and_report, propose_and_report};
use rdy_engine::engine::write_typed;
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
}

pub(super) fn propose(cx: &OpContext, a: ProposeArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    propose_and_report(cx.engine, a.max_tokens, move |r| {
        r.propose_with(
            &s,
            &view,
            &a.title,
            None,
            a.summary.as_deref(),
            a.alternatives.as_ref(),
        )
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
    let mut v = land_and_report(cx.engine, move |r| r.land_with(&s, &a.proposal, opts))?;
    if let Some(by) = recorded {
        v["relayed_by"] = json!(by);
    }
    Ok(v)
}

pub(super) fn tag(cx: &OpContext, a: TagArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    write_typed(cx.engine, move |r| {
        r.tag(&s, &a.version, a.landing.as_deref(), a.message)
    })
}
