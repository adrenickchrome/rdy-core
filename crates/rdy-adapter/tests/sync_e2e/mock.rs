//! The fixture hub of the two-machine sync tests: a loopback TLS listener on an allocated port
//! with a test CA, the sign-in the clients carry as a ready credential, and the sync steps of
//! rdyhub's wire fixture v2 over a temporary holder. The holder is the adapter's: state, presence,
//! history pages and get batches are reads of its snapshots, a put is checked into a quarantine
//! per push, and a write is a move through the store's one writer. The verdicts are scripted
//! (take, hold, refuse, force): they are test behavior, not a hub gate.
//!
//! Where fixture v2 serves a case, the mock answers with its shapes. It does not serve held
//! writes and outcomes, view writes, a state with a held view, or the write of a pushed tag; the
//! mock serves those with the shapes of the hub design (i-B40VF, i-6YB7Y) as the client decodes
//! them.
//!
//! A put is checked once when it arrives, after a bounded zstd decode when it is sent
//! compressed, and the push keeps that check with its file ([`CheckedBatchFile`]); its write
//! checks the push from those checks. The test driver may open a pull scope around one client
//! command ([`Hub::scope`]): the scope's first known pull prepares the pull once, with that
//! request's base or none ([`rdy_adapter::Snapshot::prepare_pull_reply_with_base`]), and the
//! known pages of that purpose, root and base and every get of that purpose and root read that
//! one selection. A get checks its keys against the selection and walks nothing again. A get
//! that offers zstd may come back as one zstd frame. Requests outside a scope keep their
//! one-request snapshot reads. A scope is test-driver ownership, not a hub session.
//! [`Peer`] plays an older hub or client for the encoding checks.
//!
//! The state reply lists stream format 5 in `stream_write`: the hub sends a canon tip's state
//! (D-GGTYA). A known pull or get of purpose `canon_state` reads one `PreparedCanonState`
//! ([`rdy_adapter::Snapshot::prepare_canon_state`]): the scope's, prepared once on its first
//! page, or the request's own outside a scope. Its known pages carry `bytes`, the state's exact
//! object frame bytes. It never reads a history page, a history closure or a prepared pull; the
//! hub counts those reads ([`Hub::history_reads`], [`Hub::history_walks`]).
//! [`Hub::send_canon_state`] is the test switch that takes format 5 out of the state reply, for
//! the fallback cases: it is test configuration, not a wire field. [`Hub::seed`] seeds hub canon
//! from batch files in process, and [`Hub::call`] serves one sync step in process.
//!
//! All requests and faults are in memory. A request holds the hub's state lock for its whole run,
//! so two requests never interleave. Nothing sleeps: each connection runs on its own thread, and
//! [`Hub::finish`] stops the listener and joins every thread.
#![allow(dead_code)]

#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Ca, LOOPBACK};
use rdy_adapter::stream::{BodyEncoding, ObjectKey, PreparedCanonState, PullReply, StreamPurpose};
use rdy_adapter::sync::{CanonMove, CheckedBatchFile, Moved, check_batch_file};
use rdy_adapter::{Hash, RdyError, RepoId, StoreHandle};
use rdy_engine::readiness::assessment::pack_digest;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// The remote's path on the hub, and the sync base discovery names under it.
pub const REMOTE_PATH: &str = "/acme/docs";
pub const SYNC_BASE: &str = "/sync/acme/docs";
const REQUEST_ID: &str = "req_00000000000000000000000009";
const LANDING: u8 = 4;
const VIEW_OP: u8 = 13;
/// The most bytes of one batch, as the state step names it: a put's decoded body is read to at
/// most this many bytes.
const BATCH_BYTES: u64 = 104_857_600;

/// What the hub's gate does with the next write (scripted, one verdict per write, in order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Take the write as its kind says: seed or extend, the force of a force write, a view head.
    Take,
    /// Keep the pushed head beside canon as a landing request.
    Hold,
    /// Refuse the write with this status and code.
    Refuse(u16, String),
    /// Take the write as a force of canon, whatever its kind says.
    Force,
}

/// A fault of the next matching request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The write is applied, then its connection closes with no reply: a lost response.
    LoseWriteReply,
    /// The next get reply stops after this many body bytes.
    CutGet(usize),
    /// The next get that offers zstd comes back as `Content-Encoding: zstd` with bytes that are
    /// no zstd frame.
    BadZstdGet,
}

/// Which side of a sync the hub plays older than this build, for the reply encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// This build's hub and client: a get that offers zstd may come back compressed.
    New,
    /// A hub that knows no compressed get: it ignores `Accept-Encoding` and names no `zstd` in
    /// its state reply.
    OldHub,
    /// A client that offers no compressed get: the hub reads each request without its
    /// `Accept-Encoding`.
    OldClient,
}

