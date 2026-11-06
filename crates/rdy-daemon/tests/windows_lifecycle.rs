//! A daemon's life on Windows, through the real `connect_or_start`, `serve_command`, `spawn_serve`
//! and `serve`: starts that race, a start that fails at each stage, names held by a daemon that is
//! leaving, and a stop by the event. The daemon here is this test binary, which is why it has its
//! own `main`: `serve_command`'s arguments reach it as they reach `rdy`, and a test harness would
//! refuse them. On every other platform this binary has no tests.

#[cfg(windows)]
mod native {
    use rdy_daemon::lock::FileLock;
    use rdy_daemon::serve::{StartPoint, with_start_hook_for_tests};
    use rdy_daemon::sys::{self, Liveness};
    use rdy_daemon::{
        Attempt, Client, E_DAEMON_START, ErrorReply, Hello, PROTO_VERSION, Proto, RunDir,
        ServeError, ServeOptions, Spawned, StartOptions, connect, connect_or_start, process_alive,
        serve, serve_command, spawn_serve,
    };
    use rdy_engine::Rdy;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const ENV_PROJECT: &str = "WX17_PROJECT";
    const ENV_POINT: &str = "WX17_POINT";
    const ENV_ACT: &str = "WX17_ACT";
    const ENV_AT: &str = "WX17_AT";
    /// A long bound for what a test waits on: a failure message, never a measure.
    const BARRIER: Duration = Duration::from_secs(120);
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const START_FIX: &str =
        "look at .rdy/run/daemon.log; run rdy serve in the foreground to see why";

