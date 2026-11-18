//! scc10 (D-7CDEQ, D-H0BZN, D-D5J18, D-5Y8Y1, D-47HVF): two machines and a URL clone sync through
//! one fixture hub, over real HTTPS.
//!
//! Each machine is a project with its own user home (config, credentials and meta project), its
//! own daemon and its own sign-in. The hub is a loopback TLS listener whose test CA each home
//! trusts through its install's `ca_file` ([`mock::Hub`]); its holder is an adapter holder. The
//! tests drive the release `rdy` binary and its MCP clients only: the sync service is never
//! called in this process. A missing binary fails the tests; they never skip.
//!
//! The binary is `$RDY_BIN`, else the `rdy` beside this test's `deps` directory. Projects are
//! made, landed and read through the core calls while their daemons are stopped.

#[path = "sync_e2e/mock.rs"]
mod mock;

use mock::{Fault, Hub, Verdict};
use rdy_adapter::stream::StreamPurpose;
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::hub_config::{
    Credential, CredentialFile, ReadyCredential, RemoteMode, encode_credentials,
};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{Origin, Secret};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, now_ms};
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

// ---------------------------------------------------------------------------------------------
// the binary and the machines
// ---------------------------------------------------------------------------------------------

/// The `rdy` binary under test. A missing one fails the test.
fn rdy_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDY_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("this test's executable");
    let dir = exe.parent().expect("its directory");
    let target = if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().expect("the profile directory")
    } else {
        dir
    };
    let bin = target.join(format!("rdy{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.exists(),
        "{} not found: run `cargo build --release -p rdy-cli` first, or set RDY_BIN",
        bin.display()
    );
    bin
}

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const ACCOUNT: &str = "acc_00000000000000000000000001";

#[cfg(unix)]
fn private(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn private(_path: &Path, _mode: u32) {}

/// A directory short enough for the daemon's socket path under it.
fn short_dir(prefix: &str) -> tempfile::TempDir {
    let root = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("a short directory")
}

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// Writes a doc with one prose block in `view`.
fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> Value {
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": "page"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": body}
                ]))
                .unwrap(),
                justification: None,
                message: "scc10".into(),
            },
        )
        .unwrap_or_else(|e| panic!("write {view}: {e:#?}"));
    json!(w.created)
}

/// Proposes view `view` and lands it; returns the landing.
fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), view, view, Some("test summary"))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"));
    r.land(&session(Role::Approver), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:#?}"))
        .parse()
        .unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// One machine: its own user home (config, credentials, meta project) that trusts the hub's test
/// CA and holds a fresh sign-in, and its own project directory with its own daemon.
struct Machine {
    name: &'static str,
    home: tempfile::TempDir,
    dir: tempfile::TempDir,
}

impl Machine {
    /// A machine whose home trusts `hub` and is signed in there. Its project directory is empty.
    fn new(name: &'static str, hub: &Hub) -> Machine {
        let home = tempfile::tempdir().expect("a home");
        private(home.path(), 0o700);
        std::fs::create_dir(home.path().join("rdy")).unwrap();
        private(&home.path().join("rdy"), 0o700);
        let pem = home.path().join("ca.pem");
        std::fs::write(&pem, &hub.ca_pem).unwrap();
        std::fs::write(
            home.path().join("rdy/config.yaml"),
            format!(
                "actor: human:qa\nhub:\n  installs:\n    \"{}\":\n      ca_file: {}\n",
                hub.origin(),
                pem.display()
            ),
        )
        .unwrap();
        let mut file = CredentialFile::default();
        file.set(
            Origin::parse(hub.origin()).unwrap(),
            Some(Credential::Ready(ReadyCredential {
                access_token: Secret::new(ACCESS),
                refresh_token: Secret::new(REFRESH),
                expires_at_ms: now_ms() + 3_000_000,
                account: ACCOUNT.into(),
                handle: format!("person-{name}"),
            })),
        );
        let creds = home.path().join("rdy/credentials.yaml");
        std::fs::write(&creds, encode_credentials(&file).unwrap()).unwrap();
        private(&creds, 0o600);
        Machine {
            name,
            home,
            dir: short_dir("rdys"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("rdy/config.yaml")
    }

    /// The environment of every process this machine's person starts.
    fn env(&self) -> Vec<(String, String)> {
        let home = self.home.path().to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".into(), home.clone()),
            ("HOME".into(), home),
            ("USER".into(), "qa".into()),
        ]
    }

    /// `rdy <args>` as this machine's person, from `cwd`, with nothing else of this process.
    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(rdy_bin());
        cmd.env_clear()
            .envs(std::env::var_os("SystemRoot").map(|v| ("SystemRoot", v)))
            .envs(std::env::var_os("TEMP").map(|v| ("TEMP", v)))
            .envs(std::env::var_os("TMP").map(|v| ("TMP", v)))
            .envs(self.env())
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null());
        cmd
    }

    /// The project as a new project (made, not landed): `rdy init` through the core call.
    fn init(&self) {
        drop(Rdy::init(self.path(), "rdy-standard").expect("init"));
    }

    /// Opens the project (its daemon stopped first), runs `f`, and closes it.
    fn with<T>(&self, f: impl FnOnce(&mut Rdy) -> T) -> T {
        self.stop();
        let mut r = Rdy::open(self.path()).expect("open the project");
        let out = f(&mut r);
        r.close().expect("close the project");
        out
    }

    /// Lands one doc in its own view; returns the landing.
    fn land_doc(&self, title: &str, body: &str) -> Hash {
        self.with(|r| {
            let view = format!("w-{title}");
            doc(r, &view, title, body);
            land(r, &view)
        })
    }

    /// The project's canon tip, read through an adapter handle (its daemon stopped).
    fn canon(&self) -> Option<Hash> {
        self.stop();
        let h = StoreHandle::open(self.path()).expect("open the store");
        let tip = h.snapshot().expect("a snapshot").canon();
        h.close().unwrap();
        tip
    }

    /// Adds the share remote `origin` at `url`, and the daemon's idle limit.
    fn remote(&self, url: &str) {
        RemoteService::new(self.path(), &self.config())
            .add("origin", url, RemoteMode::Share, RemoteLevel::Project)
            .expect("add the remote");
        self.serve_limit();
    }

    fn serve_limit(&self) {
        let cfg = self.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        if !text.contains("idle_timeout") {
            text.push_str("serve:\n  idle_timeout: 60\n");
            std::fs::write(&cfg, text).unwrap();
        }
    }

    /// Stops the project's daemon, if one runs.
    fn stop(&self) {
        let _ = self
            .command(
                self.path(),
                &[
                    "--project",
                    self.path().to_str().unwrap(),
                    "serve",
                    "--stop",
                ],
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// `rdy --project <p> --json <args>`: the JSON of a success, or of the error on failure.
    fn cli(&self, args: &[&str]) -> (bool, Value) {
        let mut all = vec!["--project", self.path().to_str().unwrap(), "--json"];
        all.extend_from_slice(args);
        run_json(self.command(self.path(), &all))
    }

    fn cli_ok(&self, args: &[&str]) -> Value {
        let (ok, v) = self.cli(args);
        assert!(ok, "{} rdy {args:?}: {v}", self.name);
        v
    }

    fn cli_err(&self, args: &[&str]) -> Value {
        let (ok, v) = self.cli(args);
        assert!(!ok, "{} rdy {args:?} succeeded: {v}", self.name);
        v
    }

    /// The text (stdout and stderr) of a successful run.
    fn cli_text(&self, args: &[&str]) -> String {
        let mut all = vec!["--project", self.path().to_str().unwrap()];
        all.extend_from_slice(args);
        let o = self.command(self.path(), &all).output().expect("run rdy");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "{} rdy {args:?}: {text}", self.name);
        assert_no_token(&text);
        text
    }

    /// The answer of an agent that relays the person's quoted words to a card: the same
    /// arguments plus the card's hash, the answer and the relay flags, run as an agent session.
    fn relay(&self, args: &[&str], card: &Value, words: &str) -> Value {
        let hash = card["card_hash"].as_str().expect("a card hash").to_string();
        let mut all = vec!["--project", self.path().to_str().unwrap(), "--json"];
        all.extend_from_slice(args);
        all.extend([
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
            "--relayed-by",
            "agent:relay",
            "--evidence",
            words,
            "--question",
            "Move hub canon?",
            "--session-ref",
            "chat-10",
        ]);
        let mut cmd = self.command(self.path(), &all);
        cmd.env("RDY_ACTOR", "agent:relay").env("RDY_RUN", "r9");
        let (ok, v) = run_json(cmd);
        assert!(ok, "{} relay {args:?}: {v}", self.name);
        v
    }

    /// A stdio MCP client of this machine's daemon with a writer session of an agent.
    fn chat(&self) -> Stdio3 {
        Stdio3::start(self)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn assert_no_token(text: &str) {
    for s in [ACCESS, REFRESH, "Bearer"] {
        assert!(!text.contains(s), "an output printed {s}:\n{text}");
    }
}

/// The JSON of a finished command: stdout on success, the error object on stderr on failure.
fn run_json(mut cmd: Command) -> (bool, Value) {
    let o = cmd.output().expect("run rdy");
    let ok = o.status.success();
    let text = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr }).into_owned();
    assert_no_token(&text);
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        serde_json::from_str(text.trim().lines().last().unwrap_or(""))
            .unwrap_or(Value::String(text))
    });
    (ok, v)
}

