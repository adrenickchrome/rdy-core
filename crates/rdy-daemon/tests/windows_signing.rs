//! The caller's SSH agent over its native endpoint: a named pipe on Windows, a socket elsewhere.
//! Only the seam's exports and `CallerAgent` reach the endpoint. The fake agent on Windows is a raw
//! pipe, as a real agent is: it does not speak the daemon's own stream layer. The Windows crate does
//! not build until every `sys` part exists, so an isolated probe runs this file there until then.
//! Every key here is a fixed test key. No test runs an SSH program.

use ed25519_dalek::Signer as _;
use rdy_daemon::signing_agent::CallerAgent;
use rdy_daemon::signing_transport::{CallerLink, CallerSigner, Stop, never_interrupted};
use rdy_daemon::sys::{self, AgentCancel};
use rdy_engine::engine::{Local, with_writer_result};
use rdy_engine::signing_act::{SigningAct, run_signed_act};
use rdy_engine::signing_crypto::{
    CallCancel, NS_APPROVAL, SigningAgent, blob_fingerprint, resolve_key, sign_sshsig,
    verify_sshsig,
};
use rdy_engine::{E_NO_AGENT, LandOptions, Rdy, RdyError, Session, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
use ssh_key::sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio::sync::Notify;

/// A hung test fails here. It asserts no timing: every wait below ends on an event.
const HANG_LIMIT: Duration = Duration::from_secs(120);

// ---- fixed test keys ----

const SK_APP: &[u8] = b"ssh:";

fn string(bytes: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

fn frame(payload: &[u8]) -> Vec<u8> {
    string(payload)
}

/// A fixed in-memory Ed25519 key, plain or as a security key's.
#[derive(Clone, Copy)]
struct TestKey {
    seed: [u8; 32],
    sk: bool,
}

impl TestKey {
    fn dalek(&self) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&self.seed)
    }

    fn public(&self) -> [u8; 32] {
        self.dalek().verifying_key().to_bytes()
    }

    fn blob(&self) -> Vec<u8> {
        if self.sk {
            [
                string(b"sk-ssh-ed25519@openssh.com"),
                string(&self.public()),
                string(SK_APP),
            ]
            .concat()
        } else {
            [string(b"ssh-ed25519"), string(&self.public())].concat()
        }
    }

    fn fingerprint(&self) -> String {
        blob_fingerprint(&self.blob())
    }

    /// An agent's signature blob over `payload`. A security key's carries user presence and a
    /// counter.
    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        if self.sk {
            let (flags, counter) = (1u8, 42u32);
            let mut signed = Sha256::digest(SK_APP).to_vec();
            signed.push(flags);
            signed.extend(counter.to_be_bytes());
            signed.extend(Sha256::digest(payload));
            let raw = self.dalek().sign(&signed).to_bytes();
            let mut out = [string(b"sk-ssh-ed25519@openssh.com"), string(&raw)].concat();
            out.push(flags);
            out.extend(counter.to_be_bytes());
            out
        } else {
            let raw = self.dalek().sign(payload).to_bytes();
            [string(b"ssh-ed25519"), string(&raw)].concat()
        }
    }
}

const KEY_A: TestKey = TestKey {
    seed: [0xa1; 32],
    sk: false,
};
const KEY_B: TestKey = TestKey {
    seed: [0xb2; 32],
    sk: false,
};
const KEY_SK: TestKey = TestKey {
    seed: [0xc3; 32],
    sk: true,
};

fn identities_answer(keys: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![12u8];
    out.extend(u32::try_from(keys.len()).unwrap().to_be_bytes());
    for key in keys {
        out.extend(string(key));
        out.extend(string(b"test key"));
    }
    out
}

/// The payload string of a sign request's body.
fn sign_payload(body: &[u8]) -> Vec<u8> {
    let len = |at: usize| u32::from_be_bytes(body[at..at + 4].try_into().unwrap()) as usize;
    let key = len(0);
    let payload = len(4 + key);
    body[8 + key..8 + key + payload].to_vec()
}

// ---- the fake agent ----

