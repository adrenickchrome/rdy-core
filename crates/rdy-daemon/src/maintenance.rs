//! Store maintenance around the daemon (D-WWAK7): `rdy upgrade` on a project directory, and the
//! upgrade an ordinary connection completes before it starts a daemon.
//!
//! The coordinator plans the closed store first (`rdy_engine::maintenance::plan`). A plan with
//! no steps and no pending completion returns unchanged: no daemon is stopped, no lock taken.
//! Otherwise it takes `start.lock` (so no client starts a daemon meanwhile), stops the
//! project's daemon with [`request_stop`] (its peer identity checks included), waits for that
//! daemon to exit, and applies the plan through `rdy_engine::maintenance::apply_planned_under`,
//! which borrows the guard, takes the store lock, plans again and runs the upgrade and its
//! completion. The old daemon is not restarted: the next ordinary connection starts this rdy's.
//!
//! A dry run plans only: it never starts or stops a daemon, opens a writer, creates a lock, or
//! writes a backup or an index. The connected MCP path never comes here.
use crate::client::{LOCK_RETRY, StopRequest, request_stop};
use crate::error::{E_DAEMON_GONE, E_DAEMON_START, E_DAEMON_VERSION, ErrorReply};
use crate::hello::{Hello, PROTO_VERSION};
use crate::layout::RunDir;
use crate::session::Proto;
use crate::sys::{self, Liveness};
use rdy_engine::maintenance::{self as m, UpgradeReport};
use rdy_engine::{E_HELD_BY_READER, E_IO, E_LOCKED, RdyError};
use rdy_store::start_lock::StartGuard;
use rdy_store::upgrade::{UpgradePlan, UpgradeStep};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often the coordinator looks for a stopped daemon to be gone, and for its store lock.
const POLL: Duration = Duration::from_millis(25);

/// How long the coordinator waits for a stopped daemon to exit: its drain, then its blocking
/// work and the store's close (as `rdy serve --stop` waits).
const STOP_WAIT: Duration = Duration::from_secs(120);

thread_local! {
    /// The steps an ordinary connection's start plans with on this thread
    /// ([`with_start_registry_for_tests`]); None is this rdy's.
    static START_REGISTRY: std::cell::RefCell<Option<Vec<UpgradeStep>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: runs `f` with the start of an ordinary connection on this thread
/// ([`before_start`], and the stop of another version's daemon) planning with `registry`.
#[doc(hidden)]
pub fn with_start_registry_for_tests<T>(registry: Vec<UpgradeStep>, f: impl FnOnce() -> T) -> T {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            START_REGISTRY.with(|r| *r.borrow_mut() = None);
        }
    }
    START_REGISTRY.with(|r| *r.borrow_mut() = Some(registry));
    let _off = Off;
    f()
}

/// The steps a connection's start plans with on this thread.
fn start_registry() -> Option<Vec<UpgradeStep>> {
    START_REGISTRY.with(|r| r.borrow().clone())
}

thread_local! {
    /// Runs before each try at the store on this thread ([`with_apply_hook_for_tests`]).
    static APPLY_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: runs `f` with `hook` called before each try at the store on this thread, so a test
/// can tell how far a coordinator has come. No CLI, MCP, config or environment path reaches it.
#[doc(hidden)]
pub fn with_apply_hook_for_tests<T>(hook: impl FnMut() + 'static, f: impl FnOnce() -> T) -> T {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            APPLY_HOOK.with(|h| *h.borrow_mut() = None);
        }
    }
    APPLY_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    let _off = Off;
    f()
}

/// How many times a coordinator has run in this process (a test reads it).
static COORDINATOR_RUNS: AtomicU64 = AtomicU64::new(0);

/// Test hook: how many times the maintenance coordinator has run in this process.
#[doc(hidden)]
pub fn coordinator_runs_for_tests() -> u64 {
    COORDINATOR_RUNS.load(Ordering::SeqCst)
}

/// Inspects (`dry_run`) or upgrades the store of the project at `root`, stopping its daemon
/// first when there is work (see the module docs). The caller may be inside a Tokio runtime:
/// the coordinator then runs on one joined scoped thread with a current-thread runtime of its
/// own; on a thread with no runtime it runs on a current-thread runtime of this thread.
pub fn run(root: &Path, dry_run: bool) -> Result<UpgradeReport, RdyError> {
    run_with(root, dry_run, None)
}

