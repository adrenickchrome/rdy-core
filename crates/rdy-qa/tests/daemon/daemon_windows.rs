#![cfg(windows)]
//! The daemon on Windows through the real `rdy.exe` and the harness: a pipe is no file, so its
//! name proves nothing on a disk. A shutdown is proved by the process, by the transport and by the
//! store's lock. Also the counterparts of the Unix-only cases in `daemon_short_socket_unix` and
//! `cli_daemon_unix`, and the native repeats of Task 8a's upgrade witnesses (`start_handoff.rs` in
//! rdy-daemon is Unix only).
use crate::daemon_short_socket::{
    KillOnDrop, StopOnDrop, copy_project, long_project, ok, one_daemon_through, ping_pid,
};
use rdy_daemon::lock::FileLock;
use rdy_daemon::maintenance::{
    run_with_registry_for_tests, with_apply_hook_for_tests, with_start_registry_for_tests,
};
use rdy_daemon::sys::{self, records::PIPE_PREFIX};
use rdy_daemon::{
    Attempt, Client, HelloReply, PROTO_VERSION, Proto, RunDir, Spawned, StartOptions, connect,
    connect_or_start,
};
use rdy_engine::{CompletionPhase, Rdy, with_completion_hook_for_tests};
use rdy_qa::daemon::{
    Served, child_daemon_entry, client, endpoint_gone, endpoint_live, exited, human_command,
    short_tempdir, spawn_child_daemon, wait_endpoint_gone,
};
use rdy_store::StoreLock;
use rdy_store::format::{FEATURES_FILE, StoreFeature, StoreFeatures, registered_features};
use rdy_store::start_lock::StartGuard;
use rdy_store::upgrade::UpgradeStep;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A long bound for what a test waits on: a failure message, never a measure.
const BARRIER: Duration = Duration::from_secs(120);

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Waits until `f` holds, or panics naming `what` once [`BARRIER`] has passed.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + BARRIER;
    while !f() {
        assert!(Instant::now() < until, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A child daemon of the harness: it returns at once in a normal test run.
#[test]
fn wx25b_child() {
    child_daemon_entry();
}

/// The harness starts the real `rdy.exe`, reaches it over its pipe and stops it, and a child daemon
/// of the test binary too. The end is proved by the process, the pipe and the store's lock.
#[test]
fn wx25b_harness_starts_connects_and_stops_a_pipe_daemon() {
    // the real binary, started by a command
    let s = Served::new();
    let run = s.run_dir();
    assert!(s.daemon_pid().is_none());
    assert!(endpoint_gone(&run), "no daemon yet");
    let (started, v) = s.json(&["--json", "status"]);
    assert!(started, "{v}");
    let pid = s.daemon_pid().expect("a daemon was started");
    let pipe = run.socket();
    assert!(
        pipe.to_str().is_some_and(|p| p.starts_with(PIPE_PREFIX)),
        "{}",
        pipe.display()
    );
    assert!(endpoint_live(&run));
    assert_eq!(run.recorded_socket(), Some(pipe), "the daemon records it");
    assert_eq!(ping_pid(s.dir.path()), u64::from(pid), "one daemon answers");
    let held = Rdy::open(s.dir.path()).map(drop).unwrap_err();
    assert_eq!(held.code, "E_LOCKED", "{held:?}");

    let out = s.rdy(&["serve", "--stop"]);
    ok(&out);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "stopped");
    wait_for("the daemon's exit", || exited(pid));
    wait_endpoint_gone(&run);
    assert!(!run.pid_file().exists());
    assert!(!run.socket_record().exists());
    let r = Rdy::open(s.dir.path()).expect("the store is free");
    assert!(!r.open_report().unclean_shutdown, "the stop closed it");
    drop(r);

    // a child daemon of this test binary, through the client's start
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn, count) = spawn_child_daemon(&rdy_qa::test_name!("wx25b_child"), &run, "1", &[]);
    let pid = rt().block_on(async {
        let mut c = // the clone: a child daemon ends with the last clone of its spawner
        connect_or_start(&run, client(&run, "1", dir.path(), spawn.clone()))
            .await
            .unwrap();
        let pid = c.ctl("ping", json!({})).await.unwrap()["pid"]
            .as_u64()
            .unwrap() as u32;
        c.ctl("stop", json!({})).await.unwrap();
        pid
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_ne!(pid, std::process::id());
    wait_for("the child's exit", || exited(pid));
    wait_endpoint_gone(&run);
}

/// Neither run file is a shutdown. With both gone, the daemon still has its pipe and its store, and
/// a stop over the pipe still reaches it.
#[test]
fn wx25b_a_missing_pipe_record_is_not_a_stop() {
    let s = Served::new();
    let run = s.run_dir();
    let (started, v) = s.json(&["--json", "status"]);
    assert!(started, "{v}");
    let pid = s.daemon_pid().expect("a daemon was started");
    std::fs::remove_file(run.socket_record()).unwrap();
    std::fs::remove_file(run.pid_file()).unwrap();
    assert!(s.daemon_pid().is_none());
    assert!(run.recorded_socket().is_none());
    assert!(!exited(pid), "the process runs");
    assert!(endpoint_live(&run), "the pipe stands");
    let held = Rdy::open(s.dir.path()).map(drop).unwrap_err();
    assert_eq!(held.code, "E_LOCKED", "{held:?}");

    ok(&s.rdy(&["serve", "--stop"]));
    wait_for("the daemon's exit", || exited(pid));
    wait_endpoint_gone(&run);
    drop(Rdy::open(s.dir.path()).expect("the store is free"));
}

/// The child's project, and the directory of the files it and the test talk through.
const ENV_OLD_PROJECT: &str = "WX25B_OLD_PROJECT";
const ENV_OLD_FILES: &str = "WX25B_OLD_FILES";
/// Set when the child has no endpoint: it only holds the store.
const ENV_OLD_SILENT: &str = "WX25B_OLD_SILENT";

/// A daemon of another build that gives its client no pid: it holds the store, answers each hello
/// `exiting` until the file `close` exists, then closes its pipe, and holds the store until the
/// file `release` exists. Each connection adds a line to the file `seen`, and `holding` says it has
/// begun. It returns at once in a normal test run.
#[test]
fn wx25b_child_old_daemon() {
    let Some(project) = std::env::var_os(ENV_OLD_PROJECT).map(PathBuf::from) else {
        return;
    };
    let files = PathBuf::from(std::env::var_os(ENV_OLD_FILES).unwrap());
    let silent = std::env::var_os(ENV_OLD_SILENT).is_some();
    let held = StoreLock::take(&project.join(".rdy").join("store")).unwrap();
    let run = RunDir::of_project(&project);
    let seen = files.join("seen");
    let note = move |what: &str| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&seen)
            .unwrap();
        writeln!(f, "{what}").unwrap();
    };
    let close = files.join("close");
    let serve = async {
        let mut listener = sys::bind(&run.socket()).unwrap();
        std::fs::write(files.join("holding"), b"").unwrap();
        let closed = async {
            while !close.exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        };
        let note = note.clone();
        let accept = async {
            loop {
                let stream = listener.accept().await.unwrap();
                let note = note.clone();
                tokio::spawn(async move {
                    let (read, mut write) = sys::split(stream);
                    let mut line = String::new();
                    let n = BufReader::new(read).read_line(&mut line).await.unwrap_or(0);
                    if n == 0 {
                        note("probe");
                    } else {
                        note("hello");
                        let reply = serde_json::to_string(&HelloReply::Exiting {
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
    };
    if silent {
        std::fs::write(files.join("holding"), b"").unwrap();
    } else {
        rt().block_on(serve);
    }
    wait_for("the test's release", || files.join("release").exists());
    drop(held);
    // an exit with no libtest summary to wait for
    std::process::exit(0);
}

/// A process of this test binary as the old daemon, ended if the test fails first.
struct OldDaemon(std::process::Child);

impl OldDaemon {
    fn start(root: &Path, files: &Path, silent: bool) -> OldDaemon {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            &rdy_qa::test_name!("wx25b_child_old_daemon"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_OLD_PROJECT, root)
        .env(ENV_OLD_FILES, files)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        if silent {
            cmd.env(ENV_OLD_SILENT, "1");
        }
        let child = cmd.spawn().unwrap();
        wait_for("the old daemon's start", || files.join("holding").exists());
        OldDaemon(child)
    }

    fn runs(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }
}

impl Drop for OldDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fresh project under `dir`.
fn project(dir: &Path) -> PathBuf {
    let root = dir.join("p");
    std::fs::create_dir(&root).unwrap();
    Rdy::init(&root, "rdy-standard").unwrap().close().unwrap();
    root
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

fn format_text(root: &Path) -> String {
    std::fs::read_to_string(root.join(".rdy").join("store").join(FEATURES_FILE)).unwrap()
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
            || run_with_registry_for_tests(&root, false, &registry()),
        )
    });
    (tries, thread)
}

/// No usable pid: the coordinator waits for the old pipe to stop accepting, and then for the store's
/// lock, which it takes itself under start.lock. The old daemon is a real process that outlives its
/// pipe. A daemon that never answers (the no-reply path) proves nothing either.
#[test]
fn wx25b_no_pid_shutdown_waits_for_store_release() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(dir.path());
    let run = RunDir::of_project(&root);
    run.ensure().unwrap();
    let before = format_text(&root);
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let mut old = OldDaemon::start(&root, &files, false);
    let looks = |what: &str| {
        std::fs::read_to_string(files.join("seen"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == what)
            .count()
    };
    assert!(run.pid_record().is_none(), "no usable pid");
    assert!(run.recorded_socket().is_none(), "no record");
    let (tries, upgrade) = upgrade_on_thread(root.clone());

    // a pipe that still accepts is still live, however long it takes
    wait_for("the stop's hello", || looks("hello") >= 1);
    wait_for("two looks at the pipe", || looks("probe") >= 2);
    assert!(!upgrade.is_finished());
    assert!(
        tries.try_recv().is_err(),
        "no store try while the pipe accepts"
    );
    assert!(
        StartGuard::try_take(&root.join(".rdy")).unwrap().is_none(),
        "the coordinator holds start.lock across the wait"
    );

    // the pipe is gone, and the old process still runs and holds the store
    std::fs::write(files.join("close"), b"").unwrap();
    tries.recv_timeout(BARRIER).expect("a first try");
    tries
        .recv_timeout(BARRIER)
        .expect("a second try: E_LOCKED is retried");
    assert!(old.runs(), "the old process outlives its pipe");
    assert!(endpoint_gone(&run), "the pipe is gone");
    assert!(!upgrade.is_finished());
    assert_eq!(
        format_text(&root),
        before,
        "no upgrade while the store is held"
    );

    std::fs::write(files.join("release"), b"").unwrap();
    let report = upgrade.join().unwrap().unwrap();
    assert!(report.changed && report.checked);
    assert_ne!(format_text(&root), before);
    assert!(!old.runs(), "the store came free when the process ended");
    assert!(
        FileLock::try_take(&run.start_lock()).unwrap().is_some(),
        "start.lock is released"
    );

    // the no-reply path: nothing answers, and the store's lock still decides
    let dir = tempfile::tempdir().unwrap();
    let root = project(dir.path());
    let before = format_text(&root);
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let mut old = OldDaemon::start(&root, &files, true);
    let (tries, upgrade) = upgrade_on_thread(root.clone());
    tries.recv_timeout(BARRIER).expect("a first try");
    tries.recv_timeout(BARRIER).expect("a second try");
    assert!(old.runs());
    assert!(!upgrade.is_finished());
    assert_eq!(format_text(&root), before, "no reply is not a free store");
    std::fs::write(files.join("release"), b"").unwrap();
    assert!(upgrade.join().unwrap().unwrap().changed);
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

/// Task 8a's witness: `rdy upgrade` holds start.lock from its stop through the upgrade and the
/// reopen, and the engine's upgrade borrows that guard.
#[test]
fn wx25b_explicit_upgrade_borrows_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(dir.path());
    let seen: Arc<Mutex<Vec<(CompletionPhase, bool)>>> = Arc::default();
    let report =
        with_completion_hook_for_tests(busy_at_each_phase(root.join(".rdy"), seen.clone()), || {
            run_with_registry_for_tests(&root, false, &registry())
        })
        .unwrap();
    assert!(report.changed && report.checked);
    assert_eq!(seen.lock().unwrap().as_slice(), ALL_BUSY);
    assert!(
        FileLock::try_take(&RunDir::of_project(&root).start_lock())
            .unwrap()
            .is_some(),
        "released at its end"
    );
}

/// Task 8a's witness: a connection's upgrade before its spawn borrows the guard it holds.
#[test]
fn wx25b_automatic_upgrade_borrows_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(dir.path());
    let run = RunDir::of_project(&root);
    let seen: Arc<Mutex<Vec<(CompletionPhase, bool)>>> = Arc::default();
    let at_spawn: Arc<Mutex<Option<bool>>> = Arc::default();
    let (rdy, spawn_saw) = (root.join(".rdy"), at_spawn.clone());
    let opts = StartOptions::new(
        env!("CARGO_PKG_VERSION").to_string(),
        Proto::Ctl,
        root.clone(),
        Box::new(move |_run: &RunDir| -> std::io::Result<Spawned> {
            *spawn_saw.lock().unwrap() = Some(StartGuard::try_take(&rdy).unwrap().is_none());
            Err(std::io::Error::other("this test spawns no daemon"))
        }),
    );
    let runtime = rt();
    let out =
        with_completion_hook_for_tests(busy_at_each_phase(root.join(".rdy"), seen.clone()), || {
            with_start_registry_for_tests(registry(), || {
                runtime
                    .block_on(connect_or_start(&run, opts))
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
    assert!(
        FileLock::try_take(&run.start_lock()).unwrap().is_some(),
        "released on the failed start"
    );
}

/// The counterpart of `cli2_sigint_and_sigterm_stop_a_foreground_serve_cleanly`: the project's stop
/// event stops a foreground `rdy serve` the way `rdy serve --stop` does.
#[test]
fn wx25b_the_stop_event_stops_a_foreground_serve_cleanly() {
    let s = Served::new();
    let mut child = s.spawn(&["serve"]);
    wait_for("the foreground daemon's pid", || {
        s.daemon_pid() == Some(child.id())
    });
    let run = s.run_dir();
    wait_for("the foreground daemon's pipe", || endpoint_live(&run));
    assert!(
        sys::signal_project_stop(&run.root()).unwrap(),
        "the event stands"
    );
    wait_for("the daemon's exit", || child.try_wait().unwrap().is_some());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    wait_endpoint_gone(&run);
    assert!(!run.pid_file().exists());
    let r = Rdy::open(s.dir.path()).unwrap();
    assert!(!r.open_report().unclean_shutdown);
}

/// The counterpart of `df3_a_short_project_keeps_its_socket_in_rdy_run`: a project's endpoint is a
/// pipe however long its path is, and no file of that name stands in the run directory.
#[test]
fn wx25b_a_project_has_a_pipe_and_no_socket_file() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let run = RunDir::of_project(&project);
    assert!(endpoint_live(&run));
    assert!(
        run.socket()
            .to_str()
            .is_some_and(|p| p.starts_with(PIPE_PREFIX)),
        "{}",
        run.socket().display()
    );
    assert_eq!(run.short_socket(), None);
    assert!(project.join(".rdy/run/daemon.log").exists());
    assert!(!project.join(".rdy/run/rdy.sock").exists());
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    wait_endpoint_gone(&run);
}

/// The counterpart of `df3_two_spellings_of_one_project_reach_one_daemon` and of
/// `df3_every_spelling_of_a_long_project_path_reaches_one_socket`: an 8.3 name, a path with a `.`
/// and a relative `--project` reach one pipe and one daemon.
#[test]
fn wx25b_every_spelling_of_a_project_reaches_one_pipe() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    let alias = rdy_qa::platform::short_alias(&project).unwrap();
    assert_ne!(alias, project, "the 8.3 name is another spelling");
    let dotted = project.join(".");
    let sock = RunDir::of_project(&project).socket();
    for spelling in [&alias, &dotted] {
        assert_eq!(RunDir::of_project(spelling).socket(), sock);
        one_daemon_through(&project, spelling, &project);
        one_daemon_through(&project, &project, spelling);
    }

    // a relative --project from the project's parent's parent reaches the daemon
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let pid = ping_pid(&project);
    let grand = project.parent().unwrap().parent().unwrap();
    let relative = project.strip_prefix(grand).unwrap();
    let out = rdy_qa::fixtures::rdy_command()
        .current_dir(grand)
        .arg("--project")
        .arg(relative)
        .args(["--json", "status"])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .output()
        .unwrap();
    ok(&out);
    assert_eq!(
        ping_pid(&project),
        pid,
        "a relative --project started another daemon"
    );
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    wait_endpoint_gone(&RunDir::of_project(&project));
}

/// `df3_a_restored_project_replaces_its_old_daemon` with the old daemon in this process, whose end
/// can be read. A person copies the project away, deletes it and copies it back. The old daemon's
/// `.rdy` is gone, so the restored project's `status` makes it refuse and leave, and the client
/// starts the project's own daemon.
#[test]
fn wx25b_a_restored_project_replaces_its_old_daemon() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    let old = rdy_daemon::start(rdy_daemon::ServeOptions {
        project: project.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        detach: false,
        idle_timeout: None,
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    let away = base.path().join("away");
    copy_project(&project, &away);
    std::fs::remove_dir_all(&project).unwrap();
    copy_project(&away, &project);
    std::fs::remove_dir_all(&away).unwrap();
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let new = ping_pid(&project) as u32;
    assert_ne!(new, std::process::id(), "it reached the old daemon");
    assert_eq!(old.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
    ok(&human_command(&project, &[], &["serve", "--stop"])
        .output()
        .unwrap());
    wait_for("the new daemon's exit", || exited(new));
}

/// The client's first look at a pipe nobody serves is no daemon, and nothing is started by it.
#[test]
fn wx25b_a_pipe_nobody_serves_is_no_daemon() {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let run = RunDir::of_project(dir.path());
    let hello = rdy_daemon::Hello {
        rdy: PROTO_VERSION,
        version: "1".into(),
        build: rdy_daemon::BUILD.to_string(),
        proto: Proto::Ctl,
        cwd: dir.path().into(),
        project: run.project_id().unwrap(),
    };
    rt().block_on(async {
        assert!(matches!(connect(&run, &hello).await, Attempt::NoDaemon(_)));
    });
    assert!(endpoint_gone(&run));
}

/// A project whose `.rdy` still stands has lost its store under a real daemon, as a delete that
/// stopped part way leaves it. `.rdy` is the one the daemon bound, and the daemon still ends.
#[test]
fn wx25c_a_store_gone_under_a_standing_rdy_dir_stops_the_daemon() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let daemon = ping_pid(&project) as u32;
    let _kill = KillOnDrop(daemon);
    let endpoint = RunDir::of_project(&project);
    std::fs::remove_dir_all(project.join(".rdy").join("store")).unwrap();
    assert!(project.join(".rdy").join("run").is_dir());
    wait_for("the daemon's exit", || exited(daemon));
    wait_endpoint_gone(&endpoint);
}

/// A daemon in this process, on a new project, and the project's store directory.
fn daemon_on_a_new_project(dir: &Path) -> (rdy_daemon::ServeRun, PathBuf) {
    drop(Rdy::init(dir, "rdy-standard").unwrap());
    let run = rdy_daemon::start(rdy_daemon::ServeOptions {
        project: dir.to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        detach: false,
        idle_timeout: None,
        human_actor: None,
        site_lan_override: None,
    })
    .unwrap();
    (run, dir.join(".rdy").join("store"))
}

/// A new directory in the store's place is another store, and the store that stands is no reason
/// to stop.
#[test]
fn wx25c_a_store_replaced_under_a_standing_rdy_dir_stops_the_daemon() {
    let dir = short_tempdir();
    let (run, store) = daemon_on_a_new_project(dir.path());
    assert!(!run.handle().check_project(), "its store stands");
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::create_dir(&store).unwrap();
    assert!(run.handle().check_project());
    assert_eq!(run.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
}

/// `del /s` leaves every directory and takes every file: the store directory stands, and no store
/// is in it.
#[test]
fn wx25c_a_store_emptied_in_its_own_directory_stops_the_daemon() {
    fn remove_files(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                remove_files(&entry.path());
            } else {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
    }
    let dir = short_tempdir();
    let (run, store) = daemon_on_a_new_project(dir.path());
    remove_files(&store);
    assert!(store.is_dir());
    assert!(run.handle().check_project());
    assert_eq!(run.wait().unwrap(), rdy_daemon::ExitReason::ProjectGone);
}

/// The system refuses to move `.rdy` while a daemon has files open below it, however they are
/// shared. Nothing has moved, so the daemon serves on.
#[test]
fn wx25c_a_refused_move_of_rdy_leaves_the_daemon_serving() {
    let base = short_tempdir();
    let project = long_project(base.path());
    let _stop = StopOnDrop(&project);
    ok(&human_command(&project, &[], &["--json", "status"])
        .output()
        .unwrap());
    let daemon = ping_pid(&project);
    let _kill = KillOnDrop(daemon as u32);
    let refused = std::fs::rename(project.join(".rdy"), project.join(".rdy-away")).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(5), "{refused}");
    assert_eq!(ping_pid(&project), daemon);
}
