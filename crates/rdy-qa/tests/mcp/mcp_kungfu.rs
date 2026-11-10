//! kfu4 (FW-MH.kungfu, D-Y18E1): the server checks each call's `kungfu` value. It warns and counts,
//! and it never changes the action. Every behaviour test sends raw arguments: a client from
//! [`rdy_qa::mcp::McpClient::raw`], or JSON-RPC lines on a duplex stream to an in-process server.
//! Every person lives in an isolated home.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{
    self, Caller, Contribution, Layer, LoadRequest, PersonHost, REMINDER,
};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::kungfu;
use rdy_engine::kungfu_counts::{self, Case};
use rdy_engine::{Rdy, RdyError, ReadState};
use rdy_mcp::bootstrap::{ProjectlessServer, serve_projectless};
use rdy_mcp::claims::{self, Asked, Claim, Key, MAX_SETTINGS, Registry, SETTINGS_IDLE};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{DaemonBuildWarning, REQUEST_TOO_LARGE, RdyServer, serve, serve_on};
use rdy_mcp::tools::{self, McpState};
use rdy_model::Role;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{land, write};
use rdy_qa::instructions::{Set, World, author};
use rdy_qa::mcp::{DEADLINE, McpClient, Reply, fixture_value, supply_kungfu};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// A value no ruleset has, of the right form.
const STALE: &str = "kf-00000000";

const NOT_LOADED: &str = "This call has no kungfu value: you have not loaded this ruleset.";
const LOST: &str = "This call has no kungfu value: you may have lost your instructions.";
const WRONG: &str =
    "This call's kungfu value is not current: your instructions may be out of date.";

/// Every owner's load handle.
const OWNERS: [&str; 9] = [
    "rdy:bootstrap",
    "rdy:write",
    "rdy:after-write",
    "rdy:approvals",
    "rdy:read",
    "rdy:publish",
    "rdy:review",
    "rdy:parallel",
    "rdy:project-ops",
];

/// The warning a reply carries, written out here so that the test owns the words.
fn warning(message: &str, ruleset: &str) -> Value {
    let fix = if ruleset == "rdy:bootstrap" {
        "Call rdy_instructions and re-read it".to_string()
    } else {
        format!("Call rdy_instructions {{load: [\"{ruleset}\"]}} and re-read it before you go on.")
    };
    json!({"code": "W_KUNGFU", "message": message, "fix": fix, "ruleset": ruleset})
}

