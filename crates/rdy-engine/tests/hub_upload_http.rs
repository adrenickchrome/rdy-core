//! hpb3: the upload transport seam over HA2a's HTTPS sender, and TASK-291's origin fixes. Every
//! test runs the real transport against loopback TLS fixtures with generated CAs; no public
//! network, sleep or elapsed-time assertion is used. Request counters come from the fixtures'
//! logs and accept counts.
#[path = "support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Ca, LOOPBACK, Recorded, Reply, Server};
use rdy_engine::hub_http::{
    HttpHubTransport, JsonLimits, MAX_UPLOAD_BODY_BYTES, MAX_UPLOAD_REPLY_BYTES, UploadHttpRequest,
    UploadMethod, check_json, decode_bounded_json, route,
};
use rdy_engine::hub_types::*;
use rdy_engine::{
    E_BAD_INPUT, E_IO, E_LIMIT, E_UPSTREAM_ORIGIN, E_UPSTREAM_TOKEN, RdyError, Session,
    SessionKind, UPSTREAM_ORIGIN_FIX, UPSTREAM_PUBLISH_ORIGIN_FIX, upstream_origin,
    upstream_publish_origin,
};
use rdy_model::Role;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::cell::Cell;
use std::io::{self, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";
const CHALLENGE_ID: &str = "kch_00000000000000000000000002";
const KEY_ID: &str = "skey_00000000000000000000000003";
const FINGERPRINT: &str = "SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM";
const PUBLIC_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur";
const PROOF: &str = "U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgzFkXAhAcuh8XkL9JjZQ+vrOCqPNR9xfr5cgKMCGy+6sAAAAOcmR5LWtleWJpbmQtdjEAAAAAAAAABnNoYTUxMgAAAFMAAAALc3NoLWVkMjU1MTkAAABA12AaXZO7xGPGx60J5a1JLmiKfRLNj0h+d0zhRo6jsA9h9riGCaqukgi6feNhe0qfxybjswnpsk/FX3YA0v/nDQ==";
const COMPROMISED: &str = "2026-10-17T11:00:00Z";
const METADATA_REPLY: &str = r#"{"issuer":"{o}/auth","authorization_endpoint":"{o}/auth/authorize","device_authorization_endpoint":"{o}/auth/device","token_endpoint":"{o}/auth/token","revocation_endpoint":"{o}/auth/revoke","registration_endpoint":"{o}/auth/register","scopes_supported":["pull","push","upload","change"],"response_types_supported":["code"],"grant_types_supported":["authorization_code","urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]}"#;

/// TASK-291's two fixes, as literal text.
const INSTALL_FIX: &str = "use the install origin";
const PUBLISH_FIX: &str = "use the install origin, or add its destination to the pinned origins of this install (hub.installs.<install>.pinned_origins)";
const BODY_MESSAGE: &str = "the upload body does not match its declared length";

const OCTET: &str = "application/octet-stream";
const SYNC_JSON: &str = "application/vnd.rdy.sync+json";
const OBJECTS: &str = "/sync/v1/objects/put?push=push_1";
const REPLY_CAP: usize = 1_048_576;
const OBJECT_REPLY: &str = r#"{"records":1,"bytes":5}"#;

/// The oauth bounds HA2a settled, which the upload seam must not widen.
const OAUTH_LIMITS: JsonLimits = JsonLimits {
    bytes: MAX_JSON_BYTES,
    depth: MAX_DEPTH,
    values: MAX_MEMBERS,
    scalar_bytes: MAX_SCALAR_BYTES,
};

fn claim() -> RequestClaim {
    RequestClaim::from_session(&Session {
        actor: "agent:claude".into(),
        run: Some("r7".into()),
        role: Role::Writer,
        kind: SessionKind::Agent,
    })
    .unwrap()
}

fn origin(server: &Server) -> Origin {
    Origin::parse(server.origin()).unwrap()
}

/// A CA bundle trusting every server in `servers`.
fn bundle(dir: &Path, servers: &[&Server]) -> PathBuf {
    let path = dir.join("ca.pem");
    let pem: String = servers.iter().map(|s| s.ca_pem()).collect();
    std::fs::write(&path, pem).unwrap();
    path
}

fn transport(install: &Origin, pins: &[Origin]) -> HttpHubTransport {
    HttpHubTransport::new(OriginFence::new(install.clone(), pins.iter().cloned()).unwrap()).unwrap()
}

fn post<'a>(destination: &'a str, body: &'a mut dyn Read, len: u64) -> UploadHttpRequest<'a> {
    UploadHttpRequest {
        method: UploadMethod::Post,
        destination,
        content_type: OCTET,
        body: Some(body),
        body_len: len,
        reply_limit: REPLY_CAP,
    }
}

fn get(destination: &str) -> UploadHttpRequest<'_> {
    UploadHttpRequest {
        method: UploadMethod::Get,
        destination,
        content_type: SYNC_JSON,
        body: None,
        body_len: 0,
        reply_limit: REPLY_CAP,
    }
}

/// A short name for a failure.
fn kind(f: &HubFailure) -> String {
    match f {
        HubFailure::ActorClaim => "ActorClaim".into(),
        HubFailure::AuthRequired => "AuthRequired".into(),
        HubFailure::Refused => "Refused".into(),
        HubFailure::Transport { may_have_sent } => format!("Transport({may_have_sent})"),
        HubFailure::Protocol => "Protocol".into(),
        HubFailure::InvalidHandle => "InvalidHandle".into(),
        HubFailure::Local(e) => format!("Local({})", e.code),
    }
}

