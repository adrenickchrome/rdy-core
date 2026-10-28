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
    /// The words of the old decision that this one retires; omit to supersede all of it.
    pub part: Option<String>,
    /// The new decision. It inherits the old decision's affects, depends_on, conditional_on and refines unless you give that kind; an empty list drops it.
    pub decision: DecideParams,
    /// The intent (rdy_intent, in-<hex>) this change is made under; used when decision has none.
    pub intent: Option<String>,
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
    check_tokens(p.decision.max_tokens)?;
    let s = session(st)?;
    let view = p.decision.view.clone().unwrap_or_else(|| st.view.clone());
    let max_tokens = p.decision.max_tokens;
    let parent = p.decision.parent.clone();
    let mut input = decision_input(p.decision);
    input.intent = input.intent.or(p.intent);
    let (old, part) = (p.old, p.part);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        input.parent = r
            .decision_parent(&view, parent.as_deref())?
            .unwrap_or_default();
        r.supersede(&s, &view, &old, part, input)
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
