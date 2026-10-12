//! rdy_session, rdy_overview and rdy_tags: the session, the overview and the tags.

use super::*;
use rdy_engine::engine::open_report_json;
use rdy_engine::*;
use rdy_model::Role;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionParams {
    /// Who is acting, e.g. "agent:designer" or "human:nick".
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
                "approvals are made by humans with rdy approve",
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
    if let Some(v) = p.view.as_deref().filter(|v| v.contains(':')) {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            format!("view names cannot contain ':' ({v:?})"),
            "pick a view name without ':'",
        ));
    }
    let session = Session::agent(p.actor, p.run, role)?;
    let view = p.view.unwrap_or_else(|| session.default_view());
    // ses1: a reader may read canon by default; a session that writes needs a view of its own
    if view == "canon" && role != Role::Reader {
        return Err(RdyError::new(
            E_RESERVED_VIEW,
            "canon cannot be a writer's or reviewer's session view",
            "pick another view name; only a reader session may use canon",
        ));
    }
    // ib3: the subscription is written before the session is set, so a refused area leaves the
    // earlier session as it was
    let sub = rdy_engine::inbox::sub::subscribe(&*st.engine, &session, p.areas)?;
    st.hub
        .identify(st.hub_sid(), &session.actor, session.run.as_deref());
    st.view = view;
    st.session = Some(session);
    st.reported.clear();
    let mut out = json!({ "view": st.view, "role": p.role, "areas": sub.areas });
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
    // cc6 (FW-H7): coverage is for the current state only; at a past landing the overview leaves it out, as briefings, search hits and fact sheets do
    let past = st_.is_past_landing(&scope);
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
    // the tree gets what the fixed parts (coverage included) leave
    let reserve =
        300 + coverage.as_ref().map_or(0, tokens_of) + coverage_error.as_ref().map_or(0, tokens_of);
    let tree = fit_tree(st_.tree(&scope, None, None)?, max.saturating_sub(reserve));
    let views: Vec<Value> = st_
        .view_names()
        .into_iter()
        .map(|name| {
            let n = st_.included(&name).len();
            json!({"name": name, "changes": n})
        })
        .collect();
    let landings = st_.landings()?.len();
    let mut out = json!({
        "pack": st_.pack().name, "session": {"actor": s.actor, "view": st.view},
        "project": {"name": st_.project_name(), "description": st_.project_description()},
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
    Ok(budget(out, p.max_tokens))
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
