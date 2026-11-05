#![cfg(unix)]
// Unix only: a script stands in for the rdy binary and a Unix socket for the old daemon. Tasks 17 and 25b repeat these cases natively.
//! The start lock and the handoff from a client to the daemon it launches (Task 8a). The daemon
//! here is this test binary, re-executed through a script at `rdy_bin`, so the real
//! `serve_command`, `spawn_serve` and `serve::start` run and a test can stop the child at any
//! point of its start ([`StartPoint`]) with a file as the barrier.
use rdy_daemon::lock::FileLock;
use rdy_daemon::maintenance::{with_apply_hook_for_tests, with_start_registry_for_tests};
use rdy_daemon::serve::{StartPoint, with_start_hook_for_tests};
use rdy_daemon::{
    Client, E_DAEMON_START, Proto, RunDir, ServeOptions, Spawned, StartOptions, connect_or_start,
    process_alive, serve_command, spawn_serve, start,
};
use rdy_engine::{CompletionPhase, Rdy, with_completion_hook_for_tests};
use rdy_store::StoreLock;
use rdy_store::format::{FEATURES_FILE, StoreFeature, StoreFeatures, registered_features};
use rdy_store::layout::RUN_DIR;
use rdy_store::start_lock::{HandoffToken, RESERVATION_FILE, StartGuard};
use rdy_store::upgrade::UpgradeStep;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The child's project; unset in a normal test run.
const ENV_PROJECT: &str = "WX8A_PROJECT";
/// The start point where the child acts, and what it does there.
const ENV_POINT: &str = "WX8A_POINT";
const ENV_ACT: &str = "WX8A_ACT";
/// The file the child's point writes its pid to, and the file it waits for.
const ENV_AT: &str = "WX8A_AT";
const ENV_GO: &str = "WX8A_GO";

/// A long bound for what a barrier waits on: a failure message, never a measure.
const BARRIER: Duration = Duration::from_secs(120);

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Waits until `f` holds, or panics naming `what` once [`BARRIER`] has passed.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + BARRIER;
    while !f() {
        assert!(Instant::now() < until, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The error of `out`, which must have failed for the reason `what` names. The success types here
/// have no `Debug`, so `expect_err` does not apply.
fn failed<T, E>(out: Result<T, E>, what: &str) -> E {
    match out {
        Err(e) => e,
        Ok(_) => panic!("{what}"),
    }
}

/// A fresh project under a directory short enough for its socket, and the script that runs this
/// test binary as its daemon.
struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    run: RunDir,
    script: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("rdyd")
            .tempdir_in("/tmp")
            .unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("p");
        std::fs::create_dir(&root).unwrap();
        Rdy::init(&root, "rdy-standard").unwrap().close().unwrap();
        let script = dir.path().join("rdy-child");
        let exe = std::env::current_exe().unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nexec '{}' --exact wx8a_child_serves --test-threads=1 --nocapture\n",
                exe.display()
            ),
        )
        .unwrap();
        rdy_store::pio::set_mode(&script, 0o755).unwrap();
        let run = RunDir::of_project(&root);
        Fixture {
            dir,
            root,
            run,
            script,
        }
    }

    fn rdy_dir(&self) -> PathBuf {
        self.root.join(".rdy")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Options whose spawn launches this test binary as the daemon, acting at `act`, and count.
    fn options(
        &self,
        act: Option<(&'static str, String)>,
        count: Arc<AtomicUsize>,
        start_timeout: Duration,
    ) -> StartOptions {
        let (script, project) = (self.script.clone(), self.root.clone());
        let (at, go) = (self.file("at"), self.file("go"));
        let mut o = StartOptions::new(
            env!("CARGO_PKG_VERSION").to_string(),
            Proto::Ctl,
            self.root.clone(),
            Box::new(move |run: &RunDir| -> std::io::Result<Spawned> {
                count.fetch_add(1, Ordering::SeqCst);
                let mut cmd = serve_command(&script, &project, run)?;
                cmd.env(ENV_PROJECT, &project)
                    .env(ENV_AT, &at)
                    .env(ENV_GO, &go);
                if let Some((point, act)) = &act {
                    cmd.env(ENV_POINT, point).env(ENV_ACT, act);
                }
                loop {
                    match spawn_serve(&mut cmd) {
                        // another thread's fork held the just-written script open for a moment
                        Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                            std::thread::yield_now();
                        }
                        out => return out,
                    }
                }
            }),
        );
        o.start_timeout = start_timeout;
        o
    }

    /// Stops the daemon `c` is connected to and waits for its process to end.
    fn stop(&self, rt: &tokio::runtime::Runtime, mut c: Client) {
        let pid = rt.block_on(c.ctl("ping", json!({}))).unwrap()["pid"]
            .as_u64()
            .unwrap() as u32;
        rt.block_on(c.ctl("stop", json!({}))).unwrap();
        c.close();
        wait_for("the daemon's exit", || !process_alive(pid));
    }

    fn reservation(&self) -> Vec<u8> {
        std::fs::read(self.rdy_dir().join(RUN_DIR).join(RESERVATION_FILE)).unwrap_or_default()
    }

    fn raw_lock_free(&self) -> bool {
        FileLock::try_take(&self.run.start_lock())
            .unwrap()
            .is_some()
    }
}