/// The `W_KUNGFU` entries of a reply's body.
fn advisories(body: &Value) -> Vec<Value> {
    body["warnings"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|w| w["code"] == "W_KUNGFU")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// `body` without the kungfu advisory: the warning, and a `warnings` list that holds nothing else.
/// An overview's `kungfu` count line goes too (kfu5): it follows each miss, and the action is not it.
fn without_warning(body: &Value) -> Value {
    let mut out = body.clone();
    if let Some(m) = out.as_object_mut() {
        m.remove("kungfu");
    }
    if let Some(all) = out.get_mut("warnings").and_then(Value::as_array_mut) {
        all.retain(|w| w["code"] != "W_KUNGFU");
        if all.is_empty() {
            out.as_object_mut().unwrap().remove("warnings");
        }
    }
    out
}

/// A reply's body, a refusal's too.
fn body(r: Reply) -> Value {
    match r {
        Reply::Ok(v) | Reply::ToolError(v) => v,
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    }
}

/// `args` with a value.
fn with(args: Value, value: impl Into<Value>) -> Value {
    let mut args = args;
    args["kungfu"] = value.into();
    args
}

/// `args` with this source's value for `tool`.
fn right(tool: &str, args: Value) -> Value {
    let mut args = args;
    supply_kungfu(tool, &mut args);
    assert!(args.get("kungfu").is_some(), "{tool} is exempt");
    args
}

/// A raw `rdy mcp --no-daemon` of world `w` on `dir`.
fn raw_client(w: &World, dir: &Path) -> McpClient {
    let env = w.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    McpClient::start_with_env(dir, &pairs).raw()
}

/// A raw client in session `actor`/`run`, as a writer.
fn in_session(w: &World, dir: &Path, actor: &str, run: &str) -> McpClient {
    let mut c = raw_client(w, dir);
    open(&mut c, actor, run);
    c
}

fn open(c: &mut McpClient, actor: &str, run: &str) {
    let r = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
}

/// Loads `handles`. A successful reply.
fn load(c: &mut McpClient, handles: &[&str]) {
    let r = c.call("rdy_instructions", json!({"load": handles}));
    assert!(r.is_ok(), "{r:?}");
}

/// The spec doc, landed in canon, with no process holding the store.
fn seeded(dir: &Path) -> Spec {
    let mut r = Rdy::open(dir).expect("open the project");
    let spec = spec_doc(&mut r);
    r.close().expect("close the project");
    spec
}

/// The count rows of the project at `dir`, as the file holds them now.
fn file_rows(dir: &Path) -> Vec<Value> {
    let path = dir
        .join(".rdy/run")
        .join(rdy_engine::kungfu_counts::FILE_NAME);
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let v: Value = serde_json::from_str(&text).expect("a count file");
    assert_eq!(v["schema"], 1);
    v["rows"].as_array().expect("rows").clone()
}

/// The `(not_loaded, lost, wrong)` sums of owner `name` in `rows`.
fn totals(rows: &[Value], name: &str) -> (u64, u64, u64) {
    let mut t = (0, 0, 0);
    for r in rows.iter().filter(|r| r["ruleset"] == name) {
        t.0 += r["not_loaded"].as_u64().unwrap();
        t.1 += r["lost"].as_u64().unwrap();
        t.2 += r["wrong"].as_u64().unwrap();
    }
    t
}

/// How a raw client's misses reach the file: one sentinel miss at a time. The worker flushes every
/// earlier count with or before a later one, so a file that shows the sentinel holds them all.
struct Drain {
    /// The `rdy:parallel` misses this client has made: its own calls and the sentinels.
    sent: u64,
}

impl Drain {
    /// A drain for a client that has made no `rdy:parallel` miss.
    fn new() -> Drain {
        Drain::after(0)
    }

    /// A drain for a client that has made `misses` `rdy:parallel` misses of its own. The
    /// sentinel is the next one: a total of the earlier ones alone does not stand for it.
    fn after(misses: u64) -> Drain {
        Drain { sent: misses }
    }

    /// The rows once every count of `c`'s earlier replies is in the file. The deadline is a hang
    /// guard: the loop waits for the sentinel and asserts nothing about the time it took.
    fn rows(&mut self, c: &mut McpClient, dir: &Path) -> Vec<Value> {
        // `rdy:parallel` is the sentinel's owner: a miss of it this drain was not told of would
        // stand for the sentinel before the sentinel's own count is in the file
        let (a, b, d) = totals(&file_rows(dir), "parallel");
        assert!(
            a + b + d <= self.sent,
            "the file has {} rdy:parallel misses, more than the {} this drain knows of",
            a + b + d,
            self.sent
        );
        self.sent += 1;
        body(c.call("rdy_view", json!({"op": "show"})));
        let start = Instant::now();
        loop {
            let rows = file_rows(dir);
            let (a, b, d) = totals(&rows, "parallel");
            if a + b + d >= self.sent {
                return rows;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the sentinel never reached the file"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

// ------------------------------------------------------------------- the in-process wire

/// A person host over an isolated home: nothing here reads the real person layer.
struct Home(PathBuf);

impl PersonHost for Home {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        resolve_person(
            Platform::HOST,
            &PathEnv {
                home: Some(self.0.clone().into()),
                // Windows places the default meta project from the profile, not HOME
                user_profile: Some(self.0.clone().into()),
                xdg_config_home: Some(self.0.join("cfg").into()),
                ..PathEnv::default()
            },
        )
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

/// An in-process project on `dir`, whose person layer lives in `home`.
fn local(dir: &Path, home: &Path) -> Arc<dyn Engine> {
    Arc::new(
        Local::open(dir)
            .expect("open the project")
            .with_person_host(Arc::new(Home(home.to_path_buf()))),
    )
}

/// One JSON-RPC connection on a duplex stream. It sends what it is given.
struct Wire {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Wire {
    async fn over(stream: DuplexStream) -> Wire {
        let (r, w) = tokio::io::split(stream);
        let mut c = Wire {
            r: BufReader::new(r),
            w,
            next: 0,
        };
        c.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "kfu4", "version": "0"}}}),
        )
        .await;
        let init = c.recv(json!(0)).await;
        assert!(init.get("result").is_some(), "{init}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    /// A connection to a project server on `hub`, under session id `sid`.
    async fn project(engine: &Arc<dyn Engine>, hub: &Arc<NotifyHub>, sid: u64) -> Wire {
        let (client, end) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(end);
        let server = RdyServer::new(Arc::clone(engine), Some(sid), Arc::clone(hub));
        tokio::spawn(async move {
            let _ = serve(server, r, w).await;
        });
        Wire::over(client).await
    }

    /// A connection to a projectless server started in `dir`.
    async fn projectless(dir: &Path, home: &Path, hub: &Arc<NotifyHub>) -> Wire {
        let (client, end) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(end);
        let server =
            ProjectlessServer::new(dir, Arc::new(Home(home.to_path_buf())), Arc::clone(hub));
        tokio::spawn(serve_projectless(server, r, w));
        Wire::over(client).await
    }

    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn recv(&mut self, id: Value) -> Value {
        loop {
            let mut line = String::new();
            let n = self.r.read_line(&mut line).await.unwrap();
            assert!(n > 0, "the session closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// One `tools/call`'s whole message.
    async fn message(&mut self, tool: &str, args: Value, meta: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args, "_meta": meta}}))
            .await;
        self.recv(json!(id)).await
    }

    /// `(isError, body)` of one `tools/call`.
    async fn call(&mut self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        let v = self.message(tool, args, meta).await;
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {v}"));
        (
            v["result"]["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn key(actor: &str, run: &str) -> Key {
    (actor.to_string(), Some(run.to_string()))
}

/// A project of its own, its person layer in a home of its own.
struct InProcess {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    engine: Arc<dyn Engine>,
}

fn in_process() -> InProcess {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let engine = local(dir.path(), home.path());
    InProcess {
        _dir: dir,
        _home: home,
        engine,
    }
}

// ------------------------------------------------------------------- the 60-tool matrix

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Correct,
    Absent,
    Wrong,
}

/// Masks what differs between two runs of one script: ids, hashes, times and the run's folders. A
/// list's order may follow its random ids, so lists compare sorted.
fn normalized(v: &Value, roots: &[String], masks: &[regex::Regex]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for r in roots {
                s = s.replace(r, "<root>");
            }
            for m in masks {
                s = m.replace_all(&s, "<x>").into_owned();
            }
            // a card lists its decisions in the order of their random ids
            if s.contains("<x>") || s.len() > 200 {
                let mut words: Vec<&str> = s
                    .split_whitespace()
                    .map(|w| w.trim_matches(|c: char| ",;.\"".contains(c)))
                    .collect();
                words.sort_unstable();
                s = words.join(" ");
            }
            Value::String(s)
        }
        Value::Number(n) if n.as_u64().is_some_and(|n| n > 1_000_000_000) => json!("<big>"),
        Value::Array(a) => {
            let mut a: Vec<Value> = a.iter().map(|x| normalized(x, roots, masks)).collect();
            a.sort_by_key(|x| x.to_string());
            Value::Array(a)
        }
        // a map may be keyed by random ids: its masked keys compare sorted, numbered when equal
        Value::Object(m) => {
            let mut rows: Vec<(String, Value)> = m
                .iter()
                .map(|(k, x)| {
                    let Value::String(k) = normalized(&json!(k), roots, masks) else {
                        unreachable!()
                    };
                    // a file's size on disk follows the file system's blocks
                    if k.contains("bytes") && x.is_number() {
                        return (k, json!("<bytes>"));
                    }
                    (k, normalized(x, roots, masks))
                })
                .collect();
            rows.sort_by_key(|(k, x)| (k.clone(), x.to_string()));
            let mut out = serde_json::Map::new();
            for (k, x) in rows {
                let n = out
                    .keys()
                    .filter(|o| o.split('#').next() == Some(&k))
                    .count();
                out.insert(if n == 0 { k } else { format!("{k}#{n}") }, x);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// One run of the script: a raw client that sends each tool's value as `mode` says.
struct Script {
    c: McpClient,
    mode: Mode,
    masks: Vec<regex::Regex>,
    roots: Vec<String>,
    called: BTreeSet<String>,
    /// `(tool, isError, code, the reply and the state after it)`, normalized, by step.
    out: Vec<(String, bool, Value, Value)>,
    /// Owners whose first call in this run has gone out.
    warned: BTreeSet<String>,
    /// Calls per owner name.
    calls: BTreeMap<String, u64>,
}

impl Script {
    fn new(w: &World, dir: &Path, scratch: &Path, mode: Mode) -> Script {
        // the daemon's proxy: every tool has its real route there
        let c = w.mcp(dir).raw();
        let roots = [dir, scratch]
            .iter()
            .flat_map(|p| {
                let real = p.canonicalize().unwrap();
                [
                    p.to_string_lossy().into_owned(),
                    real.to_string_lossy().into_owned(),
                ]
            })
            .chain([w.config_dir().to_string_lossy().into_owned()])
            .collect();
        let masks = [
            // a world's temporary folders, and the project's name, which comes from one
            r"(?i)[^\s\x22']*rdyd[0-9a-z]+",
            // a block's, a landing's or a proposal's id
            r"\b[A-Za-z]{1,3}-[0-9A-Z]{5}\b",
            // hashes, tokens and the hex of a proposal's ref
            r"[0-9a-f]{8,}",
            // a time
            r"\d{4}-\d{2}-\d{2}[T ][0-9:.]+Z?",
            // a size, which a compressed or random part of a file may move
            r"\b\d{5,}\b",
        ]
        .iter()
        .map(|m| regex::Regex::new(m).unwrap())
        .collect();
        Script {
            c,
            mode,
            masks,
            roots,
            called: BTreeSet::new(),
            out: vec![],
            warned: BTreeSet::new(),
            calls: BTreeMap::new(),
        }
    }

    fn norm(&self, v: &Value) -> Value {
        normalized(v, &self.roots, &self.masks)
    }

    /// The canon log, the session view and the project's tree and views: the effects of a call.
    fn effects(&mut self) -> Value {
        let mut all = vec![];
        for (tool, args) in [
            ("rdy_log", json!({"limit": 1000, "max_tokens": 100_000})),
            ("rdy_view", json!({"op": "show", "max_tokens": 100_000})),
            ("rdy_overview", json!({"max_tokens": 100_000})),
        ] {
            let v = body(self.c.call(tool, right(tool, args)));
            all.push(without_warning(&v));
        }
        self.norm(&Value::Array(all))
    }

    /// One call of `tool`: its reply, with the kungfu advisory removed.
    fn step(&mut self, tool: &str, args: Value) -> Value {
        let mut args = args;
        let owner = fixture_value(tool).map(|_| {
            kungfu::policy(tool)
                .unwrap()
                .ruleset
                .unwrap()
                .strip_prefix("rdy:")
                .unwrap()
                .to_string()
        });
        match self.mode {
            Mode::Correct => supply_kungfu(tool, &mut args),
            Mode::Absent => {}
            Mode::Wrong => args["kungfu"] = json!(STALE),
        }
        self.called.insert(tool.to_string());
        let reply = self.c.call(tool, args);
        let (is_err, v) = match reply {
            Reply::Ok(v) => (false, v),
            Reply::ToolError(v) => (true, v),
            Reply::RpcError(e) => panic!("{tool}: a protocol error: {e}"),
        };
        let carried = advisories(&v);
        match (self.mode, &owner) {
            (Mode::Correct, _) | (_, None) => {
                assert!(carried.is_empty(), "{tool} warned: {v}")
            }
            (_, Some(name)) => {
                *self.calls.entry(name.clone()).or_default() += 1;
                // the first object reply of each owner carries it. The effects below read with
                // correct values, which rearm their owners, so a later reply may carry it too;
                // kfu4_warning_rearms_after_correct_or_reload pins the once-per-owner rule.
                assert!(carried.len() <= 1, "{tool}: {v}");
                if v.is_object() && self.warned.insert(name.clone()) {
                    assert_eq!(carried.len(), 1, "{tool}: {v}");
                }
            }
        }
        let clean = without_warning(&v);
        let effects = self.effects();
        let code = clean.get("code").cloned().unwrap_or(Value::Null);
        let norm = self.norm(&clean);
        self.out
            .push((tool.to_string(), is_err, code, json!([norm, effects])));
        clean
    }
}

/// The script: every tool once at least, on the seeded spec, with each step's reply and effects
/// compared. External services are refused before any network, and those refusals are compared
/// too. Each step succeeds unless [`REFUSED`] names it.
fn script(x: &mut Script, spec: &Spec, scratch: &Path) {
    let s = |p: &str| scratch.join(p).to_string_lossy().into_owned();
    let (a, c, section, decision) = (
        spec.a.to_string(),
        spec.c.to_string(),
        spec.section.to_string(),
        spec.decision.to_string(),
    );
    x.step(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    );
    x.step("rdy_instructions", json!({"full": true}));
    x.step("rdy_overview", json!({}));
    x.step("rdy_schema", json!({}));
    x.step("rdy_read", json!({"id": "spec", "scope": "canon"}));
    x.step("rdy_context", json!({"id": a, "scope": "canon"}));
    x.step("rdy_search", json!({"query": "holds", "scope": "canon"}));
    x.step(
        "rdy_query",
        json!({"filter": "type = decision", "scope": "canon"}),
    );
    let brief = x.step("rdy_brief", json!({"path": "spec"}));
    x.step("rdy_suspects", json!({"scope": "canon"}));
    x.step("rdy_tensions", json!({"scope": "canon"}));
    x.step("rdy_pending", json!({}));
    x.step("rdy_site", json!({"scope": "canon"}));

    let token = brief["base"]["token"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    x.step(
        "rdy_write",
        json!({"ops": [{"op": "update", "id": a, "body": "A holds, by m."}],
            "brief": token, "message": "m edits A"}),
    );
    let made = x.step(
        "rdy_decide",
        json!({"statement": "Cache the reads", "title": "Cache the reads",
            "why": "A person decides how reads are cached", "parent": section}),
    );
    let made = made["created"]["$decision"]
        .as_str()
        .unwrap_or("none")
        .to_string();
    x.step(
        "rdy_supersede",
        json!({"old": decision, "decision": {"statement": "Keep A always", "title": "Keep A always",
            "why": "A person keeps A", "parent": section}}),
    );
    x.step(
        "rdy_revoke",
        json!({"target": made, "reason": "the reads stay uncached", "parent": section}),
    );
    x.step(
        "rdy_accept",
        json!({"suggestions": [{"kind": "entity", "literal": "Fee", "name": "Fee", "type": "term",
            "body": "Fee is what a payment costs."}], "parent": section}),
    );
    x.step(
        "rdy_intent",
        json!({"op": "post", "blocks": [c], "angle": "fees", "ttl": "30m"}),
    );
    x.step("rdy_asset", json!({"data_base64": "aGVsbG8="}));
    x.step("rdy_plan", json!({}));
    let proposed = x.step(
        "rdy_propose",
        json!({"title": "Fix A", "summary": "Rewords A."}),
    );
    let pref = proposed["ref"].as_str().unwrap_or("none").to_string();
    x.step(
        "rdy_resolve",
        json!({"tension": "none", "dismiss": {"reason": "checked"}}),
    );
    let queue = x.step("rdy_pending", json!({"max_tokens": 100_000}));
    let (item, card_hash) = queue["items"]
        .as_array()
        .and_then(|all| all.first())
        .map(|i| {
            (
                i["ref"].as_str().unwrap().to_string(),
                i["card_hash"].as_str().unwrap().to_string(),
            )
        })
        .unwrap_or(("none".into(), "none".into()));
    x.step(
        "rdy_answer",
        json!({"ref": item, "option": "later", "card_hash": card_hash,
            "evidence": "Not yet.", "question": "Land it?", "session_ref": "chat-1"}),
    );
    x.step("rdy_land", json!({"proposal": pref}));
    x.step("rdy_dispute", json!({"op": "list"}));
    x.step(
        "rdy_tag",
        json!({"version": "0.0.1", "evidence": "Tag it.", "question": "Tag?"}),
    );

    x.step("rdy_trace", json!({"id": a, "scope": "canon"}));
    x.step("rdy_diff", json!({"from": "canon"}));
    let log = x.step("rdy_log", json!({"limit": 5}));
    let landing = log[0]["landing"].as_str().unwrap_or("none").to_string();
    x.step("rdy_impact", json!({"id": a, "scope": "canon"}));
    x.step("rdy_inbox", json!({}));
    x.step("rdy_coverage", json!({"scope": "canon"}));
    x.step("rdy_tags", json!({}));
    x.step("rdy_links", json!({}));

    x.step(
        "rdy_publication_check",
        json!({"profile": "implicit:org", "scope": "canon"}),
    );
    x.step(
        "rdy_publish",
        json!({"tag": "none", "to": "none", "site": "none"}),
    );
    x.step("rdy_render", json!({"out": s("render"), "scope": "canon"}));
    x.step(
        "rdy_export",
        json!({"out": s("export"), "format": "md", "scope": "canon"}),
    );
    x.step("rdy_remote", json!({"op": "list"}));
    // a credential store differs by host: this refusal comes before any store or hub
    x.step("rdy_auth", json!({"op": "state"}));
    x.step("rdy_key_show", json!({}));
    x.step("rdy_key_bind", json!({"kind": "agent", "remote": "none"}));
    x.step("rdy_key_retire", json!({"key": "none", "remote": "none"}));
    x.step("rdy_key_revoke", json!({"key": "none", "remote": "none"}));

    x.step("rdy_review", json!({"op": "list"}));
    // the reviewer's tools need that role, in the same run
    x.step(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "reviewer"}),
    );
    x.step("rdy_review_next", json!({"kind": "tension"}));
    x.step(
        "rdy_review_submit",
        json!({"request": "none", "case_hash": "none", "verdict": "pass"}),
    );
    x.step(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    );

    x.step("rdy_view", json!({"op": "create", "name": "fork-a"}));
    x.step("rdy_alternatives", json!({"op": "list"}));
    // rtc5: a rule to remove that does not exist, so the call changes nothing and is the same
    // refusal whatever its value
    x.step(
        "rdy_rule",
        json!({"scope": "project", "op": "remove", "id": "ai-00000", "why": "No longer wanted.",
            "evidence": "Drop it.", "session_ref": "chat-1"}),
    );

    x.step("rdy_dump", json!({"out": s("dump.rdy")}));
    x.step("rdy_stream_check", json!({"file": s("dump.rdy")}));
    x.step(
        "rdy_load",
        json!({"file": s("dump.rdy"), "dir": s("loaded")}),
    );
    x.step("rdy_clone", json!({"dir": s("cloned")}));
    import_tree(&scratch.join("tree"));
    x.step("rdy_import", json!({"dir": s("tree"), "view": "imported"}));
    x.step(
        "rdy_redact",
        json!({"targets": [c], "reason_class": "secret", "reason": "a leaked code", "dry_run": true}),
    );
    x.step("rdy_sign", json!({"target": landing}));
    x.step("rdy_upgrade", json!({"dry_run": true}));
    x.step("rdy_compact", json!({"dry_run": true}));
    // last: a policy changes what later acts need
    x.step("rdy_policy", json!({"op": "init", "doc": "spec"}));
}

/// A format-3 export tree at `out`, for `rdy_import`.
fn import_tree(out: &Path) {
    std::fs::create_dir_all(out.join("docs")).unwrap();
    std::fs::write(
        out.join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: work\n",
    )
    .unwrap();
    std::fs::write(
        out.join("docs/imported.yaml"),
        "- id: d-QM001\n  type: doc\n  order: a1\n  fields:\n    doc_type: overview\n    key: imported\n    title: Imported\n",
    )
    .unwrap();
    std::fs::write(
        out.join("authors.yaml"),
        "- id: d-QM001\n  actor: agent:a\n  run: r1\n  role: writer\n  at_ms: 100\n",
    )
    .unwrap();
}

/// The steps whose call the script cannot complete, with each one's own refusal, in script order.
/// Each needs what this world does not hold: a tension, a person's answer, a hub, a credential store,
/// a key, a review request, a stream or an SSH agent. Every other step succeeds.
const REFUSED: &[(&str, &str)] = &[
    ("rdy_resolve", "E_NOT_FOUND"),
    ("rdy_land", "E_EXCEPTION_OPEN"),
    ("rdy_publish", "E_BAD_TAG_NAME"),
    ("rdy_auth", "E_BAD_INPUT"),
    ("rdy_key_bind", "E_NOT_FOUND"),
    ("rdy_key_retire", "E_BAD_INPUT"),
    ("rdy_key_revoke", "E_BAD_INPUT"),
    ("rdy_review_submit", "E_BAD_INPUT"),
    ("rdy_rule", "E_NOT_FOUND"),
    ("rdy_stream_check", "E_BAD_RECORD"),
    ("rdy_sign", "E_PERMISSION"),
];

/// One full run in a fresh world: its replies and effects by step, and its count rows.
struct Ran {
    out: Vec<(String, bool, Value, Value)>,
    called: BTreeSet<String>,
    calls: BTreeMap<String, u64>,
    rows: Vec<Value>,
}

fn run_script(mode: Mode) -> Ran {
    let w = World::new();
    let pd = w.project();
    let spec = seeded(pd.path());
    let scratch = tempfile::tempdir().unwrap();
    let mut x = Script::new(&w, pd.path(), scratch.path(), mode);
    script(&mut x, &spec, scratch.path());
    // the script's own `rdy:parallel` misses come before the sentinel's
    let mut drain = Drain::after(x.calls.get("parallel").copied().unwrap_or(0));
    let rows = drain.rows(&mut x.c, pd.path());
    Ran {
        out: std::mem::take(&mut x.out),
        called: std::mem::take(&mut x.called),
        calls: std::mem::take(&mut x.calls),
        rows,
    }
}

/// The first place `a` and `b` differ, as a path and the two values there.
fn first_difference(a: &Value, b: &Value, at: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| {
                let null = Value::Null;
                let (p, q) = (x.get(k).unwrap_or(&null), y.get(k).unwrap_or(&null));
                first_difference(p, q, &format!("{at}/{k}"))
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (p, q))| first_difference(p, q, &format!("{at}/{i}"))),
        _ => (a != b).then(|| format!("{at}: {a} != {b}")),
    }
}

/// Every step of `b` gives what the same step of `a` gave.
fn same_steps(a: &Ran, b: &Ran, what: &str) {
    assert_eq!(a.out.len(), b.out.len(), "{what}");
    let mut differences = vec![];
    for (x, y) in a.out.iter().zip(&b.out) {
        assert_eq!((&x.0, x.1, &x.2), (&y.0, y.1, &y.2), "{what}");
        if let Some(d) = first_difference(&x.3, &y.3, "") {
            differences.push(format!("{}: {d}", x.0));
        }
    }
    assert!(differences.is_empty(), "{what}: {differences:#?}");
}

#[test]
fn kfu4_each_tool_keeps_its_action_for_all_values() {
    let correct = run_script(Mode::Correct);
    let absent = run_script(Mode::Absent);
    let wrong = run_script(Mode::Wrong);

    // the matrix accounts for every router name
    let names: BTreeSet<String> = kungfu::TOOLS.iter().map(|p| p.tool.to_string()).collect();
    assert_eq!(names.len(), 60);
    assert_eq!(correct.called, names);

    same_steps(&correct, &absent, "absent");
    same_steps(&correct, &wrong, "wrong");

    // each step's outcome is the script's own: a success, or the refusal REFUSED names
    let refused: Vec<(&str, &str)> = correct
        .out
        .iter()
        .filter(|(_, is_err, _, _)| *is_err)
        .map(|(tool, _, code, _)| (tool.as_str(), code.as_str().unwrap()))
        .collect();
    assert_eq!(refused, REFUSED);

    // the counts: nothing for a correct value, each call for a miss, under its own case
    for (ran, case) in [(&absent, 1), (&wrong, 2)] {
        for (name, n) in &ran.calls {
            // the drain's sentinel owns `parallel`
            if name == "parallel" {
                continue;
            }
            let t = totals(&ran.rows, name);
            let got = [t.0, t.1, t.2];
            let mut want = [0, 0, 0];
            want[case] = *n;
            assert_eq!(got, want, "{name}");
        }
    }
    for o in OWNERS {
        let name = o.strip_prefix("rdy:").unwrap();
        if name != "parallel" {
            assert_eq!(totals(&correct.rows, name), (0, 0, 0), "{name}");
        }
    }
}

// ------------------------------------------------------------------- the three cases

#[test]
fn kfu4_three_cases_have_exact_messages() {
    let w = World::new();
    let pd = w.project();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");

    // never received
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");

    // received, then missing
    load(&mut c, &["rdy:read"]);
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(LOST, "rdy:read")], "{v}");

    // wrong: a stale value, another owner's, and values that are not strings
    let values = [
        json!(STALE),
        json!(fixture_value("rdy_write").unwrap()),
        json!(""),
        json!("x".repeat(100_000)),
        Value::Null,
        json!(1),
        json!(true),
        json!([fixture_value("rdy_tags").unwrap()]),
        json!({"kungfu": fixture_value("rdy_tags").unwrap()}),
    ];
    for value in values {
        load(&mut c, &["rdy:read"]);
        let v = body(c.call("rdy_tags", with(json!({}), value.clone())));
        assert_eq!(advisories(&v), [warning(WRONG, "rdy:read")], "{value}: {v}");
        // the reply never shows the supplied or the expected value
        let text = v.to_string();
        assert!(!text.contains("kf-"), "{text}");
    }

    // the right value: no warning, and the reply never shows it
    load(&mut c, &["rdy:read"]);
    let v = body(c.call("rdy_tags", right("rdy_tags", json!({}))));
    assert_eq!(v.get("warnings"), None, "{v}");

    // a bootstrap tool's fix names no ruleset to load
    let v = body(c.call("rdy_search", json!({"query": "x"})));
    assert_eq!(
        advisories(&v),
        [warning(NOT_LOADED, "rdy:bootstrap")].map(|_| { warning(LOST, "rdy:bootstrap") }),
        "{v}"
    );
    assert_eq!(
        advisories(&v)[0]["fix"],
        "Call rdy_instructions and re-read it"
    );

    // a nested key of the same name is no value, and the tool's own check refuses it
    load(&mut c, &["rdy:write"]);
    let r = c.call(
        "rdy_view",
        right(
            "rdy_view",
            json!({"op": "show", "owner": {"actor": "agent:k", "kungfu": STALE}}),
        ),
    );
    assert_eq!(r.code(), Some("E_BAD_INPUT"), "{r:?}");
}

#[test]
fn kfu4_warning_rearms_after_correct_or_reload() {
    let w = World::new();
    let pd = w.project();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    let mut drain = Drain::new();

    // once per owner, across its tools and across its cases; every miss counts
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")]);
    let v = body(c.call("rdy_inbox", json!({})));
    assert!(advisories(&v).is_empty(), "{v}");
    let v = body(c.call("rdy_coverage", with(json!({}), STALE)));
    assert!(advisories(&v).is_empty(), "{v}");
    // another owner has its own mark
    let v = body(c.call("rdy_review", json!({"op": "list"})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:review")]);

    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "read"), (2, 0, 1));
    assert_eq!(totals(&rows, "review"), (1, 0, 0));

    // a correct value rearms that owner only
    let v = body(c.call("rdy_tags", right("rdy_tags", json!({}))));
    assert_eq!(v.get("warnings"), None, "{v}");
    let v = body(c.call("rdy_inbox", with(json!({}), STALE)));
    assert_eq!(advisories(&v), [warning(WRONG, "rdy:read")]);
    let v = body(c.call("rdy_review", json!({"op": "list"})));
    assert!(advisories(&v).is_empty(), "{v}");

    // a reload rearms what it delivered; a load of another owner does not
    load(&mut c, &["rdy:write"]);
    let v = body(c.call("rdy_tags", json!({})));
    assert!(advisories(&v).is_empty(), "{v}");
    load(&mut c, &["rdy:read"]);
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(LOST, "rdy:read")]);

    // a failed reload does not
    let r = c.call("rdy_instructions", json!({"load": ["rdy:nowhere"]}));
    assert!(!r.is_ok(), "{r:?}");
    let v = body(c.call("rdy_tags", json!({})));
    assert!(advisories(&v).is_empty(), "{v}");

    // a correct value clears the mark but delivers nothing: this owner stays not loaded
    let v = body(c.call("rdy_review", right("rdy_review", json!({"op": "list"}))));
    assert_eq!(v.get("warnings"), None, "{v}");
    let v = body(c.call("rdy_review", json!({"op": "list"})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:review")]);

    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "read"), (3, 2, 2));
    assert_eq!(totals(&rows, "review"), (3, 0, 0));
}