/// The raw bytes an agent writes for one request (its type and body), length prefix included.
type Script = Arc<dyn Fn(u8, &[u8]) -> Vec<u8> + Send + Sync>;

fn holding(key: TestKey) -> Script {
    Arc::new(move |ty, body| match ty {
        11 => frame(&identities_answer(&[key.blob()])),
        13 => {
            let mut reply = vec![14u8];
            reply.extend(string(&key.sign(&sign_payload(body))));
            frame(&reply)
        }
        _ => frame(&[5]),
    })
}

fn always(raw: Vec<u8>) -> Script {
    Arc::new(move |_, _| raw.clone())
}

/// What a fake agent does with each connection.
#[derive(Clone)]
enum Act {
    /// Reads the request, writes the script's reply and closes.
    Reply(Script),
    /// Reads the request, then answers nothing. It reports the client's close, and closes the
    /// connection itself at `release`.
    Hold { release: Arc<Notify> },
    /// Reads `bytes` bytes, reports them, and reads no more until `drain`. It then reads to the
    /// end and reports the client's close.
    ReadSome { bytes: usize, drain: Arc<Notify> },
}

/// What a fake agent saw.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    /// One whole request, length prefix included.
    Request(Vec<u8>),
    /// A [`Act::ReadSome`] has its bytes.
    Started,
    /// The client closed its connection: its handle is gone.
    Closed,
}

struct Fake {
    path: PathBuf,
    seen: Receiver<Seen>,
    _endpoint: Endpoint,
    _runtime: Runtime,
}

impl Fake {
    fn next(&self) -> Seen {
        self.seen.recv_timeout(HANG_LIMIT).expect("the test hung")
    }

    /// Everything seen so far.
    fn so_far(&self) -> Vec<Seen> {
        self.seen.try_iter().collect()
    }
}

/// One endpoint name, unused by any other test or process.
struct Endpoint {
    path: PathBuf,
    /// Holds a socket's directory.
    _dir: Option<tempfile::TempDir>,
}

static NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
fn endpoint() -> Endpoint {
    let n = NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    Endpoint {
        path: PathBuf::from(format!(
            r"\\.\pipe\rdy-wx18-test-{}-{n}",
            std::process::id()
        )),
        _dir: None,
    }
}

