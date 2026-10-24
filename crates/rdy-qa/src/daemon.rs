//! Gate helpers for the `daemon` area: an echo daemon run in a child process (the test binary
//! re-executed with `RDY_DAEMON_CHILD`), a spawner that starts it, and client options for it.
use crate::platform;
use rdy_daemon::site::LanOverride;
use rdy_daemon::{
    Attempt, Client, CtlRequest, Daemon, DaemonConfig, E_BAD_INPUT, ErrorReply, ExitReason,
    Handler, Hello, PROTO_VERSION, Proto, RunDir, ServeOptions, ServeRun, SessionInfo, Spawned,
    StartOptions, connect, spawn_serve, start,
};
use rdy_engine::Rdy;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The child's project root; unset in a normal test run.
const ENV_CHILD: &str = "RDY_DAEMON_CHILD";
/// The child daemon's version (default "1").
const ENV_VERSION: &str = "RDY_DAEMON_VERSION";
/// The child daemon's idle timeout in ms (default `CHILD_IDLE_TIMEOUT`).
const ENV_IDLE_MS: &str = "RDY_DAEMON_IDLE_MS";
/// A child daemon's idle timeout unless `RDY_DAEMON_IDLE_MS` sets one: far past any gate test's
/// own run, so it never fires mid-test, but a test that fails before stopping its daemon leaves
/// no process behind for long.
const CHILD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the child reports `starting` before `ready`, in ms (default 0).
const ENV_PROGRESS_MS: &str = "RDY_DAEMON_PROGRESS_MS";
/// A file path: when set, the child stays `starting` until that file exists (then `ready`), so a
/// test releases readiness itself instead of racing a timer.
const ENV_READY_FILE: &str = "RDY_DAEMON_READY_FILE";

/// How long `client`'s `connect_or_start` waits for a daemon.
const CLIENT_START_TIMEOUT: Duration = Duration::from_secs(30);

/// A directory short enough for a socket path under it: `/tmp/rdyd…` (macOS `sun_path` is 104
/// bytes and its `temp_dir()` is already about 50).
pub fn short_tempdir() -> tempfile::TempDir {
    short_tempdir_in(platform::short_tmp_root())
}

/// `short_tempdir`, under `parent` (itself short: the swarm's `/tmp/rdysw-<pid>`).
pub fn short_tempdir_in(parent: &Path) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rdyd")
        .tempdir_in(parent)
        .unwrap_or_else(|e| panic!("tempdir in {}: {e}", parent.display()))
}

/// The test handler: `echo` returns the session, cwd and args; `sleep` sleeps `args.ms`;
/// `count` increments `count` and returns the new value. Ended sessions count into `ended`.
#[derive(Default)]
pub struct EchoHandler {
    pub count: AtomicUsize,
    pub ended: AtomicUsize,
}

impl Handler for EchoHandler {
    fn ctl(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        _agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        match r.op.as_str() {
            "echo" => Ok(json!({"session": s.id, "cwd": s.cwd, "args": r.args})),
            "sleep" => {
                std::thread::sleep(Duration::from_millis(r.args["ms"].as_u64().unwrap_or(0)));
                Ok(json!({"slept": true}))
            }
            "count" => Ok(json!(self.count.fetch_add(1, Ordering::SeqCst) + 1)),
            other => Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("unknown op {other:?}"),
                "use echo, sleep or count",
            )),
        }
    }

    fn session_ended(&self, _s: &SessionInfo) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}

fn env_ms(name: &str) -> Option<u64> {
    std::env::var(name).ok().map(|v| {
        v.parse()
            .unwrap_or_else(|e| panic!("{name}={v:?} is not a number of ms: {e}"))
    })
}