#[tokio::test]
async fn kfu4_receipt_follows_complete_rows() {
    let world = in_process();
    let hub = NotifyHub::new();
    let mut c = Wire::project(&world.engine, &hub, 1).await;
    let meta = claim("agent:r", "r1");
    let bit = |name: &str| rdy_mcp::kungfu::bit_of(name).unwrap();
    let received = |hub: &NotifyHub, k: &Key| hub.claims().get(k).map_or(0, |s| s.kungfu_received);
    let all: u16 = OWNERS
        .iter()
        .fold(0, |n, o| n | bit(o.strip_prefix("rdy:").unwrap()));
    let k = key("agent:r", "r1");

    // nothing yet
    assert_eq!(received(&hub, &k), 0);
    // a failed load marks nothing, and the reminder stays owed
    let (err, _) = c
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:nowhere"]}),
            meta.clone(),
        )
        .await;
    assert!(err);
    assert_eq!(received(&hub, &k), 0);
    let (_, v) = c.call("rdy_read", json!({"id": "x"}), meta.clone()).await;
    assert_eq!(v["reminder"], REMINDER, "{v}");

    // bootstrap alone: the menu names the others, and that is no delivery
    let (err, v) = c.call("rdy_instructions", json!({}), meta.clone()).await;
    assert!(!err, "{v}");
    assert_eq!(v["menu"].as_array().unwrap().len(), 12, "{v}");
    assert_eq!(received(&hub, &k), bit("bootstrap"));
    // a load adds its owner, and clears both marks
    let (_, v) = c.call("rdy_trace", json!({"id": "x"}), meta.clone()).await;
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
    let (_, v) = c
        .call("rdy_search", json!({"query": "x"}), meta.clone())
        .await;
    assert_eq!(advisories(&v), [warning(LOST, "rdy:bootstrap")], "{v}");
    let (err, _) = c
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:write"]}),
            meta.clone(),
        )
        .await;
    assert!(!err);
    assert_eq!(received(&hub, &k), bit("bootstrap") | bit("write"));
    let saved = hub.claims().get(&k).unwrap();
    assert_eq!(saved.kungfu_warned & bit("bootstrap"), 0);
    assert_ne!(saved.kungfu_warned & bit("read"), 0);
    // sets that own no tool add no bit
    let (err, _) = c
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:as-built", "rdy:read"]}),
            meta.clone(),
        )
        .await;
    assert!(!err);
    assert_eq!(
        received(&hub, &k),
        bit("bootstrap") | bit("write") | bit("read")
    );
    // full: all nine, and no mark stays
    let (err, _) = c
        .call("rdy_instructions", json!({"full": true}), meta.clone())
        .await;
    assert!(!err);
    let saved = hub.claims().get(&k).unwrap();
    assert_eq!(saved.kungfu_received, all);
    assert_eq!(saved.kungfu_warned, 0);
    assert_eq!(all.count_ones(), 9);

    // the projectless server, in its own registry
    let outside = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let bare = NotifyHub::new();
    let mut p = Wire::projectless(outside.path(), home.path(), &bare).await;
    let meta = claim("agent:p", "p1");
    let k = key("agent:p", "p1");
    let (err, _) = p
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:nowhere"]}),
            meta.clone(),
        )
        .await;
    assert!(err);
    assert_eq!(received(&bare, &k), 0);
    let (err, _) = p
        .call("rdy_instructions", with(json!({}), STALE), meta.clone())
        .await;
    assert!(!err);
    assert_eq!(received(&bare, &k), bit("bootstrap"));
    let (err, _) = p
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:review"]}),
            meta.clone(),
        )
        .await;
    assert!(!err);
    assert_eq!(received(&bare, &k), bit("bootstrap") | bit("review"));
    let (err, _) = p
        .call("rdy_instructions", json!({"full": true}), meta.clone())
        .await;
    assert!(!err);
    assert_eq!(received(&bare, &k), all);

    // a reply past its bound marks nothing, on either server: the bound is checked first
    let many: Vec<String> = (0..1025).map(|i| format!("d-{i:05}")).collect();
    let other = claim("agent:q", "q1");
    let (err, v) = p
        .call(
            "rdy_instructions",
            json!({"full": true, "load": many}),
            other,
        )
        .await;
    assert!(err, "{v}");
    assert_eq!(received(&bare, &key("agent:q", "q1")), 0);
}

