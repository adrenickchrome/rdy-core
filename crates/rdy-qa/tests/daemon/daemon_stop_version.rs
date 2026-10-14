//! dl11: `rdy serve --stop` reaches a busy daemon of another version. The client signals the
//! daemon's process only when the socket record ties it to this project and the pid file names
//! it; every other client of another version is still refused (D-ABRC8, D-R7N9J, DOGFOOD 52).
use rdy_daemon::*;
use rdy_qa::daemon::*;
use serde_json::json;
use std::time::{Duration, Instant};

const UNTIED: &str = "the running daemon is another rdy version and has other clients, and rdy cannot tie its process to this project";
const UNTIED_FIX: &str = "close the daemon's other sessions, then run rdy serve --stop again";

#[test]
fn dl11_child() {
    child_daemon_entry();
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn hello(run: &RunDir, version: &str, cwd: &std::path::Path) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: version.into(),
        proto: Proto::Ctl,
        cwd: cwd.to_path_buf(),
        project: run.project_id().unwrap(),
    }
}

fn wait_dead(pid: u32) {
    let t0 = Instant::now();
    while process_alive(pid) {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "process {pid} did not exit"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A 0.1.0 child daemon with one 0.1.0 session open, and its pid.
async fn busy_old(run: &RunDir, dir: &std::path::Path) -> (Client, u32) {
    let (spawn, _) = spawn_child_daemon(&rdy_qa::test_name!("dl11_child"), run, "0.1.0", &[]);
    let mut c = connect_or_start(run, client(run, "0.1.0", dir, spawn))
        .await
        .unwrap();
    let pid = c.ctl("ping", json!({})).await.unwrap()["pid"]
        .as_u64()
        .unwrap() as u32;
    (c, pid)
}

#[test]
fn dl11_stop_reaches_a_busy_daemon_of_another_version() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let pid = rt().block_on(async {
        let (_old, pid) = busy_old(&run, dir.path()).await;
        let r = request_stop(
            &run,
            &hello(&run, "0.2.0", dir.path()),
            Duration::from_secs(10),
        )
        .await
        .unwrap()
        .expect("a daemon runs");
        assert_eq!((r.pid, r.signalled), (Some(pid), true), "{r:?}");
        pid
    });
    wait_dead(pid);
    // the newer client now starts its own daemon
    let (spawn_new, count) =
        spawn_child_daemon(&rdy_qa::test_name!("dl11_child"), &run, "0.2.0", &[]);
    rt().block_on(async {
        let mut c = connect_or_start(&run, client(&run, "0.2.0", dir.path(), spawn_new))
            .await
            .unwrap();
        assert_eq!(c.version(), "0.2.0");
        c.ctl("stop", json!({})).await.unwrap();
    });
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn dl11_a_pid_file_that_names_another_process_is_never_signalled() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let mut bystander = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    rt().block_on(async {
        let (mut old, pid) = busy_old(&run, dir.path()).await;
        std::fs::write(run.pid_file(), format!("{}\n", bystander.id())).unwrap();
        let e = request_stop(
            &run,
            &hello(&run, "0.2.0", dir.path()),
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str(), e.fix.as_str()),
            ("E_DAEMON_VERSION", UNTIED, UNTIED_FIX)
        );
        assert!(process_alive(bystander.id()), "the bystander was signalled");
        assert!(process_alive(pid), "the daemon was signalled");
        old.ctl("ping", json!({}))
            .await
            .expect("the daemon still answers");
        old.ctl("stop", json!({})).await.unwrap();
    });
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}

#[test]
fn dl11_a_record_of_another_project_is_never_followed_to_a_signal() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    rt().block_on(async {
        let (mut old, pid) = busy_old(&run, dir.path()).await;
        // the record now names another `.rdy` (as in a copied project); the socket line stays
        let text = std::fs::read_to_string(run.socket_record()).unwrap();
        let socket = text.lines().next().unwrap().to_string();
        let tmp = run.path().join("daemon.sock.dl11");
        std::fs::write(&tmp, format!("{socket}\n1:1\n")).unwrap();
        std::fs::rename(&tmp, run.socket_record()).unwrap();
        assert!(run.recorded_socket().is_none());
        let e = request_stop(
            &run,
            &hello(&run, "0.2.0", dir.path()),
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str(), e.fix.as_str()),
            ("E_DAEMON_VERSION", UNTIED, UNTIED_FIX)
        );
        assert!(process_alive(pid), "the daemon was signalled");
        old.ctl("ping", json!({}))
            .await
            .expect("the daemon still answers");
        old.ctl("stop", json!({})).await.unwrap();
    });
}

#[test]
fn dl11_every_other_client_of_another_version_is_still_refused() {
    // guard: the hello check stays for everything but the stop (D-ABRC8)
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let (spawn_new, count) =
        spawn_child_daemon(&rdy_qa::test_name!("dl11_child"), &run, "0.2.0", &[]);
    rt().block_on(async {
        let (mut old, pid) = busy_old(&run, dir.path()).await;
        let e = connect_or_start(&run, client(&run, "0.2.0", dir.path(), spawn_new))
            .await
            .unwrap_err();
        assert_eq!(e.code, E_DAEMON_VERSION);
        assert_eq!(
            e.message,
            "the running daemon is rdy 0.1.0, this client is rdy 0.2.0"
        );
        assert_eq!(e.fix, "close the other sessions or run rdy serve --stop");
        assert!(process_alive(pid));
        old.ctl("ping", json!({})).await.unwrap();
        old.ctl("stop", json!({})).await.unwrap();
    });
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn dl11_a_stop_of_the_same_version_asks_over_the_socket() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let pid = rt().block_on(async {
        let (_old, pid) = busy_old(&run, dir.path()).await;
        let r = request_stop(
            &run,
            &hello(&run, "0.1.0", dir.path()),
            Duration::from_secs(10),
        )
        .await
        .unwrap()
        .expect("a daemon runs");
        assert_eq!((r.pid, r.signalled), (Some(pid), false), "{r:?}");
        pid
    });
    wait_dead(pid);
}

#[test]
fn dl11_terminate_never_signals_init_or_a_group() {
    for pid in [0, 1] {
        let e = terminate(pid).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "pid {pid}");
    }
}

#[test]
fn dl11_terminate_says_when_the_process_is_gone() {
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(terminate(pid).unwrap(), "the signal was sent");
    child.wait().unwrap();
    assert!(!terminate(pid).unwrap(), "the process is gone");
}
