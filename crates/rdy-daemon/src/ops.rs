//! The ctl ops: one function per op, each against `&dyn Engine`. Every read op takes one
//! snapshot (`engine.read()`) and reads only through it; none touches the writer. Every write
//! op parses and checks its role here, then runs its engine call as one `Engine::write`
//! closure on the writer. Relative paths in args resolve against the session's working
//! directory.
use rdy_engine::brief::Coverage;
use rdy_engine::brief::e_page_alone;
use rdy_engine::code_brief::e_code_path_alone;
use rdy_engine::consequence::{land_and_report, propose_and_report, write_and_report};
use rdy_engine::engine::{Engine, open_report_json, write_typed};
use rdy_engine::search::SearchFilters;
use rdy_engine::{
    E_BAD_INPUT, E_IO, E_LIMIT, E_NOT_FOUND, E_PERMISSION, HISTORY_MAX, LandOptions, OpInput,
    RdyError, ReadState, Relay, Scope, SearchQuery, Session, WriteRequest, block_to_yaml,
    check_path, parse_justification, resolve_pack, view_api,
};
use rdy_mcp::tools::MAX_TOKENS_LIMIT;
use rdy_model::{ActorRun, ChangeLinks, RelayEvidence, Role};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What an op runs against: the engine, the calling session and its working directory.
pub struct OpContext<'a> {
    pub engine: &'a dyn Engine,
    pub session: &'a Session,
    pub cwd: &'a Path,
}

