use clap::{Parser, Subcommand};
use rdy_engine::*;
use rdy_model::Role;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "rdy",
    version,
    about = "Version control for agent-aided design"
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Project directory (default: nearest ancestor containing .rdy/).
    #[arg(long, global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Init {
        dir: Option<PathBuf>,
        #[arg(long, default_value = "rdy-standard")]
        pack: String,
    },
    Status,
    Show {
        /// An id or a path such as wallets/transactions; `id@version` or a ref line `[id@version] …` reads under that version.
        id: String,
        /// canon (the default), an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long)]
        scope: Option<String>,
    },
    Log,
    Impact {
        /// An id or a path such as wallets/transactions.
        id: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    Propose {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "work")]
        view: String,
    },
    Approve {
        proposal: String,
    },
    /// Name a landing (default: canon's head) as a version, e.g. v1.2.0. Approver only.
    Tag {
        version: String,
        #[arg(long)]
        landing: Option<String>,
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// List versions, oldest first.
    Tags,
    /// Create a folder or a doc at a path (missing folders are created too).
    New {
        #[command(subcommand)]
        what: NewCmd,
    },
    /// List the folder and doc tree.
    Ls {
        path: Option<String>,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
        #[arg(long)]
        depth: Option<u32>,
    },
    Edit {
        /// An id or a path such as wallets/transactions.
        id: String,
        #[arg(long, default_value = "work")]
        view: String,
    },
    Render {
        /// Output directory: new, empty, or a site rdy rendered before (replaced as a whole).
        #[arg(long)]
        out: PathBuf,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    Export {
        #[arg(long)]
        out: PathBuf,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    Import {
        dir: PathBuf,
        #[arg(long)]
        view: String,
    },
    Reindex,
    Fsck,
    /// Run the MCP server over stdio.
    Mcp,
    /// Lint export-format-2 files (a doc file, a _folder.yaml or a directory) without touching the store.
    Check {
        path: PathBuf,
        /// Check against this pack (required outside a project).
        #[arg(long)]
        pack: Option<String>,
    },
    Pack {
        #[command(subcommand)]
        cmd: PackCmd,
    },
}

#[derive(Subcommand)]
enum NewCmd {
    Folder {
        path: String,
        #[arg(long, default_value = "work")]
        view: String,
    },
    Doc {
        path: String,
        #[arg(long = "type")]
        doc_type: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "work")]
        view: String,
    },
}

fn ls_text(nodes: &[TreeNode], depth: usize, out: &mut Vec<String>) {
    for n in nodes {
        let mut line = format!(
            "{}{}{}  {}  {}",
            "  ".repeat(depth),
            n.name,
            if n.block_type == "folder" { "/" } else { "" },
            n.block_type,
            n.id
        );
        if let Some(t) = &n.doc_type {
            line.push_str(&format!("  {t}"));
        }
        if let Some(l) = &n.lifecycle {
            line.push_str(&format!(" · {l}"));
        }
        if n.collapsed > 0 {
            line.push_str(&format!("  (+{} more)", n.collapsed));
        }
        out.push(line);
        ls_text(&n.children, depth + 1, out);
    }
}

#[derive(Subcommand)]
enum PackCmd {
    /// Resolve the project's pack and report problems.
    Check,
}

fn find_project(start: &Path) -> PathBuf {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".rdy/config.yaml").exists() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    start.to_path_buf()
}

fn human(role: Role) -> Session {
    let actor = std::env::var("RDY_ACTOR").unwrap_or_else(|_| {
        format!(
            "human:{}",
            std::env::var("USER").unwrap_or_else(|_| "unknown".into())
        )
    });
    Session {
        actor,
        run: None,
        role,
    }
}

fn hash_short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

/// show1: a decision's derived status in words, e.g. "superseded by D-3K0TY" or "live".
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

