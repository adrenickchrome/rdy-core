//! The client's connect-or-start protocol (start1).
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn opts(
    version: &str,
    cwd: &std::path::Path,
    spawn: impl FnMut(&RunDir) -> std::io::Result<()> + Send + 'static,
) -> StartOptions {
    let mut o = StartOptions::new(
        version.into(),
        Proto::Ctl,
        cwd.to_path_buf(),
        Box::new(spawn),
    );
    o.start_timeout = Duration::from_secs(3);
    o
}

/// A spawn closure that starts an in-process echo daemon of `version` on the runtime (counting calls).
fn in_process_spawn(
    version: &'static str,
    count: Arc<AtomicUsize>,
    ready_after: Duration,
) -> impl FnMut(&RunDir) -> std::io::Result<()> + Send + 'static {
    move |run: &RunDir| {
        count.fetch_add(1, Ordering::SeqCst);
        let run = run.clone();
        tokio::spawn(async move {
            let mut d = Daemon::new(
                DaemonConfig::new(run, version.into()),
                Arc::new(Echo::default()),
            );
            d.bind().await.unwrap();
            let r = d.readiness();
            let t = tokio::spawn(d.run());
            r.progress("warming up");
            tokio::time::sleep(ready_after).await;
            r.ready();
            let _ = t.await;
        });
        Ok(())
    }
}

#[tokio::test]
async fn start1_a_running_daemon_is_used_and_nothing_is_spawned() {
    let d = daemon("1", true, |_| {}).await;
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &d.run,
        opts(
            "1",
            d.dir.path(),
            in_process_spawn("1", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 0);
    let v = c.ctl("echo", json!({"k": 1})).await.unwrap();
    assert_eq!(v["args"]["k"], 1);
    assert_eq!(c.session(), SessionId(1));
    d.handle.stop();
}

#[tokio::test]
async fn start1_no_socket_spawns_once_and_waits_for_ready_with_progress() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let n = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(vec![]));
    let mut o = opts(
        "1",
        dir.path(),
        in_process_spawn("1", n.clone(), Duration::from_millis(400)),
    );
    let s2 = seen.clone();
    o.progress = Box::new(move |p| s2.lock().unwrap().push(p.to_string()));
    let mut c = connect_or_start(&run, o).await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(c.ctl("ping", json!({})).await.unwrap()["sessions"], 1);
    assert!(
        seen.lock().unwrap().contains(&"warming up".to_string()),
        "{:?}",
        seen.lock().unwrap()
    );
    assert!(run.start_lock().exists(), "the lock file stays");
    c.ctl("stop", json!({})).await.unwrap();
}

