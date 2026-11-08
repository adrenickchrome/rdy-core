#![cfg(unix)]
// Unix only: these tests bind a Unix socket and read its peer's uid; a named pipe has neither.
//! The client's connect-or-start protocol on a Unix socket (start1). They moved out of
//! `client.rs` unchanged.
mod common;
use common::*;
use rdy_daemon::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// What a plain listener heard: the bytes of the first connection it had queued, or `None` when
/// no client ever connected.
#[cfg(unix)]
fn heard(l: &std::os::unix::net::UnixListener) -> Option<Vec<u8>> {
    use std::io::Read;
    l.set_nonblocking(true).unwrap();
    match l.accept() {
        Ok((mut s, _)) => {
            s.set_nonblocking(false).unwrap();
            // best effort: macOS refuses a timeout on a socket whose peer has gone (EINVAL)
            let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
            let mut b = vec![];
            s.read_to_end(&mut b).unwrap();
            Some(b)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(e) => panic!("accept: {e}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn start1_a_socket_whose_peer_is_another_uid_is_no_daemon_and_hears_no_hello() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let l = std::os::unix::net::UnixListener::bind(run.socket()).unwrap();
    let me = socket::own_uid();
    let t0 = std::time::Instant::now();
    match client::connect_expecting_account(&run, &hello_for(dir.path()), me + 1).await {
        Attempt::NoDaemon(e) => {
            assert_eq!(
                e.to_string(),
                format!("the socket's peer is uid {me}, not ours")
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "refused at once, not after the hello timeout"
    );
    assert_eq!(heard(&l), Some(vec![]), "the foreign peer heard nothing");
}

#[cfg(unix)]
#[tokio::test]
async fn start1_a_symlinked_socket_is_not_followed() {
    let dir = short_tempdir();
    let other = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let evil = other.path().join("evil.sock");
    let l = std::os::unix::net::UnixListener::bind(&evil).unwrap();
    std::os::unix::fs::symlink(&evil, run.socket()).unwrap();
    match connect(&run, &hello_for(dir.path())).await {
        Attempt::NoDaemon(e) => assert!(e.to_string().contains(LINK_REFUSAL), "{e}"),
        other => panic!("{other:?}"),
    }
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
    use std::os::unix::fs::FileTypeExt;
    assert!(
        std::fs::symlink_metadata(run.socket())
            .unwrap()
            .file_type()
            .is_socket(),
        "the daemon's bind replaced the link"
    );
    assert_eq!(heard(&l), None, "the linked-to socket was never connected");
    c.ctl("stop", json!({})).await.unwrap();
}

/// df3 (F12, D-YMWEM): a project whose `.rdy/run/rdy.sock` is over `sun_path` gets a short
/// socket, `/tmp/rdy-<uid>/<hash>.sock`, and its daemon starts there; every other run file stays
/// in `.rdy/run/`.
#[cfg(unix)]
#[tokio::test]
async fn start1_a_long_project_path_gets_a_short_socket() {
    let dir = short_tempdir();
    let deep = dir.path().join("a".repeat(120));
    std::fs::create_dir_all(&deep).unwrap();
    let run = RunDir::of_project(&deep);
    let sock = run.socket();
    let short = run.short_socket().expect("the socket leaves .rdy/run");
    assert_eq!(sock, short);
    assert!(
        sock.starts_with(format!("/tmp/rdy-{}", socket::own_uid()))
            && sock.as_os_str().len() <= MAX_SOCKET_PATH,
        "{}",
        sock.display()
    );
    let n = Arc::new(AtomicUsize::new(0));
    let mut c = connect_or_start(
        &run,
        opts("1", &deep, in_process_spawn("1", n.clone(), Duration::ZERO)),
    )
    .await
    .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(c.ctl("ping", json!({})).await.unwrap()["sessions"], 1);
    assert!(sock.exists(), "the daemon listens on the short socket");
    assert!(!run.path().join(SOCKET_FILE).exists());
    assert!(run.pid_file().exists() && run.start_lock().exists());
    c.ctl("stop", json!({})).await.unwrap();
}
