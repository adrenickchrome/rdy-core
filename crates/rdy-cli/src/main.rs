use clap::{Parser, Subcommand};
use rdy_engine::*;
use rdy_model::{BlockId, Role};
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
        id: String,
        /// canon, a view name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    Log,
    Impact {
        id: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
        /// canon, a view name, tag:<version>, landing:<id> or view:<name>.
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
    Edit {
        id: String,
        #[arg(long, default_value = "work")]
        view: String,
    },
    Render {
        #[arg(long)]
        out: PathBuf,
        /// canon, a view name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    Export {
        #[arg(long)]
        out: PathBuf,
        /// canon, a view name, tag:<version>, landing:<id> or view:<name>.
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
    Pack {
        #[command(subcommand)]
        cmd: PackCmd,
    },
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
            match cmd {
                Cmd::Status => {
                    let landings = r.store().landings()?.len();
                    let canon = r.version_position(&Scope::Canon)?;
                    let mut warnings = vec![];
                    let mut views = vec![];
                    for (n, _) in r.store().refs_with_prefix("view/")? {
                        let name = n.trim_start_matches("view/").to_string();
                        if name.contains(':') {
                            warnings.push(format!(
                                "view {name:?} has ':' in its name (reserved for scopes since addendum A): read it as view:{name}, propose it with --view view:{name}; it can't receive new writes"
                            ));
                        }
                        let changes = r.store().view(&name)?.changes.len();
                        let pos = r.version_position(&Scope::View(name.clone()))?;
                        views.push(
                            json!({"name": name, "changes": changes, "version_position": pos}),
                        );
                    }
                    let suspects = r.suspects(&Scope::Canon)?.len();
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
                        json!({"pack": r.pack().name, "landings": landings, "suspects": suspects, "version_position": canon, "views": views, "warnings": warnings}),
                        text,
                    ))
                }
                Cmd::Show { id, scope } => {
                    let bid = BlockId::parse(&id).map_err(|e| {
                        RdyError::new(E_BAD_INPUT, e.to_string(), "ids look like b-7KQ2M")
                    })?;
                    let scope = r.resolve_scope(&scope)?;
                    let ctx = r.context(&scope, &bid)?;
                    let text = format!(
                        "{} {} (version {})\n{}\n{}",
                        ctx.id,
                        ctx.block_type,
                        hash_short(&ctx.version),
                        serde_json::to_string_pretty(&ctx.fields).unwrap_or_default(),
                        ctx.body
                    );
                    Ok((serde_json::to_value(&ctx).expect("json"), text))
                }
                Cmd::Log => {
                    let mut rows = vec![];
                    let mut text = String::new();
                    let mut tags: std::collections::HashMap<String, Vec<String>> =
                        Default::default();
                    for v in r.versions()? {
                        tags.entry(v.landing.clone()).or_default().push(v.name);
                    }
                    for (id, rec) in r.store().landings()? {
                        let p: rdy_model::Proposal = r.store().get(&rec.proposal)?;
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
                        rows.push(json!({"landing": id.to_string(), "title": p.title, "approved_by": rec.approved_by.actor, "at_ms": rec.approved_by.at_ms, "changes": p.changes.len(), "tags": names}));
                    }
                    Ok((Value::Array(rows), text))
                }
                Cmd::Impact { id, depth, scope } => {
                    let bid = BlockId::parse(&id).map_err(|e| {
                        RdyError::new(E_BAD_INPUT, e.to_string(), "ids look like b-7KQ2M")
                    })?;
                    let scope = r.resolve_scope(&scope)?;
                    let items = r.impact(&scope, &bid, depth)?;
                    let text = items
                        .iter()
                        .map(|i| {
                            format!(
                                "{}{} {}: {}",
                                "  ".repeat(i.depth as usize),
                                i.id,
                                i.block_type,
                                i.summary
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
                    let landing = r.land(&human(Role::Approver), &proposal)?;
                    Ok((
                        json!({"landing": landing}),
                        format!("landed {}", hash_short(&landing)),
                    ))
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
                    let versions = r.versions()?;
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
                    let bid = BlockId::parse(&id).map_err(|e| {
                        RdyError::new(E_BAD_INPUT, e.to_string(), "ids look like b-7KQ2M")
                    })?;
                    let block = r.block(&Scope::View(view.clone()), &bid)?;
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
                    let scope = r.resolve_scope(&scope)?;
                    let s = rdy_render::render_site(&mut r, &scope, &out)?;
                    Ok((
                        serde_json::to_value(&s).expect("json"),
                        format!("rendered {} pages to {}", s.pages, out.display()),
                    ))
                }
                Cmd::Export { out, scope } => {
                    let scope = r.resolve_scope(&scope)?;
                    let s = r.export(&scope, &out)?;
                    Ok((
                        serde_json::to_value(&s).expect("json"),
                        format!(
                            "exported {} docs, {} blocks, {} blobs",
                            s.docs, s.blocks, s.blobs
                        ),
                    ))
                }
                Cmd::Import { dir, view } => {
                    let res = r.import(&human(Role::Approver), &view, &dir)?;
                    Ok((
                        serde_json::to_value(&res).expect("json"),
                        format!("imported {} blocks into view {view}", res.created.len()),
                    ))
                }
                Cmd::Reindex => {
                    r.store_mut().reindex()?;
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
            }
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let as_json = cli.json;
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