fn local(r: Result<(u16, Vec<u8>), HubFailure>) -> RdyError {
    match r {
        Err(HubFailure::Local(e)) => e,
        Err(f) => panic!("expected a local refusal, got {}", kind(&f)),
        Ok((status, _)) => panic!("expected a local refusal, got {status}"),
    }
}

fn fails(r: Result<(u16, Vec<u8>), HubFailure>) -> String {
    match r {
        Err(f) => kind(&f),
        Ok((status, _)) => panic!("expected a failure, got {status}"),
    }
}

/// One upload to `server` with `reply` queued: the result and the one request the hub read.
fn served(
    server: &mut Server,
    reply: Reply,
    send: impl FnOnce() -> Result<(u16, Vec<u8>), HubFailure>,
) -> (Result<(u16, Vec<u8>), HubFailure>, Recorded) {
    let before = server.requests().len();
    server.reply(reply);
    server.accept_n(1);
    let r = send();
    let log = server.requests();
    assert_eq!(log.len(), before + 1, "one request per call: {r:?}");
    (r, log.last().unwrap().clone())
}

/// One upload to `server` on a connection that delivers no request (a failed handshake).
fn unread(
    server: &mut Server,
    send: impl FnOnce() -> Result<(u16, Vec<u8>), HubFailure>,
) -> Result<(u16, Vec<u8>), HubFailure> {
    let before = server.requests().len();
    let conn = server.accepted();
    server.accept_n(1);
    let r = send();
    server.wait_closed(conn);
    assert_eq!(server.accepted(), conn + 1, "one connection: {r:?}");
    assert_eq!(server.requests().len(), before, "no whole request: {r:?}");
    r
}

/// An upload refused before any connection: `server` accepts nothing more.
fn unsent(
    server: &Server,
    send: impl FnOnce() -> Result<(u16, Vec<u8>), HubFailure>,
) -> Result<(u16, Vec<u8>), HubFailure> {
    let accepted = server.accepted();
    let r = send();
    assert_eq!(server.accepted(), accepted, "no connection: {r:?}");
    r
}

/// What one sink connection read: the body length its head declared and the body bytes that
/// arrived before the client closed it.
struct Sunk {
    declared: Option<u64>,
    received: u64,
}

/// A loopback TLS hub for bodies that never complete. It sends no TLS 1.3 session ticket, so a
/// client that stops mid-body holds no unread byte and its close is a clean end the sink reads,
/// after every body byte the client sent. (With a ticket unread, the client's close is a reset
/// that a loopback peer with data in flight may never see.)
struct Sink {
    listener: Arc<TcpListener>,
    origin: String,
    ca: Ca,
    config: Arc<ServerConfig>,
}

impl Sink {
    fn start() -> Sink {
        let ca = Ca::new();
        let (chain, key) = ca.leaf(&[LOOPBACK]);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("server certificate");
        config.send_tls13_tickets = 0;
        let listener = TcpListener::bind((LOOPBACK, 0)).expect("bind loopback");
        let port = listener.local_addr().expect("address").port();
        Sink {
            listener: Arc::new(listener),
            origin: format!("https://{LOOPBACK}:{port}"),
            ca,
            config: Arc::new(config),
        }
    }

    /// Runs `send` while one connection is read to its end; its result and what was read.
    fn aborted(
        &self,
        send: impl FnOnce() -> Result<(u16, Vec<u8>), HubFailure>,
    ) -> (Result<(u16, Vec<u8>), HubFailure>, Sunk) {
        let listener = Arc::clone(&self.listener);
        let config = Arc::clone(&self.config);
        let reader = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let conn = ServerConnection::new(config).expect("server connection");
            let mut tls = StreamOwned::new(conn, stream);
            let mut all = Vec::new();
            let mut chunk = [0u8; 16_384];
            while let Ok(n) = tls.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&chunk[..n]);
            }
            let end = all
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("a request head");
            let head = String::from_utf8(all[..end].to_vec()).expect("UTF-8 head");
            let declared = head.lines().find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<u64>().expect("length"))
            });
            Sunk {
                declared,
                received: (all.len() - end - 4) as u64,
            }
        });
        let r = send();
        (r, reader.join().expect("sink thread"))
    }

    /// No further connection is waiting.
    fn finish(self) {
        self.listener.set_nonblocking(true).expect("nonblocking");
        match self.listener.accept() {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Ok(_) => panic!("an unexpected connection reached the sink"),
            Err(e) => panic!("accept failed: {e}"),
        }
    }
}

fn assert_origin(e: &RdyError, install: &Origin, fix: &str) {
    assert_eq!(e.code, E_UPSTREAM_ORIGIN, "{e}");
    assert_eq!(
        e.message,
        format!("the request destination is not trusted for {install}"),
        "{e}"
    );
    assert_eq!(e.fix, fix, "{e}");
}

/// `text` holds none of `secrets`.
fn assert_hidden(text: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!text.contains(s), "{s} leaked into {text}");
    }
}

/// A reader that hands out `data` at most `step` bytes at a time, reporting an interruption
/// before every other read, and counts the bytes it gave.
struct Chunky {
    data: Vec<u8>,
    pos: usize,
    step: usize,
    interrupt: bool,
    given: Cell<usize>,
}

impl Chunky {
    fn new(data: Vec<u8>, step: usize) -> Chunky {
        Chunky {
            data,
            pos: 0,
            step,
            interrupt: false,
            given: Cell::new(0),
        }
    }
}

