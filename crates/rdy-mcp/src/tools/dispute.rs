//! rdy_dispute (D-ZWP7Q "Disputes"): a person opens, resolves and lists disputes of landings.
//! Open and resolve are a person's acts: the session is the relay of the person's words, as
//! `rdy_view own` (no `relayed_by`). The request runs through `rdy_engine::dispute::run_dispute`,
//! the runner the daemon's `dispute` op shares.
use super::*;
use rdy_engine::dispute::{DisputeRequest, run_dispute};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisputeParams {
    /// open, resolve or list. open and resolve are a person's acts: relay their words with evidence.
    pub op: String,
    /// open: the landing to dispute; list: only this landing's disputes (a hash or a prefix of 6 or more).
    pub landing: Option<String>,
    /// open: why the landing is wrong, 1 to 2000 characters.
    pub reason: Option<String>,
    /// resolve: the dispute (its id or a prefix of 6 or more).
    pub dispute: Option<String>,
    /// resolve: upheld, dismissed or withdrawn. Only the person who opened the dispute withdraws it.
    pub outcome: Option<String>,
    /// resolve, upheld only: the landing that corrects the disputed one.
    pub correction: Option<String>,
    /// list: only the open disputes (default false).
    pub open: Option<bool>,
    /// open and resolve: the person's words, quoted. Without them an agent session is refused (E_PERMISSION).
    pub evidence: Option<String>,
    /// open and resolve: the question the person answered.
    pub question: Option<String>,
    /// open and resolve: the chat session the person spoke in (default mcp).
    pub session_ref: Option<String>,
}

/// `rdy_dispute`: runs the request for the session; the session relays the person's words.
pub fn rdy_dispute(st: &mut McpState, p: DisputeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root())?;
    let req = DisputeRequest {
        op: p.op,
        landing: p.landing,
        dispute: p.dispute,
        reason: p.reason,
        outcome: p.outcome,
        correction: p.correction,
        open: p.open,
        relayed_by: None,
        evidence: p.evidence,
        question: p.question,
        session_ref: p.session_ref,
    };
    run_dispute(&*st.engine, &s, &person, req)
}
