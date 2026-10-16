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
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
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
    propose_and_report(cx.engine, a.max_tokens, move |r| match relay {
        None => r.propose_with(
            &s,
            &view,
            &a.title,
            None,
            a.summary.as_deref(),
            a.alternatives.as_ref(),
        ),
        Some(relay) => r.propose_as(
            &s,
            &person,
            &relay,
            false,
            &view,
            &a.title,
            None,
            a.summary.as_deref(),
            a.alternatives.as_ref(),
        ),
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

/// A tag. With no relay flags, as before: a human session tags, an agent session is refused
/// (`AGENT_NOT_APPROVER`, O19). With them, the relayed tag (`tag_as`, D-DGCH7).
pub(super) fn tag(cx: &OpContext, a: TagArgs) -> Result<Value, RdyError> {
    let Some(relay) = relay_of(a.relayed_by, a.evidence, a.question, a.session_ref) else {
        let s = approver(cx.session)?;
        return write_typed(cx.engine, move |r| {
            r.tag(&s, &a.version, a.landing.as_deref(), a.message)
        });
    };
    let s = cx.session.clone();
    let person = cx.human.to_string();
    write_typed(cx.engine, move |r| {
        r.tag_as(
            &s,
            &person,
            &relay,
            false,
            &a.version,
            a.landing.as_deref(),
            a.message,
        )
    })
}