/// The re-executed daemon: it returns at once in a normal test run.
#[test]
fn wx8a_child_serves() {
    let Some(project) = std::env::var_os(ENV_PROJECT) else {
        return;
    };
    let project = PathBuf::from(project);
    let run = RunDir::of_project(&project);
    let point = std::env::var(ENV_POINT).unwrap_or_default();
    let act = std::env::var(ENV_ACT).unwrap_or_default();
    let at = PathBuf::from(std::env::var_os(ENV_AT).unwrap());
    let go = PathBuf::from(std::env::var_os(ENV_GO).unwrap());
    let hook = move |p: StartPoint| -> std::io::Result<()> {
        if p == StartPoint::Opened {
            // every open happens under the OS lock: the file says what a new opener found
            let held = FileLock::try_take(&run.start_lock())?.is_none();
            std::fs::write(at.with_extension("opened"), held.to_string())?;
        }
        if format!("{p:?}") != point {
            return Ok(());
        }
        std::fs::write(&at, std::process::id().to_string())?;
        match act.split_once(':').unwrap_or((act.as_str(), "")) {
            ("exit", code) => std::process::exit(code.parse().unwrap()),
            ("fail", _) => Err(std::io::Error::other("the test fails this start")),
            ("wait", _) => {
                wait_for("the test's go", || go.exists());
                Ok(())
            }
            (other, _) => panic!("unknown act {other}"),
        }
    };
    let served = with_start_hook_for_tests(hook, || {
        start(ServeOptions {
            project,
            version: env!("CARGO_PKG_VERSION").to_string(),
            detach: true,
            idle_timeout: Some(BARRIER),
            human_actor: None,
            site_lan_override: None,
        })
    });
    match served {
        Ok(run) => {
            let _ = run.wait();
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// A client takes the lock, launches the daemon and reaches `ready`; the daemon owned the start
/// while it opened the store, and nothing is left held once it serves.
#[test]
fn wx8a_auto_start_under_parent_lock_reaches_ready() {
    let f = Fixture::new();
    let rt = rt();
    let n = Arc::new(AtomicUsize::new(0));
    let opts = f.options(None, n.clone(), BARRIER);
    let mut c = rt.block_on(connect_or_start(&f.run, opts)).unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    let pid = rt.block_on(c.ctl("ping", json!({}))).unwrap()["pid"]
        .as_u64()
        .unwrap();
    assert_ne!(pid, u64::from(std::process::id()), "another process serves");
    assert_eq!(
        std::fs::read_to_string(f.file("at").with_extension("opened")).unwrap(),
        "true",
        "the daemon held start.lock across its open"
    );
    assert_eq!(f.reservation(), b"", "the reservation ended");
    assert!(f.raw_lock_free(), "start.lock is free once it serves");
    assert!(
        StartGuard::try_take(&f.rdy_dir()).unwrap().is_some(),
        "a later owner takes the guard"
    );
    f.stop(&rt, c);
}

/// A rival arrives in the window where the parent has let the OS lock go and its daemon has not
/// taken it: it spawns nothing and cannot take the guard, and it uses the daemon once that serves.
#[test]
fn wx8a_racing_start_cannot_steal_handoff() {
    let f = Fixture::new();
    let n = Arc::new(AtomicUsize::new(0));
    let opts = f.options(Some(("Claimed", "wait:".into())), n.clone(), BARRIER);
    let run = f.run.clone();
    let parent = std::thread::spawn(move || {
        let rt = rt();
        let c = rt.block_on(connect_or_start(&run, opts)).unwrap();
        (rt, c)
    });
    let at = f.file("at");
    wait_for("the daemon's claim", || at.exists());
    wait_for("the parent's release", || f.raw_lock_free());

    assert!(
        StartGuard::try_take(&f.rdy_dir()).unwrap().is_none(),
        "the OS lock is free, and the start is still the daemon's"
    );
    let rt2 = rt();
    let spawned = Arc::new(AtomicUsize::new(0));
    let e = failed(
        rt2.block_on(connect_or_start(
            &f.run,
            f.options(None, spawned.clone(), Duration::from_secs(1)),
        )),
        "the rival waits and gives up",
    );
    assert_eq!(e.code, E_DAEMON_START);
    assert_eq!(
        e.message,
        "another client has been starting the daemon for 1 s"
    );
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "no duplicate spawn");

    std::fs::write(f.file("go"), b"").unwrap();
    let (rt, mut c) = parent.join().unwrap();
    let served_by = rt.block_on(c.ctl("ping", json!({}))).unwrap()["pid"].clone();
    let mut rival = rt2
        .block_on(connect_or_start(
            &f.run,
            f.options(None, spawned.clone(), BARRIER),
        ))
        .unwrap();
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "the rival uses it");
    assert_eq!(
        rt2.block_on(rival.ctl("ping", json!({}))).unwrap()["pid"],
        served_by
    );
    rival.close();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    f.stop(&rt, c);
}

/// Death, a failed open and a timeout each end the handoff and free what the failed side held,
/// and a later start works. The last case has another owner by then: the late child leaves that
/// owner's lock and reservation alone.
#[test]
fn wx8a_failed_handoff_releases_only_its_own_lock() {
    let rt = rt();
    for (point, act, text) in [
        (
            "Claimed",
            "exit:7",
            "the daemon exited with code 7 before it was ready",
        ),
        (
            "Guarded",
            "exit:8",
            "the daemon exited with code 8 before it was ready",
        ),
        (
            "Guarded",
            "fail:",
            "the daemon exited with code 1 before it was ready",
        ),
    ] {
        let f = Fixture::new();
        let n = Arc::new(AtomicUsize::new(0));
        let e = rt
            .block_on(connect_or_start(
                &f.run,
                f.options(Some((point, act.into())), n.clone(), BARRIER),
            ))
            .err()
            .unwrap_or_else(|| panic!("{point} {act}: the start fails"));
        assert_eq!(e.code, E_DAEMON_START, "{point} {act}");
        assert!(e.message.starts_with(text), "{point} {act}: {}", e.message);
        assert!(f.raw_lock_free(), "{point} {act}: start.lock is free");
        let guard = StartGuard::try_take(&f.rdy_dir()).unwrap();
        assert!(guard.is_some(), "{point} {act}: nothing is reserved");
        assert_eq!(f.reservation(), b"", "{point} {act}");
        drop(guard);
        let c = rt
            .block_on(connect_or_start(
                &f.run,
                f.options(None, n.clone(), BARRIER),
            ))
            .unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 2, "{point} {act}: a later start");
        f.stop(&rt, c);
    }

    // the parent times out before its child claims, and another owner comes
    let f = Fixture::new();
    let n = Arc::new(AtomicUsize::new(0));
    let e = failed(
        rt.block_on(connect_or_start(
            &f.run,
            f.options(
                Some(("BeforeClaim", "wait:".into())),
                n.clone(),
                Duration::from_secs(1),
            ),
        )),
        "the parent gives up",
    );
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        e.message
            .starts_with("the daemon did not become ready within 1 s"),
        "{}",
        e.message
    );
    assert_eq!(f.reservation(), b"", "the parent withdrew its reservation");
    let child: u32 = std::fs::read_to_string(f.file("at"))
        .unwrap()
        .parse()
        .unwrap();
    let winner = StartGuard::try_take(&f.rdy_dir()).unwrap().expect("free");
    let token = HandoffToken::new();
    let reserved = winner.reserve(u32::MAX, &token).unwrap().unwrap();
    let theirs = f.reservation();
    std::fs::write(f.file("go"), b"").unwrap();
    wait_for("the late child's exit", || !process_alive(child));
    assert_eq!(f.reservation(), theirs, "the winner's record is untouched");
    assert!(!f.raw_lock_free(), "the winner still holds start.lock");
    assert!(
        StoreLock::take(&f.rdy_dir().join("store")).is_ok(),
        "the late child never opened the store"
    );
    assert!(reserved.withdraw().unwrap());
    drop(winner);
}

