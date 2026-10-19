//! An MCP stdio client for tests. The server is untrusted: every request has a deadline, and a
//! missing or late reply is a Critical failure (the server is killed and the test panics).
use crate::daemon::Served;
use crate::fixtures::rdy_bin;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a request waits for its reply before the server counts as hung: a deadline, not a
/// speed claim (the perf tier measures speed), so it is generous for a loaded machine.
pub const DEADLINE: Duration = Duration::from_secs(30);
/// The `protocolVersion` the client sends in its `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone)]
pub enum Reply {
    Ok(Value),
    ToolError(Value),
    RpcError(Value),
}

impl Reply {
    /// True for a result only: a tool error and a protocol (JSON-RPC) error are both failures.
    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok(_))
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            Reply::ToolError(v) => v["code"].as_str(),
            _ => None,
        }
    }
    pub fn ok(self) -> Value {
        match self {
            Reply::Ok(v) => v,
            other => panic!("expected a result, got {other:?}"),
        }
    }
}

/// Everything a server wrote, from [`McpClient::finish`].
#[derive(Debug, Clone)]
pub struct Transcript {
    /// Its exit code (`None` when killed by a signal).
    pub code: Option<i32>,
    /// Every stdout line, in arrival order, each ended by `\n`.
    pub stdout: String,
    /// Its stderr (empty when it was not piped).
    pub stderr: String,
}

/// Each of `hidden` that one of `channels` (a name and its text) holds, as `{name}: {hidden}`,
/// in channel order; empty when every channel is clean.
pub fn exposed(hidden: &[&str], channels: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, text) in channels {
        for s in hidden {
            if text.contains(s) {
                out.push(format!("{name}: {s}"));
            }
        }
    }
    out
}

/// Each of `hidden` in what a server wrote (`t`: its raw stdout and its stderr) or in
/// `project`'s daemon log (`.rdy/run/daemon.log`, which a detached daemon's stdout and stderr
/// go to), as `{channel}: {hidden}`. Read it after the server and the daemon have exited and
/// before the project is removed. A missing log reads as empty.
pub fn exposed_outputs(hidden: &[&str], t: &Transcript, project: &Path) -> Vec<String> {
    let log = rdy_daemon::RunDir::of_project(project).log_file();
    let log = match std::fs::read(&log) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => panic!("{}: {e}", log.display()),
    };
    exposed(
        hidden,
        &[
            ("mcp stdout", &t.stdout),
            ("mcp stderr", &t.stderr),
            ("daemon.log", &log),
        ],
    )
}

