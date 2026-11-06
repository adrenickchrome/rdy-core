#![cfg(unix)]
// Unix only: it sends signals to a foreground serve. daemon_windows.rs holds the counterpart, the stop event.
//! cli2: signals stop a foreground serve.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use std::time::{Duration, Instant};

/// Final review fix round 1 (minor 12): SIGINT (Ctrl-C) and SIGTERM stop a foreground
/// `rdy serve` the way `rdy serve --stop` does: the run files go, the store closes cleanly and
/// the process exits 0.
#[test]
fn cli2_sigint_and_sigterm_stop_a_foreground_serve_cleanly() {
    for sig in ["-INT", "-TERM"] {
        let s = Served::new();
        let mut child = s.spawn(&["serve"]);
        let t = Instant::now();
        while s.daemon_pid() != Some(child.id()) {
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "{sig}: the foreground daemon did not write its pid"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = std::process::Command::new("kill")
            .args([sig, &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let t = Instant::now();
        let exit = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if t.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                panic!("{sig}: rdy serve did not exit within 30 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let out = child.wait_with_output().unwrap();
        assert!(
            exit.success(),
            "{sig}: {exit:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!s.run_dir().socket().exists(), "{sig}");
        assert!(!s.run_dir().pid_file().exists(), "{sig}");
        let r = Rdy::open(s.dir.path()).unwrap();
        assert!(!r.open_report().unclean_shutdown, "{sig}");
        drop(r);
    }
}
