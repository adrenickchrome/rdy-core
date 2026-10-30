//! ap9 (D-ZWP7Q, D-P3YD5, D-X3136, O2, O20): `rdy_policy init` / `rdy policy init`. It starts the
//! authority policy in the session's view: one proposed decision, `Adopt the <pack> authority
//! policy`, and the engine's template block (O2) under the doc, which implements the decision.
//! It then proposes the view, and a person approves it like any proposal.
//!
//! [`run_policy`] is the one request runner of the MCP tool `rdy_policy` and the daemon op
//! `policy`.
use crate::brief::Coverage;
use crate::consequence::propose_and_report;
use crate::did_you_mean::nearest;
use crate::engine::Engine;
use crate::error::*;
use crate::plan::ProposePlanRequest;
use crate::policy::live_policy;
use crate::{DecisionInput, OpInput, ProposalInfo, Rdy, Scope, Session, WriteRequest};
use rdy_model::tree::DOC;
use rdy_model::{BlockId, FieldValue, Justification};
use rdy_schema::policy::{POLICY_TYPE, template_fields};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The ops `rdy_policy` takes.
const OPS: [&str; 1] = ["init"];

/// O20: `doc` is absent, or names a block that is not a doc.
const NAME_DOC: &str = "name the doc that holds the authority policy";
const NAME_DOC_FIX: &str = "pass doc: rdy_policy {op: \"init\", doc} / rdy policy init --doc <doc>";
/// O20: the project already has a policy.
const HAS_POLICY_FIX: &str = "change it with rdy_write / rdy edit, with implements a decision";

/// What `init` did: the decision and the policy block it wrote, and the proposal of the view.
#[derive(Clone, Debug, Serialize)]
pub struct PolicyInitInfo {
    pub decision: String,
    pub policy: String,
    pub proposal: ProposalInfo,
}

/// One `rdy_policy` / `policy` request. `op` takes only `init`. Unknown keys are refused
/// (`did_you_mean` names a near key).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    pub op: String,
    /// init: the doc that holds the policy block (a path, a key or an id).
    pub doc: Option<String>,
    /// init: the view to write in and propose (default: the session's own).
    pub view: Option<String>,
}

fn bad(message: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

/// The JSON of a template field value, as a write takes it.
fn json_of(v: &FieldValue) -> Value {
    match v {
        FieldValue::Null => Value::Null,
        FieldValue::Bool(b) => json!(b),
        FieldValue::Int(i) => json!(i),
        FieldValue::Str(s) => json!(s),
        FieldValue::List(items) => Value::Array(items.iter().map(json_of).collect()),
        FieldValue::Map(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), json_of(v))).collect())
        }
    }
}

