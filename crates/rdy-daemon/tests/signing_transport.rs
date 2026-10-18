#![cfg(unix)]
// Unix only: it runs a daemon on its Unix socket and sends SIGINT to itself. FW-XP1 Phase 4 ports it.
//! sig5: a signing request goes to the process at the other end of the request's own
//! connection (D-QVH4N), over that connection, while the writer goes on (D-1Z951).

mod common;

use common::short_tempdir;
use rdy_daemon::signing_transport::{
    CALLBACK_MARK, CallerLink, CallerSigner, HostMcp, MAX_CALLBACK_LINE_BYTES, RemoteAgent,
    host_mcp, never_interrupted, read_reply,
};
use rdy_daemon::*;
use rdy_engine::engine::{Engine, write_typed};
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent};
use rdy_engine::{E_NO_AGENT, NoAgentCause, Rdy, RdyError, Session, WriteRequest, no_agent};
use rdy_model::Role;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Condvar, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::mpsc;

// ---- keys, payloads and the wire ----

fn string(b: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(b.len()).unwrap().to_be_bytes().to_vec();
    v.extend_from_slice(b);
    v
}

/// An Ed25519 public-key blob whose key bytes are all `n`.
fn key(n: u8) -> Vec<u8> {
    [string(b"ssh-ed25519"), string(&[n; 32])].concat()
}

/// SSHSIG signed data of an approval, as rdy asks an agent to sign it.
fn signed_data() -> Vec<u8> {
    [
        b"SSHSIG".to_vec(),
        string(b"rdy-approval-v1"),
        string(b""),
        string(b"sha512"),
        string(&[9; 64]),
    ]
    .concat()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a fake agent signs `key`'s requests with.
fn fake_signature(key: &[u8]) -> Vec<u8> {
    [b"sig:".as_slice(), key].concat()
}

fn no_agent_text(cause: NoAgentCause) -> String {
    format!("the SSH agent could not sign: {cause}")
}

// ---- a fake caller agent with explicit signals ----

#[derive(Default)]
struct Gate {
    /// Releases not yet taken: a call takes one and answers.
    released: usize,
    /// A cancel latched for the call in flight.
    cancelled: bool,
}

/// A caller's agent holding `keys`. A gated one reports each call on `ready` and waits for a
/// release or a cancel; it never waits on a clock.
#[derive(Default)]
struct Fake {
    keys: Vec<Vec<u8>>,
    gated: bool,
    gate: Mutex<Gate>,
    cv: Condvar,
    ready: Mutex<Option<std_mpsc::Sender<()>>>,
    calls: AtomicUsize,
    active: AtomicUsize,
    cancels: AtomicUsize,
}

impl Fake {
    fn open(keys: Vec<Vec<u8>>) -> Arc<Fake> {
        Arc::new(Fake {
            keys,
            ..Fake::default()
        })
    }

    fn gated(keys: Vec<Vec<u8>>) -> (Arc<Fake>, std_mpsc::Receiver<()>) {
        let (tx, rx) = std_mpsc::channel();
        let fake = Fake {
            keys,
            gated: true,
            ready: Mutex::new(Some(tx)),
            ..Fake::default()
        };
        (Arc::new(fake), rx)
    }

    fn release(&self) {
        self.gate.lock().unwrap().released += 1;
        self.cv.notify_all();
    }

    fn wait(&self) -> Result<(), RdyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let out = self.gate_through();
        self.active.fetch_sub(1, Ordering::SeqCst);
        out
    }

    fn gate_through(&self) -> Result<(), RdyError> {
        if !self.gated {
            return Ok(());
        }
        if let Some(tx) = &*self.ready.lock().unwrap() {
            let _ = tx.send(());
        }
        let mut g = self.gate.lock().unwrap();
        loop {
            if std::mem::take(&mut g.cancelled) {
                return Err(no_agent(NoAgentCause::Cancelled));
            }
            if g.released > 0 {
                g.released -= 1;
                return Ok(());
            }
            g = self.cv.wait(g).unwrap();
        }
    }
}

