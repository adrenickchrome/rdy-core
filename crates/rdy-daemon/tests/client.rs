//! The client's connect-or-start protocol (start1).
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    drop(rdy_daemon::sys::bind(&run.socket()).unwrap());
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
    // lines from an earlier run, then the 30 lines this start's daemon writes
    std::fs::write(run.log_file(), "from an earlier run\n").unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let mut inner = in_process_spawn("1", n, Duration::from_secs(60));
    let mut o = opts("1", dir.path(), move |run: &RunDir| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(run.log_file())
            .unwrap();
        for i in 1..=30 {
            writeln!(f, "line {i}").unwrap();
        }
        inner(run)
    });
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
        e.message.contains("line 30")
            && !e.message.contains("line 10\n")
            && !e.message.contains("earlier run"),
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
    let mut l = rdy_daemon::sys::bind(&run.socket()).unwrap();
    let dead = tokio::spawn(async move {
        let s = l.accept().await.unwrap();
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

fn victim(dir: &std::path::Path) -> std::path::PathBuf {
    let v = dir.join("victim");
    std::fs::write(&v, "keep\n").unwrap();
    #[cfg(unix)]
    chmod(&v, 0o644);
    v
}

fn assert_untouched(v: &std::path::Path) {
    assert_eq!(std::fs::read_to_string(v).unwrap(), "keep\n");
    #[cfg(unix)]
    assert_eq!(mode_bits(v), 0o644);
}

#[tokio::test]
async fn start1_a_symlinked_start_lock_is_refused_and_its_target_is_untouched() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let v = victim(dir.path());
    symlink_file(&v, &run.start_lock());
    match lock::FileLock::try_take(&run.start_lock()) {
        Err(e) => assert!(e.to_string().contains(LINK_REFUSAL), "{e}"),
        Ok(got) => panic!("took a symlinked lock: {}", got.is_some()),
    }
    let n = Arc::new(AtomicUsize::new(0));
    let e = connect_or_start(
        &run,
        opts(
            "1",
            dir.path(),
            in_process_spawn("1", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        e.message
            .starts_with(&format!("could not lock {}: ", run.start_lock().display()))
            && e.message.contains(LINK_REFUSAL),
        "{e}"
    );
    assert_eq!(n.load(Ordering::SeqCst), 0);
    assert_untouched(&v);
}

#[tokio::test]
async fn start1_a_symlinked_daemon_log_is_never_read_into_an_error() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let v = victim(dir.path());
    symlink_file(&v, &run.log_file());
    assert!(log_tail(&run, LOG_TAIL_LINES).is_empty());
    let mut o = opts("1", dir.path(), |_| Ok(running()));
    o.start_timeout = Duration::from_millis(300);
    let e = connect_or_start(&run, o).await.unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        !e.message.contains("keep")
            && e.message
                .ends_with("\n(the daemon wrote nothing to daemon.log)"),
        "{e}"
    );
    assert_eq!(e.extra["log_tail"], json!([]));
    assert_untouched(&v);
}

/// The daemon we spawned is of another version than this client (the rdy on PATH is not this
/// one): it answers `exiting`, and waiting longer cannot help.
#[tokio::test]
async fn start1_an_exiting_reply_from_the_daemon_we_spawned_is_e_daemon_start_at_once() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let n = Arc::new(AtomicUsize::new(0));
    let t0 = std::time::Instant::now();
    let e = connect_or_start(
        &run,
        opts(
            "0.2.0",
            dir.path(),
            in_process_spawn("0.1.0", n.clone(), Duration::ZERO),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        e.message
            .starts_with("the daemon we started is rdy 0.1.0, this client is rdy 0.2.0"),
        "{e}"
    );
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert_eq!(n.load(Ordering::SeqCst), 1, "spawned once, not again");
}

/// `into_stream` hands on the raw stream; bytes the client had already read past its last `ctl`
/// reply would be lost with the buffer, so calling it then is a bug (asserted in debug builds).
#[cfg(debug_assertions)]
#[tokio::test]
#[should_panic(expected = "read past the last ctl reply")]
async fn start1_into_stream_after_reading_past_a_reply_is_a_bug() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let mut l = rdy_daemon::sys::bind(&run.socket()).unwrap();
    // a fake daemon that answers the first request with two lines at once
    tokio::spawn(async move {
        let s = l.accept().await.unwrap();
        let mut s = tokio::io::BufReader::new(s);
        let mut line = String::new();
        s.read_line(&mut line).await.unwrap();
        s.get_mut()
            .write_all(b"{\"status\":\"ready\",\"version\":\"1\",\"build\":\"0123456789abcdef\",\"session\":1}\n")
            .await
            .unwrap();
        line.clear();
        s.read_line(&mut line).await.unwrap();
        s.get_mut()
            .write_all(b"{\"id\":1,\"ok\":1}\n{\"id\":2,\"ok\":2}\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let Attempt::Ready(mut c) = connect(&run, &hello_for(dir.path())).await else {
        panic!("no ready")
    };
    assert_eq!(c.ctl("echo", json!({})).await.unwrap(), json!(1));
    let _ = c.into_stream();
}

/// A spawned daemon that exits before it answers (it found the store locked, say) fails the
/// client within one retry interval, citing the log, rather than at the start timeout.
#[tokio::test]
async fn cli2_a_spawned_daemon_that_exits_before_ready_is_e_daemon_start_at_once() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    std::fs::write(run.log_file(), "from an earlier run\n").unwrap();
    let t0 = std::time::Instant::now();
    let mut o = opts("1", dir.path(), |run: &RunDir| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(run.log_file())
            .unwrap();
        writeln!(f, "already running: the store at x").unwrap();
        Ok(Spawned {
            exited: Box::new(|| Some(2)),
        })
    });
    o.start_timeout = Duration::from_secs(10);
    let e = connect_or_start(&run, o).await.unwrap_err();
    assert_eq!(e.code, E_DAEMON_START);
    assert!(
        e.message
            .starts_with("the daemon exited with code 2 before it was ready"),
        "{e}"
    );
    assert_eq!(
        e.extra["log_tail"],
        json!(["already running: the store at x"])
    );
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}

