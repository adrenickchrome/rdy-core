//! rdy_plan: read a change plan, or answer its items (D-4JMPH; reference/mcp-tools p-92F5M,
//! p-CSBA7, p-JJNYH, p-PZX3T). The daemon's `plan` op runs the same function.

use super::*;
use rdy_engine::engine::{Engine, with_writer_result};
use rdy_engine::plan::{PLAN_DEFAULT_TOKENS, PlanAnswerInput, PlanQuery, check_plan_request, page};
use rdy_engine::{E_INTERNAL, RdyError, Session};
use rdy_model::plan::PlanAct;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// What a plan item was done with. The schema mirror of the model's `PlanAct`, which keeps
/// `schemars` out of the model crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanActParam {
    /// The subject still holds as it is.
    Confirm,
    /// Leave the item for later, with a reason.
    Defer,
    /// The work the item names is done.
    Done,
}

impl From<PlanActParam> for PlanAct {
    fn from(a: PlanActParam) -> PlanAct {
        match a {
            PlanActParam::Confirm => PlanAct::Confirm,
            PlanActParam::Defer => PlanAct::Defer,
            PlanActParam::Done => PlanAct::Done,
        }
    }
}

/// One answer to a plan item.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanAnswerParams {
    /// The item: a pi- id (a unique prefix of one is enough) or a cq- consequence id from the last write or proposal.
    pub item: String,
    /// confirm, defer or done. Which acts an item takes depends on its kind.
    pub act: PlanActParam,
    /// Why (1 to 2000 characters). Required for confirm and defer; done keeps none.
    pub reason: Option<String>,
}

impl From<PlanAnswerParams> for PlanAnswerInput {
    fn from(a: PlanAnswerParams) -> PlanAnswerInput {
        PlanAnswerInput {
            item: a.item,
            act: a.act.into(),
            reason: a.reason,
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanParams {
    /// Read the plan of this view (default: the session's view). The only selector that goes with answers.
    pub view: Option<String>,
    /// Read the plan of this proposal version, P-<id>@<n>.
    pub proposal: Option<String>,
    /// Read the plan a landing sealed: its full hash.
    pub landing: Option<String>,
    /// Read every seal that names this decision.
    pub decision: Option<String>,
    /// Answer items of the view's plan. All the answers are checked first, and either all are recorded or none. The reply is the plan after them. Empty: a read.
    pub answers: Option<Vec<PlanAnswerParams>>,
    /// The budget for the reply; the items are cut to fit.
    pub max_tokens: Option<usize>,
    /// Continue a cut plan: its next cursor. Alone (max_tokens allowed).
    pub page: Option<String>,
}

pub fn rdy_plan(st: &mut McpState, p: PlanParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    run_plan(&*st.engine, &s, &st.view, p)
}

/// Runs a plan request as `session`; `default_view` is the view none is given for. The tokens,
/// the selector and page exclusions and the answer count are checked before anything is
/// written. A request with answers records them and reads the plan they leave in one writer
/// closure, then cuts that reply to the budget; one without is a read and writes nothing.
pub fn run_plan(
    engine: &dyn Engine,
    session: &Session,
    default_view: &str,
    p: PlanParams,
) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let answers: Vec<PlanAnswerInput> = p
        .answers
        .unwrap_or_default()
        .into_iter()
        .map(PlanAnswerInput::from)
        .collect();
    let view = p
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v));
    let q = PlanQuery {
        view,
        proposal: p.proposal,
        landing: p.landing,
        decision: p.decision,
        page: p.page,
        max_tokens: p.max_tokens,
    };
    check_plan_request(&q, &answers)?;
    if answers.is_empty() {
        return serialize(&engine.read().change_plan(&q, default_view)?);
    }
    let view = q.view.unwrap_or_else(|| default_view.to_string());
    let s = session.clone();
    let full = with_writer_result(engine, move |r| {
        r.answer_plan(&s, &view, &answers)?;
        r.read().view_plan(&view)
    })?;
    serialize(&page::first(
        full,
        q.max_tokens.unwrap_or(PLAN_DEFAULT_TOKENS),
    ))
}

fn serialize<T: serde::Serialize>(v: &T) -> Result<Value, RdyError> {
    serde_json::to_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a change plan did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}