pub struct McpClient {
    child: Child,
    writer: Sender<String>,
    written: Receiver<bool>,
    lines: Receiver<String>,
    /// The child's stderr, drained on a thread (a child started with a piped stderr only;
    /// empty otherwise).
    stderr: Arc<Mutex<String>>,
    stderr_drain: Option<std::thread::JoinHandle<()>>,
    /// Every line the child wrote to stdout, as it came: replies, notifications, and lines
    /// that are not JSON-RPC at all (which `recv` skips). `None` for a client that does not
    /// keep its output (a long run would grow it without bound).
    stdout: Option<Arc<Mutex<String>>>,
    stdout_drain: Option<std::thread::JoinHandle<()>>,
    pending: Vec<Value>,
    /// The notifications (lines with a `method` and no `id`) received, in arrival order (ib10).
    notifications: Vec<Value>,
    /// The server's reply to `initialize`.
    init: Value,
    next: u64,
    calls: usize,
    latencies: Vec<(String, Duration)>,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpClient {
    pub fn start(project: &Path) -> McpClient {
        McpClient::start_with_env(project, &[])
    }

    /// `rdy mcp --no-daemon --project <project>`: the in-process stdio server, no daemon.
    pub fn start_with_env(project: &Path, env: &[(&str, &str)]) -> McpClient {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--no-daemon", "--project"])
            .arg(project)
            .envs(env.iter().copied())
            .stderr(Stdio::null());
        McpClient::launch_with(cmd, false)
    }

    /// `start_with_env` with the child's stderr kept, for a check of everything the server
    /// writes ([`McpClient::finish`]).
    pub fn start_capturing(project: &Path, env: &[(&str, &str)]) -> McpClient {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--no-daemon", "--project"])
            .arg(project)
            .envs(env.iter().copied())
            .stderr(Stdio::piped());
        McpClient::launch(cmd)
    }

    /// `start_proxy_at` that keeps every line the proxy writes, for [`McpClient::finish`].
    pub fn start_proxy_capturing(dir: &Path, env: &[(String, String)]) -> McpClient {
        McpClient::launch(McpClient::proxy_command(dir, env))
    }

    /// `rdy mcp --project <s.dir>` as `s`'s human: the proxy to the project's daemon, which it
    /// starts if none runs (`Served` stops it at drop). Its stderr is kept for `wait_exit`.
    pub fn start_proxy(s: &Served) -> McpClient {
        McpClient::start_proxy_at(s.dir.path(), &s.env_pairs())
    }

    /// `start_proxy` for a caller that cannot hold the `Served` (a worker thread that outlives
    /// its borrow): the project's directory and the human's environment (`Served::env_pairs`).
    pub fn start_proxy_at(dir: &Path, env: &[(String, String)]) -> McpClient {
        McpClient::launch_with(McpClient::proxy_command(dir, env), false)
    }

    fn proxy_command(dir: &Path, env: &[(String, String)]) -> Command {
        let mut cmd = Command::new(rdy_bin());
        cmd.args(["mcp", "--project"])
            .arg(dir)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stderr(Stdio::piped());
        cmd
    }

    /// Starts `cmd` as the server and initializes it, keeping every line it writes for
    /// [`McpClient::finish`]. `cmd` sets its own stderr: piped to keep it, null to drop it. A
    /// test may launch a stub server this way.
    pub fn launch(cmd: Command) -> McpClient {
        McpClient::launch_with(cmd, true)
    }

    fn launch_with(mut cmd: Command, keep: bool) -> McpClient {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn rdy mcp");
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_drain = child.stderr.take().map(|mut err| {
            // drained so a full pipe never blocks the child
            let sink = stderr.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match err.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => sink
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_str(&String::from_utf8_lossy(&buf[..n])),
                    }
                }
            })
        });
        let out = child.stdout.take().expect("stdout");
        let (tx, rx) = channel();
        let stdout = keep.then(|| Arc::new(Mutex::new(String::new())));
        let raw = stdout.clone();
        let stdout_drain = std::thread::spawn(move || {
            let mut out = BufReader::new(out);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match out.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                // invalid UTF-8 becomes a line that fails to parse (skipped), not a dead reader
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                if let Some(raw) = &raw {
                    let mut all = raw.lock().unwrap_or_else(|e| e.into_inner());
                    all.push_str(&line);
                    all.push('\n');
                }
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        // stdin writes happen on their own thread so a server that stops reading cannot block us
        let mut stdin = child.stdin.take().expect("stdin");
        let (wtx, wrx) = channel::<String>();
        let (dtx, drx) = channel::<bool>();
        std::thread::spawn(move || {
            for line in wrx {
                let ok = writeln!(stdin, "{line}")
                    .and_then(|_| stdin.flush())
                    .is_ok();
                if dtx.send(ok).is_err() || !ok {
                    break;
                }
            }
        });
        let mut c = McpClient {
            child,
            writer: wtx,
            written: drx,
            lines: rx,
            stderr,
            stderr_drain,
            stdout,
            stdout_drain: Some(stdout_drain),
            pending: vec![],
            notifications: vec![],
            init: Value::Null,
            next: 0,
            calls: 0,
            latencies: vec![],
        };
        let id = c.next_id();
        c.send(&json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "rdy-qa", "version": "0"}}}));
        c.init = c.recv(id, DEADLINE);
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    /// Closes the child's stdin: the writer thread ends and drops the pipe.
    pub fn close_stdin(&mut self) {
        let (tx, _) = channel();
        self.writer = tx;
    }

    /// Waits at most `budget` for the child to exit: its exit code (`None` when killed by a
    /// signal) and the stderr captured so far. Past `budget` it is a Critical failure.
    pub fn wait_exit(&mut self, budget: Duration) -> (Option<i32>, String) {
        let end = Instant::now() + budget;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    // the drain thread ends at the pipe's EOF, which follows the exit; it is
                    // waited on briefly, in case a grandchild still holds the pipe
                    if let Some(h) = self.stderr_drain.take() {
                        let t = Instant::now();
                        while !h.is_finished() && t.elapsed() < Duration::from_secs(1) {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    let err = self
                        .stderr
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    return (status.code(), err);
                }
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => self.critical(&format!("the server did not exit within {budget:?}")),
                Err(e) => self.critical(&format!("waiting for the server failed: {e}")),
            }
        }
    }

    /// Closes the child's stdin, waits at most `budget` for it to exit, then for both of its
    /// pipes to reach their end, and returns everything it wrote. A pipe still open after
    /// `budget` (a grandchild holds it) is a Critical failure: its output cannot be checked, as
    /// is a client that does not keep its output.
    pub fn finish(mut self, budget: Duration) -> Transcript {
        let Some(stdout) = self.stdout.clone() else {
            self.critical(
                "this client does not keep its output; start it with a capturing constructor",
            );
        };
        self.close_stdin();
        let end = Instant::now() + budget;
        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => self.critical(&format!("the server did not exit within {budget:?}")),
                Err(e) => self.critical(&format!("waiting for the server failed: {e}")),
            }
        };
        for h in [self.stdout_drain.take(), self.stderr_drain.take()]
            .into_iter()
            .flatten()
        {
            while !h.is_finished() {
                if Instant::now() >= end {
                    self.critical(&format!("a pipe of the server stayed open past {budget:?}"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = h.join();
        }
        let take = |m: &Arc<Mutex<String>>| m.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Transcript {
            code,
            stdout: take(&stdout),
            stderr: take(&self.stderr),
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn send(&mut self, v: &Value) {
        self.send_raw(&v.to_string());
    }

    pub fn send_raw(&mut self, line: &str) {
        self.send_raw_within(line, DEADLINE);
    }

    /// Write one line to the server, waiting at most `budget` for it to be accepted.
    pub fn send_raw_within(&mut self, line: &str, budget: Duration) {
        if self.writer.send(line.to_string()).is_err() {
            self.critical("the server closed its input");
        }
        match self.written.recv_timeout(budget) {
            Ok(true) => {}
            Ok(false) => self.critical("the server closed its input"),
            Err(_) => self.critical(&format!(
                "the server did not accept a request within {budget:?}"
            )),
        }
    }

    fn critical(&mut self, what: &str) -> ! {
        let _ = self.child.kill();
        panic!("CRITICAL: {what}");
    }

    /// The raw JSON-RPC response with `id`, waiting at most `budget`.
    pub fn recv(&mut self, id: u64, budget: Duration) -> Value {
        if let Some(i) = self.pending.iter().position(|v| v["id"] == json!(id)) {
            return self.pending.remove(i);
        }
        let end = Instant::now() + budget;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(_) => self.critical(&format!("no reply to request {id} within {budget:?}")),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"] == json!(id) {
                return v;
            }
            if v.get("id").is_some() {
                self.pending.push(v);
            } else {
                self.keep_notification(v);
            }
        }
    }

    /// The next reply line in arrival order, whatever its id, waiting at most `budget`. Replies
    /// already set aside by [`McpClient::recv`] come first, in the order they arrived.
    pub fn recv_next(&mut self, budget: Duration) -> Value {
        if !self.pending.is_empty() {
            return self.pending.remove(0);
        }
        let end = Instant::now() + budget;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(_) => self.critical(&format!("no reply within {budget:?}")),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("id").is_some() {
                return v;
            }
            self.keep_notification(v);
        }
    }

    /// Keeps a line with a `method` and no `id`: a notification.
    fn keep_notification(&mut self, v: Value) {
        if v.get("method").is_some() {
            self.notifications.push(v);
        }
    }

    /// The notifications received so far, in arrival order (ib10).
    pub fn notifications(&self) -> &[Value] {
        &self.notifications
    }

    /// The server's reply to `initialize` (its `result` and `id`).
    pub fn initialize_result(&self) -> &Value {
        &self.init
    }

    /// Waits at most `budget` for a line, then takes every line already waiting: notifications go
    /// to [`McpClient::notifications`], replies are set aside for `recv`. Returns the lines taken.
    pub fn drain(&mut self, budget: Duration) -> usize {
        let mut taken = 0;
        let mut wait = budget;
        while let Ok(line) = self.lines.recv_timeout(wait) {
            wait = Duration::ZERO;
            taken += 1;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("id").is_some() {
                self.pending.push(v);
            } else {
                self.keep_notification(v);
            }
        }
        taken
    }

    /// The `resources` of `resources/list`.
    pub fn list_resources(&mut self) -> Vec<Value> {
        let v = self.request("resources/list", json!({}));
        v["result"]["resources"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The `resourceTemplates` of `resources/templates/list`.
    pub fn list_resource_templates(&mut self) -> Vec<Value> {
        let v = self.request("resources/templates/list", json!({}));
        v["result"]["resourceTemplates"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The raw JSON-RPC reply to `resources/read`: `result.contents` or `error`.
    pub fn read_resource(&mut self, uri: &str) -> Value {
        self.request("resources/read", json!({"uri": uri}))
    }

    /// The raw JSON-RPC reply to (legacy) `resources/subscribe`.
    pub fn subscribe(&mut self, uri: &str) -> Value {
        self.request("resources/subscribe", json!({"uri": uri}))
    }

    /// True once a `notifications/resources/updated` for `uri` has arrived, polling for at most
    /// `budget`. It also finds one that arrived earlier.
    pub fn wait_notification(&mut self, uri: &str, budget: Duration) -> bool {
        let end = Instant::now() + budget;
        loop {
            if self.notifications.iter().any(|n| {
                n["method"] == "notifications/resources/updated" && n["params"]["uri"] == uri
            }) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            self.drain(left.min(Duration::from_millis(50)));
        }
    }

    /// Sends one request and returns its raw JSON-RPC reply.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.recv(id, DEADLINE)
    }

    pub fn call(&mut self, tool: &str, args: Value) -> Reply {
        self.call_within(tool, args, DEADLINE)
    }

    pub fn call_within(&mut self, tool: &str, args: Value, budget: Duration) -> Reply {
        let id = self.next_id();
        let t = Instant::now();
        self.calls += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
        let v = self.recv(id, budget);
        self.latencies.push((tool.to_string(), t.elapsed()));
        if v.get("error").is_some() {
            return Reply::RpcError(v["error"].clone());
        }
        let text = v["result"]["content"][0]["text"].as_str().unwrap_or("null");
        let body: Value = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
        if v["result"]["isError"] == json!(true) {
            Reply::ToolError(body)
        } else {
            Reply::Ok(body)
        }
    }

    pub fn tools(&mut self) -> Vec<Value> {
        let id = self.next_id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}}));
        self.recv(id, DEADLINE)["result"]["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The server still answers a normal read within the deadline.
    pub fn alive(&mut self) {
        match self.call("rdy_overview", json!({})) {
            Reply::Ok(_) | Reply::ToolError(_) => {}
            Reply::RpcError(e) => {
                self.critical(&format!("rdy_overview failed at the protocol level: {e}"))
            }
        }
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn latencies(&self) -> &[(String, Duration)] {
        &self.latencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake server: answers `initialize`, then sends one notification and one reply with an id
    /// the client is not waiting for.
    fn fake() -> McpClient {
        let script = r#"read line
echo '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"resources":{"subscribe":true}}}}'
echo '{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":"rdy://inbox"}}'
echo '{"jsonrpc":"2.0","id":99,"result":{}}'
sleep 20"#;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]).stderr(Stdio::null());
        McpClient::launch(cmd)
    }

    #[test]
    fn ib10_the_client_keeps_the_initialize_reply_and_the_notifications() {
        let mut c = fake();
        assert_eq!(
            c.initialize_result()["result"]["capabilities"]["resources"]["subscribe"],
            true
        );
        assert!(c.wait_notification("rdy://inbox", Duration::from_secs(10)));
        // the one notification the fake sent, and nothing taken for one
        let uris: Vec<&str> = c
            .notifications()
            .iter()
            .map(|n| n["params"]["uri"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(uris, ["rdy://inbox"]);
        // the reply that was not asked for is set aside, never taken for a notification
        assert_eq!(c.recv(99, Duration::from_secs(5))["id"], 99);
    }

    #[test]
    fn ib10_drain_returns_at_once_when_nothing_is_waiting() {
        let mut c = fake();
        assert!(c.wait_notification("rdy://inbox", Duration::from_secs(10)));
        // the fake's one stray reply may still be on its way; once it is taken, nothing is left
        c.drain(Duration::from_millis(200));
        assert_eq!(c.drain(Duration::from_millis(50)), 0);
    }
}