impl Read for Chunky {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let n = buf.len().min(self.step).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.given.set(self.given.get() + n);
        Ok(n)
    }
}

/// An endless reader of `b'z'`, counting the bytes it gave.
struct Endless {
    given: u64,
}

impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        buf.fill(b'z');
        self.given += buf.len() as u64;
        Ok(buf.len())
    }
}

/// A reader that gives `left` bytes, then fails with an error quoting `secret`.
struct Failing {
    left: usize,
    secret: &'static str,
}

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::other(format!("disk failed: {}", self.secret)));
        }
        let n = buf.len().min(self.left);
        buf[..n].fill(b'f');
        self.left -= n;
        Ok(n)
    }
}

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Every HubOperation with the settled fixtures' inputs. The match below has no wildcard, so a
/// new operation must be added here.
fn every_operation() -> Vec<HubOperation> {
    let ops = vec![
        HubOperation::Metadata,
        HubOperation::DeviceStart {
            machine_name: "laptop".into(),
        },
        HubOperation::DevicePoll {
            device_code: Secret::new(DEVICE_CODE),
        },
        HubOperation::Refresh {
            refresh_token: Secret::new(REFRESH),
        },
        HubOperation::Me,
        HubOperation::SignOut {
            token: Secret::new(REFRESH),
        },
        HubOperation::BindChallenge {
            kind: KeyKind::Person,
            label: Some("laptop key".into()),
        },
        HubOperation::Bind {
            challenge: CHALLENGE_ID.into(),
            public_key: PUBLIC_KEY.into(),
            proof: PROOF.into(),
        },
        HubOperation::FindKey {
            fingerprint: FINGERPRINT.into(),
        },
        HubOperation::Retire {
            key_id: KEY_ID.into(),
        },
        HubOperation::Revoke {
            key_id: KEY_ID.into(),
            compromised_from: Some(COMPROMISED.into()),
        },
    ];
    let mut seen = std::collections::BTreeSet::new();
    for op in &ops {
        seen.insert(match op {
            HubOperation::Metadata => 0,
            HubOperation::DeviceStart { .. } => 1,
            HubOperation::DevicePoll { .. } => 2,
            HubOperation::Refresh { .. } => 3,
            HubOperation::Me => 4,
            HubOperation::SignOut { .. } => 5,
            HubOperation::BindChallenge { .. } => 6,
            HubOperation::Bind { .. } => 7,
            HubOperation::FindKey { .. } => 8,
            HubOperation::Retire { .. } => 9,
            HubOperation::Revoke { .. } => 10,
        });
    }
    assert_eq!(seen.len(), 11, "every operation once");
    ops
}

/// The call of `op` on `install` as a service would make it, sent to `base` instead.
fn call_to(install: &Origin, base: &str, ca: &Path, op: HubOperation) -> HubCall {
    let path = match &op {
        HubOperation::DeviceStart { .. } => "/auth/device".to_string(),
        HubOperation::DevicePoll { .. } | HubOperation::Refresh { .. } => "/auth/token".into(),
        HubOperation::SignOut { .. } => "/auth/revoke".into(),
        other => route(install, other)
            .unwrap()
            .unwrap()
            .strip_prefix(install.as_str())
            .unwrap()
            .to_string(),
    };
    let key_route = matches!(
        op,
        HubOperation::BindChallenge { .. }
            | HubOperation::Bind { .. }
            | HubOperation::FindKey { .. }
            | HubOperation::Retire { .. }
            | HubOperation::Revoke { .. }
    );
    let bearer = key_route || matches!(op, HubOperation::Me);
    HubCall {
        install: install.clone(),
        destination: format!("{base}{path}"),
        ca_file: Some(ca.to_path_buf()),
        claim: key_route.then(claim),
        bearer: bearer.then(|| Secret::new(ACCESS)),
        operation: op,
    }
}

// ---------------------------------------------------------------------------------------------

