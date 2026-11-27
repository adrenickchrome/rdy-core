//! A loopback TLS hub that speaks the sync protocol (rdyhub wire fixture v2's shapes, and v5's
//! dump step), shared by the `cli_sync` and `mcp_sync` tests and by the sync perf rows
//! ([`crate::sync_perf`]). It serves discovery, state (with views), known (push and pull forms),
//! objects put and get, write, outcome, and dump, and it records every request, so a test
//! compares what each surface sent.
//!
//! A hub records in one of two modes ([`Capture`]). The default keeps every request with its body,
//! as the tests read them. The perf mode keeps counters and at most [`PERF_ENTRIES`] entries of
//! method, path, status and byte counts, and drops each body once its request is handled; it also
//! says the hub takes zstd put bodies and decodes them under the batch limits.
//!
//! The hub keeps no store of its own for a push: it takes each put batch with core's checks,
//! holds its keys, and moves canon (or a view) to the batch's root when a write comes. A pull is
//! served from a project the test hands it with [`Hub::serve`]: known and get read that
//! project's history, as a real hub reads its store. Nothing sleeps: each connection runs on its
//! own thread, and [`Hub::finish`] stops the listener and joins every thread.
#![allow(dead_code)]

#[allow(clippy::duplicate_mod)]
#[path = "../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use crate::platform::set_mode;
use hub_tls::{Ca, LOOPBACK};
use rdy_engine::hub_config::{Credential, CredentialFile, ReadyCredential, encode_credentials};
use rdy_engine::hub_types::{Origin, Secret};
use rdy_engine::{ReadState, now_ms, stream_check, stream_export};
use rdy_model::Hash;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::StreamPurpose;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
pub const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
pub const ACCOUNT: &str = "acc_00000000000000000000000001";
pub const HANDLE: &str = "test-person";
/// The remote's path on the hub, and the sync base discovery names under it.
pub const REMOTE_PATH: &str = "/acme/docs";
pub const SYNC_BASE: &str = "/sync/acme/docs";
const REQUEST_ID: &str = "req_00000000000000000000000009";
const LANDING: u8 = 4;
const VIEW_OP: u8 = 13;

/// What the hub does differently from a plain success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// The write request is refused with this status and hub error body.
    RefuseWrite(u16, String),
    /// The write request's connection closes with no reply: a lost response.
    LoseWrite,
    /// The gate rules hold the write as the landing request of this number, version 1: hub
    /// canon stays (rdyhub-design i-B40VF).
    HoldWrite(u64),
    /// The outcome lookup is refused with this status and hub error body.
    RefuseOutcome(u16, String),
}

/// What a hub keeps of the requests it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Every request with its body, as [`Hub::requests`] returns them. The default.
    Bodies,
    /// Counters and at most [`PERF_ENTRIES`] entries ([`Hub::perf`]); no body is kept after its
    /// request is handled. The state reply says the hub takes zstd put bodies.
    Perf,
}

/// The most entries a perf-mode hub keeps.
pub const PERF_ENTRIES: usize = 4096;

/// The decoded size one put body may reach: the state reply's `batch_bytes`.
const BATCH_BYTES: u64 = 104_857_600;

/// The compressed size one put body may reach (the sender's upload bound).
const WIRE_BYTES: u64 = 2_147_483_648;

/// The keys one known page of a perf-mode hub holds.
const KNOWN_PAGE: usize = 100_000;

/// The error of a perf-mode hub that was sent more than [`PERF_ENTRIES`] requests.
pub const PERF_OVERFLOW: &str = "sync perf request log exceeds 4096 entries";

/// One request of a perf-mode hub: its method, path, the status it was answered with, the bytes
/// of its body on the wire and the bytes of its reply's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerfEntry {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub request_bytes: u64,
    pub reply_bytes: u64,
}

/// What a perf-mode hub counted. `raw_object_bytes` and `wire_object_bytes` add up the objects
/// that moved: each put body decoded and as sent, and each get reply (identity, so equal).
/// `server_elapsed` sums the handler durations of every request: not a share of the client's wall
/// time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerfStats {
    pub entries: Vec<PerfEntry>,
    pub requests: u64,
    pub put_requests: u64,
    pub get_requests: u64,
    pub write_requests: u64,
    /// The known requests that named a base.
    pub known_with_base: u64,
    /// The keys the get requests asked for, in all.
    pub get_keys: u64,
    pub raw_object_bytes: u64,
    pub wire_object_bytes: u64,
    pub server_elapsed: Duration,
}

/// The perf log: the stats, and whether a request came past the bound.
#[derive(Debug, Default)]
pub struct PerfLog {
    stats: PerfStats,
    overflowed: bool,
}

impl PerfLog {
    /// Counts one request and keeps its entry. The request after [`PERF_ENTRIES`] entries is
    /// refused: the log is then over its bound for good, and [`PerfLog::stats`] says so.
    pub fn record(&mut self, entry: PerfEntry, elapsed: Duration) -> Result<(), String> {
        self.stats.requests += 1;
        self.stats.server_elapsed += elapsed;
        if self.stats.entries.len() >= PERF_ENTRIES {
            self.overflowed = true;
            return Err(PERF_OVERFLOW.to_string());
        }
        self.stats.entries.push(entry);
        Ok(())
    }

    /// The stats, or the overflow error: a log that dropped a request never reports success.
    pub fn stats(&self) -> Result<PerfStats, String> {
        if self.overflowed {
            return Err(PERF_OVERFLOW.to_string());
        }
        Ok(self.stats.clone())
    }
}

/// One request the hub read.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The route under the sync base, e.g. `/write`.
    pub fn route(&self) -> &str {
        self.path.strip_prefix(SYNC_BASE).unwrap_or("")
    }
}

