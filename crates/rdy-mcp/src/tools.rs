use base64::Engine;
use rdy_engine::*;
use rdy_model::{decision, BlockId, EditorialClass, Justification, Role};
use rdy_schema::{BodyRule, Trait};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

pub const E_NO_SESSION: &str = "E_NO_SESSION";

pub struct McpState {
    pub rdy: Rdy,
    pub session: Option<Session>,
    pub view: String,
}

impl McpState {
    pub fn new(rdy: Rdy) -> McpState {
        McpState { rdy, session: None, view: "work".into() }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionParams {
    /// Who is acting, e.g. "agent:designer" or "human:nick".
    pub actor: String,
    /// The run this agent belongs to; also the default view name.
    pub run: Option<String>,
    /// reader, writer or reviewer.
    pub role: String,
    /// Override the default view for writes.
    pub view: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScopeParams {
    /// "canon" or a view name. Defaults to the session view.
    pub scope: Option<String>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdParams {
    pub id: String,
    pub scope: Option<String>,
    /// rdy_read only: compact (default), markdown or yaml.
    pub format: Option<String>,
    /// rdy_impact only: traversal depth (default 3).
    pub depth: Option<u32>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaParams {
    /// Describe one block type; omit to list all types.
    pub block_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditorialParams {
    /// typo, restructure or clarify.
    pub class: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteParams {
    pub view: Option<String>,
    /// Ops: {"op":"create","type":..,"id":"$name"?,"parent":..,"after":..,"fields":{..},"body":..,"links":{..}},
    /// {"op":"update","id":..,"base":..?,"fields":{..},"unset":[..],"body":..,"links":{..}},
    /// {"op":"move","id":..,"parent":..,"after":..}, {"op":"delete","id":..}.
    pub ops: Vec<Value>,
    /// The decision this change carries out.
    pub implements: Option<String>,
    pub editorial: Option<EditorialParams>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DecideParams {
    pub view: Option<String>,
    pub parent: String,
    pub statement: String,
    pub options: Option<Vec<String>>,
    pub authority_basis: Option<String>,
    pub attested_by: Option<String>,
    pub reversibility: Option<String>,
    pub kind: Option<String>,
    pub review_by: Option<String>,
    pub body: Option<String>,
    pub affects: Option<Vec<String>>,
    pub depends_on: Option<Vec<String>>,
    pub conditional_on: Option<Vec<String>>,
    pub refines: Option<Vec<String>>,
    pub answers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupersedeParams {
    pub old: String,
    /// The words of the old decision that this one retires; omit to supersede all of it.
    pub part: Option<String>,
    pub decision: DecideParams,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RevokeParams {
    pub view: Option<String>,
    pub target: String,
    pub reason: String,
    pub parent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeParams {
    pub view: Option<String>,
    pub title: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssetParams {
    pub data_base64: String,
}

pub fn error_json(e: &RdyError) -> Value {
    serde_json::to_value(e).unwrap_or_else(|_| json!({"code": e.code, "message": e.message, "fix": e.fix}))
}

pub fn budget(v: Value, max_tokens: Option<usize>) -> Value {
    let max = max_tokens.unwrap_or(4000);
    let size = |v: &Value| serde_json::to_string(v).map(|s| s.len() / 4).unwrap_or(0);
    if size(&v) <= max {
        return v;
    }
    let mut v = match v {
        Value::Array(a) => json!({ "items": a }),
        other => other,
    };
    let Some(obj) = v.as_object_mut() else { return v };
    let key = obj.iter().filter_map(|(k, x)| x.as_array().map(|a| (serde_json::to_string(a).map(|s| s.len()).unwrap_or(0), k.clone()))).max().map(|(_, k)| k);
    let Some(key) = key else {
        // no list to shorten: cut the longest string instead (e.g. rdy_read's text)
        let longest = obj.iter().filter_map(|(k, x)| x.as_str().map(|s| (s.len(), k.clone()))).max().map(|(_, k)| k);
        if let Some(k) = longest {
            let text = obj[&k].as_str().unwrap_or_default().to_string();
            let keep = (max * 4).saturating_sub(200).min(text.len());
            let cut = (0..=keep).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
            obj.insert(k, json!(text[..cut]));
            obj.insert("truncated".into(), json!(true));
            obj.insert("omitted".into(), json!(text.len() - cut));
        }
        return v;
    };
    let full = obj[&key].as_array().cloned().unwrap_or_default();
    let (mut lo, mut hi) = (0usize, full.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        obj.insert(key.clone(), Value::Array(full[..mid].to_vec()));
        obj.insert("truncated".into(), json!(true));
        obj.insert("omitted".into(), json!(full.len() - mid));
        if size(&Value::Object(obj.clone())) <= max { lo = mid } else { hi = mid - 1 }
    }
    obj.insert(key, Value::Array(full[..lo].to_vec()));
    obj.insert("truncated".into(), json!(true));
    obj.insert("omitted".into(), json!(full.len() - lo));
    v
}

fn session(st: &McpState) -> Result<Session, RdyError> {
    st.session.clone().ok_or_else(|| RdyError::new(E_NO_SESSION, "no session", "call rdy_session first with your actor, run and role"))
}

fn scope_of(st: &McpState, s: &Option<String>) -> Scope {
    Scope::parse(s.as_deref().unwrap_or(&st.view))
}

fn id_of(s: &str) -> Result<BlockId, RdyError> {
    BlockId::parse(s).map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "ids look like b-7KQ2M"))
}

fn ok<T: serde::Serialize>(t: T) -> Result<Value, RdyError> {
    Ok(serde_json::to_value(t).expect("serializable"))
}

pub fn rdy_session(st: &mut McpState, p: SessionParams) -> Result<Value, RdyError> {
    let role = match p.role.as_str() {
        "reader" => Role::Reader,
        "writer" => Role::Writer,
        "reviewer" => Role::Reviewer,
        "approver" => return Err(RdyError::new(E_PERMISSION, "approver sessions are not available over MCP", "approvals are made by humans with rdy approve")),
        other => return Err(RdyError::new(E_BAD_INPUT, format!("unknown role {other:?}"), "use reader, writer or reviewer")),
    };
    st.view = p.view.or_else(|| p.run.clone()).unwrap_or_else(|| "work".into());
    if st.view == "canon" {
        return Err(RdyError::new(E_RESERVED_VIEW, "canon cannot be a session view", "pick another view name"));
    }
    st.session = Some(Session { actor: p.actor, run: p.run, role });
    Ok(json!({ "view": st.view, "role": p.role }))
}

pub fn rdy_overview(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let scope = scope_of(st, &p.scope);
    let docs: Vec<Value> = st.rdy.materialized(&scope)?.children(None).iter().map(|d| json!({"id": d.id.to_string(), "title": d.field_str("title"), "doc_type": d.field_str("doc_type")})).collect();
    let store = st.rdy.store();
    let views: Vec<Value> = store
        .refs_with_prefix("view/")?
        .into_iter()
        .map(|(name, _)| {
            let name = name.trim_start_matches("view/").to_string();
            let n = store.view(&name).map(|v| v.changes.len()).unwrap_or(0);
            json!({"name": name, "changes": n})
        })
        .collect();
    let landings = store.landings()?.len();
    Ok(budget(json!({
        "pack": st.rdy.pack().name, "session": {"actor": s.actor, "view": st.view},
        "canon": {"landings": landings}, "views": views, "docs": docs,
    }), p.max_tokens))
}

pub fn rdy_read(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    session(st)?;
    let scope = scope_of(st, &p.scope);
    let id = id_of(&p.id)?;
    let suspects: std::collections::BTreeSet<BlockId> = st.rdy.suspects(&scope)?.into_iter().map(|s| s.block).collect();
    st.rdy.graph(&scope)?;
    let m = st.rdy.materialized(&scope)?.clone();
    let g = st.rdy.graph(&scope)?;
    let root = m.get(&id).ok_or_else(|| RdyError::new(E_UNKNOWN_BLOCK, format!("no block {id}"), "list docs with rdy_overview"))?;
    let format = p.format.as_deref().unwrap_or("compact");
    let mut lines = vec![];
    let mut stack = vec![(root, 0usize)];
    while let Some((b, depth)) = stack.pop() {
        let status = g.status(&b.id).map(|s| serde_json::to_value(s).ok().and_then(|v| v["status"].as_str().map(String::from)).unwrap_or_default());
        match format {
            "yaml" => lines.push(block_to_yaml(b)),
            "markdown" => lines.push(match b.block_type.as_str() {
                "doc" => format!("# {}", b.field_str("title").unwrap_or("")),
                "section" => format!("{} {}", "#".repeat((depth + 1).min(6)), b.field_str("title").unwrap_or("")),
                "term" | "component" | "actor" | "interface" => format!("**{}** — {}", b.field_str("name").unwrap_or(""), b.body),
                t if t == decision::DECISION => format!("> decision {}: {} [{}]", b.id, b.field_str("statement").unwrap_or(""), status.clone().unwrap_or_default()),
                _ => b.body.clone(),
            }),
            _ => {
                let mut line = format!("{}{} {}: {}", "  ".repeat(depth), b.id, b.block_type, summary(b));
                if suspects.contains(&b.id) {
                    line.push_str(" [suspect]");
                }
                if let Some(s) = &status {
                    line.push_str(&format!(" [{s}]"));
                }
                lines.push(line);
            }
        }
        for c in m.children(Some(&b.id)).into_iter().rev() {
            stack.push((c, depth + 1));
        }
    }
    let sep = if format == "yaml" { "---\n" } else { "\n" };
    Ok(budget(json!({"id": id.to_string(), "format": format, "text": lines.join(sep)}), p.max_tokens))
}

pub fn rdy_context(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    session(st)?;
    let scope = scope_of(st, &p.scope);
    let ctx = st.rdy.context(&scope, &id_of(&p.id)?)?;
    Ok(budget(serde_json::to_value(ctx).expect("context json"), p.max_tokens))
}

pub fn rdy_impact(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    session(st)?;
    let scope = scope_of(st, &p.scope);
    let items = st.rdy.impact(&scope, &id_of(&p.id)?, p.depth.unwrap_or(3))?;
    Ok(budget(json!({"items": items}), p.max_tokens))
}

pub fn rdy_suspects(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    session(st)?;
    let scope = scope_of(st, &p.scope);
    Ok(budget(json!({"items": st.rdy.suspects(&scope)?}), p.max_tokens))
}

pub fn rdy_schema(st: &mut McpState, p: SchemaParams) -> Result<Value, RdyError> {
    let pack = st.rdy.pack();
    let traits = |ts: &std::collections::BTreeSet<Trait>| ts.iter().map(|t| format!("{t:?}").to_lowercase()).collect::<Vec<_>>();
    match p.block_type {
        Some(t) => {
            let def = pack.block_types.get(&t).ok_or_else(|| RdyError::new(E_BAD_INPUT, format!("unknown block type {t:?}"), "call rdy_schema without block_type to list types"))?;
            let fields: serde_json::Map<String, Value> = def.fields.iter().map(|(k, f)| (k.clone(), json!({"kind": format!("{:?}", f.kind).to_lowercase(), "required": f.required, "values": f.values}))).collect();
            let example_fields: serde_json::Map<String, Value> = def.fields.iter().filter(|(_, f)| f.required).map(|(k, f)| (k.clone(), f.values.first().map(|v| json!(v)).unwrap_or(json!("…")))).collect();
            let mut example = json!({"op": "create", "type": t, "parent": "<section id>", "fields": example_fields});
            if def.body == BodyRule::Required {
                example["body"] = json!("…");
            }
            Ok(json!({"type": t, "prefix": def.prefix, "traits": traits(&def.traits), "body": format!("{:?}", def.body).to_lowercase(), "fields": fields, "children": def.children, "example": example}))
        }
        None => Ok(json!({
            "pack": pack.name,
            "types": pack.block_types.iter().map(|(n, d)| json!({"type": n, "prefix": d.prefix, "traits": traits(&d.traits)})).collect::<Vec<_>>(),
            "link_types": pack.link_types,
            "doc_types": pack.doc_types.iter().map(|(n, d)| json!({"doc_type": n, "skeleton": d.skeleton.iter().map(|s| json!({"title": s.title, "required": s.required})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        })),
    }
}

pub fn rdy_write(st: &mut McpState, p: WriteParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let ops: Vec<OpInput> = p
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, v)| serde_json::from_value(v).map_err(|e| RdyError::new(E_BAD_INPUT, format!("op {i}: {e}"), "each op needs \"op\" (create, update, move, delete); create needs \"type\"; see rdy_schema for fields")))
        .collect::<Result<_, _>>()?;
    let justification = match (p.implements, p.editorial) {
        (Some(d), _) => Some(Justification::Implements(id_of(&d)?)),
        (None, Some(e)) => Some(Justification::Editorial {
            class: match e.class.as_str() {
                "typo" => EditorialClass::Typo,
                "restructure" => EditorialClass::Restructure,
                "clarify" => EditorialClass::Clarify,
                other => return Err(RdyError::new(E_BAD_INPUT, format!("unknown editorial class {other:?}"), "use typo, restructure or clarify")),
            },
            reason: e.reason,
        }),
        (None, None) => None,
    };
    let view = p.view.unwrap_or_else(|| st.view.clone());
    ok(st.rdy.write(&s, WriteRequest { view, ops, justification, message: p.message.unwrap_or_default() })?)
}

fn decision_input(p: DecideParams) -> DecisionInput {
    DecisionInput {
        parent: p.parent,
        statement: p.statement,
        options: p.options.unwrap_or_default(),
        authority_basis: p.authority_basis,
        attested_by: p.attested_by,
        reversibility: p.reversibility,
        kind: p.kind,
        review_by: p.review_by,
        body: p.body.unwrap_or_default(),
        affects: p.affects.unwrap_or_default(),
        depends_on: p.depends_on.unwrap_or_default(),
        conditional_on: p.conditional_on.unwrap_or_default(),
        refines: p.refines.unwrap_or_default(),
        answers: p.answers.unwrap_or_default(),
    }
}

pub fn rdy_decide(st: &mut McpState, p: DecideParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.clone().unwrap_or_else(|| st.view.clone());
    ok(st.rdy.decide(&s, &view, decision_input(p))?)
}

pub fn rdy_supersede(st: &mut McpState, p: SupersedeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.decision.view.clone().unwrap_or_else(|| st.view.clone());
    ok(st.rdy.supersede(&s, &view, &p.old, p.part, decision_input(p.decision))?)
}

pub fn rdy_revoke(st: &mut McpState, p: RevokeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    ok(st.rdy.revoke(&s, &view, &p.target, &p.reason, &p.parent)?)
}

pub fn rdy_propose(st: &mut McpState, p: ProposeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    ok(st.rdy.propose(&s, &view, &p.title)?)
}

pub fn rdy_asset(st: &mut McpState, p: AssetParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    if !s.role.can_write() {
        return Err(RdyError::new(E_PERMISSION, "readers cannot upload assets", "start the session as a writer"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(&p.data_base64).map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "send the file as standard base64"))?;
    let h = st.rdy.store_mut().put_blob(&bytes)?;
    Ok(json!({"hash": h.to_string(), "bytes": bytes.len(), "use": "set a figure block's `blob` field to this hash"}))
}
