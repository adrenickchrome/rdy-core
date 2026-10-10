use rdy_daemon::{
    Client, ErrorReply, OpContext, Proto, RunDir, StartOptions, connect_or_start, dispatch,
    serve_command, spawn_serve,
};
use rdy_engine::engine::Local;
use rdy_engine::wc::state::{POINTER_FILE, Place, WcState, pointer_project};
use rdy_engine::{
    E_BAD_INPUT, E_IO, RdyError, Session, classify, human_actor_for_env, serve_config,
};
use rdy_model::Role;
use serde_json::{Value, json};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// This binary's version: the hello's `version`, so a client only talks to a daemon of its own.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether `dir` holds a `.rdy` directory: the main place of a project, as the working copy
/// finds it (`Place::try_locate`).
fn has_rdy_dir(dir: &Path) -> bool {
    fs::metadata(dir.join(".rdy")).is_ok_and(|m| m.is_dir())
}

pub(crate) fn find_project(start: &Path) -> PathBuf {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        // a worktree's pointer file names its project; it is checked before the .rdy directory
        // at each level. A damaged pointer stops the walk at its directory, so
        // `Place::try_locate` reports it instead of the walk reaching the main working copy.
        if fs::symlink_metadata(dir.join(POINTER_FILE)).is_ok_and(|m| m.is_file()) {
            return pointer_project(dir).unwrap_or_else(|| dir.to_path_buf());
        }
        if has_rdy_dir(dir) {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    start.to_path_buf()
}

/// A project: the project config and the store under `.rdy`, as the engine's open checks it.
pub(crate) fn is_project(dir: &Path) -> bool {
    rdy_engine::config_levels::is_project(dir)
}

/// `E_NO_PROJECT` for `dir` unless it is a project, in the words the engine's open uses: checked
/// before a daemon is started there.
pub(crate) fn require_project(dir: &Path) -> Result<(), ErrorReply> {
    rdy_engine::config_levels::require_project(dir).map_err(Into::into)
}

/// A path argument as the JSON string an op takes; a path that is not UTF-8 cannot be sent.
pub(crate) fn path_arg(p: &Path) -> Result<String, ErrorReply> {
    p.to_str().map(String::from).ok_or_else(|| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("{} is not valid UTF-8", p.display()),
            "rename the path, or pass one that is valid UTF-8",
        )
    })
}

pub(crate) fn io_error(e: std::io::Error, fix: &str) -> ErrorReply {
    RdyError::new(E_IO, e.to_string(), fix).into()
}

/// Where a command runs: the project, the caller's working directory, and whether it goes
/// through the daemon.
pub(crate) struct Env {
    pub(crate) project: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) no_daemon: bool,
    /// The working copy the command runs in: the one `cwd` is inside when it belongs to the
    /// command's project, else the project's own.
    pub(crate) place: Option<Place>,
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
/// there is no working copy or it is not bound. A `bound.yaml` that does not read is its error.
pub(crate) fn bound_view(env: &Env) -> Result<Option<String>, ErrorReply> {
    let Some(place) = env.place.as_ref() else {
        return Ok(None);
    };
    Ok(WcState::load_bound(place)?.map(|b| b.view))
}

/// Whether the command runs as the human: `RDY_ACTOR` unset or naming the human actor.
pub(crate) fn human_session(env: &Env) -> bool {
    match std::env::var("RDY_ACTOR") {
        Ok(a) => a == human_actor_for_env(&env.project),
        Err(_) => true,
    }
}

/// `args` with the bound view as `view` for an op in `BOUND_VIEW_OPS` that names none, and for
/// `brief` naming neither `view` nor `scope`; only in a human session.
fn with_bound_view(env: &Env, op: &str, mut args: Value) -> Result<Value, ErrorReply> {
    let wants = if op == "brief" {
        args["view"].is_null() && args["scope"].is_null()
    } else {
        BOUND_VIEW_OPS.contains(&op) && args["view"].is_null()
    };
    if wants
        && (args.is_object() || args.is_null())
        && human_session(env)
        && let Some(view) = bound_view(env)?
    {
        args["view"] = Value::String(view);
    }
    Ok(args)
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, ErrorReply> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| io_error(e, "retry"))
}

/// `connect_or_start` options for `project`: this binary spawns the daemon (`rdy serve --detach
/// --spawned`), and each `starting` progress text goes to stderr when it is a terminal.
pub(crate) fn start_options(project: &Path, cwd: &Path, proto: Proto) -> StartOptions {
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
pub(crate) enum Conn {
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
    pub(crate) fn open(env: &Env) -> Result<Conn, ErrorReply> {
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

    pub(crate) fn op(&mut self, op: &str, args: Value) -> Result<Value, ErrorReply> {
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

    pub(crate) fn close(self) {
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
pub(crate) fn call(env: &Env, op: &str, args: Value) -> Result<Value, ErrorReply> {
    let args = with_bound_view(env, op, args)?;
    let mut c = Conn::open(env)?;
    let out = c.op(op, args);
    c.close();
    out
}

#[cfg(test)]
mod tests {
    use super::notices;

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
