//! The edit ops: new_folder, new_doc, new_profile, new_finding, new_source, keys_assign and
//! edit_block.
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

/// `rdy new finding` (D-Z8P09): a statement and its evidence entries, each `{quote,
/// session_ref?, question?}` or `{source}` with the source as an id, key or path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewFindingArgs {
    statement: String,
    evidence: Vec<Value>,
    parent: Option<String>,
    view: Option<String>,
}

/// `rdy new source` (D-Z8P09): a release, commit or spec.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewSourceArgs {
    title: String,
    kind: String,
    locator: String,
    version: Option<String>,
    parent: Option<String>,
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

/// One ordinary `create` of `ty` with `fields` (and `parent` when given) through the calling
/// session's write, as `rdy_write` takes it: every rule (the schema and the evidence check, the
/// decision home for a missing parent, briefing, consequences, role, view ownership) is the write
/// path's own. No working-copy file is written: both types live only in the store.
fn create_supporting(
    cx: &OpContext,
    ty: &str,
    placeholder: &str,
    fields: serde_json::Map<String, Value>,
    parent: Option<String>,
    view: Option<String>,
    message: String,
) -> Result<Value, RdyError> {
    let mut op = json!({"op": "create", "type": ty, "id": placeholder, "fields": fields});
    if let Some(p) = parent {
        op["parent"] = json!(p);
    }
    let ops = rdy_engine::parse_ops(vec![op])?;
    let s = cx.session.clone();
    let view = view_of(&s, view);
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

/// `rdy new finding`: each evidence `source` given as an id, key or path is resolved in the view
/// with the reader's resolver and written as the full reference `{{S-…}}`; everything else is
/// the write's to check.
pub(super) fn new_finding(cx: &OpContext, a: NewFindingArgs) -> Result<Value, RdyError> {
    let view = view_of(cx.session, a.view.clone());
    let mut evidence = a.evidence;
    {
        let st = cx.engine.read();
        let scope = Scope::View(view.clone());
        for entry in &mut evidence {
            let Some(text) = entry.get("source").and_then(Value::as_str) else {
                continue;
            };
            if text.starts_with("{{") {
                continue;
            }
            let id = st.resolve_id(&scope, text)?;
            entry["source"] = json!(format!("{{{{{id}}}}}"));
        }
    }
    let mut fields = serde_json::Map::new();
    fields.insert("statement".into(), json!(a.statement));
    fields.insert("evidence".into(), Value::Array(evidence));
    create_supporting(
        cx,
        "finding",
        "$finding",
        fields,
        a.parent,
        Some(view),
        "new finding".into(),
    )
}

/// `rdy new source`.
pub(super) fn new_source(cx: &OpContext, a: NewSourceArgs) -> Result<Value, RdyError> {
    let mut fields = serde_json::Map::new();
    fields.insert("title".into(), json!(a.title));
    fields.insert("kind".into(), json!(a.kind));
    fields.insert("locator".into(), json!(a.locator));
    if let Some(v) = a.version {
        fields.insert("version".into(), json!(v));
    }
    let message = format!("new source {}", a.title);
    create_supporting(cx, "source", "$source", fields, a.parent, a.view, message)
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
