//! The run dir, the socket and the peer check (sock1).
mod common;
use common::*;
use rdy_daemon::*;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

fn mode(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn sock1_bind_makes_a_private_run_dir_socket_and_pid_file() {
    let d = daemon("1", true, |_| {}).await;
    assert_eq!(mode(d.run.path()), 0o700);
    assert_eq!(mode(&d.run.socket()), 0o600);
    assert_eq!(mode(&d.run.pid_file()), 0o600);
    let pid: u32 = std::fs::read_to_string(d.run.pid_file())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid, std::process::id());
    d.handle.stop();
    assert!(matches!(
        d.task.await.unwrap().unwrap(),
        ExitReason::Stopped
    ));
    assert!(!d.run.socket().exists(), "stop unlinks the socket");
    assert!(!d.run.pid_file().exists(), "stop removes the pid file");
}

#[tokio::test]
async fn sock1_an_existing_run_dir_is_tightened_and_a_file_is_refused() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    std::fs::create_dir_all(run.path()).unwrap();
    std::fs::set_permissions(run.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    run.ensure().unwrap();
    assert_eq!(mode(run.path()), 0o700);
    let dir2 = short_tempdir();
    let run2 = RunDir::of_project(dir2.path());
    std::fs::create_dir_all(run2.path().parent().unwrap()).unwrap();
    std::fs::write(run2.path(), b"not a dir").unwrap();
    let e = run2.ensure().unwrap_err();
    assert!(e.to_string().contains("is not a directory"), "{e}");
}

#[tokio::test]
async fn sock1_a_stale_socket_file_is_replaced_by_bind() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    drop(std::os::unix::net::UnixListener::bind(run.socket()).unwrap()); // dead listener, file stays
    assert!(run.socket().exists());
    assert!(
        std::os::unix::net::UnixStream::connect(run.socket()).is_err(),
        "stale: refused"
    );
    let d = daemon_at(dir).await;
    let (_r, first) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    assert!(first.contains("\"ready\""), "{first}");
    d.handle.stop();
}

/// A ready daemon bound in an existing dir (the stale-socket case).
async fn daemon_at(dir: tempfile::TempDir) -> TestDaemon {
    let run = RunDir::of_project(dir.path());
    let echo = std::sync::Arc::new(Echo::default());
    let mut d = Daemon::new(DaemonConfig::new(run.clone(), "1".into()), echo.clone());
    d.bind().await.unwrap();
    let (handle, readiness) = (d.handle(), d.readiness());
    readiness.ready();
    let task = tokio::spawn(d.run());
    TestDaemon {
        dir,
        run,
        handle,
        readiness,
        echo,
        task,
    }
}

#[tokio::test]
async fn sock1_a_socket_path_over_the_limit_is_refused_with_a_clear_error() {
    let dir = short_tempdir();
    let deep = dir.path().join("a".repeat(120));
    std::fs::create_dir_all(&deep).unwrap();
    let run = RunDir::of_project(&deep);
    let e = socket::bind(&run).await.unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    let msg = e.to_string();
    assert!(
        msg.contains("the limit is 103") && msg.contains("shorter path"),
        "{msg}"
    );
}

#[tokio::test]
async fn sock1_a_peer_of_another_uid_is_closed_without_a_byte() {
    let me = socket::own_uid();
    let d = daemon("1", true, |c| c.expected_uid = Some(me + 1)).await;
    let s = UnixStream::connect(d.run.socket()).await.unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    let n = tokio::time::timeout(std::time::Duration::from_secs(3), r.read_line(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((n, out.as_str()), (0, ""), "no reply, just EOF");
    assert_eq!(d.handle.sessions().len(), 0);
    d.handle.stop();
}

#[tokio::test]
async fn sock1_peer_uid_of_a_same_user_connection_is_our_uid() {
    let d = daemon("1", true, |_| {}).await;
    let (mut r, _) = connect_with(&d.run, &hello("1", "ctl", d.dir.path())).await;
    let v = ctl(&mut r, 1, "sessions", serde_json::json!({})).await;
    assert_eq!(v["ok"][0]["peer_uid"], socket::own_uid());
    d.handle.stop();
}

/// A checkout that plants `.rdy/run/daemon.pid.tmp` as a symlink must not get its target
/// truncated, overwritten with the pid and chmod'ed when the daemon binds.
#[tokio::test]
async fn sock1_a_symlinked_pid_tmp_file_is_refused_and_its_target_is_untouched() {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    run.ensure().unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep\n").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&victim, run.path().join(format!("{PID_FILE}.tmp"))).unwrap();
    let e = socket::bind(&run).await.unwrap_err();
    assert!(e.to_string().contains("symbolic link"), "{e}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep\n");
    assert_eq!(mode(&victim), 0o644);
    assert!(
        !run.pid_file().exists(),
        "no pid file for a daemon that did not bind"
    );
    assert!(
        !run.socket().exists(),
        "the socket of a failed bind is removed"
    );
}