/// One request as the hub served it, for the speed report: the command it served, its route,
/// its bytes on the wire and as batches, and the hub's time on it.
#[derive(Clone, Debug)]
pub struct Served {
    /// The scope's operation (`push`, `clone`), or empty outside a scope.
    pub operation: String,
    pub route: String,
    /// The request's and the reply's body bytes on the wire.
    pub wire_bytes: u64,
    /// The same bodies with each batch counted at its decoded bytes.
    pub raw_bytes: u64,
    pub server_ms: f64,
    /// Whether the request offered zstd, as the hub read it.
    pub offered_zstd: bool,
    /// Whether the reply body was one zstd frame.
    pub zstd_reply: bool,
    /// The history closure walks (`rdy_engine::stream_export::walks_for_tests`, one per
    /// `history_closure_before` call) the step made: a pull's preparation counts its walk on the
    /// step that prepared it.
    pub walks: u64,
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

/// How the hub ended a push's write.
#[derive(Clone, Debug)]
enum Outcome {
    Taken {
        kind: String,
        tip: ObjectKey,
        view: Option<String>,
    },
    Held {
        kind: String,
        request: u64,
    },
    Refused {
        kind: String,
        code: String,
    },
}

/// The settings the last taking canon write sent.
#[derive(Clone)]
struct Kept {
    tip: Hash,
    gate: Value,
    render_inputs: Value,
}

/// The records of one push: its batch files, the keys they hold, and the check of each file
/// made when its put arrived (one per file, in order). The checks live until the push's write
/// ends; a write that finds none checks the files itself.
#[derive(Default)]
struct Push {
    files: Vec<PathBuf>,
    keys: BTreeSet<ObjectKey>,
    checked: Vec<CheckedBatchFile>,
}

/// One client command's pull scope: the pull it prepared, by purpose, root and base, and the
/// canon state it prepared, by root.
#[derive(Default)]
struct Scope {
    operation: String,
    reply: Option<(StreamPurpose, Hash, Option<Hash>, PullReply)>,
    state: Option<(Hash, PreparedCanonState)>,
}

/// One sync step served in process ([`Hub::call`]): its status, head fields and body.
#[derive(Clone, Debug)]
pub struct Called {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Called {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON reply")
    }

    /// The value of the head field `name` (lower case), if any.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The hub repo's state.
struct Repo {
    dir: tempfile::TempDir,
    holder: Option<StoreHandle>,
    repo_id: Option<RepoId>,
    pushes: BTreeMap<String, Push>,
    outcomes: BTreeMap<String, Outcome>,
    verdicts: VecDeque<Verdict>,
    faults: VecDeque<Fault>,
    kept: Option<Kept>,
    requests_opened: u64,
    /// The gate's own refusal of a state step, when scripted.
    min_rdy: Option<String>,
    peer: Peer,
    scope: Option<Scope>,
    /// The pulls prepared so far, first and incremental.
    prepared: u64,
    served: Vec<Served>,
    /// The wire and decoded bytes of the batch of the request being served.
    batch: Option<(u64, u64)>,
    /// Whether the reply being served is one zstd frame.
    zstd_reply: bool,
    /// Whether the state reply lists stream format 5 (the test switch).
    canon_state: bool,
    /// The canon states prepared so far.
    states_prepared: u64,
    /// The history reads the steps made: history pages, history gets and prepared pulls.
    history_reads: u64,
    /// The history closure walks (`rdy_engine::stream_export::walks_for_tests`) the steps made.
    history_walks: u64,
}

pub struct Shared {
    config: Arc<ServerConfig>,
    pub origin: String,
    log: Mutex<Vec<Req>>,
    repo: Mutex<Repo>,
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
        Hub::start_as(Peer::New)
    }

