//! The session table (sess1).
use rdy_daemon::*;
use std::path::PathBuf;

/// The account a session's peer has here.
#[cfg(not(windows))]
fn peer() -> sys::Account {
    7
}

/// The account a session's peer has here: a real account's form of SID.
#[cfg(windows)]
fn peer() -> sys::Account {
    let mut bytes = vec![1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0];
    for sub in [1_u32, 2, 3, 1002] {
        bytes.extend(sub.to_le_bytes());
    }
    rdy_store::pio::Sid::parse(&bytes).unwrap()
}

#[test]
fn sess1_ids_start_at_one_and_are_never_reused() {
    let s = Sessions::default();
    let a = s.insert(
        Proto::Ctl,
        "v".into(),
        BUILD.into(),
        PathBuf::from("/a"),
        peer(),
    );
    let b = s.insert(
        Proto::Mcp,
        "v".into(),
        BUILD.into(),
        PathBuf::from("/b"),
        peer(),
    );
    assert_eq!((a.id, b.id), (SessionId(1), SessionId(2)));
    assert_eq!(s.len(), 2);
    assert_eq!(s.remove(a.id).unwrap().cwd, PathBuf::from("/a"));
    assert!(s.remove(a.id).is_none(), "removing twice is None");
    let c = s.insert(
        Proto::Ctl,
        "v".into(),
        BUILD.into(),
        PathBuf::from("/c"),
        peer(),
    );
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
    let a = s.insert(
        Proto::Mcp,
        "0.1.0".into(),
        BUILD.into(),
        PathBuf::from("/p"),
        peer(),
    );
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["proto"], "mcp");
    assert_eq!(v["version"], "0.1.0");
    assert_eq!(v["build"], BUILD);
    assert_eq!(v["cwd"], "/p");
    // each platform shows only its own field (D-FARN9)
    #[cfg(not(windows))]
    {
        assert_eq!(v["peer_uid"], 7);
        assert!(v.get("peer_sid").is_none());
    }
    #[cfg(windows)]
    {
        assert_eq!(v["peer_sid"], "S-1-5-21-1-2-3-1002");
        assert!(v.get("peer_uid").is_none());
    }
    assert_eq!(SessionId(42).to_string(), "42");
}

#[test]
fn sess1_run_dir_names_are_fixed() {
    // a root that does not exist, so its canonical form is the path as written
    #[cfg(not(windows))]
    let (root, run) = ("/proj", "/proj/.rdy/run");
    #[cfg(windows)]
    let (root, run) = (
        r"C:\rdy-no-such-project",
        r"C:\rdy-no-such-project\.rdy\run",
    );
    let (root, run) = (std::path::Path::new(root), PathBuf::from(run));
    let r = RunDir::of_project(root);
    assert_eq!(r.path(), run);
    assert_eq!(r.start_lock(), run.join("start.lock"));
    assert_eq!(r.pid_file(), run.join("daemon.pid"));
    assert_eq!(r.log_file(), run.join("daemon.log"));
    assert_eq!(r.socket_record(), run.join("daemon.sock"));
    #[cfg(not(windows))]
    assert_eq!(r.socket(), PathBuf::from("/proj/.rdy/run/rdy.sock"));
    // the endpoint is a pipe here, no file of the run dir: the hash of the root's UTF-16LE units
    #[cfg(windows)]
    {
        let units: Vec<u8> = root
            .to_str()
            .unwrap()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            r.socket(),
            PathBuf::from(format!(r"\\.\pipe\rdy-{}", blake3::hash(&units).to_hex()))
        );
    }
}