/// One upgrade step, from any manifest to one with the registered `assessment` feature.
fn registry() -> Vec<UpgradeStep> {
    let feature: StoreFeature = registered_features()
        .into_iter()
        .find(|f| f.name == "assessment")
        .expect("assessment is registered");
    vec![UpgradeStep {
        id: format!("add_{}", feature.name),
        from: StoreFeatures::of(vec![]),
        to: StoreFeatures::of(vec![feature]),
    }]
}

fn format_text(f: &Fixture) -> String {
    std::fs::read_to_string(f.rdy_dir().join("store").join(FEATURES_FILE)).unwrap()
}

/// What a completion hook finds at each phase of an upgrade: whether the guard was busy.
fn busy_at_each_phase(
    rdy: PathBuf,
    seen: Arc<Mutex<Vec<(CompletionPhase, bool)>>>,
) -> impl FnMut(CompletionPhase) -> Result<(), rdy_engine::RdyError> + 'static {
    move |p| {
        let busy = StartGuard::try_take(&rdy).unwrap().is_none();
        seen.lock().unwrap().push((p, busy));
        Ok(())
    }
}

const ALL_BUSY: [(CompletionPhase, bool); 3] = [
    (CompletionPhase::BeforeFsck, true),
    (CompletionPhase::Checked, true),
    (CompletionPhase::Published, true),
];