/// Five required rulesets of 4 MiB each: every set passes its own bound, and the reply does not.
fn big_sets() -> Vec<Set<'static>> {
    let text: &'static str = Box::leak("x".repeat(4 * 1024 * 1024).into_boxed_str());
    let rules: &'static [(&'static [&'static str], &'static str)] =
        Box::leak(vec![(&["all"][..], text)].into_boxed_slice());
    ["big0", "big1", "big2", "big3", "big4"]
        .iter()
        .map(|name| Set {
            name,
            title: name,
            required: true,
            lead: None,
            rules,
        })
        .collect()
}

/// A reply past its bound: the check runs once the rows are assembled, so it marks nothing.
#[test]
fn kfu4_a_failed_bounded_reply_marks_nothing() {
    // the project server
    let w = World::new();
    let pd = w.project();
    author(pd.path(), "Large rules stay in one place", &big_sets());
    let mut c = in_session(&w, pd.path(), "agent:b", "b1");
    let r = c.call("rdy_instructions", json!({}));
    assert_eq!(r.code(), Some("E_LIMIT"), "{r:?}");
    let v = body(c.call("rdy_read", json!({"id": "x"})));
    assert_eq!(v["reminder"], REMINDER, "the reminder stays owed: {v}");
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");

    // the projectless server, with the same rules in the person's meta project
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let meta = home.path().join(".rdy").join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    drop(Rdy::init(&meta, "rdy-standard").unwrap());
    author(&meta, "Large rules stay in one place", &big_sets());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let hub = NotifyHub::new();
        let mut p = Wire::projectless(outside.path(), home.path(), &hub).await;
        let (err, v) = p
            .call("rdy_instructions", json!({}), claim("agent:p", "p1"))
            .await;
        assert!(err, "{v}");
        assert_eq!(v["code"], "E_LIMIT", "{v}");
        assert!(
            hub.claims()
                .get(&key("agent:p", "p1"))
                .is_none_or(|s| s.kungfu_received == 0 && !s.instructions_read),
            "nothing marked"
        );
    });
}

