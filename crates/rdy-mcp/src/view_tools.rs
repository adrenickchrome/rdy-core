//! mview1: `rdy_view` (create, fork, include, exclude, replay, compare, list, show, close, own, resolve,
//! undo), the
//! views paragraph of `rdy_schema`, and `rdy_log`. The request itself is run by
//! `rdy_engine::view_api::run_view`, the dispatcher the daemon's `view` op shares.
use crate::tools::{McpState, budget, check_tokens, session};
use rdy_engine::view_api::{OwnerInput, ViewRequest, run_view};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// own: the new owner, its actor and run apart (D-PWB82).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerParams {
    /// The new owner's actor: human:<name>, agent:<name>[/<name>], or ci:<repo_id>/<name>.
    pub actor: String,
    /// The owner's run. Omit it for an owner with no run.
    pub run: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewParams {
    /// create, fork, include, exclude, replay, compare, list, show, close, own, resolve, undo. own and close with stale are a person's acts: relay their words with evidence.
    pub op: String,
    /// create and fork: the new view's name; close, own and show: the view.
    pub name: Option<String>,
    /// fork: the view to fork.
    pub from: Option<String>,
    /// create: the landing or version to start from (default: canon's head).
    pub base: Option<String>,
    /// create: pin the view to its base, so landings do not rebase it (default false).
    pub pin: Option<bool>,
    /// include, exclude, replay, resolve, undo, show: the view to act on (default: your own). You need to own it for include, exclude, replay, resolve, close and undo.
    pub view: Option<String>,
    /// close: several views at once.
    pub views: Option<Vec<String>>,
    /// include, exclude and replay: the change ids. Include brings the changes' dependencies with them; exclude refuses while others depend on them unless cascade is true; replay (at least one) brings none.
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
    /// own: the new owner as {actor, run?}.
    pub owner: Option<OwnerParams>,
    /// own: leave the view unowned.
    pub none: Option<bool>,
    /// undo: the op to undo (default: your newest undoable op on the view).
    pub target: Option<String>,
    /// resolve: the block whose conflict to resolve (id, key or path).
    pub block: Option<String>,
    /// resolve: true takes the landed side of a conflict on an accepted or rejected decision: the view then reads canon's frozen version (required).
    pub landed: Option<bool>,
    /// own and close with stale: the person's words, quoted. Without them an agent session is refused (E_PERMISSION).
    pub evidence: Option<String>,
    /// own and close with stale: the question the person answered.
    pub question: Option<String>,
    /// own and close with stale: the chat session the person spoke in (default mcp).
    pub session_ref: Option<String>,
    /// compare, list and show: cut the answer to about this many tokens; include, exclude, replay and undo: the budget of the consequences.
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
    /// true: check every signature offline; each row adds signatures and tag_signatures (default false).
    pub verify: Option<bool>,
    /// A landing id, landing:<id> or tag:<version>: read that landing's row and its change list.
    pub landing: Option<String>,
    /// With landing: next from the previous page.
    pub page: Option<String>,
}