/// The body of the re-exec child. Returns at once unless `RDY_DAEMON_CHILD` names a project;
/// then it serves an `EchoHandler` daemon for that project until the daemon exits, and exits
/// the process.
pub fn child_daemon_entry() {
    let Some(project) = std::env::var_os(ENV_CHILD) else {
        return;
    };
    let run = RunDir::of_project(Path::new(&project));
    let version = std::env::var(ENV_VERSION).unwrap_or_else(|_| "1".to_string());
    let idle = env_ms(ENV_IDLE_MS)
        .map(Duration::from_millis)
        .unwrap_or(CHILD_IDLE_TIMEOUT);
    let progress = Duration::from_millis(env_ms(ENV_PROGRESS_MS).unwrap_or(0));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    let code = rt.block_on(async move {
        let mut config = DaemonConfig::new(run, version);
        config.idle_timeout = Some(idle);
        let mut d = Daemon::new(config, Arc::new(EchoHandler::default()));
        if let Err(e) = d.bind().await {
            eprintln!("race1 child: bind failed: {e}");
            return 1;
        }
        let readiness = d.readiness();
        readiness.progress("starting");
        let task = tokio::spawn(d.run());
        tokio::time::sleep(progress).await;
        if let Some(gate) = std::env::var_os(ENV_READY_FILE) {
            while !Path::new(&gate).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        readiness.ready();
        match task.await {
            Ok(Ok(reason)) => {
                eprintln!("race1 child: exited: {reason:?}");
                0
            }
            Ok(Err(e)) => {
                eprintln!("race1 child: run failed: {e}");
                1
            }
            Err(e) => {
                eprintln!("race1 child: run panicked: {e}");
                1
            }
        }
    });
    // do not return into the test harness: the child's work is the daemon, now done
    std::process::exit(code);
}

/// Starts child daemons for one run dir: the test binary re-executed on `test_fn`.
#[derive(Clone)]
pub struct ChildSpawner {
    test_fn: String,
    run: RunDir,
    version: String,
    extra_env: Vec<(String, String)>,
    count: Arc<AtomicUsize>,
}

impl ChildSpawner {
    /// Runs `<current_exe> <test_fn> --exact --nocapture` with `RDY_DAEMON_CHILD` set to the
    /// project root, stdin null, stdout and stderr appended to the run dir's log, in a process
    /// group of its own. Does not wait for it (the `Spawned` watches it), and counts the spawn.
    pub fn spawn(&self, run: &RunDir) -> io::Result<Spawned> {
        if *run != self.run {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "this spawner starts daemons for {}, not {}",
                    self.run.path().display(),
                    run.path().display()
                ),
            ));
        }
        let project = project_of(run)?;
        run.ensure()?;
        let log = platform::open_private_log(&run.log_file())?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args([self.test_fn.as_str(), "--exact", "--nocapture"])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        platform::own_process_group(&mut cmd);
        // nothing from the parent's environment steers the child; only what we set here
        for (k, _) in std::env::vars_os() {
            if k.as_encoded_bytes().starts_with(b"RDY_DAEMON_") {
                cmd.env_remove(k);
            }
        }
        cmd.env(ENV_CHILD, &project).env(ENV_VERSION, &self.version);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        let spawned = spawn_serve(&mut cmd)?;
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(spawned)
    }
}

/// The project root a run dir belongs to (`<root>/.rdy/run`).
fn project_of(run: &RunDir) -> io::Result<PathBuf> {
    run.path()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other(format!("{} has no project", run.path().display())))
}

/// A spawner of `version` child daemons for `run`, running the test function `test_fn` (which
/// must call `child_daemon_entry`), with `extra_env` set in the child; and its spawn counter.
pub fn spawn_child_daemon(
    test_fn: &str,
    run: &RunDir,
    version: &str,
    extra_env: &[(&str, &str)],
) -> (ChildSpawner, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let spawner = ChildSpawner {
        test_fn: test_fn.to_string(),
        run: run.clone(),
        version: version.to_string(),
        extra_env: extra_env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        count: count.clone(),
    };
    (spawner, count)
}

/// `ctl` client options of `version` in `cwd` that start a daemon with `spawner`, with a 10 s
/// start timeout. `spawner` refuses any run dir but its own.
pub fn client(_run: &RunDir, version: &str, cwd: &Path, spawner: ChildSpawner) -> StartOptions {
    let mut opts = StartOptions::new(
        version.to_string(),
        Proto::Ctl,
        cwd.to_path_buf(),
        Box::new(move |r| spawner.spawn(r)),
    );
    opts.start_timeout = CLIENT_START_TIMEOUT;
    opts
}

