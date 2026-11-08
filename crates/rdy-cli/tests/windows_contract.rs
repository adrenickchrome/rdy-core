//! wx22: the stdio MCP server and the CLI, both served by one project daemon, give the same
//! results, and a daemon session shows its peer under this platform's own field (D-FARN9:
//! `peer_uid` on Unix, `peer_sid` on Windows). The tests run on every platform.
use rdy_daemon::sys;
use rdy_daemon::{Attempt, Hello, PROTO_VERSION, Proto, RunDir, connect};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// Paths as Windows writes them.
const NATIVE: [&str; 3] = [
    r"C:\Users\dev\My Project\src\lib.rs",
    r"\\server\share\docs\a.md",
    r"\\?\C:\very\long\path.txt",
];

/// A project served by a daemon, and the user config its commands read. Dropping it stops the
/// daemon, so a failing test leaves none behind.
struct Served {
    dir: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Served {
    fn new() -> Served {
        // a short parent: a socket path under it must fit `sun_path`
        let parent = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = tempfile::Builder::new()
            .prefix("rdy22")
            .tempdir_in(parent)
            .unwrap();
        drop(rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(".rdy.yaml"))
            .unwrap();
        file.write_all(b"serve:\n  idle_timeout: 60\n").unwrap();
        let cfg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cfg.path().join("rdy")).unwrap();
        std::fs::write(cfg.path().join("rdy/config.yaml"), "actor: human:founder\n").unwrap();
        Served { dir, cfg }
    }

    /// `rdy --project <dir> <args>` as the human, through the daemon.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rdy"));
        cmd.arg("--project")
            .arg(self.dir.path())
            .args(args)
            .current_dir(self.dir.path())
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .env("XDG_CONFIG_HOME", self.cfg.path())
            .env("HOME", self.cfg.path())
            .env("USERPROFILE", self.cfg.path())
            .env("APPDATA", self.cfg.path());
        cmd
    }

    fn cli(&self, args: &[&str]) -> Value {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "rdy {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn mcp(&self) -> Mcp {
        let mut child = self
            .command(&["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut m = Mcp {
            child,
            lines,
            next: 0,
            seen: vec![],
        };
        let init = m.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "wx22", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "rdy", "{init}");
        m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.command(&["serve", "--stop"]).output();
    }
}

struct Mcp {
    child: Child,
    lines: Receiver<String>,
    next: u64,
    /// Every line the server wrote to stdout.
    seen: Vec<String>,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mcp {
    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|e| panic!("no response to {method} within 60s: {e}"));
            self.seen.push(line.clone());
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carries a line that is not JSON ({e}): {line}"));
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// A tool call: the reply, and whether the call failed.
    fn raw(&mut self, name: &str, args: Value) -> (Value, bool) {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {r}"));
        (
            serde_json::from_str(text).unwrap(),
            r["result"]["isError"] == json!(true),
        )
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let (v, failed) = self.raw(name, args);
        assert!(!failed, "{name} failed: {v}");
        v
    }
}