impl SigningAgent for Fake {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        self.wait()?;
        Ok(self
            .keys
            .iter()
            .map(|k| AgentIdentity {
                public_key: k.clone(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], _payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        self.wait()?;
        Ok(fake_signature(public_key))
    }
}

impl CallerSigner for Fake {
    fn cancel(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        if self.active.load(Ordering::SeqCst) > 0 {
            self.gate.lock().unwrap().cancelled = true;
            self.cv.notify_all();
        }
    }
}

/// Waits on a std receiver without blocking the runtime.
async fn recv(rx: &mut std_mpsc::Receiver<()>) {
    tokio::task::block_in_place(|| rx.recv().unwrap());
}

// ---- a host handler that signs through each request's agent ----

#[derive(Default)]
struct Signer {
    engine: Option<Arc<dyn Engine>>,
    signs: AtomicUsize,
    /// Each agent result, as the handler saw it.
    results: Mutex<Option<mpsc::UnboundedSender<Result<Value, RdyError>>>>,
}

impl Signer {
    fn with_results() -> (
        Arc<Signer>,
        mpsc::UnboundedReceiver<Result<Value, RdyError>>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let s = Signer {
            results: Mutex::new(Some(tx)),
            ..Signer::default()
        };
        (Arc::new(s), rx)
    }

    fn report(&self, r: Result<Value, RdyError>) -> Result<Value, ErrorReply> {
        if let Some(tx) = &*self.results.lock().unwrap() {
            let _ = tx.send(r.clone());
        }
        r.map_err(ErrorReply::from)
    }
}

impl Handler for Signer {
    fn ctl(
        &self,
        _s: &SessionInfo,
        r: CtlRequest,
        agent: Arc<dyn SigningAgent>,
    ) -> Result<Value, ErrorReply> {
        match r.op.as_str() {
            "identities" => {
                let keys = agent
                    .identities()
                    .map(|ids| json!(ids.iter().map(|i| hex(&i.public_key)).collect::<Vec<_>>()));
                self.report(keys)
            }
            "sign" => {
                self.signs.fetch_add(1, Ordering::SeqCst);
                let n = u8::try_from(r.args["key"].as_u64().unwrap_or(1)).unwrap();
                let sig = agent
                    .sign(&key(n), &signed_data())
                    .map(|s| json!({"signature": hex(&s)}));
                self.report(sig)
            }
            "big" => {
                let keys = agent.identities().map_err(ErrorReply::from)?;
                let size = usize::try_from(r.args["size"].as_u64().unwrap()).unwrap();
                Ok(json!({"keys": keys.len(), "big": "x".repeat(size)}))
            }
            "write" => {
                let e = self.engine.as_ref().expect("an engine");
                write_typed(&**e, |r| {
                    let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "w", "title": "W", "doc_type": "page"}}])).unwrap();
                    let s = Session::human("human:t".into(), Role::Writer);
                    r.write(&s, WriteRequest { view: "work".into(), ops, justification: None, message: String::new() })
                })
                .map_err(ErrorReply::from)
            }
            other => Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("unknown op {other:?}"),
                "use sign",
            )),
        }
    }
}

// ---- a daemon, and its clients ----

struct Up {
    dir: tempfile::TempDir,
    run: RunDir,
    handle: DaemonHandle,
}

impl Drop for Up {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

async fn daemon_in(dir: tempfile::TempDir, handler: Arc<dyn Handler>) -> Up {
    let run = RunDir::of_project(dir.path());
    let mut d = Daemon::new(DaemonConfig::new(run.clone(), "1".into()), handler);
    d.bind().await.expect("bind");
    let handle = d.handle();
    d.readiness().ready();
    tokio::spawn(d.run());
    Up { dir, run, handle }
}

async fn daemon(handler: Arc<dyn Handler>) -> Up {
    daemon_in(short_tempdir(), handler).await
}

/// A ctl client of `up`, with no agent.
async fn bare_client(up: &Up) -> Client {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "1".into(),
        proto: Proto::Ctl,
        cwd: up.dir.path().to_path_buf(),
        project: up.run.project_id().unwrap(),
    };
    match connect(&up.run, &hello).await {
        Attempt::Ready(c) => c.with_interrupt(never_interrupted()),
        other => panic!("not ready: {other:?}"),
    }
}

/// A ctl client of `up` whose own agent is `agent`.
async fn client(up: &Up, agent: Arc<dyn CallerSigner>) -> Client {
    bare_client(up).await.with_agent(agent)
}

fn assert_no_agent(r: &Result<Value, ErrorReply>, cause: NoAgentCause) {
    let e = r.as_ref().expect_err("an E_NO_AGENT error");
    assert_eq!(e.code, E_NO_AGENT, "{e:?}");
    assert_eq!(e.message, no_agent_text(cause));
}

fn assert_cause(r: &Result<Value, RdyError>, cause: NoAgentCause) {
    let e = r.as_ref().expect_err("an E_NO_AGENT error");
    assert_eq!(e.code, E_NO_AGENT, "{e:?}");
    assert_eq!(e.message, no_agent_text(cause));
}

