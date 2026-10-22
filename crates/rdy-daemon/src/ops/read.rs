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
    /// Check every signature offline: each row adds `signatures` and `tag_signatures`.
    #[serde(default)]
    verify: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CoverageArgs {
    scope: Option<String>,
    path: Option<String>,
    max_tokens: Option<usize>,
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
    }
    for (name, changes) in st.views_with_changes() {
        let pos = st.version_position(&Scope::View(name.clone()))?;
        let owner = st.view_head(&name).and_then(|h| h.owner.as_ref());
        views.push(
            json!({"name": name, "changes": changes, "owner": owner, "version_position": pos}),
        );
    }
    for w in rdy_engine::config_levels::config_warnings_for_env(&cx.engine.root()) {
        warnings.push(format!("{}: {}; {}", w.code, w.message, w.fix));
    }
    // a carried removal this store does not repeat is a warning while a span still holds its
    // payload (D-QF2HQ): the string joins the list, the structured form is `redaction_warnings`
    let carried = st.carried_warnings()?;
    for w in &carried {
        warnings.push(format!("{}: {}; {}", w.code, w.message, w.fix));
    }
    let suspects = st.suspect_details(&Scope::Canon)?.len();
    // cc15 (FW-CH3a): the current score, its dimensions, the issue count and the kinds, from
    // the memoised summary; the pages, areas and issues are `rdy coverage`'s. A coverage read
    // error never fails the status: `coverage_error` says why the coverage is left out.
    let (coverage, coverage_error) = match st.coverage_summary(&Scope::Canon) {
        Ok(s) => (Some(to_json(&*s)), None),
        Err(e) => (None, Some(rdy_engine::coverage::coverage_error_text(&e))),
    };
    let mut v = json!({
        "pack": st.pack().name,
        "landings": landings,
        "suspects": suspects,
        "version_position": canon,
        "views": views,
        "warnings": warnings,
        "seq": st.seq(),
        "open_report": open_report_json(&cx.engine.open_report()),
        "quarantine": quarantine(&cx.engine.root()),
    });
    // ec10 (D-DAGNB): the repo id, and the origin of a clone or a fork
    if let Some(o) = v.as_object_mut() {
        o.extend(rdy_engine::repo::repo_json(&st)?);
    }
    if let Some(c) = coverage {
        v["coverage"] = c;
    }
    if let Some(e) = coverage_error {
        v["coverage_error"] = Value::String(e);
    }
    if !carried.is_empty() {
        v["redaction_warnings"] = to_json(&carried);
    }
    Ok(v)
}

pub(super) fn show(cx: &OpContext, a: ShowArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    // pshow1: `P-<id>@<n>` is a proposal version, never a block; scope does not apply to it.
    // A full hash names a proposal version too, as an approval reads it (rdx13: `rdy approve`
    // reads the version's removal card here, by any spelling the approval takes)
    if rdy_engine::is_proposal_ref(&a.id) || a.id.parse::<rdy_model::Hash>().is_ok() {
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

/// `log`; with `verify`, each row with its signature reports, checked offline with the
/// configured trust file, parsed once (no agent is asked).
pub(super) fn log(cx: &OpContext, a: LogArgs) -> Result<Value, RdyError> {
    if a.verify {
        let trust = rdy_engine::signing_read::configured_trust(&cx.engine.root())?;
        let st = cx.engine.read();
        return rdy_engine::signing_read::verified_log(
            &st,
            a.since.as_deref(),
            a.limit,
            trust.as_ref(),
        );
    }
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
    Ok(json!({"items": st.suspect_details(&scope)?}))
}

/// The coverage report of a scope (default canon) or a path, cut to `max_tokens` as the MCP
/// tool cuts it. A read: nothing is written.
pub(super) fn coverage(cx: &OpContext, a: CoverageArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    rdy_mcp::tools::coverage_reply(&st, &scope, a.path.as_deref(), a.max_tokens)
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
