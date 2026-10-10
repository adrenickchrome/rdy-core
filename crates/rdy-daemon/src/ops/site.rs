//! The site ops: render, export and import.
use super::*;
use rdy_engine::consequence::write_and_report_as;
use rdy_engine::{RdyError, Scope};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutArgs {
    out: String,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportArgs {
    dir: String,
    view: String,
}

pub(super) fn render(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let s = rdy_render::render_site(&st, &scope, &resolve_path(cx.cwd, &a.out))?;
    Ok(to_json(&s))
}

pub(super) fn export(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.export(&scope, &resolve_path(cx.cwd, &a.out))?))
}

pub(super) fn import(cx: &OpContext, a: ImportArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    let view = view_of(&s, Some(a.view));
    let dir = resolve_path(cx.cwd, &a.dir);
    let scope = Scope::View(view.clone());
    let target = view.clone();
    // the reply is the importer's: every author change is read as this session (finding 11)
    let reader = s.clone();
    let mut v = write_and_report_as(cx.engine, &reader, &target, None, move |r| {
        r.import(&s, &view, &dir)
    })?;
    // each warning names its doc: the title as imported, then the id (the CLI prints it)
    let st = cx.engine.read();
    let docs: serde_json::Map<String, Value> = v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["block"].as_str())
        .filter_map(|id| {
            let bid = rdy_model::BlockId::parse(id).ok()?;
            let label = match st.block(&scope, &bid).ok().and_then(|b| {
                b.field_str("title")
                    .or(b.field_str("name"))
                    .map(str::to_string)
            }) {
                Some(t) if !t.trim().is_empty() => format!("{} ({id}): ", t.trim()),
                _ => format!("{id}: "),
            };
            Some((id.to_string(), Value::String(label)))
        })
        .collect();
    if !docs.is_empty() {
        v["warning_docs"] = Value::Object(docs);
    }
    Ok(v)
}