/// Test hook: [`run`] with the steps `registry`, through the same coordinator. No CLI, MCP,
/// config or environment path reaches it.
#[doc(hidden)]
pub fn run_with_registry_for_tests(
    root: &Path,
    dry_run: bool,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, RdyError> {
    run_with(root, dry_run, Some(registry))
}

fn run_with(
    root: &Path,
    dry_run: bool,
    registry: Option<&[UpgradeStep]>,
) -> Result<UpgradeReport, RdyError> {
    let planned = plan(root, registry)?;
    if !dry_run {
        m::refuse_read_only(&planned)?;
    }
    if dry_run || !m::needs_work(&planned) {
        return m::unchanged(root, &planned, dry_run);
    }
    block_on(move || coordinate(root, planned, registry))
}

/// Runs the future `make` makes to its end on a current-thread runtime: of this thread when it
/// has no runtime, else of one joined scoped thread, where the future is made (`block_on`
/// inside a runtime would panic).
fn block_on<T: Send, F: std::future::Future<Output = Result<T, RdyError>>>(
    make: impl FnOnce() -> F + Send,
) -> Result<T, RdyError> {
    let rt = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| RdyError::new(E_IO, e.to_string(), "retry"))
    };
    if tokio::runtime::Handle::try_current().is_err() {
        return rt()?.block_on(make());
    }
    std::thread::scope(|s| {
        s.spawn(|| rt()?.block_on(make()))
            .join()
            .unwrap_or_else(|_| Err(RdyError::new(E_IO, "the upgrade thread panicked", "retry")))
    })
}

fn plan(root: &Path, registry: Option<&[UpgradeStep]>) -> Result<UpgradePlan, RdyError> {
    match registry {
        None => m::plan(root),
        Some(r) => m::plan_with_registry_for_tests(root, r),
    }
}

fn apply(
    root: &Path,
    planned: &UpgradePlan,
    registry: Option<&[UpgradeStep]>,
    start: &StartGuard,
) -> Result<UpgradeReport, RdyError> {
    match registry {
        None => m::apply_planned_under(root, planned, start),
        Some(r) => m::apply_planned_under_with_registry_for_tests(root, planned, start, r),
    }
}

/// `e`, a daemon client's refusal, as an rdy error with the same code, message and fix, and the
/// same diagnostic members.
pub(crate) fn rdy_error(e: ErrorReply) -> RdyError {
    let code = rdy_engine::core_error_code(&e.code)
        .or_else(|| {
            [E_DAEMON_START, E_DAEMON_VERSION, E_DAEMON_GONE]
                .into_iter()
                .find(|c| *c == e.code)
        })
        .unwrap_or(E_IO);
    RdyError::new(code, e.message, e.fix).with_reply_members(&e.extra)
}

/// The hello this rdy sends to the project's daemon.
fn hello(root: &Path, run: &RunDir) -> Option<Hello> {
    let project = run.project_id().ok()?;
    Some(Hello {
        rdy: PROTO_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: crate::BUILD.to_string(),
        proto: Proto::Ctl,
        cwd: root.to_path_buf(),
        project,
    })
}

/// The coordinator of [`run`]: takes `start.lock`, so no ordinary connection starts a daemon
/// meanwhile, then stops the project's daemon, waits for it to exit and applies `planned` under
/// the lock.
async fn coordinate(
    root: &Path,
    planned: UpgradePlan,
    registry: Option<&[UpgradeStep]>,
) -> Result<UpgradeReport, RdyError> {
    COORDINATOR_RUNS.fetch_add(1, Ordering::SeqCst);
    let run = RunDir::of_project(root);
    let timeout = rdy_engine::serve_config(root).start_timeout;
    run.ensure().map_err(|e| {
        RdyError::new(
            E_IO,
            format!(
                "could not create the run directory {}: {e}",
                run.path().display()
            ),
            "check the project directory's permissions",
        )
    })?;
    let start = start_lock(&run, Instant::now() + timeout)
        .await
        .map_err(rdy_error)?;
    stop_daemon(root, &run, timeout, None)
        .await
        .map_err(rdy_error)?;
    apply_while_locked(root, &planned, registry, &start, Instant::now() + timeout).await
}