// ---------------------------------------------------------------------------------------------
// the MCP clients
// ---------------------------------------------------------------------------------------------

/// An MCP client of `rdy mcp` over stdio: one JSON-RPC message a line.
struct Stdio3 {
    child: Child,
    lines: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Stdio3 {
    fn start(m: &Machine) -> Stdio3 {
        let mut cmd = m.command(m.path(), &["mcp", "--project", m.path().to_str().unwrap()]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn rdy mcp");
        let lines = BufReader::new(child.stdout.take().unwrap());
        let mut c = Stdio3 {
            child,
            lines,
            next: 0,
        };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "scc10", "version": "0"}}),
        );
        assert!(init["result"].is_object(), "{init}");
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        let (ok, v) = c.call(
            "rdy_session",
            json!({"actor": "agent:claude", "run": "r7", "role": "writer"}),
        );
        assert!(ok, "{v}");
        c
    }

    fn send(&mut self, v: &Value) {
        let stdin = self.child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.lines.read_line(&mut line).unwrap() > 0,
                "rdy mcp closed"
            );
            let v: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// A tool call: whether it succeeded, and the rdy body.
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        tool_body(&self.request("tools/call", json!({"name": tool, "arguments": args})))
    }
}

impl Drop for Stdio3 {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// A `tools/call` result as (ok, rdy body).
fn tool_body(reply: &Value) -> (bool, Value) {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool reply: {reply}"));
    assert_no_token(text);
    (
        reply["result"]["isError"] != json!(true),
        serde_json::from_str(text).expect("a JSON tool body"),
    )
}

/// The project's MCP endpoint over HTTP: its URL and bearer, as `rdy mcp --http-config` prints.
struct Http {
    addr: String,
    token: String,
    next: u64,
}

impl Http {
    fn start(m: &Machine) -> Http {
        let local = m.path().join(".rdy/local.yaml");
        std::fs::write(&local, "serve:\n  mcp_http: true\n").unwrap();
        // this output holds the MCP bearer on purpose, so it skips the hub-token check
        let args = [
            "--project",
            m.path().to_str().unwrap(),
            "--json",
            "mcp",
            "--http-config",
        ];
        let o = m.command(m.path(), &args).output().expect("run rdy");
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let v: Value = serde_json::from_slice(&o.stdout).expect("the config JSON");
        let url = v["url"].as_str().expect("a url").to_string();
        let addr = url
            .strip_prefix("http://")
            .and_then(|r| r.split('/').next())
            .expect("an http url")
            .to_string();
        let token = v["headers"]["Authorization"]
            .as_str()
            .and_then(|a| a.strip_prefix("Bearer "))
            .expect("a bearer")
            .to_string();
        Http {
            addr,
            token,
            next: 0,
        }
    }