// ---- an in-memory ctl connection ----

fn info() -> DaemonInfo {
    DaemonInfo {
        pid: std::process::id(),
        version: "t".into(),
        started_at: std::time::Instant::now(),
        sessions: Sessions::default(),
        connections: Arc::new(AtomicUsize::new(1)),
        stop: Arc::new(tokio::sync::watch::channel(false).0),
    }
}

fn served(
    handler: Arc<dyn Handler>,
) -> (
    BufReader<DuplexStream>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let i = info();
    let (client, server) = tokio::io::duplex(1 << 20);
    let session = i.sessions.insert(Proto::Ctl, "t".into(), "/".into(), 0);
    let (r, w) = tokio::io::split(server);
    (
        BufReader::new(client),
        tokio::spawn(serve_ctl(r, w, session, handler, i)),
    )
}

async fn send(c: &mut BufReader<DuplexStream>, line: &[u8]) {
    c.get_mut().write_all(line).await.unwrap();
    c.get_mut().flush().await.unwrap();
}

async fn line(c: &mut BufReader<DuplexStream>) -> Vec<u8> {
    let mut out = Vec::new();
    c.read_until(b'\n', &mut out).await.unwrap();
    out
}

/// The next line, which must be a callback request; its JSON.
async fn callback(c: &mut BufReader<DuplexStream>) -> Value {
    let l = line(c).await;
    assert_eq!(
        l.first(),
        Some(&CALLBACK_MARK),
        "{:?}",
        String::from_utf8_lossy(&l)
    );
    serde_json::from_slice(&l[1..]).unwrap()
}

/// The next line, which must be an ordinary ctl reply; its JSON.
async fn reply(c: &mut BufReader<DuplexStream>) -> Value {
    let l = line(c).await;
    assert_ne!(
        l.first(),
        Some(&CALLBACK_MARK),
        "{:?}",
        String::from_utf8_lossy(&l)
    );
    serde_json::from_slice(&l).unwrap_or_else(|e| panic!("{e}: {:?}", String::from_utf8_lossy(&l)))
}

fn cb(json: &str) -> Vec<u8> {
    let mut v = vec![CALLBACK_MARK];
    v.extend_from_slice(json.as_bytes());
    v.push(b'\n');
    v
}

fn assert_reply_cause(v: &Value, cause: NoAgentCause) {
    assert_eq!(v["error"]["code"], E_NO_AGENT, "{v}");
    assert_eq!(v["error"]["message"], no_agent_text(cause), "{v}");
}

// ---- an in-memory MCP connection: the proxy, the host pump and a fake service ----

struct Mcp {
    /// The MCP client's stdin.
    input: DuplexStream,
    /// The MCP client's stdout.
    output: BufReader<DuplexStream>,
    /// Every line the service read, in order.
    seen: mpsc::UnboundedReceiver<String>,
    /// Each sign result the service saw.
    results: mpsc::UnboundedReceiver<Result<Vec<u8>, RdyError>>,
    proxy: tokio::task::JoinHandle<i32>,
}

fn notification(id: &Value, at: &str) -> String {
    json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"id": id, "at": at}})
        .to_string()
        + "\n"
}

/// A fake MCP service on the host's side: each `tools/call` writes a notification, signs with
/// the connection's agent, writes another notification, then answers with the signature and
/// `big` bytes of padding. A reader task records every line as it arrives.
fn fake_service(
    host: HostMcp,
    big: usize,
) -> (
    mpsc::UnboundedReceiver<String>,
    mpsc::UnboundedReceiver<Result<Vec<u8>, RdyError>>,
) {
    let HostMcp {
        stream,
        agent,
        pump,
    } = host;
    tokio::spawn(pump);
    let (seen_tx, seen) = mpsc::unbounded_channel();
    let (calls_tx, mut calls) = mpsc::unbounded_channel::<Value>();
    let (results_tx, results) = mpsc::unbounded_channel();
    let (r, mut w) = tokio::io::split(stream);
    tokio::spawn(async move {
        let mut r = BufReader::new(r);
        loop {
            let mut l = String::new();
            if r.read_line(&mut l).await.unwrap_or(0) == 0 {
                break;
            }
            let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
            let _ = seen_tx.send(l);
            if v["method"] == "tools/call" {
                let _ = calls_tx.send(v);
            }
        }
    });
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let id = call["id"].clone();
            w.write_all(notification(&id, "before").as_bytes())
                .await
                .unwrap();
            w.flush().await.unwrap();
            let a: Arc<RemoteAgent> = Arc::clone(&agent);
            let out = tokio::task::spawn_blocking(move || a.sign(&key(1), &signed_data()))
                .await
                .unwrap();
            let _ = results_tx.send(out.clone());
            w.write_all(notification(&id, "after").as_bytes())
                .await
                .unwrap();
            let reply = match out {
                Ok(sig) => {
                    json!({"jsonrpc": "2.0", "id": id, "result": {"signature": hex(&sig), "big": "y".repeat(big)}})
                }
                Err(e) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.message}})
                }
            };
            w.write_all((reply.to_string() + "\n").as_bytes())
                .await
                .unwrap();
            w.flush().await.unwrap();
        }
        let _ = w.shutdown().await;
    });
    (seen, results)
}

