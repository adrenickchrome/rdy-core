use base64::Engine as _;
use rdy_engine::engine::{Engine, open_report_json, write_typed};
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Justification, Role};
use rdy_schema::{BodyRule, Trait};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const E_NO_SESSION: &str = "E_NO_SESSION";

/// The most ops one `rdy_write` may carry.
pub const MAX_OPS_PER_WRITE: usize = 5_000;
/// The longest JSON-RPC request line the stdio server passes on.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// The largest asset (decoded bytes) `rdy_asset` accepts.
pub const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;
/// Room left in a request line for the JSON-RPC envelope around an asset's base64.
pub const REQUEST_ENVELOPE_MARGIN: usize = 4096;
/// The largest asset (raw bytes) whose base64 fits in one stdio request line with its envelope;
/// over stdio this, not [`MAX_ASSET_BYTES`], is the effective limit.
pub const MAX_ASSET_BYTES_STDIO: usize = (MAX_REQUEST_BYTES - REQUEST_ENVELOPE_MARGIN) / 4 * 3;
/// The largest `max_tokens` any read may ask for.
pub const MAX_TOKENS_LIMIT: usize = 100_000;

fn limit(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_LIMIT, what, fix)
}

fn check_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
    match max_tokens {
        Some(n) if n > MAX_TOKENS_LIMIT => Err(limit(
            format!("max_tokens {n} is over the limit of {MAX_TOKENS_LIMIT}"),
            format!("ask for at most {MAX_TOKENS_LIMIT} tokens and page through the rest"),
        )),
        _ => Ok(()),
    }
}

pub struct McpState {
    /// The one door to the project: reads from the published state, writes on the writer.
    pub engine: Arc<dyn Engine>,
    pub session: Option<Session>,
    pub view: String,
    /// The daemon's id for this connection (`SessionId.0`); `None` for stdio.
    pub sid: Option<u64>,
}

impl McpState {
    pub fn new(engine: Arc<dyn Engine>) -> McpState {
        McpState {
            engine,
            session: None,
            view: "work".into(),
            sid: None,
        }
    }