/// `rdy upgrade` holds start.lock from its stop through the upgrade and the reopen. The engine's
/// upgrade borrows that guard: a second take of it would be refused, and fail the upgrade.
#[test]
fn wx8a_explicit_upgrade_borrows_guard_without_reacquiring() {
    let f = Fixture::new();
    let seen: Arc<Mutex<Vec<(CompletionPhase, bool)>>> = Arc::default();
    let report =
        with_completion_hook_for_tests(busy_at_each_phase(f.rdy_dir(), seen.clone()), || {
            rdy_daemon::maintenance::run_with_registry_for_tests(&f.root, false, &registry())
        })
        .unwrap();
    assert!(report.changed && report.checked);
    assert_eq!(seen.lock().unwrap().as_slice(), ALL_BUSY);
    assert!(f.raw_lock_free(), "released at its end");
}

/// A connection's upgrade before its spawn borrows the guard the connection holds.
#[test]
fn wx8a_automatic_upgrade_borrows_guard_without_reacquiring() {
    let f = Fixture::new();
    let seen: Arc<Mutex<Vec<(CompletionPhase, bool)>>> = Arc::default();
    let at_spawn: Arc<Mutex<Option<bool>>> = Arc::default();
    let (rdy, spawn_saw) = (f.rdy_dir(), at_spawn.clone());
    let opts = StartOptions::new(
        env!("CARGO_PKG_VERSION").to_string(),
        Proto::Ctl,
        f.root.clone(),
        Box::new(move |_run: &RunDir| -> std::io::Result<Spawned> {
            *spawn_saw.lock().unwrap() = Some(StartGuard::try_take(&rdy).unwrap().is_none());
            Err(std::io::Error::other("this test spawns no daemon"))
        }),
    );
    let rt = rt();
    let out = with_completion_hook_for_tests(busy_at_each_phase(f.rdy_dir(), seen.clone()), || {
        with_start_registry_for_tests(registry(), || {
            rt.block_on(connect_or_start(&f.run, opts))
                .map(Client::close)
        })
    });
    let e = out.expect_err("the test's spawn fails");
    assert_eq!(
        e.message,
        "could not start the daemon: this test spawns no daemon"
    );
    assert_eq!(seen.lock().unwrap().as_slice(), ALL_BUSY);
    assert_eq!(
        *at_spawn.lock().unwrap(),
        Some(true),
        "still held at the spawn"
    );
    assert!(f.raw_lock_free(), "released on the failed start");
    assert_eq!(f.reservation(), b"");
}

/// A guard of another project opens nothing and upgrades nothing here.
#[test]
fn wx8a_an_upgrade_refuses_another_projects_guard() {
    let (f, other) = (Fixture::new(), Fixture::new());
    let foreign = StartGuard::try_take(&other.rdy_dir()).unwrap().unwrap();
    let planned =
        rdy_engine::maintenance::plan_with_registry_for_tests(&f.root, &registry()).unwrap();
    let before = format_text(&f);
    let e = rdy_engine::maintenance::apply_planned_under_with_registry_for_tests(
        &f.root,
        &planned,
        &foreign,
        &registry(),
    )
    .err()
    .expect("refused");
    assert_eq!(e.code, rdy_engine::E_LOCKED);
    assert_eq!(format_text(&f), before, "nothing changed");
    let e = failed(Rdy::open_under_start(&f.root, &foreign), "refused");
    assert_eq!(e.code, rdy_engine::E_LOCKED);
}

/// What the fake old daemon has seen: hellos it answered, and connections that sent nothing.
#[derive(Default)]
struct OldDaemon {
    hellos: AtomicUsize,
    probes: AtomicUsize,
}