/// A real host (`rdy_daemon::start`) in this process over a fresh `rdy-standard` project, with a
/// 60 s idle timeout. Dropping it stops the daemon and waits for the store to close.
pub struct Hosted {
    pub dir: tempfile::TempDir,
    pub run: Option<ServeRun>,
}

/// How long `Hosted`'s clients wait for `ready`.
const HOSTED_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// A connection for `hello` to the daemon at `run`, past `ready`: waits up to 30 s while the
/// daemon answers `starting`. Panics if the daemon does not answer or does not become ready.
pub async fn connect_ready(run: &RunDir, hello: &Hello) -> Client {
    connect_ready_noting(run, hello, || {}).await
}

/// `connect_ready`, calling `on_starting` once, when the daemon first answers `starting` and
/// before the helper waits: a test uses it to release a gated daemon only after the helper has
/// met `starting`.
pub async fn connect_ready_noting(
    run: &RunDir,
    hello: &Hello,
    on_starting: impl FnOnce(),
) -> Client {
    match connect(run, hello).await {
        Attempt::Ready(c) => c,
        Attempt::Starting(p) => {
            on_starting();
            p.wait(
                std::time::Instant::now() + HOSTED_READY_TIMEOUT,
                &mut |_| {},
            )
            .await
            .unwrap_or_else(|e| panic!("the host did not become ready: {e}"))
        }
        other => panic!("the host did not answer the hello: {other:?}"),
    }
}

impl Hosted {
    /// `new_with_human("human:qa")`.
    pub fn new() -> Hosted {
        Hosted::new_with_human("human:qa")
    }

    /// A host whose configured human actor is `human`.
    pub fn new_with_human(human: &str) -> Hosted {
        let dir = short_tempdir();
        drop(Rdy::init(dir.path(), "rdy-standard").expect("init the hosted project"));
        Hosted::serve(dir, human, None)
    }

    /// A host over the project already in `dir` (human actor `human:qa`).
    pub fn start_in(dir: tempfile::TempDir) -> Hosted {
        Hosted::serve(dir, "human:qa", None)
    }

    /// `start_in`, with `lan_override` standing in for the LAN address of the default route
    /// while `serve.site_lan` is on (`ServeOptions::site_lan_override`).
    pub fn start_with(dir: tempfile::TempDir, lan_override: Option<LanOverride>) -> Hosted {
        Hosted::serve(dir, "human:qa", lan_override)
    }

    /// `start_with`, as the version of the `rdy` binary, so that a real `rdy` client (its
    /// `mcp` proxy, its CLI) talks to this host and does not replace it.
    pub fn start_for_binary(dir: tempfile::TempDir, lan_override: Option<LanOverride>) -> Hosted {
        Hosted::serve_as(dir, "human:qa", lan_override, env!("CARGO_PKG_VERSION"))
    }

    fn serve(dir: tempfile::TempDir, human: &str, lan_override: Option<LanOverride>) -> Hosted {
        Hosted::serve_as(dir, human, lan_override, "t")
    }

    fn serve_as(
        dir: tempfile::TempDir,
        human: &str,
        lan_override: Option<LanOverride>,
        version: &str,
    ) -> Hosted {
        let run = start(ServeOptions {
            project: dir.path().to_path_buf(),
            version: version.into(),
            detach: false,
            idle_timeout: Some(Duration::from_secs(60)),
            human_actor: Some(human.into()),
            site_lan_override: lan_override,
        })
        .unwrap_or_else(|e| panic!("start the host: {e}"));
        Hosted {
            dir,
            run: Some(run),
        }
    }