    /// This state's daemon session id.
    pub fn with_sid(mut self, sid: u64) -> McpState {
        self.sid = Some(sid);
        self
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
    /// Override the default view for writes. A reader may pass "canon" to read canon by default.
    pub view: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScopeParams {
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdParams {
    /// A block id or a path. rdy_read and rdy_context also take `id@version` or a whole ref line `[id@version] …`, and read under that version.
    pub id: String,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
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
    /// Ops, each a JSON object (rdy_schema lists them under "ops"):
    /// {"op":"create","type":<type>,"id":"$name"?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?,"fields":{..}?,"body":".."?,"links":{..}?}
    /// {"op":"update","id":<id or path>,"base":<version>?,"fields":{..}?,"unset":[<field>]?,"body":".."?,"links":{..}?}
    /// {"op":"move","id":<id or path>,"base":<version>?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}
    /// {"op":"delete","id":<id or path>,"base":<version>?}
    /// {"op":"rebalance","parent":<id or path>?}
    /// {"op":"repin","id":<id or path>,"base":<version>?,"targets":[<id or path>]?}
    /// links: {"<kind>": [{"target": "<id or path>", "part": "<optional words>"}]}, e.g. {"decided_by": [{"target": "D-W2HXR"}]}. An update replaces only the link kinds it names; "<kind>": [] removes that kind.
    /// after puts a block behind a sibling and before ahead of one (before the first child makes it first); give one, or neither to append.
    /// repin re-pins a block's references and links (all, or the targets named) to their current versions, after you've checked them: it clears "changed" suspects.
    /// Ids, parents, afters, befores and link targets also take paths (wallets/transactions); a move without parent goes to the top level (docs and folders); rebalance re-keys a parent's children (frozen decisions stay put; in one write it must come before any other op on those children, and a child it re-keys can't be edited again in the same write).
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
    /// The new decision. It inherits the old decision's affects, depends_on, conditional_on and refines unless you give that kind; an empty list drops it.
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
    serde_json::to_value(e)
        .unwrap_or_else(|_| json!({"code": e.code, "message": e.message, "fix": e.fix}))
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
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let key = obj
        .iter()
        .filter_map(|(k, x)| {
            x.as_array().map(|a| {
                (
                    serde_json::to_string(a).map(|s| s.len()).unwrap_or(0),
                    k.clone(),
                )
            })
        })
        .max()
        .map(|(_, k)| k);
    let Some(key) = key else {
        // no list to shorten: cut the longest string instead (e.g. rdy_read's text)
        let longest = obj
            .iter()
            .filter_map(|(k, x)| x.as_str().map(|s| (s.len(), k.clone())))
            .max()
            .map(|(_, k)| k);
        if let Some(k) = longest {
            let text = obj[&k].as_str().unwrap_or_default().to_string();
            let mut keep = (max * 4).saturating_sub(200).min(text.len());
            loop {
                let cut = (0..=keep)
                    .rev()
                    .find(|i| text.is_char_boundary(*i))
                    .unwrap_or(0);
                obj.insert(k.clone(), json!(text[..cut]));
                obj.insert("truncated".into(), json!(true));
                obj.insert("omitted".into(), json!(text.len() - cut));
                if size(&Value::Object(obj.clone())) <= max || cut == 0 {
                    break;
                }
                keep = cut.saturating_sub((cut / 8).max(1));
            }
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
        if size(&Value::Object(obj.clone())) <= max {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    obj.insert(key, Value::Array(full[..lo].to_vec()));
    obj.insert("truncated".into(), json!(true));
    obj.insert("omitted".into(), json!(full.len() - lo));
    v
}

fn session(st: &McpState) -> Result<Session, RdyError> {
    st.session.clone().ok_or_else(|| {
        RdyError::new(
            E_NO_SESSION,
            "no session",
            "call rdy_session first with your actor, run and role",
        )
    })
}

/// The scope a tool reads, resolved on the state the tool reads (`read`), so that a view created
/// by a later publish is never accepted here and then read on a state without it.
fn scope_of(st: &McpState, read: &ReadState, s: &Option<String>) -> Result<Scope, RdyError> {
    match s {
        // scope1: an unknown view is not found, unless it is this session's own view
        Some(text) => read.resolve_read_scope(text, Some(&st.view)),
        // ses1: a reader session whose view is canon reads canon by default
        None if st.view == "canon" => Ok(Scope::Canon),
        None => Ok(Scope::View(st.view.clone())),
    }
}

/// refv1: the scope and the block id of a read call whose `id` may be `id@version` or a ref line.
/// A pinned version reads under `tag:<version>`; an explicit scope that differs is refused.
fn read_target(
    st: &McpState,
    read: &ReadState,
    id: &str,
    scope: &Option<String>,
) -> Result<(Scope, BlockId), RdyError> {
    let t = rdy_model::reference::parse_target(id)
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::BAD_REF_FIX))?;
    let pinned = t
        .scope_text(scope.as_deref())
        .map_err(|m| RdyError::new(E_BAD_INPUT, m, rdy_model::reference::PIN_CLASH_FIX))?;
    let scope = match pinned {
        Some(text) => read.resolve_read_scope(&text, Some(&st.view))?,
        None => scope_of(st, read, scope)?,
    };
    let bid = read.resolve_id(&scope, &t.id)?;
    Ok((scope, bid))
}

fn id_of(s: &str) -> Result<BlockId, RdyError> {
    BlockId::parse(s)
        .map_err(|e| RdyError::new(E_BAD_INPUT, e.to_string(), "ids look like b-7KQ2M"))
}

const NAME_RULES: &str = "names: 1-64 characters from A-Z a-z 0-9 . _ -; not starting with . or _; not ending .yaml or .yml; not index; not shaped like a block id; not a Windows device name (con, prn, aux, nul, com1-9, lpt1-9); unique among siblings ignoring case. Omit name on create to have one assigned from the title.";

/// Every write op and its JSON shape (`?` marks optional keys). rdy_schema lists them under
/// "ops", and rdy_write's `ops` description repeats them; a QA test keeps the two in step.
pub const WRITE_OPS: &[(&str, &str)] = &[
    (
        "create",
        r#"{"op":"create","type":<type>,"id":"$name"?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?,"fields":{..}?,"body":".."?,"links":{..}?}"#,
    ),
    (
        "update",
        r#"{"op":"update","id":<id or path>,"base":<version>?,"fields":{..}?,"unset":[<field>]?,"body":".."?,"links":{..}?}"#,
    ),
    (
        "move",
        r#"{"op":"move","id":<id or path>,"base":<version>?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}"#,
    ),
    (
        "delete",
        r#"{"op":"delete","id":<id or path>,"base":<version>?}"#,
    ),
    ("rebalance", r#"{"op":"rebalance","parent":<id or path>?}"#),
    (
        "repin",
        r#"{"op":"repin","id":<id or path>,"base":<version>?,"targets":[<id or path>]?}"#,
    ),
];

/// doc1: the shape of a block's `links`.
pub const LINK_SHAPE: &str = r#"links: {"<kind>": [{"target": "<id or path>", "part": "<optional: the words of the target this link is about>"}]}"#;
pub const LINK_EXAMPLE: &str =
    r#"{"decided_by": [{"target": "D-W2HXR"}], "describes": [{"target": "wallets/transactions"}]}"#;

/// link1: how an update treats links.
pub const LINK_UPDATE: &str = r#"an update replaces only the link kinds it names; "<kind>": [] removes that kind; kinds it doesn't name are kept"#;

fn link_example() -> Value {
    serde_json::from_str(LINK_EXAMPLE).expect("LINK_EXAMPLE is JSON")
}

/// Whether `v` has the shape {"<kind>": [{"target": "<id or path>", ...}]}.
fn links_shape_ok(v: &Value) -> bool {
    v.as_object().is_some_and(|kinds| {
        kinds.values().all(|l| {
            l.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|x| x.get("target").is_some_and(Value::is_string))
            })
        })
    })
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
    st.view = view;
    st.session = Some(session);
    Ok(json!({ "view": st.view, "role": p.role }))
}

pub fn rdy_overview(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(st, &st_, &p.scope)?;
    let position = st_.version_position(&scope)?;
    let max = p.max_tokens.unwrap_or(4000);
    let tree = fit_tree(st_.tree(&scope, None, None)?, max.saturating_sub(300));
    let views: Vec<Value> = st_
        .view_names()
        .into_iter()
        .map(|name| {
            let n = st_.included(&name).len();
            json!({"name": name, "changes": n})
        })
        .collect();
    let landings = st_.landings()?.len();
    Ok(budget(
        json!({
            "pack": st_.pack().name, "session": {"actor": s.actor, "view": st.view},
            "canon": {"landings": landings},
            "open_report": open_report_json(&st.engine.open_report()),
            "views": views, "tree": tree, "version_position": position,
        }),
        p.max_tokens,
    ))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TagsParams {}

pub fn rdy_tags(st: &mut McpState, _p: TagsParams) -> Result<Value, RdyError> {
    session(st)?;
    let st_ = st.engine.read();
    let versions = st_.versions()?;
    let current = versions.last().map(|v| v.name.clone());
    Ok(json!({"versions": versions, "current": current}))
}

pub fn rdy_read(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let (scope, id) = read_target(st, &st_, &p.id, &p.scope)?;
    let suspects: std::collections::BTreeSet<BlockId> =
        st_.suspects(&scope)?.into_iter().map(|s| s.block).collect();
    let m = st_.materialized(&scope)?;
    let g = st_.graph(&scope)?;
    let root = m.get(&id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id}"),
            "list docs with rdy_overview",
        )
    })?;
    let format = p.format.as_deref().unwrap_or("compact");
    if !["compact", "markdown", "yaml"].contains(&format) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown format {format:?}"),
            "use compact, markdown or yaml",
        ));
    }
    let mut lines = vec![];
    let mut stack = vec![(root, 0usize)];
    while let Some((b, depth)) = stack.pop() {
        let status = g.status(&b.id).map(|s| {
            serde_json::to_value(s)
                .ok()
                .and_then(|v| v["status"].as_str().map(String::from))
                .unwrap_or_default()
        });
        match format {
            "yaml" => lines.push(block_to_yaml(b)),
            "markdown" => lines.push(crate::markdown::block_markdown(
                b,
                &m,
                crate::markdown::doc_depth(&m, b),
                status.as_deref(),
            )),
            _ => {
                let mut line = format!(
                    "{}{} {}: {}",
                    "  ".repeat(depth),
                    b.id,
                    b.block_type,
                    compact_summary(b)
                );
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
    let sep = match format {
        "yaml" => "---\n",
        "markdown" => "\n\n",
        _ => "\n",
    };
    Ok(budget(
        json!({"id": id.to_string(), "format": format, "text": lines.join(sep)}),
        p.max_tokens,
    ))
}

pub fn rdy_context(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let (scope, id) = read_target(st, &st_, &p.id, &p.scope)?;
    let ctx = st_.context(&scope, &id)?;
    Ok(budget(
        serde_json::to_value(ctx).expect("context json"),
        p.max_tokens,
    ))
}

pub fn rdy_impact(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(st, &st_, &p.scope)?;
    let id = st_.resolve_id(&scope, &p.id)?;
    let items = st_.impact(&scope, &id, p.depth.unwrap_or(3))?;
    Ok(budget(json!({"items": items}), p.max_tokens))
}

pub fn rdy_suspects(st: &mut McpState, p: ScopeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(st, &st_, &p.scope)?;
    Ok(budget(
        json!({"items": st_.suspects(&scope)?}),
        p.max_tokens,
    ))
}

/// What bodies and text fields accept (md1, dtext1, key1).
const TEXT_RULES: &str = "Bodies and text fields: **bold**, *italic*, `code`, [link](https://… or a relative path), and in bodies - bullets and 1. numbered lists (indent two spaces to nest). No raw HTML. {{id}} or {{id|words}} references a block; \\{{ is a literal {{ and a leading @ in words is written \\@. fields.key: optional short human id, unique per project ignoring case, letters/digits/-_. , not shaped like a block id.";

pub fn rdy_schema(st: &mut McpState, p: SchemaParams) -> Result<Value, RdyError> {
    let st_ = st.engine.read();
    let pack = st_.pack();
    let traits = |ts: &std::collections::BTreeSet<Trait>| {
        ts.iter()
            .map(|t| format!("{t:?}").to_lowercase())
            .collect::<Vec<_>>()
    };
    match p.block_type {
        Some(t) => {
            let def = pack.block_types.get(&t).ok_or_else(|| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("unknown block type {t:?}"),
                    "call rdy_schema without block_type to list types",
                )
            })?;
            let fields: serde_json::Map<String, Value> = def.fields.iter().map(|(k, f)| (k.clone(), json!({"kind": format!("{:?}", f.kind).to_lowercase(), "required": f.required, "values": f.values}))).collect();
            let example_fields: serde_json::Map<String, Value> = def
                .fields
                .iter()
                .filter(|(_, f)| f.required)
                .map(|(k, f)| {
                    (
                        k.clone(),
                        f.values.first().map(|v| json!(v)).unwrap_or(json!("…")),
                    )
                })
                .collect();
            let mut example = json!({"op": "create", "type": t, "parent": "<section id>", "fields": example_fields});
            if def.body == BodyRule::Required {
                example["body"] = json!("…");
            }
            let mut out = json!({"type": t, "prefix": def.prefix, "traits": traits(&def.traits), "body": format!("{:?}", def.body).to_lowercase(), "fields": fields, "children": def.children, "example": example});
            out["links"] = json!({"shape": LINK_SHAPE, "example": link_example(), "update": LINK_UPDATE, "kinds": pack.link_types});
            if rdy_model::tree::is_tree_type(&t) {
                out["example"]["parent"] = json!("<folder id or path; omit for the top level>");
                out["name_rules"] = json!(NAME_RULES);
            }
            Ok(out)
        }
        None => Ok(json!({
            "pack": pack.name,
            "types": pack.block_types.iter().map(|(n, d)| json!({"type": n, "prefix": d.prefix, "traits": traits(&d.traits)})).collect::<Vec<_>>(),
            "link_types": pack.link_types,
            "link_shape": {"shape": LINK_SHAPE, "example": link_example(), "update": LINK_UPDATE},
            "ops": WRITE_OPS.iter().map(|(n, s)| (n.to_string(), json!(s))).collect::<serde_json::Map<_, _>>(),
            "limits": {"ops_per_write": MAX_OPS_PER_WRITE, "request_bytes": MAX_REQUEST_BYTES, "asset_bytes": MAX_ASSET_BYTES, "asset_bytes_stdio": MAX_ASSET_BYTES_STDIO, "max_tokens": MAX_TOKENS_LIMIT, "socket_line_bytes": MAX_REQUEST_BYTES},
            "errors": {
                "E_VIEW_OWNER": {"when": "a non-human session commits into a view it doesn't own", "fix": "write to your own view, fork it, or include its changes into your own view"},
                "E_DAEMON_START": {"when": "the daemon couldn't be started or reached", "fix": "read the last lines of .rdy/run/daemon.log it cites; run rdy serve in the foreground to see why"},
                "E_DAEMON_VERSION": {"when": "another binary version's daemon has clients", "fix": "close them, or rdy serve --stop"},
                "E_DAEMON_GONE": {"when": "the daemon closed mid-request", "fix": "check rdy_overview or rdy status; a write may or may not have committed; it is not retried"},
            },
            "doc_types": pack.doc_types.iter().map(|(n, d)| json!({"doc_type": n, "skeleton": d.skeleton.iter().map(|s| json!({"title": s.title, "required": s.required})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "text": TEXT_RULES,
        })),
    }
}

pub fn rdy_write(st: &mut McpState, p: WriteParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    if p.ops.len() > MAX_OPS_PER_WRITE {
        return Err(limit(
            format!(
                "{} ops in one write; the limit is {MAX_OPS_PER_WRITE}",
                p.ops.len()
            ),
            "split the write into batches",
        ));
    }
    let ops: Vec<OpInput> = p
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value(v.clone()).map_err(|e| {
                // doc1: a malformed links value gets the shape, not the generic hint
                let fix = if v.get("links").is_some_and(|l| !links_shape_ok(l)) {
                    format!("write {LINK_SHAPE}, e.g. {LINK_EXAMPLE}")
                } else {
                    "each op needs \"op\" (rdy_schema lists them under \"ops\"); create needs \"type\"; see rdy_schema for fields".to_string()
                };
                RdyError::new(E_BAD_INPUT, format!("op {i}: {e}"), fix)
            })
        })
        .collect::<Result<_, _>>()?;
    let justification = match (p.implements, p.editorial) {
        (Some(_), Some(_)) => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "give either implements or editorial, not both",
                "implements for decision-backed changes, editorial for typo/restructure/clarify",
            ));
        }
        (Some(d), None) => Some(Justification::Implements(id_of(&d)?)),
        (None, Some(e)) => Some(Justification::Editorial {
            class: match e.class.as_str() {
                "typo" => EditorialClass::Typo,
                "restructure" => EditorialClass::Restructure,
                "clarify" => EditorialClass::Clarify,
                other => {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("unknown editorial class {other:?}"),
                        "use typo, restructure or clarify",
                    ));
                }
            },
            reason: e.reason,
        }),
        (None, None) => None,
    };
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let req = WriteRequest {
        view,
        ops,
        justification,
        message: p.message.unwrap_or_default(),
    };
    write_typed(&*st.engine, move |r| r.write(&s, req))
}