    /// The daemon this binary is when `serve_command` starts it.
    fn daemon() -> ! {
        let project = PathBuf::from(std::env::var_os(ENV_PROJECT).unwrap());
        let point = std::env::var(ENV_POINT).unwrap_or_default();
        let act = std::env::var(ENV_ACT).unwrap_or_default();
        let at = PathBuf::from(std::env::var_os(ENV_AT).unwrap());
        let hook = move |p: StartPoint| -> std::io::Result<()> {
            if format!("{p:?}") != point {
                return Ok(());
            }
            std::fs::write(&at, std::process::id().to_string())?;
            match act.as_str() {
                "exit" => std::process::exit(7),
                "fail" => Err(std::io::Error::other("the test fails this start")),
                "mark" => Ok(()),
                other => panic!("unknown act {other}"),
            }
        };
        let served = with_start_hook_for_tests(hook, || {
            serve(ServeOptions {
                project,
                version: VERSION.to_string(),
                detach: true,
                idle_timeout: Some(BARRIER),
                human_actor: None,
                site_lan_override: None,
            })
        });
        match served {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let until = Instant::now() + BARRIER;
        while !f() {
            assert!(Instant::now() < until, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A fresh project.
    struct Fixture {
        dir: tempfile::TempDir,
        root: PathBuf,
        run: RunDir,
        spawns: Arc<AtomicUsize>,
        /// A client's connection lives on the runtime that made it.
        rt: tokio::runtime::Runtime,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("p");
            std::fs::create_dir(&root).unwrap();
            Rdy::init(&root, "rdy-standard").unwrap().close().unwrap();
            let run = RunDir::of_project(&root);
            Fixture {
                dir,
                root,
                run,
                spawns: Arc::new(AtomicUsize::new(0)),
                rt: tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            }
        }

        fn at(&self) -> PathBuf {
            self.dir.path().join("at")
        }

        /// Options whose spawn starts this binary as the daemon, acting at `act`.
        fn options(&self, act: Option<(&'static str, &'static str)>) -> StartOptions {
            let (project, at, spawns) = (self.root.clone(), self.at(), self.spawns.clone());
            let exe = std::env::current_exe().unwrap();
            let mut o = StartOptions::new(
                VERSION.to_string(),
                Proto::Ctl,
                self.root.clone(),
                Box::new(move |run: &RunDir| -> std::io::Result<Spawned> {
                    spawns.fetch_add(1, Ordering::SeqCst);
                    let mut cmd = serve_command(&exe, &project, run)?;
                    cmd.env(ENV_PROJECT, &project).env(ENV_AT, &at);
                    if let Some((point, act)) = act {
                        cmd.env(ENV_POINT, point).env(ENV_ACT, act);
                    }
                    spawn_serve(&mut cmd)
                }),
            );
            o.start_timeout = BARRIER;
            o
        }

        fn start(&self, act: Option<(&'static str, &'static str)>) -> Result<Client, ErrorReply> {
            self.rt
                .block_on(connect_or_start(&self.run, self.options(act)))
        }

        /// Stops the daemon `c` is connected to and waits for its process to end.
        fn stop(&self, mut c: Client) -> u32 {
            let pid = pid_of(&self.rt, &mut c);
            self.rt.block_on(c.ctl("stop", json!({}))).unwrap();
            c.close();
            let until = Instant::now() + BARRIER;
            while process_alive(pid) {
                assert!(
                    Instant::now() < until,
                    "process {pid} never ended: {:#?}",
                    rdy_daemon::log_tail(&self.run, 12)
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            pid
        }

        fn hello(&self, version: &str) -> Hello {
            Hello {
                rdy: PROTO_VERSION,
                version: version.to_string(),
                build: rdy_daemon::BUILD.to_string(),
                proto: Proto::Ctl,
                cwd: self.root.clone(),
                project: self.run.project_id().unwrap(),
            }
        }

        /// No record, no pid file, no pipe, and nobody holds the start.
        fn assert_nothing_left(&self) {
            assert!(!self.run.socket_record().exists(), "daemon.sock");
            assert!(!self.run.pid_file().exists(), "daemon.pid");
            self.assert_no_pipe();
            assert!(
                FileLock::try_take(&self.run.start_lock())
                    .unwrap()
                    .is_some(),
                "start.lock is free"
            );
        }

        fn assert_no_pipe(&self) {
            assert_eq!(
                rt().block_on(sys::endpoint_liveness(&self.run.socket())),
                Liveness::Gone
            );
        }
    }

    fn pid_of(rt: &tokio::runtime::Runtime, c: &mut Client) -> u32 {
        rt.block_on(c.ctl("ping", json!({}))).unwrap()["pid"]
            .as_u64()
            .unwrap() as u32
    }

    /// The error of a start that must fail.
    fn failed(out: Result<Client, ErrorReply>, what: &str) -> ErrorReply {
        match out {
            Err(e) => e,
            Ok(_) => panic!("{what}"),
        }
    }

    fn wx17_native_auto_start_reaches_ready_and_a_stop_leaves_nothing() {
        let f = Fixture::new();
        let c = f.start(None).unwrap();
        assert_eq!(f.spawns.load(Ordering::SeqCst), 1);
        assert!(FileLock::try_take(&f.run.start_lock()).unwrap().is_some());
        assert!(f.run.recorded_socket().is_some());
        f.stop(c);
        f.assert_nothing_left();
    }

    fn wx17_native_racing_starts_share_one_daemon() {
        let f = Arc::new(Fixture::new());
        let clients: Vec<_> = (0..8)
            .map(|_| {
                let f = Arc::clone(&f);
                std::thread::spawn(move || f.start(None).unwrap())
            })
            .collect();
        let mut clients: Vec<Client> = clients.into_iter().map(|t| t.join().unwrap()).collect();
        let pids: Vec<u32> = clients.iter_mut().map(|c| pid_of(&f.rt, c)).collect();
        assert!(pids.iter().all(|p| *p == pids[0]), "{pids:?}");
        assert_eq!(f.spawns.load(Ordering::SeqCst), 1);
        let last = clients.pop().unwrap();
        drop(clients);
        f.stop(last);
        f.assert_nothing_left();
    }

    /// A start that fails at any stage says so, holds nothing of the project, and the next start
    /// works. A process that dies leaves what it wrote; the next daemon replaces it.
    fn wx17_native_a_failed_start_tears_down_at_each_stage() {
        for point in [
            "BeforeClaim",
            "Claimed",
            "Guarded",
            "Opened",
            "StopEvent",
            "Bound",
        ] {
            let f = Fixture::new();
            let e = failed(f.start(Some((point, "fail"))), point);
            assert_eq!(e.code, E_DAEMON_START, "{point}: {e:?}");
            assert!(
                e.message.contains("the test fails this start"),
                "{point}: {}",
                e.message
            );
            let pid: u32 = std::fs::read_to_string(f.at()).unwrap().parse().unwrap();
            wait_for("the failed daemon's exit", || !process_alive(pid));
            f.assert_nothing_left();
            f.stop(f.start(None).unwrap());
            f.assert_nothing_left();

            let f = Fixture::new();
            let e = failed(f.start(Some((point, "exit"))), point);
            assert_eq!(e.code, E_DAEMON_START, "{point}: {e:?}");
            assert!(
                e.message.contains("exited with code 7"),
                "{point}: {}",
                e.message
            );
            f.assert_no_pipe();
            f.stop(f.start(None).unwrap());
            f.assert_nothing_left();
        }
    }

    fn holding_the_event(root: &Path) -> (tokio::runtime::Runtime, sys::StopSignals) {
        let rt = rt();
        let signals = {
            let _g = rt.enter();
            sys::StopSignals::install(&RunDir::of_project(root).root()).unwrap()
        };
        (rt, signals)
    }

    /// A stop event someone else keeps stops the start before any endpoint stands: a daemon that
    /// could not be stopped never answers.
    fn wx17_native_a_held_stop_event_refuses_the_start() {
        let f = Fixture::new();
        let held = holding_the_event(&f.root);
        let e = failed(f.start(None), "the start goes on without its event");
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(e.fix, START_FIX);
        let line = e
            .message
            .lines()
            .find(|l| l.contains("could not start the daemon: could not create stop event "))
            .unwrap_or_else(|| panic!("{}", e.message));
        assert!(line.contains(r"Local\rdy-stop-"), "{line}");
        f.assert_no_pipe();
        assert!(!f.run.socket_record().exists());
        drop(held);
        f.stop(f.start(None).unwrap());
    }

    /// A pipe someone else keeps stops the start too, in the words of the plan.
    fn wx17_native_a_held_pipe_refuses_the_start() {
        let f = Fixture::new();
        // a pipe's overlapped handle needs a runtime
        let rt = rt();
        let _g = rt.enter();
        let held = sys::bind_endpoint(&f.run.socket()).unwrap();
        let e = failed(f.start(None), "the start goes on without its pipe");
        assert_eq!(e.code, E_DAEMON_START);
        assert_eq!(e.fix, START_FIX);
        assert!(
            e.message.contains(&format!(
                "could not start the daemon: could not create named pipe {}: ",
                f.run.socket().display()
            )),
            "{}",
            e.message
        );
        drop(held);
        f.stop(f.start(None).unwrap());
    }

    /// A daemon that is leaving holds its names until its handles close. Its successor outlasts
    /// that: the holder here lets go once the successor has the store.
    fn wx17_native_names_held_for_a_moment_are_outlasted() {
        let f = Fixture::new();
        let _g = f.rt.enter();
        let pipe = sys::bind_endpoint(&f.run.socket()).unwrap();
        let event = holding_the_event(&f.root);
        let at = f.at();
        let letting_go = std::thread::spawn(move || {
            wait_for("the successor's open", || at.exists());
            drop(event);
        });
        let c = {
            let f = &f;
            std::thread::scope(|s| {
                let start = s.spawn(move || f.start(Some(("Opened", "mark"))));
                letting_go.join().unwrap();
                drop(pipe);
                start.join().unwrap().unwrap()
            })
        };
        f.stop(c);
        f.assert_nothing_left();
    }

    /// The event stops a daemon that has a session, and a successor can start straight after.
    fn wx17_native_the_stop_event_stops_a_real_daemon() {
        let f = Fixture::new();
        let mut c = f.start(None).unwrap();
        let pid = pid_of(&f.rt, &mut c);
        assert!(sys::signal_project_stop(&f.run.root()).unwrap());
        wait_for("the daemon's exit", || !process_alive(pid));
        assert!(f.rt.block_on(c.ctl("ping", json!({}))).is_err());
        f.assert_nothing_left();
        f.stop(f.start(None).unwrap());
    }

    /// A client of another version or build, alone, replaces the daemon. Its successor starts while
    /// the old process still drains, and finds the names free in time.
    fn wx17_native_a_replaced_daemon_gives_way_to_its_successor() {
        let f = Fixture::new();
        for replace in ["version", "build"] {
            let mut c = f.start(None).unwrap();
            let pid = pid_of(&f.rt, &mut c);
            c.close();
            // the daemon reads the close before the next hello, or that hello latches it
            wait_for("the session's end", || {
                let mut hello = f.hello(if replace == "version" {
                    "0.0.1"
                } else {
                    VERSION
                });
                if replace == "build" {
                    hello.build = "0123456789abcdef".to_string();
                }
                match f.rt.block_on(connect(&f.run, &hello)) {
                    Attempt::Exiting | Attempt::NoDaemon(_) => true,
                    Attempt::Ready(c) => {
                        c.close();
                        false
                    }
                    Attempt::Refused(_) | Attempt::Starting(_) => false,
                }
            });
            let next = f.start(None).unwrap();
            wait_for("the old daemon's exit", || !process_alive(pid));
            let mut next = next;
            assert_ne!(pid_of(&f.rt, &mut next), pid);
            f.stop(next);
            f.assert_nothing_left();
        }
    }

    /// A store held by a process that is no daemon is refused without a spawn.
    fn wx17_native_a_store_held_by_no_daemon_refuses_the_spawn() {
        let f = Fixture::new();
        let held = Rdy::open(&f.root).unwrap();
        let e = failed(f.start(None), "a daemon cannot open a held store");
        assert_eq!(e.code, E_DAEMON_START);
        assert!(e.message.contains("that is not a daemon"), "{}", e.message);
        assert_eq!(f.spawns.load(Ordering::SeqCst), 0);
        held.close().unwrap();
        f.stop(f.start(None).unwrap());
    }

    /// A second daemon of a project with a live one hears what it always heard, in the store's
    /// words: the live daemon's event and pipe never come into it, and that daemon goes on.
    fn wx17_native_a_second_daemon_is_already_running() {
        let f = Fixture::new();
        let mut c = f.start(None).unwrap();
        let second = serve(ServeOptions {
            project: f.root.clone(),
            version: VERSION.to_string(),
            detach: false,
            idle_timeout: None,
            human_actor: None,
            site_lan_override: None,
        });
        assert!(
            matches!(second, Err(ServeError::AlreadyRunning(_))),
            "{second:?}"
        );
        pid_of(&f.rt, &mut c);
        f.stop(c);
        f.assert_nothing_left();
    }

    type Test = (&'static str, fn());

    const TESTS: &[Test] = &[
        (
            "wx17_native_auto_start_reaches_ready_and_a_stop_leaves_nothing",
            wx17_native_auto_start_reaches_ready_and_a_stop_leaves_nothing,
        ),
        (
            "wx17_native_racing_starts_share_one_daemon",
            wx17_native_racing_starts_share_one_daemon,
        ),
        (
            "wx17_native_a_failed_start_tears_down_at_each_stage",
            wx17_native_a_failed_start_tears_down_at_each_stage,
        ),
        (
            "wx17_native_a_held_stop_event_refuses_the_start",
            wx17_native_a_held_stop_event_refuses_the_start,
        ),
        (
            "wx17_native_a_held_pipe_refuses_the_start",
            wx17_native_a_held_pipe_refuses_the_start,
        ),
        (
            "wx17_native_names_held_for_a_moment_are_outlasted",
            wx17_native_names_held_for_a_moment_are_outlasted,
        ),
        (
            "wx17_native_the_stop_event_stops_a_real_daemon",
            wx17_native_the_stop_event_stops_a_real_daemon,
        ),
        (
            "wx17_native_a_replaced_daemon_gives_way_to_its_successor",
            wx17_native_a_replaced_daemon_gives_way_to_its_successor,
        ),
        (
            "wx17_native_a_store_held_by_no_daemon_refuses_the_spawn",
            wx17_native_a_store_held_by_no_daemon_refuses_the_spawn,
        ),
        (
            "wx17_native_a_second_daemon_is_already_running",
            wx17_native_a_second_daemon_is_already_running,
        ),
    ];

    /// Runs every test whose name holds the first plain argument, in order.
    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().is_some_and(|a| a == "serve") {
            daemon();
        }
        let filter = args.iter().find(|a| !a.starts_with('-'));
        let mut failures = 0;
        let mut ran = 0;
        for (name, test) in TESTS {
            if filter.is_some_and(|f| !name.contains(f.as_str())) {
                continue;
            }
            ran += 1;
            if std::panic::catch_unwind(test).is_ok() {
                println!("test {name} ... ok");
            } else {
                println!("test {name} ... FAILED");
                failures += 1;
            }
        }
        println!("test result: {} passed; {failures} failed", ran - failures);
        std::process::exit(i32::from(failures > 0));
    }
}

fn main() {
    #[cfg(windows)]
    native::main();
}
