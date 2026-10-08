use clap::{ArgGroup, Parser, Subcommand};
use rdy_daemon::{
    Attempt, Client, E_DAEMON_GONE, ErrorReply, Hello, OpContext, PROTO_VERSION, Proto, RunDir,
    STOP_GRACE, ServeError, ServeOptions, StartOptions, connect, connect_or_start, dispatch,
    process_alive, serve, serve_command, spawn_serve,
};
use rdy_engine::engine::Local;
use rdy_engine::wc::state::{Place, WcState};
use rdy_engine::{
    E_BAD_INPUT, E_CONFIG, E_IO, E_NO_PROJECT, Rdy, RdyError, Session, check_path, classify,
    human_actor_for_env, project_pack, serve_config,
};
use rdy_model::Role;
use serde_json::{Value, json};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

mod alt_cmd;
mod brief_text;
mod config_cmd;
mod history_text;
mod intent_cmd;
mod view_cmd;
mod wc;

/// This binary's version: the hello's `version`, so a client only talks to a daemon of its own.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often `rdy serve --stop` looks for the daemon to be gone.
const STOP_POLL: Duration = Duration::from_millis(50);

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
    /// Open the store in this process instead of going through the project's daemon (also on
    /// when RDY_NO_DAEMON=1).
    #[arg(long, global = true)]
    no_daemon: bool,
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
        /// Also list every version: its change, author, landing and approver.
        #[arg(long)]
        blame: bool,
    },
    Log {
        /// At most this many landings, newest first.
        #[arg(long)]
        limit: Option<usize>,
        /// Only landings after this one: a landing id (at least 6 hex characters), landing:<id> or tag:<version>.
        #[arg(long)]
        since: Option<String>,
    },
    Impact {
        /// An id or a path such as wallets/transactions.
        id: String,
        #[arg(long, default_value_t = 3)]
        depth: u32,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long, default_value = "canon")]
        scope: String,
    },
    /// Brief on a file, a directory or a block of the working copy (and record the briefing for the next commit), or on a file in a linked code repo with --code.
    #[command(group(ArgGroup::new("what").required(true).multiple(false).args(["target", "code"])))]
    Brief {
        /// A doc file, a folder or a directory of the working copy, or a block id or key. Outside a working copy, a target with a `/` is a path.
        target: Option<String>,
        /// A file in a linked code repo: relative to the repo's top folder, or a path from here.
        #[arg(long = "code")]
        code: Option<String>,
        /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name> (with --code).
        #[arg(long, requires = "code")]
        scope: Option<String>,
        /// The budget for the briefing; the longest lists are cut to fit.
        #[arg(long = "max-tokens")]
        max_tokens: Option<usize>,
    },
    /// Find text in blocks: a literal (or, with --regex, a pattern) in every block's body and string fields.
    Search {
        /// The text to find (a pattern with --regex).
        query: String,
        /// Treat the query as a regular expression.
        #[arg(long)]
        regex: bool,
        /// Match regardless of case.
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// canon (the default), an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long)]
        scope: Option<String>,
        /// The budget for this page of matches.
        #[arg(long = "max-tokens")]
        max_tokens: Option<usize>,
        /// Continue a cut search: the cursor its last page printed.
        #[arg(long)]
        page: Option<String>,
        /// Keep this block type (repeat for more).
        #[arg(long = "type")]
        types: Vec<String>,
        /// Keep blocks with this trait (repeat for more).
        #[arg(long = "trait")]
        traits: Vec<String>,
        /// Keep blocks under this doc or folder path.
        #[arg(long)]
        path: Option<String>,
        /// draft, stable or deprecated, as inherited.
        #[arg(long)]
        lifecycle: Option<String>,
        /// A version, tag:<version> or landing:<id>: keep blocks changed since.
        #[arg(long = "changed-since")]
        changed_since: Option<String>,
        /// Leave out matches inside superseded, rejected or revoked decisions.
        #[arg(long = "exclude-history")]
        exclude_history: bool,
        /// Keep blocks this decision governs.
        #[arg(long = "governed-by")]
        governed_by: Option<String>,
        /// Keep blocks that describe this block.
        #[arg(long)]
        describes: Option<String>,
        /// Keep blocks linked to or from this block.
        #[arg(long = "linked-to")]
        linked_to: Option<String>,
        /// Keep only blocks with an open suspect.
        #[arg(long)]
        suspect: bool,
    },
    /// List the suspects in a scope (default canon), as rdy_suspects does.
    Suspects {
        /// canon (the default), an existing view's name, tag:<version>, landing:<id> or view:<name>.
        #[arg(long)]
        scope: Option<String>,
    },
    Propose {
        #[arg(long)]
        title: String,
        /// The view to propose (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
        /// Your plain-words summary for the approver.
        #[arg(long)]
        summary: Option<String>,
        /// The alternative set (as-<id>) the view is a member of; goes with --chosen.
        #[arg(long)]
        alt: Option<String>,
        /// The view's own member of that set (its label); goes with --alt.
        #[arg(long)]
        chosen: Option<String>,
    },
    /// Approve a proposal version: P-<id>@<n>, P-<id> (its latest version) or a version hash.
    Approve {
        proposal: String,
        /// The agent that relays the human's approval, as actor[/run]. Records a relayed approval.
        #[arg(long, requires = "evidence")]
        relayed_by: Option<String>,
        /// The human's words, quoted. Requires --relayed-by and the exact version.
        #[arg(long, requires = "relayed_by")]
        evidence: Option<String>,
        /// The question the human was asked.
        #[arg(long, requires = "relayed_by")]
        question: Option<String>,
        /// Where the human said it (default: cli).
        #[arg(long = "session", requires = "relayed_by")]
        session_ref: Option<String>,
        /// Refuse the approval when canon has moved since the version was proposed.
        #[arg(long)]
        strict: bool,
    },
    /// Create, fork, include, exclude, compare, list, show, close and hand over views.
    View {
        #[command(subcommand)]
        cmd: view_cmd::ViewCmd,
    },
    /// Create, change, list, compare and abandon alternative sets: views that answer one question.
    Alt {
        #[command(subcommand)]
        cmd: alt_cmd::AltCmd,
    },
    /// Post, renew, withdraw and list intents: advisory notes on what you mean to change.
    Intent {
        #[command(subcommand)]
        cmd: intent_cmd::IntentCmd,
    },
    /// Undo the newest op of this session on a view, or the named op.
    Undo {
        /// The view (default: the session's own).
        #[arg(long)]
        view: Option<String>,
        /// The view op to undo (default: this session's newest undoable op).
        op: Option<String>,
        /// Also exclude the included changes that depend on the ones the undo excludes.
        #[arg(long)]
        cascade: bool,
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
        /// The view to edit in (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
        /// Why the edit is made (an editorial clarify). A tracking field of an accepted or
        /// rejected decision changes only with one.
        #[arg(long)]
        reason: Option<String>,
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
    Fsck {
        /// Also remove the quarantine files a crash left (.torn- segment tails and damaged indexes). Human only.
        #[arg(long)]
        clean_quarantine: bool,
    },
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
    /// Keys: short names for docs and folders.
    Keys {
        #[command(subcommand)]
        cmd: KeysCmd,
    },
    /// Run the project's daemon in the foreground (every other command starts one as needed).
    Serve {
        /// Start the daemon in the background and return once it is ready.
        #[arg(long, conflicts_with = "stop")]
        detach: bool,
        /// Stop the project's daemon.
        #[arg(long)]
        stop: bool,
        /// Set by a client that spawns the daemon: serve here, detached, with the idle timeout.
        #[arg(long, hide = true, requires = "detach")]
        spawned: bool,
    },
    /// Show the settings in effect, one row per key (no daemon is started).
    Config {
        /// Also name the file each value comes from, or `default`.
        #[arg(long)]
        show_origin: bool,
    },
    #[command(flatten)]
    Wc(wc::WcCmd),
}