    /// A legacy `tools/call` with the claim of an agent writer: (ok, rdy body).
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        self.next += 1;
        let body = json!({"jsonrpc": "2.0", "id": self.next, "method": "tools/call",
            "params": {"name": tool, "arguments": args,
                "_meta": {"rdy/actor": "agent:claude", "rdy/run": "r7", "rdy/role": "writer"}}})
        .to_string();
        let raw = format!(
            "POST /~mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: 2025-06-18\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.token,
            body.len()
        );
        let mut s = std::net::TcpStream::connect(&self.addr).expect("connect the site");
        s.write_all(raw.as_bytes()).unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).unwrap();
        let text = String::from_utf8_lossy(&got).into_owned();
        let (_, payload) = text.split_once("\r\n\r\n").expect("a reply");
        let message = if text.to_ascii_lowercase().contains("text/event-stream") {
            payload
                .split("\n\n")
                .filter_map(|e| {
                    let d: String = e
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:"))
                        .map(str::trim)
                        .collect();
                    serde_json::from_str::<Value>(&d).ok()
                })
                .last()
                .expect("an event")
        } else {
            serde_json::from_str(payload.trim()).unwrap_or_else(|_| {
                // a chunked body: the JSON starts at its first brace
                let from = payload.find('{').expect("a JSON body");
                let to = payload.rfind('}').expect("a JSON body");
                serde_json::from_str(&payload[from..=to]).expect("a JSON reply")
            })
        };
        tool_body(&message)
    }
}

// ---------------------------------------------------------------------------------------------
// reads of a store
// ---------------------------------------------------------------------------------------------

/// The landings of the canon history of the project at `dir`, oldest first.
fn history(m: &Machine) -> Vec<Hash> {
    m.stop();
    history_at(m.path())
}

/// [`history`] of a project directory whose daemon is not running.
fn history_at(dir: &Path) -> Vec<Hash> {
    let h = StoreHandle::open(dir).expect("open the store");
    let snap = h.snapshot().expect("a snapshot");
    let mut hashes: Vec<Hash> = snap
        .canon_history()
        .unwrap()
        .iter()
        .map(|l| l.hash)
        .collect();
    hashes.reverse();
    drop(snap);
    h.close().unwrap();
    hashes
}

/// The number of snapshot deltas (kind 30) and landings (kind 4) in the canon history of `tip`
/// that a snapshot of the store at `dir` exports.
fn deltas_and_landings(m: &Machine) -> (usize, usize) {
    m.stop();
    deltas_and_landings_at(m.path())
}

/// [`deltas_and_landings`] of a project directory whose daemon is not running.
fn deltas_and_landings_at(dir: &Path) -> (usize, usize) {
    let h = StoreHandle::open(dir).expect("open the store");
    let snap = h.snapshot().expect("a snapshot");
    let tip = snap.canon().expect("a canon tip");
    let report = snap
        .write_history(
            StreamPurpose::CanonHistory,
            tip,
            None,
            &BTreeSet::new(),
            std::io::sink(),
        )
        .expect("export");
    let count = |kind: u8| report.keys.iter().filter(|k| k.kind == kind).count();
    drop(snap);
    h.fsck().expect("fsck passes");
    h.close().unwrap();
    (
        count(rdy_store::kinds::SNAPSHOT_DELTA),
        count(rdy_store::kinds::LANDING),
    )
}

/// A machine `name` whose project is a copy of `from`'s as it stands (its daemon stopped), with
/// its own home, sign-in and daemon, and the hub remote.
fn copy_of(name: &'static str, from: &Machine, hub: &Hub) -> Machine {
    from.stop();
    let m = Machine::new(name, hub);
    copy_dir(from.path(), m.path());
    let _ = std::fs::remove_file(m.path().join(".rdy/daemon.json"));
    let _ = std::fs::remove_dir_all(m.path().join(".rdy/run"));
    m
}

// ---------------------------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------------------------

/// scc10: a push from machine A seeds the hub, a pull into machine B takes hub canon, and a URL
/// clone into C makes a third project. Each machine has its own home and daemon. C's settings
/// come from the hub's settings step (the capturing push's inputs); every store holds one snapshot
/// delta for each landing, and the hub's holder passes its own check.
#[test]
fn scc10_two_machines_push_pull_and_clone_over_tls() {
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    a.init();
    // machine B is a second project of the same repo, made before any landing
    a.stop();
    let b = copy_of("b", &a, &hub);
    let landings: Vec<Hash> = ["alpha", "beta"]
        .iter()
        .map(|d| a.land_doc(d, &format!("{d} is landed.")))
        .collect();
    a.remote(&hub.url());
    b.remote(&hub.url());
    // settings the clone must take from the hub
    let cfg = a.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(
        "name: Atlas\ndescription: The atlas of decisions.\ncoverage:\n  target: 75\n  gate: no_lower\npropose:\n  cadence: task\n",
    );
    std::fs::write(&cfg, text).unwrap();

    // A pushes: the hub's empty repo takes the seed
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub.canon(), landings.last().copied());
    let writes = hub.of("/write");
    assert_eq!(writes.len(), 1);
    let sent = writes[0].json();
    assert_eq!(sent["kind"], "landings");
    assert_eq!(sent["gate"]["coverage_target"], 75);
    assert_eq!(sent["render_inputs"]["title"], "Atlas");
    hub.fsck();
    let (deltas, lands) = hub
        .with_snapshot(|s| {
            let tip = s.canon().unwrap();
            let r = s
                .write_history(
                    StreamPurpose::CanonHistory,
                    tip,
                    None,
                    &BTreeSet::new(),
                    std::io::sink(),
                )
                .unwrap();
            let n = |kind: u8| r.keys.iter().filter(|k| k.kind == kind).count();
            (
                n(rdy_store::kinds::SNAPSHOT_DELTA),
                n(rdy_store::kinds::LANDING),
            )
        })
        .unwrap();
    assert_eq!(
        (deltas, lands),
        (2, 2),
        "the hub holds one delta for each landing"
    );

    // B pulls hub canon
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    assert_eq!(b.canon(), landings.last().copied());
    assert_eq!(history(&b), landings);
    assert_eq!(deltas_and_landings(&b), (2, 2));

    // C clones the hub repo by URL, outside any project
    let c = Machine::new("c", &hub);
    let target = c.path().join("clone");
    let v = {
        let mut cmd = c.command(
            c.path(),
            &["--json", "clone", &hub.url(), target.to_str().unwrap()],
        );
        cmd.stdin(Stdio::null());
        let (ok, v) = run_json(cmd);
        assert!(ok, "clone: {v}");
        v
    };
    assert!(v.to_string().contains("clone"), "{v}");
    assert_eq!(history_at(&target), landings);
    assert_eq!(
        deltas_and_landings_at(&target),
        (2, 2),
        "SD1: one delta for each landing in C"
    );
    let cloned = std::fs::read_to_string(target.join(".rdy.yaml")).unwrap();
    for want in ["Atlas", "75", "no_lower", "task", "hub:"] {
        assert!(cloned.contains(want), "C's settings hold {want}:\n{cloned}");
    }
    // the clone sent only the hub's read steps
    let routes = hub.routes();
    assert!(routes.iter().any(|r| r == "settings"), "{routes:?}");
    assert!(
        !hub.of("/write").is_empty() && hub.of("/write").len() == 1,
        "the clone wrote nothing"
    );
    hub.finish();
}