/// The hub repo's state.
#[derive(Default)]
pub struct Repo {
    /// Hub canon's tip (a landing hash, hex), or none.
    pub canon: Option<String>,
    pub repo_id: Option<String>,
    /// Each view: root and head view-op hashes (hex).
    pub views: BTreeMap<String, (String, String)>,
    /// The keys of every taken push.
    pub held: BTreeSet<ObjectKey>,
    /// Each push id's quarantined keys, and its batch root.
    pub quarantine: BTreeMap<String, BTreeSet<ObjectKey>>,
    pub roots: BTreeMap<String, Hash>,
    /// Each push id's put bytes so far: a put reply names them, and they never fall.
    pub put_bytes: BTreeMap<String, u64>,
    /// Each taken push: its write kind and tip.
    pub outcomes: BTreeMap<String, (String, String)>,
    /// The project whose history known and get serve.
    pub store: Option<Arc<ReadState>>,
    /// The settings step's reply body; None serves fixture v2's "none" case.
    pub settings: Option<Value>,
    /// The state step's `min_rdy`.
    pub min_rdy: Option<String>,
    /// Hub canon's tip after the settings step: a hub move during a clone.
    pub move_on_settings: Option<String>,
    /// A get batch that leaves out every record of this kind, as if the client held them.
    pub get_omits: Option<u8>,
    /// The dump files the dump step serves, by id; any other id gets wire fixture v5's refusal.
    pub dumps: BTreeMap<String, Vec<u8>>,
    /// A perf-mode hub's last history closure, by purpose and root: every get of one pull walks
    /// the same history, so it is walked once. A new served project clears it.
    closure: Option<(String, Hash, Arc<Vec<ObjectKey>>)>,
    /// When set, the state reply lists stream format 5 and known and get of purpose
    /// `canon_state` serve the canon tip's state (D-GGTYA), prepared once for its root.
    pub canon_state: bool,
    prepared: Option<Arc<rdy_engine::canon_state_export::PreparedCanonState>>,
}

pub struct Shared {
    config: Arc<ServerConfig>,
    pub origin: String,
    capture: Capture,
    log: Mutex<Vec<Req>>,
    perf: Mutex<PerfLog>,
    fault: Mutex<Fault>,
    pub repo: Mutex<Repo>,
    /// The next history get waits here, once: it says it arrived, then waits for the release
    /// (or for the hold to be dropped). Taken by that get, before the repo lock.
    history_hold: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

/// A hold on the hub's next history get ([`Hub::hold_history_get`]).
pub struct HistoryHold {
    arrived: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl HistoryHold {
    /// Waits until the held get arrived; panics after a minute (a get that never came).
    pub fn wait_arrived(&self) {
        self.arrived
            .recv_timeout(Duration::from_secs(60))
            .expect("the held history get arrived");
    }

    /// Lets the held get be answered.
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

pub struct Hub {
    pub shared: Arc<Shared>,
    pub ca_pem: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Hub {
    pub fn start() -> Hub {
        Hub::start_with(Capture::Bodies)
    }

    /// A hub that records as `capture` says.
    pub fn start_with(capture: Capture) -> Hub {
        let ca = Ca::new();
        let (chain, key) = ca.leaf(&[LOOPBACK]);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("server certificate");
        let listener = TcpListener::bind((LOOPBACK, 0)).expect("bind loopback");
        let addr = listener.local_addr().expect("address");
        let shared = Arc::new(Shared {
            config: Arc::new(config),
            origin: format!("https://{LOOPBACK}:{}", addr.port()),
            capture,
            log: Mutex::new(Vec::new()),
            perf: Mutex::new(PerfLog::default()),
            fault: Mutex::new(Fault::None),
            repo: Mutex::new(Repo::default()),
            history_hold: Mutex::new(None),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let workers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let acceptor = {
            let (shared, stop, workers) =
                (Arc::clone(&shared), Arc::clone(&stop), Arc::clone(&workers));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    let worker = std::thread::spawn(move || serve(stream, &shared));
                    workers.lock().unwrap().push(worker);
                }
            })
        };
        Hub {
            shared,
            ca_pem: ca.pem().to_string(),
            addr,
            stop,
            acceptor: Some(acceptor),
            workers,
        }
    }

    pub fn origin(&self) -> &str {
        &self.shared.origin
    }

    pub fn fault(&self, f: Fault) {
        *self.shared.fault.lock().unwrap() = f;
    }

    /// The hub repo holds canon at `tip` (a landing hash) and has this repo id.
    pub fn set_canon(&self, tip: Option<Hash>, repo_id: Option<String>) {
        let mut r = self.shared.repo.lock().unwrap();
        r.canon = tip.map(|h| h.to_string());
        r.repo_id = repo_id;
    }

    /// Serves known and get from `read`'s history.
    pub fn serve(&self, read: Arc<ReadState>) {
        let mut repo = self.shared.repo.lock().unwrap();
        repo.store = Some(read);
        repo.closure = None;
        repo.prepared = None;
    }

    /// Holds the next history get (any purpose but `canon_state`) before it is answered.
    pub fn hold_history_get(&self) -> HistoryHold {
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        *self.shared.history_hold.lock().unwrap() = Some((arrived_tx, release_rx));
        HistoryHold { arrived, release }
    }

    /// The hub sends a canon tip's state (stream format 5): a clone from it is ready before
    /// its history arrives.
    pub fn send_canon_state(&self) {
        self.shared.repo.lock().unwrap().canon_state = true;
    }

    /// The settings step replies with `body`.
    pub fn settings(&self, body: Value) {
        self.shared.repo.lock().unwrap().settings = Some(body);
    }

    /// The state step names `min_rdy`.
    pub fn min_rdy(&self, v: &str) {
        self.shared.repo.lock().unwrap().min_rdy = Some(v.to_string());
    }

    /// Hub canon moves to `tip` once the settings step is served.
    pub fn move_on_settings(&self, tip: Hash) {
        self.shared.repo.lock().unwrap().move_on_settings = Some(tip.to_string());
    }

    /// The dump step serves `bytes` for the dump id `id` (D-7CDEQ).
    pub fn dump(&self, id: &str, bytes: Vec<u8>) {
        self.shared
            .repo
            .lock()
            .unwrap()
            .dumps
            .insert(id.to_string(), bytes);
    }

    /// Each get batch leaves out the records of `kind`.
    pub fn get_omits(&self, kind: u8) {
        self.shared.repo.lock().unwrap().get_omits = Some(kind);
    }

    /// The routes of every request so far, in order (`discovery` for the remote's GET).
    pub fn routes(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| match r.route() {
                "" => "discovery".to_string(),
                route => route.trim_start_matches('/').to_string(),
            })
            .collect()
    }

    /// Hub canon's tip now.
    pub fn canon(&self) -> Option<String> {
        self.shared.repo.lock().unwrap().canon.clone()
    }

    /// Every request read so far, in order. A perf-mode hub keeps none ([`Hub::perf`]).
    pub fn requests(&self) -> Vec<Req> {
        self.shared.log.lock().unwrap().clone()
    }

    /// What a perf-mode hub counted. It is an error for a hub of the default mode, and for one
    /// that was sent more than [`PERF_ENTRIES`] requests.
    pub fn perf(&self) -> Result<PerfStats, String> {
        if self.shared.capture != Capture::Perf {
            return Err("this hub keeps request bodies, not perf counters".to_string());
        }
        self.shared.perf.lock().unwrap().stats()
    }

    /// The requests of one route, e.g. `/write`.
    pub fn of(&self, route: &str) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.route() == route)
            .collect()
    }

