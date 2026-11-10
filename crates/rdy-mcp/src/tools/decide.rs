//! rdy_decide, rdy_supersede and rdy_revoke: the decision tools.

use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideParams {
    pub view: Option<String>,
    /// The doc, section or folder to record the decision in (id, key or path). Omit parent to record it in the decision home, or at the project root when the project names none.
    pub parent: Option<String>,
    pub statement: String,
    /// The card's plain-words title: what a person is asked to confirm, in 80 characters or fewer. A decision with no title gets a W_CARD_WORDS warning.
    pub title: Option<String>,
    /// The card's one line on why a person must answer, in 160 characters or fewer.
    pub why: Option<String>,
    /// What you recommend the person does with this decision: confirm or reject. Give recommend_reason with it.
    pub recommend: Option<String>,
    /// The reason for the recommendation, in one line of 200 characters or fewer.
    pub recommend_reason: Option<String>,
    pub options: Option<Vec<String>>,
    pub authority_basis: Option<String>,
    pub attested_by: Option<String>,
    pub reversibility: Option<String>,
    pub kind: Option<String>,
    pub review_by: Option<String>,
    pub body: Option<String>,
    pub affects: Option<Vec<String>>,
    pub depends_on: Option<Vec<String>>,
    pub conditional_on: Option<Vec<String>>,
    pub refines: Option<Vec<String>>,
    pub answers: Option<Vec<String>>,
    /// The cited decisions, cited instructions or external decisions this decision departs from (`<alias>:<id>`, `<alias>:<KEY>` or the stored `<repo>:<id>`). A person confirms a deviation, and it covers the local blocks that cite its target until it is revoked. A target that is not cited or external is refused (E_VALIDATION).
    pub deviates_from: Option<Vec<String>>,
    /// Pack-defined decision fields, e.g. {"key": "FW-E3", "disposition": "built in M2c3"}: rdy_schema decision lists them; validated against the pack as rdy_write validates fields. Not statement, title, why, recommend, recommend_reason, options or the other fields with their own parameter, nor status, decided_at or recorded_at.
    pub fields: Option<serde_json::Map<String, Value>>,
    /// When the decision takes effect (RFC 3339); defaults to when it is recorded.
    pub effective_from: Option<String>,
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
    /// The intent (rdy_intent, in-<hex>) this change is made under: recorded on the change.
    pub intent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SupersedeParams {
    pub old: String,
    /// The words of the old decision that this one retires; omit to supersede all of it. Goes with decision, not with adopted_as.
    pub part: Option<String>,
    /// The new decision. It inherits the old decision's affects, depends_on, conditional_on and refines unless you give that kind; an empty list drops it. Give decision or adopted_as, not both.
    pub decision: Option<DecideParams>,
    /// A cited decision (`<alias>:<id>` or `<alias>:<KEY>`) that is the upstream copy of the old decision: the old decision is superseded by it, and nothing is copied. The old decision must be accepted and local. A person approves it at landing. Give decision or adopted_as, not both.
    pub adopted_as: Option<String>,
    /// The intent (rdy_intent, in-<hex>) this change is made under; used when decision has none.
    pub intent: Option<String>,
    /// With adopted_as only: the view to write in (default: your session's). With decision, give view inside decision.
    pub view: Option<String>,
    /// With adopted_as only: the briefing token from rdy_brief. With decision, give brief inside decision.
    pub brief: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeParams {
    pub view: Option<String>,
    pub target: String,
    pub reason: String,
    /// The doc, section or folder to record the revoking decision in (id, key or path). Omit parent to record it in the decision home, or at the project root when the project names none.
    pub parent: Option<String>,
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
    /// The intent (rdy_intent, in-<hex>) this change is made under: recorded on the change.
    pub intent: Option<String>,
}

fn decision_input(p: DecideParams) -> DecisionInput {
    // sup1: a kind given as an empty list is not inherited by rdy_supersede
    let no_inherit = [
        ("affects", &p.affects),
        ("depends_on", &p.depends_on),
        ("conditional_on", &p.conditional_on),
        ("refines", &p.refines),
    ]
    .into_iter()
    .filter(|(_, v)| v.as_ref().is_some_and(Vec::is_empty))
    .map(|(k, _)| k.to_string())
    .collect();
    DecisionInput {
        // a placeholder: the tools overwrite it with the resolved parent (given, else the home,
        // else empty for the project root)
        parent: p.parent.unwrap_or_default(),
        statement: p.statement,
        title: p.title,
        why: p.why,
        recommend: p.recommend,
        recommend_reason: p.recommend_reason,
        options: p.options.unwrap_or_default(),
        authority_basis: p.authority_basis,
        attested_by: p.attested_by,
        reversibility: p.reversibility,
        kind: p.kind,
        review_by: p.review_by,
        effective_from: p.effective_from,
        body: p.body.unwrap_or_default(),
        affects: p.affects.unwrap_or_default(),
        depends_on: p.depends_on.unwrap_or_default(),
        conditional_on: p.conditional_on.unwrap_or_default(),
        refines: p.refines.unwrap_or_default(),
        answers: p.answers.unwrap_or_default(),
        deviates_from: p.deviates_from.unwrap_or_default(),
        no_inherit,
        brief: p.brief,
        fields: p
            .fields
            .map(|m| m.into_iter().collect())
            .unwrap_or_default(),
        intent: p.intent,
    }
}

pub fn rdy_decide(st: &mut McpState, p: DecideParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p.view.clone().unwrap_or_else(|| st.view.clone());
    let max_tokens = p.max_tokens;
    let parent = p.parent.clone();
    let mut input = decision_input(p);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        input.parent = r
            .decision_parent(&view, parent.as_deref())?
            .unwrap_or_default();
        r.decide(&s, &view, input)
    })
    .map(|v| report_once(st, v))
}

