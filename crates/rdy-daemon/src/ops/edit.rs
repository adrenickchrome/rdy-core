//! The edit ops: new_folder, new_doc, new_profile, keys_assign and edit_block.
use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::{EditBase, RdyError};
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
pub(super) struct NewProfileArgs {
    key: String,
    parent: String,
    title: String,
    audience: String,
    select: Vec<String>,
    description: Option<String>,
    trail: Option<bool>,
    view: Option<String>,
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
    /// msw8: the conflict the editor read on the block (the `block` op's `conflict`); none is
    /// not conflicted. Read only with `base`.
    conflict: Option<String>,
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

/// The block type of a publication profile (D-KDFZ3).
const PROFILE_TYPE: &str = "publication_profile";

/// `rdy new profile`: one ordinary `create` of a publication profile through the calling
/// session's write, the same op `rdy_write` takes. Every rule on the write (the profile schema,
/// briefing, consequence, role, view ownership) is the write path's own; nothing is checked or
/// stored here. `trail` is left out of the fields when absent, so the audience's default holds.
pub(super) fn new_profile(cx: &OpContext, a: NewProfileArgs) -> Result<Value, RdyError> {
    let mut fields = serde_json::Map::new();
    fields.insert("key".into(), json!(a.key));
    fields.insert("title".into(), json!(a.title));
    fields.insert("audience".into(), json!(a.audience));
    fields.insert("select".into(), json!(a.select));
    if let Some(d) = a.description {
        fields.insert("description".into(), json!(d));
    }
    if let Some(t) = a.trail {
        fields.insert("trail".into(), json!(t));
    }
    let message = format!("new publication profile {}", a.key);
    let ops = rdy_engine::parse_ops(vec![json!({
        "op": "create",
        "type": PROFILE_TYPE,
        "id": "$profile",
        "parent": a.parent,
        "fields": fields,
    })])?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (v, name) = (view.clone(), view.clone());
    let mut out = write_and_report(cx.engine, &v, None, move |r| {
        r.write(
            &s,
            rdy_engine::WriteRequest {
                view,
                ops,
                justification: None,
                message,
            },
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
    let (base, conflict, reason) = (a.base, a.conflict, a.reason);
    write_and_report(cx.engine, &v, max_tokens, move |r| {
        let base = base.as_deref().map(|version| EditBase {
            version,
            conflict: conflict.as_deref(),
        });
        r.edit_yaml_at(&s, &view, &yaml, base, brief, reason.as_deref())
    })
}
