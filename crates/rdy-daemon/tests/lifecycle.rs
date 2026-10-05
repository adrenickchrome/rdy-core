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

#[tokio::test]
async fn life1_a_stuck_connection_is_dropped_after_the_grace() {
    let d = daemon("1", true, |_| {}).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    // 7 s: past the 5 s grace, but short, since the runtime's drop waits for the blocking thread
    let line = json!({"id": 1, "op": "sleep", "args": {"ms": 7_000}}).to_string() + "\n";
    r.get_mut().write_all(line.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t0 = Instant::now();
    d.handle.stop();
    let reason = tokio::time::timeout(Duration::from_secs(8), d.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(reason, ExitReason::Stopped));
    assert!(
        t0.elapsed() >= Duration::from_secs(5) && t0.elapsed() < Duration::from_secs(6),
        "{:?}",
        t0.elapsed()
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