/// Holds the project's store lock (`.rdy/store/lock`) as a daemon does, until dropped.
fn hold_store_lock(project: &std::path::Path) -> std::fs::File {
    let store = project.join(".rdy").join("store");
    std::fs::create_dir_all(&store).unwrap();
    let f = std::fs::File::create(store.join("lock")).unwrap();
    f.try_lock().unwrap();
    f
}

/// df3 fix round 1 (ruling 2): a spawned daemon that exits because a live daemon, which answers
/// on no socket we know, holds the store says so, with the socket the client tried: the recorded
/// one when there is a record, else its own.
#[tokio::test]
async fn df3_a_store_locked_by_a_daemon_that_does_not_answer_is_named() {
    for recorded in [false, true] {
        let dir = short_tempdir();
        let run = RunDir::of_project(dir.path());
        run.ensure().unwrap();
        let _held = hold_store_lock(dir.path());
        let pid = std::process::id();
        rdy_store::fsutil::write_private_durable(
            &run.pid_file(),
            &rdy_daemon::sys::own_pid_record().unwrap(),
        )
        .unwrap();
        let tried = if recorded {
            let dead = dead_endpoint(dir.path());
            write_record(&run, &dead, &run.project_id().unwrap());
            assert_eq!(
                run.recorded_socket(),
                Some(dead.clone()),
                "the fixture's record is a valid one"
            );
            dead
        } else {
            run.socket()
        };
        let mut o = opts("1", dir.path(), |_: &RunDir| {
            Ok(Spawned {
                exited: Box::new(|| Some(2)),
            })
        });
        o.start_timeout = Duration::from_secs(10);
        let e = connect_or_start(&run, o).await.unwrap_err();
        assert_eq!(e.code, E_DAEMON_START, "{e}");
        assert_eq!(
            e.message,
            format!(
                "the store is locked by daemon {pid}, which does not answer on {}",
                tried.display()
            ),
            "recorded: {recorded}"
        );
        assert_eq!(
            e.fix,
            format!("stop it with rdy serve --stop from the project, or end process {pid}")
        );
    }
}