#[derive(Subcommand)]
enum NewCmd {
    Folder {
        path: String,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
    Doc {
        path: String,
        #[arg(long = "type")]
        doc_type: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Give every doc and folder without a key the key derived from its path, in one change.
    Assign {
        /// The view (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
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

fn is_project(dir: &Path) -> bool {
    dir.join(".rdy/config.yaml").exists()
}

/// `E_NO_PROJECT` for `dir` unless it is a project, in the words the engine's open uses: checked
/// before a daemon is started there.
fn require_project(dir: &Path) -> Result<(), ErrorReply> {
    if is_project(dir) {
        return Ok(());
    }
    Err(RdyError::new(
        E_NO_PROJECT,
        format!("{} is not an rdy project", dir.display()),
        "run rdy init in the project directory",
    )
    .into())
}

fn hash_short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

/// The string at `v`, or "".
fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The number at `v`, or 0.
fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// A path argument as the JSON string an op takes; a path that is not UTF-8 cannot be sent.
fn path_arg(p: &Path) -> Result<String, ErrorReply> {
    p.to_str().map(String::from).ok_or_else(|| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("{} is not valid UTF-8", p.display()),
            "rename the path, or pass one that is valid UTF-8",
        )
    })
}

fn io_error(e: std::io::Error, fix: &str) -> ErrorReply {
    RdyError::new(E_IO, e.to_string(), fix).into()
}

/// Where a command runs: the project, the caller's working directory, and whether it goes
/// through the daemon.
struct Env {
    project: PathBuf,
    cwd: PathBuf,
    no_daemon: bool,
    /// The working copy the command runs in: the one `cwd` is inside when it belongs to the
    /// command's project, else the project's own.
    place: Option<Place>,
}

/// The ops that write to a view; for the human, the bound view is their default view (R-P10).
const BOUND_VIEW_OPS: &[&str] = &[
    "new_folder",
    "new_doc",
    "block",
    "edit_block",
    "propose",
    "keys_assign",
];

/// The view the working copy is bound to, read from `bound.yaml` without the lock; `None` when
/// it is not bound (or the file does not read).
fn bound_view(env: &Env) -> Option<String> {
    let place = env.place.as_ref()?;
    Some(WcState::load_bound(place).ok()??.view)
}

/// Whether the command runs as the human: `RDY_ACTOR` unset or naming the human actor.
fn human_session(env: &Env) -> bool {
    match std::env::var("RDY_ACTOR") {
        Ok(a) => a == human_actor_for_env(&env.project),
        Err(_) => true,
    }
}

/// `args` with the bound view as `view` for an op in `BOUND_VIEW_OPS` that names none, and for
/// `brief` naming neither `view` nor `scope`; only in a human session.
fn with_bound_view(env: &Env, op: &str, mut args: Value) -> Value {
    let wants = if op == "brief" {
        args["view"].is_null() && args["scope"].is_null()
    } else {
        BOUND_VIEW_OPS.contains(&op) && args["view"].is_null()
    };
    if wants
        && (args.is_object() || args.is_null())
        && human_session(env)
        && let Some(view) = bound_view(env)
    {
        args["view"] = Value::String(view);
    }
    args
}

fn runtime() -> Result<tokio::runtime::Runtime, ErrorReply> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| io_error(e, "retry"))
}

/// `connect_or_start` options for `project`: this binary spawns the daemon (`rdy serve --detach
/// --spawned`), and each `starting` progress text goes to stderr when it is a terminal.
fn start_options(project: &Path, cwd: &Path, proto: Proto) -> StartOptions {
    let spawn_project = project.to_path_buf();
    StartOptions {
        version: VERSION.to_string(),
        proto,
        cwd: cwd.to_path_buf(),
        start_timeout: serve_config(project).start_timeout,
        spawn: Box::new(move |run| {
            spawn_serve(&mut serve_command(
                &std::env::current_exe()?,
                &spawn_project,
                run,
            )?)
        }),
        progress: Box::new(|p| {
            if std::io::stderr().is_terminal() {
                eprintln!("starting: {p}");
            }
        }),
    }
}

/// An open session on the project: the store opened in this process (`--no-daemon`), or a
/// connection to the project's daemon.
enum Conn {
    Direct {
        local: Box<Local>,
        session: Session,
        cwd: PathBuf,
    },
    /// `client` before `rt`: the connection drops while its runtime still runs. `pending` is
    /// the notice the `session` reply carried, printed with the first op's.
    Daemon {
        client: Client,
        rt: tokio::runtime::Runtime,
        pending: Option<String>,
    },
}

impl Conn {
    fn open(env: &Env) -> Result<Conn, ErrorReply> {
        let actor = std::env::var("RDY_ACTOR").ok();
        let run = std::env::var("RDY_RUN").ok();
        if env.no_daemon {
            let local = Local::open(&env.project)?;
            match classify(
                &human_actor_for_env(&env.project),
                actor.as_deref(),
                run.as_deref(),
                Role::Writer,
            ) {
                Ok(session) => Ok(Conn::Direct {
                    local: Box::new(local),
                    session,
                    cwd: env.cwd.clone(),
                }),
                Err(e) => {
                    close_local(local);
                    Err(e.into())
                }
            }
        } else {
            require_project(&env.project)?;
            let rt = runtime()?;
            let run_dir = RunDir::of_project(&env.project);
            let opts = start_options(&env.project, &env.cwd, Proto::Ctl);
            let mut client = rt.block_on(async {
                let mut c = connect_or_start(&run_dir, opts).await?;
                c.ctl("session", json!({"actor": actor, "run": run}))
                    .await?;
                Ok::<Client, ErrorReply>(c)
            })?;
            // a pack_changed landing between ready and the session op rides on its reply
            let pending = client.take_notice();
            Ok(Conn::Daemon {
                rt,
                client,
                pending,
            })
        }
    }

