use crate::conn::{Env, VERSION, io_error, require_project, runtime, start_options};
use crate::output::{print_error, text};
use crate::site_cmd::lan_warning;
use rdy_daemon::signing_agent::CallerAgent;
use rdy_daemon::{
    Attempt, Client, E_DAEMON_GONE, ErrorReply, Hello, PROTO_VERSION, ProfileSiteReady, Proto,
    RunDir, STOP_GRACE, ServeError, ServeOptions, StopRequest, connect, connect_backoff,
    connect_or_start, not_ready, process_alive, request_stop, serve, serve_with_profile,
};
use rdy_engine::engine::{SiteFacts, SiteLink, site_reply_value};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{E_IO, E_SITE, RdyError, serve_config};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
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
    // the help is given in full: a doc comment would lose its closing period
    #[arg(
        long,
        conflicts_with_all = ["stop", "detach", "spawned"],
        help = "Serve canon through this profile on a separate port. Starts in the foreground, or reuses the running daemon."
    )]
    pub(crate) profile: Option<String>,
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
    // the session signs through this process's own agent, never a private key file
    let agent = rdy_daemon::signing_transport::direct_agent();
    rt.block_on(rdy_mcp::server::serve_stdio(env.project.clone(), agent))
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
            // a closed stderr loses the line, never panics
            let _ = writeln!(std::io::stderr(), "{e}");
            ExitCode::FAILURE
        }
    }
}

/// `rdy serve`, `rdy serve --detach`, `rdy serve --stop` and `rdy serve --profile`.
pub(crate) fn run_serve(
    env: &Env,
    detach: bool,
    stop: bool,
    spawned: bool,
    profile: Option<&str>,
    as_json: bool,
) -> ExitCode {
    if let Some(profile) = profile {
        return serve_profile(env, profile, as_json);
    }
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

/// What the connection attempt of `rdy serve --profile` found.
enum Reached {
    /// A ready daemon's connection, before its `session` op.
    Client(Client),
    /// No daemon answered: this process serves in the foreground.
    NoDaemon,
    /// A refusal, or a start that did not become ready: printed as it is, exit 1.
    Failed(ErrorReply),
}

/// Prints each `starting` progress text to stderr when it is a terminal, as `start_options`.
fn progress(p: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("starting: {p}");
    }
}

/// The hello of this CLI's ctl connection; `None` when the project has no `.rdy` (no daemon).
fn ctl_hello(env: &Env, run: &RunDir) -> Option<Hello> {
    let project = run.project_id().ok()?;
    Some(Hello {
        rdy: PROTO_VERSION,
        version: VERSION.to_string(),
        proto: Proto::Ctl,
        cwd: env.cwd.clone(),
        project,
    })
}

/// One connection attempt, and its follow-ups, before any store is opened: a starting daemon is
/// waited for, and a daemon exiting to be replaced is asked again (`connect_backoff`) until it
/// is gone or the start deadline passes. Nothing is spawned here.
async fn reach(run: &RunDir, hello: &Hello, start_timeout: Duration) -> Reached {
    let deadline = Instant::now() + start_timeout;
    let mut tries = 0u32;
    loop {
        match connect(run, hello).await {
            Attempt::Ready(c) => return Reached::Client(c),
            Attempt::Starting(p) => {
                return match p.wait(deadline, &mut progress).await {
                    Ok(c) => Reached::Client(c),
                    Err(e) => Reached::Failed(e),
                };
            }
            Attempt::Refused(e) => return Reached::Failed(e),
            Attempt::NoDaemon(_) => return Reached::NoDaemon,
            Attempt::Exiting => {
                let now = Instant::now();
                if now >= deadline {
                    return Reached::Failed(not_ready(run, start_timeout, None));
                }
                tokio::time::sleep(connect_backoff(tries).min(deadline - now)).await;
                tries += 1;
            }
        }
    }
}

/// `rdy serve --profile <PROFILE>` (D-DK03K): the profile-only site of a running daemon, which
/// is reused (exit 0), or of a daemon this process starts and serves in the foreground. A store
/// held by another rdy process that answers as no daemon is exit 2. No detached daemon is ever
/// started, and no other process's daemon or socket is touched.
fn serve_profile(env: &Env, profile: &str, as_json: bool) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            print_error(&e, as_json);
            return ExitCode::FAILURE;
        }
    };
    let run = RunDir::of_project(&env.project);
    let start_timeout = serve_config(&env.project).start_timeout;
    let hello = ctl_hello(env, &run);
    let reached = match &hello {
        Some(h) => rt.block_on(reach(&run, h, start_timeout)),
        None => Reached::NoDaemon,
    };
    match reached {
        Reached::Client(c) => reuse(&rt, c, profile, as_json),
        Reached::Failed(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
        Reached::NoDaemon => {
            let opts = ServeOptions {
                project: env.project.clone(),
                version: VERSION.to_string(),
                detach: false,
                idle_timeout: None,
                human_actor: None,
                site_lan_override: None,
            };
            match serve_with_profile(opts, profile, |ready| print_ready(ready, as_json)) {
                Err(ServeError::AlreadyRunning(_)) => {
                    // a concurrent start may have won the store: use its daemon once it answers
                    match ctl_hello(env, &run).map(|h| rt.block_on(winner(&run, &h, start_timeout)))
                    {
                        Some(Some(c)) => reuse(&rt, c, profile, as_json),
                        _ => ExitCode::from(2),
                    }
                }
                r => serve_exit(r),
            }
        }
    }
}

