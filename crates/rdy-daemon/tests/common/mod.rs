//! The shared daemon fixture. It reaches the endpoint only through the `sys` seam, so one harness
//! serves a Unix socket and a Windows named pipe.
#![allow(dead_code)]
use rdy_daemon::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The stream of the daemon's endpoint: a Unix socket or a named pipe.
pub type Conn = rdy_daemon::sys::Stream;

/// A new project directory. On Unix it is under /tmp, so `.rdy/run/rdy.sock` fits `sun_path`.
#[cfg(unix)]
pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rdyd")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp")
}

/// A new project directory. The endpoint is a named pipe, so the path length does not matter.
#[cfg(windows)]
pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rdyd")
        .tempdir()
        .expect("tempdir")
}

/// Echoes `args`; records ended sessions.
#[derive(Default)]
pub struct Echo {
    pub ended: Mutex<Vec<SessionId>>,
}
impl Handler for Echo {
    fn ctl(
        &self,
        s: &SessionInfo,
        r: CtlRequest,
        _agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        match r.op.as_str() {
            "echo" => Ok(json!({"session": s.id, "cwd": s.cwd, "args": r.args})),
            "sleep" => {
                std::thread::sleep(std::time::Duration::from_millis(
                    r.args["ms"].as_u64().unwrap_or(0),
                ));
                Ok(json!({"slept": true}))
            }
            other => Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("unknown op {other:?}"),
                "use echo",
            )),
        }
    }
    fn session_ended(&self, s: &SessionInfo) {
        self.ended.lock().unwrap().push(s.id);
    }
}

pub struct TestDaemon {
    pub dir: tempfile::TempDir,
    pub run: RunDir,
    pub handle: DaemonHandle,
    pub readiness: Readiness,
    pub echo: Arc<Echo>,
    pub task: tokio::task::JoinHandle<std::io::Result<ExitReason>>,
}

impl TestDaemon {
    /// Stops the daemon, waits for it to end and removes its directory. The removal fails while
    /// any handle in the directory is open, which is the check that nothing leaked.
    pub async fn finish(self) -> ExitReason {
        self.handle.stop();
        let reason = self.task.await.expect("daemon task").expect("daemon run");
        self.dir.close().expect("the fixture directory closes");
        reason
    }
}

/// A bound daemon of `version` with the echo handler, not yet ready unless `ready`.
pub async fn daemon(version: &str, ready: bool, cfg: impl FnOnce(&mut DaemonConfig)) -> TestDaemon {
    let dir = short_tempdir();
    let run = RunDir::of_project(dir.path());
    let mut c = DaemonConfig::new(run.clone(), version.to_string());
    cfg(&mut c);
    let echo = Arc::new(Echo::default());
    let mut d = Daemon::new(c, echo.clone());
    d.bind().await.expect("bind");
    let (handle, readiness) = (d.handle(), d.readiness());
    if ready {
        readiness.ready();
    }
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

/// A hello line from a client in the project at `cwd`, which names that project's `.rdy`.
pub fn hello(version: &str, proto: &str, cwd: &Path) -> String {
    let project = RunDir::of_project(cwd)
        .project_id()
        .expect("the project's .rdy");
    json!({"rdy": 1, "version": version, "build": BUILD, "proto": proto, "cwd": cwd, "project": project})
        .to_string()
        + "\n"
}

/// A `ctl` hello line of `version` and `build` from a client in the project at `cwd`.
pub fn hello_built(version: &str, build: &str, cwd: &Path) -> String {
    let project = RunDir::of_project(cwd)
        .project_id()
        .expect("the project's .rdy");
    json!({"rdy": 1, "version": version, "build": build, "proto": "ctl", "cwd": cwd, "project": project})
        .to_string()
        + "\n"
}

/// The endpoint's name as a record holds it: a Unix path's bytes, or a pipe name (always UTF-8).
#[cfg(unix)]
fn endpoint_bytes(endpoint: &Path) -> Vec<u8> {
    rdy_daemon::sys::path_bytes(endpoint).to_vec()
}

#[cfg(windows)]
fn endpoint_bytes(endpoint: &Path) -> Vec<u8> {
    endpoint
        .to_str()
        .expect("a pipe name is UTF-8")
        .as_bytes()
        .to_vec()
}

/// Writes `run`'s socket record as a daemon does: the socket, then the `.rdy` identity.
pub fn write_record(run: &RunDir, socket: &Path, project: &ProjectId) {
    let mut bytes = endpoint_bytes(socket);
    bytes.push(b'\n');
    bytes.extend_from_slice(format!("{project}\n").as_bytes());
    std::fs::write(run.socket_record(), bytes).unwrap();
}

/// Connects, sends `line` as the hello, and returns the reader and the first reply line.
pub async fn connect_with(run: &RunDir, line: &str) -> (BufReader<Conn>, String) {
    let mut s = rdy_daemon::sys::connect(&run.socket())
        .await
        .expect("connect");
    s.write_all(line.as_bytes()).await.unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    (r, out)
}

pub async fn ctl(r: &mut BufReader<Conn>, id: u64, op: &str, args: Value) -> Value {
    let line = json!({"id": id, "op": op, "args": args}).to_string() + "\n";
    r.get_mut().write_all(line.as_bytes()).await.unwrap();
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out:?}"))
}
