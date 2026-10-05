//! The session table (sess1).
use rdy_daemon::*;
use std::path::PathBuf;

#[test]
fn sess1_ids_start_at_one_and_are_never_reused() {
    let s = Sessions::default();
    let a = s.insert(Proto::Ctl, "v".into(), PathBuf::from("/a"), 501);
    let b = s.insert(Proto::Mcp, "v".into(), PathBuf::from("/b"), 501);
    assert_eq!((a.id, b.id), (SessionId(1), SessionId(2)));
    assert_eq!(s.len(), 2);
    assert_eq!(s.remove(a.id).unwrap().cwd, PathBuf::from("/a"));
    assert!(s.remove(a.id).is_none(), "removing twice is None");
    let c = s.insert(Proto::Ctl, "v".into(), PathBuf::from("/c"), 501);
    assert_eq!(c.id, SessionId(3));
    assert_eq!(
        s.list().iter().map(|x| x.id.0).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(s.get(SessionId(2)).unwrap().proto, Proto::Mcp);
    assert!(c.connected_at_ms > 1_700_000_000_000);
}

#[test]
fn sess1_session_info_serializes_for_the_sessions_op() {
    let s = Sessions::default();
    let a = s.insert(Proto::Mcp, "0.1.0".into(), PathBuf::from("/p"), 7);
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["proto"], "mcp");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["cwd"], "/p");
    assert_eq!(v["peer_uid"], 7);
    assert_eq!(SessionId(42).to_string(), "42");
}

#[test]
fn sess1_run_dir_names_are_fixed() {
    let r = RunDir::of_project(std::path::Path::new("/proj"));
    assert_eq!(r.path(), std::path::Path::new("/proj/.rdy/run"));
    assert_eq!(r.socket(), PathBuf::from("/proj/.rdy/run/rdy.sock"));
    assert_eq!(r.start_lock(), PathBuf::from("/proj/.rdy/run/start.lock"));
    assert_eq!(r.pid_file(), PathBuf::from("/proj/.rdy/run/daemon.pid"));
    assert_eq!(r.log_file(), PathBuf::from("/proj/.rdy/run/daemon.log"));
}