fn mcp(agent: Arc<dyn CallerSigner>, big: usize) -> Mcp {
    let (client_end, host_end) = tokio::io::duplex(1 << 16);
    let (seen, results) = fake_service(host_mcp(host_end), big);
    let (input, proxy_in) = tokio::io::duplex(1 << 16);
    let (proxy_out, output) = tokio::io::duplex(1 << 16);
    let proxy = tokio::spawn(rdy_daemon::proxy::run_proxy(
        proxy_in, proxy_out, client_end, agent,
    ));
    Mcp {
        input,
        output: BufReader::new(output),
        seen,
        results,
        proxy,
    }
}

fn tool_call(id: u64) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "rdy_sign", "arguments": {}}})
        .to_string()
        + "\n"
}

fn cancelled(id: u64) -> String {
    json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}})
        .to_string()
        + "\n"
}

async fn out_line(m: &mut Mcp) -> String {
    let mut l = Vec::new();
    m.output.read_until(b'\n', &mut l).await.unwrap();
    assert!(
        !l.contains(&CALLBACK_MARK),
        "a callback byte reached stdout"
    );
    String::from_utf8(l).unwrap()
}

/// Waits until the service has read a line that contains `needle`.
async fn service_saw(m: &mut Mcp, needle: &str) {
    loop {
        let l = m.seen.recv().await.expect("the service ended");
        if l.contains(needle) {
            return;
        }
    }
}