/// Two machines of one repo that share a base landing, a hub that A seeded with it, and B in
/// step with the hub. Returns (hub, a, b, base).
fn pair() -> (Hub, Machine, Machine, Hash) {
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    a.init();
    let base = a.land_doc("base", "The base is landed.");
    let b = copy_of("b", &a, &hub);
    a.remote(&hub.url());
    b.remote(&hub.url());
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub.canon(), Some(base));
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    (hub, a, b, base)
}

/// A signature of landing `l` on the project, with the bytes of a signer that is not here: the
/// replay carries a signature whole and needs no key (D-8ZEHP).
fn sign(m: &Machine, l: Hash) -> Signature {
    m.with(|r| {
        let repo = r.store().reader().repo_id().unwrap().unwrap();
        let rec = r.store().reader().landing(&l).unwrap();
        let fingerprint = rdy_engine::signing_crypto::blob_fingerprint(b"scc10 key blob");
        let stmt = statement(StatementSource::Approval(&rec), Some(repo), &fingerprint).unwrap();
        let ns = parse_statement(&stmt).unwrap().namespace().to_string();
        let g = Signature {
            subject: l,
            sig: b"scc10 signature bytes".to_vec(),
            namespace: ns,
            statement: stmt,
            signed_by: Provenance {
                actor: "agent:signer".into(),
                run: Some("s/1".into()),
                role: Role::Approver,
                at_ms: 1_000,
            },
        };
        r.store_mut().put_signature(&g).unwrap();
        r.seal_open().unwrap();
        g
    })
}

#[test]
fn scc10_divergence_replay_and_signature_carry_round_trip() {
    let (hub, a, b, base) = pair();
    let a1 = a.land_doc("alpha", "Alpha is landed.");
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub.canon(), Some(a1));
    // B lands and signs its own landing, not knowing a1
    let b1 = b.land_doc("beta", "Beta is landed.");
    let g = sign(&b, b1);
    let writes = hub.of("/write").len();
    let e = b.cli_err(&["push"]);
    assert_eq!(e["code"], "E_DIVERGED", "{e}");
    assert_eq!(
        hub.of("/write").len(),
        writes,
        "a diverged push writes nothing"
    );
    // the pull replays B's landing onto the hub tip, with its signature carried whole
    let v = b.cli_ok(&["pull"]);
    let replayed = &v["canon"]["replayed"];
    assert_eq!(replayed.as_array().unwrap().len(), 1, "{v}");
    assert_eq!(replayed[0]["source"], b1.to_string());
    assert_eq!(replayed[0]["signatures"], 1);
    assert_eq!(v["canon"]["not_replayed"], json!([]));
    assert_eq!(v["canon"]["left_canon"], json!([b1.to_string()]));
    let b1x: Hash = replayed[0]["landing"].as_str().unwrap().parse().unwrap();
    assert_ne!(b1x, b1);
    assert_eq!(history(&b), vec![base, a1, b1x]);
    let carried = b.with(|r| {
        let ids = r.store().reader().signatures_of(&b1x).unwrap();
        assert_eq!(ids.len(), 1, "{ids:?}");
        r.store().reader().signature(&ids[0]).unwrap()
    });
    assert_eq!(carried.subject, b1x);
    assert_eq!(carried.statement, g.statement, "the statement is unchanged");
    assert_eq!(carried.sig, g.sig, "the signature bytes are unchanged");
    // the next push takes: it expects the hub tip and carries the replayed landing and signature
    let v = b.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub.canon(), Some(b1x));
    let sent = hub.of("/write").pop().unwrap().json();
    assert_eq!(sent["expected"]["hash"], a1.to_string());
    assert_eq!(sent["kind"], "landings");
    // (no fsck of the hub's holder here: the signature's bytes are a signer's that is not here,
    // so fsck reads it as invalid by design; the other tests check the holder)
    let (deltas, lands, sigs) = hub
        .with_snapshot(|s| {
            let r = s
                .write_history(
                    StreamPurpose::CanonHistory,
                    b1x,
                    None,
                    &BTreeSet::new(),
                    std::io::sink(),
                )
                .unwrap();
            let n = |kind: u8| r.keys.iter().filter(|k| k.kind == kind).count();
            (
                n(rdy_store::kinds::SNAPSHOT_DELTA),
                n(rdy_store::kinds::LANDING),
                n(rdy_store::kinds::SIGNATURE),
            )
        })
        .unwrap();
    // the replayed landing names its source (D-8ZEHP), so the history holds B's first landing and
    // its signature too: four landings, one delta each, and the two signatures
    assert_eq!(
        (deltas, lands, sigs),
        (4, 4, 2),
        "one delta for each landing"
    );
    // A takes B's landing back
    let v = a.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    assert_eq!(history(&a), vec![base, a1, b1x]);
    hub.finish();
}

