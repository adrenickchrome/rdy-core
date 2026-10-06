//! The ctl ops: one function per op, each against `&dyn Engine`. Every read op takes one
//! snapshot (`engine.read()`) and reads only through it; none touches the writer. Every write
//! op parses and checks its role here, then runs its engine call as one `Engine::write`
//! closure on the writer. Relative paths in args resolve against the session's working
//! directory.
use rdy_engine::brief::Coverage;
use rdy_engine::code_brief::e_code_path_alone;
use rdy_engine::consequence::write_and_report;
use rdy_engine::engine::{Engine, open_report_json, write_typed};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_PERMISSION, LandOptions, RdyError, ReadState, Relay, Scope, Session,
    block_from_yaml, block_to_yaml, check_path, resolve_pack, view_api,
};
use rdy_mcp::tools::MAX_TOKENS_LIMIT;
use rdy_model::{ActorRun, RelayEvidence, Role};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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
    "render",
    "export",
    "check",
    "new_folder",
    "new_doc",
    "edit_block",
    "propose",
    "approve",
    "tag",
    "import",
    "reindex",
    "fsck",
    "view",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    id: String,
    scope: Option<String>,
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
    scope: Option<String>,
    /// A write target, resolved as `edit_block` writes it: this view when it has a head, else
    /// canon.
    view: Option<String>,
    max_tokens: Option<usize>,
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
        "show" => show(cx, parse(op, args, "{id, scope?}")?),
        "ls" => ls(cx, parse(op, args, "{path?, scope?, depth?}")?),
        "log" => {
            parse::<NoArgs>(op, args, "{}")?;
            log(cx)
        }
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
                "{ids?, path?, code_path?, scope?, view?, max_tokens?}",
            )?,
        ),
        "render" => render(cx, parse(op, args, "{out, scope?}")?),
        "export" => export(cx, parse(op, args, "{out, scope?}")?),
        "check" => check(cx, parse(op, args, "{path, pack?}")?),
        "new_folder" => new_folder(cx, parse(op, args, "{path, view?}")?),
        "new_doc" => new_doc(cx, parse(op, args, "{path, view?, doc_type?, title?}")?),
        "edit_block" => edit_block(cx, parse(op, args, "{yaml, view?, brief?, max_tokens?}")?),
        "propose" => propose(cx, parse(op, args, "{title, view?}")?),
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
        "fsck" => {
            parse::<NoArgs>(op, args, "{}")?;
            fsck(cx)
        }
        "view" => view_api::run_view(
            cx.engine,
            cx.session,
            &cx.session.default_view(),
            parse(
                op,
                args,
                "{op, name?, from?, base?, pin?, view?, views?, changes?, cascade?, a?, b?, all?, stale?, reason?, owner?, none?, target?}",
            )?,
        ),
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
    Ok(v)
}

fn ls(cx: &OpContext, a: LsArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.tree(&scope, a.path.as_deref(), a.depth)?))
}

fn log(cx: &OpContext) -> Result<Value, RdyError> {
    Ok(to_json(&cx.engine.read().log_rows()?))
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

fn edit_block(cx: &OpContext, a: EditBlockArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    let block = block_from_yaml(&a.yaml)?;
    let (brief, max_tokens, v) = (a.brief, a.max_tokens, view.clone());
    write_and_report(cx.engine, &v, max_tokens, move |r| {
        r.edit_block(&s, &view, block, brief)
    })
}

fn propose(cx: &OpContext, a: ProposeArgs) -> Result<Value, RdyError> {
    let s = cx.session.clone();
    let view = view_of(&s, a.view);
    write_typed(cx.engine, move |r| r.propose(&s, &view, &a.title))
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
    write_typed(cx.engine, move |r| r.land_with(&s, &a.proposal, opts))
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

fn fsck(cx: &OpContext) -> Result<Value, RdyError> {
    let mut v = write_typed(cx.engine, |r| r.fsck())?;
    v["open_report"] = open_report_json(&cx.engine.open_report());
    v["quarantine"] = json!(quarantine(&cx.engine.root()));
    Ok(v)
}
