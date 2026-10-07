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
pub struct ViewParams {
    /// create, fork, include, exclude, compare, list, show, close, undo (own is the human's: E_PERMISSION here).
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
    /// compare, list and show: cut the answer to about this many tokens.
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LogParams {
    /// At most this many landings, newest first.
    pub limit: Option<usize>,
    /// Only landings after this one: a landing id, landing:<id> or tag:<version>.
    pub since: Option<String>,
    pub max_tokens: Option<usize>,
}

/// `rdy_schema`'s paragraph on views.
pub const SCHEMA_DOC: &str = "rdy_view {op, ...} works on views. Ops: create {name, base?, pin?}, fork {from, name}, include {changes, view?} (brings the changes' dependencies; a change that already landed is refused), exclude {changes, cascade?, view?}, compare {a?, b?} (default: your view against canon), list {all?, stale?}, show {view?}, close {name|views|stale, reason?}, undo {view?, target?, cascade?}; own is the human's (E_PERMISSION from an MCP session). Every view op defaults to your own view. Ownership: you may include into, exclude from, close and undo on a view only if you own it (the session that created it, or first wrote to it); never write into another session's view, include its changes into yours instead. A conflict is recorded, not an error: two sides changed one block, reads show the base version with a conflict (sides, changes, landings), and you resolve it by writing the block from the version you want; rdy_propose lists every conflict and marks the ones its version carries as blocking. Codes: E_VIEW_OWNER: the view belongs to another session (write to your own, fork it, or include its changes); E_VIEW_EXISTS: a view of that name exists (pick another name); E_VIEW_CLOSED: the view is closed and takes no write, include or exclude (undo the close, or fork it); E_HAS_DEPENDENTS: changes in the view depend on what you exclude (exclude them too, or cascade: true); E_DEPS_MISSING: a proposal subset leaves out changes its members depend on (add them to changes); E_CONFLICT_UNRESOLVED: the proposal version carries a conflict it does not resolve (write the block, then propose again); E_PROPOSAL_MOVED: a newer version exists, or a landing since the version changed blocks it writes, or the project requires proposals on the current canon (propose again; a human approves the latest P-x@n); E_UNDO_BLOCKED: undoing would leave the view inconsistent (undo with cascade: true, or undo the dependent ops first); E_UNDO_LANDING: the undo would bring back or remove work that has landed (a landing is undone by rdy_revoke); E_NOTHING_TO_UNDO: you have no undoable op of your own on the view (name one with target).";

/// `rdy_view`: builds the request field by field and runs it for the session.
pub fn rdy_view(st: &mut McpState, p: ViewParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
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
    };
    let op = req.op.clone();
    let mut v = run_view(&*st.engine, &s, &st.view, req)?;
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