/// df3 fix round 2 (ruling task-3-round1): a client follows `daemon.sock` only when the record
/// names this project's `.rdy` (`dev:ino`). A copy of a project carries the original's record,
/// which names the original's `.rdy`: the copy never follows it. A record that names this
/// project but leads to another project's daemon is refused by that daemon at the hello, and
/// the client takes the refusal as it is: no retry, no spawn.
#[tokio::test]
async fn df3_a_recorded_socket_is_followed_only_when_it_names_this_project() {
    let log = rdy_daemon::daemon::capture_log_for_tests(&["refused a client of another project"]);
    let d = daemon("1", true, |_| {}).await;
    let copy = short_tempdir();
    let run = RunDir::of_project(copy.path());
    run.ensure().unwrap();
    let hello = hello_for(copy.path());
    // what cp -R carries: the original's socket and the original's identity
    write_record(&run, &d.run.socket(), &d.run.project_id().unwrap());
    assert_eq!(run.recorded_socket(), None, "a copied record is stale");
    assert!(
        matches!(connect(&run, &hello).await, Attempt::NoDaemon(_)),
        "the copy reached the original's daemon"
    );
    // a record that names this project is followed; the daemon there serves another project
    // and refuses the hello
    write_record(&run, &d.run.socket(), &run.project_id().unwrap());
    assert_eq!(run.recorded_socket(), Some(d.run.socket()));
    let root = std::fs::canonicalize(d.dir.path()).unwrap();
    let refused = |e: &ErrorReply| {
        assert_eq!(e.code, E_DAEMON_START, "{e}");
        assert_eq!(
            e.message,
            format!(
                "the daemon on {} serves another project ({}, process {})",
                d.run.socket().display(),
                root.display(),
                std::process::id()
            )
        );
        assert_eq!(
            e.fix,
            "this project's daemon is not running; run rdy serve here, or remove a stale .rdy/run/daemon.sock copied from another project"
        );
    };
    match connect(&run, &hello).await {
        Attempt::Refused(e) => refused(&e),
        other => panic!("not refused: {other:?}"),
    }
    let spawns = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&spawns);
    let e = connect_or_start(
        &run,
        opts("1", copy.path(), move |_: &RunDir| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(running())
        }),
    )
    .await
    .unwrap_err();
    refused(&e);
    assert_eq!(spawns.load(Ordering::SeqCst), 0, "a refusal is not retried");
    assert_eq!(d.handle.sessions().len(), 0);
    d.handle.stop();
    // the daemon logs each refusal, with the copy's identity
    let line = format!(
        "refused a client of another project ({})",
        run.project_id().unwrap()
    );
    assert_eq!(log.lines(), [line.clone(), line]);
}

/// wx21b: a copy of a project, a restored one and a deleted one never reach the daemon of the
/// original. Each is a project of its own by its `.rdy` identity, on a socket and on a pipe.
#[tokio::test]
async fn wx21b_copied_restored_and_deleted_projects_are_isolated() {
    let d = daemon("1", true, |_| {}).await;
    let original = d.run.project_id().unwrap();
    // copied: the copy carries the original's endpoint record, which names the original's `.rdy`
    let copy = short_tempdir();
    let copy_run = RunDir::of_project(copy.path());
    copy_run.ensure().unwrap();
    write_record(&copy_run, &d.run.socket(), &original);
    assert_eq!(copy_run.recorded_socket(), None, "a copied record is stale");
    assert!(matches!(
        connect(&copy_run, &hello_for(copy.path())).await,
        Attempt::NoDaemon(_)
    ));
    // restored: the project's `.rdy` is made again, so it has a new identity; the old record no
    // longer names it. The old one is renamed, not removed, so its identity cannot be reused.
    let before = copy_run.project_id().unwrap();
    std::fs::rename(copy.path().join(".rdy"), copy.path().join(".rdy-before")).unwrap();
    let restored = RunDir::of_project(copy.path());
    restored.ensure().unwrap();
    write_record(&restored, &d.run.socket(), &before);
    assert_eq!(restored.recorded_socket(), None);
    write_record(&restored, &d.run.socket(), &restored.project_id().unwrap());
    // its hello reaches the original's daemon, which refuses it and opens no session
    match connect(&restored, &hello_for(copy.path())).await {
        Attempt::Refused(e) => assert_eq!(e.code, E_DAEMON_START, "{e}"),
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(d.handle.sessions().len(), 0);
    // deleted: nothing is left to name, and nothing answers
    let gone = short_tempdir();
    let gone_path = gone.path().to_path_buf();
    gone.close().unwrap();
    assert!(RunDir::of_project(&gone_path).project_id().is_err());
    // the original is still served
    let (_r, first) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    assert!(first.contains("\"ready\""), "{first}");
    d.finish().await;
}