// ---- the tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_two_callers_use_two_agents_not_the_daemon_agent() {
    // The daemon has no agent of its own: its handler signs only through the per-connection
    // agent, which is each caller's.
    let up = daemon(Arc::new(Signer::default())).await;
    let (a, mut a_ready) = Fake::gated(vec![key(1)]);
    let (b, mut b_ready) = Fake::gated(vec![key(2), key(3)]);
    let mut ca = client(&up, a.clone()).await;
    let mut cb = client(&up, b.clone()).await;
    let ta = tokio::spawn(async move {
        let r = ca.ctl("identities", json!({})).await;
        (ca, r)
    });
    let tb = tokio::spawn(async move {
        let r = cb.ctl("identities", json!({})).await;
        (cb, r)
    });
    // both callers are asked at the same time, each by its own connection
    recv(&mut a_ready).await;
    recv(&mut b_ready).await;
    b.release();
    let (mut cb, rb) = tb.await.unwrap();
    a.release();
    let (mut ca, ra) = ta.await.unwrap();
    assert_eq!(ra.unwrap(), json!([hex(&key(1))]));
    assert_eq!(rb.unwrap(), json!([hex(&key(2)), hex(&key(3))]));

    // each sign goes to the agent of the connection that asked
    a.release();
    b.release();
    let (sa, sb) = tokio::join!(
        ca.ctl("sign", json!({"key": 1})),
        cb.ctl("sign", json!({"key": 2}))
    );
    assert_eq!(sa.unwrap()["signature"], hex(&fake_signature(&key(1))));
    assert_eq!(sb.unwrap()["signature"], hex(&fake_signature(&key(2))));
    assert_eq!(a.calls.load(Ordering::SeqCst), 2);
    assert_eq!(b.calls.load(Ordering::SeqCst), 2);

    // a caller without an agent is told so; nothing falls back to another agent
    let mut none = bare_client(&up).await;
    assert_no_agent(
        &none.ctl("identities", json!({})).await,
        NoAgentCause::Unset,
    );
    assert_eq!(
        a.calls.load(Ordering::SeqCst) + b.calls.load(Ordering::SeqCst),
        4
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_callbacks_are_invisible_to_mcp_and_ctl_clients() {
    // MCP: three pipelined calls; each answers with two notifications and a 2 MiB reply, and
    // signs between them. The client sees exactly the service's lines, in order.
    let big = 2 << 20;
    let mut m = mcp(Fake::open(vec![key(1)]), big);
    let calls: String = (1..=3).map(tool_call).collect();
    m.input.write_all(calls.as_bytes()).await.unwrap();
    for id in 1..=3u64 {
        let id = json!(id);
        assert_eq!(out_line(&mut m).await, notification(&id, "before"));
        assert_eq!(out_line(&mut m).await, notification(&id, "after"));
        let reply: Value = serde_json::from_str(&out_line(&mut m).await).unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["signature"], hex(&fake_signature(&key(1))));
        assert_eq!(reply["result"]["big"].as_str().unwrap().len(), big);
    }
    // the service read the three calls and nothing else: the callback replies were taken out
    for id in 1..=3 {
        assert_eq!(m.seen.recv().await.unwrap(), tool_call(id));
    }
    drop(m.input);
    assert_eq!(m.proxy.await.unwrap(), 0);
    assert!(
        m.seen.try_recv().is_err(),
        "the service read a callback line"
    );

    // ctl: three pipelined requests that each ask the agent and answer 3 MiB, and a built-in
    let (mut c, _h) = served(Arc::new(Signer::default()));
    let size = 3 << 20;
    let mut pipelined = String::new();
    for id in 1..=3 {
        pipelined += &(json!({"id": id, "op": "big", "args": {"size": size}}).to_string() + "\n");
    }
    pipelined += &(json!({"id": 4, "op": "ping"}).to_string() + "\n");
    send(&mut c, pipelined.as_bytes()).await;
    let mut link = CallerLink::new(Some(Fake::open(vec![key(1), key(2)])), never_interrupted());
    for id in 1..=3 {
        let l = read_reply(&mut link, &mut c)
            .await
            .unwrap()
            .expect("a reply");
        let v: Value = serde_json::from_slice(&l).unwrap();
        assert_eq!(v["id"], id, "replies keep request order");
        assert_eq!(v["ok"]["keys"], 2);
        assert_eq!(v["ok"]["big"].as_str().unwrap().len(), size);
    }
    let l = read_reply(&mut link, &mut c)
        .await
        .unwrap()
        .expect("a reply");
    let v: Value = serde_json::from_slice(&l).unwrap();
    assert_eq!(v["id"], 4);
    assert!(v["ok"]["pid"].is_number(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_waiting_for_an_agent_leaves_the_writer_available() {
    let dir = short_tempdir();
    let shared = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let signer = Signer {
        engine: Some(shared.engine()),
        ..Signer::default()
    };
    let up = daemon_in(dir, Arc::new(signer)).await;
    let (a, mut a_ready) = Fake::gated(vec![key(1)]);
    let mut waiting = client(&up, a.clone()).await;
    let signing = tokio::spawn(async move { waiting.ctl("sign", json!({"key": 1})).await });
    // the caller's agent waits (a person has not touched the key yet)
    recv(&mut a_ready).await;
    // another connection writes, and its write lands before the agent answers
    let mut writer = bare_client(&up).await;
    let wrote = writer
        .ctl("write", json!({}))
        .await
        .expect("the write lands");
    assert!(wrote.is_object(), "{wrote}");
    assert_eq!(a.active.load(Ordering::SeqCst), 1, "the agent still waits");
    a.release();
    let signed = signing.await.unwrap().unwrap();
    assert_eq!(signed["signature"], hex(&fake_signature(&key(1))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_disconnect_and_cancel_release_the_signer() {
    // 1. The caller's socket closes while the host waits for its callback reply.
    let (signer, mut results) = Signer::with_results();
    let (mut c, served_task) = served(signer.clone());
    send(&mut c, br#"{"id":1,"op":"sign","args":{"key":1}}"#).await;
    send(&mut c, b"\n").await;
    let ask = callback(&mut c).await;
    assert_eq!(ask["op"], "sign");
    drop(c);
    assert_cause(
        &results.recv().await.unwrap(),
        NoAgentCause::ConnectionFailed,
    );
    let _ = served_task.await.unwrap();
    assert_eq!(
        signer.signs.load(Ordering::SeqCst),
        1,
        "the write is not retried"
    );

    // 2. The host goes away while the caller's agent waits: the caller cancels its agent and
    // its worker ends.
    let (host, caller) = tokio::io::duplex(1 << 16);
    let (fake, mut ready) = Fake::gated(vec![key(1)]);
    let mut link = CallerLink::new(Some(fake.clone()), never_interrupted());
    let mut caller = BufReader::new(caller);
    let mut host = host;
    host.write_all(&cb(r#"{"id":1,"op":"identities"}"#))
        .await
        .unwrap();
    let reading = tokio::spawn(async move { read_reply(&mut link, &mut caller).await });
    recv(&mut ready).await;
    drop(host);
    let out = reading.await.unwrap();
    assert!(
        !matches!(out, Ok(Some(_))),
        "no reply from a gone host: {out:?}"
    );
    assert_eq!(fake.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(
        fake.active.load(Ordering::SeqCst),
        0,
        "no signing worker remains"
    );

    // 3. MCP: a cancel of another request leaves the signer waiting; a cancel of the active
    // request ends it.
    let (fake, mut ready) = Fake::gated(vec![key(1)]);
    let mut m = mcp(fake.clone(), 0);
    m.input.write_all(tool_call(7).as_bytes()).await.unwrap();
    recv(&mut ready).await;
    m.input.write_all(cancelled(99).as_bytes()).await.unwrap();
    service_saw(&mut m, "notifications/cancelled").await;
    assert_eq!(
        fake.cancels.load(Ordering::SeqCst),
        0,
        "request 99 is not the active one"
    );
    assert_eq!(
        fake.active.load(Ordering::SeqCst),
        1,
        "the signer still waits"
    );
    fake.release();
    assert_eq!(
        m.results.recv().await.unwrap().unwrap(),
        fake_signature(&key(1))
    );
    assert_eq!(out_line(&mut m).await, notification(&json!(7), "before"));
    assert_eq!(out_line(&mut m).await, notification(&json!(7), "after"));
    let v: Value = serde_json::from_str(&out_line(&mut m).await).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["result"]["signature"], hex(&fake_signature(&key(1))));

    m.input.write_all(tool_call(8).as_bytes()).await.unwrap();
    recv(&mut ready).await;
    m.input.write_all(cancelled(8).as_bytes()).await.unwrap();
    assert_cause(
        &m.results.recv().await.unwrap().map(|_| Value::Null),
        NoAgentCause::Cancelled,
    );
    assert_eq!(
        fake.active.load(Ordering::SeqCst),
        0,
        "no signing worker remains"
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2, "nothing is retried");

    // 4. CLI: SIGINT while the caller's agent waits cancels the request.
    let (signer, mut results) = Signer::with_results();
    let up = daemon(signer.clone()).await;
    let (fake, mut ready) = Fake::gated(vec![key(1)]);
    // the caller's own SIGINT handling (no injected interrupt)
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: "1".into(),
        proto: Proto::Ctl,
        cwd: up.dir.path().to_path_buf(),
        project: up.run.project_id().unwrap(),
    };
    let Attempt::Ready(c) = connect(&up.run, &hello).await else {
        panic!("not ready")
    };
    let mut c = c.with_agent(fake.clone());
    let signing = tokio::spawn(async move { c.ctl("sign", json!({"key": 1})).await });
    recv(&mut ready).await;
    let status = std::process::Command::new("kill")
        .args(["-INT", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    assert_no_agent(&signing.await.unwrap(), NoAgentCause::Cancelled);
    assert_cause(&results.recv().await.unwrap(), NoAgentCause::Cancelled);
    assert_eq!(
        fake.active.load(Ordering::SeqCst),
        0,
        "no signing worker remains"
    );
    assert_eq!(
        signer.signs.load(Ordering::SeqCst),
        1,
        "the write is not retried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_callback_ids_and_bounds_are_connection_local() {
    let ask = br#"{"id":1,"op":"identities","args":{}}
"#;
    let ask2 = br#"{"id":2,"op":"identities","args":{}}
"#;
    // the request is exact, and a good reply is taken
    let (mut c, _h) = served(Arc::new(Signer::default()));
    send(&mut c, ask).await;
    let l = line(&mut c).await;
    assert_eq!(l, cb(r#"{"id":1,"op":"identities"}"#));
    send(
        &mut c,
        &cb(&format!(
            r#"{{"id":1,"ok":{{"keys":["{}"]}}}}"#,
            hex(&key(4))
        )),
    )
    .await;
    assert_eq!(reply(&mut c).await["ok"], json!([hex(&key(4))]));
    // a repeated id closes the channel: the next ask fails without a callback
    send(&mut c, &cb(r#"{"id":1,"ok":{"keys":[]}}"#)).await;
    send(&mut c, ask2).await;
    assert_reply_cause(&reply(&mut c).await, NoAgentCause::InvalidReply);

    // each bad reply fails the waiting ask, and every later one, as `invalid reply`
    let bad: Vec<Vec<u8>> = vec![
        cb(r#"{"id":1,"ok":{"keys":[]},"extra":1}"#),
        cb(r#"{"id":1,"ok":{"keys":[],"extra":1}}"#),
        cb(r#"{"id":1,"ok":{"keys":["0A"]}}"#),
        cb(r#"{"id":1,"ok":{"keys":["abc"]}}"#),
        cb(r#"{"id":1,"op":"identities"}"#),
        cb(r#"{"id":1,"ok":{"signature":"00"}}"#),
        cb(r#"{"id":1,"error":{"code":"E_NO_AGENT","cause":"the agent said no"}}"#),
        cb(r#"{"id":0,"ok":{"keys":[]}}"#),
        cb(r#"{"id":2,"ok":{"keys":[]}}"#),
        {
            let mut over = vec![CALLBACK_MARK];
            over.extend(std::iter::repeat_n(b'x', MAX_CALLBACK_LINE_BYTES));
            over.push(b'\n');
            over
        },
    ];
    for (i, b) in bad.iter().enumerate() {
        let (mut c, _h) = served(Arc::new(Signer::default()));
        send(&mut c, ask).await;
        assert_eq!(callback(&mut c).await["id"], 1);
        send(&mut c, b).await;
        assert_reply_cause(&reply(&mut c).await, NoAgentCause::InvalidReply);
        send(&mut c, ask2).await;
        let v = reply(&mut c).await;
        assert_eq!(v["id"], 2, "case {i}");
        assert_reply_cause(&v, NoAgentCause::InvalidReply);
        // the connection itself lives
        send(&mut c, b"{\"id\":3,\"op\":\"ping\"}\n").await;
        assert!(reply(&mut c).await["ok"]["pid"].is_number(), "case {i}");
    }

    // an error reply with a closed cause is that cause, and the channel stays open
    let (mut c, _h) = served(Arc::new(Signer::default()));
    send(&mut c, ask).await;
    callback(&mut c).await;
    send(
        &mut c,
        &cb(r#"{"id":1,"error":{"code":"E_NO_AGENT","cause":"key is not held"}}"#),
    )
    .await;
    assert_reply_cause(&reply(&mut c).await, NoAgentCause::NotHeld);
    send(&mut c, ask2).await;
    assert_eq!(callback(&mut c).await["id"], 2, "ids increase");

    // ids are the connection's own: another connection's id fails this one only
    let (mut x, _hx) = served(Arc::new(Signer::default()));
    let (mut y, _hy) = served(Arc::new(Signer::default()));
    send(&mut x, ask).await;
    callback(&mut x).await;
    send(&mut x, &cb(r#"{"id":1,"ok":{"keys":[]}}"#)).await;
    reply(&mut x).await;
    send(&mut x, ask2).await;
    assert_eq!(callback(&mut x).await["id"], 2);
    send(&mut y, ask).await;
    assert_eq!(callback(&mut y).await["id"], 1);
    // y's id sent on x
    send(&mut x, &cb(r#"{"id":1,"ok":{"keys":[]}}"#)).await;
    assert_reply_cause(&reply(&mut x).await, NoAgentCause::InvalidReply);
    send(&mut y, &cb(r#"{"id":1,"ok":{"keys":[]}}"#)).await;
    assert_eq!(reply(&mut y).await["ok"], json!([]));

    // the caller's side fails closed too: a reply-shaped ask, a repeated id, a raw-sign ask
    let fake = Fake::open(vec![key(1)]);
    let mut link = CallerLink::new(Some(fake.clone()), never_interrupted());
    let never = || std::future::pending::<()>();
    let identities = cb(r#"{"id":1,"op":"identities"}"#);
    let line_of = |v: Option<Vec<u8>>| String::from_utf8(v.expect("a reply")).unwrap();
    assert_eq!(
        line_of(
            link.answer(&identities[..identities.len() - 1], never())
                .await
        ),
        format!(
            "\u{1e}{{\"id\":1,\"ok\":{{\"keys\":[\"{}\"]}}}}\n",
            hex(&key(1))
        )
    );
    let raw_sign = format!(
        r#"{{"id":2,"op":"sign","key":"{}","payload":"{}"}}"#,
        hex(&key(1)),
        hex(b"raw bytes")
    );
    assert_eq!(
        line_of(link.answer(raw_sign.as_bytes(), never()).await),
        "\u{1e}{\"id\":2,\"error\":{\"code\":\"E_NO_AGENT\",\"cause\":\"request refused\"}}\n"
    );
    let good_sign = format!(
        r#"{{"id":3,"op":"sign","key":"{}","payload":"{}"}}"#,
        hex(&key(1)),
        hex(&signed_data())
    );
    assert_eq!(
        line_of(link.answer(good_sign.as_bytes(), never()).await),
        format!(
            "\u{1e}{{\"id\":3,\"ok\":{{\"signature\":\"{}\"}}}}\n",
            hex(&fake_signature(&key(1)))
        )
    );
    assert_eq!(
        line_of(link.answer(br#"{"id":3,"op":"identities"}"#, never()).await),
        "\u{1e}{\"id\":3,\"error\":{\"code\":\"E_NO_AGENT\",\"cause\":\"invalid reply\"}}\n",
        "a repeated id"
    );
    assert_eq!(
        line_of(link.answer(br#"{"id":9,"op":"identities"}"#, never()).await),
        "\u{1e}{\"id\":9,\"error\":{\"code\":\"E_NO_AGENT\",\"cause\":\"invalid reply\"}}\n",
        "closed after a bad ask"
    );
    let mut other = CallerLink::new(Some(fake.clone()), never_interrupted());
    assert_eq!(
        line_of(other.answer(br#"{"id":1,"ok":{"keys":[]}}"#, never()).await),
        "\u{1e}{\"id\":1,\"error\":{\"code\":\"E_NO_AGENT\",\"cause\":\"invalid reply\"}}\n",
        "a reply sent to the caller"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        2,
        "only the two good asks reached the agent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sig5_direct_cli_and_stdio_receive_the_caller_agent() {
    // The caller's own agent, made from its environment: with no SSH_AUTH_SOCK it is
    // E_NO_AGENT, even when private key files sit in the caller's home. Run in a child process
    // so the environment is the child's own.
    if std::env::var_os("RDY_SIG5_CHILD").is_some() {
        let agent = rdy_daemon::signing_agent::CallerAgent::from_env();
        assert_cause(
            &agent.identities().map(|_| Value::Null),
            NoAgentCause::Unset,
        );
        assert_cause(
            &agent.sign(&key(1), &signed_data()).map(|_| Value::Null),
            NoAgentCause::Unset,
        );
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".ssh")).unwrap();
    std::fs::write(
        home.path().join(".ssh/id_ed25519"),
        "not a key, never read\n",
    )
    .unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sig5_direct_cli_and_stdio_receive_the_caller_agent",
        ])
        .env_remove("SSH_AUTH_SOCK")
        .env("HOME", home.path())
        .env("RDY_SIG5_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    // The direct path hands the op the caller's agent; a read never asks it.
    let dir = tempfile::tempdir().unwrap();
    let local = rdy_engine::engine::Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let fake = Fake::open(vec![key(1)]);
    let s = Session::human("human:t".into(), Role::Writer);
    let cx = OpContext {
        engine: &local,
        session: &s,
        human: "human:t",
        cwd: dir.path(),
        agent: &*fake,
    };
    for op in ["status", "ls", "log", "tags"] {
        dispatch(&cx, op, json!({})).unwrap_or_else(|e| panic!("{op}: {e}"));
    }
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "reads never ask the agent"
    );
    assert_eq!(
        cx.agent.identities().unwrap().len(),
        1,
        "the op's agent is the caller's"
    );

    // An MCP state starts with no agent: nothing falls back to private files; a server takes
    // the agent it is given.
    let engine: Arc<dyn Engine> = Arc::new(local);
    let st = rdy_mcp::tools::McpState::new(Arc::clone(&engine));
    assert_cause(
        &st.agent.identities().map(|_| Value::Null),
        NoAgentCause::Unset,
    );
    let server = rdy_mcp::server::RdyServer::new(
        Arc::clone(&engine),
        None,
        rdy_mcp::notify::NotifyHub::new(),
    )
    .with_agent(fake.clone());
    assert_eq!(server.agent().identities().unwrap().len(), 1);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sig5_stdin_eof_preserves_an_inflight_signed_request() {
    let (fake, mut ready) = Fake::gated(vec![key(1)]);
    let mut m = mcp(fake.clone(), 0);
    m.input.write_all(tool_call(1).as_bytes()).await.unwrap();
    recv(&mut ready).await;
    // the host closes stdin while its signed request is in flight
    m.input.shutdown().await.unwrap();
    drop(m.input);
    fake.release();
    assert_eq!(
        m.results.recv().await.unwrap().unwrap(),
        fake_signature(&key(1))
    );
    let mut rest = String::new();
    m.output.read_to_string(&mut rest).await.unwrap();
    let lines: Vec<&str> = rest.lines().collect();
    assert_eq!(lines.len(), 3, "{rest}");
    let v: Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["signature"], hex(&fake_signature(&key(1))));
    assert_eq!(m.proxy.await.unwrap(), 0);
}
