//! The harness's Unix-only helpers, in one place (FW-XP1 D-1RXEA).
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(not(unix))]
compile_error!("rdy_qa::platform has no implementation for this platform yet (FW-XP1 Phase 4)");

/// A directory short enough for a socket path under it: `/tmp` (macOS `sun_path` is 104 bytes
/// and its `temp_dir()` is already about 50).
pub fn short_tmp_root() -> &'static Path {
    Path::new("/tmp")
}

/// Whether process `pid` has exited: `ps` lists no such process, or lists a zombie (killed, not
/// yet reaped by its parent).
pub fn process_exited(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => {
            let stat = String::from_utf8_lossy(&o.stdout);
            let stat = stat.trim();
            !o.status.success() || stat.is_empty() || stat.starts_with('Z')
        }
        Err(e) => panic!("cannot run ps: {e}"),
    }
}

/// Whether process `pid` is alive (`kill -0` succeeds).
pub fn process_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// SIGKILLs process `pid` (`kill -9`); panics if the command cannot run or fails.
pub fn kill_hard(pid: u32) {
    let status = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -9 {pid} failed");
}

/// Puts the child `cmd` starts in its own process group.
pub fn own_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

/// Opens `path` for append, creating it with mode 0o600.
pub fn open_private_log(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

/// Makes `link` a symbolic link to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// A command that does nothing and succeeds (`true`).
pub fn no_op_command() -> Command {
    Command::new("true")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    #[test]
    fn ps9_short_tmp_root_is_tmp() {
        assert_eq!(short_tmp_root(), Path::new("/tmp"));
    }

    #[test]
    fn ps9_a_child_runs_then_exits_after_kill_hard() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        own_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id();
        assert!(process_running(pid), "the child is running");
        assert!(!process_exited(pid), "the child has not exited");
        kill_hard(pid);
        child.wait().expect("reap the child");
        let until = Instant::now() + Duration::from_secs(10);
        while !process_exited(pid) {
            assert!(Instant::now() < until, "the child did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(process_exited(pid));
    }

    #[test]
    fn ps9_a_private_log_is_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log");
        let f = open_private_log(&path).expect("open the log");
        let mode = f.metadata().expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        open_private_log(&path).expect("open the log again (append)");
    }

    #[test]
    fn ps9_symlink_makes_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"x").expect("write");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("symlink");
        assert_eq!(std::fs::read_link(&link).expect("read_link"), target);
        assert!(no_op_command().status().expect("run true").success());
    }
}
