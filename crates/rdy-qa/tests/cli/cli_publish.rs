//! hpb15 (D-Q0AVZ, D-KDFZ3, D-PJ65F, D-KQ4CA, D-8QV9G, D-WDKWQ, D-50JXR; hub i-6ZXW0, i-F19YZ,
//! r-K71GM): `rdy publish <tag> --to <remote> --site <site> [--keep-latest true|false]` and the
//! MCP tool `rdy_publish` run the one publish coordinator against the same loopback TLS hub.
//! Each test drives the real binary: the CLI through the project's daemon or with
//! `--no-daemon`, and MCP through the daemon's proxy (`rdy mcp`) or in process
//! (`rdy mcp --no-daemon`, whose engine has no publisher). Every child gets a private user
//! config home that trusts the hub's generated CA and holds a fresh sign-in.
//!
//! The hub here answers the publication upload protocol from what it reads: discovery, state,
//! known, objects, files/known, files and publish. It records every request, so a test
//! compares what each surface sent; the push id and the candidate id are the only parts that
//! differ between two runs. Nothing sleeps: the hub serves each connection on its own thread,
//! and [`Hub::finish`] stops the listener and joins every thread.
// the hub sign-in tests of this binary load the same fixture source as a module of their own
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Ca, LOOPBACK};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::hub_config::{
    Credential, CredentialFile, HubConfig, ReadyCredential, RemoteMode, encode_credentials,
};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish::{PublishRequest, PublishService, result_json};
use rdy_engine::hub_publish_wire::HttpPublishWire;
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{Origin, Secret, SystemHubClock};
use rdy_engine::{Rdy, Session, now_ms};
use rdy_model::Role;
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::{approver, land, rdy_bin, write};
use rdy_qa::mcp::{McpClient, Reply as McpReply};
use rdy_qa::platform::set_mode;
use rdy_render::publish::PublicationRenderer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const HANDLE: &str = "test-person";
/// The remote's path on the hub, and the sync base discovery names under it.
const REMOTE_PATH: &str = "/acme/docs";
const SYNC_BASE: &str = "/sync/acme/docs";
const SITE: &str = "docs";
const REQUEST_ID: &str = "req_00000000000000000000000009";
/// The fallback of an engine that has no daemon.
const NO_DAEMON_MESSAGE: &str = "rdy_publish runs in the daemon, and this session runs without one";
const NO_DAEMON_FIX: &str =
    "drop --no-daemon, or run rdy publish <tag> --to <remote> --site <site>";

// ---------------------------------------------------------------------------------------------
// The hub
// ---------------------------------------------------------------------------------------------

/// What the hub does differently from a plain success.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Discovery names a sync base on this origin, outside the install and its pins.
    ForeignBase(String),
    /// The publish request is refused with this status and hub error body.
    RefusePublish(u16, String),
    /// The publish request's connection closes with no reply: a lost response.
    LosePublish,
}

/// One request the hub read.
#[derive(Clone, Debug)]
struct Req {
    method: String,
    /// The path, without the query.
    path: String,
    query: BTreeMap<String, String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

struct Shared {
    config: Arc<ServerConfig>,
    origin: String,
    log: Mutex<Vec<Req>>,
    fault: Mutex<Fault>,
    /// The state reply's `profile` and `audience`.
    site: Mutex<(Value, Value)>,
    /// The bytes put to each push's quarantine so far, and its batches.
    objects: Mutex<BTreeMap<String, (u64, u64)>>,
    candidates: AtomicUsize,
}

/// A loopback TLS hub that speaks the publication upload protocol.
struct Hub {
    shared: Arc<Shared>,
    ca_pem: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Hub {
    fn start() -> Hub {
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
            fault: Mutex::new(Fault::None),
            site: Mutex::new((Value::Null, Value::Null)),
            objects: Mutex::new(BTreeMap::new()),
            candidates: AtomicUsize::new(0),
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

    fn origin(&self) -> &str {
        &self.shared.origin
    }

    fn fault(&self, f: Fault) {
        *self.shared.fault.lock().unwrap() = f;
    }

    /// The state reply's site profile and audience (null for none).
    fn site(&self, profile: Value, audience: Value) {
        *self.shared.site.lock().unwrap() = (profile, audience);
    }

    /// Every request read so far, in order. A request is logged before it is answered, and the
    /// client reads its reply before it returns, so a finished command's requests are all here.
    fn requests(&self) -> Vec<Req> {
        self.shared.log.lock().unwrap().clone()
    }

    /// The publish requests read so far.
    fn publishes(&self) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.path == format!("{SYNC_BASE}/publish"))
            .collect()
    }

    /// Stops the listener and joins every thread.
    fn finish(mut self) {
        self.stop_threads();
    }

    fn stop_threads(&mut self) {
        let Some(acceptor) = self.acceptor.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        // wake the blocked accept
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

/// Reads one request: its head, then exactly its content-length of body.
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
        _ => "Status",
    }
}

fn reply(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    )
    .into_bytes()
}