/// scc10: a force push, once the person's card is answered, replaces hub canon, and a machine
/// that held the replaced canon never brings it back. B pushed the chain that A then forces
/// out; B's pull, then its take of the hub tip, leave B on A's chain, and B's next push sends
/// nothing and leaves hub canon as it is.
#[test]
fn scc10_force_then_pull_never_restores_old_hub_canon() {
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    a.init();
    let base = a.land_doc("base", "The base is landed.");
    let b = copy_of("b", &a, &hub);
    a.remote(&hub.url());
    b.remote(&hub.url());
    // B pushes first: hub canon is B's chain
    let b1 = b.land_doc("beta", "Beta is landed.");
    assert_eq!(b.cli_ok(&["push"])["canon"]["status"], "taken");
    assert_eq!(hub.canon(), Some(b1));
    // A lands on the base and cannot push: its canon does not hold the hub tip
    let a1 = a.land_doc("alpha", "Alpha is landed.");
    let e = a.cli_err(&["push"]);
    assert_eq!(e["code"], "E_DIVERGED", "{e}");
    // `--force` returns a card first and sends nothing
    let writes = hub.of("/write").len();
    let card = a.cli_ok(&["push", "--force"]);
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "force_push");
    assert_eq!(card["before"], b1.to_string());
    assert_eq!(card["after"], a1.to_string());
    assert_eq!(hub.of("/write").len(), writes);
    // an agent relays the person's quoted words with the card's hash: the hub's gate forces
    hub.script(&[Verdict::Force]);
    let v = a.relay(&["push", "--force"], &card, "Yes, force it.");
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    let sent = hub.of("/write").pop().unwrap().json();
    assert_eq!(sent["kind"], "force");
    assert_eq!(sent["expected"]["hash"], b1.to_string());
    assert_eq!(hub.canon(), Some(a1));
    hub.fsck();
    // B pulls: the hub's tip is not B's chain, and B's landing counts as pushed, so canon takes
    // the hub's tip and B's landing is left under `replaced/`, not replayed
    let v = b.cli_ok(&["pull", "--strict", "--remote", "origin"]);
    assert_eq!(v["canon"]["canon"], a1.to_string(), "{v}");
    assert_eq!(v["canon"]["replayed"], json!([]), "{v}");
    assert_eq!(v["canon"]["left_canon"], json!([b1.to_string()]));
    assert_eq!(v["canon"]["replaced"], b1.to_string());
    assert_eq!(history(&b), vec![base, a1]);
    // B pushes: nothing is sent, and the old canon never returns to the hub
    let writes = hub.of("/write").len();
    let v = b.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "up_to_date", "{v}");
    assert_eq!(hub.of("/write").len(), writes, "B has nothing to push");
    assert_eq!(hub.canon(), Some(a1));
    // and a second pull brings nothing back
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["canon"], a1.to_string(), "{v}");
    assert_eq!(history(&b), vec![base, a1]);
    assert_eq!(deltas_and_landings(&b), (2, 2));
    hub.finish();
    let _ = base;
}

/// Writes a doc into view `view` of the project (its daemon stopped) and returns the view's head.
fn view_write(m: &Machine, view: &str, title: &str) -> Hash {
    m.with(|r| {
        doc(r, view, title, &format!("{title} is in flight."));
        r.store().reader().view_head(view).unwrap().unwrap().head
    })
}

fn view_head(m: &Machine, view: &str) -> Option<Hash> {
    m.stop();
    let h = StoreHandle::open(m.path()).expect("open the store");
    let head = h
        .snapshot()
        .unwrap()
        .view_head(view)
        .unwrap()
        .map(|v| v.head);
    h.close().unwrap();
    head
}

#[test]
fn scc10_views_rechain_close_and_take_round_trip() {
    let (hub, a, b, _base) = pair();
    let d1 = view_write(&a, "draft", "Draft one");
    view_write(&a, "other", "Other");
    let v = a.cli_ok(&["push", "--view", "draft"]);
    eprintln!("A push view: {v}");
    assert_eq!(hub.view("draft"), Some(d1));
    assert_eq!(hub.view("other"), None, "only the named view travels");
    let v = b.cli_ok(&["pull", "--view", "draft"]);
    assert_eq!(v["views"][0]["status"], "new", "{v}");
    assert_eq!(view_head(&b, "draft"), Some(d1));
    // both machines move the view: A pushes one op, B adds its own and pulls
    let d2 = view_write(&a, "draft", "Draft two");
    a.cli_ok(&["push", "--view", "draft"]);
    assert_eq!(hub.view("draft"), Some(d2));
    let b1 = view_write(&b, "draft", "Draft three");
    assert_ne!(b1, d1);
    let v = b.cli_ok(&["pull", "--view", "draft"]);
    let row = &v["views"][0];
    assert_eq!(row["status"], "rechained", "{v}");
    assert_eq!(row["hub_head"], d2.to_string());
    let rechained = view_head(&b, "draft").unwrap();
    assert_eq!(row["head"], rechained.to_string());
    assert_ne!(rechained, b1, "B's ops are chained after the hub head");
    assert_eq!(
        row["replaced"],
        b1.to_string(),
        "B's old head stays readable"
    );
    assert!(!row["rechained"].as_array().unwrap().is_empty(), "{v}");
    // B pushes the re-chained view: it extends the hub head
    let v = b.cli_ok(&["push", "--view", "draft"]);
    assert_eq!(v["views"][0]["end"]["base"], d2.to_string(), "{v}");
    assert_eq!(v["views"][0]["end"]["status"], "taken");
    assert_eq!(hub.view("draft"), Some(rechained));
    // A moves to it, closes the view and pushes the close; B pulls the close
    let v = a.cli_ok(&["pull", "--view", "draft"]);
    assert_eq!(v["views"][0]["status"], "fast_forward", "{v}");
    assert_eq!(view_head(&a, "draft"), Some(rechained));
    a.cli_ok(&["view", "close", "draft"]);
    let v = a.cli_ok(&["push", "--view", "draft"]);
    let closed = hub.view("draft").unwrap();
    assert_eq!(v["views"][0]["end"]["tip"], closed.to_string(), "{v}");
    let v = b.cli_ok(&["pull", "--view", "draft"]);
    assert_eq!(v["views"][0]["status"], "fast_forward", "{v}");
    assert_eq!(view_head(&b, "draft"), Some(closed));
    b.stop();
    let h = StoreHandle::open(b.path()).unwrap();
    assert!(
        h.snapshot()
            .unwrap()
            .view_head("draft")
            .unwrap()
            .unwrap()
            .closed,
        "B's view is closed"
    );
    h.close().unwrap();
    // a take moves only the views it names, and keeps the old chain readable
    let o1 = view_write(&a, "other", "Other one");
    view_write(&a, "third", "Third");
    a.cli_ok(&["push", "--view", "other", "--view", "third"]);
    b.cli_ok(&["pull", "--view", "other"]);
    assert_eq!(view_head(&b, "other"), Some(o1));
    let o2 = view_write(&a, "other", "Other two");
    a.cli_ok(&["push", "--view", "other"]);
    let local = view_write(&b, "other", "Other local");
    let v = b.cli_ok(&["pull", "--take", "--view", "other"]);
    assert!(
        v.get("card").is_none() && v["views"].as_array().unwrap().len() == 1,
        "{v}"
    );
    assert_eq!(
        view_head(&b, "other"),
        Some(o2),
        "the take moves B's view to the hub head"
    );
    assert_eq!(
        view_head(&b, "third"),
        None,
        "a view the take does not name stays on the hub"
    );
    assert_ne!(local, o2);
    // the hub's holder holds every view op it was sent, and passes its own check
    hub.fsck();
    hub.finish();
}

