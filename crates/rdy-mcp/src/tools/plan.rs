//! rdy_plan: read a change plan, or answer its items (D-4JMPH; reference/mcp-tools p-92F5M,
//! p-CSBA7, p-JJNYH, p-PZX3T). The daemon's `plan` op runs the same function.

use super::*;
use rdy_engine::engine::{Engine, with_writer_result};
use rdy_engine::plan::api::PlanAnswerSpec;
use rdy_engine::plan::group::PlanGroup;
use rdy_engine::plan::{PLAN_DEFAULT_TOKENS, PlanAnswerInput, PlanQuery, check_plan_request, page};
use rdy_engine::{E_INTERNAL, RdyError, Session};
use rdy_model::plan::{PlanAct, PlanKind};
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
    /// A tension item does not apply. Only a tension item takes it.
    Dismiss,
}

impl From<PlanActParam> for PlanAct {
    fn from(a: PlanActParam) -> PlanAct {
        match a {
            PlanActParam::Confirm => PlanAct::Confirm,
            PlanActParam::Defer => PlanAct::Defer,
            PlanActParam::Done => PlanAct::Done,
            PlanActParam::Dismiss => PlanAct::Dismiss,
        }
    }
}

/// One answer to a plan item.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanAnswerParams {
    /// One item: a pi- id (a unique prefix of one is enough), a tn- id of a tension item or a cq- consequence id from the last write or proposal.
    pub item: String,
    /// confirm, defer, done or dismiss. Which acts an item takes depends on its kind.
    pub act: PlanActParam,
    /// Why (1 to 2000 characters). Required for confirm, defer and dismiss; done keeps none.
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

/// The kind of a plan item, as a group names it. The schema mirror of the model's `PlanKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanKindParam {
    Update,
    Rejustify,
    Reaffirm,
    Confirm,
    UpdateImplementation,
    Tension,
}

impl From<PlanKindParam> for PlanKind {
    fn from(k: PlanKindParam) -> PlanKind {
        match k {
            PlanKindParam::Update => PlanKind::Update,
            PlanKindParam::Rejustify => PlanKind::Rejustify,
            PlanKindParam::Reaffirm => PlanKind::Reaffirm,
            PlanKindParam::Confirm => PlanKind::Confirm,
            PlanKindParam::UpdateImplementation => PlanKind::UpdateImplementation,
            PlanKindParam::Tension => PlanKind::Tension,
        }
    }
}

/// A group of open plan items: every field it gives must match, and it gives at least one.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanGroupParams {
    /// The item's kind: update, rejustify, reaffirm, confirm, update_implementation or tension.
    pub kind: Option<PlanKindParam>,
    /// update, reaffirm or confirm (a confirm item and a rejustify item).
    pub follow_up: Option<String>,
    /// The id of the doc that holds the item's subject in the version.
    pub doc: Option<String>,
    /// The item's edge.
    pub edge: Option<String>,
}

impl From<PlanGroupParams> for PlanGroup {
    fn from(g: PlanGroupParams) -> PlanGroup {
        PlanGroup {
            kind: g.kind.map(Into::into),
            follow_up: g.follow_up,
            doc: g.doc,
            edge: g.edge,
        }
    }
}

/// One answer in `rdy_plan`: to one item, or to a group of the open items.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanAnswerSpecParams {
    /// One item: a pi- id (a unique prefix of one is enough), a tn- id of a tension item or a cq- consequence id. Give item or group, not both.
    pub item: Option<String>,
    /// Every open item that matches each field the group gives: {kind?, follow_up?, doc?, edge?}. The act answers all of them. Give item or group, not both.
    pub group: Option<PlanGroupParams>,
    /// confirm, defer, done or dismiss. Which acts an item takes depends on its kind.
    pub act: PlanActParam,
    /// Why (1 to 2000 characters). Required for confirm, defer and dismiss; done keeps none.
    pub reason: Option<String>,
    /// true with a confirm group: also repin each matched item's subject to its cause, in one change before the answers.
    pub repin: Option<bool>,
}

impl From<PlanAnswerSpecParams> for PlanAnswerSpec {
    fn from(a: PlanAnswerSpecParams) -> PlanAnswerSpec {
        PlanAnswerSpec {
            item: a.item,
            group: a.group.map(Into::into),
            act: a.act.into(),
            reason: a.reason,
            repin: a.repin.unwrap_or(false),
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
    /// Answer items of the view's plan: each answer names an item, or a group of the open items. All the answers are checked first, and either all are recorded or none. The reply is the plan after them. Empty: a read.
    pub answers: Option<Vec<PlanAnswerSpecParams>>,
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
    let specs: Vec<PlanAnswerSpec> = p
        .answers
        .unwrap_or_default()
        .into_iter()
        .map(PlanAnswerSpec::from)
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
    // the request rules look at the count only
    let counted: Vec<PlanAnswerInput> = specs
        .iter()
        .map(|s| PlanAnswerInput {
            item: String::new(),
            act: s.act,
            reason: None,
        })
        .collect();
    check_plan_request(&q, &counted)?;
    if specs.is_empty() {
        return serialize(&engine.read().change_plan(&q, default_view)?);
    }
    let view = q.view.unwrap_or_else(|| default_view.to_string());
    let s = session.clone();
    let full = with_writer_result(engine, move |r| {
        r.answer_plan_specs(&s, &view, &specs)?;
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