#[test]
fn hpb3_upload_accepts_only_the_install_or_its_pin() {
    let mut a = Server::start();
    let mut b = Server::start();
    let c = Server::start();
    let dir = tempfile::tempdir().unwrap();
    // The bundle trusts all three, so only the fence can refuse the third.
    let ca = bundle(dir.path(), &[&a, &b, &c]);
    let (ia, ib, ic) = (origin(&a), origin(&b), origin(&c));
    let pinned = transport(&ia, std::slice::from_ref(&ib));
    let bearer = Secret::new(ACCESS);
    let claim = claim();

    // The install itself: the exact request, streamed with its length.
    let dest = format!("{ia}{OBJECTS}");
    let mut body: &[u8] = b"hello";
    let (r, req) = served(&mut a, Reply::json(201, OBJECT_REPLY), || {
        pinned.upload(
            &ia,
            Some(ca.as_path()),
            &claim,
            &bearer,
            post(&dest, &mut body, 5),
        )
    });
    let (status, reply) = r.expect("install upload");
    assert_eq!((status, reply.as_slice()), (201, OBJECT_REPLY.as_bytes()));
    assert_eq!(req.method, "POST");
    assert_eq!(req.target, OBJECTS);
    assert_eq!(req.body, b"hello");
    assert_eq!(req.header("content-length"), Some("5"));
    assert_eq!(req.header("transfer-encoding"), None);
    assert_eq!(req.header("content-type"), Some(OCTET));
    assert_eq!(req.header("accept"), Some("application/json"));
    let auth = format!("Bearer {ACCESS}");
    assert_eq!(req.header("authorization"), Some(auth.as_str()));
    assert_eq!(req.header("rdy-actor"), Some("agent:claude"));
    assert_eq!(req.header("rdy-run"), Some("r7"));
    assert_eq!(req.header("cookie"), None);

    // The install's own pin: POST and GET.
    let dest_b = format!("{ib}{OBJECTS}");
    let mut body: &[u8] = b"hello";
    let (r, req) = served(&mut b, Reply::json(200, OBJECT_REPLY), || {
        pinned.upload(
            &ia,
            Some(ca.as_path()),
            &claim,
            &bearer,
            post(&dest_b, &mut body, 5),
        )
    });
    assert_eq!(r.expect("pinned upload").0, 200);
    assert_eq!(req.body, b"hello");
    assert_eq!(req.header("authorization"), Some(auth.as_str()));
    let disc = format!("{ib}/sync/v1");
    let (r, req) = served(&mut b, Reply::json(200, "{}"), || {
        pinned.upload(&ia, Some(ca.as_path()), &claim, &bearer, get(&disc))
    });
    assert_eq!(r.expect("pinned get").0, 200);
    assert_eq!(req.method, "GET");
    assert_eq!(req.target, "/sync/v1");
    assert_eq!(req.header("accept"), Some(SYNC_JSON));
    assert_eq!(req.header("content-type"), None);
    assert_eq!(req.header("content-length"), None);
    assert!(req.body.is_empty());

    // A third trusted origin is refused before any connection, with the pin-aware fix, and the
    // refusal names neither the destination nor the token.
    for dest in [
        format!("{ic}{OBJECTS}"),
        format!("{ic}/x?access_token={ACCESS}"),
        // a look-alike of the pin: another port, scheme, userinfo or fragment
        format!(
            "http://{}{OBJECTS}",
            ib.as_str().trim_start_matches("https://")
        ),
        format!(
            "https://u@{}{OBJECTS}",
            ib.as_str().trim_start_matches("https://")
        ),
        format!("{ib}{OBJECTS}#frag"),
    ] {
        let mut body: &[u8] = b"hello";
        let e = local(unsent(&c, || {
            pinned.upload(
                &ia,
                Some(ca.as_path()),
                &claim,
                &bearer,
                post(&dest, &mut body, 5),
            )
        }));
        assert_origin(&e, &ia, PUBLISH_FIX);
        assert_hidden(&format!("{e:?}"), &[ic.as_str(), ACCESS, "/x", "frag"]);
        assert_eq!(e.fix, upstream_publish_origin(ia.as_str()).fix);
    }
    // Without the pin, the same origin is refused the same way.
    let unpinned = transport(&ia, &[]);
    let mut body: &[u8] = b"hello";
    let e = local(unsent(&b, || {
        unpinned.upload(
            &ia,
            Some(ca.as_path()),
            &claim,
            &bearer,
            post(&dest_b, &mut body, 5),
        )
    }));
    assert_origin(&e, &ia, PUBLISH_FIX);
    // A pin belongs to its own install: C's transport does not trust A's pin. An install the
    // fence does not hold is refused with the install-only fix.
    let of_c = transport(&ic, &[]);
    let mut body: &[u8] = b"hello";
    let e = local(unsent(&b, || {
        of_c.upload(
            &ic,
            Some(ca.as_path()),
            &claim,
            &bearer,
            post(&dest_b, &mut body, 5),
        )
    }));
    assert_origin(&e, &ic, PUBLISH_FIX);
    let mut body: &[u8] = b"hello";
    let e = local(unsent(&b, || {
        pinned.upload(
            &ib,
            Some(ca.as_path()),
            &claim,
            &bearer,
            post(&dest_b, &mut body, 5),
        )
    }));
    assert_origin(&e, &ia, INSTALL_FIX);
    // The ordinary exchange stays on the install, even with the pin.
    let e = pinned
        .exchange(call_to(&ia, ib.as_str(), &ca, HubOperation::Me))
        .expect_err("refused");
    let HubFailure::Local(e) = e else {
        panic!("{}", kind(&e))
    };
    assert_origin(&e, &ia, INSTALL_FIX);

    assert_eq!(a.finish(), 0);
    assert_eq!(b.finish(), 0);
    assert_eq!(c.finish(), 0, "the refused origin saw no connection");
}

#[test]
fn hpb3_signin_and_keys_keep_install_only_fixes() {
    // The two helpers share their code and message; only the fix differs.
    let install = Origin::parse("https://a.test").unwrap();
    let plain = upstream_origin(install.as_str());
    let publish = upstream_publish_origin(install.as_str());
    assert_origin(&plain, &install, INSTALL_FIX);
    assert_origin(&publish, &install, PUBLISH_FIX);
    assert_eq!(UPSTREAM_ORIGIN_FIX, INSTALL_FIX);
    assert_eq!(UPSTREAM_PUBLISH_ORIGIN_FIX, PUBLISH_FIX);
    // The fence's own contract accepts pins, so it gives the pin-aware fix.
    let fence = OriginFence::new(install.clone(), []).unwrap();
    let e = fence.check("https://b.test/x").expect_err("refused");
    assert_origin(&e, &install, PUBLISH_FIX);

    let a = Server::start();
    let b = Server::start();
    let c = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = bundle(dir.path(), &[&a, &b, &c]);
    let (ia, ib, ic) = (origin(&a), origin(&b), origin(&c));
    let pinned = transport(&ia, std::slice::from_ref(&ib));
    for op in every_operation() {
        // On the install's pin, on an origin the fence does not trust, and for an install the
        // fence does not hold: each refusal says only "use the install origin".
        for base in [ib.as_str(), ic.as_str(), "https://evil@a.test"] {
            let call = call_to(&ia, base, &ca, op.clone());
            let e = pinned.exchange(call).expect_err("refused");
            let HubFailure::Local(e) = e else {
                panic!("{op:?} to {base}: {}", kind(&e))
            };
            assert_origin(&e, &ia, INSTALL_FIX);
        }
        let mut call = call_to(&ia, ia.as_str(), &ca, op.clone());
        call.install = ib.clone();
        let e = pinned.exchange(call).expect_err("refused");
        let HubFailure::Local(e) = e else {
            panic!("{op:?}: {}", kind(&e))
        };
        assert_origin(&e, &ia, INSTALL_FIX);
    }
    assert_eq!(a.finish(), 0);
    assert_eq!(b.finish(), 0);
    assert_eq!(c.finish(), 0);
}