/// Serves one connection: one request, its reply (or none, for a lost response), then close.
fn serve(stream: TcpStream, shared: &Shared) {
    let Ok(conn) = ServerConnection::new(Arc::clone(&shared.config)) else {
        return;
    };
    let mut tls = StreamOwned::new(conn, stream);
    let Some(req) = read_request(&mut tls) else {
        let _ = tls.sock.shutdown(Shutdown::Both);
        return;
    };
    shared.log.lock().unwrap().push(req.clone());
    // no reply closes the connection at once: a lost response
    if let Some(bytes) = answer(&req, shared) {
        let _ = tls.write_all(&bytes);
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.conn.complete_io(&mut tls.sock);
    }
    let _ = tls.sock.shutdown(Shutdown::Both);
}

/// The reply to `req`, or `None` to close the connection without one.
fn answer(req: &Req, shared: &Shared) -> Option<Vec<u8>> {
    let fault = shared.fault.lock().unwrap().clone();
    let route = req.path.strip_prefix(SYNC_BASE).unwrap_or("");
    match (req.method.as_str(), req.path.as_str(), route) {
        ("GET", REMOTE_PATH, _) => {
            let base = match &fault {
                Fault::ForeignBase(o) => format!("{o}{SYNC_BASE}"),
                _ => SYNC_BASE.to_string(),
            };
            Some(reply(
                200,
                &json!({"sync_base": base, "protocols": [1]}).to_string(),
            ))
        }
        ("POST", _, "/state") => {
            let r = req.json();
            let (profile, audience) = shared.site.lock().unwrap().clone();
            let state = json!({"canon": null, "repo_id": null, "rdy": r["rdy"],
                "stream_read": r["stream_read"], "stream_write": r["stream_write"],
                "features": [], "min_rdy": null, "batch_bytes": 104_857_600u64,
                "batch_records": 100_000, "quarantine_seconds": 86_400, "profile": profile,
                "audience": audience, "artifact_bytes": 1_073_741_824u64,
                "artifact_files": 32_000});
            Some(reply(200, &state.to_string()))
        }
        ("POST", _, "/known") => {
            let r = req.json();
            Some(reply(200, &json!({"missing": r["keys"]}).to_string()))
        }
        ("POST", _, "/objects/put") => {
            let push = req.query.get("push").cloned().unwrap_or_default();
            let mut objects = shared.objects.lock().unwrap();
            let held = objects.entry(push).or_insert((0, 0));
            held.0 += 1;
            held.1 += req.body.len() as u64;
            Some(reply(
                200,
                &json!({"records": held.0, "bytes": held.1}).to_string(),
            ))
        }
        ("POST", _, "/files/known") => {
            let r = req.json();
            let mut hashes: Vec<String> = r["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["hash"].as_str().unwrap().to_string())
                .collect();
            hashes.sort();
            hashes.dedup();
            Some(reply(200, &json!({"missing": hashes}).to_string()))
        }
        ("POST", _, "/files/put") => {
            let hash = req.query.get("hash").cloned().unwrap_or_default();
            Some(reply(
                201,
                &json!({"hash": hash, "size": req.body.len()}).to_string(),
            ))
        }
        ("POST", _, "/publish") => match fault {
            Fault::RefusePublish(status, body) => Some(reply(status, &body)),
            Fault::LosePublish => None,
            _ => {
                let n = shared.candidates.fetch_add(1, Ordering::SeqCst) + 1;
                let id = format!("pubc_{n:026}");
                let url = format!("{}/app/publications/{id}", shared.origin);
                Some(reply(
                    201,
                    &json!({"id": id, "state": "candidate", "url": url}).to_string(),
                ))
            }
        },
        _ => Some(reply(404, "")),
    }
}

// ---------------------------------------------------------------------------------------------
// Homes, projects and commands
// ---------------------------------------------------------------------------------------------

/// A private user config home: `rdy/config.yaml` names the person and trusts the hub's CA; the
/// directories are 0700.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new(hub: &Hub) -> Home {
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

    fn config(&self) -> PathBuf {
        self.dir.path().join("rdy/config.yaml")
    }

    fn creds(&self) -> PathBuf {
        self.dir.path().join("rdy/credentials.yaml")
    }

    fn env(&self) -> Vec<(String, String)> {
        let home = self.dir.path().to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".into(), home.clone()),
            ("HOME".into(), home),
            ("USER".into(), "qa".into()),
        ]
    }

    /// A ready sign-in to `origin`, fresh for 50 minutes.
    fn signed_in(&self, origin: &str) {
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

/// A served project in a short directory: an idle timeout of 60 s, and `rdy serve --stop` when
/// it is dropped. Its tag `v1.0.0` holds the folder `pubs` with the doc `a`, whose prose refers
/// to the doc `out` outside the folder, and the doc `admin` with the org profile `site`
/// (selecting `pubs`). A later landing adds the doc `b`: the tag, not canon, is published.
struct Proj {
    dir: tempfile::TempDir,
    /// The profile `site`'s block id.
    profile: String,
}

impl Proj {
    fn new() -> Proj {
        let dir = short_tempdir();
        let mut r = Rdy::init(dir.path(), "rdy-standard").expect("init");
        let w = write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
                {"op": "create", "type": "doc", "id": "$a", "parent": "$pubs", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
            ]),
        )
        .unwrap();
        let (pubs, a, out, admin) = (
            w.created["$pubs"].clone(),
            w.created["$a"].clone(),
            w.created["$out"].clone(),
            w.created["$admin"].clone(),
        );
        let w = write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}}.")},
                {"op": "create", "type": "prose", "parent": out, "body": "Outside."},
                {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin,
                 "fields": {"key": "site", "title": "Site", "audience": "org", "select": [pubs]}}
            ]),
        )
        .unwrap();
        let profile = w.created["$site"].clone();
        land(&mut r, "work").unwrap();
        r.tag(&approver(), "v1.0.0", None, None).unwrap();
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "doc", "parent": pubs, "fields": {"name": "b", "title": "B", "doc_type": "page"}}]),
        )
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        let cfg = dir.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).unwrap();
        Proj { dir, profile }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Adds the publish remote `pub` at `url` to the project config, as `home`'s user.
    fn remote(&self, home: &Home, url: &str) {
        RemoteService::new(self.path(), &home.config())
            .add("pub", url, RemoteMode::Publish, RemoteLevel::Project)
            .unwrap();
    }

    /// Stops the project's daemon, if one runs, and waits for it to exit.
    fn stop_daemon(&self, home: &Home) {
        let o = command(self.path(), home, &[], &["serve", "--stop"])
            .output()
            .expect("rdy serve --stop");
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = Command::new(rdy_bin())
            .env_clear()
            .arg("--project")
            .arg(self.dir.path())
            .args(["serve", "--stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A hub, a home that trusts it and is signed in there, and a project with the publish remote
/// `pub` on it.
struct Kit {
    hub: Hub,
    home: Home,
    proj: Proj,
}

fn kit() -> Kit {
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let proj = Proj::new();
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    Kit { hub, home, proj }
}

/// `rdy --project <p> <args>` as the person of `home`, with `extra` variables and nothing else
/// of this process.
fn command(p: &Path, home: &Home, extra: &[(&str, &str)], args: &[&str]) -> Command {
    let mut cmd = Command::new(rdy_bin());
    cmd.env_clear()
        .envs(home.env())
        .envs(extra.iter().copied())
        .arg("--project")
        .arg(p)
        .args(args)
        .current_dir(p)
        .stdin(Stdio::null());
    cmd
}

/// Runs `rdy <args>` for the kit's person and checks that no output holds a token.
fn run(k: &Kit, extra: &[(&str, &str)], args: &[&str]) -> Output {
    let o = command(k.proj.path(), &k.home, extra, args)
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

/// `rdy publish v1.0.0 --to pub --site docs` with `more` arguments.
fn publish_args<'a>(json: bool, no_daemon: bool, more: &[&'a str]) -> Vec<&'a str> {
    let mut a = Vec::new();
    if json {
        a.push("--json");
    }
    if no_daemon {
        a.push("--no-daemon");
    }
    a.extend(["publish", "v1.0.0", "--to", "pub", "--site", SITE]);
    a.extend_from_slice(more);
    a
}

/// The JSON of a `--json` run: stdout on success, the error on stderr's last line on failure.
fn parsed(o: &Output) -> (bool, Value) {
    let ok = o.status.success();
    let text = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        serde_json::from_str(text.trim().lines().last().unwrap_or(""))
            .unwrap_or(Value::String(text))
    });
    (ok, v)
}