/// `rdy_schema`'s paragraph on views.
pub const SCHEMA_DOC: &str = "rdy_view {op, ...} works on views. Ops: create {name, base?, pin?}, fork {from, name}, include {changes, view?} (brings the changes' dependencies; a change that already landed is refused), exclude {changes, cascade?, view?}, replay {changes, view?} (writes a new change for each listed change, from any view and landed or not, into your open view as one act that one undo removes; it brings no dependency; a block whose version differs from the change's base is recorded as a conflict), compare {a?, b?} (default: your view against canon), list {all?, stale?}, show {view?}, close {name|views|stale, reason?, evidence?, question?, session_ref?}, own {name, owner?: {actor, run?}, none?, evidence, question?, session_ref?}, undo {view?, target?, cascade?}, resolve {block, landed: true, view?} (takes the landed side of a conflict on an accepted or rejected decision: the view reads canon's frozen version, and the change depends on every side); own and close with stale are a person's acts: relay their words with evidence (without it an MCP session gets E_PERMISSION); the act runs as the person, and the reply names relayed_by and the answer record. Every view op defaults to your own view. Ownership: an agent commits to, includes into, excludes from, undoes on, closes and proposes a view only if it owns it (the session that created it or first wrote to it); it never acts in work, the human's view, and with no view it writes to work-<actor>; include another session's changes into yours instead. A conflict is recorded, not an error: two sides changed one block, reads show the base version with a conflict (sides, changes, landings), and you resolve it by writing the block from the version you want (on an accepted or rejected decision, an edit is E_FROZEN; take the landed side with resolve); rdy_propose lists every conflict and marks the ones its version carries as blocking. Codes (each one's when and fix are in errors): E_VIEW_OWNER, E_VIEW_EXISTS, E_VIEW_CLOSED, E_HAS_DEPENDENTS, E_DEPS_MISSING, E_CONFLICT_UNRESOLVED, E_PROPOSAL_MOVED, E_UNDO_BLOCKED, E_UNDO_LANDING, E_NOTHING_TO_UNDO. include, exclude, replay and undo return consequences (max_tokens bounds them); they take no brief.";

/// `rdy_view`: builds the request field by field and runs it for the session.
pub fn rdy_view(st: &mut McpState, p: ViewParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let person = human_actor_for_env(&st.engine.root())?;
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
        owner: p.owner.map(|o| OwnerInput {
            actor: o.actor,
            run: o.run,
        }),
        none: p.none,
        target: p.target,
        block: p.block,
        landed: p.landed,
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

/// `rdy_log`: canon's landings, newest first; with `verify`, each row with its signature reports
/// (`signing_read::verified_log`), checked offline with the configured trust file.
pub fn rdy_log(st: &mut McpState, p: LogParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    if let Some(text) = &p.landing {
        return landing_log(st, text, &p);
    }
    if p.page.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "page continues one landing's change list",
            "pass landing with page",
        ));
    }
    if p.verify == Some(true) {
        let trust = rdy_engine::signing_read::configured_trust(&st.engine.root())?;
        let rows = rdy_engine::signing_read::verified_log(
            &st.engine.read(),
            p.since.as_deref(),
            p.limit,
            trust.as_ref(),
        )?;
        return verified_budget(rows, p.max_tokens);
    }
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

/// `rdy_log {landing}` (O15): one landing's row and its compact change list. The row is never cut;
/// the engine fits the whole reply to `max_tokens` (default 4,000).
fn landing_log(st: &McpState, text: &str, p: &LogParams) -> Result<Value, RdyError> {
    if p.since.is_some() || p.limit.is_some() || p.verify == Some(true) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "landing takes no since, limit or verify",
            "drop since, limit and verify, or drop landing to page the log",
        ));
    }
    let st_ = st.engine.read();
    let id = if text.starts_with("tag:") {
        match st_.resolve_scope(text)? {
            Scope::At(h) => h,
            _ => {
                return Err(RdyError::new(
                    E_INTERNAL,
                    format!("{text:?} did not resolve to a landing"),
                    "report this as a bug in rdy",
                ));
            }
        }
    } else {
        st_.resolve_landing(text.strip_prefix("landing:").unwrap_or(text))?
    };
    let shown = st_.landing_show(
        &id,
        p.page.as_deref(),
        Some(
            p.max_tokens
                .unwrap_or(rdy_engine::diff_request::DIFF_DEFAULT_TOKENS),
        ),
    )?;
    serde_json::to_value(shown).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the landing did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// The budget of a verified log: `budget` cuts whole rows only, so no signature report is ever
/// cut; a page that had rows and keeps none is refused instead of answered empty.
fn verified_budget(rows: Value, max_tokens: Option<usize>) -> Result<Value, RdyError> {
    let had_rows = rows.as_array().is_some_and(|a| !a.is_empty());
    let v = budget(rows, max_tokens);
    let kept_none = v
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if had_rows && kept_none {
        return Err(RdyError::new(
            E_LIMIT,
            "the token budget cannot hold one verified log row",
            "increase max_tokens",
        ));
    }
    Ok(v)
}
