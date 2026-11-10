//! mht3 (D-6M0AF, D-4PHYQ): every MCP-over-HTTP request states its own claim, and the state of
//! one actor and run (saved settings, read pages, warnings, reminders, the review lease owner) is
//! shared by that claim's requests and by no other. In memory, through the production admission
//! layer and rmcp's `StreamableHttpService`; no socket.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::instructions::{
    self as instructions, Caller, Contribution, Layer, LoadRequest, PersonHost,
};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::claims::{Key, MAX_SETTINGS, Registry, SETTINGS_IDLE};
use rdy_mcp::http::{HttpMcp, MAX_WAITING_CALLS};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::server::{RdyServer, serve};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::{project, write};
use rdy_qa::mcp::supply_kungfu;
use rdy_qa::tensions::{put, upd};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const NO_SESSION_FIX: &str = "state rdy/actor and rdy/run in _meta, or call rdy_session";
const HTTP_MISMATCH: &str = "rdy_session and _meta name different actors or runs";
const HTTP_MISMATCH_FIX: &str = "use the same actor and run in rdy_session and _meta";

/// A person host over an isolated home: nothing here reads the real person layer.
struct Home(PathBuf);

impl PersonHost for Home {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        resolve_person(
            Platform::HOST,
            &PathEnv {
                home: Some(self.0.clone().into()),
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

/// One project in process, its home isolated, served over HTTP with `hub`.
struct Fixture {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    engine: Arc<dyn Engine>,
    hub: Arc<NotifyHub>,
    mcp: Arc<HttpMcp>,
    next: AtomicU64,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture::build(|_| {}, NotifyHub::new(), |m| m)
    }

    /// A project that `seed` fills before it is served, on `hub`, with `tune` on the HTTP side.
    fn build(
        seed: impl FnOnce(&mut Rdy),
        hub: Arc<NotifyHub>,
        tune: impl FnOnce(HttpMcp) -> HttpMcp,
    ) -> Fixture {
        let (dir, mut r) = project();
        seed(&mut r);
        r.close().unwrap();
        let home = tempfile::tempdir().unwrap();
        let engine: Arc<dyn Engine> = Arc::new(
            Local::open(dir.path())
                .unwrap()
                .with_person_host(Arc::new(Home(home.path().to_path_buf()))),
        );
        let mcp = Arc::new(tune(HttpMcp::new(Arc::clone(&engine), Arc::clone(&hub))));
        Fixture {
            _dir: dir,
            _home: home,
            engine,
            hub,
            mcp,
            next: AtomicU64::new(0),
        }
    }

    /// One JSON-RPC request's reply, over a supported older revision.
    async fn request(&self, method: &str, params: Value) -> Value {
        post(&self.mcp, &self.next, method, params).await
    }

    /// `(isError, body)` of `tool`, with the fixture kungfu value supplied.
    async fn call(&self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        let mut args = args;
        supply_kungfu(tool, &mut args);
        self.call_raw(tool, args, meta).await
    }

    /// `(isError, body)` of `tool` with exactly `args`.
    async fn call_raw(&self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        let v = self
            .request(
                "tools/call",
                json!({"name": tool, "arguments": args, "_meta": meta}),
            )
            .await;
        decode(&v)
    }

    fn ok(r: (bool, Value)) -> Value {
        assert!(!r.0, "expected a result: {}", r.1);
        r.1
    }
}

/// One request through `mcp`, as a client on revision 2025-11-25 sends it.
async fn post(mcp: &HttpMcp, next: &AtomicU64, method: &str, params: Value) -> Value {
    let id = next.fetch_add(1, Ordering::SeqCst) + 1;
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let p = mcp
        .post(
            &[
                ("host", "localhost"),
                ("accept", "application/json, text/event-stream"),
                ("content-type", "application/json"),
                ("mcp-protocol-version", "2025-11-25"),
            ],
            body.to_string().into_bytes(),
        )
        .await;
    assert_eq!(p.status, 200, "{p:?}");
    let reply = p.reply();
    assert_eq!(reply["id"], json!(id), "{reply}");
    reply
}

fn decode(v: &Value) -> (bool, Value) {
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool reply in {v}"));
    (
        v["result"]["isError"] == json!(true),
        serde_json::from_str(text).unwrap(),
    )
}

fn claim(actor: &str, run: &str) -> Value {
    json!({"rdy/actor": actor, "rdy/run": run})
}

fn key(actor: &str, run: &str) -> Key {
    (actor.to_string(), Some(run.to_string()))
}

fn with(mut meta: Value, k: &str, v: Value) -> Value {
    meta[k] = v;
    meta
}

fn refusal(r: (bool, Value)) -> (String, String, String) {
    assert!(r.0, "expected an rdy tool error: {}", r.1);
    let s = |k: &str| r.1[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

fn triple(code: &str, message: &str, fix: &str) -> (String, String, String) {
    (code.into(), message.into(), fix.into())
}

fn folder(name: &str) -> Value {
    json!({"ops": [{"op": "create", "type": "folder", "id": "$f", "fields": {"name": name, "title": name}}]})
}

/// One stdio connection to the same engine, in process: the transport HTTP must match.
struct Stdio {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
}

impl Stdio {
    async fn open(engine: &Arc<dyn Engine>) -> Stdio {
        let (client, end) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(end);
        let server = RdyServer::new(Arc::clone(engine), None, NotifyHub::new());
        tokio::spawn(async move {
            let _ = serve(server, r, w).await;
        });
        let (r, w) = tokio::io::split(client);
        let mut c = Stdio {
            r: BufReader::new(r),
            w,
            next: 0,
        };
        c.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "mht3", "version": "0"}}}),
        )
        .await;
        c.recv(json!(0)).await;
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn recv(&mut self, id: Value) -> Value {
        loop {
            let mut line = String::new();
            assert!(self.r.read_line(&mut line).await.unwrap() > 0, "closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    async fn call_raw(&mut self, tool: &str, args: Value, meta: Value) -> (bool, Value) {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args, "_meta": meta}}))
            .await;
        decode(&self.recv(json!(id)).await)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_every_http_call_needs_its_own_identity() {
    let f = Fixture::new();
    let a = claim("agent:a", "a1");
    let w = Fixture::ok(f.call("rdy_write", folder("one"), a.clone()).await);
    assert!(w["change"].is_string(), "{w}");
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), a.clone()).await);
    assert_eq!(ov["session"], json!({"actor": "agent:a", "view": "a1"}));
    // the next request states nothing: no request inherits another's identity
    let none = triple("E_NO_SESSION", "no session", NO_SESSION_FIX);
    for meta in [
        json!({}),
        json!({"progressToken": 7}),
        json!({"rdy/role": "reader"}),
        json!({"rdy/dedupe": true}),
    ] {
        assert_eq!(
            refusal(f.call("rdy_overview", json!({}), meta.clone()).await),
            none,
            "{meta}"
        );
        assert_eq!(
            refusal(f.call("rdy_write", folder("two"), meta).await),
            none
        );
    }
    // a resource read needs its own claim as well, in the read's error envelope
    let r = f
        .request("resources/read", json!({"uri": "rdy://project"}))
        .await;
    assert_eq!(r["error"]["data"]["code"], "E_NO_SESSION", "{r}");
    let r = f
        .request(
            "resources/read",
            json!({"uri": "rdy://project", "_meta": a.clone()}),
        )
        .await;
    assert!(r["result"]["contents"][0]["text"].is_string(), "{r}");
    // the claim keeps working for its own requests
    assert!(Fixture::ok(f.call("rdy_write", folder("three"), a).await)["change"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_session_arguments_claim_only_that_request() {
    let f = Fixture::new();
    // rdy_session states the claim in its own arguments, for itself
    let s = Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:s", "run": "s1", "role": "reader", "view": "canon"}),
            json!({}),
        )
        .await,
    );
    assert_eq!(
        (s["view"].as_str(), s["role"].as_str()),
        (Some("canon"), Some("reader"))
    );
    // the next claimless call is refused: the session authorized nothing later
    assert_eq!(
        refusal(f.call("rdy_overview", json!({}), json!({})).await),
        triple("E_NO_SESSION", "no session", NO_SESSION_FIX)
    );
    // a later call that states the claim gets its saved role and view
    let s1 = claim("agent:s", "s1");
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), s1.clone()).await);
    assert_eq!(ov["session"], json!({"actor": "agent:s", "view": "canon"}));
    assert_eq!(
        refusal(f.call("rdy_write", folder("x"), s1.clone()).await).0,
        "E_PERMISSION",
        "the saved reader role holds"
    );
    // arguments and _meta that disagree: the HTTP wording; that agree: accepted
    for args in [
        json!({"actor": "agent:x", "run": "s1"}),
        json!({"actor": "agent:s", "run": "s2"}),
        json!({"actor": "agent:s"}),
    ] {
        assert_eq!(
            refusal(f.call("rdy_session", args, s1.clone()).await),
            triple("E_BAD_INPUT", HTTP_MISMATCH, HTTP_MISMATCH_FIX)
        );
    }
    let s = Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:s", "run": "s1", "role": "writer"}),
            s1.clone(),
        )
        .await,
    );
    assert_eq!(s["role"], "writer");
    assert!(Fixture::ok(f.call("rdy_write", folder("y"), s1).await)["change"].is_string());
    // a malformed session call gives its stdio counterpart's error
    let mut stdio = Stdio::open(&f.engine).await;
    for args in [
        json!({"actor": "agent:m", "run": "m1", "bogus": 1}),
        json!({"actor": "human:m", "run": "m1"}),
        json!({"actor": "agent:m", "run": 5}),
        json!({"actor": "agent:m", "run": "m1", "role": "approver"}),
        json!({"run": "m1"}),
    ] {
        let http = f.call_raw("rdy_session", args.clone(), json!({})).await;
        let local = stdio.call_raw("rdy_session", args.clone(), json!({})).await;
        assert!(http.0, "{args}: {}", http.1);
        assert_eq!(http, local, "{args}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_meta_parse_order_matches_stdio() {
    let f = Fixture::new();
    let mut stdio = Stdio::open(&f.engine).await;
    // the claim refusals, in the parser's closed order, then the tool's own parameter check
    let cases = [
        json!({"rdy/zz": 1, "rdy/actr": 2, "rdy/actor": 3}),
        json!({"rdy/zzzzzz": 1}),
        json!({"rdy/dedupe": "x", "rdy/run": 1, "rdy/actor": "agent:a"}),
        json!({"rdy/actor": null, "rdy/run": null}),
        json!({"rdy/dedupe": null}),
        json!({"rdy/actor": "agent:a"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "a1", "rdy/role": "approver"}),
        json!({"rdy/actor": "agent:a", "rdy/run": "a1", "rdy/role": "boss"}),
        json!({"rdy/actor": "human:a", "rdy/run": "a1"}),
        json!({"rdy/actor": "not an actor", "rdy/run": "a1"}),
        // no claim with a malformed argument: the claim is refused first
        json!({"progressToken": 1}),
    ];
    for meta in cases {
        let args = json!({"id": "nope", "bogus": true});
        let http = f.call_raw("rdy_read", args.clone(), meta.clone()).await;
        let local = stdio.call_raw("rdy_read", args, meta.clone()).await;
        assert!(http.0, "{meta}: {}", http.1);
        assert_eq!(http, local, "{meta}");
    }
    // a valid claim: the parameter error is the tool's, unchanged, and the top-level kungfu
    // key leaves the arguments while a nested one stays an unknown input
    let a = claim("agent:a", "a1");
    for args in [
        json!({"id": "nope", "bogus": true}),
        json!({"id": "nope", "kungfu": "x", "scope": {"kungfu": "y"}}),
        json!({"max_tokens": "many"}),
    ] {
        let http = f.call_raw("rdy_read", args.clone(), a.clone()).await;
        let local = stdio.call_raw("rdy_read", args.clone(), a.clone()).await;
        assert!(http.0, "{args}: {}", http.1);
        assert_eq!(http, local, "{args}");
    }
    let mut args = json!({});
    supply_kungfu("rdy_tags", &mut args);
    let http = f.call_raw("rdy_tags", args.clone(), a.clone()).await;
    let local = stdio.call_raw("rdy_tags", args, a).await;
    assert!(!http.0, "{}", http.1);
    assert_eq!(http, local);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_role_override_does_not_leak_between_calls() {
    let f = Fixture::new();
    let r = claim("agent:r", "r1");
    Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:r", "run": "r1", "role": "writer"}),
            json!({}),
        )
        .await,
    );
    // a reader override holds for its call only
    let reader = with(r.clone(), "rdy/role", json!("reader"));
    assert_eq!(
        refusal(f.call("rdy_write", folder("a"), reader).await).0,
        "E_PERMISSION"
    );
    assert!(Fixture::ok(f.call("rdy_write", folder("b"), r.clone()).await)["change"].is_string());
    assert_eq!(
        f.hub.claims().get(&key("agent:r", "r1")).unwrap().role,
        Some(Role::Writer)
    );
    // a saved reader role: a writer override holds for its call, the saved role after it
    Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:r", "run": "r1", "role": "reader"}),
            r.clone(),
        )
        .await,
    );
    let writer = with(r.clone(), "rdy/role", json!("writer"));
    assert!(Fixture::ok(f.call("rdy_write", folder("c"), writer).await)["change"].is_string());
    assert_eq!(
        refusal(f.call("rdy_write", folder("d"), r.clone()).await).0,
        "E_PERMISSION"
    );
    // another claim never sees the override or the saved role: it is a writer
    let o = claim("agent:o", "o1");
    assert!(Fixture::ok(f.call("rdy_write", folder("e"), o).await)["change"].is_string());
}