#[cfg(not(windows))]
fn endpoint() -> Endpoint {
    NEXT_ENDPOINT.fetch_add(1, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    Endpoint {
        path: dir.path().join("a"),
        _dir: Some(dir),
    }
}

/// The fake's listener: a raw pipe with one waiting instance on Windows, as an agent's is.
#[cfg(windows)]
mod raw {
    use std::io;
    use std::path::{Path, PathBuf};
    use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

    pub struct Listener {
        path: PathBuf,
        waiting: NamedPipeServer,
    }

    pub type Conn = NamedPipeServer;

    fn instance(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .pipe_mode(PipeMode::Byte)
            .create(path)
    }

    pub fn bind(path: &Path) -> Listener {
        Listener {
            path: path.to_path_buf(),
            waiting: instance(path, true).unwrap(),
        }
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Conn> {
            self.waiting.connect().await?;
            let next = instance(&self.path, false)?;
            Ok(std::mem::replace(&mut self.waiting, next))
        }
    }
}

#[cfg(not(windows))]
mod raw {
    use rdy_daemon::sys;
    use std::path::Path;

    pub type Listener = sys::Listener;
    pub type Conn = sys::Stream;

    pub fn bind(path: &Path) -> Listener {
        sys::bind(path).unwrap()
    }
}

fn fake(act: Act) -> Fake {
    let endpoint = endpoint();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (tx, seen) = channel();
    let mut listener = runtime.block_on(async { raw::bind(&endpoint.path) });
    runtime.spawn(async move {
        while let Ok(conn) = listener.accept().await {
            tokio::spawn(serve(conn, act.clone(), tx.clone()));
        }
    });
    Fake {
        path: endpoint.path.clone(),
        seen,
        _endpoint: endpoint,
        _runtime: runtime,
    }
}

/// Reads to the end, then reports the client's close.
async fn report_close(conn: &mut raw::Conn, tx: &Sender<Seen>) {
    let mut buf = vec![0u8; 64 * 1024];
    while matches!(conn.read(&mut buf).await, Ok(n) if n > 0) {}
    let _ = tx.send(Seen::Closed);
}

async fn serve(mut conn: raw::Conn, act: Act, tx: Sender<Seen>) {
    if let Act::ReadSome { bytes, drain } = &act {
        let mut some = vec![0u8; *bytes];
        if conn.read_exact(&mut some).await.is_ok() {
            let _ = tx.send(Seen::Started);
            drain.notified().await;
            report_close(&mut conn, &tx).await;
        }
        return;
    }
    let mut prefix = [0u8; 4];
    if conn.read_exact(&mut prefix).await.is_err() {
        return;
    }
    let mut request = vec![0u8; u32::from_be_bytes(prefix) as usize];
    if conn.read_exact(&mut request).await.is_err() {
        return;
    }
    let _ = tx.send(Seen::Request([&prefix[..], &request].concat()));
    match act {
        Act::Reply(script) => {
            let reply = script(request[0], &request[1..]);
            let _ = conn.write_all(&reply).await;
            // A pipe write copies the bytes and returns: this waits until the pipe has them all.
            let _ = conn.write(&[]).await;
        }
        Act::Hold { release } => {
            tokio::select! {
                () = report_close(&mut conn, &tx) => {}
                () = release.notified() => {}
            }
        }
        Act::ReadSome { .. } => unreachable!("handled above"),
    }
}

// ---- results ----

fn assert_cause<T: std::fmt::Debug>(r: Result<T, RdyError>, cause: &str, what: &str) {
    let e = r.expect_err(what);
    assert_eq!(e.code, E_NO_AGENT, "{what}: {e}");
    assert_eq!(
        e.message,
        format!("the SSH agent could not sign: {cause}"),
        "{what}"
    );
}

/// Runs `call` on its own thread: the result, and the thread ended by then. No test leaves a
/// worker behind, and a call that never ends fails the test.
fn on_worker<T: Send + 'static>(
    call: impl FnOnce() -> T + Send + 'static,
) -> (thread::JoinHandle<()>, Receiver<T>) {
    let (tx, rx) = channel();
    let worker = thread::spawn(move || {
        let _ = tx.send(call());
    });
    (worker, rx)
}

fn finish<T>(worker: thread::JoinHandle<()>, done: Receiver<T>) -> T {
    let result = done.recv_timeout(HANG_LIMIT).expect("the call never ended");
    worker.join().unwrap();
    result
}