    /// A hub that plays `peer` (see [`Peer`]).
    pub fn start_as(peer: Peer) -> Hub {
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
            log: Mutex::new(Vec::new()),
            repo: Mutex::new(Repo {
                dir: tempfile::tempdir().expect("hub directory"),
                holder: None,
                repo_id: None,
                pushes: BTreeMap::new(),
                outcomes: BTreeMap::new(),
                verdicts: VecDeque::new(),
                faults: VecDeque::new(),
                kept: None,
                requests_opened: 0,
                min_rdy: None,
                peer,
                scope: None,
                prepared: 0,
                served: Vec::new(),
                batch: None,
                zstd_reply: false,
                canon_state: true,
                states_prepared: 0,
                history_reads: 0,
                history_walks: 0,
            }),
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

    /// The remote's address on this hub.
    pub fn url(&self) -> String {
        format!("{}{REMOTE_PATH}", self.shared.origin)
    }

    /// The next writes meet these verdicts, one each, in order; after them a write is taken.
    pub fn script(&self, verdicts: &[Verdict]) {
        self.shared
            .repo
            .lock()
            .unwrap()
            .verdicts
            .extend(verdicts.iter().cloned());
    }

    /// The next matching requests meet these faults, one each, in order.
    pub fn fault(&self, fault: Fault) {
        self.shared
            .repo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .faults
            .push_back(fault);
    }

    /// Opens the pull scope of one client command, named `operation` in the served
    /// requests; dropping the guard, when the command returns or fails, releases the scope and
    /// what it prepared. A new command gets a new scope.
    pub fn scope(&self, operation: &str) -> ScopeGuard {
        self.shared.repo.lock().unwrap().scope = Some(Scope {
            operation: operation.to_string(),
            reply: None,
            state: None,
        });
        ScopeGuard {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Whether the state reply lists stream format 5 (on by default): off plays a hub that sends
    /// no canon state, for the fallback cases.
    pub fn send_canon_state(&self, on: bool) {
        self.shared.repo.lock().unwrap().canon_state = on;
    }

    /// The canon states the hub prepared so far.
    pub fn states_prepared(&self) -> u64 {
        self.shared.repo.lock().unwrap().states_prepared
    }

    /// The history reads the hub's steps made so far: history pages, history gets and prepared
    /// pulls, each counted once.
    pub fn history_reads(&self) -> u64 {
        self.shared.repo.lock().unwrap().history_reads
    }

    /// The history closure walks the hub's steps made so far, as the engine counts them on the
    /// thread that served each step.
    pub fn history_walks(&self) -> u64 {
        self.shared.repo.lock().unwrap().history_walks
    }

    /// Seeds hub canon from the batch `files` of one canon history, as a seed write takes them,
    /// in process: a new holder of the files' repo, their quarantine check and the seed move.
    pub fn seed(&self, files: &[PathBuf]) {
        let mut repo = self.shared.repo.lock().unwrap();
        assert!(repo.holder.is_none(), "a hub repo is seeded once");
        let first = check_batch_file(
            File::open(&files[0]).expect("a batch file"),
            None,
            |_, _, _| {},
        )
        .expect("a checked batch");
        let id = first.report().header.repo_id;
        let holder =
            StoreHandle::create_holder(&repo.dir.path().join("holder"), id).expect("the holder");
        let opened: Vec<File> = files
            .iter()
            .map(|f| File::open(f).expect("a batch file"))
            .collect();
        let checked = holder
            .snapshot()
            .expect("a snapshot")
            .check_quarantine(&opened)
            .expect("the seed's check");
        holder
            .move_canon(&checked, CanonMove::Seed, None)
            .expect("the seed move");
        repo.holder = Some(holder);
        repo.repo_id = Some(id);
    }

    /// Serves one sync step `route` (such as `/state`) with the JSON `body` in process, as the
    /// listener serves it, and returns its reply. `offer_zstd` sends `Accept-Encoding: zstd`.
    pub fn call(&self, route: &str, body: &Value, offer_zstd: bool) -> Called {
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if offer_zstd {
            headers.push(("accept-encoding".to_string(), "zstd".to_string()));
        }
        let req = Req {
            method: "POST".into(),
            path: format!("{SYNC_BASE}{route}"),
            query: BTreeMap::new(),
            headers,
            body: serde_json::to_vec(body).expect("a JSON body"),
        };
        self.shared.log.lock().unwrap().push(req.clone());
        let Answer::Whole(bytes) = answer(&req, &self.shared) else {
            panic!("an in-process step has a whole reply");
        };
        let end = find(&bytes, b"\r\n\r\n").expect("a reply head");
        let head = String::from_utf8(bytes[..end].to_vec()).expect("a text head");
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .and_then(|s| s.parse().ok())
            .expect("a status");
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        Called {
            status,
            headers,
            body: bytes[end + 4..].to_vec(),
        }
    }

    /// The pulls the hub prepared so far, first and incremental.
    pub fn prepared(&self) -> u64 {
        self.shared
            .repo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prepared
    }

    /// Every request the hub served so far, in order.
    pub fn served(&self) -> Vec<Served> {
        self.shared
            .repo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .served
            .clone()
    }

    /// The pushes whose files still hold their put's checks.
    pub fn held_checks(&self) -> usize {
        self.shared
            .repo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pushes
            .values()
            .map(|p| p.checked.len())
            .sum()
    }

    /// Hub canon's tip now.
    pub fn canon(&self) -> Option<Hash> {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        repo.holder.as_ref()?.snapshot().ok()?.canon()
    }

    /// The hub's head of view `name`, if it holds one.
    pub fn view(&self, name: &str) -> Option<Hash> {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        let snap = repo.holder.as_ref()?.snapshot().ok()?;
        snap.view_head(name).ok()??.head.into()
    }

    /// The tags the hub holds: name to the landing it names.
    pub fn tags(&self) -> BTreeMap<String, Hash> {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        let Some(h) = repo.holder.as_ref() else {
            return BTreeMap::new();
        };
        h.snapshot()
            .unwrap()
            .tags()
            .unwrap()
            .into_iter()
            .map(|(_, t)| (t.name, t.landing))
            .collect()
    }

    /// Runs `f` on a snapshot of the hub's holder (none before the first push).
    pub fn with_snapshot<T>(&self, f: impl FnOnce(&rdy_adapter::Snapshot) -> T) -> Option<T> {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        let snap = repo.holder.as_ref()?.snapshot().ok()?;
        Some(f(&snap))
    }

    /// Whether any file of the hub's holder holds `needle`.
    pub fn holder_holds(&self, needle: &str) -> bool {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        fn walk(dir: &std::path::Path, needle: &[u8]) -> bool {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, needle)
                    } else {
                        std::fs::read(&p)
                            .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
                    }
                })
        }
        walk(&repo.dir.path().join("holder"), needle.as_bytes())
    }

    /// Writes a dump of the hub's holder to `out`, as the adapter dumps a holder.
    pub fn dump_holder(&self, out: &std::path::Path) {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        let h = repo.holder.as_ref().expect("a holder");
        h.dump(out).expect("dump the holder");
    }

    /// The holder's own check of its store (`fsck`): the closure and SD1 checks. It must pass.
    pub fn fsck(&self) {
        let repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = repo.holder.as_ref() {
            h.fsck().expect("the hub holder passes fsck");
        }
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

    /// Every request read so far, in order.
    pub fn requests(&self) -> Vec<Req> {
        self.shared.log.lock().unwrap().clone()
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
        let _ = acceptor.join();
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for w in workers {
            let _ = w.join();
        }
        // the holder closes with the hub
        let mut repo = self.shared.repo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = repo.holder.take() {
            let _ = h.close();
        }
    }
}

/// An open pull scope ([`Hub::scope`]).
pub struct ScopeGuard {
    shared: Arc<Shared>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.shared
            .repo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scope = None;
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop_threads();
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP over TLS
// ---------------------------------------------------------------------------------------------

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
        413 => "Content Too Large",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Status",
    }
}

