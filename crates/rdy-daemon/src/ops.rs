//! The ctl ops: one function per op, each against `&dyn Engine`. Every read op takes one
//! snapshot (`engine.read()`) and reads only through it; none touches the writer. Relative
//! paths in args resolve against the session's working directory.
use rdy_engine::engine::{Engine, open_report_json};
use rdy_engine::{
    E_BAD_INPUT, RdyError, ReadState, Scope, Session, block_to_yaml, check_path, resolve_pack,
};
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
/// host; the write ops are filled in by their own arms.
pub const OPS: &[&str] = &[
    "session",
    "status",
    "show",
    "ls",
    "log",
    "impact",
    "tags",
    "block",
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
struct BlockArgs {
    id: String,
    view: Option<String>,
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

/// Runs the ctl op `op` with `args`. An unknown op, or a write op not yet served, is
/// `E_BAD_INPUT` naming every op.
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
        "tags" => {
            parse::<NoArgs>(op, args, "{}")?;
            tags(cx)
        }
        "block" => block(cx, parse(op, args, "{id, view?}")?),
        "render" => render(cx, parse(op, args, "{out, scope?}")?),
        "export" => export(cx, parse(op, args, "{out, scope?}")?),
        "check" => check(cx, parse(op, args, "{path, pack?}")?),
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
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let bid = st.resolve_id(&scope, &a.id)?;
    let ctx = st.context(&scope, &bid)?;
    let mut v = to_json(&ctx);
    let words = status_words(&v["status"]);
    v["status_words"] = words.map_or(Value::Null, Value::String);
    Ok(v)
}

fn ls(cx: &OpContext, a: LsArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    Ok(to_json(&st.tree(&scope, a.path.as_deref(), a.depth)?))
}

fn log(cx: &OpContext) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let mut tags: std::collections::HashMap<String, Vec<String>> = Default::default();
    for v in st.versions()? {
        tags.entry(v.landing.clone()).or_default().push(v.name);
    }
    let mut rows = vec![];
    for (id, rec) in st.landings()? {
        let p = st.reader().proposal(&rec.proposal)?;
        let names = tags.get(&id.to_string()).cloned().unwrap_or_default();
        rows.push(json!({"landing": id.to_string(), "title": p.title, "approved_by": rec.approved_by.actor, "assurance": rec.assurance.as_str(), "at_ms": rec.approved_by.at_ms, "changes": p.changes.len(), "tags": names}));
    }
    Ok(Value::Array(rows))
}

fn impact(cx: &OpContext, a: ImpactArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let bid = st.resolve_id(&scope, &a.id)?;
    Ok(to_json(&st.impact(&scope, &bid, a.depth.unwrap_or(3))?))
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