/// A doc of `n` claims, landed in work.
fn many(n: usize) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Many", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim number {i} holds.")}));
    }
    Value::Array(ops)
}

/// A fixture with a 200-claim doc in view `work`, and the doc's id.
fn paged() -> (Fixture, String) {
    let doc = Arc::new(Mutex::new(String::new()));
    let d = Arc::clone(&doc);
    let f = Fixture::build(
        move |r| {
            let res = write(r, "work", many(200)).unwrap();
            *d.lock().unwrap() = res.created["$d"].clone();
        },
        NotifyHub::new(),
        |m| m,
    );
    let id = doc.lock().unwrap().clone();
    (f, id)
}

fn work(actor: &str, run: &str) -> Value {
    json!({"actor": actor, "run": run, "role": "writer", "view": "work"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_same_claim_keeps_pages_warnings_and_reminders() {
    let (f, doc) = paged();
    let a = claim("agent:p", "p1");
    Fixture::ok(
        f.call("rdy_session", work("agent:p", "p1"), a.clone())
            .await,
    );
    // a read cut into pages continues on a later request of the same claim
    let first = Fixture::ok(
        f.call("rdy_read", json!({"id": doc, "max_tokens": 300}), a.clone())
            .await,
    );
    let next = first["next"].as_str().expect("a cut read").to_string();
    let second = Fixture::ok(
        f.call(
            "rdy_read",
            json!({"page": next, "max_tokens": 300}),
            a.clone(),
        )
        .await,
    );
    assert!(second["text"].is_string(), "{second}");
    assert_ne!(first["text"], second["text"]);
    // the instructions reminder is due until this claim reads its instructions
    let reminder = rdy_engine::instructions::REMINDER_KEY;
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), a.clone()).await);
    assert!(ov.get(reminder).is_some(), "{ov}");
    Fixture::ok(
        f.call("rdy_instructions", json!({"load": ["rdy:read"]}), a.clone())
            .await,
    );
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), a.clone()).await);
    assert!(ov.get(reminder).is_none(), "{ov}");
    // a missing kungfu value warns once for this claim
    let warned = |v: &Value| {
        v["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|w| w["code"] == "W_KUNGFU"))
    };
    let t = Fixture::ok(f.call_raw("rdy_tags", json!({}), a.clone()).await);
    assert!(warned(&t), "{t}");
    let t = Fixture::ok(f.call_raw("rdy_tags", json!({}), a.clone()).await);
    assert!(!warned(&t), "the warning went out once for this claim: {t}");
    // another claim has its own marks: its reminder is still due
    let b = claim("agent:q", "q1");
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), b).await);
    assert!(ov.get(reminder).is_some(), "{ov}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_other_claim_cannot_use_saved_state() {
    let (f, doc) = paged();
    let a = claim("agent:p", "p1");
    let b = claim("agent:q", "q1");
    Fixture::ok(
        f.call("rdy_session", work("agent:p", "p1"), a.clone())
            .await,
    );
    Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:q", "run": "q1", "role": "reader", "view": "canon"}),
            b.clone(),
        )
        .await,
    );
    let first = Fixture::ok(
        f.call("rdy_read", json!({"id": doc, "max_tokens": 300}), a.clone())
            .await,
    );
    let next = first["next"].as_str().unwrap().to_string();
    // B cannot page A's read: the cursor is not one of its reads
    let e = refusal(
        f.call(
            "rdy_read",
            json!({"page": next.clone(), "max_tokens": 300}),
            b.clone(),
        )
        .await,
    );
    assert_eq!(e.0, "E_PAGE_STALE", "{e:?}");
    // a claimless request cannot either
    assert_eq!(
        refusal(
            f.call("rdy_read", json!({"page": next.clone()}), json!({}))
                .await
        )
        .0,
        "E_NO_SESSION"
    );
    // each claim keeps its own view and role
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), b.clone()).await);
    assert_eq!(ov["session"], json!({"actor": "agent:q", "view": "canon"}));
    let ov = Fixture::ok(f.call("rdy_overview", json!({}), a.clone()).await);
    assert_eq!(ov["session"], json!({"actor": "agent:p", "view": "work"}));
    // and A's cursor still works for A
    assert!(
        Fixture::ok(
            f.call("rdy_read", json!({"page": next, "max_tokens": 300}), a)
                .await
        )["text"]
            .is_string()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_concurrent_claims_never_exchange_state() {
    let f = Arc::new(Fixture::new());
    const CLAIMS: usize = 6;
    for i in 0..CLAIMS {
        Fixture::ok(
            f.call(
                "rdy_session",
                json!({"actor": format!("agent:c{i}"), "run": format!("c{i}"), "view": format!("view{i}")}),
                json!({}),
            )
            .await,
        );
    }
    let mut calls = Vec::new();
    for round in 0..8 {
        for i in 0..CLAIMS {
            let f = Arc::clone(&f);
            calls.push(tokio::spawn(async move {
                let c = claim(&format!("agent:c{i}"), &format!("c{i}"));
                let ov = Fixture::ok(f.call("rdy_overview", json!({}), c.clone()).await);
                let w = Fixture::ok(
                    f.call("rdy_write", folder(&format!("f{round}x{i}")), c)
                        .await,
                );
                (i, ov, w)
            }));
        }
    }
    for call in calls {
        let (i, ov, w) = call.await.unwrap();
        assert_eq!(
            ov["session"],
            json!({"actor": format!("agent:c{i}"), "view": format!("view{i}")}),
            "{ov}"
        );
        assert!(w["change"].is_string(), "{w}");
    }
    // each write landed in its own claim's view, under its own actor and run
    for i in 0..CLAIMS {
        let c = claim(&format!("agent:c{i}"), &format!("c{i}"));
        let shown = Fixture::ok(
            f.call(
                "rdy_view",
                json!({"op": "show", "view": format!("view{i}")}),
                c,
            )
            .await,
        );
        let changes = shown["changes"].as_array().expect("changes");
        assert_eq!(changes.len(), 8, "{shown}");
        for ch in changes {
            assert_eq!(
                (ch["actor"].as_str(), ch["run"].as_str()),
                (
                    Some(format!("agent:c{i}").as_str()),
                    Some(format!("c{i}").as_str())
                ),
                "{shown}"
            );
        }
    }
    assert_eq!(f.hub.claims().http_len(), CLAIMS);
}

/// A fixture whose tension requests wait for review, with set clocks: the claim registry's and
/// the lease expiry's.
fn reviewed(at: Arc<Mutex<Instant>>, lease_ms: Arc<AtomicI64>) -> Fixture {
    let clock = Arc::clone(&at);
    let hub = NotifyHub::with_claims(Registry::with_clock(Arc::new(move || {
        *clock.lock().unwrap()
    })));
    Fixture::build(
        |r| {
            let spec = spec_doc(r);
            put(r, "a", upd(&spec.c, "C contradicts B."));
        },
        hub,
        move |m| {
            m.with_lease_clock(Arc::new(move || {
                let set = lease_ms.load(Ordering::SeqCst);
                if set == 0 { rdy_engine::now_ms() } else { set }
            }))
        },
    )
}

fn reviewer(actor: &str, run: &str) -> Value {
    with(claim(actor, run), "rdy/role", json!("reviewer"))
}

/// Waits until no request of claim `k` is admitted: rmcp has dropped each one.
async fn settled(mcp: &HttpMcp, k: &Key) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while mcp.queued(k) > 0 {
        assert!(Instant::now() < deadline, "a request of {k:?} never ended");
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_settings_expire_but_live_lease_owner_survives() {
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let lease_ms = Arc::new(AtomicI64::new(0));
    let f = reviewed(Arc::clone(&at), Arc::clone(&lease_ms));
    let ka = key("agent:ra", "ra1");
    Fixture::ok(
        f.call(
            "rdy_session",
            json!({"actor": "agent:ra", "run": "ra1", "role": "reviewer"}),
            json!({}),
        )
        .await,
    );
    let n = Fixture::ok(
        f.call(
            "rdy_review_next",
            json!({"kind": "tension"}),
            claim("agent:ra", "ra1"),
        )
        .await,
    );
    assert!(n["request"].is_string(), "{n}");
    let owner = f.mcp.lease_owner(&ka).expect("the claim's state is held");
    let leased = f.engine.leases().with_prefix("", rdy_engine::now_ms());
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].1.owner, owner, "the lease is the claim's");
    // an hour idle: the settings expire, the owner stays while its lease lives
    *at.lock().unwrap() = base + SETTINGS_IDLE;
    assert!(
        f.hub.claims().get(&ka).is_none(),
        "the saved settings expired"
    );
    assert_eq!(f.mcp.lease_owner(&ka), Some(owner));
    // the claim's next request is a writer again (no saved role), with the same lease owner
    assert_eq!(
        refusal(
            f.call(
                "rdy_review_next",
                json!({"kind": "tension"}),
                claim("agent:ra", "ra1")
            )
            .await
        )
        .0,
        "E_PERMISSION"
    );
    assert_eq!(f.mcp.lease_owner(&ka), Some(owner));
    // the owner is never evicted to admit another claim, even in a full registry
    for i in 0..MAX_SETTINGS - 1 {
        f.hub
            .claims()
            .save(
                &key(&format!("agent:f{i}"), "f"),
                "v".into(),
                Role::Writer,
                None,
            )
            .unwrap();
    }
    assert_eq!(
        refusal(
            f.call("rdy_overview", json!({}), claim("agent:new", "n1"))
                .await
        )
        .1,
        "saved session settings exceed 1024 sessions"
    );
    assert_eq!(f.mcp.lease_owner(&ka), Some(owner));
    // once its last lease lapses and it stays idle, the owner goes, and the claim gets a new one
    *at.lock().unwrap() = base + SETTINGS_IDLE * 3;
    lease_ms.store(
        rdy_engine::now_ms() + 365 * 24 * 3600 * 1000,
        Ordering::SeqCst,
    );
    assert_eq!(f.mcp.lease_owner(&ka), None);
    Fixture::ok(
        f.call("rdy_overview", json!({}), claim("agent:ra", "ra1"))
            .await,
    );
    let renewed = f.mcp.lease_owner(&ka).unwrap();
    assert_ne!(renewed, owner);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_http_disconnect_keeps_lease_until_submit_or_ttl() {
    let at = Arc::new(Mutex::new(Instant::now()));
    let f = reviewed(at, Arc::new(AtomicI64::new(0)));
    let ra = reviewer("agent:ra", "ra1");
    let n = Fixture::ok(
        f.call("rdy_review_next", json!({"kind": "tension"}), ra.clone())
            .await,
    );
    let request = n["request"].as_str().expect("a request").to_string();
    let hex = request
        .strip_prefix("rq-")
        .expect("a request id")
        .to_string();
    let ka = key("agent:ra", "ra1");
    let owner = f.mcp.lease_owner(&ka).unwrap();
    // the request has ended: its place in the claim's queue goes when rmcp drops the request
    settled(&f.mcp, &ka).await;
    // the response is gone, and nothing released the lease: it keeps its owner and TTL
    let now = rdy_engine::now_ms();
    let held = f.engine.leases().with_prefix(&hex, now);
    assert_eq!(held.len(), 1, "{request}");
    assert_eq!(held[0].1.owner, owner);
    assert!(held[0].1.until_ms > now);
    // so another reviewer is offered another request
    let other = Fixture::ok(
        f.call(
            "rdy_review_next",
            json!({"kind": "tension"}),
            reviewer("agent:rb", "rb1"),
        )
        .await,
    );
    assert!(
        other["request"].is_string() && other["request"] != json!(request),
        "{other}"
    );
    // the same claim submits on a later request; the submit frees the lease
    let block = n["case"]["blocks"][0]["id"].clone();
    let sub = Fixture::ok(
        f.call(
            "rdy_review_submit",
            json!({"request": request, "case_hash": n["case_hash"], "verdict": "tension",
                "citations": [{"block": block, "note": "these two cannot both hold"}]}),
            ra,
        )
        .await,
    );
    assert_eq!(sub["severity"], "advisory", "{sub}");
    assert!(
        f.engine
            .leases()
            .with_prefix(&hex, rdy_engine::now_ms())
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mht3_claim_capacity_and_call_queue_are_bounded() {
    let f = Arc::new(Fixture::new());
    for i in 0..MAX_SETTINGS {
        f.hub
            .claims()
            .save(
                &key(&format!("agent:f{i}"), "f"),
                "v".into(),
                Role::Writer,
                None,
            )
            .unwrap();
    }
    // a new claim takes the capacity its settings would: the registry's own refusal
    assert_eq!(
        refusal(
            f.call("rdy_overview", json!({}), claim("agent:new", "n1"))
                .await
        ),
        triple(
            "E_LIMIT",
            "saved session settings exceed 1024 sessions",
            "reuse an actor and run, or try again later: idle settings expire after one hour"
        )
    );
    assert_eq!(f.hub.claims().http_len(), 0, "the refusal held nothing");
    // a claim with settings is served, and its HTTP state takes no other slot
    let q = claim("agent:f1", "f");
    let kq = key("agent:f1", "f");
    Fixture::ok(f.call("rdy_overview", json!({}), q.clone()).await);
    assert_eq!(f.hub.claims().http_len(), 1);
    // the queue: one call runs, eight wait, the tenth is refused at once
    let turn = f.mcp.hold_turn(&kq).await.expect("held state");
    let mut waiting = Vec::new();
    for _ in 0..=MAX_WAITING_CALLS {
        let f = Arc::clone(&f);
        let q = q.clone();
        waiting.push(tokio::spawn(async move {
            f.call("rdy_overview", json!({}), q).await
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while f.mcp.queued(&kq) < MAX_WAITING_CALLS + 1 {
        assert!(Instant::now() < deadline, "the calls were never admitted");
        tokio::task::yield_now().await;
    }
    // the refusal never waits for the turn: a hang guard turns a wait into a failure
    let tenth = tokio::time::timeout(
        Duration::from_secs(60),
        f.call("rdy_overview", json!({}), q.clone()),
    )
    .await
    .expect("the tenth call is refused at once");
    assert_eq!(
        refusal(tenth),
        triple(
            "E_LIMIT",
            "this HTTP claim has 8 calls waiting",
            "wait for a call with this actor and run to finish"
        )
    );
    // another claim is not held up by this one's queue
    Fixture::ok(
        f.call("rdy_overview", json!({}), claim("agent:f2", "f"))
            .await,
    );
    drop(turn);
    for w in waiting {
        let ov = Fixture::ok(w.await.unwrap());
        assert_eq!(ov["session"]["actor"], "agent:f1");
    }
    assert_eq!(f.mcp.queued(&kq), 0);
}