/// Runs this test binary's helper test `name` in a child, with `SSH_AUTH_SOCK` set to `sock` or
/// removed, and `WX18_CASE` set to `case`.
fn helper(name: &str, sock: Option<&std::ffi::OsStr>, case: &str) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--ignored", "--nocapture"])
        .env("WX18_CASE", case)
        .env_remove("SSH_AUTH_SOCK");
    if let Some(sock) = sock {
        cmd.env("SSH_AUTH_SOCK", sock);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{name} {case}: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---- the caller's own agent, and no other ----

#[test]
fn wx18_signing_uses_only_the_callers_agent_pipe() {
    let mine = fake(Act::Reply(holding(KEY_A)));
    let other = fake(Act::Reply(holding(KEY_B)));
    let agent = CallerAgent::with_socket(&mine.path);

    // The exact requests, on the named endpoint: identities, then one sign of the statement.
    let statement = b"statement bytes \xff\x00\x01 kept whole";
    let key = resolve_key("signing.key", &KEY_A.fingerprint(), &agent).unwrap();
    assert_eq!(key.public_key, KEY_A.blob());
    assert_eq!(mine.next(), Seen::Request(vec![0, 0, 0, 1, 11]));
    let blob = sign_sshsig(&agent, &key, NS_APPROVAL, statement).unwrap();
    match mine.next() {
        Seen::Request(request) => {
            assert_eq!(request[4], 13);
            let body = &request[5..];
            assert_eq!(body[..4 + KEY_A.blob().len()], string(&KEY_A.blob()));
            assert_eq!(request[request.len() - 4..], [0, 0, 0, 0], "flags 0");
        }
        seen => panic!("{seen:?}"),
    }
    let check = verify_sshsig(NS_APPROVAL, statement, &blob);
    assert!(check.valid, "{check:?}");
    assert_eq!(check.key.as_deref(), Some(KEY_A.fingerprint().as_str()));
    assert!(!verify_sshsig(NS_APPROVAL, b"other bytes", &blob).valid);

    // A key the agent does not hold is not looked for anywhere else.
    assert_cause(
        resolve_key("signing.key", &KEY_B.fingerprint(), &agent),
        "key is not held",
        "another agent's key",
    );
    assert_eq!(other.so_far(), [], "no other agent is asked");

    // A security key signs through the same pipe, its presence bit intact.
    let sk = fake(Act::Reply(holding(KEY_SK)));
    let agent = CallerAgent::with_socket(&sk.path);
    let key = resolve_key("signing.key", &KEY_SK.fingerprint(), &agent).unwrap();
    let blob = sign_sshsig(&agent, &key, NS_APPROVAL, statement).unwrap();
    let check = verify_sshsig(NS_APPROVAL, statement, &blob);
    assert!(check.valid && check.presence, "{check:?}");

    // The environment of the caller names the agent. With nothing named there is no agent, and
    // nothing looks for the system's agent service.
    let name = "wx18_helper_the_callers_environment";
    let _ = mine.so_far();
    helper(name, Some(mine.path.as_os_str()), "reaches");
    assert_eq!(mine.so_far(), [Seen::Request(vec![0, 0, 0, 1, 11])]);
    helper(name, None, "unset");
    helper(name, Some(std::ffi::OsStr::new("")), "unset");
    assert_eq!(mine.so_far(), []);
    // A process whose environment names an agent, but which serves another's request, does not
    // use it: a caller with no agent of its own answers `not set`.
    helper(name, Some(mine.path.as_os_str()), "not-mine");
    assert_eq!(mine.so_far(), []);
}

#[test]
#[ignore = "wx18_signing_uses_only_the_callers_agent_pipe runs this in a child process"]
fn wx18_helper_the_callers_environment() {
    match std::env::var("WX18_CASE").unwrap().as_str() {
        "reaches" => {
            let keys = CallerAgent::from_env().identities().unwrap();
            assert_eq!(keys.len(), 1);
            assert_eq!(keys[0].public_key, KEY_A.blob());
        }
        "unset" => assert_cause(
            CallerAgent::from_env().identities(),
            "SSH_AUTH_SOCK is not set",
            "no agent named",
        ),
        "not-mine" => {
            let rt = Runtime::new().unwrap();
            let mut link = CallerLink::new(None, never_interrupted());
            let reply = rt.block_on(link.answer(
                br#"{"id":1,"request":1,"op":"identities"}"#,
                |_| Some(Arc::new(CallCancel::new())),
                std::future::pending(),
            ));
            let reply = reply.expect("a reply");
            let reply: Value = serde_json::from_slice(&reply[1..]).unwrap();
            assert_eq!(
                reply["error"],
                json!({"code": E_NO_AGENT, "cause": "SSH_AUTH_SOCK is not set"})
            );
        }
        other => panic!("{other}"),
    }
}

// ---- cancellation ----

#[test]
fn wx18_cancel_interrupts_a_blocked_agent_read() {
    let release = Arc::new(Notify::new());
    let f = fake(Act::Hold {
        release: Arc::clone(&release),
    });
    let agent = Arc::new(CallerAgent::with_socket(&f.path));

    // CallerAgent::cancel, during the identities read
    let (worker, done) = {
        let agent = Arc::clone(&agent);
        on_worker(move || agent.identities().map(|k| k.len()))
    };
    assert_eq!(f.next(), Seen::Request(vec![0, 0, 0, 1, 11]));
    agent.cancel();
    assert_cause(finish(worker, done), "request cancelled", "identities read");
    assert_eq!(f.next(), Seen::Closed, "the agent's connection is gone");

    // A request's cancel, during a security key's wait for a touch: no timeout ends this one.
    let cancel = Arc::new(CallCancel::new());
    let (worker, done) = {
        let (agent, cancel) = (Arc::clone(&agent), Arc::clone(&cancel));
        on_worker(move || agent.sign_until(&KEY_SK.blob(), b"signed data", &cancel))
    };
    assert!(matches!(f.next(), Seen::Request(_)));
    cancel.cancel();
    assert_cause(finish(worker, done), "request cancelled", "sign read");
    assert_eq!(f.next(), Seen::Closed);

    // The cancels applied to those calls only. A cancel with no call in flight changes nothing.
    agent.cancel();
    let answered = fake(Act::Reply(holding(KEY_A)));
    let agent = CallerAgent::with_socket(&answered.path);
    agent.cancel();
    assert_eq!(agent.identities().unwrap().len(), 1);

    // A cancel before the call starts: the call never connects.
    let f2 = fake(Act::Reply(holding(KEY_A)));
    let agent = CallerAgent::with_socket(&f2.path);
    let cancel = CallCancel::new();
    cancel.cancel();
    assert_cause(
        CallerSigner::identities_until(&agent, &cancel),
        "request cancelled",
        "cancelled first",
    );
    assert_eq!(f2.so_far(), []);
}

#[test]
fn wx18_close_interrupts_a_blocked_agent_write() {
    // Larger than every buffer between the two ends, so the write waits for its reader.
    const REQUEST: usize = 8 * 1024 * 1024;
    let drain = Arc::new(Notify::new());
    let f = fake(Act::ReadSome {
        bytes: 4096,
        drain: Arc::clone(&drain),
    });
    let mut stream = sys::agent_connect(&f.path, &AgentCancel::new().unwrap()).unwrap();
    let closer = sys::agent_handle(&stream).unwrap();
    let (worker, done) = on_worker(move || {
        let r = stream.write_all(&vec![0x41u8; REQUEST]);
        (r.map_err(|e| e.kind()), stream)
    });
    // The write is under way and the reader has stopped.
    assert_eq!(f.next(), Seen::Started);
    sys::agent_close(&closer);
    let (written, mut stream) = finish(worker, done);
    assert!(written.is_err(), "a closed write must fail");
    // Later calls fail at once on a closed stream.
    assert!(stream.write_all(b"more").is_err());
    let mut buf = [0u8; 4];
    assert!(matches!(stream.read(&mut buf), Err(_) | Ok(0)));
    // The handles go, and the agent sees the end.
    drop(stream);
    drop(closer);
    drain.notify_one();
    assert_eq!(f.next(), Seen::Closed);
}

#[test]
fn wx18_cancel_set_stays_set_and_wakes_a_wait() {
    let cancel = Arc::new(AgentCancel::new().unwrap());
    assert!(!cancel.is_set());
    let (worker, done) = {
        let cancel = Arc::clone(&cancel);
        on_worker(move || {
            cancel.wait();
            cancel.is_set()
        })
    };
    cancel.cancel();
    cancel.cancel();
    assert!(finish(worker, done));
    assert!(cancel.is_set());
    // A set cancel stops a connect before it starts.
    let f = fake(Act::Reply(holding(KEY_A)));
    let Err(e) = sys::agent_connect(&f.path, &cancel) else {
        panic!("a set cancel refuses");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::Interrupted);
}

// ---- disconnects ----

#[test]
fn wx18_disconnect_reaps_the_agent_worker() {
    // The agent goes while the call waits on its read: the worker ends as a lost connection.
    let release = Arc::new(Notify::new());
    let f = fake(Act::Hold {
        release: Arc::clone(&release),
    });
    let agent = Arc::new(CallerAgent::with_socket(&f.path));
    let (worker, done) = {
        let agent = Arc::clone(&agent);
        on_worker(move || agent.identities().map(|k| k.len()))
    };
    assert_eq!(f.next(), Seen::Request(vec![0, 0, 0, 1, 11]));
    release.notify_one();
    assert_cause(finish(worker, done), "connection failed", "the agent left");

    // The caller's own connection to the daemon goes while its worker waits on the agent: the
    // ask ends only once the worker has, and the agent's connection is closed by then. Repeated,
    // since each round would leave a worker behind.
    let f = fake(Act::Hold {
        release: Arc::new(Notify::new()),
    });
    let agent: Arc<dyn CallerSigner> = Arc::new(CallerAgent::with_socket(&f.path));
    let rt = Runtime::new().unwrap();
    for round in 1..=20u64 {
        let mut link = CallerLink::new(Some(Arc::clone(&agent)), never_interrupted());
        let line = format!(r#"{{"id":{round},"request":1,"op":"identities"}}"#);
        let (gone, stop) = tokio::sync::oneshot::channel::<()>();
        let answer = rt.spawn(async move {
            link.answer(
                line.as_bytes(),
                |_| Some(Arc::new(CallCancel::new())),
                async move {
                    let _ = stop.await;
                    Stop::Gone
                },
            )
            .await
        });
        assert_eq!(f.next(), Seen::Request(vec![0, 0, 0, 1, 11]), "{round}");
        gone.send(()).unwrap();
        let reply = rt
            .block_on(async { tokio::time::timeout(HANG_LIMIT, answer).await })
            .expect("the ask hung")
            .unwrap();
        assert_eq!(reply, None, "no one to reply to");
        assert_eq!(f.next(), Seen::Closed, "{round}");
    }
}

// ---- limits, causes and the modes ----

/// A project whose canon holds a seed doc.
struct Project {
    dir: tempfile::TempDir,
    engine: Arc<Local>,
    section: String,
    views: AtomicUsize,
}

fn person() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}

fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let seeded = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "seed", "title": "Seed", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "one"}
        ]),
    );
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&person(), &p.id).unwrap();
    Project {
        dir,
        engine: Arc::new(Local::new(r)),
        section: seeded.created["$s"].clone(),
        views: AtomicUsize::new(0),
    }
}

