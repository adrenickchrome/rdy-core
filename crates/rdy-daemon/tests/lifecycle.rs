#![cfg(unix)]
// Unix only: it uses Unix sockets. FW-XP1 Phase 4 ports it.
//! Idle shutdown, stop and drain (life1).
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[tokio::test]
async fn life1_a_detached_daemon_exits_after_the_idle_timeout() {
    let d = daemon("1", true, |c| {
        c.idle_timeout = Some(Duration::from_millis(600))
    })
    .await;
    let start = Instant::now();
    let reason = tokio::time::timeout(Duration::from_secs(3), d.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(reason, ExitReason::Idle), "{reason:?}");
    assert!(start.elapsed() >= Duration::from_millis(600));
    assert!(!d.run.socket().exists() && !d.run.pid_file().exists());
}

#[tokio::test]
async fn life1_a_connection_disarms_the_timer_and_its_end_rearms_it() {
    let d = daemon("1", true, |c| {
        c.idle_timeout = Some(Duration::from_millis(500))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let v = ctl(&mut r, 1, "ping", json!({})).await;
    assert_eq!(
        v["ok"]["connections"], 1,
        "alive past the timeout while connected"
    );
    let closed_at = Instant::now();
    drop(r);
    let reason = tokio::time::timeout(Duration::from_secs(3), d.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(reason, ExitReason::Idle));
    assert!(
        closed_at.elapsed() >= Duration::from_millis(450),
        "the timer restarts at the last close"
    );
}

#[tokio::test]
async fn life1_a_connection_still_in_its_hello_holds_off_the_idle_timer() {
    let d = daemon("1", true, |c| {
        c.idle_timeout = Some(Duration::from_millis(400))
    })
    .await;
    let _s = UnixStream::connect(d.run.socket()).await.unwrap(); // no hello sent
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !d.task.is_finished(),
        "a silent connection is still a connection"
    );
    assert_eq!(d.handle.connections(), 1);
    d.handle.stop();
}

#[tokio::test]
async fn life1_foreground_never_idles_out() {
    let d = daemon("1", true, |c| c.idle_timeout = None).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!d.task.is_finished());
    d.handle.stop();
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
}

#[tokio::test]
async fn life1_stop_drains_an_in_flight_request_then_closes() {
    let d = daemon("1", true, |_| {}).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    let line = json!({"id": 1, "op": "sleep", "args": {"ms": 700}}).to_string() + "\n";
    r.get_mut().write_all(line.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t0 = Instant::now();
    d.handle.stop();
    assert!(!d.run.socket().exists(), "the socket goes first");
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"]["slept"], true, "the in-flight request is answered");
    out.clear();
    assert_eq!(
        r.read_line(&mut out).await.unwrap(),
        0,
        "then the connection closes"
    );
    let reason = d.task.await.unwrap().unwrap();
    assert!(matches!(reason, ExitReason::Stopped));
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert_eq!(d.echo.ended.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn life1_the_stop_op_drains_like_the_handle() {
    let d = daemon("1", true, |_| {}).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    let v = ctl(&mut r, 1, "stop", json!({})).await;
    assert_eq!(v["ok"]["stopping"], true);
    let mut out = String::new();
    assert_eq!(r.read_line(&mut out).await.unwrap(), 0);
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
}

#[test]
fn life1_a_stuck_connection_is_dropped_after_the_grace() {
    let log = rdy_daemon::daemon::capture_log_for_tests(&["still open after"]);
    // its own runtime, shut down in the background: the 60 s sleep runs on a blocking thread,
    // and a #[tokio::test] runtime's drop would wait for it
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let d = daemon("1", true, |_| {}).await;
        let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
        // 60 s: far past the 5 s grace; the stop must not wait for it
        let line = json!({"id": 1, "op": "sleep", "args": {"ms": 60_000}}).to_string() + "\n";
        r.get_mut().write_all(line.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t0 = Instant::now();
        d.handle.stop();
        // hang guard, not a speed: the task ends long before the 60 s sleep would
        let reason = tokio::time::timeout(Duration::from_secs(30), d.task)
            .await
            .expect("the stop abandoned the stuck connection")
            .unwrap()
            .unwrap();
        assert!(matches!(reason, ExitReason::Stopped));
        assert!(t0.elapsed() >= Duration::from_secs(5), "{:?}", t0.elapsed());
    });
    rt.shutdown_background();
    assert_eq!(
        log.lines(),
        ["1 connections still open after 5 s; abandoning them"]
    );
}

#[tokio::test]
async fn life1_connections_after_stop_are_refused() {
    let d = daemon("1", true, |_| {}).await;
    d.handle.stop();
    d.task.await.unwrap().unwrap();
    assert!(UnixStream::connect(d.run.socket()).await.is_err());
}

#[tokio::test]
async fn life1_a_stop_closes_a_connection_still_in_its_hello() {
    use tokio::io::AsyncReadExt;
    let d = daemon("1", true, |_| {}).await;
    let mut s = UnixStream::connect(d.run.socket()).await.unwrap(); // no hello sent
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(d.handle.connections(), 1);
    let t0 = Instant::now();
    d.handle.stop();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    assert!(
        out.is_empty(),
        "closed without a reply: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn life1_a_handle_stop_leaves_a_successors_run_files_alone() {
    let d = daemon("1", true, |_| {}).await;
    d.handle.stop();
    assert!(!d.run.socket().exists() && !d.run.pid_file().exists());
    // a successor binds the same paths before the old `run` gets to its cleanup
    let successor = std::os::unix::net::UnixListener::bind(d.run.socket()).unwrap();
    std::fs::write(d.run.pid_file(), "424242\n").unwrap();
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
    assert!(d.run.socket().exists(), "the successor's socket is kept");
    assert_eq!(
        std::fs::read_to_string(d.run.pid_file()).unwrap(),
        "424242\n"
    );
    drop(successor);
}

#[tokio::test]
async fn life1_the_stop_op_removes_the_run_files() {
    let d = daemon("1", true, |_| {}).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    assert_eq!(
        ctl(&mut r, 1, "stop", json!({})).await["ok"]["stopping"],
        true
    );
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
    assert!(!d.run.socket().exists() && !d.run.pid_file().exists());
}

/// A client that hears `exiting` spawns a successor at once; the old daemon must have removed
/// its socket and pid file before that reply, or it removes the successor's afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn life1_a_version_replaced_daemon_removes_its_run_files_before_it_answers_exiting() {
    let d = daemon("1", true, |_| {}).await;
    // a blocking client on a thread of its own looks at the run dir the moment the reply lands,
    // not when the runtime next gets round to it
    let (run, line) = (d.run.clone(), hello("2", "ctl", d.dir.path()));
    let seen = std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        let mut s = std::os::unix::net::UnixStream::connect(run.socket()).unwrap();
        s.write_all(line.as_bytes()).unwrap();
        let mut first = String::new();
        std::io::BufReader::new(&s).read_line(&mut first).unwrap();
        let present = (run.socket().exists(), run.pid_file().exists());
        (first, present)
    })
    .join()
    .unwrap();
    let (first, present) = seen;
    assert!(first.contains("\"exiting\""), "{first}");
    assert_eq!(
        present,
        (false, false),
        "the run files are gone by the time the client reads exiting"
    );
    // the client's successor binds the same paths at once
    let successor = std::os::unix::net::UnixListener::bind(d.run.socket()).unwrap();
    std::fs::write(d.run.pid_file(), "424242\n").unwrap();
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::VersionReplaced
    ));
    assert!(d.run.socket().exists(), "the successor's socket is kept");
    assert_eq!(
        std::fs::read_to_string(d.run.pid_file()).unwrap(),
        "424242\n"
    );
    drop(successor);
}

