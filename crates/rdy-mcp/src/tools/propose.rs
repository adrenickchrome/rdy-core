//! rdy_propose: a proposal for a change.

use super::*;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::consequence::propose_and_report;
use rdy_engine::plan::{PlanAnswerInput, PlanQuery, ProposePlanRequest, check_plan_request};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeParams {
    pub view: Option<String>,
    pub title: String,
    /// Propose only these changes (ids from rdy_view show). The set must be closed under dependencies, or it is E_DEPS_MISSING. Omit to propose all the view's changes. A new version of an open proposal replaces the old one.
    pub changes: Option<Vec<String>>,
    /// Your plain-words summary of the proposal, for the person who approves it. Stored with the version; a new version without it keeps the last one.
    pub summary: Option<String>,
    /// The budget for the consequences in the result.
    pub max_tokens: Option<usize>,
    /// A proposal from a member view of an alternative set (rdy_alternatives) names the set and the view's own member: {set, chosen}. Landing it chooses that member and closes the others.
    pub alternatives: Option<crate::alt_tools::AltPickParams>,
    /// Propose the person's own view (one they own, or "work") on their word: {evidence, question?, session_ref?}. The proposal records the person, and an answer records your relay.
    pub relay: Option<RelayParams>,
    /// Answers to the items of the change plan, recorded in the same batch as the proposal: [{item, act, reason?}], as rdy_plan takes them. All of them are checked against the version first, and either the proposal and every answer are recorded or nothing is. The reply carries the plan's counts and items.
    pub plan_answers: Option<Vec<PlanAnswerParams>>,
}

/// The person's words that ask for a proposal of their view.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelayParams {
    /// The person's words, quoted (1 to 2000 characters).
    pub evidence: String,
    /// The question you asked the person.
    pub question: Option<String>,
    /// The chat session the person spoke in (default: mcp).
    pub session_ref: Option<String>,
}

pub fn rdy_propose(st: &mut McpState, p: ProposeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v))
        .unwrap_or_else(|| st.view.clone());
    let changes = p.changes.clone();
    let alternatives = p.alternatives.map(rdy_engine::alt::AltPick::from);
    // with a relay, the person's proposal of their own view (O32); the session is the relay
    let root = st.engine.root();
    let relay = p.relay.map(|r| {
        let input = RelayInput {
            relayed_by: None,
            evidence: Some(r.evidence),
            question: r.question,
            session_ref: r.session_ref,
            kind: None,
        };
        (input, human_actor_for_env(&root))
    });
    let answers: Vec<PlanAnswerInput> = p
        .plan_answers
        .unwrap_or_default()
        .into_iter()
        .map(PlanAnswerInput::from)
        .collect();
    check_plan_request(&PlanQuery::default(), &answers)?;
    propose_and_report(&*st.engine, p.max_tokens, move |r| {
        let req = ProposePlanRequest {
            view: &view,
            title: &p.title,
            changes: changes.as_deref(),
            summary: p.summary.as_deref(),
            alternatives: alternatives.as_ref(),
            plan_answers: &answers,
        };
        match relay {
            Some((relay, person)) => r.propose_plan_as(&s, &person, &relay, true, req),
            None => r.propose_plan(&s, req),
        }
    })
}
