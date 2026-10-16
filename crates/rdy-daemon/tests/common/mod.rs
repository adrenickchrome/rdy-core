#![cfg(unix)]
// Unix only: it uses Unix sockets and path bytes. FW-XP1 Phase 4 ports it.
#![allow(dead_code)]
use rdy_daemon::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rdyd")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp")
}

/// Echoes `args`; records ended sessions.
#[derive(Default)]
pub struct Echo {
    pub ended: Mutex<Vec<SessionId>>,
}
impl Handler for Echo {
    fn ctl(&self, s: &SessionInfo, r: CtlRequest) -> Result<Value, ErrorReply> {
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
    json!({"rdy": 1, "version": version, "proto": proto, "cwd": cwd, "project": project})
        .to_string()
        + "\n"
}

/// Writes `run`'s socket record as a daemon does: the socket, then the `.rdy` identity.
pub fn write_record(run: &RunDir, socket: &Path, project: &ProjectId) {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = socket.as_os_str().as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend_from_slice(format!("{project}\n").as_bytes());
    std::fs::write(run.socket_record(), bytes).unwrap();
}

/// Connects, sends `line` as the hello, and returns the reader and the first reply line.
pub async fn connect_with(run: &RunDir, line: &str) -> (BufReader<UnixStream>, String) {
    let mut s = UnixStream::connect(run.socket()).await.expect("connect");
    s.write_all(line.as_bytes()).await.unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    (r, out)
}

pub async fn ctl(r: &mut BufReader<UnixStream>, id: u64, op: &str, args: Value) -> Value {
    let line = json!({"id": id, "op": op, "args": args}).to_string() + "\n";
    r.get_mut().write_all(line.as_bytes()).await.unwrap();
    let mut out = String::new();
    r.read_line(&mut out).await.unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out:?}"))
}
