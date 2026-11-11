//! The repo-link ops (repo links, D-DAPR8, D-8VJAZ): `links` reads the project's repo links as
//! `rdy_links` does, `new_repo_link` creates a `repo_link` block through the ordinary write, and
//! `adopt_upstream` supersedes a local decision by its adopted copy as `rdy_supersede
//! {old, adopted_as}` does. Reads take one snapshot. Writes run on the writer, briefed as
//! `reaffirm` is: the daemon fetches the briefing itself. There is no op that takes a delivery,
//! revokes a link or exports one: those belong to the hub (the adapter), never to a ctl caller.
use super::*;
use rdy_engine::consequence::write_and_report;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// `links`: the same closed keys as `rdy_links`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinksArgs {
    op: Option<String>,
    alias: Option<String>,
}

/// `new_repo_link`: the fields of a `repo_link` block, under the doc `parent`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewRepoLinkArgs {
    alias: String,
    parent: String,
    repo: String,
    profile: String,
    title: Option<String>,
    view: Option<String>,
}

/// `adopt_upstream`: the local decision and the cited decision that is its upstream copy.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdoptUpstreamArgs {
    old: String,
    adopted_as: String,
    view: Option<String>,
}

fn bad(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

/// The links, one link's delivery, or the pins to cited content: the reads of `rdy_links`, with
/// its refusals.
pub(super) fn links(cx: &OpContext, a: LinksArgs) -> Result<Value, RdyError> {
    let op = a.op.as_deref().unwrap_or("list");
    let st = cx.engine.read();
    match (op, a.alias.as_deref()) {
        ("list", None) => Ok(to_json(&st.links()?)),
        ("list", Some(_)) => Err(bad(
            "list takes no alias",
            "pass op show or refs with alias, or leave alias out",
        )),
        ("show", Some(alias)) => Ok(to_json(&st.link_show(alias)?)),
        ("show", None) => Err(bad(
            "show needs an alias",
            "pass the alias of a repo link: rdy links lists them",
        )),
        ("refs", alias) => Ok(to_json(&st.link_refs(alias)?)),
        (other, _) => Err(bad(
            format!("unknown op {other:?}"),
            format!("use op list, show or refs: {}", rdy_mcp::tools::LINKS_OPS),
        )),
    }
}

/// `rdy new repo-link`: one ordinary `create` of a `repo_link` under the doc `parent`, briefed on
/// that parent as the other supporting creates are. The schema, the alias and repo checks and
/// every other rule are the write path's own. A person approves its landing: the authority policy
/// never accepts a version that changes a `repo_link` block.
pub(super) fn new_repo_link(cx: &OpContext, a: NewRepoLinkArgs) -> Result<Value, RdyError> {
    let mut fields = serde_json::Map::new();
    fields.insert("alias".into(), json!(a.alias));
    fields.insert("repo".into(), json!(a.repo));
    fields.insert("profile".into(), json!(a.profile));
    if let Some(t) = a.title {
        fields.insert("title".into(), json!(t));
    }
    let op = json!({"op": "create", "type": "repo_link", "id": "$repo_link", "fields": fields});
    super::edit::create_briefed(
        cx,
        op,
        Some(a.parent),
        a.view,
        format!("new repo link {}", a.alias),
    )
}

/// `rdy adopt-upstream`: the engine's `supersede_adopted_as` on the writer. The briefing of the
/// old decision is fetched and registered in the same closure, so the write cites a token that
/// nothing moved. An old decision that does not resolve is left to the engine, which refuses it
/// in its own words (cited, external, or no decision).
pub(super) fn adopt_upstream(cx: &OpContext, a: AdoptUpstreamArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let briefs = cx.engine.briefs();
    let (old, adopted_as) = (a.old, a.adopted_as);
    let named = view.clone();
    let mut out = write_and_report(cx.engine, &named, None, move |r| {
        let scope = Scope::View(view.clone());
        let token = r
            .read()
            .resolve_id(&scope, &old)
            .ok()
            .and_then(|id| r.read().brief_ids(&scope, &BTreeSet::from([id]), None).ok())
            .map(|briefing| {
                briefing.register(&briefs);
                briefing.base.token.clone()
            });
        r.supersede_adopted_as(&s, &view, &old, &adopted_as, token, None)
    })?;
    out["view"] = json!(named);
    Ok(out)
}
