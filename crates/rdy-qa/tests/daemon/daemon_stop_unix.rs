#![cfg(unix)]
// Unix only: `terminate` sends SIGTERM by pid. No process is ended by its pid alone on Windows, and windows_stop.rs in rdy-daemon holds its counterpart.
//! dl11: what the signal of a stop may reach.
use rdy_daemon::terminate;

#[test]
fn dl11_terminate_never_signals_init_or_a_group() {
    for pid in [0, 1] {
        let e = terminate(pid).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "pid {pid}");
    }
}

#[test]
fn dl11_terminate_says_when_the_process_is_gone() {
    let mut child = crate::daemon_children::child_fixture::fixture_command("hang")
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(terminate(pid).unwrap(), "the signal was sent");
    child.wait().unwrap();
    assert!(!terminate(pid).unwrap(), "the process is gone");
}