/// Whether any file of the project `m` (its daemon stopped) holds `needle`.
fn project_holds(m: &Machine, needle: &str) -> bool {
    m.stop();
    fn walk(dir: &Path, needle: &[u8]) -> bool {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, needle)
                } else {
                    std::fs::read(&p).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
                }
            })
    }
    walk(&m.path().join(".rdy"), needle.as_bytes())
}

/// Removes every version of block `target` through the store's own landing path, and returns the
/// landing of the removal.
fn remove(r: &mut Rdy, target: &str) -> Hash {
    let s = session(Role::Writer);
    r.redact_in(
        &s,
        "redact",
        RedactInput {
            targets: vec![target.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r
        .propose(&s, "redact", "Remove the code", Some("test summary"))
        .unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r.store().reader().canon_head().unwrap().unwrap()
}

/// scc10: an approved removal travels with a push and a pull, and a local copy that no removal
/// covers keeps its bytes. The status repeats the warning with D-47HVF's words.
#[test]
fn scc10_removal_propagates_without_erasing_uncovered_local_copy() {
    use rdy_engine::{SYNC_UNCOVERED_FIX, SYNC_UNCOVERED_MESSAGE};
    const SECRET: &str = "scc10-secret-token-4471";
    let hub = Hub::start();
    let a = Machine::new("a", &hub);
    a.init();
    let prose = a.with(|r| {
        let created = doc(r, "seed", "Secret page", SECRET);
        land(r, "seed");
        created["$p"].as_str().unwrap().to_string()
    });
    let b = copy_of("b", &a, &hub);
    a.remote(&hub.url());
    b.remote(&hub.url());
    // B writes the prose away and back in a view of its own: a copy no removal covers
    b.with(|r| {
        for body in ["scc10-away-text", SECRET] {
            r.write(
                &session(Role::Writer),
                WriteRequest {
                    view: "keep".into(),
                    ops: serde_json::from_value(
                        json!([{"op": "update", "id": prose, "body": body}]),
                    )
                    .unwrap(),
                    justification: None,
                    message: "scc10".into(),
                },
            )
            .unwrap();
        }
    });
    assert!(project_holds(&b, SECRET));
    // A removes the secret and pushes the removal
    let removal = a.with(|r| remove(r, &prose));
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub.canon(), Some(removal));
    assert!(
        !hub.holder_holds(SECRET),
        "the hub never holds the removed text"
    );
    hub.fsck();
    // B pulls it: canon takes the removal
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["canon"], removal.to_string(), "{v}");
    assert_eq!(b.canon(), Some(removal));
    // B's own copy keeps its bytes, and the status repeats the warning with the exact words
    assert!(
        project_holds(&b, SECRET),
        "an uncovered local copy keeps its bytes"
    );
    let status = b.cli_ok(&["status"]);
    let w = status["uncovered_warnings"].as_array().expect("warnings");
    assert_eq!(w.len(), 1, "{status}");
    let (change, removal_id) = (
        w[0]["change"].as_str().unwrap(),
        w[0]["removal"].as_str().unwrap(),
    );
    let message = SYNC_UNCOVERED_MESSAGE
        .replace("<change>", change)
        .replace("<removal>", removal_id);
    assert_eq!(w[0]["code"], "W_REDACTION_UNCOVERED");
    assert_eq!(w[0]["message"], message);
    assert_eq!(w[0]["fix"], SYNC_UNCOVERED_FIX);
    let text = b.cli_text(&["status"]);
    assert!(
        text.contains(&format!(
            "warning: W_REDACTION_UNCOVERED: {message}; {SYNC_UNCOVERED_FIX}"
        )),
        "{text}"
    );
    // the person never sees the text through the project's canon
    assert_eq!(deltas_and_landings(&b), (2, 2));
    hub.finish();
}

/// Tags landing `l` as `name` in the project `m` (its daemon stopped).
fn tag_at(m: &Machine, name: &str) {
    m.with(|r| {
        r.tag(&session(Role::Approver), name, None, None).unwrap();
    });
}

/// scc10: a restore of a project dump sends the dump's gate and render inputs, and one of a
/// holder dump sends none; a refused restore moves nothing. A pull whose tag name the project
/// already holds on another landing keeps the local tag and reports the hub tag as not taken, and
/// writes no half of its work.
#[test]
fn scc10_restore_and_tag_collision_are_atomic() {
    let hub1 = Hub::start();
    let a = Machine::new("a", &hub1);
    a.init();
    let base = a.land_doc("base", "The base is landed.");
    tag_at(&a, "v1.0.0");
    a.remote(&hub1.url());
    let cfg = a.path().join(".rdy.yaml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("name: Atlas\n");
    std::fs::write(&cfg, text).unwrap();
    let dump = a.path().join("p.rdydump");
    let args = [
        "--project",
        a.path().to_str().unwrap(),
        "dump",
        "--out",
        dump.to_str().unwrap(),
    ];
    let o = a.command(a.path(), &args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // B is a copy: it holds the remote and the base
    let b = copy_of("b", &a, &hub1);
    // a refused restore moves nothing (a hub code in no table is core's own text), and a retry takes
    hub1.script(&[Verdict::Refuse(422, "E_HUB_REFUSED".into())]);
    let v = a.cli_ok(&["push", "--restore", "p.rdydump"]);
    assert_eq!(v["canon"]["status"], "refused", "{v}");
    assert_eq!(v["canon"]["error"]["code"], "E_UPSTREAM_REFUSED", "{v}");
    assert_eq!(
        v["canon"]["error"]["message"],
        "the hub refused the request"
    );
    assert_eq!(hub1.canon(), None, "a refused restore moves nothing");
    let v = a.cli_ok(&["push", "--restore", "p.rdydump"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(v["holder"], false);
    let sent = hub1.of("/write").pop().unwrap().json();
    assert_eq!(sent["kind"], "restore");
    assert!(
        sent["gate"].is_object(),
        "a project dump sends its gate settings: {sent}"
    );
    assert_eq!(
        sent["render_inputs"]["title"], "Atlas",
        "and its render inputs"
    );
    assert_eq!(hub1.canon(), Some(base));
    assert_eq!(hub1.tags().get("v1.0.0"), Some(&base));
    hub1.fsck();
    // a holder dump of the hub restores into another hub repo, and sends no inputs
    let hdump = a.path().join("h.rdydump");
    hub1.dump_holder(&hdump);
    let hub2 = Hub::start();
    let d = Machine::new("d", &hub2);
    d.init();
    d.remote(&hub2.url());
    let v = d.cli_ok(&["push", "--restore", hdump.to_str().unwrap()]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(v["holder"], true, "{v}");
    assert_eq!(v["without_inputs"], json!(["v1.0.0"]), "{v}");
    let sent = hub2.of("/write").pop().unwrap().json();
    assert_eq!(sent["kind"], "restore");
    assert!(
        sent["gate"].is_null() && sent["render_inputs"].is_null(),
        "{sent}"
    );
    assert_eq!(hub2.canon(), Some(base));
    hub2.fsck();
    // a tag collision on a pull: B first pulls the restored canon, so it has a sync state; then
    // both machines tag v2.0.0, on different landings
    assert_eq!(b.cli_ok(&["pull"])["canon"]["status"], "pulled");
    let b1 = b.land_doc("beta", "Beta is landed.");
    tag_at(&b, "v2.0.0");
    let a1 = a.land_doc("alpha", "Alpha is landed.");
    tag_at(&a, "v2.0.0");
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(hub1.tags().get("v2.0.0"), Some(&a1));
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["fetched"]["tags_kept"], json!(["v2.0.0"]), "{v}");
    assert_eq!(v["fetched"]["tags_taken"], json!([]), "{v}");
    // the local tag still names B's own landing: it is never retargeted to a replay
    b.stop();
    let h = StoreHandle::open(b.path()).unwrap();
    let tags: Vec<(String, Hash)> = h
        .snapshot()
        .unwrap()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(_, t)| (t.name, t.landing))
        .collect();
    assert!(tags.contains(&("v2.0.0".to_string(), b1)), "{tags:?}");
    assert!(tags.contains(&("v1.0.0".to_string(), base)), "{tags:?}");
    h.fsck().expect("nothing is half written");
    h.close().unwrap();
    assert_eq!(
        v["canon"]["replayed"].as_array().map(Vec::len),
        Some(1),
        "{v}"
    );
    assert_eq!(
        history(&b).len(),
        3,
        "base, the hub's landing and the replayed one"
    );
    hub1.finish();
    hub2.finish();
}

/// scc10: a write whose reply is lost is uncertain, never failed: the next push or pull finds its
/// outcome by push id and sends no write again (D-H0BZN). A fetch cut mid-batch takes nothing, and
/// the next pull heals.
#[test]
fn scc10_lost_write_reply_and_fetch_crash_resume_safely() {
    let (hub, a, b, base) = pair();
    let a1 = a.land_doc("alpha", "Alpha is landed.");
    // the hub takes the write, and its reply never arrives
    hub.fault(Fault::LoseWriteReply);
    let e = a.cli_err(&["push"]);
    assert_eq!(hub.canon(), Some(a1), "the hub took the write");
    let writes = hub.of("/write");
    assert_eq!(writes.len(), 2, "the seed and this write");
    let push = writes[1].json()["push"].as_str().unwrap().to_string();
    assert_eq!(e["code"], "E_UPSTREAM_UNCERTAIN", "{e}");
    assert_eq!(
        e["message"],
        format!("the sync write of push {push} may have completed")
    );
    assert_eq!(
        e["fix"],
        format!(
            "run rdy push or rdy pull again: rdy finds the outcome of push {push} before it writes again"
        )
    );
    // the next push finds the outcome by push id and sends no write
    let v = a.cli_ok(&["push"]);
    assert_eq!(v["lookup"]["push"], push, "{v}");
    assert_eq!(v["lookup"]["result"]["outcome"], "taken");
    assert_eq!(v["lookup"]["result"]["tip"], a1.to_string());
    assert_eq!(v["canon"]["status"], "up_to_date", "{v}");
    assert_eq!(hub.of("/write").len(), 2, "no write was resent");
    assert!(
        hub.of("/outcome").iter().any(|r| r.json()["push"] == push),
        "the lookup names the push"
    );
    // the same on the pull path: the lost write of a second landing resolves in a pull
    let a2 = a.land_doc("gamma", "Gamma is landed.");
    hub.fault(Fault::LoseWriteReply);
    a.cli_err(&["push"]);
    assert_eq!(hub.canon(), Some(a2));
    let push2 = hub.of("/write").pop().unwrap().json()["push"]
        .as_str()
        .unwrap()
        .to_string();
    let v = a.cli_ok(&["pull"]);
    assert_eq!(v["lookup"]["push"], push2, "{v}");
    assert_eq!(v["lookup"]["result"]["outcome"], "taken", "{v}");
    assert_eq!(hub.of("/write").len(), 3, "the pull resent no write");
    // a fetch cut mid-batch: B takes nothing, and the next pull heals
    hub.fault(Fault::CutGet(100));
    let e = b.cli_err(&["pull"]);
    assert_eq!(e["code"], "E_UPSTREAM_TRANSPORT", "{e}");
    assert_eq!(b.canon(), Some(base), "a cut fetch moves nothing");
    assert_eq!(
        deltas_and_landings(&b),
        (1, 1),
        "and leaves the store whole"
    );
    let v = b.cli_ok(&["pull"]);
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    assert_eq!(history(&b), vec![base, a1, a2]);
    assert_eq!(deltas_and_landings(&b), (3, 3));
    hub.fsck();
    hub.finish();
}

/// The three clients of a daemon: the command line, a stdio MCP client and an HTTP MCP client.
/// Each call starts its client afresh, so a landing between two calls (which stops the daemon)
/// never leaves one holding a dead connection.
#[derive(Clone, Copy, Debug)]
enum Surface {
    Cli,
    Stdio,
    Http,
}

impl Surface {
    /// `rdy_push` (`push`) or `rdy_pull` (`pull`) with no arguments: (ok, the rdy body).
    fn call(self, m: &Machine, tool: &str) -> (bool, Value) {
        match self {
            Surface::Cli => m.cli(&[tool.strip_prefix("rdy_").unwrap()]),
            Surface::Stdio => m.chat().call(tool, json!({})),
            Surface::Http => Http::start(m).call(tool, json!({})),
        }
    }
}

/// What differs between runs and not between clients, taken out of a result: push ids, and each
/// hash replaced by the number of its first appearance in the run (a run lands its own
/// landings). An MCP error also holds an empty `issues` list, which is dropped. A reply of an MCP client also holds the `W_KUNGFU` warning of a session that has
/// not loaded the ruleset of the tool; it is dropped, and no other warning is.
fn normal(v: &Value, seen: &mut Vec<String>) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "push" | "request_id"))
                .map(|(k, x)| (k.clone(), normal(x, seen)))
                .filter(|(k, x)| !matches!(k.as_str(), "warnings" | "issues") || x != &json!([]))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .filter(|x| x["code"] != "W_KUNGFU")
                .map(|x| normal(x, seen))
                .collect(),
        ),
        Value::String(t) => {
            let words: Vec<String> = t
                .split(' ')
                .map(|w| {
                    if w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()) {
                        let at = seen.iter().position(|h| h == w).unwrap_or_else(|| {
                            seen.push(w.to_string());
                            seen.len() - 1
                        });
                        format!("<hash {at}>")
                    } else {
                        w.to_string()
                    }
                })
                .collect();
            Value::String(words.join(" "))
        }
        other => other.clone(),
    }
}