/// Every ctl op, in the order the docs list them. `session` is answered by the connection
/// host.
pub const OPS: &[&str] = &[
    "session",
    "status",
    "show",
    "ls",
    "log",
    "impact",
    "suspects",
    "tags",
    "block",
    "brief",
    "search",
    "render",
    "export",
    "check",
    "new_folder",
    "new_doc",
    "keys_assign",
    "edit_block",
    "propose",
    "approve",
    "tag",
    "import",
    "reindex",
    "fsck",
    "view",
    "wc_view",
    "wc_commit",
    "wc_nonce",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogArgs {
    limit: Option<usize>,
    since: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FsckArgs {
    #[serde(default)]
    clean_quarantine: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    id: String,
    scope: Option<String>,
    #[serde(default)]
    blame: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LsArgs {
    path: Option<String>,
    scope: Option<String>,
    depth: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImpactArgs {
    id: String,
    depth: Option<u32>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeArgs {
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockArgs {
    id: String,
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefArgs {
    ids: Option<Vec<String>>,
    path: Option<String>,
    code_path: Option<String>,
    /// Continue a cut briefing: its `next` cursor. Alone (scope and max_tokens allowed).
    page: Option<String>,
    scope: Option<String>,
    /// A write target, resolved as `edit_block` writes it: this view when it has a head, else
    /// canon.
    view: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    regex: Option<bool>,
    ignore_case: Option<bool>,
    scope: Option<String>,
    max_tokens: Option<usize>,
    page: Option<String>,
    types: Option<Vec<String>>,
    traits: Option<Vec<String>>,
    path: Option<String>,
    lifecycle: Option<String>,
    changed_since: Option<String>,
    exclude_history: Option<bool>,
    governed_by: Option<String>,
    describes: Option<String>,
    linked_to: Option<String>,
    suspect: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutArgs {
    out: String,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    path: String,
    pack: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewFolderArgs {
    path: String,
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDocArgs {
    path: String,
    view: Option<String>,
    doc_type: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysAssignArgs {
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditBlockArgs {
    yaml: String,
    view: Option<String>,
    brief: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeArgs {
    title: String,
    view: Option<String>,
    summary: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveArgs {
    proposal: String,
    relayed_by: Option<String>,
    evidence: Option<String>,
    question: Option<String>,
    session_ref: Option<String>,
    strict: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagArgs {
    version: String,
    landing: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WcViewArgs {
    view: Option<String>,
    ids: Option<Vec<String>>,
    blobs_dir: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WcCommitArgs {
    view: String,
    ops: Vec<Value>,
    nonce: String,
    rewrite: Vec<String>,
    message: Option<String>,
    implements: Option<Vec<String>>,
    editorial: Option<EditorialArg>,
    brief: Option<String>,
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorialArg {
    class: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WcNonceArgs {
    view: String,
    nonce: String,
    rewrite: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportArgs {
    dir: String,
    view: String,
}

/// Why an agent session is refused an approver op.
pub(crate) const AGENT_NOT_APPROVER: &str = "agent sessions cannot approve, tag or import";
/// How the refusal is fixed.
pub(crate) const AGENT_NOT_APPROVER_FIX: &str = "approvals are made by the human actor: unset RDY_ACTOR, or set actor: in the user's rdy config";

/// The session an approver op runs as: a human session as an approver; an agent session is
/// refused with `E_PERMISSION`.
pub(crate) fn approver(s: &Session) -> Result<Session, RdyError> {
    if s.is_human() {
        Ok(Session {
            role: Role::Approver,
            ..s.clone()
        })
    } else {
        Err(RdyError::new(
            E_PERMISSION,
            AGENT_NOT_APPROVER,
            AGENT_NOT_APPROVER_FIX,
        ))
    }
}

/// The view a write op targets: `view` without a leading `view:`, else the session's default.
pub(crate) fn view_of(s: &Session, view: Option<String>) -> String {
    match view {
        Some(v) => v.strip_prefix("view:").map(String::from).unwrap_or(v),
        None => s.default_view(),
    }
}

/// Deserializes `args` as `T`; a failure is `E_BAD_INPUT` naming the op and its shape.
fn parse<T: serde::de::DeserializeOwned>(
    op: &str,
    args: Value,
    shape: &str,
) -> Result<T, RdyError> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{op}: {e}"),
            format!("args for {op}: {shape}"),
        )
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("a reply serializes")
}

/// `p` as given when absolute, else under `cwd`.
pub(crate) fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The scope a read op names; reads default to canon, as the CLI does.
pub(crate) fn scope_of(
    st: &ReadState,
    s: &OpContext,
    text: Option<&str>,
) -> Result<Scope, RdyError> {
    match text {
        Some(t) => st.resolve_read_scope(t, Some(&s.session.default_view())),
        None => Ok(Scope::Canon),
    }
}

/// The quarantine files an open left behind: torn segment tails (`segments/<name>`) and
/// damaged indexes, sorted. A missing directory contributes nothing.
pub fn quarantine(root: &Path) -> Vec<String> {
    let store = root.join(".rdy/store");
    let names = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out: Vec<String> = names(&store.join("segments"))
        .into_iter()
        .filter(|n| n.contains(".torn-"))
        .map(|n| format!("segments/{n}"))
        .collect();
    out.extend(
        names(&store)
            .into_iter()
            .filter(|n| n.starts_with("index.redb.damaged-")),
    );
    out.sort();
    out
}

/// What `clean_quarantine` did: the names it removed and the ones it left, with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// Removes the quarantine files `quarantine(root)` names, in its order. Only regular files are
/// removed: a symlink or a directory carrying a quarantine name is skipped untouched (never
/// followed, never recursed), and an error leaves the file in `skipped` with the error text.
pub fn clean_quarantine(root: &Path) -> Cleaned {
    let store = root.join(".rdy/store");
    let mut c = Cleaned::default();
    for name in quarantine(root) {
        let path = store.join(&name);
        let result = std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if m.file_type().is_file() {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())
                } else {
                    Err("not a regular file".to_string())
                }
            });
        match result {
            Ok(()) => c.removed.push(name),
            Err(reason) => c.skipped.push((name, reason)),
        }
    }
    c
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

/// Runs the ctl op `op` with `args`. An unknown op is `E_BAD_INPUT` naming every op.
pub fn dispatch(cx: &OpContext, op: &str, args: Value) -> Result<Value, RdyError> {
    match op {
        "session" => Err(RdyError::new(
            E_BAD_INPUT,
            "the session op is answered by the daemon connection, not by dispatch",
            "send it once, first, on a ctl connection",
        )),
        "status" => {
            parse::<NoArgs>(op, args, "{}")?;
            status(cx)
        }
        "show" => show(cx, parse(op, args, "{id, scope?, blame?}")?),
        "ls" => ls(cx, parse(op, args, "{path?, scope?, depth?}")?),
        "log" => log(cx, parse(op, args, "{limit?, since?}")?),
        "impact" => impact(cx, parse(op, args, "{id, depth?, scope?}")?),
        "suspects" => suspects(cx, parse(op, args, "{scope?}")?),
        "tags" => {
            parse::<NoArgs>(op, args, "{}")?;
            tags(cx)
        }
        "block" => block(cx, parse(op, args, "{id, view?}")?),
        "brief" => brief(
            cx,
            parse(
                op,
                args,
                "{ids?, path?, code_path?, page?, scope?, view?, max_tokens?}",
            )?,
        ),
        "search" => search(
            cx,
            parse(
                op,
                args,
                "{query, regex?, ignore_case?, scope?, max_tokens?, page?, types?, traits?, path?, lifecycle?, changed_since?, exclude_history?, governed_by?, describes?, linked_to?, suspect?}",
            )?,
        ),
        "render" => render(cx, parse(op, args, "{out, scope?}")?),
        "export" => export(cx, parse(op, args, "{out, scope?}")?),
        "check" => check(cx, parse(op, args, "{path, pack?}")?),
        "new_folder" => new_folder(cx, parse(op, args, "{path, view?}")?),
        "new_doc" => new_doc(cx, parse(op, args, "{path, view?, doc_type?, title?}")?),
        "keys_assign" => keys_assign(cx, parse(op, args, "{view?}")?),
        "edit_block" => edit_block(cx, parse(op, args, "{yaml, view?, brief?, max_tokens?}")?),
        "propose" => propose(
            cx,
            parse(op, args, "{title, view?, summary?, max_tokens?}")?,
        ),
        "approve" => approve(
            cx,
            parse(
                op,
                args,
                "{proposal, relayed_by?, evidence?, question?, session_ref?, strict?}",
            )?,
        ),
        "tag" => tag(cx, parse(op, args, "{version, landing?, message?}")?),
        "import" => import(cx, parse(op, args, "{dir, view}")?),
        "reindex" => {
            parse::<NoArgs>(op, args, "{}")?;
            reindex(cx)
        }
        "fsck" => fsck(cx, parse(op, args, "{clean_quarantine?}")?),
        "view" => view_api::run_view(
            cx.engine,
            cx.session,
            &cx.session.default_view(),
            parse(
                op,
                args,
                "{op, name?, from?, base?, pin?, view?, views?, changes?, cascade?, a?, b?, all?, stale?, reason?, owner?, none?, target?, max_tokens?}",
            )?,
        ),
        "wc_view" => wc_view(cx, parse(op, args, "{view?, ids?, blobs_dir?}")?),
        "wc_commit" => wc_commit(
            cx,
            parse(
                op,
                args,
                "{view, ops, nonce, rewrite, message?, implements?, editorial?, brief?, max_tokens?}",
            )?,
        ),
        "wc_nonce" => wc_nonce(cx, parse(op, args, "{view, nonce, rewrite}")?),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown op {op:?}"),
            format!("one of: {}", OPS.join(", ")),
        )),
    }
}

fn status(cx: &OpContext) -> Result<Value, RdyError> {
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
        "version_position": canon,
        "views": views,
        "warnings": warnings,
        "seq": st.seq(),
        "open_report": open_report_json(&cx.engine.open_report()),
        "quarantine": quarantine(&cx.engine.root()),
    }))
}

fn show(cx: &OpContext, a: ShowArgs) -> Result<Value, RdyError> {
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

fn ls(cx: &OpContext, a: LsArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.tree(&scope, a.path.as_deref(), a.depth)?))
}

fn log(cx: &OpContext, a: LogArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    Ok(to_json(&st.log_page(a.since.as_deref(), a.limit)?))
}

fn impact(cx: &OpContext, a: ImpactArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let bid = st.resolve_id(&scope, &a.id)?;
    Ok(to_json(&st.impact(&scope, &bid, a.depth.unwrap_or(3))?))
}

fn suspects(cx: &OpContext, a: ScopeArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(json!({"items": st.suspects(&scope)?}))
}

fn tags(cx: &OpContext) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let versions = st.versions()?;
    let current = versions.last().map(|v| v.name.clone());
    Ok(json!({"versions": versions, "current": current}))
}

fn block(cx: &OpContext, a: BlockArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let view = a.view.unwrap_or_else(|| cx.session.default_view());
    let scope = Scope::View(view);
    let bid = st.resolve_id(&scope, &a.id)?;
    let b = st.block(&scope, &bid)?;
    Ok(
        json!({"id": b.id.to_string(), "version": b.version_hash().to_string(), "yaml": block_to_yaml(&b)}),
    )
}

/// `E_LIMIT` for a `max_tokens` over `MAX_TOKENS_LIMIT`, as the MCP tools refuse it.
fn check_max_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
    match max_tokens {
        Some(n) if n > MAX_TOKENS_LIMIT => Err(RdyError::new(
            E_LIMIT,
            format!("max_tokens {n} is over the limit of {MAX_TOKENS_LIMIT}"),
            format!("ask for at most {MAX_TOKENS_LIMIT} tokens"),
        )),
        _ => Ok(()),
    }
}

/// A briefing over blocks and/or a doc or folder path, registered so its token can be cited.
/// The scope is the one given (a read: an unknown view is `E_NOT_FOUND`); else `view` as a write
/// target (that view when it has a head, else canon, as a write there would read it); else the
/// session's default view when it has a head, else canon.
fn brief(cx: &OpContext, a: BriefArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let st = cx.engine.read();
    let own = cx.session.default_view();
    if a.scope.is_some() && a.view.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "brief takes scope or view, not both",
            "pass scope to read a scope, or view to brief a write target",
        ));
    }
    let scope = match (a.scope.as_deref(), a.view) {
        (Some(t), _) => st.resolve_read_scope(t, Some(&own))?,
        (None, view) => {
            let target = view.unwrap_or(own);
            if st.view_head(&target).is_some() {
                Scope::View(target)
            } else {
                Scope::Canon
            }
        }
    };
    if let Some(cursor) = &a.page {
        if a.ids.is_some() || a.path.is_some() || a.code_path.is_some() {
            return Err(e_page_alone());
        }
        let b = st.brief_page(&scope, &cx.engine.briefs(), cursor, a.max_tokens)?;
        return Ok(to_json(&b));
    }
    if let Some(cp) = &a.code_path {
        if a.ids.is_some() || a.path.is_some() {
            return Err(e_code_path_alone());
        }
        let b = st.brief_code(&scope, cp, Some(cx.cwd), a.max_tokens)?;
        b.briefing.register(&cx.engine.briefs());
        return Ok(to_json(&b));
    }
    let b = st.brief(
        &scope,
        &Coverage {
            ids: a.ids.unwrap_or_default(),
            path: a.path,
        },
        a.max_tokens,
    )?;
    b.register(&cx.engine.briefs());
    Ok(to_json(&b))
}

/// msrch1: the blocks whose text matches, in canon unless a scope is named.
fn search(cx: &OpContext, a: SearchArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let r = st.search(
        &scope,
        &SearchQuery {
            text: a.query,
            regex: a.regex.unwrap_or(false),
            ignore_case: a.ignore_case.unwrap_or(false),
            filters: SearchFilters {
                types: a.types.unwrap_or_default(),
                traits: a.traits.unwrap_or_default(),
                path: a.path,
                lifecycle: a.lifecycle,
                changed_since: a.changed_since,
                exclude_history: a.exclude_history.unwrap_or(false),
                governed_by: a.governed_by,
                describes: a.describes,
                linked_to: a.linked_to,
                suspect: a.suspect.unwrap_or(false),
            },
        },
        a.page.as_deref(),
        a.max_tokens,
    )?;
    Ok(to_json(&r))
}

fn render(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let s = rdy_render::render_site(&st, &scope, &resolve_path(cx.cwd, &a.out))?;
    Ok(to_json(&s))
}

fn export(cx: &OpContext, a: OutArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.export(&scope, &resolve_path(cx.cwd, &a.out))?))
}

fn check(cx: &OpContext, a: CheckArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let owned;
    let pack = match a.pack.as_deref() {
        Some(name) => {
            owned = resolve_pack(&cx.engine.root(), name)?;
            &owned
        }
        None => st.pack(),
    };
    let canon = st.materialized(&Scope::Canon)?;
    Ok(to_json(&check_path(
        pack,
        Some(&*canon),
        &resolve_path(cx.cwd, &a.path),
    )?))
}

fn new_folder(cx: &OpContext, a: NewFolderArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let v = view.clone();
    write_and_report(cx.engine, &v, None, move |r| {
        r.new_folder(&s, &view, &a.path)
    })
}

fn new_doc(cx: &OpContext, a: NewDocArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let v = view.clone();
    write_and_report(cx.engine, &v, None, move |r| {
        r.new_doc(
            &s,
            &view,
            &a.path,
            a.doc_type.as_deref(),
            a.title.as_deref(),
        )
    })
}

fn keys_assign(cx: &OpContext, a: KeysAssignArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (v, name) = (view.clone(), view.clone());
    let mut out = write_and_report(cx.engine, &v, None, move |r| r.assign_keys(&s, &view))?;
    out["view"] = json!(name);
    Ok(out)
}

fn edit_block(cx: &OpContext, a: EditBlockArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let (yaml, brief, max_tokens, v) = (a.yaml, a.brief, a.max_tokens, view.clone());
    write_and_report(cx.engine, &v, max_tokens, move |r| {
        r.edit_yaml(&s, &view, &yaml, brief)
    })
}

fn propose(cx: &OpContext, a: ProposeArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    propose_and_report(cx.engine, a.max_tokens, move |r| {
        r.propose_subset(&s, &view, &a.title, None, a.summary.as_deref())
    })
}

fn approve(cx: &OpContext, a: ApproveArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    let bad = |message: &str, fix: &str| RdyError::new(E_BAD_INPUT, message, fix);
    let relay = match (a.relayed_by, a.evidence) {
        (Some(by), Some(quoted)) => {
            let (actor, run) = match by.split_once('/') {
                Some((actor, run)) => (actor.to_string(), Some(run.to_string())),
                None => (by, None),
            };
            Some(Relay {
                by: ActorRun { actor, run },
                evidence: RelayEvidence {
                    quoted,
                    session_ref: a.session_ref.unwrap_or_else(|| "cli".into()),
                    question: a.question.filter(|q| !q.trim().is_empty()),
                },
            })
        }
        (None, None) => {
            if a.session_ref.is_some() {
                return Err(bad(
                    "session_ref needs relayed_by",
                    "pass relayed_by and evidence with it, or drop session_ref",
                ));
            }
            if a.question.is_some() {
                return Err(bad(
                    "question needs relayed_by",
                    "pass relayed_by and evidence with it, or drop question",
                ));
            }
            None
        }
        _ => {
            return Err(bad(
                "relayed_by and evidence go together",
                "pass both (--relayed-by and --evidence) or neither",
            ));
        }
    };
    let opts = LandOptions {
        strict: a.strict.unwrap_or(false),
        relay,
        evidence: None,
    };
    land_and_report(cx.engine, move |r| r.land_with(&s, &a.proposal, opts))
}

fn tag(cx: &OpContext, a: TagArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    write_typed(cx.engine, move |r| {
        r.tag(&s, &a.version, a.landing.as_deref(), a.message)
    })
}

fn import(cx: &OpContext, a: ImportArgs) -> Result<Value, RdyError> {
    let s = approver(cx.session)?;
    let view = view_of(&s, Some(a.view));
    let dir = resolve_path(cx.cwd, &a.dir);
    let scope = Scope::View(view.clone());
    let target = view.clone();
    let mut v = write_and_report(cx.engine, &target, None, move |r| r.import(&s, &view, &dir))?;
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

fn reindex(cx: &OpContext) -> Result<Value, RdyError> {
    cx.engine.write(Box::new(|r| {
        r.reindex()?;
        Ok(json!({"reindexed": true}))
    }))
}

/// `fsck`; with `clean_quarantine`, a passing check then removes the quarantine files (the
/// human's only: an agent is refused before the check runs). A failed check removes nothing.
fn fsck(cx: &OpContext, a: FsckArgs) -> Result<Value, RdyError> {
    if a.clean_quarantine && !cx.session.is_human() {
        return Err(RdyError::new(
            E_PERMISSION,
            "only the human removes quarantine files",
            "run rdy fsck --clean-quarantine as the human (unset RDY_ACTOR)",
        ));
    }
    let mut v = write_typed(cx.engine, |r| r.fsck())?;
    if a.clean_quarantine {
        let c = clean_quarantine(&cx.engine.root());
        v["removed"] = json!(c.removed);
        v["skipped"] = json!(
            c.skipped
                .iter()
                .map(|(n, r)| json!({"name": n, "reason": r}))
                .collect::<Vec<_>>()
        );
    }
    v["open_report"] = open_report_json(&cx.engine.open_report());
    v["quarantine"] = json!(quarantine(&cx.engine.root()));
    Ok(v)
}

/// wcco1: a view as working-copy files. The scope is the view when it has a head, else canon;
/// `ids` resolve in that scope. With `blobs_dir` (resolved against the session's directory), each
/// blob whose file is not there yet is written from the store; a blob the store lacks is
/// `E_NOT_FOUND`, as export's is.
fn wc_view(cx: &OpContext, a: WcViewArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let view = view_of(cx.session, a.view);
    let scope = if st.view_head(&view).is_some() {
        Scope::View(view.clone())
    } else {
        Scope::Canon
    };
    let ids = a
        .ids
        .map(|ids| {
            ids.iter()
                .map(|t| st.resolve_id(&scope, t))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut proj = st.wc_project(&scope, ids.as_deref())?;
    proj.view = view;
    if let Some(dir) = a.blobs_dir {
        let dir = resolve_path(cx.cwd, &dir);
        for b in &proj.blobs {
            let path = dir.join(&b.name);
            if std::fs::symlink_metadata(&path).is_ok() {
                continue;
            }
            let not_found = || {
                RdyError::new(
                    E_NOT_FOUND,
                    format!(
                        "a figure names blob {}, which is not a blob in the store",
                        b.hash
                    ),
                    "attach the asset again (rdy_asset) and point the figure at it, or delete the figure",
                )
            };
            let h: rdy_model::Hash = b.hash.parse().map_err(|_| not_found())?;
            if !st.reader().has_blob(&h)? {
                return Err(not_found());
            }
            let bytes = st.reader().get_blob(&h)?;
            let io = |e: std::io::Error| {
                RdyError::new(
                    E_IO,
                    format!("cannot write {}: {e}", path.display()),
                    "check the directory and its permissions",
                )
            };
            std::fs::create_dir_all(&dir).map_err(io)?;
            std::fs::write(&path, bytes).map_err(io)?;
        }
    }
    Ok(to_json(&proj))
}

/// The most changes `wc_nonce` reads before it answers that no change carries the nonce (R-I6).
const NONCE_SCAN_LIMIT: usize = 10_000;

/// How long `wc_nonce` waits for a `wc_commit` with its nonce that is still running before it
/// answers `pending`.
const NONCE_WAIT: Duration = Duration::from_secs(60);

/// The nonces of the `wc_commit` requests this daemon is running: from dispatch until their
/// write has returned (and its state is published). A client that dies after it sends one
/// leaves the request running; the next command's `wc_nonce` waits for it.
static IN_FLIGHT: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static IN_FLIGHT_DONE: Condvar = Condvar::new();

fn in_flight() -> MutexGuard<'static, BTreeSet<String>> {
    IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a `wc_commit` with `nonce` is running in this process.
pub fn wc_in_flight(nonce: &str) -> bool {
    in_flight().contains(nonce)
}

/// A `wc_commit`'s nonce, in flight while this lives.
struct InFlight(String);

impl InFlight {
    fn start(nonce: &str) -> InFlight {
        in_flight().insert(nonce.to_string());
        InFlight(nonce.to_string())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        in_flight().remove(&self.0);
        IN_FLIGHT_DONE.notify_all();
    }
}

/// Waits until no `wc_commit` with `nonce` runs, up to `NONCE_WAIT`; false when one still runs.
fn wait_not_in_flight(nonce: &str) -> bool {
    let deadline = Instant::now() + NONCE_WAIT;
    let mut set = in_flight();
    while set.contains(nonce) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        set = IN_FLIGHT_DONE
            .wait_timeout(set, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    true
}

/// The scope a working copy of `view` reads: the view when it has a head, else canon.
fn wc_scope(st: &ReadState, view: &str) -> Scope {
    if st.view_head(view).is_some() {
        Scope::View(view.to_string())
    } else {
        Scope::Canon
    }
}

/// The projection of the files of `rewrite` (ids as text) in `view`, as reply keys: `files`,
/// `blocks`, `blobs`, `missing` and `head`. An id that does not parse, or that the scope does
/// not hold, is listed in `missing`.
fn wc_rewrite(st: &ReadState, view: &str, rewrite: &[String]) -> Result<Value, RdyError> {
    let scope = wc_scope(st, view);
    let mut ids = vec![];
    let mut unparsed = vec![];
    for t in rewrite {
        match rdy_model::BlockId::parse(t) {
            Ok(id) => ids.push(id),
            Err(_) => unparsed.push(t.clone()),
        }
    }
    let mut proj = st.wc_project(&scope, Some(&ids))?;
    proj.missing.extend(unparsed);
    Ok(json!({
        "files": proj.files,
        "blocks": proj.blocks,
        "blobs": proj.blobs,
        "missing": proj.missing,
        "head": proj.head,
    }))
}

/// Merges the keys of the object `extra` into the object `v`.
fn merge_into(v: &mut Value, extra: Value) {
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
}

/// wccm1: a working-copy commit. One write of `ops` into `view` that records `nonce` on the
/// change, justified as `rdy_write` justifies it; the reply gains the projection of the files
/// of `rewrite` as the write left them.
fn wc_commit(cx: &OpContext, a: WcCommitArgs) -> Result<Value, RdyError> {
    // in flight from dispatch until the write has returned, before anything can fail or queue
    let _in_flight = InFlight::start(&a.nonce);
    check_max_tokens(a.max_tokens)?;
    if !cx.session.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot write", cx.session.role),
            "start the session as a writer",
        ));
    }
    let view = view_of(cx.session, Some(a.view));
    let justification = parse_justification(
        &cx.engine.read(),
        &Scope::View(view.clone()),
        a.implements,
        a.editorial.map(|e| (e.class, e.reason)),
    )?;
    let ops: Vec<OpInput> = a
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value(v).map_err(|e| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("op {i}: {e}"),
                    "each op needs \"op\" (rdy_schema lists them under \"ops\"); create needs \"type\"",
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let req = WriteRequest {
        view: view.clone(),
        ops,
        justification,
        message: a.message.unwrap_or_default(),
    };
    let s = cx.session.clone();
    let (nonce, brief) = (a.nonce, a.brief);
    let mut out = write_and_report(cx.engine, &view, a.max_tokens, move |r| {
        r.write_linked(
            &s,
            req,
            ChangeLinks {
                client_nonce: Some(nonce),
                ..Default::default()
            },
            brief.as_deref(),
        )
    })?;
    let proj = wc_rewrite(&cx.engine.read(), &view, &a.rewrite)?;
    merge_into(&mut out, proj);
    Ok(out)
}

/// wccm1, R-I6: the change a working-copy commit with `nonce` recorded, when the store has it,
/// and then the projection of the files of `rewrite` (as `wc_commit` adds it); else
/// `{change: null}`. The answer is conclusive: it first waits while a `wc_commit` with the nonce
/// runs in this daemon (`{change: null, pending: true}` when it still runs after `NONCE_WAIT`),
/// then reads on the writer, after every write queued before it.
fn wc_nonce(cx: &OpContext, a: WcNonceArgs) -> Result<Value, RdyError> {
    if !wait_not_in_flight(&a.nonce) {
        return Ok(json!({"change": null, "pending": true}));
    }
    let view = view_of(cx.session, Some(a.view));
    let (nonce, rewrite) = (a.nonce, a.rewrite);
    cx.engine.write(Box::new(move |r| {
        let st = r.read();
        match st.change_by_nonce(&view, &nonce, NONCE_SCAN_LIMIT)? {
            None => Ok(json!({"change": null})),
            Some(h) => {
                let mut out = json!({"change": h.to_string()});
                merge_into(&mut out, wc_rewrite(&st, &view, &rewrite)?);
                Ok(out)
            }
        }
    }))
}