fn decision_input(p: DecideParams) -> DecisionInput {
    // sup1: a kind given as an empty list is not inherited by rdy_supersede
    let no_inherit = [
        ("affects", &p.affects),
        ("depends_on", &p.depends_on),
        ("conditional_on", &p.conditional_on),
        ("refines", &p.refines),
    ]
    .into_iter()
    .filter(|(_, v)| v.as_ref().is_some_and(Vec::is_empty))
    .map(|(k, _)| k.to_string())
    .collect();
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
        no_inherit,
    }
}

pub fn rdy_decide(st: &mut McpState, p: DecideParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.clone().unwrap_or_else(|| st.view.clone());
    let input = decision_input(p);
    write_typed(&*st.engine, move |r| r.decide(&s, &view, input))
}

pub fn rdy_supersede(st: &mut McpState, p: SupersedeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.decision.view.clone().unwrap_or_else(|| st.view.clone());
    let (old, part, input) = (p.old, p.part, decision_input(p.decision));
    write_typed(&*st.engine, move |r| {
        r.supersede(&s, &view, &old, part, input)
    })
}

pub fn rdy_revoke(st: &mut McpState, p: RevokeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    write_typed(&*st.engine, move |r| {
        r.revoke(&s, &view, &p.target, &p.reason, &p.parent)
    })
}

pub fn rdy_propose(st: &mut McpState, p: ProposeParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    let view = p
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v))
        .unwrap_or_else(|| st.view.clone());
    write_typed(&*st.engine, move |r| r.propose(&s, &view, &p.title))
}

pub fn rdy_asset(st: &mut McpState, p: AssetParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            "readers cannot upload assets",
            "start the session as a writer",
        ));
    }
    let too_big = || {
        limit(
            format!("the asset is over the limit of {MAX_ASSET_BYTES} bytes"),
            format!("upload assets of at most {MAX_ASSET_BYTES} bytes"),
        )
    };
    if p.data_base64.len() > MAX_ASSET_BYTES / 3 * 4 + 4 {
        return Err(too_big());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&p.data_base64)
        .map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                e.to_string(),
                "send the file as standard base64",
            )
        })?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(too_big());
    }
    let n = bytes.len();
    let h = st.engine.write(Box::new(move |r| {
        Ok(json!(r.store_mut().put_blob(&bytes)?.to_string()))
    }))?;
    Ok(json!({"hash": h, "bytes": n, "use": "set a figure block's `blob` field to this hash"}))
}
