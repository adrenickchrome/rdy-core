//! The edit ops: new_folder, new_doc, keys_assign and edit_block.
use super::*;
use rdy_engine::RdyError;
use rdy_engine::consequence::write_and_report;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewFolderArgs {
    path: String,
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewDocArgs {
    path: String,
    view: Option<String>,
    doc_type: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeysAssignArgs {
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EditBlockArgs {
    yaml: String,
    view: Option<String>,
    /// msw8 (#1064): the version the editor read (the `block` op's `version`); none is current.
    base: Option<String>,
    brief: Option<String>,
    max_tokens: Option<usize>,
    /// The edit's editorial reason (`rdy edit --reason`).
    reason: Option<String>,
}

pub(super) fn new_folder(cx: &OpContext, a: NewFolderArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (v, name) = (view.clone(), view.clone());
    let mut out = write_and_report(cx.engine, &v, None, move |r| {
        r.new_folder(&s, &view, &a.path)
    })?;
    out["view"] = json!(name);
    Ok(out)
}

pub(super) fn new_doc(cx: &OpContext, a: NewDocArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (v, name) = (view.clone(), view.clone());
    let mut out = write_and_report(cx.engine, &v, None, move |r| {
        r.new_doc(
            &s,
            &view,
            &a.path,
            a.doc_type.as_deref(),
            a.title.as_deref(),
        )
    })?;
    out["view"] = json!(name);
    Ok(out)
}

pub(super) fn keys_assign(cx: &OpContext, a: KeysAssignArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (v, name) = (view.clone(), view.clone());
    let mut out = write_and_report(cx.engine, &v, None, move |r| r.assign_keys(&s, &view))?;
    out["view"] = json!(name);
    Ok(out)
}

pub(super) fn edit_block(cx: &OpContext, a: EditBlockArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (yaml, brief, max_tokens, v) = (a.yaml, a.brief, a.max_tokens, view.clone());
    let (base, reason) = (a.base, a.reason);
    write_and_report(cx.engine, &v, max_tokens, move |r| {
        r.edit_yaml_at(&s, &view, &yaml, base.as_deref(), brief, reason.as_deref())
    })
}