    pub fn finish(mut self) {
        self.stop_threads();
    }

    fn stop_threads(&mut self) {
        let Some(acceptor) = self.acceptor.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        acceptor.join().expect("acceptor");
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for w in workers {
            w.join().expect("worker");
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop_threads();
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_request(tls: &mut StreamOwned<ServerConnection, TcpStream>) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8(buf[..end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next()?.splitn(3, ' ');
    let method = start.next()?.to_string();
    let target = start.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .map_or(0, |(_, v)| v.parse::<usize>().expect("content-length"));
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q),
        None => (target.clone(), ""),
    };
    let query = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Some(Req {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Status",
    }
}

fn reply_bytes(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn reply(status: u16, body: &Value) -> Vec<u8> {
    reply_bytes(status, "application/json", body.to_string().as_bytes())
}

/// The hub's own error object.
fn hub_error(status: u16, code: &str, message: &str, fix: &str) -> Vec<u8> {
    reply(
        status,
        &json!({"code": code, "message": message, "fix": fix, "issues": [],
            "request_id": REQUEST_ID}),
    )
}

fn serve(stream: TcpStream, shared: &Shared) {
    let Ok(conn) = ServerConnection::new(Arc::clone(&shared.config)) else {
        return;
    };
    let mut tls = StreamOwned::new(conn, stream);
    let Some(req) = read_request(&mut tls) else {
        let _ = tls.sock.shutdown(Shutdown::Both);
        return;
    };
    if req.route() == "/objects/get" && req.json()["purpose"] != "canon_state" {
        let hold = shared.history_hold.lock().unwrap().take();
        if let Some((arrived, release)) = hold {
            let _ = arrived.send(());
            // a dropped hold releases the get too
            let _ = release.recv();
        }
    }
    let bytes = match shared.capture {
        Capture::Bodies => {
            shared.log.lock().unwrap().push(req.clone());
            answer(&req, shared)
        }
        Capture::Perf => perf_answer(req, shared),
    };
    if let Some(bytes) = bytes {
        let _ = tls.write_all(&bytes);
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.conn.complete_io(&mut tls.sock);
    }
    let _ = tls.sock.shutdown(Shutdown::Both);
}

/// The status line's code of a reply.
fn status_of(reply: &[u8]) -> u16 {
    std::str::from_utf8(reply.get(9..12).unwrap_or_default())
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// The bytes of a reply's body.
fn body_len_of(reply: &[u8]) -> u64 {
    find(reply, b"\r\n\r\n").map_or(0, |i| (reply.len() - i - 4) as u64)
}

/// A put body, decoded: one zstd frame (`Content-Encoding: zstd`) under the batch limit, or the
/// identity body as sent. The compressed size is bounded by the sender's upload bound and the
/// decoded size by `batch_bytes`; a body over either, or a frame that does not decode, is refused.
fn decode_put(req: &mut Req) -> Result<(), Vec<u8>> {
    let encoding = req
        .headers
        .iter()
        .find(|(n, _)| n == "content-encoding")
        .map(|(_, v)| v.to_ascii_lowercase());
    let refuse = |message: &str| {
        hub_error(
            422,
            "E_BAD_INPUT",
            message,
            "send the batch as one zstd frame within the batch bytes",
        )
    };
    match encoding.as_deref() {
        None | Some("identity") => {
            if req.body.len() as u64 > BATCH_BYTES {
                return Err(refuse("the batch is over the batch bytes"));
            }
            Ok(())
        }
        Some("zstd") => {
            if req.body.len() as u64 > WIRE_BYTES {
                return Err(refuse("the compressed body is over the upload bound"));
            }
            let mut decoder = zstd::stream::read::Decoder::new(&req.body[..])
                .map_err(|_| refuse("the body is not a zstd frame"))?;
            let mut out = Vec::new();
            decoder
                .by_ref()
                .take(BATCH_BYTES + 1)
                .read_to_end(&mut out)
                .map_err(|_| refuse("the body is not one valid zstd frame"))?;
            if out.len() as u64 > BATCH_BYTES {
                return Err(refuse("the batch is over the batch bytes"));
            }
            req.body = out;
            Ok(())
        }
        Some(_) => Err(refuse("the body encoding is not taken")),
    }
}

/// One request of a perf-mode hub: the put body is decoded, the request is answered and counted,
/// and no body outlives the call.
fn perf_answer(mut req: Req, shared: &Shared) -> Option<Vec<u8>> {
    let started = Instant::now();
    let route = req.route().to_string();
    let (method, path) = (req.method.clone(), req.path.clone());
    let wire = req.body.len() as u64;
    let mut base_named = false;
    let mut keys = 0u64;
    let reply = if route == "/objects/put" {
        match decode_put(&mut req) {
            Ok(()) => answer(&req, shared),
            Err(refusal) => Some(refusal),
        }
    } else {
        if route == "/known" {
            base_named = !req.json()["base"].is_null();
        }
        if route == "/objects/get" {
            keys = req.json()["keys"].as_array().map_or(0, |k| k.len() as u64);
        }
        answer(&req, shared)
    };
    let decoded = req.body.len() as u64;
    drop(req);
    let (status, reply_bytes) = reply
        .as_ref()
        .map_or((0, 0), |r| (status_of(r), body_len_of(r)));
    let elapsed = started.elapsed();
    let mut log = shared.perf.lock().unwrap();
    let entry = PerfEntry {
        method,
        path,
        status,
        request_bytes: wire,
        reply_bytes,
    };
    if log.record(entry, elapsed).is_err() {
        // the log is over its bound for good: this request is refused, never counted as success
        return Some(hub_error(
            500,
            "E_INTERNAL",
            PERF_OVERFLOW,
            "send the hub fewer requests",
        ));
    }
    let stats = &mut log.stats;
    match route.as_str() {
        "/objects/put" => {
            stats.put_requests += 1;
            stats.raw_object_bytes += decoded;
            stats.wire_object_bytes += wire;
        }
        "/objects/get" => {
            stats.get_requests += 1;
            stats.get_keys += keys;
            stats.raw_object_bytes += reply_bytes;
            stats.wire_object_bytes += reply_bytes;
        }
        "/write" => stats.write_requests += 1,
        "/known" if base_named => stats.known_with_base += 1,
        _ => {}
    }
    reply
}

fn key_json(kind: u8, hash: &str) -> Value {
    json!({"hash": hash, "kind": kind})
}

/// The canon state at `root` of the served project, prepared on its first use.
fn prepared_state(
    repo: &mut Repo,
    read: &Arc<ReadState>,
    root: Hash,
) -> Result<Arc<rdy_engine::canon_state_export::PreparedCanonState>, rdy_engine::RdyError> {
    if let Some(p) = repo.prepared.as_ref().filter(|p| p.root() == root) {
        return Ok(Arc::clone(p));
    }
    let p = Arc::new(rdy_engine::canon_state_export::PreparedCanonState::new(
        Arc::clone(read),
        root,
    )?);
    repo.prepared = Some(Arc::clone(&p));
    Ok(p)
}

fn purpose_of(name: &str) -> StreamPurpose {
    match name {
        "view_history" => StreamPurpose::ViewHistory,
        _ => StreamPurpose::CanonHistory,
    }
}

/// The hub store's history of `purpose` from `root` above `base`, in export order.
fn history(
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
) -> Vec<ObjectKey> {
    let found = rdy_store::closure::history_closure_before(
        read.reader(),
        purpose,
        root,
        &BTreeSet::new(),
        base,
        read.seq(),
    )
    .expect("a history closure");
    found.records.to_vec()
}

/// [`history`] for a request: a perf-mode hub keeps the last whole history it walked, since every
/// get of one pull asks for the same one; the default mode walks each time.
fn history_of(
    repo: &mut Repo,
    capture: Capture,
    read: &ReadState,
    purpose: StreamPurpose,
    root: Hash,
    base: Option<Hash>,
) -> Arc<Vec<ObjectKey>> {
    if capture != Capture::Perf || base.is_some() {
        return Arc::new(history(read, purpose, root, base));
    }
    let name = purpose.name().to_string();
    if let Some((n, r, keys)) = &repo.closure
        && *n == name
        && *r == root
    {
        return Arc::clone(keys);
    }
    let keys = Arc::new(history(read, purpose, root, None));
    repo.closure = Some((name, root, Arc::clone(&keys)));
    keys
}

fn parse_hash(v: &Value) -> Option<Hash> {
    v.as_str().and_then(|s| s.parse().ok())
}

fn answer(req: &Req, shared: &Shared) -> Option<Vec<u8>> {
    let fault = shared.fault.lock().unwrap().clone();
    let mut repo = shared.repo.lock().unwrap();
    match (req.method.as_str(), req.path.as_str(), req.route()) {
        ("GET", REMOTE_PATH, _) => Some(reply(
            200,
            &json!({"sync_base": SYNC_BASE, "protocols": [1]}),
        )),
        ("POST", _, "/state") => {
            let r = req.json();
            let views: Vec<Value> = r["views"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|name| {
                    let held = repo.views.get(name).cloned().or_else(|| {
                        // a view of the served project is on the hub too
                        let read = repo.store.as_ref()?;
                        let head = read.view_head(name)?.head;
                        let mut root = head;
                        while let Some(prev) = read.reader().view_op(&root).ok()?.prev {
                            root = prev;
                        }
                        Some((root.to_string(), head.to_string()))
                    });
                    match held {
                        Some((root, head)) => json!({
                            "name": name, "root": key_json(VIEW_OP, &root),
                            "head": key_json(VIEW_OP, &head),
                        }),
                        None => json!({"name": name, "root": null, "head": null}),
                    }
                })
                .collect();
            let streams = if repo.canon_state {
                json!([2, 4, 5])
            } else {
                json!([2, 4])
            };
            let mut state = json!({"canon": repo.canon.as_deref().map(|h| key_json(LANDING, h)),
                "repo_id": repo.repo_id, "rdy": r["rdy"],
                "stream_read": streams, "stream_write": streams, "features": [],
                "min_rdy": repo.min_rdy, "batch_bytes": BATCH_BYTES,
                "batch_records": 100_000, "quarantine_seconds": 86_400,
                "profile": null, "audience": null, "artifact_bytes": 1_073_741_824u64,
                "artifact_files": 32_000, "views": views});
            if shared.capture == Capture::Perf {
                // a perf-mode hub takes zstd put bodies (D-SEGWK)
                state["zstd"] = json!(true);
            }
            Some(reply(200, &state))
        }
        ("POST", _, "/known") => {
            let r = req.json();
            if r["push"].is_string() {
                let push = r["push"].as_str().unwrap_or_default();
                let mine = repo.quarantine.get(push);
                let missing: Vec<Value> = r["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| serde_json::from_value::<ObjectKey>(k.clone()).ok())
                    .filter(|k| !repo.held.contains(k) && !mine.is_some_and(|m| m.contains(k)))
                    .map(|k| key_json(k.kind, &k.hash.to_string()))
                    .collect();
                return Some(reply(200, &json!({"missing": missing})));
            }
            let Some(read) = repo.store.clone() else {
                return Some(hub_error(
                    404,
                    "E_HUB_NOT_FOUND",
                    "hub repo acme/docs holds no such record",
                    "run rdy pull again; it reads the hub's state first",
                ));
            };
            let root: ObjectKey = serde_json::from_value(r["root"].clone()).ok()?;
            let base = serde_json::from_value::<ObjectKey>(r["base"].clone())
                .ok()
                .filter(|b| read.reader().has_kind(b.kind, &b.hash).unwrap_or(false))
                .map(|b| b.hash);
            if r["purpose"] == "canon_state" {
                let after = serde_json::from_value::<ObjectKey>(r["after"].clone()).ok();
                return Some(
                    match prepared_state(&mut repo, &read, root.hash)
                        .and_then(|p| p.page(after, rdy_engine::history_facts::MAX_KEYS))
                    {
                        Ok(page) => {
                            let keys: Vec<Value> = page
                                .keys
                                .iter()
                                .map(|k| key_json(k.kind, &k.hash.to_string()))
                                .collect();
                            let next = page.next.map(|k| key_json(k.kind, &k.hash.to_string()));
                            reply(200, &json!({"keys": keys, "next": next}))
                        }
                        Err(e) => hub_error(422, "E_BAD_INPUT", &e.message, &e.fix),
                    },
                );
            }
            let purpose = purpose_of(r["purpose"].as_str().unwrap_or("canon_history"));
            let all = history_of(&mut repo, shared.capture, &read, purpose, root.hash, base);
            if shared.capture == Capture::Perf {
                // pages of the history's keys, after the cursor the client sends
                let start = match serde_json::from_value::<ObjectKey>(r["after"].clone()).ok() {
                    None => 0,
                    Some(k) => match all.iter().position(|x| *x == k) {
                        Some(i) => i + 1,
                        None => {
                            return Some(hub_error(
                                422,
                                "E_BAD_INPUT",
                                "the cursor names no key of this history",
                                "ask again from the start",
                            ));
                        }
                    },
                };
                let rest = &all[start..];
                let page = &rest[..rest.len().min(KNOWN_PAGE)];
                let keys: Vec<Value> = page
                    .iter()
                    .map(|k| key_json(k.kind, &k.hash.to_string()))
                    .collect();
                let next = (rest.len() > page.len())
                    .then(|| page.last().map(|k| key_json(k.kind, &k.hash.to_string())))
                    .flatten();
                return Some(reply(200, &json!({"keys": keys, "next": next})));
            }
            let keys: Vec<Value> = all
                .iter()
                .map(|k| key_json(k.kind, &k.hash.to_string()))
                .collect();
            Some(reply(200, &json!({"keys": keys, "next": null})))
        }
        ("POST", _, "/objects/put") => {
            let push = req.query.get("push").cloned().unwrap_or_default();
            let report = match stream_check::check_batch(&req.body[..], None) {
                Ok(r) => r,
                Err(e) => return Some(hub_error(422, "E_BAD_INPUT", &e.message, &e.fix)),
            };
            repo.roots.insert(push.clone(), report.header.tag);
            let total = {
                let t = repo.put_bytes.entry(push.clone()).or_default();
                *t += req.body.len() as u64;
                *t
            };
            let mine = repo.quarantine.entry(push).or_default();
            mine.extend(report.keys.iter().copied());
            let records = mine.len();
            // a hub replies the push's bytes so far, not this put's
            Some(reply(200, &json!({"records": records, "bytes": total})))
        }
        ("POST", _, "/objects/get") => {
            let r = req.json();
            let read = repo.store.clone()?;
            let root: ObjectKey = serde_json::from_value(r["root"].clone()).ok()?;
            if r["purpose"] == "canon_state" {
                let keys: Vec<ObjectKey> = r["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| serde_json::from_value(k.clone()).ok())
                    .collect();
                let mut out = Vec::new();
                return Some(
                    match prepared_state(&mut repo, &read, root.hash)
                        .and_then(|p| p.write_keys(&keys, &mut out).map(drop))
                    {
                        Ok(()) => reply_bytes(200, "application/octet-stream", &out),
                        Err(e) => hub_error(422, "E_BAD_INPUT", &e.message, &e.fix),
                    },
                );
            }
            let purpose = purpose_of(r["purpose"].as_str().unwrap_or("canon_history"));
            if shared.capture == Capture::Perf {
                // exactly the named records, as a hub's get reads them; a landing and its
                // snapshot delta may travel in separate batches
                let keys: Vec<ObjectKey> = r["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| serde_json::from_value(k.clone()).ok())
                    .collect();
                let mut out = Vec::new();
                return Some(
                    match stream_export::write_history_keys(
                        &read, purpose, root.hash, &keys, &mut out,
                    ) {
                        Ok(_) => reply_bytes(200, "application/octet-stream", &out),
                        Err(e) => hub_error(422, "E_BAD_INPUT", &e.message, &e.fix),
                    },
                );
            }
            let want: BTreeSet<ObjectKey> = r["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| serde_json::from_value(k.clone()).ok())
                .collect();
            let all: BTreeSet<ObjectKey> =
                history_of(&mut repo, shared.capture, &read, purpose, root.hash, None)
                    .iter()
                    .copied()
                    .collect();
            let mut known: BTreeSet<ObjectKey> = all.difference(&want).copied().collect();
            if let Some(kind) = repo.get_omits {
                known.extend(all.iter().filter(|k| k.kind == kind).copied());
            }
            let mut out = Vec::new();
            stream_export::write_history(&read, purpose, root.hash, None, &known, &mut out).ok()?;
            Some(reply_bytes(200, "application/octet-stream", &out))
        }
        ("POST", _, "/write") => match fault {
            Fault::RefuseWrite(status, body) => {
                Some(reply_bytes(status, "application/json", body.as_bytes()))
            }
            Fault::LoseWrite => None,
            Fault::HoldWrite(number) => {
                let r = req.json();
                let push = r["push"].as_str().unwrap_or_default();
                let q = repo.quarantine.remove(push).unwrap_or_default();
                repo.held.extend(q);
                let tip = repo.canon.as_deref().map(|t| key_json(LANDING, t));
                Some(reply(
                    200,
                    &json!({"outcome": "held", "tip": tip,
                        "request": {"number": number, "version": 1}, "min_rdy_raised": false}),
                ))
            }
            _ => {
                let r = req.json();
                let push = r["push"].as_str().unwrap_or_default().to_string();
                let view = r["view"].as_str().map(str::to_string);
                let head = repo
                    .roots
                    .get(&push)
                    .map(ToString::to_string)
                    .or_else(|| r["expected"]["hash"].as_str().map(str::to_string))?;
                let expected = r["expected"]["hash"].as_str().map(str::to_string);
                let (kind, current) = match &view {
                    Some(v) => (VIEW_OP, repo.views.get(v).map(|x| x.1.clone())),
                    None => (LANDING, repo.canon.clone()),
                };
                let forced = matches!(r["kind"].as_str(), Some("force" | "restore"));
                if !forced && expected != current {
                    return Some(hub_error(
                        409,
                        "E_HUB_BEHIND",
                        "Hub canon's tip, or the view's head, moved since the client read it.",
                        "Pull, then push again.",
                    ));
                }
                let q = repo.quarantine.remove(&push).unwrap_or_default();
                repo.held.extend(q);
                match &view {
                    Some(v) => {
                        let root = repo
                            .views
                            .get(v)
                            .map(|x| x.0.clone())
                            .unwrap_or_else(|| head.clone());
                        repo.views.insert(v.clone(), (root, head.clone()));
                    }
                    None => repo.canon = Some(head.clone()),
                }
                repo.outcomes.insert(
                    push,
                    (
                        r["kind"].as_str().unwrap_or("landings").to_string(),
                        head.clone(),
                    ),
                );
                Some(reply(
                    200,
                    &json!({"outcome": "taken", "tip": key_json(kind, &head),
                        "request": null, "min_rdy_raised": false}),
                ))
            }
        },
        // the dump step (wire fixture v5): the file, or the hub's refusal of a dump that does
        // not exist, is not ready or another account started
        ("POST", _, "/dump") => {
            let id = req.json()["dump"].as_str().unwrap_or_default().to_string();
            Some(match repo.dumps.get(&id) {
                Some(bytes) => reply_bytes(200, "application/octet-stream", bytes),
                None => hub_error(
                    404,
                    "E_HUB_NOT_FOUND",
                    &format!("no dump {id} of yours in acme/docs"),
                    "start a dump with dumps.create, then wait for it to succeed",
                ),
            })
        }
        ("POST", _, "/settings") => {
            let body = repo.settings.clone().unwrap_or_else(|| settings_step(5));
            if let Some(tip) = repo.move_on_settings.take() {
                repo.canon = Some(tip);
            }
            Some(reply(200, &body))
        }
        ("POST", _, "/outcome") => {
            if let Fault::RefuseOutcome(status, body) = fault {
                return Some(reply_bytes(status, "application/json", body.as_bytes()));
            }
            let r = req.json();
            let push = r["push"].as_str().unwrap_or_default();
            Some(reply(
                200,
                &match repo.outcomes.get(push) {
                    Some((kind, tip)) => json!({"outcome": "taken", "kind": kind,
                        "tip": key_json(LANDING, tip), "code": null, "request": null,
                        "request_state": null, "view": null}),
                    None => json!({"outcome": "unknown", "kind": null, "tip": null,
                        "code": null, "request": null, "request_state": null, "view": null}),
                },
            ))
        }
        _ => Some(reply(404, &json!({}))),
    }
}

/// The steps of rdyhub's wire fixture v2.
pub fn fixture_v2() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sync/sync-client-wire-v2.json");
    serde_json::from_slice(&std::fs::read(path).expect("fixture v2")).expect("fixture v2 JSON")
}

/// The settings reply body of fixture v2's step `i` (5 "none", 12 "mismatch", 19 "pack").
pub fn settings_step(i: usize) -> Value {
    let f = fixture_v2();
    let step = &f["steps"][i];
    assert_eq!(step["step"], "settings", "step {i}");
    step["reply"]["body"].clone()
}

/// A private user config home: `rdy/config.yaml` names the person and trusts the hub's CA; the
/// directories are 0700.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new(hub: &Hub) -> Home {
        let dir = tempfile::tempdir().expect("tempdir");
        set_mode(dir.path(), 0o700).unwrap();
        std::fs::create_dir(dir.path().join("rdy")).unwrap();
        set_mode(&dir.path().join("rdy"), 0o700).unwrap();
        let pem = dir.path().join("ca.pem");
        std::fs::write(&pem, &hub.ca_pem).unwrap();
        std::fs::write(
            dir.path().join("rdy/config.yaml"),
            format!(
                "actor: human:qa\nhub:\n  installs:\n    \"{}\":\n      ca_file: {}\n",
                hub.origin(),
                pem.display()
            ),
        )
        .unwrap();
        Home { dir }
    }

    pub fn config(&self) -> PathBuf {
        self.dir.path().join("rdy/config.yaml")
    }

    pub fn creds(&self) -> PathBuf {
        self.dir.path().join("rdy/credentials.yaml")
    }

    pub fn env(&self) -> Vec<(String, String)> {
        let home = self.dir.path().to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".into(), home.clone()),
            ("HOME".into(), home),
            ("USER".into(), "qa".into()),
        ]
    }

    /// A ready sign-in to `origin`, fresh for 50 minutes.
    pub fn signed_in(&self, origin: &str) {
        let mut file = CredentialFile::default();
        file.set(
            Origin::parse(origin).unwrap(),
            Some(Credential::Ready(ReadyCredential {
                access_token: Secret::new(ACCESS),
                refresh_token: Secret::new(REFRESH),
                expires_at_ms: now_ms() + 3_000_000,
                account: ACCOUNT.into(),
                handle: HANDLE.into(),
            })),
        );
        std::fs::write(self.creds(), encode_credentials(&file).unwrap()).unwrap();
        set_mode(&self.creds(), 0o600).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Projects, commands and chat clients
// ---------------------------------------------------------------------------------------------

use crate::fixtures::{land, write};
use crate::mcp::{McpClient, Reply as McpReply};
use rdy_engine::Rdy;
use rdy_engine::hub_config::RemoteMode;
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Writes the doc `name` in view `work` and lands it; returns the landing.
pub fn land_doc(r: &mut Rdy, name: &str) -> Hash {
    write(
        r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    r.read().canon_head().expect("a landing")
}

/// Adds the share remote `origin` at `url` to the project at `project`, as `home`'s user.
pub fn add_share_remote(
    project: &Path,
    home: &Home,
    url: &str,
) -> Result<(), rdy_engine::RdyError> {
    RemoteService::new(project, &home.config())
        .add("origin", url, RemoteMode::Share, RemoteLevel::Project)
        .map(|_| ())
}

/// A served project in a short directory with the landings of the docs `docs`, oldest first.
pub struct Proj {
    pub dir: tempfile::TempDir,
    pub landings: Vec<Hash>,
}

impl Proj {
    pub fn new(docs: &[&str]) -> Proj {
        let dir = crate::daemon::short_tempdir();
        let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
        let landings = docs.iter().map(|d| land_doc(&mut r, d)).collect();
        r.close().unwrap();
        let cfg = dir.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).unwrap();
        Proj { dir, landings }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Lands the docs `docs` more in the project (its daemon must not run).
    pub fn land_more(&mut self, docs: &[&str]) {
        let mut r = Rdy::open(self.path()).unwrap();
        for d in docs {
            self.landings.push(land_doc(&mut r, d));
        }
        r.close().unwrap();
    }

    /// The project's repo id.
    pub fn repo_id(&self) -> String {
        let r = Rdy::open(self.path()).unwrap();
        let id = r.read().repo_id().unwrap().to_string();
        r.close().unwrap();
        id
    }

    /// Adds the share remote `origin` at `url`, as `home`'s user.
    pub fn remote(&self, home: &Home, url: &str) {
        add_share_remote(self.path(), home, url).unwrap();
    }

    /// Stops the project's daemon, if one runs.
    pub fn stop_daemon(&self, home: &Home) {
        let _ = command(self.path(), home, &["serve", "--stop"]).output();
    }

    /// A copy of the project as it stands (its daemon stopped first): the same repo, which the
    /// hub serves as its store. Returns the copy's directory and its open project.
    pub fn copy(&self, home: &Home) -> (tempfile::TempDir, Rdy) {
        self.stop_daemon(home);
        fn copy_dir(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let p = entry.path();
                if p.is_dir() {
                    copy_dir(&p, &to.join(entry.file_name()));
                } else {
                    std::fs::copy(&p, to.join(entry.file_name())).unwrap();
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("hub");
        copy_dir(self.path(), &to);
        let _ = std::fs::remove_file(to.join(".rdy/daemon.json"));
        let r = Rdy::open(&to).unwrap();
        (dir, r)
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = crate::fixtures::rdy_command()
            .env_clear()
            .envs(crate::fixtures::coverage_env())
            .arg("--project")
            .arg(self.dir.path())
            .args(["serve", "--stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A hub, a home that trusts it and is signed in there, and a project with the share remote
/// `origin` on it.
pub struct Kit {
    pub hub: Hub,
    pub home: Home,
    pub proj: Proj,
}

pub fn kit(docs: &[&str]) -> Kit {
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let proj = Proj::new(docs);
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    Kit { hub, home, proj }
}

/// `rdy <args>` in the directory `dir`, outside every project, as the person of `home`, with
/// nothing else of this process.
pub fn command_in(dir: &Path, home: &Home, args: &[&str]) -> Command {
    let mut cmd = crate::fixtures::rdy_command();
    cmd.env_clear()
        .envs(crate::fixtures::coverage_env())
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .envs(std::env::var_os("TEMP").map(|dir| ("TEMP", dir)))
        .envs(std::env::var_os("TMP").map(|dir| ("TMP", dir)))
        .envs(home.env())
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null());
    cmd
}

/// `rdy --project <p> <args>` as the person of `home`, with nothing else of this process.
pub fn command(p: &Path, home: &Home, args: &[&str]) -> Command {
    let mut cmd = crate::fixtures::rdy_command();
    cmd.env_clear()
        .envs(crate::fixtures::coverage_env())
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .envs(std::env::var_os("TEMP").map(|dir| ("TEMP", dir)))
        .envs(std::env::var_os("TMP").map(|dir| ("TMP", dir)))
        .envs(home.env())
        .arg("--project")
        .arg(p)
        .args(args)
        .current_dir(p)
        .stdin(Stdio::null());
    cmd
}

/// Runs `rdy <args>` for the kit's person and checks that no output holds a token.
pub fn run(k: &Kit, args: &[&str]) -> Output {
    let o = command(k.proj.path(), &k.home, args)
        .output()
        .expect("run rdy");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    for s in [ACCESS, REFRESH, "Bearer"] {
        assert!(!all.contains(s), "rdy {args:?} printed {s}:\n{all}");
    }
    o
}

/// The JSON of a `--json` run: stdout on success, the error on stderr's last line on failure.
pub fn parsed(o: &Output) -> (bool, Value) {
    let ok = o.status.success();
    let text = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        serde_json::from_str(text.trim().lines().last().unwrap_or(""))
            .unwrap_or(Value::String(text))
    });
    (ok, v)
}

/// `rdy --json <args>`: its JSON, which must be a success.
pub fn cli_ok(k: &Kit, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = parsed(&run(k, &all));
    assert!(ok, "rdy {args:?}: {v}");
    v
}

/// `rdy --json <args>`: its error, which must be a failure.
pub fn cli_err(k: &Kit, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = parsed(&run(k, &all));
    assert!(!ok, "rdy {args:?} succeeded: {v}");
    v
}

/// The text of a run (stdout and stderr), which must be a success.
pub fn cli_text(k: &Kit, args: &[&str]) -> String {
    let o = run(k, args);
    assert!(
        o.status.success(),
        "rdy {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// An MCP client of the kit's daemon (`rdy mcp`, the proxy), with a session of `role` and the
/// agent identity `agent:claude` unless `role` is `None`.
pub fn chat(k: &Kit, role: Option<&str>) -> McpClient {
    let mut c = McpClient::start_proxy_capturing(k.proj.path(), &k.home.env());
    if let Some(role) = role {
        let r = c.call(
            "rdy_session",
            json!({"actor": "agent:claude", "run": "r7", "role": role}),
        );
        assert!(r.is_ok(), "{r:?}");
    }
    c
}

pub fn mcp_ok(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        McpReply::Ok(v) => v,
        other => panic!("{tool} {args}: {other:?}"),
    }
}

pub fn mcp_err(c: &mut McpClient, tool: &str, args: Value) -> Value {
    match c.call(tool, args.clone()) {
        McpReply::ToolError(v) => v,
        other => panic!("{tool} {args}: expected a refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// URL clones
// ---------------------------------------------------------------------------------------------

/// The hub URL of the kit's remote, in canonical form.
pub fn hub_url(k: &Kit) -> String {
    format!("{}{REMOTE_PATH}", k.hub.origin())
}

/// The hub serves a copy of the kit's project as hub canon, with its repo id, after `more`
/// landings in the copy. Keep the returned copy while the hub serves it.
pub fn serve_copy(k: &Kit, more: &[&str]) -> (tempfile::TempDir, Rdy) {
    let (dir, mut r) = k.proj.copy(&k.home);
    for d in more {
        land_doc(&mut r, d);
    }
    k.hub.serve(r.read());
    k.hub
        .set_canon(r.read().canon_head(), Some(k.proj.repo_id()));
    (dir, r)
}

/// Every entry under `dir`, by relative path, with a file's bytes (a directory's are empty);
/// None when `dir` is absent.
pub fn tree(dir: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let p = entry.unwrap().path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(root, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    if !dir.exists() {
        return None;
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    Some(out)
}

/// The routes a URL clone may send: hub read steps only.
pub const READ_ROUTES: [&str; 5] = ["discovery", "state", "settings", "known", "objects/get"];

/// The fetch routes: none follows a refused settings read.
pub fn fetches(k: &Kit) -> usize {
    k.hub
        .routes()
        .iter()
        .filter(|r| *r == "known" || *r == "objects/get")
        .count()
}

/// The built-in pack as the render inputs carry it: resolved, in full.
pub fn builtin_pack() -> Value {
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    serde_json::to_value(pack).unwrap()
}

/// This rdy's built-in pack in full, named `name`, as another engine would capture it: the
/// prose type lacks the `planned` field this engine gives every content type. Its digest is not
/// the built-in pack's, and its source form resolves here to another pack, so a clone of it
/// refuses whatever wire fixture is pinned.
pub fn other_engine_pack(name: &str) -> Value {
    let mut pack = builtin_pack();
    pack["name"] = json!(name);
    let fields = pack["block_types"]["prose"]["fields"]
        .as_object_mut()
        .unwrap();
    assert!(fields.remove("planned").is_some(), "prose has planned");
    pack
}

/// A settings reply holding `pack` in full, captured by rdy `rdy` (none when None).
pub fn pack_settings(pack: Value, rdy: Option<&str>) -> Value {
    let mut b = settings_step(19);
    b["pack"] = pack;
    b["rdy"] = rdy.map_or(Value::Null, |v| json!(v));
    b
}

/// This rdy's version.
pub fn this_rdy() -> String {
    rdy_engine::render_inputs::RDY_VERSION.to_string()
}

/// A plain `major.minor.patch` version as numbers.
pub fn version(v: &str) -> (u64, u64, u64) {
    let n: Vec<u64> = v.split('.').map(|p| p.parse().unwrap()).collect();
    (n[0], n[1], n[2])
}

/// A version newer than this rdy.
pub fn newer_rdy() -> String {
    let (major, minor, _) = version(&this_rdy());
    format!("{major}.{}.0", minor + 1)
}

/// A version older than this rdy.
pub fn older_rdy() -> String {
    "0.1.0".to_string()
}

/// D-7PYYS's refusal of `member` from hub repo `url`, captured by rdy `captured`: its message
/// and fix, filled in.
pub fn unfit_words(member: &str, url: &str, captured: Option<&str>) -> (String, String) {
    let this = this_rdy();
    match captured {
        Some(c) => {
            let message = format!(
                "the {member} of hub repo {url} comes from rdy {c} and does not fit rdy {this}"
            );
            let newer = version(c) > version(&this);
            let fix = if newer {
                format!("update rdy to {c} or later, then clone again")
            } else {
                format!("have the hub repo's next push come from rdy {this}, then clone again")
            };
            (message, fix)
        }
        None => (
            format!("the {member} of hub repo {url} does not fit rdy {this}"),
            format!("update rdy, or have the hub repo's next push come from rdy {this}"),
        ),
    }
}

/// The settings refusals of D-7PYYS: a label, the settings reply, the member the refusal names,
/// and the capturing rdy (none for a bundle with no `rdy`). Every case carries the sentinels
/// [`SENTINELS`] where this rdy cannot read them.
pub fn refusal_cases() -> Vec<(String, Value, &'static str, Option<String>)> {
    let mut out = Vec::new();
    for captured in [Some(newer_rdy()), Some(older_rdy()), Some(this_rdy()), None] {
        let named = captured.clone().unwrap_or_else(|| "none".into());
        let c = captured.as_deref();
        // the pack: an unknown key, an unreadable value, an unknown key in a nested part
        let mut p = builtin_pack();
        p["sentinel_key_q7"] = json!("sentinel_value_q7");
        out.push((
            format!("pack key, rdy {named}"),
            pack_settings(p, c),
            "pack",
            captured.clone(),
        ));
        let mut p = builtin_pack();
        p["version"] = json!({"sentinel_key_v3": "sentinel_value_v3"});
        out.push((
            format!("pack value, rdy {named}"),
            pack_settings(p, c),
            "pack",
            captured.clone(),
        ));
        let mut p = builtin_pack();
        let first = p["block_types"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        p["block_types"][&first]["sentinel_key_n5"] = json!(1);
        out.push((
            format!("nested pack key, rdy {named}"),
            pack_settings(p, c),
            "pack",
            captured.clone(),
        ));
        let mut p = builtin_pack();
        p["block_types"][&first]["body"] = json!("sentinel_value_n6");
        out.push((
            format!("nested pack value, rdy {named}"),
            pack_settings(p, c),
            "pack",
            captured.clone(),
        ));
        // a setting: an unknown word, a number outside its range, an unknown key
        let mut b = settings_step(12);
        b["rdy"] = c.map_or(Value::Null, |v| json!(v));
        let mut gate = b.clone();
        gate["coverage_gate"] = json!("sentinel_value_g1");
        out.push((
            format!("gate word, rdy {named}"),
            gate,
            "coverage_gate",
            captured.clone(),
        ));
        let mut target = b.clone();
        target["coverage_target"] = json!(101);
        out.push((
            format!("target range, rdy {named}"),
            target,
            "coverage_target",
            captured.clone(),
        ));
        let mut propose = b.clone();
        propose["propose"] = json!({"cadence": "task", "sentinel_key_p2": "sentinel_value_p2"});
        out.push((
            format!("propose key, rdy {named}"),
            propose,
            "propose",
            captured.clone(),
        ));
        let mut title = b.clone();
        title["title"] = json!({"sentinel_key_t4": "sentinel_value_t4"});
        out.push((
            format!("title value, rdy {named}"),
            title,
            "title",
            captured.clone(),
        ));
    }
    out
}

/// The sentinel keys and values of [`refusal_cases`]: no refusal quotes one.
pub const SENTINELS: [&str; 5] = ["sentinel", "q7", "v3", "n5", "g1"];
