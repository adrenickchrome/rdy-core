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

/// How long `rdy serve --stop` waits for a stopped daemon to exit: its drain (`STOP_GRACE`), then
/// its blocking work (a canon warm-up, a large write) and the store's close.
const STOP_WAIT: Duration = Duration::from_secs(120);

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
        site_lan_override: None,
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

/// Stops the project's daemon (never starting one): `stopped` once it has exited ([`exited`]),
/// or `no daemon running`.
fn stop_daemon(env: &Env) -> Result<&'static str, ErrorReply> {
    let rt = runtime()?;
    let run = RunDir::of_project(&env.project);
    // a project with no `.rdy` has no daemon to stop
    let Ok(project) = run.project_id() else {
        return Ok("no daemon running");
    };
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: VERSION.to_string(),
        proto: Proto::Ctl,
        cwd: env.cwd.clone(),
        project,
    };
    let start_timeout = serve_config(&env.project).start_timeout;
    let stopped = rt.block_on(async {
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
        // read while the daemon runs: it removes its record first when it stops
        let socket = stop_socket(&run);
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
        Ok(Some((pid, socket)))
    })?;
    let Some((pid, socket)) = stopped else {
        return Ok("no daemon running");
    };
    let gone = || exited(pid, || socket.exists(), process_alive);
    let quiet = STOP_GRACE + Duration::from_secs(1);
    let said = |p: Option<u32>| {
        let who = p.map_or_else(String::new, |p| format!(" (pid {p})"));
        eprintln!("waiting for the daemon{who} to finish its work and close the store");
    };
    if !wait_gone(gone, STOP_POLL, quiet, STOP_WAIT, || said(pid)) {
        return Err(ErrorReply::new(
            E_DAEMON_GONE,
            format!(
                "the daemon was asked to stop but has not exited within {} s",
                STOP_WAIT.as_secs()
            ),
            "look at .rdy/run/daemon.log (its last line is \"store closed\" once the store is released); run rdy serve --stop again",
        ));
    }
    Ok("stopped")
}

/// The socket `stop_daemon` watches when it has no pid: the one the daemon bound, which
/// `daemon.sock` names, else this client's own (`RunDir::socket`). A client of another spelling of
/// the project (a symlink, a macOS firmlink) computes another socket, which never exists. Read
/// once, before the stop: the daemon removes its record before its socket.
fn stop_socket(run: &RunDir) -> std::path::PathBuf {
    run.recorded_socket().unwrap_or_else(|| run.socket())
}

/// Whether the daemon `stop_daemon` asked to stop has exited. With its pid known, its process
/// has exited (so the store is released), whatever holds the socket now: a successor may bind it
/// at once. With no pid, its socket is gone.
fn exited(pid: Option<u32>, socket_exists: impl Fn() -> bool, alive: impl Fn(u32) -> bool) -> bool {
    match pid {
        Some(p) => !alive(p),
        None => !socket_exists(),
    }
}

/// Polls `gone` every `poll` until it holds or `bound` has passed. Once `quiet` has passed, calls
/// `slow` once, and always before giving up (so a give-up has said it waited, however late the
/// thread wakes). True when `gone` held.
fn wait_gone(
    gone: impl Fn() -> bool,
    poll: Duration,
    quiet: Duration,
    bound: Duration,
    mut slow: impl FnMut(),
) -> bool {
    let start = Instant::now();
    let mut said = false;
    loop {
        if gone() {
            return true;
        }
        let waited = start.elapsed();
        if !said && (waited >= quiet || waited >= bound) {
            said = true;
            slow();
        }
        if waited >= bound {
            return false;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn mfb4_a_quick_exit_says_nothing() {
        let slow = Cell::new(0);
        let gone = wait_gone(|| true, MS, 200 * MS, 400 * MS, || slow.set(slow.get() + 1));
        assert!(gone);
        assert_eq!(slow.get(), 0);
    }

    #[test]
    fn mfb4_a_slow_exit_waits_and_says_so_once() {
        let slow = Cell::new(0);
        let polls = Cell::new(0u32);
        let said_at = Cell::new(None::<u32>);
        // gone only on the third poll after the first line (counted in polls, never in wall
        // time): two polls past the quiet time run the once-guard before the exit
        let gone = wait_gone(
            || {
                polls.set(polls.get() + 1);
                said_at.get().is_some_and(|at| polls.get() >= at + 3)
            },
            MS,
            5 * MS,
            Duration::from_secs(60),
            || {
                slow.set(slow.get() + 1);
                if said_at.get().is_none() {
                    said_at.set(Some(polls.get()));
                }
            },
        );
        assert!(gone);
        assert!(
            polls.get() >= said_at.get().unwrap() + 3,
            "the wait went on"
        );
        assert_eq!(slow.get(), 1, "one line, however long the wait");
    }

    /// Final review M1: once the pid is known, its exit alone is the stop; a successor daemon
    /// that binds the socket does not make `--stop` wait for a process that exited.
    #[test]
    fn mfb4_a_successor_on_the_socket_does_not_hide_the_exit() {
        // the old daemon exited, and a successor holds the socket
        assert!(exited(Some(7), || true, |_| false));
        assert!(!exited(Some(7), || false, |_| true), "alive is not gone");
        // with no pid, the socket is all there is to go by
        assert!(!exited(None, || true, |_| false));
        assert!(exited(None, || false, |_| true));
    }

    /// df3 fix round 1 (review): with no pid, `--stop` waits for the socket the daemon bound,
    /// which `daemon.sock` names. A client of another spelling computes another socket, which
    /// never exists, so watching it would say `stopped` while the daemon still holds the store.
    #[test]
    fn df3_stop_without_a_pid_waits_for_the_recorded_socket() {
        let d = tempfile::tempdir().unwrap();
        let run = RunDir::of_project(d.path());
        std::fs::create_dir_all(run.path()).unwrap();
        // the daemon bound a socket other than the one this spelling computes
        let bound = d.path().join("bound.sock");
        assert_ne!(bound, run.socket());
        let project = run.project_id().unwrap();
        std::fs::write(
            run.socket_record(),
            format!("{}\n{project}\n", bound.display()),
        )
        .unwrap();
        std::fs::write(&bound, "").unwrap();
        assert!(!run.socket().exists());
        // read once, before the stop
        let socket = stop_socket(&run);
        assert_eq!(socket, bound);
        let gone = || exited(None, || socket.exists(), |_| false);
        assert!(!gone(), "the recorded socket is still there");
        // the daemon removes its record first, then its socket
        std::fs::remove_file(run.socket_record()).unwrap();
        assert!(!gone(), "the socket outlives the record");
        std::fs::remove_file(&bound).unwrap();
        assert!(gone());
        // with no record, the client's own socket is all there is to go by
        assert_eq!(stop_socket(&run), run.socket());
    }

    #[test]
    fn mfb4_a_process_that_never_exits_gives_up_at_the_bound() {
        let slow = Cell::new(0);
        let gone = wait_gone(|| false, MS, 2 * MS, 20 * MS, || slow.set(slow.get() + 1));
        assert!(!gone);
        assert_eq!(slow.get(), 1);
    }

    #[test]
    fn mfb4_the_bound_covers_the_drain_and_the_close() {
        assert_eq!(STOP_WAIT, Duration::from_secs(120));
        assert!(STOP_WAIT > STOP_GRACE + Duration::from_secs(1));
    }
}