fn head_bytes(status: u16, content_type: &str, len: usize) -> Vec<u8> {
    head_bytes_encoded(status, content_type, len, None)
}

/// A reply head whose body of `len` wire bytes has `encoding` (`Content-Encoding`), if any.
fn head_bytes_encoded(
    status: u16,
    content_type: &str,
    len: usize,
    encoding: Option<&str>,
) -> Vec<u8> {
    let encoding = encoding.map_or(String::new(), |e| format!("content-encoding: {e}\r\n"));
    format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {len}\r\n{encoding}connection: close\r\n\r\n",
        reason(status)
    )
    .into_bytes()
}

/// What the hub sends for one request.
enum Answer {
    /// The whole reply.
    Whole(Vec<u8>),
    /// The head and the first `n` bytes of the body only, then the connection closes.
    Cut(Vec<u8>, usize),
    /// Nothing: the connection closes.
    Nothing,
}

fn reply_bytes(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = head_bytes(status, content_type, body.len());
    out.extend_from_slice(body);
    out
}

fn reply(status: u16, body: &Value) -> Answer {
    Answer::Whole(reply_bytes(
        status,
        "application/json",
        body.to_string().as_bytes(),
    ))
}

/// The hub's own error object.
fn hub_error(status: u16, code: &str, message: &str, fix: &str) -> Answer {
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
    let Some(mut req) = read_request(&mut tls) else {
        let _ = tls.sock.shutdown(Shutdown::Both);
        return;
    };
    // a client older than this build offers no compressed reply
    if shared.repo.lock().unwrap().peer == Peer::OldClient {
        req.headers.retain(|(n, _)| n != "accept-encoding");
    }
    shared.log.lock().unwrap().push(req.clone());
    match answer(&req, shared) {
        Answer::Whole(bytes) => {
            let _ = tls.write_all(&bytes);
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.conn.complete_io(&mut tls.sock);
        }
        Answer::Cut(bytes, n) => {
            let split = bytes
                .len()
                .min(find(&bytes, b"\r\n\r\n").map_or(0, |i| i + 4) + n);
            let _ = tls.write_all(&bytes[..split]);
            let _ = tls.flush();
        }
        Answer::Nothing => {}
    }
    let _ = tls.sock.shutdown(Shutdown::Both);
}

// ---------------------------------------------------------------------------------------------
// the steps
// ---------------------------------------------------------------------------------------------

fn key_json(kind: u8, hash: &str) -> Value {
    json!({"hash": hash, "kind": kind})
}

fn purpose_of(name: &str) -> StreamPurpose {
    match name {
        "view_history" => StreamPurpose::ViewHistory,
        "canon_state" => StreamPurpose::CanonState,
        _ => StreamPurpose::CanonHistory,
    }
}

fn key_of(v: &Value) -> Option<ObjectKey> {
    serde_json::from_value(v.clone()).ok()
}

fn not_found(what: &str) -> Answer {
    hub_error(
        404,
        "E_HUB_NOT_FOUND",
        &format!("hub repo acme/docs holds no record {what}"),
        "run rdy pull again; it reads the hub's state first",
    )
}

/// A core refusal as the hub passes it on.
fn refusal(e: &RdyError) -> Answer {
    let code = if e.code == "E_BAD_INPUT" {
        "E_HUB_CHAIN_INVALID"
    } else {
        "E_HUB_BAD_INPUT"
    };
    hub_error(422, code, &e.message, &e.fix)
}

/// The body bytes of a reply's whole bytes.
fn body_len(bytes: &[u8]) -> u64 {
    (bytes.len() - find(bytes, b"\r\n\r\n").map_or(bytes.len(), |i| i + 4)) as u64
}

/// Whether the request offers a zstd reply.
fn offers_zstd(req: &Req) -> bool {
    req.headers.iter().any(|(n, v)| {
        n == "accept-encoding" && v.split(',').any(|e| e.trim().eq_ignore_ascii_case("zstd"))
    })
}

fn answer(req: &Req, shared: &Shared) -> Answer {
    let start = Instant::now();
    let mut repo = shared.repo.lock().unwrap();
    repo.batch = None;
    repo.zstd_reply = false;
    let (walks, _) = rdy_engine::stream_export::walks_for_tests();
    let out = route(req, &mut repo);
    let walks = rdy_engine::stream_export::walks_for_tests().0 - walks;
    repo.history_walks += walks;
    let reply_bytes = match &out {
        Answer::Whole(b) | Answer::Cut(b, _) => body_len(b),
        Answer::Nothing => 0,
    };
    let wire = req.body.len() as u64 + reply_bytes;
    let raw = match repo.batch.take() {
        Some((w, r)) => wire - w + r,
        None => wire,
    };
    let served = Served {
        operation: repo
            .scope
            .as_ref()
            .map_or(String::new(), |s| s.operation.clone()),
        route: match req.route() {
            "" => "discovery".to_string(),
            r => r.trim_start_matches('/').to_string(),
        },
        wire_bytes: wire,
        raw_bytes: raw,
        server_ms: start.elapsed().as_secs_f64() * 1000.0,
        offered_zstd: offers_zstd(req),
        zstd_reply: repo.zstd_reply,
        walks,
    };
    repo.served.push(served);
    out
}

