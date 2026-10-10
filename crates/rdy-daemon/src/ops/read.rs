//! The read ops: status, show, ls, log, impact, suspects, tags and block.
use super::*;
use rdy_engine::engine::open_report_json;
use rdy_engine::{E_BAD_INPUT, HISTORY_MAX, RdyError, Scope, block_to_yaml};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogArgs {
    limit: Option<usize>,
    since: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShowArgs {
    id: String,
    scope: Option<String>,
    #[serde(default)]
    blame: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LsArgs {
    path: Option<String>,
    scope: Option<String>,
    depth: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImpactArgs {
    id: String,
    depth: Option<u32>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopeArgs {
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BlockArgs {
    id: String,
    view: Option<String>,
}

/// A decision's derived status in words, e.g. "superseded by D-3K0TY" or "live".
fn status_words(status: &Value) -> Option<String> {
    let word = status["status"].as_str()?.replace('_', " ");
    let list = |k: &str| -> Vec<String> {
        status[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (by, parts) = (list("by"), list("parts"));
    Some(match (by.is_empty(), parts.is_empty()) {
        (true, _) => word,
        (false, true) => format!("{word} by {}", by.join(", ")),
        (false, false) => format!("{word} by {} (parts: {})", by.join(", "), parts.join("; ")),
    })
}

pub(super) fn status(cx: &OpContext) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let landings = st.landings()?.len();
    let canon = st.version_position(&Scope::Canon)?;
    let mut warnings = vec![];
    let mut views = vec![];
    for name in st.view_names() {
        if name.contains(':') {
            warnings.push(format!(
                "view {name:?} has ':' in its name (reserved for scopes since addendum A): read it as view:{name}, propose it with --view view:{name}; it can't receive new writes"
            ));
        }
        let changes = st.included(&name).len();
        let pos = st.version_position(&Scope::View(name.clone()))?;
        let owner = st.view_head(&name).and_then(|h| h.owner.as_ref());
        views.push(
            json!({"name": name, "changes": changes, "owner": owner, "version_position": pos}),
        );
    }
    for w in rdy_engine::config_levels::config_warnings_for_env(&cx.engine.root()) {
        warnings.push(format!("{}: {}; {}", w.code, w.message, w.fix));
    }
    let suspects = st.suspects(&Scope::Canon)?.len();
    Ok(json!({
        "pack": st.pack().name,
        "landings": landings,
        "suspects": suspects,
        "coverage": st.graph_coverage(&Scope::Canon, None)?,
        "version_position": canon,
        "views": views,
        "warnings": warnings,
        "seq": st.seq(),
        "open_report": open_report_json(&cx.engine.open_report()),
        "quarantine": quarantine(&cx.engine.root()),
    }))
}

pub(super) fn show(cx: &OpContext, a: ShowArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    // pshow1: `P-<id>@<n>` is a proposal version, never a block; scope does not apply to it
    if rdy_engine::is_proposal_ref(&a.id) {
        if a.blame {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "blame applies to blocks, not proposals",
                "drop --blame to read the proposal version, or pass a block id",
            ));
        }
        return Ok(json!({"proposal": to_json(&st.proposal_show(&a.id)?)}));
    }
    // refv1: `id@version` or a ref line reads under that version; a clashing scope is refused
    let target = rdy_model::reference::parse_target(&a.id)
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::BAD_REF_FIX))?;
    let text = target
        .scope_text(a.scope.as_deref())
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::PIN_CLASH_FIX))?;
    let scope = scope_of(&st, cx, text.as_deref())?;
    let bid = st.resolve_id(&scope, &target.id)?;
    let ctx = st.context(&scope, &bid)?;
    let mut v = to_json(&ctx);
    let words = status_words(&v["status"]);
    v["status_words"] = words.map_or(Value::Null, Value::String);
    if let Some(conflict) = st.conflict_info(&scope, &bid)? {
        v["conflict"] = to_json(&conflict);
    }
    // cgr1: inflight is an analysis scope with no history; its context carries `inflight`
    // instead. `--blame` there still asks history, which names the scopes it reads.
    if scope == Scope::Inflight && !a.blame {
        return Ok(v);
    }
    // blame1: the last change (or, with `blame`, every version) with its author, landing and why
    let h = st.history(&scope, &bid, if a.blame { HISTORY_MAX } else { 1 })?;
    v["justification"] = h
        .entries
        .first()
        .map_or(Value::Null, |e| to_json(&e.justification));
    v["history"] = to_json(&h.entries);
    v["history_more"] = Value::Bool(h.more);
    Ok(v)
}

pub(super) fn ls(cx: &OpContext, a: LsArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.tree(&scope, a.path.as_deref(), a.depth)?))
}

pub(super) fn log(cx: &OpContext, a: LogArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    Ok(to_json(&st.log_page(a.since.as_deref(), a.limit)?))
}

pub(super) fn impact(cx: &OpContext, a: ImpactArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let bid = st.resolve_id(&scope, &a.id)?;
    let items = st.impact(&scope, &bid, a.depth.unwrap_or(3))?;
    let mut v = json!({"items": items});
    let by_area = rdy_engine::by_area(&items);
    if !by_area.is_empty() {
        v["by_area"] = json!(by_area);
    }
    Ok(v)
}

pub(super) fn suspects(cx: &OpContext, a: ScopeArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(json!({"items": st.suspects(&scope)?}))
}

pub(super) fn tags(cx: &OpContext) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let versions = st.versions()?;
    let current = versions.last().map(|v| v.name.clone());
    Ok(json!({"versions": versions, "current": current}))
}

pub(super) fn block(cx: &OpContext, a: BlockArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let view = view_of(cx.session, a.view);
    let scope = Scope::View(view);
    let bid = st.resolve_id(&scope, &a.id)?;
    let b = st.block(&scope, &bid)?;
    // msw8: the conflict the view reads the block under, which `rdy edit` sends back with the
    // version, so a conflict that changed while the editor was open is refused
    let conflict = st.conflict_on(&scope, &bid)?.map(|c| c.id.to_string());
    Ok(json!({
        "id": b.id.to_string(),
        "version": b.version_hash().to_string(),
        "conflict": conflict,
        "yaml": block_to_yaml(&b),
    }))
}
