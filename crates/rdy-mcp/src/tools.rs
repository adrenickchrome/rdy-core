use base64::Engine as _;
use rdy_engine::code_brief::e_code_path_alone;
use rdy_engine::consequence::{propose_and_report, write_and_report};
use rdy_engine::engine::{Engine, open_report_json};
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Justification, Role};
use rdy_schema::{BodyRule, Trait};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::view_tools;

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
/// `rdy_schema`'s note on the request-line limit, the same on both transports.
pub const TRANSPORT_NOTE: &str = "one request is one JSON line of at most 16 MiB (request_bytes); the daemon's socket carries the same line limit as stdio (socket_line_bytes), so asset_bytes_stdio, the largest asset whose base64 fits in one line, binds on both";
/// The largest `max_tokens` any read may ask for.
pub const MAX_TOKENS_LIMIT: usize = 100_000;

fn limit(what: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_LIMIT, what, fix)
}

pub(crate) fn check_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
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
    /// The pack generation this session has heard of (its `pack_changed` notice).
    pub seen_gen: u64,
}

impl McpState {
    pub fn new(engine: Arc<dyn Engine>) -> McpState {
        let seen_gen = engine.pack_generation();
        McpState {
            engine,
            session: None,
            view: "work".into(),
            sid: None,
            seen_gen,
        }
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

/// llst1: one text, or a list of them.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
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
    /// links: {"<kind>": <target> | [<target>, …] | [{"target": "<id, key or path>", "part": "<optional words>"}]}, e.g. {"decided_by": [{"target": "D-W2HXR"}]}. An update replaces only the link kinds it names; "<kind>": [] removes that kind.
    /// after puts a block behind a sibling and before ahead of one (before the first child makes it first); give one, or neither to append.
    /// repin re-pins a block's references and links (all, or the targets named) to their current versions, after you've checked them: it clears "changed" suspects.
    /// Ids, parents, afters, befores and link targets also take paths (wallets/transactions); a move without parent goes to the top level (docs and folders); rebalance re-keys a parent's children (frozen decisions stay put; in one write it must come before any other op on those children, and a child it re-keys can't be edited again in the same write).
    pub ops: Vec<Value>,
    /// The decisions this change carries out: an id, key or path, or a list of them.
    pub implements: Option<OneOrMany>,
    pub editorial: Option<EditorialParams>,
    pub message: Option<String>,
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
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
    /// When the decision takes effect (RFC 3339); defaults to when it is recorded.
    pub effective_from: Option<String>,
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
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
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BriefParams {
    /// Blocks to brief on (ids, keys or paths).
    pub ids: Option<Vec<String>>,
    /// A doc or folder path: brief on every block under it.
    pub path: Option<String>,
    /// A file in a linked code repo (relative to its top folder): brief on the design blocks its commits implement (Implements: trailers) and the blocks whose reflects globs match it. Not with ids or path.
    pub code_path: Option<String>,
    /// The view to brief in; defaults to the session's view.
    pub scope: Option<String>,
    /// The budget for the briefing; the longest lists are cut to fit.
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeParams {
    pub view: Option<String>,
    pub title: String,
    /// Propose only these changes (ids from rdy_view show). The set must be closed under dependencies, or it is E_DEPS_MISSING. Omit to propose all the view's changes. A new version of an open proposal replaces the old one.
    pub changes: Option<Vec<String>>,
    /// Your plain-words summary of the proposal, for the person who approves it. Stored with the version; a new version without it keeps the last one.
    pub summary: Option<String>,
    /// The budget for the consequences in the result.
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssetParams {
    pub data_base64: String,
}

pub fn error_json(e: &RdyError) -> Value {
    serde_json::to_value(e)
        .unwrap_or_else(|_| json!({"code": e.code, "message": e.message, "fix": e.fix}))
}

/// How many versions `rdy_context` carries of a block's history.
pub const CONTEXT_HISTORY: usize = 20;

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

pub(crate) fn session(st: &McpState) -> Result<Session, RdyError> {
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
pub const LINK_SHAPE: &str = r#"links: {"<kind>": "<id, key or path>" | ["<target>", …] | [{"target": "<id, key or path>", "part": "<optional: the words of the target this link is about>"}]}"#;
pub const LINK_EXAMPLE: &str =
    r#"{"decided_by": [{"target": "D-W2HXR"}], "describes": [{"target": "wallets/transactions"}]}"#;

/// link1: how an update treats links.
pub const LINK_UPDATE: &str = r#"an update replaces only the link kinds it names; "<kind>": [] removes that kind; kinds it doesn't name are kept"#;

fn link_example() -> Value {
    serde_json::from_str(LINK_EXAMPLE).expect("LINK_EXAMPLE is JSON")
}

/// Whether `v` has the shape of `LINK_SHAPE`: each kind is a string, or a list of strings and
/// `{"target": <string>, ...}` objects.
fn links_shape_ok(v: &Value) -> bool {
    let entry = |x: &Value| x.is_string() || x.get("target").is_some_and(Value::is_string);
    v.as_object().is_some_and(|kinds| {
        kinds
            .values()
            .all(|l| l.is_string() || l.as_array().is_some_and(|items| items.iter().all(entry)))
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
            "project": {"name": st_.project_name(), "description": st_.project_description()},
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
    // the scope's recorded conflicts by block, for the marker on each conflicted block's line
    let conflicts: std::collections::BTreeMap<BlockId, String> = st_
        .conflicts(&scope)?
        .iter()
        .map(|c| (c.block.clone(), c.id.short()))
        .collect();
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
    // compact lines mark the justification of the change that last wrote each block; a damaged
    // producer index (status and fsck report it) costs the markers and a warning, not the read
    let mut warnings = vec![];
    let justified: std::collections::BTreeMap<BlockId, String> = if format == "compact" {
        let mut ids = vec![];
        let mut walk = vec![root];
        while let Some(b) = walk.pop() {
            ids.push(b.id.clone());
            walk.extend(m.children(Some(&b.id)));
        }
        match st_.last_changes(&scope, &ids) {
            Ok(last) => last
                .into_iter()
                .filter_map(|(id, e)| e.justification.map(|j| (id, j.label())))
                .collect(),
            Err(e) => {
                warnings.push(json!({"code": e.code, "message": e.message, "fix": e.fix}));
                Default::default()
            }
        }
    } else {
        Default::default()
    };
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
                if let Some(c) = conflicts.get(&b.id) {
                    line.push_str(&format!(" [conflict {c}]"));
                }
                if let Some(s) = &status {
                    line.push_str(&format!(" [{s}]"));
                }
                if let Some(j) = justified.get(&b.id) {
                    line.push_str(&format!(" [{j}]"));
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
    let mut out = json!({"id": id.to_string(), "format": format, "text": lines.join(sep)});
    if let Some(c) = st_.conflict_info(&scope, &id)? {
        out["conflict"] = json!(c);
    }
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(budget(out, p.max_tokens))
}

pub fn rdy_context(st: &mut McpState, p: IdParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let (scope, id) = read_target(st, &st_, &p.id, &p.scope)?;
    let ctx = st_.context(&scope, &id)?;
    let mut v = serde_json::to_value(ctx).expect("context json");
    if let Some(c) = st_.conflict_info(&scope, &id)? {
        v["conflict"] = json!(c);
    }
    let h = st_.history(&scope, &id, CONTEXT_HISTORY)?;
    v["justification"] = json!(h.entries.first().and_then(|e| e.justification.clone()));
    v["history_more"] = json!(h.more);
    v["history"] = json!(h.entries);
    Ok(budget(v, p.max_tokens))
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
const TEXT_RULES: &str = "Bodies and text fields: **bold**, *italic*, `code`, [link](https://… or a relative path), and in bodies - bullets and 1. numbered lists (indent two spaces to nest). No raw HTML. {{id}} or {{id|words}} references a block; \\{{ is a literal {{ and a leading @ in words is written \\@. fields.key: optional short human id, unique per project ignoring case, letters/digits/-_. , not shaped like a block id. fields.reflects: optional list of code path globs (e.g. crates/*/src/error.rs) this block follows; rdy_brief {code_path} lists the blocks whose globs match a file.";

/// How to edit with briefings, in `rdy_schema`.
pub const EDIT_LOOP_DOC: &str = "Brief before editing: rdy_brief {ids or path} returns governs, dependents, will_go_stale, suspects and base (each covered block's version and the briefing token). Pass brief: <token> on rdy_write, rdy_decide, rdy_supersede (inside decision) and rdy_revoke. A write that touches stable content (a block whose own or inherited lifecycle is stable before or after the op; for a create, its parent's) needs a current briefing that covers it: brief the block, or its parent to create under it. A briefing is current while no block it recorded has moved in the write's view. Drafts need none; a stale one there only warns (W_BRIEF_STALE). E_BRIEF_REQUIRED and E_BRIEF_STALE carry a fresh briefing for exactly the blocks the write touches: retry with its base.token. Every write response lists consequences: {id, block, kind (suspect or dependent), because {edge, changed}, follow_up (update, confirm, reaffirm or resolve)}; max_tokens bounds them.";

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
            "views": view_tools::SCHEMA_DOC,
            "limits": {"ops_per_write": MAX_OPS_PER_WRITE, "request_bytes": MAX_REQUEST_BYTES, "asset_bytes": MAX_ASSET_BYTES, "asset_bytes_stdio": MAX_ASSET_BYTES_STDIO, "max_tokens": MAX_TOKENS_LIMIT, "socket_line_bytes": MAX_REQUEST_BYTES, "transport": TRANSPORT_NOTE},
            "errors": {
                "E_VIEW_OWNER": {"when": "a non-human session commits into a view it doesn't own", "fix": "write to your own view, fork it, or include its changes into your own view"},
                "E_VIEW_EXISTS": {"when": "a view of that name exists", "fix": "pick a free name, or fork the view under another name"},
                "E_VIEW_CLOSED": {"when": "the view is closed and takes no write, include or exclude", "fix": "reopen it with rdy_view {op: \"undo\", view, target: <the close op>}, or fork it"},
                "E_HAS_DEPENDENTS": {"when": "changes in the view depend on a change you exclude (each is an issue)", "fix": "exclude them too, or pass cascade: true"},
                "E_DEPS_MISSING": {"when": "a proposal subset leaves out changes its members depend on (each is an issue)", "fix": "add the missing changes to rdy_propose's changes"},
                "E_CONFLICT_UNRESOLVED": {"when": "the proposal version carries a conflict it does not resolve (each is an issue)", "fix": "write the block from the version you want, then propose again"},
                "E_PROPOSAL_MOVED": {"when": "a newer version exists, or a landing since the version changed blocks it writes, or the project requires proposals on the current canon", "fix": "propose again; a human approves the latest P-x@n"},
                "E_UNDO_BLOCKED": {"when": "an undo would leave the view inconsistent (each blocking op is an issue)", "fix": "undo with cascade: true, or undo the dependent ops first"},
                "E_UNDO_LANDING": {"when": "an undo would bring back or remove work that has landed", "fix": "a landing is undone by a revoking decision (rdy_revoke)"},
                "E_NOTHING_TO_UNDO": {"when": "you have no undoable op of your own on the view", "fix": "name an op with target (rdy_view show lists them)"},
                "E_DAEMON_START": {"when": "the daemon couldn't be started or reached", "fix": "read the last lines of .rdy/run/daemon.log it cites; run rdy serve in the foreground to see why"},
                "E_DAEMON_VERSION": {"when": "another binary version's daemon has clients", "fix": "close them, or rdy serve --stop"},
                "E_DAEMON_GONE": {"when": "the daemon closed mid-request", "fix": "check rdy_overview or rdy status; a write may or may not have committed; it is not retried"},
                "E_BRIEF_REQUIRED": {"when": "a write touches stable content and cites no briefing, or one that does not cover the block", "fix": "retry with the token of the briefing the error carries"},
                "E_BRIEF_STALE": {"when": "the cited briefing is unknown, or a block it recorded moved in the write's view", "fix": "read what moved, then retry with the token of the briefing the error carries"},
                "W_BRIEF_STALE": {"when": "a write to drafts cited a stale or unknown briefing (it went ahead)", "fix": "brief again before the next write if the moved blocks matter"},
            },
            "doc_types": pack.doc_types.iter().map(|(n, d)| json!({"doc_type": n, "skeleton": d.skeleton.iter().map(|s| json!({"title": s.title, "required": s.required})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "text": TEXT_RULES,
            "edit_loop": EDIT_LOOP_DOC,
        })),
    }
}

pub fn rdy_brief(st: &mut McpState, p: BriefParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(st, &st_, &p.scope)?;
    if let Some(cp) = &p.code_path {
        if p.ids.is_some() || p.path.is_some() {
            return Err(e_code_path_alone());
        }
        let b = st_.brief_code(&scope, cp, None, p.max_tokens)?;
        b.briefing.register(&st.engine.briefs());
        return serialize_briefing(&b);
    }
    let b = st_.brief(
        &scope,
        &Coverage {
            ids: p.ids.unwrap_or_default(),
            path: p.path,
        },
        p.max_tokens,
    )?;
    b.register(&st.engine.briefs());
    serialize_briefing(&b)
}

fn serialize_briefing<T: serde::Serialize>(b: &T) -> Result<Value, RdyError> {
    serde_json::to_value(b).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a briefing did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

pub fn rdy_write(st: &mut McpState, p: WriteParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
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
        (Some(d), None) => {
            let texts = match d {
                OneOrMany::One(t) => vec![t],
                OneOrMany::Many(v) => v,
            };
            if texts.is_empty() {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "implements is empty",
                    "name the decision this change carries out, or leave implements out",
                ));
            }
            let scope = Scope::View(p.view.clone().unwrap_or_else(|| st.view.clone()));
            let read = st.engine.read();
            let mut ids: Vec<BlockId> = vec![];
            for t in &texts {
                let id = read.resolve_id(&scope, t)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            Some(Justification::Implements(ids))
        }
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
    let (brief, max_tokens, view) = (p.brief, p.max_tokens, req.view.clone());
    write_and_report(&*st.engine, &view, max_tokens, move |r| {
        r.write_briefed(&s, req, brief)
    })
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
        effective_from: p.effective_from,
        body: p.body.unwrap_or_default(),
        affects: p.affects.unwrap_or_default(),
        depends_on: p.depends_on.unwrap_or_default(),
        conditional_on: p.conditional_on.unwrap_or_default(),
        refines: p.refines.unwrap_or_default(),
        answers: p.answers.unwrap_or_default(),
        no_inherit,
        brief: p.brief,
    }
}

pub fn rdy_decide(st: &mut McpState, p: DecideParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p.view.clone().unwrap_or_else(|| st.view.clone());
    let max_tokens = p.max_tokens;
    let input = decision_input(p);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        r.decide(&s, &view, input)
    })
}

pub fn rdy_supersede(st: &mut McpState, p: SupersedeParams) -> Result<Value, RdyError> {
    check_tokens(p.decision.max_tokens)?;
    let s = session(st)?;
    let view = p.decision.view.clone().unwrap_or_else(|| st.view.clone());
    let max_tokens = p.decision.max_tokens;
    let (old, part, input) = (p.old, p.part, decision_input(p.decision));
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        r.supersede(&s, &view, &old, part, input)
    })
}

pub fn rdy_revoke(st: &mut McpState, p: RevokeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let (brief, max_tokens) = (p.brief, p.max_tokens);
    let v = view.clone();
    write_and_report(&*st.engine, &v, max_tokens, move |r| {
        r.revoke(&s, &view, &p.target, &p.reason, &p.parent, brief)
    })
}

pub fn rdy_propose(st: &mut McpState, p: ProposeParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let view = p
        .view
        .map(|v| v.strip_prefix("view:").map(String::from).unwrap_or(v))
        .unwrap_or_else(|| st.view.clone());
    let changes = p.changes.clone();
    propose_and_report(&*st.engine, p.max_tokens, move |r| {
        r.propose_subset(
            &s,
            &view,
            &p.title,
            changes.as_deref(),
            p.summary.as_deref(),
        )
    })
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