pub fn rdy_supersede(st: &mut McpState, p: SupersedeParams) -> Result<Value, RdyError> {
    match (p.decision, p.adopted_as) {
        (Some(_), Some(_)) => Err(RdyError::new(
            E_BAD_INPUT,
            "give decision or adopted_as, not both",
            "pass decision to supersede with a new decision, or adopted_as to adopt the cited upstream copy",
        )),
        (None, None) => Err(RdyError::new(
            E_BAD_INPUT,
            "give decision or adopted_as",
            "pass decision, the new decision, or adopted_as, the cited decision that is the upstream copy",
        )),
        (Some(decision), None) => {
            if p.view.is_some() || p.brief.is_some() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "view and brief go inside decision when decision is given",
                    "move them into decision, or use adopted_as",
                ));
            }
            supersede_with(st, p.old, p.part, decision, p.intent)
        }
        (None, Some(adopted_as)) => {
            if p.part.is_some() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "part goes with decision, not with adopted_as",
                    "leave part out: an adopted copy supersedes the whole decision",
                ));
            }
            supersede_adopted(st, p.old, adopted_as, p.view, p.brief, p.intent)
        }
    }
}

fn supersede_with(
    st: &mut McpState,
    old: String,
    part: Option<String>,
    decision: DecideParams,
    intent: Option<String>,
) -> Result<Value, RdyError> {
    check_tokens(decision.max_tokens)?;
    let s = session(st)?;
    let view = decision.view.clone().unwrap_or_else(|| st.view.clone());
    let max_tokens = decision.max_tokens;
    let parent = decision.parent.clone();
    let mut input = decision_input(decision);
    input.intent = input.intent.or(intent);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        input.parent = r
            .decision_parent(&view, parent.as_deref())?
            .unwrap_or_default();
        r.supersede(&s, &view, &old, part, input)
    })
    .map(|v| report_once(st, v))
}

fn supersede_adopted(
    st: &mut McpState,
    old: String,
    adopted_as: String,
    view: Option<String>,
    brief: Option<String>,
    intent: Option<String>,
) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = view.unwrap_or_else(|| st.view.clone());
    let v = view.clone();
    write_and_report(&*st.engine, &v, None, move |r| {
        r.supersede_adopted_as(&s, &view, &old, &adopted_as, brief, intent)
    })
    .map(|v| report_once(st, v))
}

pub fn rdy_revoke(st: &mut McpState, p: RevokeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let (brief, max_tokens, intent, parent) = (p.brief, p.max_tokens, p.intent, p.parent.clone());
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        let pid = r
            .decision_parent(&view, parent.as_deref())?
            .unwrap_or_default();
        r.revoke_with_intent(&s, &view, &p.target, &p.reason, &pid, brief, intent)
    })
    .map(|v| report_once(st, v))
}