fn route(req: &Req, repo: &mut Repo) -> Answer {
    match (req.method.as_str(), req.path.as_str(), req.route()) {
        ("GET", REMOTE_PATH, _) => reply(200, &json!({"sync_base": SYNC_BASE, "protocols": [1]})),
        ("POST", _, "/state") => state(req, repo),
        ("POST", _, "/known") => known(req, repo),
        ("POST", _, "/objects/put") => put(req, repo),
        ("POST", _, "/objects/get") => get(req, repo),
        ("POST", _, "/write") => write(req, repo),
        ("POST", _, "/settings") => settings(repo),
        ("POST", _, "/outcome") => outcome(req, repo),
        _ => reply(404, &json!({})),
    }
}

fn state(req: &Req, repo: &Repo) -> Answer {
    let r = req.json();
    let snap = repo.holder.as_ref().and_then(|h| h.snapshot().ok());
    let views: Vec<Value> = r["views"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(
            |name| match snap.as_ref().and_then(|s| s.view_head(name).ok().flatten()) {
                Some(v) => json!({"name": name, "root": key_json(VIEW_OP, &v.root.to_string()),
                    "head": key_json(VIEW_OP, &v.head.to_string())}),
                None => json!({"name": name, "root": null, "head": null}),
            },
        )
        .collect();
    let features: Vec<Value> = snap
        .as_ref()
        .map(|s| {
            let mut f: Vec<_> = s.features().features.iter().collect();
            f.sort_by(|a, b| a.name.cmp(&b.name));
            f.iter()
                .map(|f| json!({"name": f.name, "class": f.class}))
                .collect()
        })
        .unwrap_or_default();
    let min_rdy = repo.min_rdy.clone().or_else(|| {
        snap.as_ref()
            .and_then(|s| s.min_rdy())
            .map(|v| v.to_string())
    });
    reply(
        200,
        &json!({"canon": snap.as_ref().and_then(|s| s.canon()).map(|h| key_json(LANDING, &h.to_string())),
            "repo_id": repo.repo_id.map(|r| r.to_string()), "rdy": r["rdy"],
            "stream_read": [2, 4], "stream_write": stream_write(repo), "features": features,
            "min_rdy": min_rdy, "batch_bytes": 104_857_600u64,
            "batch_records": 100_000, "quarantine_seconds": 86_400,
            "profile": null, "audience": null, "artifact_bytes": 1_073_741_824u64,
            "artifact_files": 32_000, "views": views})
        .as_object()
        .cloned()
        .map(|mut m| {
            // a hub of this build takes compressed puts; an older one names no such member
            if repo.peer != Peer::OldHub {
                m.insert("zstd".into(), json!(true));
            }
            Value::Object(m)
        })
        .unwrap(),
    )
}

/// The formats the hub sends: 2 and 4, and 5 (the canon state) unless the switch is off.
fn stream_write(repo: &Repo) -> Value {
    if repo.canon_state {
        json!([2, 4, 5])
    } else {
        json!([2, 4])
    }
}

/// Runs `f` on the canon state of `root`: the scope's, prepared once, or this request's own
/// outside a scope. A hub with format 5 switched off refuses the purpose; a root the holder
/// does not hold as a landing is not found.
fn with_state<T>(
    repo: &mut Repo,
    snap: &rdy_adapter::Snapshot,
    root: ObjectKey,
    f: impl FnOnce(&PreparedCanonState) -> T,
) -> Result<T, Answer> {
    if !repo.canon_state {
        return Err(hub_error(
            422,
            "E_HUB_VALIDATION",
            "this hub sends no canon state",
            "pull the canon history",
        ));
    }
    let missing = || not_found(&format!("{}:{}", root.kind, root.hash));
    if root.kind != LANDING {
        return Err(missing());
    }
    if let Some(scope) = repo.scope.as_mut() {
        if scope.state.as_ref().is_none_or(|(h, _)| *h != root.hash) {
            let prepared = snap.prepare_canon_state(root.hash).map_err(|_| missing())?;
            scope.state = Some((root.hash, prepared));
            repo.states_prepared += 1;
        }
        let (_, prepared) = repo
            .scope
            .as_ref()
            .and_then(|s| s.state.as_ref())
            .expect("the scope's state");
        return Ok(f(prepared));
    }
    let prepared = snap.prepare_canon_state(root.hash).map_err(|_| missing())?;
    repo.states_prepared += 1;
    Ok(f(&prepared))
}

/// A known pull of a canon state: one page of the prepared state, with the state's exact
/// object frame bytes.
fn known_state(
    repo: &mut Repo,
    snap: &rdy_adapter::Snapshot,
    root: ObjectKey,
    base: Option<Hash>,
    after: Option<ObjectKey>,
) -> Answer {
    if base.is_some() {
        return hub_error(
            422,
            "E_HUB_VALIDATION",
            "a canon state has no base",
            "pull the state with no base",
        );
    }
    let page = with_state(repo, snap, root, |p| {
        p.page(after, 100_000).map(|page| (page, p.frame_bytes()))
    });
    match page {
        Ok(Ok((page, bytes))) => {
            let keys: Vec<Value> = page
                .keys
                .iter()
                .map(|k| key_json(k.kind, &k.hash.to_string()))
                .collect();
            let next = page.next.map(|k| key_json(k.kind, &k.hash.to_string()));
            reply(200, &json!({"bytes": bytes, "keys": keys, "next": next}))
        }
        Ok(Err(_)) => not_found(&format!("{}:{}", root.kind, root.hash)),
        Err(answer) => answer,
    }
}

fn known(req: &Req, repo: &mut Repo) -> Answer {
    let r = req.json();
    if let Some(push) = r["push"].as_str() {
        // the push form: the keys that neither the holder nor this push's quarantine holds
        let keys: Vec<ObjectKey> = r["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(key_of)
            .collect();
        let held = match repo.holder.as_ref().and_then(|h| h.snapshot().ok()) {
            Some(snap) => snap.has_objects(&keys).expect("presence"),
            None => Vec::new(),
        };
        let mine = repo.pushes.get(push).map(|p| &p.keys);
        let missing: Vec<Value> = keys
            .iter()
            .enumerate()
            .filter(|(i, k)| {
                !held.get(*i).is_some_and(|p| p.present) && !mine.is_some_and(|m| m.contains(k))
            })
            .map(|(_, k)| key_json(k.kind, &k.hash.to_string()))
            .collect();
        return reply(200, &json!({"missing": missing}));
    }
    let Some(snap) = repo.holder.as_ref().and_then(|h| h.snapshot().ok()) else {
        return not_found("of that root");
    };
    let (Some(root), after) = (key_of(&r["root"]), key_of(&r["after"])) else {
        return hub_error(
            422,
            "E_HUB_VALIDATION",
            "a known pull names a root",
            "name the root",
        );
    };
    let base = key_of(&r["base"]).map(|b| b.hash);
    let purpose = purpose_of(r["purpose"].as_str().unwrap_or("canon_history"));
    if purpose == StreamPurpose::CanonState {
        return known_state(repo, &snap, root, base, after);
    }
    repo.history_reads += 1;
    // a pull in a scope: prepared once, on its first page, with that page's base
    if let Some(scope) = repo.scope.as_mut() {
        if scope.reply.is_none() {
            match snap.prepare_pull_reply_with_base(purpose, root.hash, base) {
                Ok(reply) => {
                    scope.reply = Some((purpose, root.hash, base, reply));
                    repo.prepared += 1;
                }
                Err(_) => return not_found(&format!("{}:{}", root.kind, root.hash)),
            }
        }
        if let Some((p, h, b, reply)) = repo.scope.as_ref().and_then(|s| s.reply.as_ref())
            && (*p, *h, *b) == (purpose, root.hash, base)
        {
            return match reply.page(after, 100_000) {
                Ok(page) => page_reply(&page.keys, page.next),
                Err(_) => not_found(&format!("{}:{}", root.kind, root.hash)),
            };
        }
    }
    match snap.history_page(purpose, root.hash, base, after, 100_000) {
        Ok(page) => page_reply(&page.keys, page.next),
        Err(_) => not_found(&format!("{}:{}", root.kind, root.hash)),
    }
}

/// A known pull's page reply.
fn page_reply(keys: &[ObjectKey], next: Option<ObjectKey>) -> Answer {
    let keys: Vec<Value> = keys
        .iter()
        .map(|k| key_json(k.kind, &k.hash.to_string()))
        .collect();
    let next = next.map(|k| key_json(k.kind, &k.hash.to_string()));
    reply(200, &json!({"keys": keys, "next": next}))
}

fn put(req: &Req, repo: &mut Repo) -> Answer {
    let push = req.query.get("push").cloned().unwrap_or_default();
    let compressed = req
        .headers
        .iter()
        .any(|(n, v)| n == "content-encoding" && v.eq_ignore_ascii_case("zstd"));
    // a compressed body is decoded to at most the batch bytes the state step names
    let body = if compressed {
        let mut out = Vec::new();
        let decoded = zstd::stream::read::Decoder::new(&req.body[..])
            .and_then(|d| d.take(BATCH_BYTES + 1).read_to_end(&mut out));
        if decoded.is_err() {
            return hub_error(
                422,
                "E_HUB_BAD_INPUT",
                "the put body is not one zstd frame",
                "push again with rdy push from the project",
            );
        }
        if out.len() as u64 > BATCH_BYTES {
            return hub_error(
                413,
                "E_HUB_LIMIT",
                "the put body is over the batch bytes",
                "push again with rdy push from the project",
            );
        }
        out
    } else {
        req.body.clone()
    };
    repo.batch = Some((req.body.len() as u64, body.len() as u64));
    let n = repo.pushes.get(&push).map_or(0, |p| p.files.len());
    if n >= rdy_store::sync_stream::MAX_BATCH_FILES {
        return hub_error(
            422,
            "E_HUB_LIMIT",
            "a push holds at most 1024 batch files",
            "push again with rdy push from the project",
        );
    }
    let path = repo.dir.path().join(format!("{push}-{n}.rdysync"));
    std::fs::write(&path, &body).expect("a batch file");
    // the put's one check of the file, kept with the push until its write
    let checked = match check_batch_file(
        File::open(&path).expect("a batch file"),
        repo.repo_id,
        |_, _, _| {},
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return hub_error(422, "E_BAD_INPUT", &e.message, &e.fix);
        }
    };
    if repo.holder.is_none() {
        let id = checked.report().header.repo_id;
        repo.holder = Some(
            StoreHandle::create_holder(&repo.dir.path().join("holder"), id).expect("the holder"),
        );
        repo.repo_id = Some(id);
    }
    let p = repo.pushes.entry(push).or_default();
    p.files.push(path);
    p.keys.extend(checked.report().keys.iter().copied());
    p.checked.push(checked);
    reply(
        200,
        &json!({"records": p.keys.len(), "bytes": req.body.len()}),
    )
}

fn get(req: &Req, repo: &mut Repo) -> Answer {
    let r = req.json();
    let Some(snap) = repo.holder.as_ref().and_then(|h| h.snapshot().ok()) else {
        return not_found("of that root");
    };
    let Some(root) = key_of(&r["root"]) else {
        return not_found("of that root");
    };
    let purpose = purpose_of(r["purpose"].as_str().unwrap_or("canon_history"));
    let state = purpose == StreamPurpose::CanonState;
    if !state {
        repo.history_reads += 1;
    }
    let keys: Vec<ObjectKey> = r["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(key_of)
        .collect();
    // a hub older than this build ignores the offer
    let accept = offers_zstd(req) && repo.peer != Peer::OldHub;
    if accept && repo.faults.front() == Some(&Fault::BadZstdGet) {
        repo.faults.pop_front();
        let body = b"these bytes are no zstd frame";
        let mut bytes =
            head_bytes_encoded(200, "application/octet-stream", body.len(), Some("zstd"));
        bytes.extend_from_slice(body);
        repo.zstd_reply = true;
        return Answer::Whole(bytes);
    }
    let scoped = repo
        .scope
        .as_ref()
        .and_then(|s| s.reply.as_ref())
        .filter(|(p, h, _, _)| (*p, *h) == (purpose, root.hash))
        .map(|(_, _, _, reply)| reply);
    let bytes = if state {
        // the canon state, as the wire bytes: one zstd frame only when offered
        let enc = match with_state(repo, &snap, root, |p| p.write_keys_encoded(&keys, accept)) {
            Ok(Ok(enc)) => enc,
            Ok(Err(_)) => return not_found("of one of those keys"),
            Err(answer) => return answer,
        };
        let mut enc = enc;
        let mut body = Vec::new();
        enc.body.read_to_end(&mut body).expect("the reply body");
        assert_eq!(body.len() as u64, enc.wire_bytes);
        let zstd = enc.encoding == BodyEncoding::Zstd;
        let mut bytes = head_bytes_encoded(
            200,
            "application/octet-stream",
            body.len(),
            zstd.then_some("zstd"),
        );
        bytes.extend_from_slice(&body);
        repo.batch = Some((enc.wire_bytes, enc.report.bytes));
        repo.zstd_reply = zstd;
        bytes
    } else if let Some(reply) = scoped {
        // the scope's prepared pull, as the wire bytes: one zstd frame only when offered
        let Ok(mut enc) = reply.write_keys_encoded(&keys, accept) else {
            return not_found("of one of those keys");
        };
        let mut body = Vec::new();
        enc.body.read_to_end(&mut body).expect("the reply body");
        assert_eq!(body.len() as u64, enc.wire_bytes);
        let zstd = enc.encoding == BodyEncoding::Zstd;
        let mut bytes = head_bytes_encoded(
            200,
            "application/octet-stream",
            body.len(),
            zstd.then_some("zstd"),
        );
        bytes.extend_from_slice(&body);
        repo.batch = Some((enc.wire_bytes, enc.report.bytes));
        repo.zstd_reply = zstd;
        bytes
    } else {
        let mut out = Vec::new();
        if snap
            .write_history_keys(purpose, root.hash, &keys, &mut out)
            .is_err()
        {
            return not_found("of one of those keys");
        }
        reply_bytes(200, "application/octet-stream", &out)
    };
    match repo.faults.front() {
        Some(Fault::CutGet(n)) => {
            let n = *n;
            repo.faults.pop_front();
            Answer::Cut(bytes, n)
        }
        _ => Answer::Whole(bytes),
    }
}

fn write(req: &Req, repo: &mut Repo) -> Answer {
    let r = req.json();
    let push = r["push"].as_str().unwrap_or_default().to_string();
    let kind = r["kind"].as_str().unwrap_or("landings").to_string();
    let expected = key_of(&r["expected"]).map(|k| k.hash);
    // the put checks end with this write, whatever its outcome
    let checks = repo
        .pushes
        .get_mut(&push)
        .map(|p| std::mem::take(&mut p.checked))
        .unwrap_or_default();
    let Some(p) = repo.pushes.get(&push) else {
        return hub_error(
            422,
            "E_HUB_VALIDATION",
            "a write names a push that sent records",
            "push again with rdy push from the project",
        );
    };
    if kind != "view" && (r["render_inputs"].is_null() || r["gate"].is_null()) && kind != "restore"
    {
        return hub_error(
            422,
            "E_HUB_VALIDATION",
            "a landings write must send render_inputs and gate",
            "push again with rdy push from the project",
        );
    }
    let files: Vec<File> = p
        .files
        .iter()
        .map(|f| File::open(f).expect("a batch file"))
        .collect();
    let verdict = repo.verdicts.pop_front().unwrap_or(Verdict::Take);
    if let Verdict::Refuse(status, code) = &verdict {
        repo.outcomes.insert(
            push,
            Outcome::Refused {
                kind,
                code: code.clone(),
            },
        );
        return hub_error(
            *status,
            code,
            "the hub's gate refused the write",
            "change the work, then push again",
        );
    }
    let holder = repo.holder.as_ref().expect("a holder after a put");
    let snap = holder.snapshot().expect("a snapshot");
    // the put checks when every file has one; else (a write sent again) each file is checked
    let checked = if checks.len() == files.len() {
        snap.check_quarantine_checked(&checks)
    } else {
        snap.check_quarantine(&files)
    };
    drop(checks);
    let checked = match checked {
        Ok(c) => c,
        Err(e) => return refusal(&e),
    };
    let facts = checked.facts().clone();
    let canon = snap.canon();
    let mv = match (&verdict, kind.as_str()) {
        (Verdict::Hold, _) => CanonMove::Hold,
        (Verdict::Force, _) | (_, "force") => CanonMove::Force,
        (_, "restore") if canon.is_some() => CanonMove::Force,
        (_, "view") => {
            let Some(v) = facts.view.clone() else {
                return hub_error(
                    422,
                    "E_HUB_BAD_INPUT",
                    "a view write pushes a view",
                    "push a view",
                );
            };
            CanonMove::SetViewHead {
                name: v.name,
                root: v.root,
            }
        }
        _ if canon.is_none() => CanonMove::Seed,
        _ => CanonMove::Extend,
    };
    let moved = match holder.move_canon(&checked, mv.clone(), expected) {
        Ok(m) => m,
        Err(e) => return refusal(&e),
    };
    let min_raised = facts.min_rdy_before != facts.min_rdy_after;
    let mut lose = false;
    if let Some(Fault::LoseWriteReply) = repo.faults.front() {
        repo.faults.pop_front();
        lose = true;
    }
    let body = match (moved, mv) {
        (Moved::Behind { .. }, _) => {
            repo.outcomes.insert(
                push,
                Outcome::Refused {
                    kind,
                    code: "E_HUB_BEHIND".into(),
                },
            );
            if lose {
                return Answer::Nothing;
            }
            return hub_error(
                409,
                "E_HUB_BEHIND",
                "Hub canon's tip, or the view's head, moved since the client read it.",
                "Pull, then push again.",
            );
        }
        (Moved::Moved { tip }, CanonMove::Hold) => {
            repo.requests_opened += 1;
            let n = repo.requests_opened;
            repo.outcomes
                .insert(push, Outcome::Held { kind, request: n });
            json!({"outcome": "held", "tip": null,
                "request": {"number": n, "version": 1}, "min_rdy_raised": false, "held": tip.to_string()})
        }
        (Moved::Moved { tip }, mv) => {
            let is_view = matches!(mv, CanonMove::SetViewHead { .. });
            let key = ObjectKey {
                kind: if is_view { VIEW_OP } else { LANDING },
                hash: tip,
            };
            let view = r["view"].as_str().map(str::to_string);
            repo.outcomes.insert(
                push,
                Outcome::Taken {
                    kind,
                    tip: key,
                    view,
                },
            );
            if !is_view {
                repo.kept = Some(Kept {
                    tip,
                    gate: r["gate"].clone(),
                    render_inputs: r["render_inputs"].clone(),
                });
            }
            json!({"outcome": "taken", "tip": key_json(key.kind, &key.hash.to_string()),
                "request": null, "min_rdy_raised": min_raised})
        }
    };
    if lose {
        return Answer::Nothing;
    }
    reply(200, &body)
}

/// The settings step: none before a taking canon write, else the last write's inputs; the pack
/// only when the gate's digest names it (rdyhub-design D-BYZR0).
fn settings(repo: &Repo) -> Answer {
    let Some(k) = &repo.kept else {
        return reply(
            200,
            &json!({"coverage_gate": null, "coverage_target": null, "decisions_home": null,
                "description": null, "gate_source": null, "pack": null, "pack_reason": "none",
                "propose": null, "rdy": null, "render_source": null, "title": null}),
        );
    };
    let ri = &k.render_inputs;
    let named = k.gate["pack_digest"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let matches = serde_json::from_value::<rdy_schema::Pack>(ri["pack"].clone())
        .is_ok_and(|p| pack_digest(&p).to_string() == named);
    let source = key_json(LANDING, &k.tip.to_string());
    reply(
        200,
        &json!({"coverage_gate": k.gate["coverage_gate"], "coverage_target": k.gate["coverage_target"],
            "decisions_home": k.gate["decisions_home"], "description": ri["description"],
            "gate_source": source, "pack": if matches { ri["pack"].clone() } else { Value::Null },
            "pack_reason": if matches { Value::Null } else { json!("mismatch") },
            "propose": k.gate["propose"], "rdy": ri["rdy"], "render_source": source,
            "title": ri["title"]}),
    )
}

fn outcome(req: &Req, repo: &Repo) -> Answer {
    let push = req.json()["push"].as_str().unwrap_or_default().to_string();
    let body = match repo.outcomes.get(&push) {
        Some(Outcome::Taken { kind, tip, view }) => json!({"outcome": "taken", "kind": kind,
            "tip": key_json(tip.kind, &tip.hash.to_string()), "code": null, "request": null,
            "request_state": null, "view": view}),
        Some(Outcome::Held { kind, request }) => json!({"outcome": "held", "kind": kind,
            "tip": null, "code": null, "request": {"number": request, "version": 1},
            "request_state": "open", "view": null}),
        Some(Outcome::Refused { kind, code }) => json!({"outcome": "refused", "kind": kind,
            "tip": null, "code": code, "request": null, "request_state": null, "view": null}),
        None => json!({"outcome": "unknown", "kind": null, "tip": null, "code": null,
            "request": null, "request_state": null, "view": null}),
    };
    reply(200, &body)
}

/// The hash of a hex string, or None.
pub fn hash_of(v: &Value) -> Option<Hash> {
    v.as_str().and_then(|s| s.parse().ok())
}
