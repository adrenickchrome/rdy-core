//! rdy_session, rdy_overview and rdy_tags: the session, the overview and the tags.

use super::*;
use rdy_engine::engine::open_report_json;
use rdy_engine::pending::PendingQuery;
use rdy_engine::*;
use rdy_model::Role;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionParams {
    /// Who is acting: this agent's own actor, e.g. "agent:designer" (a human actor is refused).
    pub actor: String,
    /// The run this agent belongs to; also the default view name.
    pub run: Option<String>,
    /// reader, writer or reviewer.
    pub role: String,
    /// Override the default view for writes. A reader may pass "canon" to read canon by default.
    pub view: Option<String>,
    /// Areas this session watches for landings, persisted with its inbox cursor; [] clears them (the areas of the blocks you change apply).
    pub areas: Option<Vec<String>>,
}

fn depth_of(nodes: &[TreeNode]) -> usize {
    nodes
        .iter()
        .map(|n| 1 + depth_of(&n.children))
        .max()
        .unwrap_or(0)
}

fn count_nodes(ns: &[TreeNode]) -> usize {
    ns.iter()
        .map(|n| 1 + n.collapsed + count_nodes(&n.children))
        .sum()
}

/// Replaces the children of every node `level` levels down with a count.
fn collapse_at(nodes: &mut [TreeNode], level: usize) {
    for n in nodes {
        if level == 0 {
            if !n.children.is_empty() {
                n.collapsed += count_nodes(&n.children);
                n.children.clear();
            }
        } else {
            collapse_at(&mut n.children, level - 1);
        }
    }
}

/// Collapses the deepest levels first until the tree fits in `max_tokens` (or only roots remain).
fn fit_tree(mut nodes: Vec<TreeNode>, max_tokens: usize) -> Vec<TreeNode> {
    let size = |n: &Vec<TreeNode>| serde_json::to_string(n).map(|s| s.len() / 4).unwrap_or(0);
    let mut depth = depth_of(&nodes);
    while depth > 1 && size(&nodes) > max_tokens {
        depth -= 1;
        collapse_at(&mut nodes, depth - 1);
    }
    nodes
}

/// The fix of an agent session's human claim over MCP (D-ME2C0): the CLI's names RDY_ACTOR.
const MCP_HUMAN_CLAIM_FIX: &str =
    "claim your own agent: actor, and relay the person's words with rdy_answer";

pub fn rdy_session(st: &mut McpState, p: SessionParams) -> Result<Value, RdyError> {
    // compiled only for this crate's own tests: a release binary has no hook and reads no env
    #[cfg(test)]
    if p.actor == "__rdy_test_panic__" {
        panic!("test hook: deliberate panic")
    }
    let role = match p.role.as_str() {
        "reader" => Role::Reader,
        "writer" => Role::Writer,
        "reviewer" => Role::Reviewer,
        "approver" => {
            return Err(RdyError::new(
                E_PERMISSION,
                "approver sessions are not available over MCP",
                "relay the person's answer with rdy_answer",
            ));
        }
        other => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("unknown role {other:?}"),
                "use reader, writer or reviewer",
            ));
        }
    };
    // D-CQMA4: the views of re-affirmation cards' landings are private
    if p.view.as_deref().is_some_and(rdy_engine::is_reserved_view) {
        return Err(rdy_engine::reserved_view_error());
    }
    if let Some(v) = p.view.as_deref().filter(|v| v.contains(':')) {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            format!("view names cannot contain ':' ({v:?})"),
            "pick a view name without ':'",
        ));
    }
    // D-ME2C0: MCP is an agent surface; a human actor here is a claim, with its MCP fix
    let session = Session::agent(p.actor, p.run, role).map_err(|mut e| {
        if Session::is_human_claim(&e) {
            e.fix = MCP_HUMAN_CLAIM_FIX.to_string();
        }
        e
    })?;
    let view = p.view.unwrap_or_else(|| session.default_view());
    // ses1: a reader may read canon by default; a session that writes needs a view of its own
    if view == "canon" && role != Role::Reader {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            "canon cannot be a writer's or reviewer's session view",
            "pick another view name; only a reader session may use canon",
        ));
    }
    // the configured person is resolved before anything changes: a refusal (an unreadable config,
    // an actor outside the form) leaves the earlier session, view and subscription as they were
    let person = human_actor_for_env(&st.engine.root())?;
    // ib3: the subscription is written before the session is set, so a refused area leaves the
    // earlier session as it was
    let sub = rdy_engine::inbox::sub::subscribe(&*st.engine, &session, p.areas)?;
    st.hub
        .identify(st.hub_sid(), &session.actor, session.run.as_deref());
    st.view = view;
    st.session = Some(session);
    st.reported.clear();
    let read = st.engine.read();
    // FW-PC1: the cadence the agent follows, and the count under cadence `changes`
    let cadence = read.propose_cadence();
    let mut propose = json!({ "cadence": cadence.as_str() });
    if cadence == ProposeCadence::Changes {
        propose["changes"] = json!(read.propose_changes());
    }
    let mut out = json!({
        "view": st.view, "role": p.role, "areas": sub.areas, "propose": propose,
    });
    // s-4A488: what waits for the person, so the agent checks the queue at session start. The
    // count never hides a snoozed item; a queue that cannot be read does not fail the session.
    let query = PendingQuery {
        person,
        ..PendingQuery::default()
    };
    // final review fix round 1 (R6): both counts are of the whole queue, past its cap too
    match read.pending_counts(&query) {
        Ok(c) => {
            out["pending"] = json!({"items": c.items, "blocking": c.blocking});
        }
        Err(e) => {
            out["pending"] = json!({"items": 0, "blocking": 0});
            out["pending_error"] = json!(format!("{}: {}", e.code, e.message));
        }
    }
    // R-P6: a config key rdy does not know is a warning, never silence
    let warnings = rdy_engine::config_levels::config_warnings_for_env(&st.engine.root());
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(out)
}

