//! Gate helpers for the `daemon` area: an echo daemon run in a child process (the test binary
//! re-executed with `RDY_DAEMON_CHILD`), a spawner that starts it, and client options for it.
use rdy_daemon::{
    Attempt, Client, CtlRequest, Daemon, DaemonConfig, E_BAD_INPUT, ErrorReply, ExitReason,
    Handler, Hello, PROTO_VERSION, Proto, RunDir, ServeOptions, ServeRun, SessionInfo,
    StartOptions, connect, spawn_serve, start,
};
use rdy_engine::Rdy;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::UnixStream;

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

/// How long `client`'s `connect_or_start` waits for a daemon.
const CLIENT_START_TIMEOUT: Duration = Duration::from_secs(10);

/// A directory short enough for a socket path under it: `/tmp/rdyd…` (macOS `sun_path` is 104
/// bytes and its `temp_dir()` is already about 50).
pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rdyd")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp")
}

/// The test handler: `echo` returns the session, cwd and args; `sleep` sleeps `args.ms`;
/// `count` increments `count` and returns the new value. Ended sessions count into `ended`.
#[derive(Default)]
pub struct EchoHandler {
    pub count: AtomicUsize,
    pub ended: AtomicUsize,
}

impl Handler for EchoHandler {
    fn ctl(&self, s: &SessionInfo, r: CtlRequest) -> Result<Value, ErrorReply> {
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
    /// group of its own. Does not wait for it (a thread reaps it), and counts the spawn.
    pub fn spawn(&self, run: &RunDir) -> io::Result<()> {
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
        let log = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(run.log_file())?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args([self.test_fn.as_str(), "--exact", "--nocapture"])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .process_group(0);
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
        spawn_serve(&mut cmd)?;
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
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
const HOSTED_READY_TIMEOUT: Duration = Duration::from_secs(10);

impl Hosted {
    /// `new_with_human("human:qa")`.
    pub fn new() -> Hosted {
        Hosted::new_with_human("human:qa")
    }

    /// A host whose configured human actor is `human`.
    pub fn new_with_human(human: &str) -> Hosted {
        let dir = short_tempdir();
        drop(Rdy::init(dir.path(), "rdy-standard").expect("init the hosted project"));
        let run = start(ServeOptions {
            project: dir.path().to_path_buf(),
            version: "t".into(),
            detach: false,
            idle_timeout: Some(Duration::from_secs(60)),
            human_actor: Some(human.into()),
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

    /// A connection of `proto` past `ready`.
    async fn connect(&self, proto: Proto) -> Client {
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: "t".into(),
            proto,
            cwd: self.dir.path().into(),
        };
        let run = RunDir::of_project(self.dir.path());
        match connect(&run, &hello).await {
            Attempt::Ready(c) => c,
            Attempt::Starting(p) => p
                .wait(
                    std::time::Instant::now() + HOSTED_READY_TIMEOUT,
                    &mut |_| {},
                )
                .await
                .unwrap_or_else(|e| panic!("the host did not become ready: {e}")),
            other => panic!("the host did not answer the hello: {other:?}"),
        }
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
    pub async fn mcp_stream(&self) -> UnixStream {
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