/// scc10: the same push, pull and refusal give the same results through the command line, the
/// stdio MCP client and the HTTP MCP client. One hub, one pair of machines and one base for each
/// client, so no run sees another's state.
#[test]
fn scc10_cli_stdio_and_http_mcp_share_results() {
    // the base of every pair: one landed project with no remote
    let seed_hub = Hub::start();
    let seed = Machine::new("seed", &seed_hub);
    seed.init();
    let base = seed.land_doc("base", "The base is landed.");
    let mut results: Vec<(Surface, Vec<(bool, Value)>)> = vec![];
    for surface in [Surface::Cli, Surface::Stdio, Surface::Http] {
        let hub = Hub::start();
        let p = copy_of("p", &seed, &hub);
        let q = copy_of("q", &seed, &hub);
        p.remote(&hub.url());
        q.remote(&hub.url());
        let a1 = p.land_doc("alpha", "Alpha is landed.");
        let mut seen = vec![];
        seen.push(surface.call(&p, "rdy_push")); // seeds the hub
        seen.push(surface.call(&q, "rdy_pull")); // q takes it
        assert_eq!(hub.canon(), Some(a1), "{surface:?}");
        assert_eq!(history(&q), vec![base, a1], "{surface:?}");
        let a2 = p.land_doc("gamma", "Gamma is landed.");
        seen.push(surface.call(&p, "rdy_push")); // extends the hub
        q.land_doc("beta", "Beta is landed.");
        seen.push(surface.call(&q, "rdy_push")); // q is diverged: a refusal
        seen.push(surface.call(&q, "rdy_push")); // and again: the same refusal
        seen.push(surface.call(&p, "rdy_pull")); // p is in step: nothing to take
        assert_eq!(hub.canon(), Some(a2), "{surface:?}");
        assert_eq!(
            hub.of("/write").len(),
            2,
            "{surface:?}: a refused push writes nothing"
        );
        hub.finish();
        results.push((surface, seen));
    }
    let (_, first) = &results[0];
    let oks: Vec<bool> = first.iter().map(|(ok, _)| *ok).collect();
    assert_eq!(oks, vec![true, true, true, false, false, true]);
    assert_eq!(first[3].1["code"], "E_DIVERGED", "{:?}", first[3]);
    let mut first_seen = vec![];
    let want: Vec<Value> = first
        .iter()
        .map(|(_, v)| normal(v, &mut first_seen))
        .collect();
    for (surface, seen) in &results[1..] {
        let mut hashes = vec![];
        for (i, ((ok, v), (ok0, _))) in seen.iter().zip(first).enumerate() {
            assert_eq!(ok, ok0, "{surface:?} step {i}");
            assert_eq!(normal(v, &mut hashes), want[i], "{surface:?} step {i}");
        }
    }
    seed_hub.finish();
}