impl Rdy {
    /// O20: starts the authority policy in `view` (default: the session's own) under `doc`.
    ///
    /// Refused with `E_BAD_INPUT` when `doc` is absent or is not a doc, and when the project has
    /// a policy block in canon or in the view. Otherwise it writes, in the view, one decision with
    /// no basis and no parent beyond the one `decision_parent` places it under (D-X3136, D-P3YD5),
    /// then the template block as the only block under `doc`, with `implements` the decision,
    /// each briefed on the blocks it touches as `assign_keys` briefs its write, and proposes the
    /// view with a summary.
    pub fn policy_init(
        &mut self,
        s: &Session,
        doc: Option<&str>,
        view: Option<&str>,
    ) -> Result<PolicyInitInfo, RdyError> {
        let view = view
            .map(|v| v.strip_prefix("view:").unwrap_or(v).to_string())
            .unwrap_or_else(|| s.default_view());
        let scope = Scope::View(view.clone());
        let (doc_id, pack) = {
            let st = self.read();
            let text = doc
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .ok_or_else(|| bad(NAME_DOC, NAME_DOC_FIX))?;
            let id = st.resolve_id(&scope, text)?;
            if st.block(&scope, &id)?.block_type != DOC {
                return Err(bad(NAME_DOC, NAME_DOC_FIX));
            }
            let canon = st.materialized(&Scope::Canon)?;
            let held = match live_policy(&st, &canon) {
                Some(live) => Some(live.block),
                None => st
                    .materialized(&scope)?
                    .iter()
                    .find(|b| b.block_type == POLICY_TYPE)
                    .map(|b| b.id.clone()),
            };
            if let Some(ap) = held {
                return Err(bad(
                    format!("the project has an authority policy: {ap}"),
                    HAS_POLICY_FIX,
                ));
            }
            (id, st.pack().name.clone())
        };
        let title = format!("Adopt the {pack} authority policy");
        // the decision: no basis, `proposed`, placed as `decision_parent` places any decision
        let parent = self.decision_parent(&view, None)?;
        let brief = match &parent {
            Some(p) => Some(self.self_brief(&scope, p)?),
            None => None,
        };
        let written = self.decide(
            s,
            &view,
            DecisionInput {
                parent: parent.unwrap_or_default(),
                statement: title.clone(),
                title: Some(title.clone()),
                why: Some(
                    "The policy says what an agent may land without a person, and who approves the rest."
                        .into(),
                ),
                reversibility: Some("two_way".into()),
                brief,
                ..DecisionInput::default()
            },
        )?;
        let decision = written
            .created
            .get("$decision")
            .cloned()
            .expect("a decision write reports the decision it created");
        // the policy block: the engine's template (O2), the only block under `doc`
        let fields: BTreeMap<String, Value> = template_fields()
            .iter()
            .map(|(k, v)| (k.clone(), json_of(v)))
            .collect();
        let brief = self.self_brief(&scope, &doc_id.to_string())?;
        let decision_id = BlockId::parse(&decision).expect("rdy wrote the decision id");
        let wrote = self.write_briefed(
            s,
            WriteRequest {
                view: view.clone(),
                ops: vec![OpInput::Create {
                    block_type: POLICY_TYPE.into(),
                    id: Some("$policy".into()),
                    parent: Some(doc_id.to_string()),
                    after: None,
                    before: None,
                    fields,
                    body: String::new(),
                    links: BTreeMap::new(),
                }],
                justification: Some(Justification::Implements(vec![decision_id])),
                message: format!("adopt the {pack} authority policy"),
            },
            Some(brief),
        )?;
        let policy = wrote
            .created
            .get("$policy")
            .cloned()
            .expect("a policy write reports the block it created");
        let summary = format!(
            "Adopts the {pack} authority policy. It accepts nothing by itself until a person adds a rule."
        );
        let proposal = self.propose_plan(
            s,
            ProposePlanRequest {
                summary: Some(&summary),
                ..ProposePlanRequest::new(&view, &title)
            },
        )?;
        Ok(PolicyInitInfo {
            decision,
            policy,
            proposal,
        })
    }

    /// The token of a briefing of block `id` in `scope`, registered as `assign_keys` registers
    /// its own: the write that follows cites it, so a stable parent needs no briefing from the
    /// caller.
    fn self_brief(&self, scope: &Scope, id: &str) -> Result<String, RdyError> {
        let brief = self.read().brief(
            scope,
            &Coverage {
                ids: vec![id.to_string()],
                path: None,
            },
            None,
        )?;
        brief.register(&self.briefs);
        Ok(brief.base.token.clone())
    }
}

/// Runs `req` for session `s`: the reply is `{decision, policy, proposal}`, with `proposal` the
/// `rdy_propose` reply (consequences and coverage included).
pub fn run_policy(engine: &dyn Engine, s: &Session, req: PolicyRequest) -> Result<Value, RdyError> {
    if req.op != "init" {
        let fix = match nearest(&req.op, &OPS) {
            Some(near) => format!("did you mean \"{near}\"? the op is {}", OPS.join(", ")),
            None => format!("use {}", OPS.join(", ")),
        };
        return Err(bad(format!("unknown policy op {:?}", req.op), fix));
    }
    let s = s.clone();
    let ids: Arc<Mutex<Option<(String, String)>>> = Arc::default();
    let seen = Arc::clone(&ids);
    let proposal = propose_and_report(engine, None, move |r| {
        let info = r.policy_init(&s, req.doc.as_deref(), req.view.as_deref())?;
        *seen.lock().expect("no other holder panics") = Some((info.decision, info.policy));
        Ok(info.proposal)
    })?;
    let (decision, policy) = ids
        .lock()
        .expect("no other holder panics")
        .take()
        .expect("a successful init reports its ids");
    Ok(json!({"decision": decision, "policy": policy, "proposal": proposal}))
}