    /// A runtime for the test's client side.
    pub fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("client runtime")
    }

    /// The hello of a `proto` connection to this host.
    pub fn hello(&self, proto: Proto) -> Hello {
        Hello {
            rdy: PROTO_VERSION,
            version: "t".into(),
            proto,
            cwd: self.dir.path().into(),
            project: rdy_daemon::RunDir::of_project(std::path::Path::new(&self.dir.path()))
                .project_id()
                .expect("the project's .rdy"),
        }
    }

    /// A connection of `proto` past `ready`. Waits while the host answers `starting`.
    pub async fn connect(&self, proto: Proto) -> Client {
        connect_ready(&RunDir::of_project(self.dir.path()), &self.hello(proto)).await
    }

    /// A `ctl` connection past `ready` whose session op sent `actor` and `run`.
    pub async fn client(&self, actor: Option<&str>, run: Option<&str>) -> Client {
        let mut c = self.connect(Proto::Ctl).await;
        c.ctl("session", json!({"actor": actor, "run": run}))
            .await
            .unwrap_or_else(|e| panic!("the session op failed: {e}"));
        c
    }

    /// An `mcp` connection past `ready`.
    pub async fn mcp_stream(&self) -> rdy_daemon::sys::Stream {
        self.connect(Proto::Mcp).await.into_stream()
    }

    /// Stops the daemon and waits for it (the store is closed when this returns).
    pub fn stop(&mut self) -> ExitReason {
        let run = self.run.take().expect("the host is running");
        run.handle().stop();
        run.wait()
            .unwrap_or_else(|e| panic!("the host failed: {e}"))
    }
}

impl Default for Hosted {
    fn default() -> Self {
        Hosted::new()
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            run.handle().stop();
            // a failing host must not panic inside a drop (the test may already be unwinding)
            if let Err(e) = run.wait() {
                eprintln!("the hosted daemon exited with an error: {e}");
            }
        }
    }
}

/// A fresh `rdy-standard` project whose commands run the real `rdy` binary as a client of its
/// daemon: `.rdy.yaml` sets `serve.idle_timeout: 60`, the human actor is `human:qa` (a
/// user config under `XDG_CONFIG_HOME`), and dropping it runs `rdy serve --stop`.
pub struct Served {
    pub dir: tempfile::TempDir,
    /// Holds the user config `env` points at.
    cfg: tempfile::TempDir,
    env: Vec<(String, String)>,
}

/// The command `rdy --project <dir> <args>` run in `dir` as a person: no `RDY_ACTOR`, `RDY_RUN`
/// or `RDY_NO_DAEMON` from this process, and `env` (a user config, as `Served::env_pairs`
/// gives). Worker threads build their commands here so the shell's session never leaks in.
pub fn human_command(dir: &Path, env: &[(String, String)], args: &[&str]) -> Command {
    let mut cmd = Command::new(crate::fixtures::rdy_bin());
    cmd.arg("--project")
        .arg(dir)
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir);
    cmd
}

/// How long `Served::kill9` waits for the killed daemon to exit.
const KILL_WAIT: Duration = Duration::from_secs(30);

/// Whether process `pid` has exited: `ps` lists no such process, or lists a zombie (killed, not
/// yet reaped by its parent).
pub fn exited(pid: u32) -> bool {
    platform::process_exited(pid)
}

impl Served {
    pub fn new() -> Served {
        Served::with(short_tempdir(), crate::fixtures::human_env("human:qa"))
    }

    /// `new`, with the project and the user config under `parent` (a short directory).
    pub fn new_in(parent: &Path) -> Served {
        Served::with(
            short_tempdir_in(parent),
            crate::fixtures::human_env_in("human:qa", parent),
        )
    }