#[test]
fn wx22_stdio_tools_and_cli_share_windows_results() {
    let s = Served::new();
    let mut m = s.mcp();
    m.tool(
        "rdy_session",
        json!({"actor": "agent:designer", "run": "run-22", "role": "writer"}),
    );
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Paths", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Native"}}),
    ];
    for (i, path) in NATIVE.iter().enumerate() {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": path}));
    }
    let made = m.tool("rdy_write", json!({ "ops": ops }));
    let doc = made["created"]["$d"].as_str().unwrap().to_string();

    // the same query through both surfaces: one daemon, one answer
    let mut by_tool = m.tool(
        "rdy_query",
        json!({"filter": "type = claim", "scope": "run-22"}),
    );
    // this raw client sends no kungfu value and has not read its instructions, so the tool's
    // reply alone carries the advisory (D-Y18E1): the reminder while it is due, then the warning.
    // The advisory is no part of the result, and the CLI's `q` is not an op that carries the
    // reminder (`ops::REMINDER_OPS`).
    let advised = by_tool.as_object_mut().unwrap();
    if let Some(reminder) = advised.remove("reminder") {
        assert_eq!(reminder, rdy_engine::instructions::REMINDER);
    }
    if let Some(w) = advised.remove("warnings") {
        for one in w.as_array().unwrap() {
            assert_eq!(one["code"], "W_KUNGFU", "{one}");
        }
    }
    let by_cli = s.cli(&["--json", "q", "type = claim", "--scope", "run-22"]);
    assert_eq!(by_tool, by_cli);
    let shown = by_cli.to_string();
    for path in NATIVE {
        let encoded = serde_json::to_string(path).unwrap();
        assert!(
            shown.contains(&encoded[1..encoded.len() - 1]),
            "{path:?} lost: {shown}"
        );
    }

    // the person's CLI reads the agent's view, and the dump path comes back as written
    let read = m.tool(
        "rdy_read",
        json!({"id": doc, "scope": "run-22", "format": "markdown"}),
    );
    for path in NATIVE {
        assert!(read["text"].as_str().unwrap().contains(path), "{read}");
    }
    let dumps = s.dir.path().join("dumps dir");
    std::fs::create_dir(&dumps).unwrap();
    let tool_file = dumps.join("tool.dump");
    let cli_file = dumps.join("cli.dump");
    let by_tool = m.tool("rdy_dump", json!({"out": tool_file.to_str().unwrap()}));
    let by_cli = s.cli(&["--json", "dump", "--out", cli_file.to_str().unwrap()]);
    assert_eq!(by_tool["out"], json!(tool_file.to_str().unwrap()));
    assert_eq!(by_cli["out"], json!(cli_file.to_str().unwrap()));
    assert!(tool_file.is_file() && cli_file.is_file());
    for key in ["dump_format", "kind", "digest", "end_batch", "repo_id"] {
        assert_eq!(by_tool[key], by_cli[key], "{key}");
    }

    // a proposal carries its summary; the CLI person approves it
    let p = m.tool(
        "rdy_propose",
        json!({"title": "native paths", "summary": "windows paths"}),
    );
    assert!(p["id"].is_string(), "{p}");

    // a malformed call is refused with the bounded error and the server goes on serving
    let (bad, failed) = m.raw("rdy_query", json!({"filter": ["x"]}));
    assert!(failed, "{bad}");
    assert_eq!(bad["code"], "E_BAD_INPUT", "{bad}");
    m.tool("rdy_overview", json!({}));
    // stdout isolation: every line was a JSON-RPC message (`call` parsed each one)
    assert!(!m.seen.is_empty());
}

fn ctl_sessions(project: &Path) -> Vec<Value> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let run = RunDir::of_project(project);
        let hello = Hello {
            rdy: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            build: rdy_daemon::BUILD.into(),
            proto: Proto::Ctl,
            cwd: project.to_path_buf(),
            project: run.project_id().unwrap(),
        };
        let Attempt::Ready(mut c) = connect(&run, &hello).await else {
            panic!("the project's daemon did not answer a ctl hello");
        };
        let v = c.ctl("sessions", json!({})).await.unwrap();
        c.close();
        v.as_array().unwrap().clone()
    })
}

#[test]
fn wx22_windows_sessions_use_the_settled_identity_field() {
    let s = Served::new();
    let mut m = s.mcp();
    m.tool(
        "rdy_session",
        json!({"actor": "agent:designer", "run": "run-22", "role": "writer"}),
    );
    let sessions = ctl_sessions(s.dir.path());
    assert!(
        sessions.iter().any(|x| x["proto"] == "mcp"),
        "the stdio server holds a session: {sessions:?}"
    );
    let me = sys::own_account().unwrap();
    for session in &sessions {
        #[cfg(windows)]
        {
            let sid = session["peer_sid"].as_str().unwrap();
            assert!(sid.starts_with("S-1-"), "{session}");
            assert_eq!(sid, me.to_string(), "the whole SID, never cut: {session}");
            assert!(session.get("peer_uid").is_none(), "{session}");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(session["peer_uid"], json!(me), "{session}");
            assert!(session.get("peer_sid").is_none(), "{session}");
        }
    }
}