#[tokio::test]
async fn kfu4_settings_survive_reconnect_and_expire_on_restart() {
    let world = in_process();
    let start = Instant::now();
    let at = Arc::new(Mutex::new(start));
    let clock = Arc::clone(&at);
    let hub = NotifyHub::with_claims(Registry::with_clock(Arc::new(move || {
        *clock.lock().unwrap()
    })));
    let meta = claim("agent:a", "r1");

    let mut one = Wire::project(&world.engine, &hub, 1).await;
    let (_, v) = one.call("rdy_tags", json!({}), meta.clone()).await;
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
    // the reconnect shares the registry: the mark stays
    drop(one);
    let mut two = Wire::project(&world.engine, &hub, 2).await;
    let (_, v) = two.call("rdy_inbox", json!({}), meta.clone()).await;
    assert!(advisories(&v).is_empty(), "{v}");
    // a repeated session keeps both sets
    let session = json!({"actor": "agent:a", "run": "r1", "role": "reader", "view": "canon"});
    let (err, v) = two.call("rdy_session", session.clone(), json!({})).await;
    assert!(!err, "{v}");
    let (_, v) = two.call("rdy_inbox", json!({}), json!({})).await;
    assert!(advisories(&v).is_empty(), "{v}");
    // a reload's receipt survives the reconnect too
    let (err, _) = two
        .call(
            "rdy_instructions",
            json!({"load": ["rdy:review"]}),
            json!({}),
        )
        .await;
    assert!(!err);
    drop(two);
    let mut three = Wire::project(&world.engine, &hub, 3).await;
    let (err, v) = three.call("rdy_session", session, json!({})).await;
    assert!(!err, "{v}");
    let (_, v) = three
        .call("rdy_review", json!({"op": "list"}), json!({}))
        .await;
    assert_eq!(advisories(&v), [warning(LOST, "rdy:review")], "{v}");

    // another actor and another run have their own
    for (actor, run) in [("agent:b", "r1"), ("agent:a", "r2")] {
        let mut c = Wire::project(&world.engine, &hub, 4).await;
        let (_, v) = c.call("rdy_tags", json!({}), claim(actor, run)).await;
        assert_eq!(
            advisories(&v),
            [warning(NOT_LOADED, "rdy:read")],
            "{actor} {run}: {v}"
        );
    }

    // an hour idle clears the entry: both sets go with it
    *at.lock().unwrap() = start + SETTINGS_IDLE;
    let mut late = Wire::project(&world.engine, &hub, 5).await;
    let (_, v) = late
        .call("rdy_review", json!({"op": "list"}), meta.clone())
        .await;
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:review")], "{v}");

    // a restarted server has a registry of its own
    let fresh = NotifyHub::new();
    let mut back = Wire::project(&world.engine, &fresh, 6).await;
    let (_, v) = back
        .call("rdy_review", json!({"op": "list"}), meta.clone())
        .await;
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:review")], "{v}");
}

/// The real daemon: a reconnect shares its registry, and its restart clears it.
#[test]
fn kfu4_a_daemon_restart_brings_the_warning_back() {
    let w = World::new();
    let pd = w.project();
    let mut a = w.mcp(pd.path()).raw();
    open(&mut a, "agent:d", "d1");
    let v = body(a.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
    let pid = w.daemon_pid(pd.path()).expect("a daemon");
    drop(a);
    let mut b = w.mcp(pd.path()).raw();
    open(&mut b, "agent:d", "d1");
    assert_eq!(w.daemon_pid(pd.path()), Some(pid), "the same daemon");
    let v = body(b.call("rdy_tags", json!({})));
    assert!(advisories(&v).is_empty(), "{v}");
    drop(b);
    w.stop(pd.path());
    let mut c = w.mcp(pd.path()).raw();
    open(&mut c, "agent:d", "d1");
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
}

#[test]
fn kfu4_kungfu_marks_cannot_exhaust_functional_settings() {
    let k = |n: usize| (format!("agent:a{n:04}"), None::<String>);
    let bit = rdy_mcp::kungfu::bit_of("read").unwrap();
    let full = || {
        let r = Registry::new();
        for n in 0..MAX_SETTINGS {
            assert!(r.mark_kungfu_warned(&k(n), bit));
        }
        assert_eq!(r.len(), MAX_SETTINGS);
        r
    };
    let new = ("agent:new".to_string(), None::<String>);

    // a new mark over the cap is skipped, and no entry grows
    let r = full();
    assert!(
        r.mark_kungfu_warned(&new, bit),
        "the warning still goes out"
    );
    assert_eq!(r.len(), MAX_SETTINGS);
    assert!(r.get(&new).is_none());

    // each admission reclaims the first kungfu-only key in map order, and no other
    let r = full();
    r.check_room(&new).unwrap();
    assert!(r.get(&k(0)).is_none());
    assert!(r.get(&k(1)).is_some());
    assert_eq!(r.len(), MAX_SETTINGS - 1);
    r.save(&new, "v".into(), Role::Writer, None).unwrap();
    assert_eq!(r.len(), MAX_SETTINGS);

    let r = full();
    r.save(&new, "v".into(), Role::Writer, Some(true)).unwrap();
    assert_eq!(r.len(), MAX_SETTINGS);
    assert!(r.get(&k(0)).is_none());
    assert_eq!(r.get(&new).unwrap().role, Some(Role::Writer));

    let r = full();
    r.save_dedupe(&new, true, || "v".into()).unwrap();
    assert_eq!(r.len(), MAX_SETTINGS);
    assert!(r.get(&k(0)).is_none());
    assert!(r.get(&new).unwrap().dedupe);

    // the same key is promoted, not reclaimed, and keeps both sets: dedupe true and false
    for dedupe in [true, false] {
        let r = full();
        r.mark_kungfu_received(&k(7), bit);
        r.mark_kungfu_warned(&k(7), rdy_mcp::kungfu::bit_of("write").unwrap());
        r.save_dedupe(&k(7), dedupe, || "seven".into()).unwrap();
        assert_eq!(r.len(), MAX_SETTINGS);
        let s = r.get(&k(7)).unwrap();
        assert_eq!(s.view.as_deref(), Some("seven"));
        assert_eq!(s.role, Some(Role::Writer));
        assert_eq!(s.dedupe, dedupe);
        assert_eq!(s.kungfu_received, bit);
        assert_eq!(s.kungfu_warned, rdy_mcp::kungfu::bit_of("write").unwrap());
        assert!(r.get(&k(0)).is_some(), "the promotion reclaims nothing");
        // a promoted entry is functional now
        r.check_room(&new).unwrap();
        r.save(&k(7), "again".into(), Role::Reader, None).unwrap();
        let s = r.get(&k(7)).unwrap();
        assert_eq!(s.view.as_deref(), Some("again"));
        assert_eq!(s.kungfu_received, bit, "a save keeps both sets");
    }

    // mixed entries: functional state stays; only kungfu-only entries go, in map order, and
    // admissions interleave with new marks
    let r = Registry::new();
    for n in 0..MAX_SETTINGS {
        if n % 2 == 0 {
            r.save(&k(n), format!("v{n}"), Role::Reader, None).unwrap();
        } else {
            r.mark_kungfu_warned(&k(n), bit);
        }
    }
    r.save(&("agent:x1".into(), None), "x".into(), Role::Writer, None)
        .unwrap();
    assert!(r.get(&k(1)).is_none());
    assert!(r.get(&k(3)).is_some());
    assert!(r.get(&k(0)).is_some());
    assert!(r.mark_kungfu_warned(&("agent:y".into(), None), bit));
    assert!(
        r.get(&("agent:y".to_string(), None)).is_none(),
        "a new mark never evicts"
    );
    r.save_dedupe(&("agent:x2".into(), None), false, || "x".into())
        .unwrap();
    assert!(r.get(&k(3)).is_none());
    assert!(r.get(&k(5)).is_some());
    assert_eq!(r.len(), MAX_SETTINGS);
    for n in (0..MAX_SETTINGS).step_by(2) {
        assert_eq!(
            r.get(&k(n)).unwrap().view,
            Some(format!("v{n}")),
            "functional state stays"
        );
    }

    // entries with a build mark or a read mark are functional
    let r = Registry::new();
    for n in 0..MAX_SETTINGS {
        match n % 3 {
            0 => {
                r.take_build_warning(&k(n));
            }
            1 => r.mark_instructions_read(&k(n)),
            _ => r.save(&k(n), "v".into(), Role::Writer, None).unwrap(),
        }
        r.mark_kungfu_warned(&k(n), bit);
    }
    // the full registry keeps its refusal, on all three paths
    let limit = "saved session settings exceed 1024 sessions";
    let e = r.check_room(&new).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("E_LIMIT", limit));
    let e = r.save(&new, "v".into(), Role::Writer, None).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("E_LIMIT", limit));
    let e = r.save_dedupe(&new, true, || "v".into()).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("E_LIMIT", limit));
    assert_eq!(r.len(), MAX_SETTINGS);
    assert!(r.get(&new).is_none());

    // through the real doors: a new rdy_session, then a new `_meta` dedupe save
    let dir = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let engine: Arc<dyn Engine> = Arc::new(Local::new(rdy));
    let hub = NotifyHub::with_claims(full());
    let mut st = McpState::new(Arc::clone(&engine));
    st.hub = Arc::clone(&hub);
    let v = tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:new", "run": "n1", "role": "writer"}))
            .unwrap(),
    );
    assert!(v.is_ok(), "{v:?}");
    let mut other = McpState::new(Arc::clone(&engine));
    other.hub = Arc::clone(&hub);
    let asked = Asked::Claim(Claim {
        actor: "agent:meta".into(),
        run: Some("m1".into()),
        role: None,
        dedupe: Some(true),
    });
    let out = claims::call(&mut other, Some(&asked), "rdy_tags", |_| Ok(json!({})));
    assert!(out.is_ok(), "{out:?}");
    assert!(hub.claims().get(&key("agent:meta", "m1")).unwrap().dedupe);
    assert_eq!(hub.claims().len(), MAX_SETTINGS);
}

// ------------------------------------------------------------------- the reminder

