use crate::conn::{Conn, Env, VERSION, io_error, require_project, runtime, start_options};
use crate::output::{print_error, text};
use crate::site_cmd::lan_warning;
use rdy_daemon::signing_agent::CallerAgent;
use rdy_daemon::{
    Attempt, Client, E_DAEMON_GONE, ErrorReply, Hello, PROTO_VERSION, ProfileSiteReady, Proto,
    RunDir, STOP_GRACE, ServeError, ServeOptions, StopRequest, connect, connect_backoff,
    connect_or_start, not_ready, process_alive, request_stop, serve, serve_with_profile,
};
use rdy_engine::engine::{SiteFacts, SiteLink, attach_mcp_url, site_reply_value};
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{E_BAD_INPUT, E_IO, E_SITE, RdyError, serve_config};
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

#[derive(clap::Args)]
pub(crate) struct McpArgs {
    // the help is given in full: a doc comment would lose its closing period
    #[arg(
        long,
        help = "Print the project's MCP over HTTP endpoint and its bearer for a client's configuration, then exit. The bearer is a secret. Needs the project daemon and serve.mcp_http set to true."
    )]
    pub(crate) http_config: bool,
}

/// `rdy mcp --http-config` (D-6M0AF): the live endpoint and the project's bearer, the one place
/// the token is shown. The daemon (started if need be) gives the URL in its status. The token is
/// read from its private file in this process, after the project's identity is checked; no daemon
/// op returns it. It never enables the setting: a disabled or unavailable endpoint is an error.
/// Text is `url: <url>` then `Authorization: Bearer <token>`; `--json` is exactly
/// `{"url":"<url>","headers":{"Authorization":"Bearer <token>"}}`.
fn mcp_http_config(env: &Env, as_json: bool) -> Result<(Value, String), ErrorReply> {
    require_project(&env.project)?;
    if env.no_daemon {
        return Err(ErrorReply::new(
            E_BAD_INPUT,
            "MCP over HTTP needs the project daemon",
            "drop --no-daemon and run rdy mcp --http-config",
        ));
    }
    let mut conn = Conn::open(env)?;
    let found = http_config_from(env, &mut conn);
    conn.close();
    let (url, token) = found?;
    let out = if as_json {
        // the key order is part of the contract, so the object is written by hand
        format!(
            "{{\"url\":{},\"headers\":{{\"Authorization\":{}}}}}\n",
            Value::String(url),
            Value::String(format!("Bearer {token}"))
        )
    } else {
        format!("url: {url}\nAuthorization: Bearer {token}\n")
    };
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| {
            ErrorReply::new(
                E_IO,
                format!("could not print the MCP configuration: {e}"),
                "check where the standard output goes, then run rdy mcp --http-config again",
            )
        })?;
    Ok((Value::Null, String::new()))
}

/// The endpoint and the bearer, or the error that says why there is none.
fn http_config_from(env: &Env, conn: &mut Conn) -> Result<(String, String), ErrorReply> {
    let status = conn.op("status", json!({}))?;
    let facts = &status["mcp_http"];
    if facts["enabled"] == json!(false) {
        return Err(ErrorReply::new(
            E_SITE,
            "MCP over HTTP is disabled for this project",
            "set serve.mcp_http to true in user or local config",
        ));
    }
    let run = RunDir::of_project(&env.project);
    // the project's identity is checked before its private file is read
    run.project_id()
        .map_err(|e| io_error(e, "check that the project directory is intact"))?;
    let token = rdy_daemon::mcp_token::read(&run).map_err(|e| match e {
        rdy_daemon::mcp_token::TokenError::Invalid => ErrorReply::new(
            E_SITE,
            e.to_string(),
            "stop rdy, remove .rdy/run/mcp-token, then run rdy mcp --http-config",
        ),
        rdy_daemon::mcp_token::TokenError::Io(e) => io_error(e, "check .rdy/run/mcp-token"),
    })?;
    match facts["url"].as_str() {
        Some(url) => Ok((url.to_string(), token.reveal())),
        None => {
            // the site is off: the site's own error says why
            conn.op("site_url", json!({}))?;
            // the site is on, so the daemon could not use the token it holds
            Err(ErrorReply::new(
                E_SITE,
                "the project's MCP token is not a valid private token file",
                "stop rdy, remove .rdy/run/mcp-token, then run rdy mcp --http-config",
            ))
        }
    }
}

pub(crate) fn mcp(
    env: &Env,
    projectless: bool,
    http_config: bool,
    as_json: bool,
) -> Result<(Value, String), ErrorReply> {
    if http_config {
        return mcp_http_config(env, as_json);
    }
    if projectless {
        return mcp_projectless(env);
    }
    // before the proxy creates .rdy/run and starts a daemon that would fail at open
    require_project(&env.project)?;
    // D-Q8NFG: before either transport starts, so the direct path's E_IO and the proxy's
    // E_DAEMON_START never stand in for the configured person's own refusal
    configured_actor(env)?;
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
    // ain5: the person layer is read in this process too: meta's unused store, or its daemon
    let person = Arc::new(crate::instructions_cmd::person_reader(true));
    rt.block_on(rdy_mcp::server::serve_stdio_with_person(
        env.project.clone(),
        agent,
        person,
    ))
    .map_err(|e| ErrorReply::new(E_IO, e.to_string(), "check the project path"))?;
    Ok((Value::Null, String::new()))
}