    fn with(
        dir: tempfile::TempDir,
        (cfg, env): (tempfile::TempDir, Vec<(String, String)>),
    ) -> Served {
        drop(Rdy::init(dir.path(), "rdy-standard").expect("init the served project"));
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join(".rdy.yaml"))
            .expect("open the project config");
        io::Write::write_all(&mut file, b"serve:\n  idle_timeout: 60\n")
            .expect("append the serve section");
        Served { dir, cfg, env }
    }

    /// The user config the served commands read (`$XDG_CONFIG_HOME/rdy/config.yaml`).
    pub fn user_config(&self) -> PathBuf {
        self.cfg.path().join("rdy/config.yaml")
    }

    /// The command `rdy --project <dir> <args>` in the project, as the human (`human_command`).
    fn command(&self, args: &[&str]) -> Command {
        human_command(self.dir.path(), &self.env, args)
    }

    /// Starts `rdy --project <dir> <args>` as the human, without waiting (stdout and stderr
    /// piped).
    pub fn spawn(&self, args: &[&str]) -> std::process::Child {
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rdy")
    }

    /// Runs `rdy --project <dir> <args>` as the human.
    pub fn rdy(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().expect("run rdy")
    }

    /// Runs `rdy --project <dir> <args>` with `RDY_ACTOR=actor` and, if given, `RDY_RUN=run`.
    pub fn rdy_as(&self, actor: &str, run: Option<&str>, args: &[&str]) -> std::process::Output {
        self.rdy_as_os(actor.as_ref(), run.map(AsRef::as_ref), args)
    }

    /// `rdy_as` with environment values that need not be Unicode.
    pub fn rdy_as_os(
        &self,
        actor: &std::ffi::OsStr,
        run: Option<&std::ffi::OsStr>,
        args: &[&str],
    ) -> std::process::Output {
        let mut cmd = self.command(args);
        cmd.env("RDY_ACTOR", actor);
        if let Some(r) = run {
            cmd.env("RDY_RUN", r);
        }
        cmd.output().expect("run rdy")
    }

    /// Whether the command succeeded, and its JSON: stdout on success, stderr on failure.
    fn parsed(out: std::process::Output) -> (bool, Value) {
        let ok = out.status.success();
        let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
        let v = serde_json::from_str(text.trim())
            .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
            .unwrap_or(Value::String(text));
        (ok, v)
    }

    /// `rdy(args)`, parsed (pass `--json` in `args`).
    pub fn json(&self, args: &[&str]) -> (bool, Value) {
        Served::parsed(self.rdy(args))
    }

    /// `rdy_as(actor, run, args)`, parsed (pass `--json` in `args`).
    pub fn json_as(&self, actor: &str, run: Option<&str>, args: &[&str]) -> (bool, Value) {
        Served::parsed(self.rdy_as(actor, run, args))
    }

    /// `rdy_as_os(actor, run, args)`, parsed (pass `--json` in `args`).
    pub fn json_as_os(
        &self,
        actor: &std::ffi::OsStr,
        run: Option<&std::ffi::OsStr>,
        args: &[&str],
    ) -> (bool, Value) {
        Served::parsed(self.rdy_as_os(actor, run, args))
    }

    /// The human's environment (`XDG_CONFIG_HOME`), for commands a test builds itself.
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        self.env.clone()
    }

    pub fn run_dir(&self) -> RunDir {
        RunDir::of_project(self.dir.path())
    }

    /// Sets `serve.start_timeout` to `secs` (appended under the `serve:` section `new` wrote), so
    /// a test can tell a fast refusal from one that waited out the timeout with a wide margin.
    pub fn set_start_timeout(&self, secs: u64) {
        let mut cfg = OpenOptions::new()
            .append(true)
            .open(self.dir.path().join(".rdy.yaml"))
            .expect("open the project config");
        io::Write::write_all(&mut cfg, format!("  start_timeout: {secs}\n").as_bytes())
            .expect("append the start timeout");
        // a config that no longer parses falls back to the 30 s default, which would hide a
        // client that waits the timeout out: the setting must be the one the CLI reads
        assert_eq!(
            rdy_engine::serve_config(self.dir.path()).start_timeout,
            Duration::from_secs(secs),
            "serve.start_timeout did not take"
        );
    }

    /// The pid in `.rdy/run/daemon.pid`, if it parses and that process is alive.
    pub fn daemon_pid(&self) -> Option<u32> {
        let pid = self.pid_file()?;
        platform::process_running(pid).then_some(pid)
    }

    fn pid_file(&self) -> Option<u32> {
        std::fs::read_to_string(self.run_dir().pid_file())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// SIGKILLs the daemon named by the pid file, then waits until that process is gone
    /// (`wait_killed`). Callable from inside a tokio runtime.
    pub fn kill9(&self) {
        let pid = self.pid_file().expect("a daemon pid file");
        platform::kill_hard(pid);
        self.wait_killed(pid);
    }

    /// Waits until the killed daemon `pid` has exited (`ps` no longer lists it, or lists it as
    /// a zombie). It does not wait for the socket to stop answering: a client may start a
    /// successor at once (final review M6). Past `KILL_WAIT` it panics (a hang guard).
    pub fn wait_killed(&self, pid: u32) {
        let deadline = std::time::Instant::now() + KILL_WAIT;
        while !exited(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon {pid} still runs {} s after kill -9",
                KILL_WAIT.as_secs()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// `rdy serve --stop`; a failure is reported on stderr, never a panic.
    pub fn stop(&self) {
        match self.command(&["serve", "--stop"]).output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!(
                "rdy serve --stop failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => eprintln!("rdy serve --stop did not run: {e}"),
        }
    }
}

impl Default for Served {
    fn default() -> Self {
        Served::new()
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The site warnings the hosted daemons of a test log (an exposed LAN site, a LAN address that
/// cannot be bound, a profile-only caveat, a profile's references outside its selection), which
/// the test expects ([`site_log`]). Its lines are taken while no other test of the binary that
/// expects site warnings runs, so each such test asserts its own; every other line stays
/// visible.
pub struct SiteLog {
    capture: rdy_daemon::daemon::LogCapture,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl SiteLog {
    /// The lines taken so far, in order, each `127.0.0.1:<port>` written so.
    pub fn lines(&self) -> Vec<String> {
        self.capture
            .lines()
            .into_iter()
            .map(|l| {
                let mut out = String::new();
                let mut rest = l.as_str();
                while let Some(i) = rest.find("127.0.0.1:") {
                    let (head, tail) = rest.split_at(i + "127.0.0.1:".len());
                    out.push_str(head);
                    out.push_str("<port>");
                    rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
                }
                out.push_str(rest);
                out
            })
            .collect()
    }
}

/// Starts taking the site warnings of this test's hosted daemons ([`SiteLog`]); call it before
/// the host starts. The site's address lines (`site: http://…`) are not warnings and stay
/// visible.
pub fn site_log() -> SiteLog {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    SiteLog {
        capture: rdy_daemon::daemon::capture_log_for_tests(&[
            "site: warning: ",
            "site: /",
            "site: could not listen ",
            "site: profile \"",
        ]),
        _serial: serial,
    }
}

/// The daemon's warning (as [`SiteLog::lines`] gives it) that its full site is on the LAN.
pub const LAN_DOCS_LINE: &str = "site: warning: anyone on your network can read these docs at http://127.0.0.1:<port>/; fix: set serve.site_lan to false to keep the site on this machine";

/// The daemon's warning that the profile-only site of `Guide book` is on the LAN.
pub const GUIDE_LAN_LINE: &str = "site: warning: anyone on your network can read the profile \"Guide book\" at http://127.0.0.1:<port>/; fix: set serve.site_lan to false to keep the site on this machine";

/// The daemon's caveat of the profile-only site of `Guide book`.
pub const GUIDE_CAVEAT_LINE: &str = "site: warning: this site shows only the profile \"Guide book\"; anyone with the project still reads everything; fix: give the full site's link from rdy site when the person needs everything";

/// The daemon's render warning of the build at `route` of an org profile with one reference
/// outside its selection.
pub fn org_refs_line(route: &str) -> String {
    format!(
        "site: {route}: warning: Org profile has 1 references outside its selection. Review these references before you share the output."
    )
}

/// `lines`, sorted: the warnings of builds and listeners that run concurrently come in no
/// fixed order.
pub fn sorted_lines<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    let mut v: Vec<String> = lines.iter().map(|l| l.as_ref().to_string()).collect();
    v.sort();
    v
}
