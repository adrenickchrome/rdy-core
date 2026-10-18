//! The harness's Unix-only helpers, in one place (FW-XP1 D-1RXEA).
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Sets the permission bits of `path` to `mode` (for example `0o555` for a read-only directory).
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// A command that does nothing and succeeds (`true`).
pub fn no_op_command() -> Command {
    Command::new("true")
}

/// A fake SSH agent that records connections: a Unix socket listener (set its path as
/// `SSH_AUTH_SOCK` for a child) that accepts each connection, counts it and closes it at once,
/// so a client that waits for a reply gets end of file instead of hanging.
pub struct RecordingAgent {
    _dir: tempfile::TempDir,
    path: PathBuf,
    listener: Arc<UnixListener>,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordingAgent {
    /// Binds a new socket in a fresh directory under [`short_tmp_root`] and starts accepting.
    pub fn start() -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("rdy-agent-")
            .tempdir_in(short_tmp_root())?;
        let path = dir.path().join("agent.sock");
        let listener = Arc::new(UnixListener::bind(&path)?);
        listener.set_nonblocking(true)?;
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (listener, count, stop) = (listener.clone(), count.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if !accept_pending(&listener, &count) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            })
        };
        Ok(Self {
            _dir: dir,
            path,
            listener,
            count,
            stop,
            thread: Some(thread),
        })
    }

    /// The socket's path, for `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The number of connections so far, including any still waiting to be accepted.
    pub fn connections(&self) -> usize {
        accept_pending(&self.listener, &self.count);
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for RecordingAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Accepts, counts and closes every waiting connection; whether there was one.
fn accept_pending(listener: &UnixListener, count: &AtomicUsize) -> bool {
    let mut any = false;
    while let Ok((stream, _)) = listener.accept() {
        count.fetch_add(1, Ordering::SeqCst);
        drop(stream);
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
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
        let status = child.wait().expect("reap the child");
        assert_eq!(
            status.signal(),
            Some(9),
            "the child was killed by SIGKILL, not left to finish ({status:?})"
        );
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

    #[test]
    fn sig2_a_recording_agent_counts_each_connection() {
        let agent = RecordingAgent::start().expect("start the agent");
        assert_eq!(agent.connections(), 0, "no connection yet");
        drop(std::os::unix::net::UnixStream::connect(agent.socket_path()).expect("connect"));
        let mut held =
            std::os::unix::net::UnixStream::connect(agent.socket_path()).expect("connect");
        // a client that waits for a reply gets end of file, not a hang
        let mut buf = [0u8; 1];
        use std::io::Read;
        assert_eq!(held.read(&mut buf).expect("read"), 0);
        assert_eq!(agent.connections(), 2);
    }
}