#[tokio::test]
async fn start1_a_stale_socket_is_treated_like_no_socket() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    drop(std::os::unix::net::UnixListener::bind(run.socket()).unwrap());
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &run,
        opts(
            "1",
            dir.path(),
            in_process_spawn("1", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    c.ctl("stop", json!({})).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start1_eight_clients_racing_spawn_exactly_one_daemon() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let n = Arc::new(AtomicUsize::new(0));
    let mut tasks = vec![];
    for _ in 0..8 {
        let (run, n, cwd) = (run.clone(), n.clone(), dir.path().to_path_buf());
        tasks.push(tokio::spawn(async move {
            let mut c = connect_or_start(
                &run,
                opts(
                    "1",
                    &cwd,
                    in_process_spawn("1", n, Duration::from_millis(300)),
                ),
            )
            .await
            .unwrap();
            c.ctl("ping", json!({})).await.unwrap()["pid"]
                .as_u64()
                .unwrap()
        }));
    }
    let pids: Vec<u64> = futures_join(tasks).await;
    assert_eq!(n.load(Ordering::SeqCst), 1, "exactly one spawn");
    assert!(pids.iter().all(|p| *p == pids[0]), "{pids:?}");
}

async fn futures_join(tasks: Vec<tokio::task::JoinHandle<u64>>) -> Vec<u64> {
    let mut out = vec![];
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

#[tokio::test]
async fn start1_a_spawn_failure_is_e_daemon_start_at_once() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let t0 = std::time::Instant::now();
    let e = connect_or_start(
        &run,
        opts("1", dir.path(), |_| Err(std::io::Error::other("no binary"))),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert_eq!(e.message, "could not start the daemon: no binary");
    assert!(
        e.fix.contains("daemon.log") && e.fix.contains("rdy serve"),
        "{e}"
    );
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn start1_a_daemon_that_never_becomes_ready_is_e_daemon_start_with_the_log_tail() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    std::fs::write(
        run.log_file(),
        (1..=30).map(|i| format!("line {i}\n")).collect::<String>(),
    )
    .unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let mut o = opts(
        "1",
        dir.path(),
        in_process_spawn("1", n, Duration::from_secs(60)),
    );
    o.start_timeout = Duration::from_millis(1500);
    let e = connect_or_start(&run, o).await.unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        e.message
            .starts_with("the daemon did not become ready within 1 s"),
        "{}",
        e.message
    );
    assert!(
        e.message.contains("line 30") && !e.message.contains("line 10\n"),
        "{}",
        e.message
    );
    assert_eq!(e.extra["log_tail"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn start1_a_version_refusal_is_returned_not_retried() {
    let d = daemon("0.1.0", true, |_| {}).await;
    let (_keep, first) = connect_with(&d.run, &hello("0.1.0", "ctl", d.dir.path())).await;
    assert!(first.contains("ready"));
    let n = Arc::new(AtomicUsize::new(0));
    let e = connect_or_start(
        &d.run,
        opts(
            "0.2.0",
            d.dir.path(),
            in_process_spawn("0.2.0", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, E_DAEMON_VERSION);
    assert_eq!(n.load(Ordering::SeqCst), 0);
    d.handle.stop();
}

#[tokio::test]
async fn start1_an_exiting_daemon_is_replaced_by_our_own() {
    let d = daemon("0.1.0", true, |_| {}).await;
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &d.run,
        opts(
            "0.2.0",
            d.dir.path(),
            in_process_spawn("0.2.0", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(c.version(), "0.2.0");
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::VersionReplaced
    ));
    c.ctl("stop", json!({})).await.unwrap();
}

#[tokio::test]
async fn start1_eof_during_the_hello_takes_the_start_path() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    // a listener that accepts and closes at once: the daemon "died" between accept and reply
    let l = tokio::net::UnixListener::bind(run.socket()).unwrap();
    let dead = tokio::spawn(async move {
        let (s, _) = l.accept().await.unwrap();
        drop(s);
        drop(l); // the file stays; connects are now refused
    });
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &run,
        opts(
            "1",
            dir.path(),
            in_process_spawn("1", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap();
    dead.await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    c.ctl("stop", json!({})).await.unwrap();
}

#[tokio::test]
async fn start1_a_closed_connection_is_e_daemon_gone() {
    let d = daemon("1", true, |_| {}).await;
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &d.run,
        opts("1", d.dir.path(), in_process_spawn("1", n, Duration::ZERO)),
    )
    .await
    .unwrap();
    d.handle.stop();
    d.task.await.unwrap().unwrap();
    let e = c.ctl("echo", json!({})).await.unwrap_err();
    assert_eq!(e.code, E_DAEMON_GONE);
    assert_eq!(e.message, "the daemon closed the connection during echo");
    assert!(e.fix.contains("may or may not have committed"), "{e}");
}

#[test]
fn start1_a_released_start_lock_is_free_while_a_spawned_child_still_shares_it() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let held = lock::FileLock::try_take(&run.start_lock())
        .unwrap()
        .expect("free");
    assert!(
        lock::FileLock::try_take(&run.start_lock())
            .unwrap()
            .is_none(),
        "held: busy"
    );
    let child_copy = held.share_descriptor_for_tests().unwrap();
    drop(held);
    let again = lock::FileLock::try_take(&run.start_lock()).unwrap();
    drop(child_copy);
    assert!(
        again.is_some(),
        "a released start.lock is free while a spawned child still holds its descriptor"
    );
}