/// An old daemon that answers each hello `exiting`, which gives its client no pid, and keeps its
/// endpoint accepting until `close` sends.
fn old_daemon(
    run: &RunDir,
) -> (
    Arc<OldDaemon>,
    tokio::sync::oneshot::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    run.ensure().unwrap();
    let socket = run.socket();
    let seen = Arc::new(OldDaemon::default());
    let (close, closed) = tokio::sync::oneshot::channel::<()>();
    let (bound_tx, bound) = mpsc::channel();
    let thread = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                bound_tx.send(()).unwrap();
                let accept = async {
                    loop {
                        let (stream, _) = listener.accept().await.unwrap();
                        let seen = seen.clone();
                        tokio::spawn(async move {
                            let (read, mut write) = stream.into_split();
                            let mut line = String::new();
                            let n = BufReader::new(read).read_line(&mut line).await.unwrap_or(0);
                            if n == 0 {
                                seen.probes.fetch_add(1, Ordering::SeqCst);
                            } else {
                                seen.hellos.fetch_add(1, Ordering::SeqCst);
                                let reply =
                                    serde_json::to_string(&rdy_daemon::HelloReply::Exiting {
                                        version: "0.0.0-old".into(),
                                    })
                                    .unwrap();
                                let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                            }
                        });
                    }
                };
                tokio::select! {
                    _ = accept => {}
                    _ = closed => {}
                }
                drop(listener);
                std::fs::remove_file(&socket).unwrap();
            });
        })
    };
    bound.recv().unwrap();
    (seen, close, thread)
}

/// The upgrade run on a thread, with each store try it makes sent on the returned channel.
fn upgrade_on_thread(
    root: PathBuf,
) -> (
    mpsc::Receiver<()>,
    std::thread::JoinHandle<Result<rdy_store::upgrade::UpgradeReport, rdy_engine::RdyError>>,
) {
    let (tried, tries) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        with_apply_hook_for_tests(
            move || {
                let _ = tried.send(());
            },
            || rdy_daemon::maintenance::run_with_registry_for_tests(&root, false, &registry()),
        )
    });
    (tries, thread)
}

/// No usable pid: the coordinator waits for the old endpoint to stop accepting, and then for the
/// store's lock, which it takes itself under start.lock. A daemon that never answers (the no-reply
/// path) proves nothing either: the store's lock still decides.
#[test]
fn wx8a_shutdown_without_usable_pid_waits_for_pipe_and_store() {
    let f = Fixture::new();
    let before = format_text(&f);
    let held = StoreLock::take(&f.rdy_dir().join("store")).unwrap();
    let (old, close, server) = old_daemon(&f.run);
    let (tries, upgrade) = upgrade_on_thread(f.root.clone());

    // an endpoint that still accepts is still live, however long it takes
    wait_for("the stop's hello", || {
        old.hellos.load(Ordering::SeqCst) >= 1
    });
    wait_for("two looks at the endpoint", || {
        old.probes.load(Ordering::SeqCst) >= 2
    });
    assert!(!upgrade.is_finished());
    assert!(
        tries.try_recv().is_err(),
        "no store try while the endpoint accepts"
    );
    assert!(
        StartGuard::try_take(&f.rdy_dir()).unwrap().is_none(),
        "the coordinator holds start.lock across the wait"
    );

    // the endpoint is gone and the old process still holds the store
    close.send(()).unwrap();
    server.join().unwrap();
    tries.recv_timeout(BARRIER).expect("a first try");
    tries
        .recv_timeout(BARRIER)
        .expect("a second try: E_LOCKED is retried");
    assert!(!upgrade.is_finished());
    assert_eq!(
        format_text(&f),
        before,
        "no upgrade while the store is held"
    );

    drop(held);
    let report = upgrade.join().unwrap().unwrap();
    assert!(report.changed && report.checked);
    assert_ne!(format_text(&f), before);
    assert!(f.raw_lock_free());

    // the no-reply path: nothing answers, and the store's lock still decides
    let f = Fixture::new();
    let before = format_text(&f);
    let held = StoreLock::take(&f.rdy_dir().join("store")).unwrap();
    let (tries, upgrade) = upgrade_on_thread(f.root.clone());
    tries.recv_timeout(BARRIER).expect("a first try");
    tries.recv_timeout(BARRIER).expect("a second try");
    assert!(!upgrade.is_finished());
    assert_eq!(format_text(&f), before, "no reply is not a free store");
    drop(held);
    let report = upgrade.join().unwrap().unwrap();
    assert!(report.changed);
}
