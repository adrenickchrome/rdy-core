//! mten1: `rdy_tensions` and `rdy_resolve`. The requests are run by
//! `rdy_engine::tensions::api::{run_tensions, run_resolve}`, the dispatchers the daemon's
//! `tensions` and `resolve` ops share; `rdy_schema`'s paragraph is `TENSIONS_DOC`.
use super::*;
use rdy_engine::tensions::api::{
    AcceptRequest, DismissRequest, MentionRequest, ResolveRequest, TensionsRequest, run_resolve,
    run_tensions,
};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TensionsParams {
    /// inflight (default: every tension), canon (only tensions whose sides are all canon) or a
    /// view name (tensions with a change side in that view).
    pub scope: Option<String>,
    /// Only this kind: edit_collision, fork, premise_shift, area_overlap, duplicate_definition,
    /// unjustified_change, deprecated_reference, value_disagreement, restatement_drift or
    /// semantic.
    pub kind: Option<String>,
    /// Only this severity: blocking or advisory.
    pub severity: Option<String>,
    /// open (default) or dismissed.
    pub status: Option<String>,
    /// Only tensions with a change side in this view (the same as a scope that is a view).
    pub view: Option<String>,
    /// Only tensions that involve a block in this area (one of rdy_schema's areas).
    pub area: Option<String>,
    /// Only tensions that involve this block (id, key or path) or this change (a hex prefix of
    /// at least 6 digits).
    pub involving: Option<String>,
    /// Also list every dismissal review: who dismissed which tension, why, and whether the
    /// dismissal still holds.
    pub reviews: Option<bool>,
    /// Cut the answer to about this many tokens (blocking tensions come first).
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveParams {
    /// The tension to dismiss: tn-<12 hex> from rdy_tensions (a unique prefix works). Pass this
    /// or mention.
    pub tension: Option<String>,
    /// A passing mention to dismiss or restore: the block and the entity a suggestion names.
    /// Pass this or tension.
    pub mention: Option<MentionParams>,
    /// Dismiss the tension or the mention, with a reason. A tension is otherwise resolved by a
    /// change.
    pub dismiss: Option<DismissParams>,
    /// Restore a dismissed mention, with a reason. A tension is not restored.
    pub restore: Option<DismissParams>,
    /// Where the mention is checked: canon or a view. Defaults to your view.
    pub scope: Option<String>,
    /// A person's answer to the advice on a proposal's semantic tensions: accept it, relayed with their words. Pass this alone.
    pub accept: Option<AcceptAdviceParams>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptAdviceParams {
    /// The exact proposal version the person was shown: P-x@n.
    pub proposal: String,
    /// The advised_digest rdy_propose or rdy_read P-x@n gave with the advised tensions the person was shown.
    pub advised_digest: String,
    /// Only these advised tensions (tn-…); leave out to accept every advised one of the version.
    pub tensions: Option<Vec<String>>,
    /// The person's words, quoted (needed: your session relays them).
    pub evidence: Option<String>,
    /// The question the person was asked.
    pub question: Option<String>,
    /// Where the person said it, e.g. the chat session (default: mcp).
    pub session_ref: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MentionParams {
    /// The block that names the entity (c-… from a suggestion's block).
    pub block: String,
    /// The entity it names (from a suggestion's target).
    pub entity: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DismissParams {
    /// Why the tension needs no action, in a few words (at most 2000 characters).
    pub reason: String,
    /// The person's words, quoted, when you relay their dismissal of a tension: it then runs as
    /// the person. Leave out to dismiss as yourself.
    pub evidence: Option<String>,
    /// The question the person was asked (needs evidence).
    pub question: Option<String>,
    /// Where the person said it, e.g. the chat session (needs evidence; default: mcp).
    pub session_ref: Option<String>,
}

impl DismissParams {
    /// The engine's request: over MCP the session is the relay, so there is no `relayed_by`.
    fn request(self) -> DismissRequest {
        DismissRequest {
            reason: self.reason,
            evidence: self.evidence,
            question: self.question,
            session_ref: self.session_ref,
            relayed_by: None,
        }
    }
}

/// `rdy_tensions`: lists tensions, filtered, for the session.
pub fn rdy_tensions(st: &mut McpState, p: TensionsParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    run_tensions(
        &*st.engine,
        &s,
        TensionsRequest {
            scope: p.scope,
            kind: p.kind,
            severity: p.severity,
            status: p.status,
            view: p.view,
            area: p.area,
            involving: p.involving,
            reviews: p.reviews,
            max_tokens: p.max_tokens,
        },
    )
}

/// `rdy_resolve`: dismisses a tension, or dismisses or restores a passing mention, with a reason
/// (a review); or relays a person's acceptance of the advice on a proposal's semantic tensions,
/// or their dismissal of a tension (the session is the relay, so there is no `relayed_by`). The
/// person is the project's human actor (O18).
pub fn rdy_resolve(st: &mut McpState, p: ResolveParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root())?;
    run_resolve(
        &*st.engine,
        &s,
        &person,
        ResolveRequest {
            tension: p.tension,
            mention: p.mention.map(|m| MentionRequest {
                block: m.block,
                entity: m.entity,
            }),
            dismiss: p.dismiss.map(DismissParams::request),
            restore: p.restore.map(DismissParams::request),
            scope: p.scope,
            accept: p.accept.map(|a| AcceptRequest {
                proposal: a.proposal,
                advised_digest: a.advised_digest,
                tensions: a.tensions.unwrap_or_default(),
                evidence: a.evidence,
                question: a.question,
                session_ref: a.session_ref,
                relayed_by: None,
            }),
        },
    )
}
