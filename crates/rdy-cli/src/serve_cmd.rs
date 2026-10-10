use crate::conn::{Env, VERSION, io_error, require_project, runtime, start_options};
use crate::output::print_error;
use rdy_daemon::{
    Attempt, E_DAEMON_GONE, ErrorReply, Hello, PROTO_VERSION, Proto, RunDir, STOP_GRACE,
    ServeError, ServeOptions, connect, connect_or_start, process_alive, serve,
};
use rdy_engine::{E_IO, serve_config};
use serde_json::{Value, json};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How often `rdy serve --stop` looks for the daemon to be gone.
const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(clap::Args)]
pub(crate) struct ServeArgs {
    /// Start the daemon in the background and return once it is ready.
    #[arg(long, conflicts_with = "stop")]
    pub(crate) detach: bool,
    /// Stop the project's daemon.
    #[arg(long)]
    pub(crate) stop: bool,
    /// Set by a client that spawns the daemon: serve here, detached, with the idle timeout.
    #[arg(long, hide = true, requires = "detach")]
    pub(crate) spawned: bool,
}

pub(crate) fn mcp(env: &Env) -> Result<(Value, String), ErrorReply> {
    // before the proxy creates .rdy/run and starts a daemon that would fail at open
    require_project(&env.project)?;
    if !env.no_daemon {
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
    let rt = tokio::runtime::Runtime::new().map_err(|e| io_error(e, "retry"))?;
    rt.block_on(rdy_mcp::server::serve_stdio(env.project.clone()))
        .map_err(|e| ErrorReply::new(E_IO, e.to_string(), "check the project path"))?;
    Ok((Value::Null, String::new()))
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
pub(crate) fn run_serve(
    env: &Env,
    detach: bool,
    stop: bool,
    spawned: bool,
    as_json: bool,
) -> ExitCode {
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