impl Project {
    fn mode(&self, mode: &str) {
        std::fs::write(
            self.dir.path().join(".rdy/local.yaml"),
            format!(
                "signing:\n  key: \"{}\"\n  agent_key: null\n  sign: {mode}\n  allowed_signers: null\n",
                KEY_A.fingerprint()
            ),
        )
        .unwrap();
    }

    /// Approves a new proposal as the person under `mode`, through `agent`. On a refusal it
    /// checks that the act wrote nothing.
    fn approve(&self, mode: &str, agent: &dyn SigningAgent) -> Result<Value, RdyError> {
        self.mode(mode);
        let view = format!("v{}", self.views.fetch_add(1, Ordering::SeqCst));
        let section = self.section.clone();
        let id = with_writer_result(&*self.engine, move |r| {
            write(
                r,
                &view,
                json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
            );
            let p = r.propose(&writer(), &view, &view, Some("Adds one claim."))?;
            Ok(p.id)
        })
        .unwrap();
        let end = with_writer_result(&*self.engine, |r| Ok(r.store().log_end())).unwrap();
        let result = run_signed_act(
            &*self.engine,
            agent,
            &person(),
            "human:qa",
            SigningAct::Approve {
                proposal: id,
                options: LandOptions::default(),
            },
        );
        if result.is_err() {
            let after = with_writer_result(&*self.engine, |r| Ok(r.store().log_end())).unwrap();
            assert_eq!(after, end, "a refused act writes nothing");
        }
        result
    }