#[test]
fn hpb3_upload_streams_exact_length_without_redirects() {
    let mut a = Server::start();
    let b = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = bundle(dir.path(), &[&a, &b]);
    let ia = origin(&a);
    let t = transport(&ia, &[]);
    let bearer = Secret::new(ACCESS);
    let claim = claim();
    let dest = format!("{ia}{OBJECTS}");
    let up = |req: UploadHttpRequest<'_>| t.upload(&ia, Some(ca.as_path()), &claim, &bearer, req);

    // Exact lengths, across and on the 65536-byte buffer, from a reader of uneven reads.
    for (n, step) in [
        (200_000, 7_777),
        (131_072, 65_536),
        (65_537, 100_000),
        (1, 1),
    ] {
        let data = bytes(n);
        let mut reader = Chunky::new(data.clone(), step);
        let (r, req) = served(&mut a, Reply::json(201, OBJECT_REPLY), || {
            up(post(&dest, &mut reader, n as u64))
        });
        assert_eq!(r.expect("exact").0, 201, "{n}");
        assert_eq!(req.body, data, "{n}");
        assert_eq!(req.header("content-length"), Some(n.to_string().as_str()));
        assert_eq!(req.header("transfer-encoding"), None);
        // At most one byte past the end is read, to see the body end.
        assert_eq!(reader.given.get(), n, "{n}");
    }
    // An empty body, given or absent, is `content-length: 0`.
    let mut empty: &[u8] = b"";
    let (r, req) = served(&mut a, Reply::json(200, OBJECT_REPLY), || {
        up(post(&dest, &mut empty, 0))
    });
    assert_eq!(r.expect("empty").0, 200);
    assert_eq!(req.header("content-length"), Some("0"));
    let (r, req) = served(&mut a, Reply::json(200, OBJECT_REPLY), || {
        up(UploadHttpRequest {
            method: UploadMethod::Post,
            destination: &dest,
            content_type: OCTET,
            body: None,
            body_len: 0,
            reply_limit: REPLY_CAP,
        })
    });
    assert_eq!(r.expect("no body").0, 200);
    assert_eq!(req.header("content-length"), Some("0"));
    assert!(req.body.is_empty());

    // A body that ends early, or runs past its length, is refused and never completes a
    // request: the hub reads the declared length and fewer body bytes, then the close.
    let sink = Sink::start();
    let sink_origin = Origin::parse(&sink.origin).unwrap();
    let sink_ca = dir.path().join("sink.pem");
    std::fs::write(&sink_ca, sink.ca.pem()).unwrap();
    let sink_t = transport(&sink_origin, &[]);
    let sink_dest = format!("{sink_origin}{OBJECTS}");
    let sink_up = |req: UploadHttpRequest<'_>| {
        sink_t.upload(&sink_origin, Some(sink_ca.as_path()), &claim, &bearer, req)
    };
    for (len, given) in [
        (200_000u64, 150_000usize),
        (65_536, 65_535),
        (100, 0),
        (100_000, 100_001),
        (65_536, 65_537),
        (131_072, 131_073),
    ] {
        let mut reader = Chunky::new(bytes(given), 9_000);
        let (r, sunk) = sink.aborted(|| sink_up(post(&sink_dest, &mut reader, len)));
        let e = local(r);
        assert_eq!((e.code, e.message.as_str()), (E_IO, BODY_MESSAGE), "{len}");
        assert_eq!(sunk.declared, Some(len), "{len}");
        if given as u64 > len {
            // The fill that reached the end was held back: the hub never had the whole body.
            assert_eq!(sunk.received, (len - 1) / 9_000 * 9_000, "{len}");
            assert_eq!(reader.given.get() as u64, len + 1, "{len}");
        } else {
            assert_eq!(sunk.received, given as u64, "{len}");
        }
    }
    // An endless body is read to its length and one byte more, never further.
    let mut endless = Endless { given: 0 };
    let (r, sunk) = sink.aborted(|| sink_up(post(&sink_dest, &mut endless, 300_000)));
    assert_eq!(local(r).code, E_IO);
    assert_eq!(endless.given, 300_001);
    assert_eq!(
        sunk.received,
        4 * 65_536,
        "four whole buffers, then the held-back end"
    );
    // A body of length 0 that has a byte is refused before any connection.
    let mut one: &[u8] = b"x";
    let e = local(unsent(&a, || up(post(&dest, &mut one, 0))));
    assert_eq!((e.code, e.message.as_str()), (E_IO, BODY_MESSAGE));
    // A failing reader is refused the same way, and its error text is not kept.
    let mut failing = Failing {
        left: 70_000,
        secret: "SENTINEL-DISK",
    };
    let (r, sunk) = sink.aborted(|| sink_up(post(&sink_dest, &mut failing, 100_000)));
    let e = local(r);
    assert_eq!((e.code, e.message.as_str()), (E_IO, BODY_MESSAGE));
    assert_hidden(&format!("{e:?}"), &["SENTINEL-DISK"]);
    assert_eq!((sunk.declared, sunk.received), (Some(100_000), 70_000));

    // The body cap: 2147483648 bytes is admitted (this body then ends early), one more is not.
    assert_eq!(MAX_UPLOAD_BODY_BYTES, 2_147_483_648);
    let mut short: &[u8] = b"0123456789";
    let (r, sunk) = sink.aborted(|| sink_up(post(&sink_dest, &mut short, MAX_UPLOAD_BODY_BYTES)));
    assert_eq!(local(r).code, E_IO);
    assert_eq!(
        (sunk.declared, sunk.received),
        (Some(MAX_UPLOAD_BODY_BYTES), 10)
    );
    let mut short: &[u8] = b"0123456789";
    let e = local(unsent(&a, || {
        up(post(&dest, &mut short, MAX_UPLOAD_BODY_BYTES + 1))
    }));
    assert_eq!(e.code, E_LIMIT);
    // A GET carries no body; a POST without one declares no length.
    let mut five: &[u8] = b"hello";
    let mut with_body = get(&dest);
    with_body.body = Some(&mut five);
    with_body.body_len = 5;
    assert_eq!(local(unsent(&a, || up(with_body))).code, E_IO);
    let mut stray = get(&dest);
    stray.body_len = 5;
    assert_eq!(local(unsent(&a, || up(stray))).code, E_IO);
    let no_body = UploadHttpRequest {
        method: UploadMethod::Post,
        destination: &dest,
        content_type: OCTET,
        body: None,
        body_len: 5,
        reply_limit: REPLY_CAP,
    };
    assert_eq!(local(unsent(&a, || up(no_body))).code, E_IO);
    // The media type is one header value.
    for bad in ["", "a\r\nx-evil: 1", "text/\u{7f}"] {
        let mut five: &[u8] = b"hello";
        let mut req = post(&dest, &mut five, 5);
        req.content_type = bad;
        assert_eq!(local(unsent(&a, || up(req))).code, E_BAD_INPUT, "{bad:?}");
    }

    // A redirect is never followed: one request reaches the install, none the location, and
    // a set cookie is never sent back.
    let elsewhere = format!("{}{OBJECTS}", b.origin());
    for status in [301, 302, 303, 307, 308] {
        let reply = Reply::with_headers(
            status,
            &[
                format!("location: {elsewhere}"),
                "set-cookie: session=SENTINEL-COOKIE".into(),
                "content-length: 0".into(),
                "connection: close".into(),
            ],
            b"",
        );
        let mut body = Chunky::new(bytes(100_000), 50_000);
        let (r, req) = served(&mut a, reply, || up(post(&dest, &mut body, 100_000)));
        assert_eq!(fails(r), "Protocol", "{status}");
        assert_eq!(req.body.len(), 100_000);
        assert_eq!(req.header("cookie"), None);
        let reply = Reply::with_headers(
            status,
            &[
                format!("location: {elsewhere}"),
                "content-length: 0".into(),
                "connection: close".into(),
            ],
            b"",
        );
        let (r, req) = served(&mut a, reply, || up(get(&dest)));
        assert_eq!(fails(r), "Protocol", "GET {status}");
        assert_eq!(req.header("cookie"), None);
    }
    assert_eq!(a.finish(), 0);
    assert_eq!(b.finish(), 0, "the redirect target saw no connection");
    sink.finish();
}