    fn op(&mut self, op: &str, args: Value) -> Result<Value, ErrorReply> {
        match self {
            Conn::Direct {
                local,
                session,
                cwd,
            } => {
                let cx = OpContext {
                    engine: &**local,
                    session,
                    cwd,
                };
                dispatch(&cx, op, args).map_err(ErrorReply::from)
            }
            Conn::Daemon {
                rt,
                client,
                pending,
            } => {
                let out = rt.block_on(client.ctl(op, args));
                for t in notices(pending.take(), client.take_notice()) {
                    eprintln!("notice: {t}");
                }
                out
            }
        }
    }

    fn close(self) {
        match self {
            Conn::Direct { local, .. } => close_local(*local),
            Conn::Daemon { client, .. } => client.close(),
        }
    }
}

/// The notices to print after an op: the one the `session` reply carried (if any, and not the
/// same standing `pack_error` the op's reply repeats), then the op's.
fn notices(session: Option<String>, op: Option<String>) -> Vec<String> {
    let mut out: Vec<String> = session.into_iter().collect();
    if let Some(t) = op
        && !out.contains(&t)
    {
        out.push(t);
    }
    out
}

/// Closes a directly opened store. A failed close does not fail the command (its writes are
/// durable in the log, and the next open catches the index up), but it is reported on stderr.
fn close_local(local: Local) {
    if let Err(e) = local.close() {
        eprintln!(
            "warning: the store did not close cleanly ({e}); the next rdy command catches its index up from the log"
        );
    }
}

/// Runs one ctl op on a session of its own.
fn call(env: &Env, op: &str, args: Value) -> Result<Value, ErrorReply> {
    let args = with_bound_view(env, op, args);
    let mut c = Conn::open(env)?;
    let out = c.op(op, args);
    c.close();
    out
}

fn ls_text(nodes: &Value, depth: usize, out: &mut Vec<String>) {
    for n in nodes.as_array().into_iter().flatten() {
        let block_type = text(&n["type"]);
        let mut line = format!(
            "{}{}{}  {}  {}",
            "  ".repeat(depth),
            text(&n["name"]),
            if block_type == "folder" { "/" } else { "" },
            block_type,
            text(&n["id"])
        );
        if let Some(t) = n["doc_type"].as_str() {
            line.push_str(&format!("  {t}"));
        }
        if let Some(l) = n["lifecycle"].as_str() {
            line.push_str(&format!(" · {l}"));
        }
        if num(&n["collapsed"]) > 0 {
            line.push_str(&format!("  (+{} more)", num(&n["collapsed"])));
        }
        out.push(line);
        ls_text(&n["children"], depth + 1, out);
    }
}

/// The `warning:` lines for what the store's open left behind: quarantine files (`status` and
/// `fsck` replies). An unclean shutdown is no warning: the open recovered from it.
fn open_warnings(v: &Value) -> String {
    let mut out = String::new();
    let names: Vec<&str> = v["quarantine"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !names.is_empty() {
        out.push_str(&format!("\nwarning: quarantine: {}", names.join(", ")));
    }
    out
}

fn warning_lines(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|w| format!("\nwarning: {}", text(w)))
        .collect()
}