#[test]
fn kfu4_reminder_replaces_only_bootstrap_warning() {
    let w = World::new();
    let pd = w.project();
    let spec = seeded(pd.path());
    let a = spec.a.to_string();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    let mut drain = Drain::new();

    // every bootstrap owner, while the reminder is due: the reminder and no warning
    let calls = [
        ("rdy_overview", json!({})),
        ("rdy_schema", json!({})),
        ("rdy_read", json!({"id": "spec", "scope": "canon"})),
        ("rdy_context", json!({"id": a, "scope": "canon"})),
        ("rdy_search", json!({"query": "holds", "scope": "canon"})),
        (
            "rdy_query",
            json!({"filter": "type = claim", "scope": "canon"}),
        ),
        ("rdy_brief", json!({"path": "spec", "scope": "canon"})),
        ("rdy_suspects", json!({"scope": "canon"})),
        ("rdy_tensions", json!({"scope": "canon"})),
        ("rdy_pending", json!({})),
        ("rdy_site", json!({"scope": "canon"})),
    ];
    let owned: BTreeSet<&str> = kungfu::TOOLS
        .iter()
        .filter(|p| p.ruleset == Some("rdy:bootstrap"))
        .map(|p| p.tool)
        .collect();
    assert_eq!(
        calls.iter().map(|(t, _)| *t).collect::<BTreeSet<_>>(),
        owned,
        "every bootstrap owner"
    );
    for (tool, args) in &calls {
        for args in [args.clone(), with(args.clone(), STALE)] {
            let v = body(c.call(tool, args));
            assert_eq!(v["reminder"], REMINDER, "{tool}: {v}");
            assert!(advisories(&v).is_empty(), "{tool}: {v}");
        }
    }
    // an ordinary refusal carries it too, and keeps its own fields
    let bad = json!({"id": "d-NONE1", "scope": "canon"});
    let r = c.call("rdy_read", right("rdy_read", bad.clone()));
    let plain = body(r);
    assert!(plain.get("reminder").is_none(), "{plain}");
    let r = c.call("rdy_read", bad);
    assert!(matches!(r, Reply::ToolError(_)), "{r:?}");
    let v = body(r);
    assert_eq!(v["reminder"], REMINDER, "{v}");
    assert_eq!(v["code"], plain["code"]);
    assert_eq!(v["message"], plain["message"]);
    assert_eq!(v["fix"], plain["fix"]);
    assert!(advisories(&v).is_empty(), "{v}");

    // a correct value: overview and brief as before; no other tool gains it
    for (tool, args) in &calls {
        let v = body(c.call(tool, right(tool, args.clone())));
        assert!(advisories(&v).is_empty(), "{tool}: {v}");
        if *tool == "rdy_overview" || *tool == "rdy_brief" {
            assert_eq!(v["reminder"], REMINDER, "{tool}: {v}");
        } else {
            assert!(v.get("reminder").is_none(), "{tool}: {v}");
        }
    }

    // a tool of another owner warns while the reminder is due, and gains no reminder
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
    assert!(v.get("reminder").is_none(), "{v}");

    // the misses counted, and no warning was marked as sent
    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "bootstrap"), (12, 0, 11));

    // once the instructions go out, the first miss warns as lost
    load(&mut c, &[]);
    let v = body(c.call("rdy_read", json!({"id": "spec", "scope": "canon"})));
    assert!(v.get("reminder").is_none(), "{v}");
    assert_eq!(advisories(&v), [warning(LOST, "rdy:bootstrap")], "{v}");
    let v = body(c.call("rdy_overview", json!({})));
    assert!(v.get("reminder").is_none(), "{v}");
    assert!(advisories(&v).is_empty(), "{v}");
}

// ------------------------------------------------------------------- early refusals

/// A state read before a content removal: every call is refused at the door.
struct Stale {
    inner: Local,
    retained: Arc<ReadState>,
}

impl Engine for Stale {
    fn read(&self) -> Arc<ReadState> {
        Arc::clone(&self.retained)
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, RdyError> {
        self.inner.write(f)
    }
    fn open_report(&self) -> rdy_engine::OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

#[tokio::test]
async fn kfu4_claim_and_early_refusals_stay_unchanged() {
    let world = in_process();
    let hub = NotifyHub::new();
    let values = [None, Some(json!(STALE)), Some(Value::Null)];
    let args = |v: &Option<Value>| match v {
        Some(v) => with(json!({}), v.clone()),
        None => json!({}),
    };

    // no session: the refusal is the claim's, and it names no value
    let mut c = Wire::project(&world.engine, &hub, 1).await;
    for v in &values {
        let (err, got) = c.call("rdy_tags", args(v), json!({})).await;
        assert!(err);
        assert_eq!(
            got,
            json!({"code": "E_NO_SESSION", "message": "no session",
                "fix": "state rdy/actor and rdy/run in _meta, or call rdy_session", "issues": []})
        );
    }
    // a claim that is not valid
    for v in &values {
        let (err, got) = c
            .call("rdy_tags", args(v), json!({"rdy/actor": "agent:a"}))
            .await;
        assert!(err);
        assert_eq!(
            got["message"],
            "a _meta claim needs both rdy/actor and rdy/run"
        );
        assert!(got.get("warnings").is_none(), "{got}");
    }
    // metadata has no value key
    let (err, got) = c
        .call("rdy_tags", json!({}), json!({"rdy/kungfu": STALE}))
        .await;
    assert!(err);
    assert_eq!(got["message"], "_meta: unknown parameter \"rdy/kungfu\"");
    // a mismatched claim, on a bound connection
    let (err, got) = c
        .call(
            "rdy_tags",
            right("rdy_tags", json!({})),
            claim("agent:a", "r1"),
        )
        .await;
    assert!(!err, "{got}");
    for v in &values {
        let (err, got) = c.call("rdy_tags", args(v), claim("agent:b", "r1")).await;
        assert!(err);
        assert_eq!(
            got["message"],
            "this stdio connection is bound to another actor or run"
        );
        assert!(got.get("warnings").is_none(), "{got}");
    }
    assert!(hub.claims().get(&key("agent:b", "r1")).is_none());

    // an unknown tool and the oversized sentinel
    let mut unknown = vec![];
    for v in &values {
        unknown.push(c.message("rdy_nowhere", args(v), json!({})).await);
    }
    assert!(unknown[0].get("error").is_some(), "{:?}", unknown[0]);
    let mut strip_id = unknown.clone();
    for m in &mut strip_id {
        m.as_object_mut().unwrap().remove("id");
    }
    assert_eq!(strip_id[0], strip_id[1]);
    assert_eq!(strip_id[0], strip_id[2]);
    for v in &values {
        let (err, got) = c.call(REQUEST_TOO_LARGE, args(v), json!({})).await;
        assert!(err);
        assert_eq!(got["code"], "E_LIMIT");
        assert!(got.get("warnings").is_none(), "{got}");
    }

    // an epoch refusal comes before everything
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "gone",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "gone", "title": "Gone", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A secret."}
        ]),
    )
    .unwrap();
    land(&mut r, "gone").unwrap();
    let retained = r.read();
    let rq = rdy_qa::tensions::ag("rq");
    r.redact_in(
        &rq,
        "rq",
        rdy_engine::redaction::RedactInput {
            targets: vec![w.created["$p"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let proposal = r
        .propose(&rq, "rq", "Remove it", Some("Removes a code."))
        .unwrap();
    r.land(&rdy_qa::fixtures::approver(), &proposal.id).unwrap();
    r.seal_open().unwrap();
    let stale: Arc<dyn Engine> = Arc::new(Stale {
        inner: Local::new(r),
        retained,
    });
    let bare = NotifyHub::new();
    let mut e = Wire::project(&stale, &bare, 1).await;
    for v in &values {
        let (err, got) = e.call("rdy_tags", args(v), claim("agent:a", "r1")).await;
        assert!(err);
        assert_eq!(got["code"], "E_REDACTED_STATE");
        assert!(
            got.get("warnings").is_none() && got.get("reminder").is_none(),
            "{got}"
        );
    }
    assert!(bare.claims().get(&key("agent:a", "r1")).is_none());

    // the projectless server: its unavailable tools are E_NO_PROJECT and carry nothing
    let outside = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let phub = NotifyHub::new();
    let mut p = Wire::projectless(outside.path(), home.path(), &phub).await;
    for v in &values {
        for tool in ["rdy_tags", "rdy_read", "rdy_nowhere"] {
            let (err, got) = p.call(tool, args(v), claim("agent:p", "p1")).await;
            assert!(err, "{tool}");
            assert_eq!(got["code"], "E_NO_PROJECT", "{tool}");
            assert!(got.get("warnings").is_none(), "{got}");
        }
    }
    assert!(phub.claims().get(&key("agent:p", "p1")).is_none());
    // its exempt tools take the argument and discard it
    let (err, got) = p
        .call(
            "rdy_session",
            with(
                json!({"actor": "agent:p", "run": "p1", "role": "reader"}),
                STALE,
            ),
            json!({}),
        )
        .await;
    assert!(!err, "{got}");
    assert!(got.get("warnings").is_none(), "{got}");

    // the exempt project tools take it too and count nothing
    let (err, got) = c
        .call(
            "rdy_session",
            with(json!({"actor": "agent:a", "run": "r1"}), 1),
            json!({}),
        )
        .await;
    assert!(!err, "{got}");
    assert!(got.get("warnings").is_none_or(|w| w == &json!([])), "{got}");
}

#[test]
fn kfu4_an_oversized_line_stays_the_limit_refusal() {
    let w = World::new();
    let pd = w.project();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    let huge = "x".repeat(tools::MAX_REQUEST_BYTES + 1);
    let id = c.next_id();
    c.send_raw(
        &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "rdy_tags", "arguments": {"kungfu": huge}}})
        .to_string(),
    );
    let v = c.recv(id, DEADLINE);
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    let got: Value = serde_json::from_str(text).unwrap();
    assert_eq!(got["code"], "E_LIMIT");
    assert!(got.get("warnings").is_none(), "{got}");
    let mut drain = Drain::new();
    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "read"), (0, 0, 0));
}

// ------------------------------------------------------------------- errors and budgets

/// An engine whose pack generation moves when a test says so.
struct Noticing {
    inner: Local,
    generation: std::sync::atomic::AtomicU64,
}

