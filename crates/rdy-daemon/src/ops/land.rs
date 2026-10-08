//! The landing ops: propose, approve and tag.
use super::*;
use rdy_engine::consequence::{land_and_report, propose_and_report};
use rdy_engine::engine::write_typed;
use rdy_engine::{E_BAD_INPUT, LandOptions, RdyError, Relay, alt};
use rdy_model::{ActorRun, RelayEvidence};
use serde::Deserialize;
use serde_json::Value;

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
    let s = approver(cx.session)?;
    let bad = |message: &str, fix: &str| RdyError::new(E_BAD_INPUT, message, fix);
    let relay = match (a.relayed_by, a.evidence) {
        (Some(by), Some(quoted)) => {
            let (actor, run) = match by.split_once('/') {
                Some((actor, run)) => (actor.to_string(), Some(run.to_string())),
                None => (by, None),
            };
            Some(Relay {
                by: ActorRun { actor, run },
                evidence: RelayEvidence {
                    quoted,
                    session_ref: a.session_ref.unwrap_or_else(|| "cli".into()),
                    question: a.question.filter(|q| !q.trim().is_empty()),
                },
            })
        }
        (None, None) => {
            if a.session_ref.is_some() {
                return Err(bad(
                    "session_ref needs relayed_by",
                    "pass relayed_by and evidence with it, or drop session_ref",
                ));
            }
            if a.question.is_some() {
                return Err(bad(
                    "question needs relayed_by",
                    "pass relayed_by and evidence with it, or drop question",
                ));
            }
            None
        }
        _ => {
            return Err(bad(
                "relayed_by and evidence go together",
                "pass both (--relayed-by and --evidence) or neither",
            ));
        }
    };
    let opts = LandOptions {
        strict: a.strict.unwrap_or(false),
        relay,
        evidence: None,
    };
    land_and_report(cx.engine, move |r| r.land_with(&s, &a.proposal, opts))
}

pub(super) fn tag(cx: &OpContext, a: TagArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    write_typed(cx.engine, move |r| {
        r.tag(&s, &a.version, a.landing.as_deref(), a.message)
    })
}