/// `rdy mcp` with no project and no meta project (ain5, D-YQPB6): the projectless server, which
/// answers the session claim and `rdy_instructions` and opens no store. The person layer is read
/// through meta's daemon, or in this process with `--no-daemon`, once the person creates meta.
fn mcp_projectless(env: &Env) -> Result<(Value, String), ErrorReply> {
    configured_actor(env)?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| io_error(e, "retry"))?;
    let person = Arc::new(crate::instructions_cmd::person_reader(env.no_daemon));
    // a rule the person's words ask for may need a signature: this process's own agent signs it
    let agent = rdy_daemon::signing_transport::direct_agent();
    rt.block_on(rdy_mcp::bootstrap::serve_projectless_stdio(
        env.cwd.clone(),
        agent,
        person,
    ))
    .map_err(|e| ErrorReply::new(E_IO, e.to_string(), "retry rdy mcp"))?;
    rt.shutdown_background();
    Ok((Value::Null, String::new()))
}

/// D-Q8NFG: the configured person, resolved as every ordinary command resolves it
/// (`Conn::open`), so an actor outside the form is the same `E_BAD_INPUT` naming its file. Run
/// before a transport or a daemon starts: a refusal creates no `.rdy/run`.
fn configured_actor(env: &Env) -> Result<(), ErrorReply> {
    rdy_engine::human_actor_for_env(&env.project)?;
    Ok(())
}

/// The exit code of a daemon run: 0, or 2 when another rdy process holds the store, else 1. The
/// host already logged an `AlreadyRunning`, `Open` or `Start` error to stderr; the others are printed
/// here.
fn serve_exit(r: Result<rdy_daemon::ExitReason, ServeError>) -> ExitCode {
    match r {
        Ok(_) => ExitCode::SUCCESS,
        Err(ServeError::AlreadyRunning(_)) => ExitCode::from(2),
        Err(ServeError::Open(_) | ServeError::Start(_)) => ExitCode::FAILURE,
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
    // D-Q8NFG: every start (foreground, detached, profile) refuses the configured person
    // before it spawns or starts a daemon. A stop needs no person, and a spawned daemon's
    // client has already checked it.
    if !stop
        && !spawned
        && let Err(e) = configured_actor(env)
    {
        print_error(&e, as_json);
        return ExitCode::FAILURE;
    }
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
        build: rdy_daemon::BUILD.to_string(),
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
    // D-ME2C0: an identity that is not Unicode is refused before the session handshake
    let (actor, run) = match crate::conn::session_env() {
        Ok(pair) => pair,
        Err(e) => {
            client.close();
            print_error(&e.into(), as_json);
            return ExitCode::FAILURE;
        }
    };
    crate::conn::warn_other_build(&client);
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
        mcp_url,
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
    let mut v = site_reply_value(&SiteTarget::ProfileOnly { key }, link, facts)?;
    attach_mcp_url(&mut v, mcp_url.as_deref());
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
    crate::conn::warn_other_build(&client);
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
        build: rdy_daemon::BUILD.to_string(),
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
    // a pipe's name is no file: only a connect can say whether it still stands
    let endpoint_stands = || {
        rt.block_on(rdy_daemon::sys::endpoint_liveness(&socket)) != rdy_daemon::sys::Liveness::Gone
    };
    let gone = || exited(pid, endpoint_stands, process_alive);
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
        run.ensure().unwrap();
        // the daemon bound an endpoint other than the one this spelling computes: a socket file
        // where sockets are files, else a pipe name (which no test can create or remove, so a
        // flag stands for its being there)
        #[cfg(not(windows))]
        let bound = d.path().join("bound.sock");
        #[cfg(windows)]
        let bound = std::path::PathBuf::from(format!(r"\\.\pipe\rdy-{}", "0".repeat(64)));
        assert_ne!(bound, run.socket());
        let project = run.project_id().unwrap();
        // the record is private to this account, as the daemon writes it: a client reads no other
        {
            use rdy_daemon::sys;
            use std::io::Write;
            let mut record = sys::open_no_follow(
                sys::private_file(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true),
                ),
                &run.socket_record(),
            )
            .unwrap();
            sys::make_private_file(&record).unwrap();
            record
                .write_all(format!("{}\n{project}\n", bound.display()).as_bytes())
                .unwrap();
        }
        let up = Cell::new(true);
        #[cfg(not(windows))]
        std::fs::write(&bound, "").unwrap();
        assert!(!run.socket().exists());
        // read once, before the stop
        let socket = stop_socket(&run);
        assert_eq!(socket, bound);
        let there = || {
            #[cfg(not(windows))]
            {
                socket.exists()
            }
            #[cfg(windows)]
            {
                up.get()
            }
        };
        let gone = || exited(None, there, |_| false);
        assert!(!gone(), "the recorded socket is still there");
        // the daemon removes its record first, then its socket
        std::fs::remove_file(run.socket_record()).unwrap();
        assert!(!gone(), "the socket outlives the record");
        #[cfg(not(windows))]
        std::fs::remove_file(&bound).unwrap();
        up.set(false);
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