    /// `never` asks no agent; `auto` saves the act with one warning; `always` refuses it.
    fn assert_modes_without_a_signature(&self, agent: &dyn SigningAgent, cause: &str) {
        let v = self.approve("never", agent).unwrap();
        assert_eq!(v["signatures"], json!([]), "{v:#}");
        assert_eq!(warnings(&v), Vec::<String>::new());

        let v = self.approve("auto", agent).unwrap();
        assert_eq!(v["signatures"], json!([]), "{v:#}");
        assert_eq!(
            warnings(&v),
            [format!(
                "the act was saved without signatures: {E_NO_AGENT}: the SSH agent could not sign: {cause}"
            )]
        );

        let e = self.approve("always", agent).unwrap_err();
        assert_eq!(e.code, E_NO_AGENT, "{e}");
        assert_eq!(
            e.message,
            format!("the SSH agent could not sign: {cause}"),
            "{e}"
        );
    }
}

/// The `W_SIGNING_FAILED` messages of an approval's result.
fn warnings(v: &Value) -> Vec<String> {
    let list = if v["landing"].is_object() {
        &v["landing"]["warnings"]
    } else {
        &v["warnings"]
    };
    list.as_array()
        .map(|w| {
            w.iter()
                .filter(|w| w["code"] == "W_SIGNING_FAILED")
                .map(|w| w["message"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A name no process serves.
fn no_endpoint() -> Endpoint {
    endpoint()
}

#[test]
fn wx18_signing_modes_keep_existing_no_agent_results() {
    // The closed causes, over the native endpoint, with every bound where it was.
    let both = |raw: Vec<u8>, cause: &str, what: &str| {
        let f = fake(Act::Reply(always(raw)));
        let agent = CallerAgent::with_socket(&f.path);
        assert_cause(agent.identities(), cause, what);
        assert_cause(agent.sign(&KEY_A.blob(), b"p"), cause, what);
    };
    both(frame(&[5]), "request refused", "refused, type 5");
    both(frame(&[30]), "request refused", "refused, type 30");
    both(frame(&[102]), "request refused", "refused, type 102");
    both(frame(&[5, 0]), "invalid reply", "a refusal with a tail");
    both(frame(&[99]), "invalid reply", "an unknown type");
    both(frame(&[]), "invalid reply", "an empty packet");
    both(vec![], "connection failed", "no reply");
    both(vec![0, 0], "connection failed", "a cut prefix");
    both(vec![0, 0, 0, 9, 12], "connection failed", "a cut packet");
    both(
        262_145u32.to_be_bytes().to_vec(),
        "reply exceeds 262144 bytes",
        "over the cap",
    );
    both(
        u32::MAX.to_be_bytes().to_vec(),
        "reply exceeds 262144 bytes",
        "the largest length",
    );
    // A body of exactly the cap is read whole, through the pipe's buffers, and judged on content.
    let mut at_cap = identities_answer(&[]);
    at_cap.resize(262_144, 0);
    let f = fake(Act::Reply(always(frame(&at_cap))));
    assert_cause(
        CallerAgent::with_socket(&f.path).identities(),
        "invalid reply",
        "at the cap",
    );
    let f = fake(Act::Reply(always(frame(&[12, 0, 0, 0, 0]))));
    assert_eq!(
        CallerAgent::with_socket(&f.path)
            .identities()
            .unwrap()
            .len(),
        0
    );
    let missing = no_endpoint();
    assert_cause(
        CallerAgent::with_socket(&missing.path).identities(),
        "connection failed",
        "nothing serves the name",
    );

    // The modes of one project, for each way an agent can fail.
    let p = project();
    p.assert_modes_without_a_signature(
        &CallerAgent::with_socket(&missing.path),
        "connection failed",
    );
    let refusing = fake(Act::Reply(always(frame(&[5]))));
    p.assert_modes_without_a_signature(
        &CallerAgent::with_socket(&refusing.path),
        "request refused",
    );
    let oversized = fake(Act::Reply(always(262_145u32.to_be_bytes().to_vec())));
    p.assert_modes_without_a_signature(
        &CallerAgent::with_socket(&oversized.path),
        "reply exceeds 262144 bytes",
    );
    let wrong_key = fake(Act::Reply(holding(KEY_B)));
    p.assert_modes_without_a_signature(
        &CallerAgent::with_socket(&wrong_key.path),
        "key is not held",
    );
    helper("wx18_helper_modes_with_no_agent", None, "unset");

    // The agent that holds the key signs under both modes that sign, and `never` asks nothing.
    let good = fake(Act::Reply(holding(KEY_A)));
    let agent = CallerAgent::with_socket(&good.path);
    for mode in ["auto", "always"] {
        let v = p.approve(mode, &agent).unwrap();
        assert_eq!(
            v["signatures"][0]["key"],
            json!(KEY_A.fingerprint()),
            "{v:#}"
        );
        assert_eq!(v["signatures"][0]["signed"], "valid", "{v:#}");
        assert_eq!(warnings(&v), Vec::<String>::new());
    }
    let _ = good.so_far();
    let v = p.approve("never", &agent).unwrap();
    assert_eq!(v["signatures"], json!([]), "{v:#}");
    assert_eq!(good.so_far(), [], "never asks no agent");
}

#[test]
#[ignore = "wx18_signing_modes_keep_existing_no_agent_results runs this in a child process"]
fn wx18_helper_modes_with_no_agent() {
    project()
        .assert_modes_without_a_signature(&CallerAgent::from_env(), "SSH_AUTH_SOCK is not set");
}

// ---- the real agent ----

/// The real Windows OpenSSH agent, reached only by the name the caller gives. The fixed test key
/// goes in over the agent protocol, with no SSH program, and comes out again however this ends.
#[cfg(windows)]
mod real {
    use super::*;

    const AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    const KEY: TestKey = TestKey {
        seed: [0x18; 32],
        sk: false,
    };

    /// One request on a connection of its own: the reply's bytes.
    fn request(body: &[u8]) -> Vec<u8> {
        let mut stream = sys::agent_connect(
            std::path::Path::new(AGENT_PIPE),
            &AgentCancel::new().unwrap(),
        )
        .unwrap();
        stream.write_all(&frame(body)).unwrap();
        let mut prefix = [0u8; 4];
        stream.read_exact(&mut prefix).unwrap();
        let mut reply = vec![0u8; u32::from_be_bytes(prefix) as usize];
        stream.read_exact(&mut reply).unwrap();
        reply
    }

    struct Added;

    impl Added {
        fn key() -> Added {
            let (public, seed) = (KEY.public(), KEY.seed);
            let body = [
                vec![17u8], // add identity
                string(b"ssh-ed25519"),
                string(&public),
                string(&[&seed[..], &public[..]].concat()),
                string(b"rdy wx18 test key"),
            ]
            .concat();
            assert_eq!(request(&body), [6], "the agent takes the test key");
            Added
        }
    }

    impl Drop for Added {
        fn drop(&mut self) {
            let body = [vec![18u8], string(&KEY.blob())].concat(); // remove identity
            let _ = request(&body);
        }
    }

    #[test]
    #[ignore = "needs the Windows OpenSSH Authentication Agent service"]
    fn wx18_real_openssh_agent_answers_through_the_callers_pipe() {
        let agent = CallerAgent::with_socket(AGENT_PIPE);
        // The agent reads the caller's token through the identification-only handle, and answers
        // for this account. It parses as every agent's answer does.
        agent.identities().unwrap();
        // A key it does not hold: the agent's own refusal, in the existing cause.
        let unheld = TestKey {
            seed: [0x19; 32],
            sk: false,
        };
        assert_cause(
            agent.sign(&unheld.blob(), b"SSHSIG"),
            "request refused",
            "a key the agent does not hold",
        );
        // A cancel before the call starts never reaches it, and a later call is served.
        let cancel = CallCancel::new();
        cancel.cancel();
        assert_cause(
            agent.identities_until(&cancel),
            "request cancelled",
            "cancelled first",
        );
        agent.identities().unwrap();
    }

    /// The agent keeps a key under the account's data protection, which a key-authenticated SSH
    /// logon cannot use: there it refuses the key, and this test fails at that assertion. Run it
    /// from a logon made with the account's password, at the console for example.
    #[test]
    #[ignore = "needs the Windows OpenSSH Authentication Agent service and a password logon"]
    fn wx18_real_openssh_agent_signs_with_a_test_key() {
        let _added = Added::key();
        let agent = CallerAgent::with_socket(AGENT_PIPE);
        let key = resolve_key("signing.key", &KEY.fingerprint(), &agent).unwrap();
        assert_eq!(key.public_key, KEY.blob());
        let statement = b"a statement for the real agent";
        let blob = sign_sshsig(&agent, &key, NS_APPROVAL, statement).unwrap();
        let check = verify_sshsig(NS_APPROVAL, statement, &blob);
        assert!(check.valid, "{check:?}");
        assert_eq!(check.key.as_deref(), Some(KEY.fingerprint().as_str()));
    }
}