fn cli_ok(k: &Kit, more: &[&str]) -> Value {
    let (ok, v) = parsed(&run(k, &[], &publish_args(true, false, more)));
    assert!(ok, "rdy publish {more:?}: {v}");
    v
}

fn cli_err(k: &Kit, no_daemon: bool) -> Value {
    let (ok, v) = parsed(&run(k, &[], &publish_args(true, no_daemon, &[])));
    assert!(!ok, "rdy publish succeeded: {v}");
    v
}

/// An MCP client of the kit's daemon (`rdy mcp`, the proxy, which starts the daemon if none
/// runs), with a session of `role` unless `role` is `None`.
fn chat(k: &Kit, role: Option<&str>) -> McpClient {
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

fn tool(c: &mut McpClient, args: Value) -> McpReply {
    let r = c.call("rdy_publish", args.clone());
    let text = format!("{r:?}");
    for s in [ACCESS, REFRESH, "Bearer"] {
        assert!(!text.contains(s), "rdy_publish {args}: {s} in {text}");
    }
    r
}

fn mcp_ok(c: &mut McpClient, args: Value) -> Value {
    match tool(c, args.clone()) {
        McpReply::Ok(v) => v,
        other => panic!("rdy_publish {args}: {other:?}"),
    }
}

fn mcp_err(c: &mut McpClient, args: Value) -> Value {
    match tool(c, args.clone()) {
        McpReply::ToolError(v) => v,
        other => panic!("rdy_publish {args}: expected a refusal, got {other:?}"),
    }
}

fn base_args() -> Value {
    json!({"tag": "v1.0.0", "to": "pub", "site": SITE})
}

/// A result with the parts each run makes for itself made common: the push id, the candidate id
/// and its page link.
fn normal(v: &Value) -> Value {
    let mut v = v.clone();
    for k in ["push", "id", "url"] {
        assert!(v[k].is_string(), "{k}: {v}");
        v[k] = json!(format!("{{{k}}}"));
    }
    v
}

/// An error's code, message, fix and issues, with `push` named as `{push}`.
fn error_parts(v: &Value, push: Option<&str>) -> Value {
    // the CLI's JSON error leaves out an empty issue list; MCP's has it
    let issues = match &v["issues"] {
        Value::Null => json!([]),
        i => i.clone(),
    };
    let text = json!({"code": v["code"], "message": v["message"], "fix": v["fix"],
        "issues": issues})
    .to_string();
    let text = match push {
        Some(p) => text.replace(p, "{push}"),
        None => text,
    };
    serde_json::from_str(&text).unwrap()
}

/// What a publish must leave unchanged: the store's history files (its log, which holds its
/// refs, and its format), the project and local config, the user config and the credentials.
/// The index, the store lock and the empty credential lock are not part of the record.
fn fixed(k: &Kit) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let store = k.proj.path().join(".rdy/store");
    let mut walk = vec![store.clone()];
    while let Some(dir) = walk.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if p.is_dir() {
                walk.push(p);
            } else if !(name.starts_with("index.") || name == "lock") {
                let rel = p.strip_prefix(&store).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    for f in [
        k.proj.path().join(".rdy.yaml"),
        k.proj.path().join(".rdy/local.yaml"),
        k.home.config(),
        k.home.creds(),
    ] {
        if let Ok(bytes) = std::fs::read(&f) {
            out.insert(f.display().to_string(), bytes);
        }
    }
    out
}

/// The publish result's members, in order.
const RESULT_KEYS: [&str; 11] = [
    "id",
    "state",
    "url",
    "push",
    "audience",
    "record",
    "references",
    "inventory",
    "warnings",
    "render_warnings",
    "review",
];

fn check_candidate(v: &Value, hub: &Hub) {
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = RESULT_KEYS.to_vec();
    want.sort();
    let mut got = keys.clone();
    got.sort();
    assert_eq!(got, want, "{v}");
    assert_eq!(v["state"], "candidate");
    assert_eq!(v["review"], "hub");
    let id = v["id"].as_str().unwrap();
    assert_eq!(
        v["url"],
        format!("{}/app/publications/{id}", hub.origin()),
        "the hub's page link, unchanged"
    );
}

/// The publish request of push `push`, and what that push uploaded: its object batches and its
/// files by hash.
fn upload_of(hub: &Hub, push: &str) -> (Value, Vec<Vec<u8>>, BTreeMap<String, Vec<u8>>) {
    let reqs = hub.requests();
    let publish = reqs
        .iter()
        .find(|r| r.path.ends_with("/publish") && r.json()["push"] == push)
        .unwrap_or_else(|| panic!("no publish of {push}"))
        .json();
    let objects = reqs
        .iter()
        .filter(|r| {
            r.path.ends_with("/objects/put")
                && r.query.get("push").map(String::as_str) == Some(push)
        })
        .map(|r| r.body.clone())
        .collect();
    let files = reqs
        .iter()
        .filter(|r| {
            r.path.ends_with("/files/put") && r.query.get("push").map(String::as_str) == Some(push)
        })
        .map(|r| (r.query["hash"].clone(), r.body.clone()))
        .collect();
    (publish, objects, files)
}

/// The publish body without its push id.
fn without_push(body: &Value) -> Value {
    let mut b = body.clone();
    b.as_object_mut().unwrap().remove("push");
    b
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// One publication, made twice by separate invocations: the CLI through the daemon as the
/// person, and MCP through the same daemon as an agent. The two results, publish bodies, object
/// batches and files are the same except the push id and the hub's candidate id.
#[test]
fn hpb15_cli_and_mcp_publish_the_same_record_and_artifact() {
    let k = kit();
    let cli = cli_ok(&k, &[]);
    let mut c = chat(&k, Some("writer"));
    let chat_reply = mcp_ok(&mut c, base_args());
    check_candidate(&cli, &k.hub);
    check_candidate(&chat_reply, &k.hub);
    assert_ne!(cli["push"], chat_reply["push"], "each run has its own push");
    assert_ne!(cli["id"], chat_reply["id"]);
    assert_eq!(
        normal(&cli),
        normal(&chat_reply),
        "one result on both surfaces"
    );
    assert_eq!(cli["audience"], "org");
    assert_eq!(cli["record"]["tag"], "v1.0.0");
    assert_eq!(
        cli["record"]["profile"],
        Value::Null,
        "the site names no profile"
    );

    let (p1, o1, f1) = upload_of(&k.hub, cli["push"].as_str().unwrap());
    let (p2, o2, f2) = upload_of(&k.hub, chat_reply["push"].as_str().unwrap());
    assert_eq!(without_push(&p1), without_push(&p2), "one publish body");
    assert_eq!(
        p1["record"], cli["record"],
        "the result's record is the one sent"
    );
    assert!(
        !o1.is_empty() && !f1.is_empty(),
        "history and files were sent"
    );
    assert_eq!(o1, o2, "one tag history");
    assert_eq!(f1, f2, "one artifact");
    let manifest: Vec<String> = p1["manifest"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["hash"].as_str().unwrap().to_string())
        .collect();
    for h in &manifest {
        assert!(f1.contains_key(h), "the manifest's file {h} was uploaded");
    }
    // the person's claim is bound to the hub handle; the agent's is its own
    let actors: Vec<Option<String>> = k
        .hub
        .publishes()
        .iter()
        .map(|r| r.header("rdy-actor").map(str::to_string))
        .collect();
    assert_eq!(
        actors,
        [
            Some("human:test-person".to_string()),
            Some("agent:claude".to_string())
        ]
    );
    drop(c);
    k.hub.finish();
}

/// A session is required over MCP, unknown keys get the nearest key, and a keep_latest that is
/// not a boolean is refused on both surfaces before any request. With one authorized hub
/// credential every role publishes: reader, writer and reviewer over MCP, the person over the
/// CLI, and an approver session through the same coordinator in process.
#[test]
fn hpb15_publish_requires_a_session_and_refuses_unknown_keys() {
    let k = kit();
    let mut c = chat(&k, None);
    let e = mcp_err(&mut c, base_args());
    assert_eq!(
        (e["code"].clone(), e["message"].clone(), e["fix"].clone()),
        (
            json!("E_NO_SESSION"),
            json!("no session"),
            json!("call rdy_session first with your actor, run and role")
        )
    );
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:claude", "run": "r7", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let e = mcp_err(&mut c, json!({"tag": "v1.0.0", "to": "pub", "ste": SITE}));
    assert_eq!(e["code"], "E_BAD_INPUT");
    assert_eq!(e["message"], "rdy_publish: unknown parameter \"ste\"");
    assert_eq!(
        e["fix"],
        "did you mean \"site\"? the parameters here are keep_latest, site, tag, to"
    );
    let mut args = base_args();
    args["keep_latest"] = json!("true");
    let e = mcp_err(&mut c, args);
    assert_eq!(e["code"], "E_BAD_INPUT");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .starts_with("rdy_publish: parameter \"keep_latest\" must be "),
        "{e}"
    );
    // clap refuses a value other than true or false, and an unknown flag
    let o = run(
        &k,
        &[],
        &publish_args(false, false, &["--keep-latest", "yes"]),
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("invalid value 'yes' for '--keep-latest <KEEP_LATEST>'"),
        "{err}"
    );
    let o = run(
        &k,
        &[],
        &["publish", "v1.0.0", "--to", "pub", "--ste", SITE],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("unexpected argument '--ste'"), "{err}");
    assert!(err.contains("'--site'"), "the nearest flag: {err}");
    assert!(k.hub.requests().is_empty(), "nothing was sent");

    // every role publishes with the same credential
    let mut pushes = Vec::new();
    drop(c);
    for role in ["reader", "writer", "reviewer"] {
        let mut c = chat(&k, Some(role));
        let v = mcp_ok(&mut c, base_args());
        check_candidate(&v, &k.hub);
        pushes.push(v["push"].as_str().unwrap().to_string());
    }
    let v = cli_ok(&k, &[]);
    check_candidate(&v, &k.hub);
    pushes.push(v["push"].as_str().unwrap().to_string());
    // an approver session exists only inside rdy (no surface starts one): it runs the same
    // coordinator, with the same renderer, credential and hub, once the daemon has stopped
    k.proj.stop_daemon(&k.home);
    let local = Local::open(k.proj.path()).unwrap();
    let install = Origin::parse(k.hub.origin()).unwrap();
    let settings = HubConfig::load(Some(k.proj.path()), Some(&k.home.config())).unwrap();
    let api = HttpHubTransport::new(settings.fence(&install).unwrap()).unwrap();
    let wire =
        HttpPublishWire::new(HttpHubTransport::new(settings.fence(&install).unwrap()).unwrap());
    let clock = SystemHubClock;
    let s = Session::human("human:qa".into(), Role::Approver).unwrap();
    assert_eq!(approver().role, Role::Approver);
    let result = PublishService::new(k.proj.path(), &k.home.config(), &api, &clock, &wire)
        .run(
            local.read(),
            &s,
            PublishRequest {
                tag: "v1.0.0".into(),
                to: "pub".into(),
                site: SITE.into(),
                keep_latest: None,
            },
            &PublicationRenderer,
        )
        .unwrap();
    let v = result_json(&result).unwrap();
    check_candidate(&v, &k.hub);
    pushes.push(v["push"].as_str().unwrap().to_string());
    local.close().unwrap();
    assert_eq!(k.hub.publishes().len(), 5, "one candidate per role");
    let published: Vec<String> = k
        .hub
        .publishes()
        .iter()
        .map(|r| r.json()["push"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(published, pushes);
    for r in k.hub.requests() {
        if r.method == "POST" {
            assert_eq!(
                r.header("authorization"),
                Some(&*format!("Bearer {ACCESS}")),
                "{}",
                r.path
            );
        }
    }
    k.hub.finish();
}

/// The text output says candidate, links to the hub's review and pins every line: the
/// candidate, its link, the profile (the full id, or implicit:org), the audience, the selected
/// block count and the review link, then each settings warning in the shared warning form, then
/// each render warning.
#[test]
fn hpb15_text_says_candidate_and_links_to_review() {
    let k = kit();
    let text_of = |o: &Output| {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    // the implicit profile: no warning
    let o = run(&k, &[], &publish_args(false, false, &[]));
    let text = text_of(&o);
    let body = k.hub.publishes().last().unwrap().json();
    let push = body["push"].as_str().unwrap();
    let (_, _, files) = upload_of(&k.hub, push);
    assert!(!files.is_empty());
    let n = body["record"]["selection"].as_array().unwrap().len();
    let id = format!("pubc_{:026}", 1);
    let url = format!("{}/app/publications/{id}", k.hub.origin());
    assert_eq!(
        text,
        format!(
            "candidate {id}\n{url}\nprofile: implicit:org\naudience: org\nselected: {n} blocks\ndisclosure review: {url}\n"
        )
    );
    assert!(!text.contains("published"), "a candidate is not live");

    // the site's own profile, a settings warning and a render warning
    let version = "0".repeat(64);
    k.hub.site(
        json!({"id": k.proj.profile, "version": version}),
        json!("org"),
    );
    let mut cfg = std::fs::read_to_string(k.home.config()).unwrap();
    cfg.push_str("      pinned_origns: []\n");
    std::fs::write(k.home.config(), cfg).unwrap();
    // the daemon reads the user config of its own process on each call
    let v = cli_ok(&k, &[]);
    let warnings = v["warnings"].as_array().unwrap().clone();
    let render_warnings = v["render_warnings"].as_array().unwrap().clone();
    assert!(!warnings.is_empty(), "a settings warning: {v}");
    assert!(!render_warnings.is_empty(), "a render warning: {v}");
    assert_eq!(v["record"]["profile"]["id"], json!(k.proj.profile));
    let o = run(&k, &[], &publish_args(false, false, &[]));
    let text = text_of(&o);
    let id = format!("pubc_{:026}", 3);
    let url = format!("{}/app/publications/{id}", k.hub.origin());
    let n = v["inventory"]["selection_count"].as_u64().unwrap();
    let mut want = format!(
        "candidate {id}\n{url}\nprofile: {}\naudience: org\nselected: {n} blocks\ndisclosure review: {url}",
        k.proj.profile
    );
    for w in &warnings {
        want.push_str(&format!(
            "\nwarning {}: {}; {}",
            w["code"].as_str().unwrap(),
            w["message"].as_str().unwrap(),
            w["fix"].as_str().unwrap()
        ));
    }
    for w in &render_warnings {
        want.push_str(&format!("\nwarning: {}", w.as_str().unwrap()));
    }
    want.push('\n');
    assert_eq!(text, want);
    k.hub.finish();
}

/// Each transport refusal is the same on both surfaces: an untrusted origin, a missing sign-in,
/// the hub's own refusal (its code, message, fix and request-id issue) and a lost publish reply,
/// which is uncertain and names the push. The text form prints the error and its issues, never
/// a candidate, and exits nonzero.
#[test]
fn hpb15_transport_refusals_match_across_surfaces() {
    let k = kit();
    let mut c = chat(&k, Some("writer"));
    let both = |c: &mut McpClient, k: &Kit| -> (Value, Value) {
        let cli = cli_err(k, false);
        let chat_reply = mcp_err(c, base_args());
        (cli, chat_reply)
    };

    // an origin the install does not trust: only discovery is sent, twice
    k.hub
        .fault(Fault::ForeignBase("https://127.0.0.1:9".into()));
    let (cli, chat_reply) = both(&mut c, &k);
    assert_eq!(error_parts(&cli, None), error_parts(&chat_reply, None));
    assert_eq!(cli["code"], "E_UPSTREAM_ORIGIN");
    assert_eq!(
        cli["fix"],
        "use the install origin, or add its destination to the pinned origins of this install (hub.installs.<install>.pinned_origins)"
    );
    let routes: Vec<String> = k.hub.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        routes,
        [REMOTE_PATH, REMOTE_PATH],
        "no request left the install"
    );

    // no sign-in: refused before any request
    k.hub.fault(Fault::None);
    let creds = std::fs::read(k.home.creds()).unwrap();
    std::fs::remove_file(k.home.creds()).unwrap();
    let (cli, chat_reply) = both(&mut c, &k);
    assert_eq!(error_parts(&cli, None), error_parts(&chat_reply, None));
    assert_eq!(cli["code"], "E_UPSTREAM_AUTH");
    assert_eq!(
        cli["message"],
        format!("sign-in is required for {}", k.hub.origin())
    );
    assert_eq!(k.hub.requests().len(), 2, "nothing was sent");
    std::fs::write(k.home.creds(), creds).unwrap();
    set_mode(&k.home.creds(), 0o600).unwrap();

    // the hub refuses the publish: its code, message and fix, and the request id
    let refused = json!({"code": "E_HUB_TAG_PUBLISHED", "message": "tag v1.0.0 is published on this site",
        "fix": "publish a new tag", "request_id": REQUEST_ID, "issues": []});
    k.hub.fault(Fault::RefusePublish(409, refused.to_string()));
    let (cli, chat_reply) = both(&mut c, &k);
    assert_eq!(error_parts(&cli, None), error_parts(&chat_reply, None));
    assert_eq!(
        (
            cli["code"].clone(),
            cli["message"].clone(),
            cli["fix"].clone()
        ),
        (
            json!("E_HUB_TAG_PUBLISHED"),
            json!("tag v1.0.0 is published on this site"),
            json!("publish a new tag")
        )
    );
    let issues = cli["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["message"] == format!("hub request {REQUEST_ID}")),
        "{cli}"
    );

    // the publish reply is lost: uncertain, naming the push the hub saw; never a failure
    k.hub.fault(Fault::LosePublish);
    let before = k.hub.publishes().len();
    let cli = cli_err(&k, false);
    let cli_push = k.hub.publishes()[before].json()["push"]
        .as_str()
        .unwrap()
        .to_string();
    let chat_reply = mcp_err(&mut c, base_args());
    let chat_push = k.hub.publishes()[before + 1].json()["push"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        error_parts(&cli, Some(&cli_push)),
        error_parts(&chat_reply, Some(&chat_push))
    );
    assert_eq!(
        error_parts(&cli, Some(&cli_push)),
        json!({"code": "E_UPSTREAM_UNCERTAIN", "message": "the publication upload may have completed",
            "fix": "check push {push} on the hub before publishing again",
            "issues": [{"severity": "error", "code": "E_UPSTREAM_UNCERTAIN", "block": null,
                "message": "upload phase publish",
                "fix": "check push {push} on the hub before publishing again"}]})
    );
    // the text form: the error and its issue on stderr, nothing on stdout, a nonzero exit
    let o = run(&k, &[], &publish_args(false, false, &[]));
    assert!(!o.status.success());
    assert!(
        o.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let push = k.hub.publishes().last().unwrap().json()["push"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "error E_UPSTREAM_UNCERTAIN: the publication upload may have completed\nfix: check push {push} on the hub before publishing again\n  E_UPSTREAM_UNCERTAIN None: upload phase publish (fix: check push {push} on the hub before publishing again)\n"
        )
    );
    let o = {
        k.hub.fault(Fault::RefusePublish(409, refused.to_string()));
        run(&k, &[], &publish_args(false, false, &[]))
    };
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.starts_with("error E_HUB_TAG_PUBLISHED: tag v1.0.0 is published on this site\nfix: publish a new tag\n"),
        "{err}"
    );
    assert!(err.contains(&format!("hub request {REQUEST_ID}")), "{err}");
    drop(c);
    k.hub.finish();
}

/// With no daemon, the CLI publishes through its own process (the established direct route),
/// and MCP in process refuses with the documented fallback before any request.
#[test]
fn hpb15_no_daemon_cli_and_mcp_follow_the_documented_paths() {
    let k = kit();
    let o = run(&k, &[], &publish_args(true, true, &[]));
    let (ok, v) = parsed(&o);
    assert!(ok, "{v}");
    check_candidate(&v, &k.hub);
    assert!(
        !k.proj.path().join(".rdy/run/daemon.pid").exists(),
        "no daemon was started"
    );
    assert_eq!(k.hub.publishes().len(), 1);
    let env = k.home.env();
    let env: Vec<(&str, &str)> = env.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut c = McpClient::start_capturing(k.proj.path(), &env);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:claude", "run": "r7", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let before = k.hub.requests().len();
    let e = mcp_err(&mut c, base_args());
    assert_eq!(
        (e["code"].clone(), e["message"].clone(), e["fix"].clone()),
        (
            json!("E_BAD_INPUT"),
            json!(NO_DAEMON_MESSAGE),
            json!(NO_DAEMON_FIX)
        )
    );
    assert_eq!(k.hub.requests().len(), before, "nothing was sent");
    drop(c);
    k.hub.finish();
}

/// `keep_latest` on both surfaces: given, it reaches the hub beside the record (never inside
/// it); not given, the publish body has no such member at all.
fn keep_latest_reaches_the_hub(given: Option<bool>) {
    let k = kit();
    let word = given.map(|b| b.to_string());
    let mut more = Vec::new();
    if let Some(w) = &word {
        more.extend(["--keep-latest", w.as_str()]);
    }
    let cli = cli_ok(&k, &more);
    let mut c = chat(&k, Some("writer"));
    let mut args = base_args();
    if let Some(b) = given {
        args["keep_latest"] = json!(b);
    }
    let chat_reply = mcp_ok(&mut c, args);
    for v in [&cli, &chat_reply] {
        let (body, _, _) = upload_of(&k.hub, v["push"].as_str().unwrap());
        let members = body.as_object().unwrap();
        match given {
            Some(b) => assert_eq!(members.get("keep_latest"), Some(&json!(b)), "{body}"),
            None => assert!(!members.contains_key("keep_latest"), "{body}"),
        }
        assert!(
            !body["record"]
                .as_object()
                .unwrap()
                .contains_key("keep_latest"),
            "keep_latest is beside the record"
        );
        let mut keys: Vec<&str> = members.keys().map(String::as_str).collect();
        keys.sort();
        let mut want = vec!["manifest", "push", "record", "site"];
        if given.is_some() {
            want.insert(0, "keep_latest");
        }
        assert_eq!(keys, want);
        // the raw bytes say the same: no null was sent
        let raw = k
            .hub
            .publishes()
            .into_iter()
            .find(|r| r.json()["push"] == v["push"])
            .unwrap();
        let text = String::from_utf8_lossy(&raw.body);
        assert_eq!(
            text.contains("\"keep_latest\""),
            given.is_some(),
            "{given:?}"
        );
    }
    assert_eq!(normal(&cli), normal(&chat_reply));
    drop(c);
    k.hub.finish();
}

#[test]
fn hpb15_keep_latest_true_is_sent_beside_the_record() {
    keep_latest_reaches_the_hub(Some(true));
}

#[test]
fn hpb15_keep_latest_false_is_sent_beside_the_record() {
    keep_latest_reaches_the_hub(Some(false));
}

#[test]
fn hpb15_no_keep_latest_sends_no_field() {
    keep_latest_reaches_the_hub(None);
}

/// A failed or uncertain upload changes nothing here: the store's log, the project, local and
/// user config and the credentials keep their bytes, on the CLI (through the daemon and with no
/// daemon) and over MCP. Each phase's baseline is taken once its sessions are open, so only the
/// publishes run between the two readings.
#[test]
fn hpb15_failed_upload_leaves_source_log_and_config_unchanged() {
    let k = kit();
    let refused = json!({"code": "E_HUB_SITE_AUDIENCE", "message": "the site audience differs",
        "fix": "choose another site", "request_id": REQUEST_ID, "issues": []});
    let faults = [
        Fault::RefusePublish(409, refused.to_string()),
        Fault::LosePublish,
    ];
    let unchanged = |before: &BTreeMap<String, Vec<u8>>, phase: &str| {
        let after = fixed(&k);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "{phase}"
        );
        for (name, bytes) in before {
            assert!(after[name] == *bytes, "{phase}: {name} changed");
        }
    };
    // through the daemon: the person's CLI session and an agent's MCP session
    let mut c = chat(&k, Some("writer"));
    let o = run(&k, &[], &["status"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let before = fixed(&k);
    for fault in &faults {
        k.hub.fault(fault.clone());
        let e = cli_err(&k, false);
        assert!(e["code"].is_string(), "{e}");
        let e = mcp_err(&mut c, base_args());
        assert!(e["code"].is_string(), "{e}");
    }
    unchanged(&before, "through the daemon");
    drop(c);
    k.proj.stop_daemon(&k.home);
    // with no daemon: the CLI's own process
    let o = run(&k, &[], &["--no-daemon", "status"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let before = fixed(&k);
    for fault in faults {
        k.hub.fault(fault);
        let e = cli_err(&k, true);
        assert!(e["code"].is_string(), "{e}");
    }
    unchanged(&before, "with no daemon");
    assert_eq!(k.hub.publishes().len(), 6, "every run reached publish");
    k.hub.finish();
}