/// Runs this test binary's `test` alone in a child with `env` set, and returns its stdout and
/// stderr. With `break_stderr`, the read end of the child's stderr is closed at once, so its
/// writes there fail (EPIPE; Rust ignores SIGPIPE).
fn child_output(test: &str, env: &str, break_stderr: bool) -> (String, String) {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture"])
        .env(env, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if break_stderr {
        drop(child.stderr.take());
    }
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn child_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// The daemon logs to stderr, which is `daemon.log`; a full disk or a closed stderr must not
/// take it down (`eprintln!` panics when the write fails).
#[test]
fn life1_a_daemon_whose_stderr_is_broken_still_serves_and_stops() {
    const CHILD: &str = "LIFE1_BROKEN_STDERR_CHILD";
    if std::env::var_os(CHILD).is_some() {
        use std::io::Write;
        // wait until the parent has closed the read end
        let t0 = Instant::now();
        while writeln!(std::io::stderr(), "probe").is_ok() {
            assert!(t0.elapsed() < Duration::from_secs(10), "stderr never broke");
            std::thread::sleep(Duration::from_millis(10));
        }
        // stderr is gone, so panics are reported on stdout
        std::panic::set_hook(Box::new(|info| println!("child panic: {info}")));
        let got = child_runtime().block_on(async {
            let d = daemon("1", true, |_| {}).await;
            let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
            let sessions = ctl(&mut r, 1, "ping", json!({})).await["ok"]["sessions"].clone();
            d.handle.stop();
            format!("sessions={sessions} exit={:?}", d.task.await)
        });
        println!("child: {got}");
        return;
    }
    let (out, err) = child_output(
        "life1_a_daemon_whose_stderr_is_broken_still_serves_and_stops",
        CHILD,
        true,
    );
    assert!(
        out.contains("child: sessions=1 exit=Ok(Ok(Stopped))"),
        "stdout: {out}\nstderr: {err}"
    );
}

/// A client that hangs up before its reply (a Ctrl-C'd CLI) is a normal end of its session, not
/// an error worth a log line.
#[test]
fn life1_a_client_that_hangs_up_before_its_reply_is_not_logged_as_an_error() {
    const CHILD: &str = "LIFE1_HANG_UP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let got = child_runtime().block_on(async {
            let d = daemon("1", true, |_| {}).await;
            let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
            let line = json!({"id": 1, "op": "sleep", "args": {"ms": 300}}).to_string() + "\n";
            r.get_mut().write_all(line.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(r);
            // the reply's write fails and the connection ends
            let t0 = Instant::now();
            while d.handle.connections() > 0 {
                assert!(
                    t0.elapsed() < Duration::from_secs(5),
                    "the connection never ended"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            d.handle.stop();
            format!("exit={:?}", d.task.await)
        });
        println!("child: {got}");
        return;
    }
    let (out, err) = child_output(
        "life1_a_client_that_hangs_up_before_its_reply_is_not_logged_as_an_error",
        CHILD,
        false,
    );
    assert!(
        out.contains("child: exit=Ok(Ok(Stopped))"),
        "stdout: {out}\nstderr: {err}"
    );
    assert!(
        err.contains("rdy serve: listening on"),
        "the log is on stderr: {err}"
    );
    assert!(!err.contains("ended with an error"), "{err}");
}