/// Asks the project's daemon to stop ([`request_stop`]) and waits for it to exit: its process
/// when its pid is known, else its endpoint, until it stops accepting. Only a proved end ends the
/// wait ([`Liveness`]). Nothing is done when no daemon answers. The wait ends after [`STOP_WAIT`],
/// or at `deadline` when that comes first (a connection's own deadline).
///
/// None of this proves the store free: no reply, a missing record and a closed endpoint say
/// nothing of a process that still drains. What upgrades a store takes the store's lock itself,
/// under `start.lock` ([`apply_while_locked`]).
pub(crate) async fn stop_daemon(
    root: &Path,
    run: &RunDir,
    start_timeout: Duration,
    deadline: Option<Instant>,
) -> Result<(), ErrorReply> {
    let Some(hello) = hello(root, run) else {
        return Ok(());
    };
    let Some(StopRequest { pid, socket, .. }) = request_stop(run, &hello, start_timeout).await?
    else {
        return Ok(());
    };
    let started = Instant::now();
    let until = deadline.map_or(started + STOP_WAIT, |d| d.min(started + STOP_WAIT));
    loop {
        let seen = match pid {
            Some(p) => sys::process_liveness(p),
            None => sys::endpoint_liveness(&socket).await,
        };
        if seen == Liveness::Gone {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(ErrorReply::new(
                E_DAEMON_GONE,
                format!(
                    "the daemon was asked to stop for the upgrade but has not exited within {} s",
                    until.saturating_duration_since(started).as_secs()
                ),
                "look at .rdy/run/daemon.log; run rdy serve --stop, then retry rdy upgrade",
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Takes `start.lock`, retrying until `deadline`: the lock `connect_or_start` spawns under.
async fn start_lock(run: &RunDir, deadline: Instant) -> Result<StartGuard, ErrorReply> {
    let path = run.start_lock();
    loop {
        match StartGuard::try_take(run.rdy_dir()) {
            Ok(Some(start)) => return Ok(start),
            Ok(None) => {}
            Err(e) => {
                return Err(ErrorReply::new(
                    E_DAEMON_START,
                    format!("could not lock {}: {e}", path.display()),
                    "check the run directory's permissions, then retry",
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(ErrorReply::new(
                E_DAEMON_START,
                "another client has been starting the daemon during the whole upgrade wait",
                "retry once the other rdy command ends",
            ));
        }
        tokio::time::sleep(LOCK_RETRY).await;
    }
}

/// Applies `planned` while the caller holds `start.lock`, which `start` lends. A store still held
/// by a daemon that is draining (`E_LOCKED`, `E_HELD_BY_READER`) is retried until `deadline`.
async fn apply_while_locked(
    root: &Path,
    planned: &UpgradePlan,
    registry: Option<&[UpgradeStep]>,
    start: &StartGuard,
    deadline: Instant,
) -> Result<UpgradeReport, RdyError> {
    loop {
        APPLY_HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
        match apply(root, planned, registry, start) {
            Err(e)
                if (e.code == E_LOCKED || e.code == E_HELD_BY_READER)
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(POLL).await;
            }
            out => return out,
        }
    }
}

/// The upgrade an ordinary connection runs before it starts a daemon, while `connect_or_start`
/// holds `start.lock` and no daemon of this rdy answers. A project with no store `FORMAT` file
/// has nothing to do here (its daemon's own open speaks for it). A store the planner refuses
/// (a preview store's dump-and-load refusal, an unknown incompatible feature) is that refusal;
/// a store that needs steps or its pending completion is upgraded and completed, with the full
/// fsck before `FORMAT` is published; a store with an unknown read-only feature is never
/// upgraded. Any other planner failure is left to the daemon's open.
pub(crate) async fn before_start(
    run: &RunDir,
    start: &StartGuard,
    deadline: Instant,
) -> Result<(), ErrorReply> {
    let root = run.root();
    let Some(planned) = start_plan(&root)? else {
        return Ok(());
    };
    COORDINATOR_RUNS.fetch_add(1, Ordering::SeqCst);
    let registry = start_registry();
    apply_while_locked(&root, &planned, registry.as_deref(), start, deadline)
        .await
        .map(|_| ())
        .map_err(ErrorReply::from)
}

/// What a launched daemon checks once it owns the start, before it opens the store: its starter
/// planned before it let go, and the store may have moved since. A store that needs work now gets
/// it here, under the same guard. A store still held (a predecessor that drains) is left to the
/// open that follows, which retries and plans again.
pub(crate) fn revalidate_plan(root: &Path, start: &StartGuard) -> Result<(), RdyError> {
    let Some(planned) = start_plan(root).map_err(rdy_error)? else {
        return Ok(());
    };
    match apply(root, &planned, start_registry().as_deref(), start) {
        Err(e) if e.code == E_LOCKED || e.code == E_HELD_BY_READER => Ok(()),
        out => out.map(|_| ()),
    }
}

/// The plan [`before_start`] acts on: `None` when there is no work for it.
fn start_plan(root: &Path) -> Result<Option<UpgradePlan>, ErrorReply> {
    let format = m::store_dir(root).join(rdy_store::format::FORMAT_FILE);
    if std::fs::symlink_metadata(&format).is_err() {
        return Ok(None);
    }
    match plan(root, start_registry().as_deref()) {
        Ok(p) if m::needs_work(&p) => Ok(Some(p)),
        Ok(_) => Ok(None),
        Err(e) if e.code == rdy_engine::E_STORE_FORMAT => Err(e.into()),
        Err(_) => Ok(None),
    }
}

/// Whether the store of the project at `run` needs work before a daemon of this rdy serves it:
/// a refused version's daemon is then stopped first ([`stop_daemon`]).
pub(crate) fn needs_upgrade(run: &RunDir) -> bool {
    matches!(start_plan(&run.root()), Ok(Some(_)))
}