/// One more connect after a foreground start found the store held: the concurrent winner's
/// connection when it is ready or becomes ready within the start timeout.
async fn winner(run: &RunDir, hello: &Hello, start_timeout: Duration) -> Option<Client> {
    match connect(run, hello).await {
        Attempt::Ready(c) => Some(c),
        Attempt::Starting(p) => p
            .wait(Instant::now() + start_timeout, &mut progress)
            .await
            .ok(),
        _ => None,
    }
}

/// The profile-only site of the daemon `client` reached: a session as every CLI command opens
/// one, then `site_url` with the profile and `only`. Closing the connection leaves the daemon
/// running.
fn reuse(rt: &tokio::runtime::Runtime, client: Client, profile: &str, as_json: bool) -> ExitCode {
    let actor = std::env::var("RDY_ACTOR").ok();
    let run = std::env::var("RDY_RUN").ok();
    let mut client = client.with_agent(Arc::new(CallerAgent::from_env()));
    let out = rt.block_on(async {
        client
            .ctl("session", json!({"actor": actor, "run": run}))
            .await?;
        let session_notice = client.take_notice();
        let v = client
            .ctl("site_url", json!({"profile": profile, "only": true}))
            .await;
        let mut notices: Vec<String> = session_notice.into_iter().collect();
        if let Some(n) = client.take_notice()
            && !notices.contains(&n)
        {
            notices.push(n);
        }
        for n in notices {
            let _ = writeln!(std::io::stderr(), "notice: {n}");
        }
        v
    });
    client.close();
    match out {
        Ok(v) => match print_profile_site(&v, as_json) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                print_error(&print_failed(e).into(), as_json);
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            print_error(&e, as_json);
            ExitCode::FAILURE
        }
    }
}

/// A failed write of the profile site's result: its stdout lines or its stderr LAN warning.
fn print_failed(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("could not print the profile site: {e}"),
        "check where the standard output and standard error go, then run rdy serve --profile again",
    )
}

/// The foreground start's result, once its profile-only listener is open: the same reply
/// `site_url` gives, from the build the start captured. A failed print fails the start, which
/// then stops the daemon it created.
fn print_ready(ready: ProfileSiteReady, as_json: bool) -> Result<(), RdyError> {
    let ProfileSiteReady {
        key,
        info,
        profile,
        warnings,
    } = ready;
    let url = info.url.map_err(|reason| {
        RdyError::new(
            E_SITE,
            format!("the live site is off: {reason}"),
            "set serve.site_port to a free port from 1024 to 65535 in .rdy/local.yaml, then run rdy serve --stop",
        )
    })?;
    let link = SiteLink {
        url,
        lan_url: info.lan_url,
        warning: info.warning,
    };
    let facts = SiteFacts {
        profile: Some(profile),
        changes: None,
        warnings,
    };
    let v = site_reply_value(&SiteTarget::ProfileOnly { key }, link, facts)?;
    print_profile_site(&v, as_json).map_err(print_failed)
}

/// Prints a profile-only `site_url` reply. Text: `profile site <key>: <url>`, then the LAN URL
/// when bound, then one `warning: <entry>` per entry of `warnings`, one line each. JSON: the
/// reply on one line. Either way the structured LAN warning goes to stderr first. A failed write
/// to either is returned.
fn print_profile_site(v: &Value, as_json: bool) -> std::io::Result<()> {
    lan_warning(v)?;
    let out = if as_json {
        format!("{v}\n")
    } else {
        let mut t = format!(
            "profile site {}: {}\n",
            text(&v["profile"]["key"]),
            text(&v["url"])
        );
        if let Some(lan) = v["lan_url"].as_str() {
            t.push_str(&format!("{lan}\n"));
        }
        for w in v["warnings"].as_array().into_iter().flatten() {
            t.push_str(&format!("warning: {}\n", text(w)));
        }
        t
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()
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
    let stopped = rt.block_on(request_stop(&run, &hello, start_timeout))?;
    let Some(stop) = stopped else {
        return Ok("no daemon running");
    };
    let StopRequest {
        pid,
        socket,
        signalled,
    } = stop;
    if let (true, Some(p)) = (signalled, pid) {
        eprintln!(
            "the daemon is another rdy version and has other clients; sent a stop signal to its process {p}"
        );
    }
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
    use rdy_daemon::stop_socket;
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
