//! mview1: `rdy_view` (create, fork, include, exclude, compare, list, show, close, undo), the
//! views paragraph of `rdy_schema`, and `rdy_log`. The request itself is run by
//! `rdy_engine::view_api::run_view`, the dispatcher the daemon's `view` op shares.
use crate::tools::{McpState, budget, check_tokens, session};
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewParams {
    /// create, fork, include, exclude, compare, list, show, close, own, undo. own and close with stale are a person's acts: relay their words with evidence.
    pub op: String,
    /// create and fork: the new view's name; close, own and show: the view.
    pub name: Option<String>,
    /// fork: the view to fork.
    pub from: Option<String>,
    /// create: the landing or version to start from (default: canon's head).
    pub base: Option<String>,
    /// create: pin the view to its base, so landings do not rebase it (default false).
    pub pin: Option<bool>,
    /// include, exclude, undo, show: the view to act on (default: your own). You need to own it for include, exclude, close and undo.
    pub view: Option<String>,
    /// close: several views at once.
    pub views: Option<Vec<String>>,
    /// include and exclude: the change ids. Include brings the changes' dependencies with them; exclude refuses while others depend on them unless cascade is true.
    pub changes: Option<Vec<String>>,
    /// exclude and undo: also exclude the changes that depend on these (default false).
    pub cascade: Option<bool>,
    /// compare: the first scope (default: your view).
    pub a: Option<String>,
    /// compare: the second scope (default: canon).
    pub b: Option<String>,
    /// list: include closed views (default false).
    pub all: Option<bool>,
    /// list: mark views idle after this many days; close: close the views idle that long.
    pub stale: Option<u32>,
    /// close: why, recorded on the view.
    pub reason: Option<String>,
    /// own: the new owner, actor or actor/run.
    pub owner: Option<String>,
    /// own: leave the view unowned.
    pub none: Option<bool>,
    /// undo: the op to undo (default: your newest undoable op on the view).
    pub target: Option<String>,
    /// own and close with stale: the person's words, quoted. Without them an agent session is refused (E_PERMISSION).
    pub evidence: Option<String>,
    /// own and close with stale: the question the person answered.
    pub question: Option<String>,
    /// own and close with stale: the chat session the person spoke in (default mcp).
    pub session_ref: Option<String>,
    /// compare, list and show: cut the answer to about this many tokens; include, exclude and undo: the budget of the consequences.
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogParams {
    /// At most this many landings, newest first.
    pub limit: Option<usize>,
    /// Only landings after this one: a landing id, landing:<id> or tag:<version>.
    pub since: Option<String>,
    pub max_tokens: Option<usize>,
}

/// `rdy_schema`'s paragraph on views.
pub const SCHEMA_DOC: &str = "rdy_view {op, ...} works on views. Ops: create {name, base?, pin?}, fork {from, name}, include {changes, view?} (brings the changes' dependencies; a change that already landed is refused), exclude {changes, cascade?, view?}, compare {a?, b?} (default: your view against canon), list {all?, stale?}, show {view?}, close {name|views|stale, reason?, evidence?, question?, session_ref?}, own {view, owner?, none?, evidence, question?, session_ref?}, undo {view?, target?, cascade?}; own and close with stale are a person's acts: relay their words with evidence (without it an MCP session gets E_PERMISSION); the act runs as the person, and the reply names relayed_by and the answer record. Every view op defaults to your own view. Ownership: an agent commits to, includes into, excludes from, undoes on, closes and proposes a view only if it owns it (the session that created it or first wrote to it); it never acts in work, the human's view, and with no view it writes to work-<actor>; include another session's changes into yours instead. A conflict is recorded, not an error: two sides changed one block, reads show the base version with a conflict (sides, changes, landings), and you resolve it by writing the block from the version you want; rdy_propose lists every conflict and marks the ones its version carries as blocking. Codes (each one's when and fix are in errors): E_VIEW_OWNER, E_VIEW_EXISTS, E_VIEW_CLOSED, E_HAS_DEPENDENTS, E_DEPS_MISSING, E_CONFLICT_UNRESOLVED, E_PROPOSAL_MOVED, E_UNDO_BLOCKED, E_UNDO_LANDING, E_NOTHING_TO_UNDO. include, exclude and undo return consequences (max_tokens bounds them); they take no brief.";

/// `rdy_view`: builds the request field by field and runs it for the session.
pub fn rdy_view(st: &mut McpState, p: ViewParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root());
    let max_tokens = p.max_tokens;
    let req = ViewRequest {
        op: p.op,
        name: p.name,
        from: p.from,
        base: p.base,
        pin: p.pin,
        view: p.view,
        views: p.views,
        changes: p.changes,
        cascade: p.cascade,
        a: p.a,
        b: p.b,
        all: p.all,
        stale: p.stale,
        reason: p.reason,
        owner: p.owner,
        none: p.none,
        target: p.target,
        max_tokens: p.max_tokens,
        evidence: p.evidence,
        question: p.question,
        session_ref: p.session_ref,
        relayed_by: None,
    };
    let op = req.op.clone();
    let mut v = run_view(&*st.engine, &s, &person, &st.view, req)?;
    match op.as_str() {
        "list" => {
            let max = max_tokens.unwrap_or(4000);
            let over = serde_json::to_string(&v).map(|t| t.len() / 4).unwrap_or(0) > max;
            if over && let Some(rows) = v["views"].as_array_mut() {
                for row in rows {
                    if let Some(o) = row.as_object_mut() {
                        o.remove("blocks");
                        o.remove("conflicts");
                    }
                }
            }
            Ok(budget(v, max_tokens))
        }
        "compare" | "show" => Ok(budget(v.take(), max_tokens)),
        _ => Ok(v),
    }
}

/// `rdy_log`: canon's landings, newest first.
pub fn rdy_log(st: &mut McpState, p: LogParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let rows = serde_json::to_value(st_.log_page(p.since.as_deref(), p.limit)?).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the log did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    Ok(budget(rows, p.max_tokens))
}