fn run(cli: Cli) -> Result<(Value, String), RdyError> {
    let cwd = std::env::current_dir()
        .map_err(|e| RdyError::new(E_IO, e.to_string(), "run from an existing directory"))?;
    let project = cli.project.clone().unwrap_or_else(|| find_project(&cwd));
    match cli.cmd {
        Cmd::Init { dir, pack } => {
            let dir = dir.or(cli.project).unwrap_or(cwd);
            Rdy::init(&dir, &pack)?;
            Ok((
                json!({"project": dir, "pack": pack}),
                format!("Initialized rdy project in {} (pack {pack})", dir.display()),
            ))
        }
        Cmd::Mcp => {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| RdyError::new(E_IO, e.to_string(), "retry"))?;
            rt.block_on(rdy_mcp::server::serve_stdio(project))
                .map_err(|e| RdyError::new(E_IO, e.to_string(), "check the project path"))?;
            Ok((Value::Null, String::new()))
        }
        cmd => {
            let mut r = Rdy::open(&project)?;
            let out = match cmd {
                Cmd::Status => {
                    let st = r.read();
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
                        views.push(
                            json!({"name": name, "changes": changes, "version_position": pos}),
                        );
                    }
                    let suspects = st.suspects(&Scope::Canon)?.len();
                    let mut text = format!(
                        "pack {}\ncanon: {} ({landings} landings, {suspects} suspects)\nviews:\n{}",
                        r.pack().name,
                        canon.label,
                        views
                            .iter()
                            .map(|v| format!(
                                "  {}: {}",
                                v["name"].as_str().unwrap_or(""),
                                v["version_position"]["label"].as_str().unwrap_or("")
                            ))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    for w in &warnings {
                        text.push_str(&format!("\nwarning: {w}"));
                    }
                    Ok((
                        json!({"pack": r.pack().name, "landings": landings, "suspects": suspects, "version_position": canon, "views": views, "warnings": warnings, "seq": st.seq()}),
                        text,
                    ))
                }
                Cmd::Show { id, scope } => {
                    let target = rdy_model::reference::parse_target(&id).map_err(|m| {
                        RdyError::new(E_BAD_INPUT, m, rdy_model::reference::BAD_REF_FIX)
                    })?;
                    let text = target
                        .scope_text(scope.as_deref())
                        .map_err(|m| {
                            RdyError::new(E_BAD_INPUT, m, rdy_model::reference::PIN_CLASH_FIX)
                        })?
                        .unwrap_or_else(|| "canon".to_string());
                    let st = r.read();
                    let scope = st.resolve_read_scope(&text, None)?;
                    let bid = st.resolve_id(&scope, &target.id)?;
                    let ctx = st.context(&scope, &bid)?;
                    let json = serde_json::to_value(&ctx).expect("json");
                    let status = status_words(&json["status"]);
                    let mut fields = json["fields"].clone();
                    if status.is_some()
                        && let Some(f) = fields.as_object_mut()
                    {
                        // the stored status ("accepted") would contradict the derived one
                        f.remove("status");
                    }
                    let mut text = format!(
                        "{} {} (version {})\n",
                        ctx.id,
                        ctx.block_type,
                        hash_short(&ctx.version)
                    );
                    if let Some(s) = &status {
                        text.push_str(&format!("status: {s}\n"));
                    }
                    text.push_str(&format!(
                        "{}\n{}",
                        serde_json::to_string_pretty(&fields).unwrap_or_default(),
                        ctx.body
                    ));
                    Ok((json, text))
                }
                Cmd::New { what } => {
                    let (kind, path, res) = match what {
                        NewCmd::Folder { path, view } => (
                            "folder",
                            path.clone(),
                            r.new_folder(&human(Role::Writer), &view, &path)?,
                        ),
                        NewCmd::Doc {
                            path,
                            doc_type,
                            title,
                            view,
                        } => (
                            "doc",
                            path.clone(),
                            r.new_doc(
                                &human(Role::Writer),
                                &view,
                                &path,
                                doc_type.as_deref(),
                                title.as_deref(),
                            )?,
                        ),
                    };
                    let last = res
                        .created
                        .values()
                        .rfind(|v| v.starts_with(if kind == "doc" { "d-" } else { "fd-" }))
                        .cloned()
                        .unwrap_or_default();
                    Ok((
                        serde_json::to_value(&res).expect("json"),
                        format!("created {kind} {path} ({last})"),
                    ))
                }
                Cmd::Ls { path, scope, depth } => {
                    let scope = r.read().resolve_read_scope(&scope, None)?;
                    let nodes = r.read().tree(&scope, path.as_deref(), depth)?;
                    let mut lines = vec![];
                    ls_text(&nodes, 0, &mut lines);
                    Ok((
                        serde_json::to_value(&nodes).expect("json"),
                        lines.join("\n"),
                    ))
                }
                Cmd::Log => {
                    let mut rows = vec![];
                    let mut text = String::new();
                    let mut tags: std::collections::HashMap<String, Vec<String>> =
                        Default::default();
                    let st = r.read();
                    for v in st.versions()? {
                        tags.entry(v.landing.clone()).or_default().push(v.name);
                    }
                    for (id, rec) in st.landings()? {
                        let p = st.reader().proposal(&rec.proposal)?;
                        let names = tags.get(&id.to_string()).cloned().unwrap_or_default();
                        text.push_str(&format!(
                            "{}  {}  by {}",
                            id.short(),
                            p.title,
                            rec.approved_by.actor
                        ));
                        if !names.is_empty() {
                            text.push_str(&format!("  [{}]", names.join(", ")));
                        }
                        text.push('\n');
                        rows.push(json!({"landing": id.to_string(), "title": p.title, "approved_by": rec.approved_by.actor, "assurance": rec.assurance.as_str(), "at_ms": rec.approved_by.at_ms, "changes": p.changes.len(), "tags": names}));
                    }
                    Ok((Value::Array(rows), text))
                }
                Cmd::Impact { id, depth, scope } => {
                    let scope = r.read().resolve_read_scope(&scope, None)?;
                    let bid = r.read().resolve_id(&scope, &id)?;
                    let items = r.read().impact(&scope, &bid, depth)?;
                    let text = items
                        .iter()
                        .map(|i| {
                            format!(
                                "depth {}  {} {}: {}  [{} {}]",
                                i.depth, i.id, i.block_type, i.summary, i.via.kind, i.via.from
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok((serde_json::to_value(&items).expect("json"), text))
                }
                Cmd::Propose { title, view } => {
                    let view = view.strip_prefix("view:").unwrap_or(&view).to_string();
                    let p = r.propose(&human(Role::Writer), &view, &title)?;
                    let text = format!(
                        "proposal {} ({} changes)\napprove with: rdy approve {}",
                        hash_short(&p.id),
                        p.changes,
                        p.id
                    );
                    Ok((serde_json::to_value(&p).expect("json"), text))
                }
                Cmd::Approve { proposal } => {
                    let l = r.land_report(&human(Role::Approver), &proposal)?;
                    let mut text = format!("landed {}", hash_short(&l.landing));
                    for w in &l.warnings {
                        text.push_str(&format!("\nwarning {}: {}", w.code, w.message));
                    }
                    Ok((json!({"landing": l.landing, "warnings": l.warnings}), text))
                }
                Cmd::Tag {
                    version,
                    landing,
                    message,
                } => {
                    let v = r.tag(
                        &human(Role::Approver),
                        &version,
                        landing.as_deref(),
                        message,
                    )?;
                    let text = format!("version {} → landing {}", v.name, hash_short(&v.landing));
                    Ok((serde_json::to_value(&v).expect("json"), text))
                }
                Cmd::Tags => {
                    let versions = r.read().versions()?;
                    let current = versions.last().map(|v| v.name.clone());
                    let text = versions
                        .iter()
                        .map(|v| {
                            let mut line = format!("{}  {}", v.name, hash_short(&v.landing));
                            if let Some(m) = &v.message {
                                line.push_str(&format!("  {m}"));
                            }
                            if Some(&v.name) == current.as_ref() {
                                line.push_str("  (current)");
                            }
                            line
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let text = if versions.is_empty() {
                        "no versions yet".to_string()
                    } else {
                        text
                    };
                    Ok((json!({"versions": versions, "current": current}), text))
                }
                Cmd::Edit { id, view } => {
                    let scope = Scope::View(view.clone());
                    let bid = r.read().resolve_id(&scope, &id)?;
                    let block = r.read().block(&scope, &bid)?;
                    let tmp = tempfile::Builder::new()
                        .prefix("rdy-edit-")
                        .suffix(".yaml")
                        .tempfile()
                        .map_err(|e| {
                            RdyError::new(E_IO, e.to_string(), "check the temp directory")
                        })?;
                    std::fs::write(tmp.path(), block_to_yaml(&block)).map_err(|e| {
                        RdyError::new(E_IO, e.to_string(), "check the temp directory")
                    })?;
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg("$EDITOR \"$1\"")
                        .arg("sh")
                        .arg(tmp.path())
                        .env("EDITOR", &editor)
                        .status()
                        .map_err(|e| {
                            RdyError::new(E_IO, format!("{editor}: {e}"), "set $EDITOR")
                        })?;
                    if !status.success() {
                        return Err(RdyError::new(
                            E_BAD_INPUT,
                            "editor exited with an error; nothing changed",
                            "save and quit the editor normally",
                        ));
                    }
                    let text = std::fs::read_to_string(tmp.path())
                        .map_err(|e| RdyError::new(E_IO, e.to_string(), "retry"))?;
                    let res = r.edit_block(&human(Role::Writer), &view, block_from_yaml(&text)?)?;
                    Ok((
                        serde_json::to_value(&res).expect("json"),
                        format!("change {}", hash_short(&res.change)),
                    ))
                }
                Cmd::Render { out, scope } => {
                    let scope = r.read().resolve_read_scope(&scope, None)?;
                    let s = rdy_render::render_site(&r.read(), &scope, &out)?;
                    Ok((
                        serde_json::to_value(&s).expect("json"),
                        format!(
                            "rendered {} pages, {} permalinks and {} assets to {}{}",
                            s.pages,
                            s.redirects,
                            s.assets,
                            out.display(),
                            s.warnings
                                .iter()
                                .map(|w| format!("\nwarning: {w}"))
                                .collect::<String>()
                        ),
                    ))
                }
                Cmd::Export { out, scope } => {
                    let scope = r.read().resolve_read_scope(&scope, None)?;
                    let s = r.read().export(&scope, &out)?;
                    Ok((
                        serde_json::to_value(&s).expect("json"),
                        format!(
                            "exported {} docs in {} folders, {} blocks, {} blobs{}",
                            s.docs,
                            s.folders,
                            s.blocks,
                            s.blobs,
                            s.warnings
                                .iter()
                                .map(|w| format!("\nwarning: {w}"))
                                .collect::<String>()
                        ),
                    ))
                }
                Cmd::Import { dir, view } => {
                    let res = r.import(&human(Role::Approver), &view, &dir)?;
                    let mut text =
                        format!("imported {} blocks into view {view}", res.created.len());
                    for w in &res.warnings {
                        text.push_str(&format!("\nwarning {}: {}", w.code, w.message));
                    }
                    Ok((serde_json::to_value(&res).expect("json"), text))
                }
                Cmd::Reindex => {
                    r.reindex()?;
                    Ok((json!({"reindexed": true}), "index rebuilt".into()))
                }
                Cmd::Fsck => {
                    let f = r.fsck()?;
                    Ok((
                        serde_json::to_value(&f).expect("json"),
                        format!(
                            "ok: {} frames, {} landings, {} versions",
                            f.frames, f.landings, f.versions
                        ),
                    ))
                }
                Cmd::Pack {
                    cmd: PackCmd::Check,
                } => Ok((
                    json!({"pack": r.pack().name, "version": r.pack().version, "types": r.pack().block_types.len()}),
                    format!("pack {} {} ok", r.pack().name, r.pack().version),
                )),
                Cmd::Init { .. } | Cmd::Mcp => unreachable!("handled above"),
                Cmd::Check { .. } => unreachable!("handled in main"),
            };
            close_project(r);
            out
        }
    }
}

/// Closes the project's store cleanly. A failed close does not fail the command (its writes are
/// durable in the log, and the next open catches the index up), but it is reported on stderr. A
/// command that returns early with an error closes on drop, which reports the same way.
fn close_project(r: Rdy) {
    if let Err(e) = r.close() {
        eprintln!(
            "warning: the store did not close cleanly ({e}); the next rdy command catches its index up from the log"
        );
    }
}

fn run_check(project: Option<PathBuf>, path: &Path, pack: Option<&str>, as_json: bool) -> ExitCode {
    let result = (|| -> Result<Vec<Diagnostic>, RdyError> {
        let cwd = std::env::current_dir()
            .map_err(|e| RdyError::new(E_IO, e.to_string(), "run from an existing directory"))?;
        let root = project.unwrap_or_else(|| find_project(&cwd));
        if root.join(".rdy/config.yaml").exists() {
            let r = Rdy::open(&root)?;
            let pk = match pack {
                Some(name) => resolve_pack(&root, name)?,
                None => r.pack().clone(),
            };
            let st = r.read();
            let canon = st.materialized(&Scope::Canon)?;
            drop(st);
            close_project(r);
            check_path(&pk, Some(&*canon), path)
        } else {
            let name = pack.ok_or_else(|| {
                RdyError::new(
                    E_NO_PROJECT,
                    "not inside an rdy project",
                    "run rdy check inside a project, or pass --pack rdy-standard",
                )
            })?;
            let pk = rdy_schema::PackSet::with_builtin()
                .resolve(name)
                .map_err(|e| {
                    RdyError::new(
                        E_CONFIG,
                        e.to_string(),
                        "pass a built-in pack such as rdy-standard",
                    )
                })?;
            check_path(&pk, None, path)
        }
    })();
    match result {
        Ok(diags) => {
            if as_json {
                println!("{}", serde_json::to_string(&diags).unwrap_or_default());
            } else if diags.is_empty() {
                println!("ok");
            } else {
                for d in &diags {
                    println!(
                        "{}:{}:{}: {} {} (fix: {})",
                        d.file, d.line, d.column, d.code, d.message, d.fix
                    );
                }
            }
            if diags
                .iter()
                .any(|d| d.severity == rdy_schema::Severity::Error)
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            if as_json {
                eprintln!("{}", serde_json::to_string(&e).unwrap_or_default());
            } else {
                eprintln!("error {}: {}\nfix: {}", e.code, e.message, e.fix);
            }
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let as_json = cli.json;
    if let Cmd::Check { path, pack } = &cli.cmd {
        return run_check(cli.project.clone(), path, pack.as_deref(), as_json);
    }
    match run(cli) {
        Ok((v, text)) => {
            if v.is_null() {
            } else if as_json {
                println!("{v}");
            } else if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if as_json {
                eprintln!("{}", serde_json::to_string(&e).unwrap_or_default());
            } else {
                eprintln!("error {}: {}\nfix: {}", e.code, e.message, e.fix);
                for i in &e.issues {
                    eprintln!("  {} {:?}: {} (fix: {})", i.code, i.block, i.message, i.fix);
                }
            }
            ExitCode::FAILURE
        }
    }
}