pub fn rdy_overview(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    let position = st_.version_position(&scope)?;
    let max = p.max_tokens.unwrap_or(4000);
    // cc6 (FW-H7): coverage is for the current state only; at a past landing or in a proposal
    // version (ls8) the overview leaves it out, as briefings, search hits and fact sheets do
    let past = st_.coverage_left_out(&scope);
    // cc15 (FW-CH3a): the score, its dimensions, the issue count and the kinds, from the
    // memoised summary; the pages, areas and issues are `rdy_coverage`'s. A coverage read error
    // never fails the overview: `coverage_error` says why the coverage is left out.
    let (coverage, coverage_error) = if past {
        (None, None)
    } else {
        let cov_scope = st_.coverage_scope(&scope)?;
        match st_.coverage_summary(&cov_scope) {
            Ok(s) => (Some(json!(&*s)), None),
            Err(e) => (
                None,
                Some(json!(rdy_engine::coverage::coverage_error_text(&e))),
            ),
        }
    };
    // a carried removal this store does not repeat is part of the current store, whatever the
    // content scope: its serialized size is reserved first and no part of it is dropped
    let carried = st_.carried_warnings()?;
    let carried_json = (!carried.is_empty()).then(|| json!(carried));
    let carried_tokens = carried_json.as_ref().map_or(0, tokens_of);
    if carried_tokens > 0 && carried_tokens + 300 > max {
        return Err(RdyError::new(
            E_REDACTION_BUDGET,
            REDACTION_BUDGET_MESSAGE,
            rdy_engine::carried_warning::CARRIED_WARNING_BUDGET_FIX,
        ));
    }
    // the tree gets what the fixed parts (coverage included) leave
    let reserve = 300
        + carried_tokens
        + coverage.as_ref().map_or(0, tokens_of)
        + coverage_error.as_ref().map_or(0, tokens_of);
    let tree = fit_tree(st_.tree(&scope, None, None)?, max.saturating_sub(reserve));
    let views: Vec<Value> = st_
        .views_with_changes()
        .into_iter()
        .map(|(name, n)| json!({"name": name, "changes": n}))
        .collect();
    let landings = st_.landings()?.len();
    // ec10 (D-KQ4CA): the repo id, and the origin of a clone or a fork, as `rdy status` shows them
    let mut project = json!({"name": st_.project_name(), "description": st_.project_description()});
    if let Some(p) = project.as_object_mut() {
        p.extend(rdy_engine::repo::repo_json(&st_)?);
    }
    let mut out = json!({
        "pack": st_.pack().name, "session": {"actor": s.actor, "view": st.view},
        "project": project,
        "canon": {"landings": landings},
        "open_report": open_report_json(&st.engine.open_report()),
        "views": views, "tree": tree, "version_position": position,
    });
    if let Some(c) = coverage {
        out["coverage"] = c;
    }
    if let Some(e) = coverage_error {
        out["coverage_error"] = e;
    }
    // malt1: a session in a member view of an open set sees the set and its other members
    if let Some((stable, version, set)) = st_.alt_of_view(&st.view)? {
        let info = st_.alt_info(stable, version, &set)?;
        out["alternatives"] = json!({
            "set": info.id,
            "title": info.title,
            "question": info.question,
            "members": info.members.iter().map(|m| json!({
                "label": m.label, "view": m.view, "owner": m.owner,
                "changes": m.changes, "mine": m.view == st.view,
            })).collect::<Vec<_>>(),
        });
    }
    // the rest of the reply fits what the warnings leave; the warnings are added whole
    let mut out = budget(out, Some(max.saturating_sub(carried_tokens)));
    if let Some(w) = carried_json {
        out["redaction_warnings"] = w;
    }
    Ok(out)
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagsParams {}

pub fn rdy_tags(st: &mut McpState, _p: TagsParams) -> Result<Value, RdyError> {
    session(st)?;
    let st_ = st.engine.read();
    let versions = st_.versions()?;
    let current = versions.last().map(|v| v.name.clone());
    Ok(json!({"versions": versions, "current": current}))
}