fn status_text(v: &Value) -> String {
    let views = v["views"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            format!(
                "  {}: {}",
                text(&w["name"]),
                text(&w["version_position"]["label"])
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!(
        "pack {}\ncanon: {} ({} landings, {} suspects)\nviews:\n{views}",
        text(&v["pack"]),
        text(&v["version_position"]["label"]),
        num(&v["landings"]),
        num(&v["suspects"]),
    );
    let caught_up = num(&v["open_report"]["caught_up_frames"]);
    if caught_up > 0 {
        out.push_str(&format!("\nopen: caught up {caught_up} frames"));
    }
    out.push_str(&warning_lines(&v["warnings"]));
    out.push_str(&open_warnings(v));
    out
}

fn show_text(v: &Value) -> String {
    if v["proposal"].is_object() {
        // the printer ends every output with a newline; proposal_text already does
        return rdy_engine::proposal_text(&v["proposal"])
            .trim_end_matches('\n')
            .to_string();
    }
    let status = v["status_words"].as_str();
    let mut fields = v["fields"].clone();
    if status.is_some()
        && let Some(f) = fields.as_object_mut()
    {
        // the stored status ("accepted") would contradict the derived one
        f.remove("status");
    }
    let mut out = format!(
        "{} {} (version {})\n",
        text(&v["id"]),
        text(&v["block_type"]),
        hash_short(text(&v["version"]))
    );
    if let Some(s) = status {
        out.push_str(&format!("status: {s}\n"));
    }
    if let Some(line) = history_text::last_change_line(&v["history"][0]) {
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(from) = v["effective_from"].as_str() {
        out.push_str(&format!("effective from: {from}\n"));
    }
    // trk1: the type's tracking fields, which stay writable once the decision is decided
    if let Some(names) = v["tracking"].as_array().filter(|a| !a.is_empty()) {
        let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
        out.push_str(&format!("tracking: {}\n", names.join(", ")));
    }
    out.push_str(&format!(
        "{}\n{}",
        serde_json::to_string_pretty(&fields).unwrap_or_default(),
        text(&v["body"])
    ));
    if v["conflict"].is_object() {
        out.push('\n');
        out.push_str(&view_cmd::conflict_text(v));
    }
    // cgr1: in the inflight scope, the in-flight changes that write the block
    if let Some(rows) = v["inflight"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("\nin flight:");
        for c in rows {
            let who = match c["run"].as_str() {
                Some(run) => format!("{}/{run}", text(&c["actor"])),
                None => text(&c["actor"]).to_string(),
            };
            let views: Vec<&str> = c["views"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut line = format!(
                "\n  {} {who} in {}",
                hash_short(text(&c["change"])),
                views.join(", ")
            );
            if let Some(i) = c["intent"].as_str() {
                line.push_str(&format!("; intent {i}"));
            }
            if c["alternative"].is_object() {
                line.push_str(&format!(
                    "; alternative {} of {}",
                    text(&c["alternative"]["member"]),
                    text(&c["alternative"]["set"])
                ));
            }
            let proposals: Vec<&str> = c["proposals"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !proposals.is_empty() {
                line.push_str(&format!("; proposed in {}", proposals.join(", ")));
            }
            out.push_str(&line);
        }
    }
    out
}

fn log_text(v: &Value) -> String {
    let rows = v.as_array().map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return "no landings".to_string();
    }
    let mut out = String::new();
    for row in rows {
        out.push_str(&history_text::log_head_line(row));
        out.push('\n');
        // malt1: the alternative the landing chose
        if let Some(member) = row["chosen"]["member"].as_str() {
            out.push_str(&format!(
                "    chose {member} of {}\n",
                text(&row["chosen"]["set"])
            ));
        }
        let (ev, relay) = (&row["evidence"], &row["relay_evidence"]);
        if ev.is_object() || relay.is_object() {
            let mut line = format!("    evidence ({})", text(&ev["kind"]));
            if relay.is_object() {
                line.push_str(&format!(": \"{}\"", text(&relay["quoted"])));
                match relay["question"].as_str() {
                    Some(q) => line.push_str(&format!(
                        " (asked: \"{q}\"; session {})",
                        text(&relay["session_ref"])
                    )),
                    None => line.push_str(&format!(" (session {})", text(&relay["session_ref"]))),
                }
            } else if let Some(c) = ev["choice"].as_str() {
                line.push_str(&format!(": chose \"{c}\""));
            }
            out.push_str(&line);
            out.push('\n');
        }
        let from: Vec<&str> = row["rebased_from"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(hash_short)
            .collect();
        if !from.is_empty() {
            out.push_str(&format!("    rebased from {}\n", from.join(", ")));
        }
    }
    out
}

fn impact_text(v: &Value) -> String {
    let mut lines: Vec<String> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            format!(
                "depth {}  {} {}: {}  [{} {}]",
                num(&i["depth"]),
                text(&i["id"]),
                text(&i["block_type"]),
                text(&i["summary"]),
                text(&i["via"]["kind"]),
                text(&i["via"]["from"])
            )
        })
        .collect();
    if let Some(areas) = v["by_area"].as_object().filter(|a| !a.is_empty()) {
        let parts: Vec<String> = areas
            .iter()
            .map(|(n, c)| format!("{n} ({})", num(c)))
            .collect();
        lines.push(format!("areas: {}", parts.join(", ")));
    }
    lines.join("\n")
}

/// csrch1: the matching blocks, one header per block and its matching lines under it.
fn search_text(v: &Value) -> String {
    let mut out = vec![format!("{} matching blocks", num(&v["total"]))];
    let docs = v["totals"]["docs"].as_array().cloned().unwrap_or_default();
    if !docs.is_empty() {
        let mut line = format!(
            "by doc: {}",
            docs.iter()
                .map(|d| format!("{} {}", d["path"].as_str().unwrap_or("-"), num(&d["hits"])))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let more = num(&v["totals"]["more_docs"]);
        if more > 0 {
            line.push_str(&format!(", … +{more} more docs"));
        }
        out.push(line);
    }
    if let Some(types) = v["totals"]["types"].as_object().filter(|t| !t.is_empty()) {
        out.push(format!(
            "by type: {}",
            types
                .iter()
                .map(|(t, n)| format!("{t} {}", num(n)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for m in v["matches"].as_array().into_iter().flatten() {
        let path = m["path"].as_str().unwrap_or("-");
        let mut head = format!("{path}  {} ({})", text(&m["id"]), text(&m["type"]));
        if m["all_words"].as_bool() == Some(true) {
            head.push_str(" [all words]");
        }
        if m["history"].as_bool() == Some(true) {
            head.push_str(" [history]");
        }
        out.push(head);
        for l in m["lines"].as_array().into_iter().flatten() {
            out.push(format!(
                "  {}:{}: {}",
                text(&l["field"]),
                num(&l["line"]),
                text(&l["text"])
            ));
        }
        let more = num(&m["more_lines"]);
        if more > 0 {
            out.push(format!("  … +{more} more lines"));
        }
        let also = m["also"].as_array().cloned().unwrap_or_default();
        if !also.is_empty() {
            out.push(format!(
                "  also: {}",
                also.iter()
                    .map(|a| format!("{} {}", a["path"].as_str().unwrap_or("-"), text(&a["id"])))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(next) = v["next"].as_str() {
        out.push(format!("next page: --page {next}"));
    }
    out.join("\n")
}

fn suspects_text(v: &Value) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return "no suspects".to_string();
    }
    items
        .iter()
        .map(|i| {
            format!(
                "{} -> {}  {}",
                text(&i["block"]),
                text(&i["target"]),
                text(&i["reason"])
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tags_text(v: &Value) -> String {
    let versions = v["versions"].as_array().cloned().unwrap_or_default();
    if versions.is_empty() {
        return "no versions yet".to_string();
    }
    let current = v["current"].as_str();
    versions
        .iter()
        .map(|t| {
            let mut line = format!("{}  {}", text(&t["name"]), hash_short(text(&t["landing"])));
            if let Some(m) = t["message"].as_str() {
                line.push_str(&format!("  {m}"));
            }
            if Some(text(&t["name"])) == current {
                line.push_str("  (current)");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `rdy edit`: the block as YAML from the `block` op, its briefing from the `brief` op shown as
/// `#` lines above the YAML, `$EDITOR` on a temp file, then the `edit_block` op citing the
/// briefing's token, with `reason` as the edit's editorial reason (a briefing that moved while the editor was open is `E_BRIEF_STALE`, and the
/// edit is kept). Each op runs on a session of its own: the editor may stay open for long, and
/// the daemon may stop meanwhile (`rdy serve --stop` after a rebuild), so the write connects
/// afresh (starting a daemon again if need be); it is sent once and never retried. When the write
/// fails, the edited file is kept and named in the error (`fix`, and `saved_edit` in JSON).
fn edit(
    env: &Env,
    id: &str,
    view: Option<&str>,
    reason: Option<&str>,
) -> Result<Value, ErrorReply> {
    let block = call(env, "block", json!({"id": id, "view": view}))?;
    // cbrf1 (Q6c): brief the block, show the briefing as `#` lines above the YAML and cite its
    // token
    let briefing = call(
        env,
        "brief",
        json!({"ids": [text(&block["id"])], "view": view}),
    )?;
    let tmp = tempfile::Builder::new()
        .prefix("rdy-edit-")
        .suffix(".yaml")
        .tempfile()
        .map_err(|e| io_error(e, "check the temp directory"))?;
    std::fs::write(
        tmp.path(),
        format!("{}{}", brief_text::header(&briefing), text(&block["yaml"])),
    )
    .map_err(|e| io_error(e, "check the temp directory"))?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("$EDITOR \"$1\"")
        .arg("sh")
        .arg(tmp.path())
        .env("EDITOR", &editor)
        .status()
        .map_err(|e| {
            ErrorReply::from(RdyError::new(E_IO, format!("{editor}: {e}"), "set $EDITOR"))
        })?;
    if !status.success() {
        return Err(ErrorReply::new(
            E_BAD_INPUT,
            "editor exited with an error; nothing changed",
            "save and quit the editor normally",
        ));
    }
    let yaml = std::fs::read_to_string(tmp.path()).map_err(|e| io_error(e, "retry"))?;
    let brief = text(&briefing["base"]["token"]);
    call(
        env,
        "edit_block",
        json!({"yaml": yaml, "view": view, "brief": brief, "reason": reason}),
    )
    .map_err(|mut e| {
        if let Ok(path) = tmp.into_temp_path().keep() {
            e.fix = format!(
                "{}; your edit is saved at {}: run rdy edit {id} again and paste it in",
                e.fix,
                path.display()
            );
            e.extra
                .insert("saved_edit".into(), json!(path.to_string_lossy()));
        }
        e
    })
}

fn run(cli: Cli, env: &Env) -> Result<(Value, String), ErrorReply> {
    match cli.cmd {
        Cmd::Config { show_origin } => config_cmd::run(&env.project, show_origin),
        Cmd::Wc(c) => wc::run(c, env),
        Cmd::Init { dir, pack } => {
            let dir = dir.or(cli.project).unwrap_or_else(|| env.cwd.clone());
            Rdy::init(&dir, &pack)?;
            Ok((
                json!({"project": dir, "pack": pack}),
                format!("Initialized rdy project in {} (pack {pack})", dir.display()),
            ))
        }
        Cmd::Mcp if !env.no_daemon => {
            // the proxy; the runtime is not waited on at exit (a stdin read may be pending)
            let rt = runtime()?;
            let start_timeout = serve_config(&env.project).start_timeout;
            let code = rt.block_on(rdy_daemon::run_stdio(
                env.project.clone(),
                VERSION.into(),
                start_timeout,
            ));
            rt.shutdown_background();
            std::process::exit(code);
        }
        Cmd::Mcp => {
            let rt = tokio::runtime::Runtime::new().map_err(|e| io_error(e, "retry"))?;
            rt.block_on(rdy_mcp::server::serve_stdio(env.project.clone()))
                .map_err(|e| ErrorReply::new(E_IO, e.to_string(), "check the project path"))?;
            Ok((Value::Null, String::new()))
        }
        Cmd::Pack {
            cmd: PackCmd::Check,
        } => {
            let pack = project_pack(&env.project)?;
            let classes: serde_json::Map<String, Value> = pack
                .block_types
                .keys()
                .map(|t| (t.clone(), json!(pack.class_of(t).as_str())))
                .collect();
            let areas: Vec<Value> = pack
                .areas
                .iter()
                .map(|(n, a)| json!({"name": n, "description": a.description, "paths": a.paths}))
                .collect();
            let mut text = format!("pack {} {} ok", pack.name, pack.version);
            for t in pack.block_types.keys() {
                text.push_str(&format!("\n  {t}  {}", pack.class_of(t).as_str()));
            }
            for (name, a) in &pack.areas {
                text.push_str(&format!("\narea {name}: {}", a.paths.join(", ")));
                if !a.description.is_empty() {
                    text.push_str(&format!(" ({})", a.description));
                }
            }
            Ok((
                json!({"pack": pack.name, "version": pack.version, "types": pack.block_types.len(), "classes": classes, "areas": areas}),
                text,
            ))
        }
        Cmd::Keys {
            cmd: KeysCmd::Assign { view },
        } => {
            let v = call(env, "keys_assign", json!({"view": view}))?;
            let keys = v["keys"].as_object().cloned().unwrap_or_default();
            let mut lines = vec![format!(
                "assigned {} keys in view {}",
                keys.len(),
                v["view"].as_str().unwrap_or_default()
            )];
            for (id, k) in &keys {
                lines.push(format!("  {id}  {}", k.as_str().unwrap_or_default()));
            }
            Ok((v, lines.join("\n")))
        }
        Cmd::Status => {
            let mut v = call(env, "status", json!({}))?;
            let mut t = status_text(&v);
            wc::status::attach(env, &mut v, &mut t)?;
            Ok((v, t))
        }
        Cmd::Show { id, scope, blame } => {
            let v = call(
                env,
                "show",
                json!({"id": id, "scope": scope, "blame": blame}),
            )?;
            let mut t = show_text(&v);
            if blame {
                t.push_str(&history_text::blame_text(&v));
            }
            Ok((v, t))
        }
        Cmd::New { what } => {
            let (kind, path, v) = match what {
                NewCmd::Folder { path, view } => {
                    let v = call(env, "new_folder", json!({"path": path, "view": view}))?;
                    ("folder", path, v)
                }
                NewCmd::Doc {
                    path,
                    doc_type,
                    title,
                    view,
                } => {
                    let v = call(
                        env,
                        "new_doc",
                        json!({"path": path, "view": view, "doc_type": doc_type, "title": title}),
                    )?;
                    ("doc", path, v)
                }
            };
            let prefix = if kind == "doc" { "d-" } else { "fd-" };
            let last = v["created"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .filter_map(Value::as_str)
                .rfind(|id| id.starts_with(prefix))
                .unwrap_or_default()
                .to_string();
            let t = format!(
                "created {kind} {path} ({last}){}",
                brief_text::consequences_text(&v)
            );
            Ok((v, t))
        }
        Cmd::Ls { path, scope, depth } => {
            let v = call(
                env,
                "ls",
                json!({"path": path, "scope": scope, "depth": depth}),
            )?;
            let mut lines = vec![];
            ls_text(&v, 0, &mut lines);
            Ok((v, lines.join("\n")))
        }
        Cmd::Log { limit, since } => {
            let v = call(env, "log", json!({"limit": limit, "since": since}))?;
            let t = log_text(&v);
            Ok((v, t))
        }
        Cmd::Impact { id, depth, scope } => {
            let v = call(
                env,
                "impact",
                json!({"id": id, "depth": depth, "scope": scope}),
            )?;
            let t = impact_text(&v);
            Ok((v, t))
        }
        Cmd::Brief {
            target,
            code,
            scope,
            max_tokens,
        } => {
            if let Some(target) = target {
                return wc::brief::run(env, target, max_tokens);
            }
            let v = call(
                env,
                "brief",
                json!({"code_path": code, "scope": scope, "max_tokens": max_tokens}),
            )?;
            let t = format!("{}{}", brief_text::code_text(&v), brief_text::plain(&v));
            Ok((v, t))
        }
        Cmd::Search {
            query,
            regex,
            ignore_case,
            scope,
            max_tokens,
            page,
            types,
            traits,
            path,
            lifecycle,
            changed_since,
            exclude_history,
            governed_by,
            describes,
            linked_to,
            suspect,
        } => {
            // Empty lists and false flags are sent as absent.
            let list = |v: Vec<String>| (!v.is_empty()).then_some(v);
            let flag = |b: bool| b.then_some(true);
            let v = call(
                env,
                "search",
                json!({"query": query, "regex": regex, "ignore_case": ignore_case, "scope": scope, "max_tokens": max_tokens, "page": page,
                    "types": list(types), "traits": list(traits), "path": path, "lifecycle": lifecycle, "changed_since": changed_since,
                    "exclude_history": flag(exclude_history), "governed_by": governed_by, "describes": describes, "linked_to": linked_to,
                    "suspect": flag(suspect)}),
            )?;
            let t = search_text(&v);
            Ok((v, t))
        }
        Cmd::Suspects { scope } => {
            let v = call(env, "suspects", json!({"scope": scope}))?;
            let t = suspects_text(&v);
            Ok((v, t))
        }
        Cmd::Propose {
            title,
            view,
            summary,
            alt,
            chosen,
        } => {
            let alternatives = match (alt, chosen) {
                (Some(set), Some(chosen)) => Some(json!({"set": set, "chosen": chosen})),
                (None, None) => None,
                _ => {
                    return Err(ErrorReply::new(
                        E_BAD_INPUT,
                        "--alt and --chosen go together",
                        "pass both: --alt <set> --chosen <member>, or neither",
                    ));
                }
            };
            let v = call(
                env,
                "propose",
                json!({"title": title, "view": view, "summary": summary, "alternatives": alternatives}),
            )?;
            let pref = text(&v["ref"]);
            let mut t = format!("proposal {pref} ({} changes)", num(&v["changes"]));
            if let Some(s) = v["summary"].as_str() {
                t.push_str(&format!("\nsummary: {s}"));
            }
            if let Some(member) = v["alternative"]["member"].as_str() {
                t.push_str(&format!(
                    "\nchooses {member} of {}",
                    text(&v["alternative"]["set"])
                ));
            }
            let blocking = v["blocking"].as_array().map_or(0, Vec::len);
            if blocking > 0 {
                t.push_str(&format!(
                    "\nblocking: {blocking} conflict(s) on blocks this proposal writes"
                ));
            }
            t.push_str(&brief_text::consequences_text(&v));
            t.push_str(&new_keys_text(&v));
            t.push_str(&format!("\napprove with: rdy approve {pref}"));
            Ok((v, t))
        }
        Cmd::Approve {
            proposal,
            relayed_by,
            evidence,
            question,
            session_ref,
            strict,
        } => {
            let v = call(
                env,
                "approve",
                json!({
                    "proposal": proposal,
                    "relayed_by": relayed_by,
                    "evidence": evidence,
                    "question": question,
                    "session_ref": session_ref,
                    "strict": strict.then_some(true),
                }),
            )?;
            let approved = text(&v["approved"]);
            let mut t = format!(
                "approving {approved}: {} ({} change(s))",
                text(&v["title"]),
                num(&v["changes"]),
            );
            if let Some(s) = v["summary"].as_str() {
                t.push_str(&format!("\nsummary: {s}"));
            }
            t.push_str(&format!(
                "\nlanded {} ({approved})",
                hash_short(text(&v["landing"]))
            ));
            if let Some(by) = relayed_by {
                t.push_str(&format!(", relayed by {by}"));
            }
            if let Some(member) = v["chosen"]["member"].as_str() {
                t.push_str(&format!(
                    "\nchose {member} of {}",
                    text(&v["chosen"]["set"])
                ));
                let closed: Vec<&str> = v["closed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !closed.is_empty() {
                    t.push_str(&format!("; closed {}", closed.join(", ")));
                }
            }
            t.push_str(&brief_text::consequences_text(&v));
            for r in v["rebased"].as_array().into_iter().flatten() {
                if num(&r["conflicts"]) > 0 {
                    t.push_str(&format!(
                        "\nrebased {}: {} conflict(s)",
                        text(&r["view"]),
                        num(&r["conflicts"])
                    ));
                }
            }
            t.push_str(&new_keys_text(&v));
            for w in v["warnings"].as_array().into_iter().flatten() {
                t.push_str(&format!(
                    "\nwarning {}: {}",
                    text(&w["code"]),
                    text(&w["message"])
                ));
            }
            Ok((v, t))
        }
        Cmd::View { cmd } => {
            let v = call(env, "view", view_cmd::args_of(&cmd))?;
            let t = view_cmd::render_view(&cmd, &v);
            Ok((v, t))
        }
        Cmd::Alt { cmd } => {
            let v = call(env, "alt", alt_cmd::args_of(&cmd)?)?;
            let t = alt_cmd::render(&cmd, &v);
            Ok((v, t))
        }
        Cmd::Intent { cmd } => {
            let v = call(env, "intent", intent_cmd::args_of(&cmd))?;
            let t = intent_cmd::render(&cmd, &v);
            Ok((v, t))
        }
        Cmd::Undo { view, op, cascade } => {
            let v = call(env, "view", view_cmd::undo_args(view, op, cascade))?;
            let t = view_cmd::render("undo", &v);
            Ok((v, t))
        }
        Cmd::Tag {
            version,
            landing,
            message,
        } => {
            let v = call(
                env,
                "tag",
                json!({"version": version, "landing": landing, "message": message}),
            )?;
            let t = format!(
                "version {} → landing {}",
                text(&v["name"]),
                hash_short(text(&v["landing"]))
            );
            Ok((v, t))
        }
        Cmd::Tags => {
            let v = call(env, "tags", json!({}))?;
            let t = tags_text(&v);
            Ok((v, t))
        }
        Cmd::Edit { id, view, reason } => {
            let v = edit(env, &id, view.as_deref(), reason.as_deref())?;
            // F4 (FW-PR3b): a rename or a top-level name re-derives keys rdy assigned
            let t = format!(
                "change {}{}{}",
                hash_short(text(&v["change"])),
                keys_lines(&v, ""),
                brief_text::consequences_text(&v)
            );
            Ok((v, t))
        }
        Cmd::Render { out, scope } => {
            let v = call(
                env,
                "render",
                json!({"out": path_arg(&out)?, "scope": scope}),
            )?;
            let t = format!(
                "rendered {} pages, {} permalinks and {} assets to {}{}",
                num(&v["pages"]),
                num(&v["redirects"]),
                num(&v["assets"]),
                out.display(),
                warning_lines(&v["warnings"])
            );
            Ok((v, t))
        }
        Cmd::Export { out, scope } => {
            let v = call(
                env,
                "export",
                json!({"out": path_arg(&out)?, "scope": scope}),
            )?;
            let t = format!(
                "exported {} docs in {} folders, {} blocks, {} blobs{}",
                num(&v["docs"]),
                num(&v["folders"]),
                num(&v["blocks"]),
                num(&v["blobs"]),
                warning_lines(&v["warnings"])
            );
            Ok((v, t))
        }
        Cmd::Import { dir, view } => {
            let v = call(env, "import", json!({"dir": path_arg(&dir)?, "view": view}))?;
            let n = v["created"].as_object().map_or(0, |m| m.len());
            let mut t = format!("imported {n} blocks into view {view}");
            for w in v["warnings"].as_array().into_iter().flatten() {
                let doc = w["block"]
                    .as_str()
                    .map_or("", |id| text(&v["warning_docs"][id]));
                t.push_str(&format!(
                    "\nwarning {}: {doc}{}",
                    text(&w["code"]),
                    text(&w["message"])
                ));
            }
            t.push_str(&brief_text::consequences_text(&v));
            Ok((v, t))
        }
        Cmd::Reindex => {
            let v = call(env, "reindex", json!({}))?;
            Ok((v, "index rebuilt".into()))
        }
        Cmd::Fsck { clean_quarantine } => {
            let v = call(env, "fsck", json!({"clean_quarantine": clean_quarantine}))?;
            let mut t = format!(
                "ok: {} frames, {} landings, {} versions",
                num(&v["frames"]),
                num(&v["landings"]),
                num(&v["versions"]),
            );
            for name in v["removed"].as_array().into_iter().flatten() {
                t.push_str(&format!("\nremoved {}", text(name)));
            }
            for s in v["skipped"].as_array().into_iter().flatten() {
                t.push_str(&format!(
                    "\nskipped {}: {}",
                    text(&s["name"]),
                    text(&s["reason"])
                ));
            }
            t.push_str(&open_warnings(&v));
            if v["open_report"]["unclean_shutdown"] == json!(true) {
                t.push_str(&format!(
                    "\nnote: this daemon started after an unclean shutdown and recovered (caught up {} frames)",
                    num(&v["open_report"]["caught_up_frames"])
                ));
            }
            Ok((v, t))
        }
        Cmd::Check { .. } | Cmd::Serve { .. } => unreachable!("handled in main"),
    }
}

/// `rdy check`: inside a project through the `check` op (the pack and canon come from the
/// project's state); outside one, against a built-in pack, with no store.
fn run_check(env: &Env, path: &Path, pack: Option<&str>, as_json: bool) -> ExitCode {
    let result = (|| -> Result<Value, ErrorReply> {
        if is_project(&env.project) {
            call(env, "check", json!({"path": path_arg(path)?, "pack": pack}))
        } else {
            let name = pack.ok_or_else(|| {
                ErrorReply::new(
                    E_NO_PROJECT,
                    "not inside an rdy project",
                    "run rdy check inside a project, or pass --pack rdy-standard",
                )
            })?;
            let pk = rdy_schema::PackSet::with_builtin()
                .resolve(name)
                .map_err(|e| {
                    ErrorReply::new(
                        E_CONFIG,
                        e.to_string(),
                        "pass a built-in pack such as rdy-standard",
                    )
                })?;
            Ok(serde_json::to_value(check_path(&pk, None, path)?).expect("diagnostics serialize"))
        }
    })();
    match result {
        Ok(diags) => {
            let diags = diags.as_array().cloned().unwrap_or_default();
            if as_json {
                println!("{}", Value::Array(diags.clone()));
            } else if diags.is_empty() {
                println!("ok");
            } else {
                for d in &diags {
                    println!(
                        "{}:{}:{}: {} {} (fix: {})",
                        text(&d["file"]),
                        num(&d["line"]),
                        num(&d["column"]),
                        text(&d["code"]),
                        text(&d["message"]),
                        text(&d["fix"])
                    );
                }
            }
            if diags.iter().any(|d| d["severity"] == "error") {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

/// The exit code of a daemon run: 0, or 2 when another rdy process holds the store, else 1. The
/// host already logged an `AlreadyRunning` or `Open` error to stderr; the others are printed
/// here.
fn serve_exit(r: Result<rdy_daemon::ExitReason, ServeError>) -> ExitCode {
    match r {
        Ok(_) => ExitCode::SUCCESS,
        Err(ServeError::AlreadyRunning(_)) => ExitCode::from(2),
        Err(ServeError::Open(_)) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// `rdy serve`, `rdy serve --detach` and `rdy serve --stop`.
fn run_serve(env: &Env, detach: bool, stop: bool, spawned: bool, as_json: bool) -> ExitCode {
    let opts = |detach: bool, idle_timeout| ServeOptions {
        project: env.project.clone(),
        version: VERSION.to_string(),
        detach,
        idle_timeout,
        human_actor: None,
    };
    if stop {
        return match stop_daemon(env) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                print_error(&e, as_json);
                ExitCode::FAILURE
            }
        };
    }
    if detach && spawned {
        let idle = serve_config(&env.project).idle_timeout;
        return serve_exit(serve(opts(true, Some(idle))));
    }
    if detach {
        return match start_detached(env) {
            Ok(pid) => {
                println!("daemon running (pid {pid})");
                ExitCode::SUCCESS
            }
            Err(e) => {
                print_error(&e, as_json);
                ExitCode::FAILURE
            }
        };
    }
    serve_exit(serve(opts(false, None)))
}

/// Starts the project's daemon in the background (or finds it running) and returns its pid, as
/// `.rdy/run/daemon.pid` names it.
fn start_detached(env: &Env) -> Result<String, ErrorReply> {
    require_project(&env.project)?;
    let rt = runtime()?;
    let run = RunDir::of_project(&env.project);
    let client = rt.block_on(connect_or_start(
        &run,
        start_options(&env.project, &env.cwd, Proto::Ctl),
    ))?;
    client.close();
    let pid = std::fs::read_to_string(run.pid_file())
        .map_err(|e| io_error(e, "look at .rdy/run/daemon.log"))?;
    Ok(pid.trim().to_string())
}

/// Stops the project's daemon (never starting one): `stopped` once its socket is gone and its
/// process has exited (so the store is released), or `no daemon running`.
fn stop_daemon(env: &Env) -> Result<&'static str, ErrorReply> {
    let rt = runtime()?;
    let run = RunDir::of_project(&env.project);
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: VERSION.to_string(),
        proto: Proto::Ctl,
        cwd: env.cwd.clone(),
    };
    let start_timeout = serve_config(&env.project).start_timeout;
    let pid = rt.block_on(async {
        let client = match connect(&run, &hello).await {
            Attempt::NoDaemon(_) => return Ok(None),
            Attempt::Ready(c) => Some(c),
            Attempt::Starting(p) => {
                Some(p.wait(Instant::now() + start_timeout, &mut |_| {}).await?)
            }
            // a daemon of another version: it is exiting already
            Attempt::Exiting => None,
            Attempt::Refused(e) => return Err(e),
        };
        let mut pid = None;
        if let Some(mut c) = client {
            pid = c
                .ctl("ping", json!({}))
                .await
                .ok()
                .and_then(|v| v["pid"].as_u64())
                .and_then(|p| u32::try_from(p).ok());
            c.ctl("stop", json!({})).await?;
            c.close();
        }
        Ok(Some(pid))
    })?;
    let Some(pid) = pid else {
        return Ok("no daemon running");
    };
    let deadline = Instant::now() + STOP_GRACE + Duration::from_secs(1);
    let gone = || !run.socket().exists() && pid.is_none_or(|p| !process_alive(p));
    while !gone() {
        if Instant::now() >= deadline {
            return Err(ErrorReply::new(
                E_DAEMON_GONE,
                format!(
                    "the daemon was asked to stop but has not exited within {} s",
                    (STOP_GRACE + Duration::from_secs(1)).as_secs()
                ),
                "look at .rdy/run/daemon.log; run rdy serve --stop again",
            ));
        }
        std::thread::sleep(STOP_POLL);
    }
    Ok("stopped")
}

fn print_error(e: &ErrorReply, as_json: bool) {
    if as_json {
        eprintln!("{}", serde_json::to_string(e).unwrap_or_default());
        return;
    }
    eprintln!("error {}: {}\nfix: {}", e.code, e.message, e.fix);
    if let Some(issues) = e.extra.get("issues").and_then(Value::as_array) {
        for i in issues {
            let block = match i["block"].as_str() {
                Some(b) => format!("Some({b})"),
                None => "None".to_string(),
            };
            eprintln!(
                "  {} {block}: {} (fix: {})",
                text(&i["code"]),
                text(&i["message"]),
                text(&i["fix"])
            );
        }
    }
    for d in e
        .extra
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        eprintln!(
            "{}:{}:{}: {} {} (fix: {})",
            text(&d["file"]),
            num(&d["line"]),
            num(&d["column"]),
            text(&d["code"]),
            text(&d["message"]),
            text(&d["fix"])
        );
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let as_json = cli.json;
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            print_error(&io_error(e, "run from an existing directory"), as_json);
            return ExitCode::FAILURE;
        }
    };
    let project = cli.project.clone().unwrap_or_else(|| find_project(&cwd));
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    let place = Place::locate(&cwd)
        .filter(|p| same(&p.project, &project))
        .unwrap_or_else(|| Place::main(&project));
    let env = Env {
        project,
        cwd,
        no_daemon: cli.no_daemon || std::env::var("RDY_NO_DAEMON").is_ok_and(|v| v == "1"),
        place: Some(place),
    };
    match &cli.cmd {
        Cmd::Check { path, pack } => return run_check(&env, path, pack.as_deref(), as_json),
        Cmd::Serve {
            detach,
            stop,
            spawned,
        } => return run_serve(&env, *detach, *stop, *spawned, as_json),
        _ => {}
    }
    // the config warnings print once per process on stderr; `rdy config --show-origin` and
    // `rdy status` carry them in their own output
    let own_warnings = match &cli.cmd {
        Cmd::Status | Cmd::Mcp => true,
        Cmd::Config { show_origin } => *show_origin && !as_json,
        _ => false,
    };
    match run(cli, &env) {
        Ok((v, text)) => {
            if !own_warnings {
                for w in rdy_engine::config_levels::config_warnings_for_env(&env.project) {
                    eprintln!("warning {}: {}; {}", w.code, w.message, w.fix);
                }
            }
            if v.is_null() {
            } else if as_json {
                println!("{v}");
            } else if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

/// One line per key rdy re-derived on propose or approve (`keys`: block id → new key, K1).
fn new_keys_text(v: &Value) -> String {
    keys_lines(v, " (another block holds the key rdy derived)")
}

/// One `new key <id>: <key><why>` line per entry of `keys` (block id → new key).
fn keys_lines(v: &Value, why: &str) -> String {
    let mut t = String::new();
    for (id, k) in v["keys"].as_object().into_iter().flatten() {
        t.push_str(&format!("\nnew key {id}: {}{why}", text(k)));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::{log_text, notices, show_text};
    use serde_json::json;

    /// Final-review fix round 1: `rdy log` prints the alternative a landing chose, as `rdy
    /// approve` does.
    #[test]
    fn malt1_log_text_prints_the_chosen_alternative() {
        let row = json!({"landing": "ab".repeat(32), "at": "2026-10-07T00:00:00Z", "title": "x",
            "proposed_by": {"actor": "agent:a"}, "approved_by": "human:qa",
            "chosen": {"set": "as-0123456789ab", "member": "a"}});
        let t = log_text(&json!([row]));
        assert!(t.contains("\n    chose a of as-0123456789ab\n"), "{t}");
        let mut plain = row.clone();
        plain.as_object_mut().unwrap().remove("chosen");
        assert!(!log_text(&json!([plain])).contains("chose"));
    }

    /// Final-review fix round 1: `rdy show --scope inflight` prints the in-flight changes that
    /// write the block, as the JSON lists them.
    #[test]
    fn cgr1_show_text_prints_the_inflight_changes() {
        let v = json!({"id": "c-AAAAA", "block_type": "claim", "version": "cd".repeat(32),
        "fields": {}, "body": "b", "inflight": [
            {"change": "ab".repeat(32), "views": ["a", "b"], "actor": "agent:a", "run": "ra",
             "intent": "in-0123456789ab", "alternative": {"set": "as-0123456789ab", "member": "x"},
             "proposals": ["P-0123456789ab@1"]},
            {"change": "ef".repeat(32), "views": ["c"], "actor": "human:qa", "run": null,
             "proposals": []}
        ]});
        let t = show_text(&v);
        assert!(
            t.ends_with("\nin flight:\n  abababababab agent:a/ra in a, b; intent in-0123456789ab; alternative x of as-0123456789ab; proposed in P-0123456789ab@1\n  efefefefefef human:qa in c"),
            "{t}"
        );
        // absent (another scope, or no in-flight change writes the block): no block
        let mut none = v.clone();
        none.as_object_mut().unwrap().remove("inflight");
        assert!(!show_text(&none).contains("in flight"));
    }

    /// Final review fix round 1 (minor 11): a notice carried on the `session` reply is printed,
    /// once, beside the op's.
    #[test]
    fn note1_the_session_replys_notice_is_kept_and_a_standing_one_prints_once() {
        let changed = "pack_changed: the pack was reloaded (generation 1)".to_string();
        let error = "pack_error: bad pack".to_string();
        let some = |t: &String| Some(t.clone());
        assert_eq!(notices(some(&changed), None), vec![changed.clone()]);
        assert_eq!(notices(None, some(&error)), vec![error.clone()]);
        assert_eq!(notices(some(&error), some(&error)), vec![error.clone()]);
        assert_eq!(
            notices(some(&changed), some(&error)),
            vec![changed.clone(), error.clone()]
        );
        assert!(notices(None, None).is_empty());
    }
}