#[test]
fn hpb3_large_upload_replies_do_not_widen_oauth_limits() {
    let mut a = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = bundle(dir.path(), &[&a]);
    let ia = origin(&a);
    let t = transport(&ia, &[]);
    let bearer = Secret::new(ACCESS);
    let claim = claim();
    let dest = format!("{ia}{OBJECTS}");
    let raw = |n: usize| {
        Reply::with_headers(
            200,
            &[format!("content-length: {n}"), "connection: close".into()],
            &vec![b'r'; n],
        )
    };
    let up = |limit: usize| {
        let mut body: &[u8] = b"hello";
        let mut req = post(&dest, &mut body, 5);
        req.reply_limit = limit;
        t.upload(&ia, Some(ca.as_path()), &claim, &bearer, req)
    };

    // The caller's reply cap: both sides of a small cap and of the 16 MiB ceiling.
    assert_eq!(MAX_UPLOAD_REPLY_BYTES, 16_777_216);
    let (r, _) = served(&mut a, raw(10), || up(10));
    assert_eq!(r.expect("at the cap").1.len(), 10);
    let (r, _) = served(&mut a, raw(11), || up(10));
    assert_eq!(fails(r), "Protocol");
    let (r, _) = served(&mut a, raw(MAX_UPLOAD_REPLY_BYTES), || {
        up(MAX_UPLOAD_REPLY_BYTES)
    });
    assert_eq!(r.expect("16 MiB").1.len(), MAX_UPLOAD_REPLY_BYTES);
    let (r, _) = served(&mut a, raw(MAX_UPLOAD_REPLY_BYTES + 1), || {
        up(MAX_UPLOAD_REPLY_BYTES)
    });
    assert_eq!(fails(r), "Protocol");
    // No caller cap is above the ceiling.
    let e = local(unsent(&a, || up(MAX_UPLOAD_REPLY_BYTES + 1)));
    assert_eq!(e.code, E_LIMIT);

    // After the large uploads the OAuth exchange keeps its own 1 MiB reply bound.
    let o = ia.as_str();
    let metadata = METADATA_REPLY.replace("{o}", o);
    let metadata_call = || HubCall {
        install: ia.clone(),
        destination: route(&ia, &HubOperation::Metadata).unwrap().unwrap(),
        ca_file: Some(ca.clone()),
        claim: None,
        bearer: None,
        operation: HubOperation::Metadata,
    };
    let exchange = |a: &mut Server, body: String| {
        let before = a.requests().len();
        a.reply(Reply::json(200, &body));
        a.accept_n(1);
        let r = t.exchange(metadata_call());
        assert_eq!(a.requests().len(), before + 1);
        r
    };
    let padded = |n: usize| format!("{metadata}{}", " ".repeat(n - metadata.len()));
    assert!(exchange(&mut a, padded(MAX_JSON_BYTES)).is_ok(), "1 MiB");
    let r = exchange(&mut a, padded(MAX_JSON_BYTES + 1));
    assert_eq!(kind(&r.expect_err("over 1 MiB")), "Protocol");
    // ...and its depth, member and scalar bounds. The metadata object holds 21 members and
    // elements and is one level deep.
    let with = |extra: String| metadata.replacen('{', &format!("{{\"x\":{extra},"), 1);
    let nest = |levels: usize| format!("{}0{}", "[".repeat(levels), "]".repeat(levels));
    assert!(
        exchange(&mut a, with(nest(MAX_DEPTH - 1))).is_ok(),
        "depth 32"
    );
    let r = exchange(&mut a, with(nest(MAX_DEPTH)));
    assert_eq!(kind(&r.expect_err("depth 33")), "Protocol");
    let list = |n: usize| format!("[{}]", vec!["0"; n].join(","));
    let room = MAX_MEMBERS - 22;
    assert!(exchange(&mut a, with(list(room))).is_ok(), "4096 members");
    let r = exchange(&mut a, with(list(room + 1)));
    assert_eq!(kind(&r.expect_err("4097 members")), "Protocol");
    let text = |n: usize| format!("\"{}\"", "s".repeat(n));
    assert!(exchange(&mut a, with(text(MAX_SCALAR_BYTES))).is_ok());
    let r = exchange(&mut a, with(text(MAX_SCALAR_BYTES + 1)));
    assert_eq!(kind(&r.expect_err("long scalar")), "Protocol");
    assert_eq!(a.finish(), 0);

    // The bounded JSON check takes the caller's limits: both sides of each one.
    let small = JsonLimits {
        bytes: 32,
        depth: 3,
        values: 3,
        scalar_bytes: 4,
    };
    let ok = |body: &str| check_json(body.as_bytes(), small).is_ok();
    assert!(ok(&format!("[1]{}", " ".repeat(29))), "32 bytes");
    assert!(!ok(&format!("[1]{}", " ".repeat(30))), "33 bytes");
    assert!(ok("[[[]]]"), "depth 3");
    assert!(!ok("[[[[]]]]"), "depth 4, 3 values");
    assert!(ok("[1,2,3]"));
    assert!(!ok("[1,2,3,4]"));
    assert!(!ok("[[1],[2]]"), "values count at every level");
    assert!(ok(r#"{"a":1,"b":2,"c":3}"#));
    assert!(!ok(r#"{"a":1,"b":2,"c":3,"d":4}"#));
    assert!(ok(r#"["abcd"]"#));
    assert!(!ok(r#"["abcde"]"#));
    assert!(!ok(r#"{"abcde":1}"#), "a key is a scalar");
    // Duplicate keys, bad UTF-8 and trailing data are refused within any limits.
    let wide = JsonLimits {
        bytes: MAX_UPLOAD_REPLY_BYTES,
        depth: 32,
        values: 1_000_000,
        scalar_bytes: 1_048_576,
    };
    for bad in [
        &br#"{"a":1,"a":2}"#[..],
        &br#"[{"k":{"a":1,"a":1}}]"#[..],
        b"\"\xff\"",
        b"{} {}",
        b"[1] x",
        b"",
    ] {
        assert!(matches!(check_json(bad, wide), Err(HubFailure::Protocol)));
        assert!(matches!(
            check_json(bad, OAUTH_LIMITS),
            Err(HubFailure::Protocol)
        ));
    }
    assert!(check_json(b" {} \n", OAUTH_LIMITS).is_ok());
    // Wide limits admit what the OAuth limits refuse, and the OAuth limits are unchanged.
    let many = list(100_000);
    assert!(check_json(many.as_bytes(), wide).is_ok());
    assert!(matches!(
        check_json(many.as_bytes(), OAUTH_LIMITS),
        Err(HubFailure::Protocol)
    ));
    // Typed decoding checks first, then reads; a wrong type is a bare contract mismatch.
    let v: Vec<u32> = decode_bounded_json(b"[1,2]", small).unwrap();
    assert_eq!(v, [1, 2]);
    assert!(matches!(
        decode_bounded_json::<Vec<u32>>(br#"["x"]"#, small),
        Err(HubFailure::Protocol)
    ));
    assert!(matches!(
        decode_bounded_json::<Vec<u32>>(b"[1,2,3,4]", small),
        Err(HubFailure::Protocol)
    ));
}

#[test]
fn hpb3_transport_failures_hide_tokens_and_raw_bodies() {
    let mut a = Server::start();
    let refused = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = bundle(dir.path(), &[&a, &refused]);
    let ia = origin(&a);
    let t = transport(&ia, &[]);
    let bearer = Secret::new(ACCESS);
    let claim = claim();
    let dest = format!("{ia}{OBJECTS}");
    let secrets = [
        ACCESS,
        "SENTINEL-HEADER",
        "SENTINEL-BODY",
        "SENTINEL-PATH",
        refused.origin(),
    ];
    let check = |f: HubFailure| {
        assert_hidden(&format!("{f:?}"), &secrets);
        let e = f.into_error(&ia);
        assert_hidden(&format!("{e:?}"), &secrets);
        assert_hidden(&e.to_string(), &secrets);
        assert_hidden(&serde_json::to_string(&e).unwrap(), &secrets);
    };
    let up = |req: UploadHttpRequest<'_>| t.upload(&ia, Some(ca.as_path()), &claim, &bearer, req);

    // Replies that carry secrets in headers and bodies fail as bare values.
    let leaky = |status: u16, extra: &[String], body: &str| {
        let mut headers = vec![
            "x-leak: SENTINEL-HEADER".to_string(),
            format!("www-authenticate: Bearer {ACCESS}"),
            "connection: close".into(),
        ];
        headers.extend_from_slice(extra);
        Reply::with_headers(status, &headers, body.as_bytes())
    };
    let body = format!("SENTINEL-BODY {ACCESS}");
    let length = format!("content-length: {}", body.len());
    // over the caller's reply cap
    let (r, _) = served(
        &mut a,
        leaky(200, std::slice::from_ref(&length), &body),
        || {
            let mut req = get(&dest);
            req.reply_limit = 4;
            up(req)
        },
    );
    check(r.expect_err("over the cap"));
    // a redirect to the secret
    let location = format!("location: {}/SENTINEL-PATH?t={ACCESS}", refused.origin());
    let (r, _) = served(
        &mut a,
        leaky(307, &[location, length.clone()], &body),
        || up(get(&dest)),
    );
    check(r.expect_err("redirect"));
    // too many header fields
    let many: Vec<String> = (0..MAX_HEADERS)
        .map(|i| format!("x-sentinel-{i}: SENTINEL-HEADER"))
        .chain([length.clone()])
        .collect();
    let (r, _) = served(&mut a, leaky(200, &many, &body), || up(get(&dest)));
    check(r.expect_err("headers"));
    // a body cut short after the request was sent
    let cut = "content-length: 100000".to_string();
    let (r, _) = served(&mut a, leaky(200, &[cut], &body), || up(get(&dest)));
    check(r.expect_err("cut"));
    // a lost reply after a whole body was sent
    let mut five: &[u8] = b"hello";
    let (r, _) = served(&mut a, Reply::Close, || up(post(&dest, &mut five, 5)));
    let f = r.expect_err("lost");
    assert_eq!(kind(&f), "Transport(true)");
    check(f);
    // a reply that is no HTTP
    let (r, _) = served(
        &mut a,
        Reply::Raw(format!("SENTINEL-BODY {ACCESS}\r\n\r\n").into_bytes()),
        || up(get(&dest)),
    );
    check(r.expect_err("not HTTP"));

    // Refusals before sending never quote the destination or a token.
    let to_refused = format!("{}/SENTINEL-PATH?access_token={ACCESS}", refused.origin());
    let r = unsent(&refused, || up(get(&to_refused)));
    check(r.expect_err("fence"));
    for bad in [
        format!("{ACCESS}x"),
        "rdyh_a_SENTINEL-HEADER".to_string(),
        String::new(),
    ] {
        let wrong = Secret::new(bad.clone());
        let r = unsent(&a, || {
            t.upload(&ia, Some(ca.as_path()), &claim, &wrong, get(&dest))
        });
        let f = r.expect_err("token");
        assert_eq!(kind(&f), format!("Local({E_UPSTREAM_TOKEN})"));
        check(f);
        if !bad.is_empty() {
            assert_hidden(&format!("{wrong:?}"), &[&bad]);
        }
    }
    // A missing CA trusts no fixture: a handshake failure, never a request.
    let r = unread(&mut a, || t.upload(&ia, None, &claim, &bearer, get(&dest)));
    let f = r.expect_err("tls");
    assert_eq!(kind(&f), "Transport(false)");
    check(f);
    // JSON checks keep no parser text.
    for bad in [
        format!(r#"{{"SENTINEL-BODY":"{ACCESS}","SENTINEL-BODY":1}}"#),
        format!(r#"["SENTINEL-BODY {ACCESS}"] SENTINEL-BODY"#),
    ] {
        check(check_json(bad.as_bytes(), OAUTH_LIMITS).expect_err("bad json"));
    }
    let wrong = format!(r#"{{"SENTINEL-BODY":"{ACCESS}"}}"#);
    check(decode_bounded_json::<Vec<u8>>(wrong.as_bytes(), OAUTH_LIMITS).expect_err("type"));
    // The transport's Debug holds no secret.
    assert_hidden(&format!("{t:?}"), &[ACCESS]);

    assert_eq!(a.finish(), 0);
    assert_eq!(refused.finish(), 0);
}