impl Noticing {
    fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Engine for Noticing {
    fn read(&self) -> Arc<ReadState> {
        self.inner.read()
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, RdyError> {
        self.inner.write(f)
    }
    fn open_report(&self) -> rdy_engine::OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
    fn pack_generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn kfu4_warning_preserves_errors_budgets_and_other_warnings() {
    // the refusal with a corrected payload: a card that moved
    let (dir, r, batch) = rdy_qa::intake::batch_project();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let engine: Arc<dyn Engine> =
        Arc::new(Local::new(r).with_person_host(Arc::new(Home(home.path().to_path_buf()))));
    let hub = NotifyHub::new();
    let mut c = Wire::project(&engine, &hub, 1).await;
    let meta = claim("agent:relay", "r1");
    let moved = "0".repeat(64);
    let stale = json!({"ref": batch.r#ref, "option": "approve", "card_hash": moved,
        "evidence": "Yes.", "question": "Approve?", "session_ref": "chat-1"});
    let clean = c
        .message(
            "rdy_answer",
            right("rdy_answer", stale.clone()),
            meta.clone(),
        )
        .await;
    assert_eq!(clean["result"]["isError"], json!(true), "{clean}");
    let (err, missing) = c.call("rdy_answer", stale, meta.clone()).await;
    assert!(err, "the refusal stays one");
    let plain: Value =
        serde_json::from_str(clean["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(plain.get("corrected").is_some(), "{plain}");
    assert_eq!(advisories(&missing), [warning(NOT_LOADED, "rdy:approvals")]);
    assert_eq!(
        without_warning(&missing),
        plain,
        "only the advisory differs"
    );

    // the build warning stays first, in its own words
    let dw = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(dw.path(), "rdy-standard").unwrap();
    let eng: Arc<dyn Engine> = Arc::new(Local::new(rdy));
    let build = DaemonBuildWarning {
        version: "1.0.0".into(),
        daemon_build: "aaa".into(),
        client_build: "bbb".into(),
    };
    let phub = NotifyHub::new();
    let (client, end) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(end);
    tokio::spawn(serve_on(
        Arc::clone(&eng),
        9,
        Arc::clone(&phub),
        Arc::new(rdy_engine::signing_crypto::UnavailableAgent),
        Some(build.clone()),
        sr,
        sw,
    ));
    let mut b = Wire::over(client).await;
    let (_, v) = b.call("rdy_tags", json!({}), claim("agent:b", "b1")).await;
    assert_eq!(
        v["warnings"],
        json!([build.to_json(), warning(NOT_LOADED, "rdy:read")]),
        "{v}"
    );
    // on a refusal too
    let (err, v) = b
        .call(
            "rdy_review",
            json!({"op": "list", "nope": 1}),
            claim("agent:b", "b1"),
        )
        .await;
    assert!(err);
    assert_eq!(v["code"], "E_BAD_INPUT");
    assert_eq!(v["warnings"], json!([warning(NOT_LOADED, "rdy:review")]));

    // warnings a tool made itself stay first, strings and objects alike
    let plan = json!({"ops": [{"op": "create", "type": "doc", "fields": {"name": "w", "title": "W", "doc_type": "page"}}]});
    let (err, got) = b.call("rdy_write", plan, json!({})).await;
    assert!(!err, "{got}");
    let all = got["warnings"].as_array().unwrap();
    assert_eq!(
        all.last().unwrap(),
        &warning(NOT_LOADED, "rdy:write"),
        "{got}"
    );

    // a budget too small for the advisory: it comes whole, and the action's reply is as it was
    for budget in [1, 5, 40] {
        let cw = tempfile::tempdir().unwrap();
        let rdy = Rdy::init(cw.path(), "rdy-standard").unwrap();
        let tiny: Arc<dyn Engine> = Arc::new(Local::new(rdy));
        let thub = NotifyHub::new();
        let mut t = Wire::project(&tiny, &thub, 1).await;
        let meta = claim("agent:t", "t1");
        let args = json!({"scope": "canon", "max_tokens": budget});
        let a = t
            .call(
                "rdy_coverage",
                right("rdy_coverage", args.clone()),
                meta.clone(),
            )
            .await;
        let z = t.call("rdy_coverage", args, meta.clone()).await;
        assert!(!a.0 && !z.0, "{a:?} {z:?}");
        assert_eq!(
            advisories(&z.1),
            [warning(NOT_LOADED, "rdy:read")],
            "{budget}"
        );
        assert_eq!(without_warning(&z.1), a.1, "{budget}");
    }

    // a pack notice rides on an ordinary reply beside the warning, and on no closed reply
    let cw = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(cw.path(), "rdy-standard").unwrap();
    let noticing = Arc::new(Noticing {
        inner: Local::new(rdy),
        generation: Default::default(),
    });
    let engine: Arc<dyn Engine> = noticing.clone();
    let nhub = NotifyHub::new();
    let mut n = Wire::project(&engine, &nhub, 1).await;
    let meta = claim("agent:n", "n1");
    noticing.bump();
    let check = json!({"profile": "implicit:org", "scope": "canon"});
    let (err, v) = n
        .call("rdy_publication_check", check.clone(), meta.clone())
        .await;
    assert!(!err, "{v}");
    assert!(v.get("notice").is_none(), "{v}");
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:publish")]);
    let (err, v) = n.call("rdy_tags", json!({}), meta.clone()).await;
    assert!(!err, "{v}");
    assert!(
        v["notice"].is_string(),
        "the notice waited for this reply: {v}"
    );
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")]);

    // `run_closed`: the closed report gains the warning, and no pack notice
    let cw = tempfile::tempdir().unwrap();
    let rdy = Rdy::init(cw.path(), "rdy-standard").unwrap();
    let closed: Arc<dyn Engine> = Arc::new(Local::new(rdy));
    let chub = NotifyHub::new();
    let mut t = Wire::project(&closed, &chub, 1).await;
    let meta = claim("agent:t", "t1");
    let check = json!({"profile": "implicit:org", "scope": "canon"});
    let a = t
        .call(
            "rdy_publication_check",
            right("rdy_publication_check", check.clone()),
            meta.clone(),
        )
        .await;
    let z = t.call("rdy_publication_check", check, meta.clone()).await;
    assert!(!a.0 && !z.0, "{a:?} {z:?}");
    assert_eq!(advisories(&z.1), [warning(NOT_LOADED, "rdy:publish")]);
    assert_eq!(without_warning(&z.1), without_warning(&a.1));
}

/// A top-level array has no place for a warning: the reply stays as it is, the miss counts, and
/// the warning stays due for the next object reply of that owner.
#[test]
fn kfu4_array_reply_keeps_the_warning_due() {
    let w = World::new();
    let pd = w.project();
    seeded(pd.path());
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    let args = json!({"limit": 2});
    let plain = body(c.call("rdy_log", right("rdy_log", args.clone())));
    assert!(plain.is_array(), "{plain}");
    let v = body(c.call("rdy_log", args.clone()));
    assert_eq!(v, plain, "the action's reply stays unchanged");
    let v = body(c.call("rdy_log", with(args, STALE)));
    assert_eq!(v, plain);
    // the next object reply of the owner carries it, as that reply's case
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
    let v = body(c.call("rdy_tags", json!({})));
    assert!(advisories(&v).is_empty(), "{v}");
    let mut drain = Drain::new();
    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "read"), (3, 0, 1));
}

/// What one page of a cut read is, its token left out.
fn page_of(v: &Value) -> Value {
    json!({
        "text": v["text"],
        "cut": v["cut"],
        "truncated": v["truncated"],
        "omitted": v["omitted"],
        "next": v["next"].as_str().map(|n| n.rsplit(':').next().unwrap().to_string()),
    })
}

#[test]
fn kfu4_warning_keeps_page_boundaries() {
    let w = World::new();
    let pd = w.project();
    {
        let mut r = Rdy::open(pd.path()).expect("open");
        let mut ops = vec![json!({"op": "create", "type": "doc", "id": "$d",
            "fields": {"name": "many", "title": "Many", "doc_type": "page"}})];
        for n in 0..60 {
            ops.push(json!({"op": "create", "type": "prose", "parent": "$d",
                "body": format!("Block number {n} says a few plain words.")}));
        }
        write(&mut r, "seed", Value::Array(ops)).expect("the doc");
        land(&mut r, "seed").expect("land");
        r.close().expect("close");
    }
    let read = |budget: u64| json!({"id": "many", "scope": "canon", "max_tokens": budget});

    // a budget where the next whole block fits, and would not fit after an advisory's reservation
    let mut c = in_session(&w, pd.path(), "agent:p", "p1");
    load(&mut c, &[]);
    let lines = |v: &Value| v["text"].as_str().unwrap().lines().count();
    let budget = (200..4000)
        .find(|b| {
            let at = body(c.call("rdy_read", right("rdy_read", read(*b))));
            let less = body(c.call("rdy_read", right("rdy_read", read(*b - 128))));
            at["truncated"] == json!(true) && lines(&at) > lines(&less)
        })
        .expect("a budget at a block's edge");

    // first and next pages, for a correct value and for a due warning: equal session states
    let first = body(c.call("rdy_read", right("rdy_read", read(budget))));
    let cursor = first["next"].as_str().unwrap().to_string();
    let next_args = json!({"page": cursor, "max_tokens": budget});
    let second = body(c.call("rdy_read", right("rdy_read", next_args.clone())));
    for value in [None, Some(json!(STALE)), Some(Value::Null)] {
        let shaped = |args: Value| match &value {
            Some(x) => with(args, x.clone()),
            None => args,
        };
        load(&mut c, &[]);
        let v = body(c.call("rdy_read", shaped(read(budget))));
        assert_eq!(advisories(&v).len(), 1, "{v}");
        assert_eq!(without_warning(&v), first, "the first page: {value:?}");
        load(&mut c, &[]);
        let v = body(c.call("rdy_read", shaped(next_args.clone())));
        assert_eq!(advisories(&v).len(), 1, "{v}");
        assert_eq!(without_warning(&v), second, "the next page: {value:?}");
    }
    drop(c);

    // the replacement reminder keeps them too: a session that owes it, on a run of its own
    let mut d = in_session(&w, pd.path(), "agent:q", "q1");
    let v = body(d.call("rdy_read", read(budget)));
    assert_eq!(v["reminder"], REMINDER, "{v}");
    assert_eq!(page_of(&v), page_of(&first));
    let cursor = v["next"].as_str().unwrap().to_string();
    let v = body(d.call("rdy_read", json!({"page": cursor, "max_tokens": budget})));
    assert_eq!(v["reminder"], REMINDER, "{v}");
    assert_eq!(page_of(&v), page_of(&second));
    drop(d);

    // every other tool that takes max_tokens
    for budget in [20, 60, 200, 800] {
        let correct = budget_run(Mode::Correct, budget);
        for mode in [Mode::Absent, Mode::Wrong] {
            let other = budget_run(mode, budget);
            assert_eq!(correct.called, other.called);
            assert_eq!(correct.out.len(), other.out.len());
            for (x, y) in correct.out.iter().zip(&other.out) {
                if let Some(d) = first_difference(&x.1, &y.1, "") {
                    panic!("{mode:?} at {budget}: {}: {d}", x.0);
                }
            }
        }
        // the run covers them all
        let all: BTreeSet<String> = correct.budgeted.iter().cloned().collect();
        let missed: Vec<&String> = all.difference(&correct.called).collect();
        assert!(missed.is_empty(), "{missed:?}");
        assert!(all.len() > 20, "{all:?}");
    }
}

/// An engine whose writes run on seeded ids and a pinned clock, by their order: two runs of one
/// script list and cut their rows alike, so a difference can only come from the check.
struct Seeded {
    inner: Local,
    writes: std::sync::atomic::AtomicU64,
}

impl Engine for Seeded {
    fn read(&self) -> Arc<ReadState> {
        self.inner.read()
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, RdyError> {
        let n = self
            .writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        rdy_model::seed_ids_for_tests(0x4B46_0040 + n);
        let _clock = rdy_engine::pin_clock(1_770_000_000_000 + (n as i64) * 1000);
        let out = self.inner.write(f);
        rdy_model::unseed_ids_for_tests();
        out
    }
    fn read_serialized(&self, f: rdy_engine::engine::ReadFn) -> Result<Value, RdyError> {
        self.inner.read_serialized(f)
    }
    fn republish(&self) -> Result<(), RdyError> {
        self.inner.republish()
    }
    fn open_report(&self) -> rdy_engine::OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

struct BudgetRun {
    called: BTreeSet<String>,
    budgeted: BTreeSet<String>,
    out: Vec<(String, Value)>,
}

/// One run of the budgeted tools, each at `budget`, in a project with seeded ids.
fn budget_run(mode: Mode, budget: u64) -> BudgetRun {
    // the seeded project, on this thread
    rdy_model::seed_ids_for_tests(0x4B46_0004);
    let clock = rdy_engine::pin_clock(1_760_000_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let spec = spec_doc(&mut r);
    rdy_model::unseed_ids_for_tests();
    drop(clock);
    let engine: Arc<dyn Engine> = Arc::new(Seeded {
        inner: Local::new(r),
        writes: Default::default(),
    });
    // the project's name comes from its temporary folder
    let root = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let hub = NotifyHub::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut c = Wire::project(&engine, &hub, 1).await;
        let meta = claim("agent:m", "m1");
        c.next = 1;
        // the tools that take max_tokens
        let id = json!(0);
        c.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}}))
            .await;
        let budgeted: BTreeSet<String> = c.recv(json!(0)).await["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["inputSchema"]["properties"].get("max_tokens").is_some())
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        let (a, section) = (spec.a.to_string(), spec.section.to_string());
        let steps = [
            ("rdy_overview", json!({})),
            ("rdy_suspects", json!({"scope": "canon"})),
            ("rdy_search", json!({"query": "holds", "scope": "canon"})),
            (
                "rdy_query",
                json!({"filter": "type = claim", "scope": "canon"}),
            ),
            ("rdy_read", json!({"id": "spec", "scope": "canon"})),
            ("rdy_context", json!({"id": a, "scope": "canon"})),
            ("rdy_brief", json!({"path": "spec", "scope": "canon"})),
            ("rdy_tensions", json!({"scope": "canon"})),
            ("rdy_pending", json!({})),
            ("rdy_trace", json!({"id": a, "scope": "canon"})),
            ("rdy_diff", json!({"from": "canon"})),
            ("rdy_log", json!({"limit": 5})),
            ("rdy_impact", json!({"id": a, "scope": "canon"})),
            ("rdy_inbox", json!({})),
            ("rdy_coverage", json!({"scope": "canon"})),
            ("rdy_alternatives", json!({"op": "list"})),
            (
                "rdy_intent",
                json!({"op": "post", "blocks": [a], "angle": "x", "ttl": "30m"}),
            ),
            (
                "rdy_write",
                json!({"ops": [{"op": "create", "type": "claim", "parent": section,
                    "body": "A new claim."}]}),
            ),
            (
                "rdy_decide",
                json!({"statement": "Cache the reads", "title": "Cache the reads",
                    "why": "A person decides", "parent": section}),
            ),
            (
                "rdy_revoke",
                json!({"target": spec.decision.to_string(), "reason": "it no longer holds",
                    "parent": section}),
            ),
            (
                "rdy_accept",
                json!({"suggestions": [{"kind": "entity", "literal": "Fee", "name": "Fee",
                    "type": "term", "body": "Fee is what a payment costs."}], "parent": section}),
            ),
            (
                "rdy_redact",
                json!({"targets": [spec.c.to_string()], "reason_class": "secret",
                    "reason": "a leaked code", "dry_run": true}),
            ),
            ("rdy_view", json!({"op": "show"})),
            ("rdy_plan", json!({})),
            (
                "rdy_propose",
                json!({"title": "A change", "summary": "Adds a claim."}),
            ),
        ];
        let mut out = vec![];
        let mut called = BTreeSet::new();
        for (tool, args) in steps {
            let mut args = args;
            args["max_tokens"] = json!(budget);
            match mode {
                Mode::Correct => supply_kungfu(tool, &mut args),
                Mode::Absent => {}
                Mode::Wrong => args["kungfu"] = json!(STALE),
            }
            called.insert(tool.to_string());
            let (is_err, mut v) = c.call(tool, args, meta.clone()).await;
            // bootstrap misses carry the reminder in place of the warning
            if let Some(m) = v.as_object_mut() {
                m.remove("reminder");
            }
            let text = without_warning(&v).to_string().replace(&root, "<root>");
            out.push((tool.to_string(), json!([is_err, text])));
        }
        BudgetRun {
            called,
            budgeted,
            out,
        }
    })
}

#[test]
fn kfu4_advisory_stays_within_its_bound() {
    // every message, every owner, alone and beside other warnings; and the reminder
    let cases = [Case::NotLoaded, Case::Lost, Case::Wrong];
    let owners = kungfu::owners();
    assert_eq!(owners.len(), 9);
    for owner in owners {
        for case in cases {
            let w = rdy_mcp::kungfu::warning(owner, case);
            let alone = format!(",\"warnings\":[{w}]");
            let beside = format!(",{w}");
            assert!(alone.len() <= 512, "{alone}");
            assert!(beside.len() <= 512, "{beside}");
        }
    }
    let reminder = format!(",\"{}\":{}", instructions::REMINDER_KEY, json!(REMINDER));
    assert!(reminder.len() <= 512, "{reminder}");

    // on real replies: the body grows by the advisory alone
    let w = World::new();
    let pd = w.project();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    load(&mut c, &["rdy:read"]);
    let clean = body(c.call("rdy_tags", right("rdy_tags", json!({})))).to_string();
    let warned = body(c.call("rdy_tags", json!({}))).to_string();
    assert!(warned.len() > clean.len());
    assert!(warned.len() - clean.len() <= 512, "{warned}");
}

// ------------------------------------------------------------------- the worker and the client

#[test]
fn kfu4_count_io_never_blocks_a_tool_result() {
    let w = World::new();
    let pd = w.project();
    let mut c = in_session(&w, pd.path(), "agent:k", "k1");
    let mut drain = Drain::new();
    // one flush makes the private directory and the lock file
    body(c.call("rdy_tags", json!({})));
    let before = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&before, "read"), (1, 0, 0));

    // this process holds the real file lock: the worker cannot flush
    let lock = std::fs::File::open(pd.path().join(".rdy/run").join(kungfu_counts::LOCK_NAME))
        .expect("the lock file");
    // the worker may be finishing the flush the sentinel reached the file by
    let start = Instant::now();
    while lock.try_lock().is_err() {
        assert!(
            start.elapsed() < DEADLINE,
            "the worker never let the lock go"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // a read and a mutation both complete, and their misses wait
    let v = body(c.call("rdy_tags", json!({})));
    assert!(v.get("versions").is_some(), "{v}");
    let v = body(c.call(
        "rdy_write",
        json!({"ops": [{"op": "create", "type": "doc",
            "fields": {"name": "held", "title": "Held", "doc_type": "page"}}]}),
    ));
    assert!(v.get("change").is_some(), "{v}");
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:write")]);
    assert_eq!(totals(&file_rows(pd.path()), "write"), (0, 0, 0));

    // released, they reach the file
    lock.unlock().unwrap();
    drop(lock);
    let rows = drain.rows(&mut c, pd.path());
    assert_eq!(totals(&rows, "read"), (2, 0, 0));
    assert_eq!(totals(&rows, "write"), (1, 0, 0));
}

#[test]
fn kfu4_qa_values_are_explicitly_bypassable() {
    let w = World::new();
    let pd = w.project();
    let env = w.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    // the helper: only an owned tool, only an object, only an absent key
    let mut a = json!({});
    supply_kungfu("rdy_tags", &mut a);
    assert_eq!(a["kungfu"], fixture_value("rdy_tags").unwrap());
    let mut a = json!({});
    supply_kungfu("rdy_session", &mut a);
    assert_eq!(a, json!({}));
    let mut a = json!({"kungfu": null});
    supply_kungfu("rdy_tags", &mut a);
    assert_eq!(a, json!({"kungfu": null}));
    let mut a = json!({"kungfu": STALE});
    supply_kungfu("rdy_tags", &mut a);
    assert_eq!(a, json!({"kungfu": STALE}));
    let mut a = json!([]);
    supply_kungfu("rdy_tags", &mut a);
    assert_eq!(a, json!([]));
    assert_eq!(fixture_value("rdy_nowhere"), None);

    // a default client supplies a value and loads nothing: the reminder stays owed
    let mut c = McpClient::start_with_env(pd.path(), &pairs);
    open(&mut c, "agent:d", "d1");
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(v.get("warnings"), None, "{v}");
    let v = body(c.call("rdy_overview", json!({})));
    assert_eq!(
        v["reminder"], REMINDER,
        "the client sent no instructions call"
    );
    assert!(advisories(&v).is_empty(), "{v}");
    // an explicit value stays
    let v = body(c.call("rdy_tags", with(json!({}), STALE)));
    assert_eq!(advisories(&v), [warning(WRONG, "rdy:read")], "{v}");
    let v = body(c.call("rdy_tags", right("rdy_tags", json!({}))));
    assert_eq!(v.get("warnings"), None, "{v}");
    let v = body(c.call("rdy_tags", with(json!({}), Value::Null)));
    assert_eq!(advisories(&v), [warning(WRONG, "rdy:read")], "{v}");
    drop(c);

    // raw mode sends what it is given
    let mut c = in_session(&w, pd.path(), "agent:e", "e1");
    let v = body(c.call("rdy_tags", json!({})));
    assert_eq!(advisories(&v), [warning(NOT_LOADED, "rdy:read")], "{v}");
}
