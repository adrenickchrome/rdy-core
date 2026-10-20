//! hpb7: the typed publication upload protocol (D-Q0AVZ; hub D-6BXJY, i-5ZHDF, i-DETA1, i-SG442,
//! i-QD955, i-4TMNM, i-PXH0J, i-F19YZ, r-QNT1T). Fixture revision: the hub's landed D-6BXJY text
//! with its step interfaces and the error object of tb-68J50; no hub endpoint runs. Every test drives the real HTTPS wire against loopback TLS fixtures with
//! generated CAs. Request counts come from the fixtures' logs and accept counts.
#[path = "support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish_wire::*;
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::publication::{Audience, SelectionEntry};
use rdy_engine::publication_manifest::{Manifest, ManifestEntry, manifest_hash};
use rdy_engine::render_inputs::{RenderInputs, capture};
use rdy_engine::{
    E_ACTOR_UNBOUND, E_BAD_INPUT, E_LIMIT, E_UNRESOLVED_REF, E_UPSTREAM_ORIGIN,
    E_UPSTREAM_PROTOCOL, E_UPSTREAM_REFUSED, E_UPSTREAM_TRANSPORT, E_UPSTREAM_UNCERTAIN, Rdy,
    RdyError, Session, SessionKind, UPSTREAM_PROTOCOL_FIX, UPSTREAM_PROTOCOL_MESSAGE,
    UPSTREAM_REFUSED_FIX, UPSTREAM_REFUSED_MESSAGE, core_error_code,
};
use rdy_model::{BlockId, Hash, RepoId, Role};
use rdy_store::object_refs::ObjectKey;
use serde_json::{Value, json};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The credential fixture's tokens: an access and a refresh token with `rdyh_`, and a token with
/// `rdyx_` (hub_http.rs's second refresh token).
const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const REFRESH_X: &str = "rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ";

const BASE: &str = "/sync/v1/acme/designs";
const REMOTE: &str = "/app/acme/designs";
const PUSH: &str = "0123456789abcdefghjkmnpqrs";
const SITE: &str = "docs";
const SYNC_JSON: &str = "application/vnd.rdy.sync+json";

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

fn claim(run: Option<&str>) -> RequestClaim {
    RequestClaim::from_session(&Session {
        actor: "agent:claude".into(),
        run: run.map(str::to_string),
        role: Role::Writer,
        kind: SessionKind::Agent,
    })
    .unwrap()
}

/// One wire to the install `servers[0]`, with the other servers' origins pinned, and a CA bundle
/// trusting every server in `trusted`.
struct Hub {
    install: Origin,
    ca: PathBuf,
    wire: HttpPublishWire,
    _dir: tempfile::TempDir,
}

impl Hub {
    fn new(install: &Server, pinned: &[&Server], trusted: &[&Server]) -> Hub {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        let pem: String = std::iter::once(install)
            .chain(trusted.iter().copied())
            .map(|s| s.ca_pem())
            .collect();
        std::fs::write(&ca, pem).unwrap();
        let origin = Origin::parse(install.origin()).unwrap();
        let pins: Vec<Origin> = pinned
            .iter()
            .map(|s| Origin::parse(s.origin()).unwrap())
            .collect();
        let fence = OriginFence::new(origin.clone(), pins).unwrap();
        Hub {
            install: origin,
            ca,
            wire: HttpPublishWire::new(HttpHubTransport::new(fence).unwrap()),
            _dir: dir,
        }
    }

    fn cx_with(&self, base: Option<String>, run: Option<&str>) -> UploadContext {
        UploadContext {
            install: self.install.clone(),
            sync_base: base,
            ca_file: Some(self.ca.clone()),
            claim: claim(run),
            bearer: Secret::new(ACCESS),
        }
    }

    fn cx(&self) -> UploadContext {
        self.cx_with(Some(format!("{}{BASE}", self.install)), Some("r7"))
    }
}

/// One call to `server` with `reply` queued: its result and the one request the hub read.
fn served<T: std::fmt::Debug>(
    server: &mut Server,
    reply: Reply,
    call: impl FnOnce() -> T,
) -> (T, Recorded) {
    let before = server.requests().len();
    server.reply(reply);
    server.accept_n(1);
    let r = call();
    let log = server.requests();
    assert_eq!(log.len(), before + 1, "one request per call: {r:?}");
    (r, log.last().unwrap().clone())
}

/// A call that must not connect at all.
fn unsent<T: std::fmt::Debug>(servers: &[&Server], call: impl FnOnce() -> T) -> T {
    let before: Vec<usize> = servers.iter().map(|s| s.accepted()).collect();
    let r = call();
    let after: Vec<usize> = servers.iter().map(|s| s.accepted()).collect();
    assert_eq!(before, after, "no connection: {r:?}");
    r
}

#[track_caller]
fn err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Err(e) => e,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

#[track_caller]
fn protocol<T: std::fmt::Debug>(r: Result<T, RdyError>) {
    let e = err(r);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_PROTOCOL,
            UPSTREAM_PROTOCOL_MESSAGE,
            UPSTREAM_PROTOCOL_FIX
        )
    );
    assert!(e.issues.is_empty(), "{e:?}");
}

fn h(n: u8) -> Hash {
    Hash::of(&[n])
}

fn block(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A hub error object; `issues` is the JSON text of the list, `extra` more members.
fn error_body(code: &str, message: &str, fix: &str, issues: &str, extra: &str) -> String {
    format!(
        r#"{{"code":{},"message":{},"fix":{},"request_id":"req_0001","issues":{issues}{extra}}}"#,
        json!(code),
        json!(message),
        json!(fix)
    )
}

fn state_reply(features: &str, profile: &str) -> String {
    format!(
        r#"{{"canon":null,"repo_id":null,"rdy":"0.10.0","stream_read":[1],"stream_write":[1],"features":{features},"min_rdy":null,"batch_bytes":67108864,"batch_records":100000,"quarantine_seconds":86400,"profile":{profile},"audience":null,"artifact_bytes":201326592,"artifact_files":32000}}"#
    )
}

fn state_request() -> StateRequest {
    StateRequest::new(&claim(Some("r7")), SITE)
}

/// The render inputs of a fresh rdy-standard project (captured once).
fn inputs() -> RenderInputs {
    static INPUTS: OnceLock<RenderInputs> = OnceLock::new();
    INPUTS
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("p");
            std::fs::create_dir(&root).unwrap();
            let r = Rdy::init(&root, "rdy-standard").unwrap();
            capture(&r.read()).unwrap()
        })
        .clone()
}

fn manifest() -> Manifest {
    Manifest {
        files: vec![
            ManifestEntry {
                path: "index.html".into(),
                hash: h(1),
                media_type: "text/html".into(),
                size: 100,
                notice_offset: Some(10),
            },
            ManifestEntry {
                path: "logo.png".into(),
                hash: h(2),
                media_type: "image/png".into(),
                size: 7,
                notice_offset: None,
            },
        ],
    }
}

fn record(profile: Option<ProfileIdentity>, m: &Manifest) -> PublicationInputs {
    PublicationInputs {
        repo_id: RepoId([0xab; 16]),
        tag: "v1.2.0".into(),
        landing: h(3),
        profile,
        selection: vec![
            SelectionEntry {
                id: block("D-0A000"),
                version: h(4),
                redaction: None,
            },
            SelectionEntry {
                id: block("D-0B000"),
                version: h(5),
                redaction: Some(h(6)),
            },
        ],
        selection_hash: h(7),
        manifest_recipe: 1,
        manifest_hash: manifest_hash(m, 1).unwrap(),
        render_inputs: inputs(),
    }
}

fn profile(version: Hash) -> ProfileIdentity {
    ProfileIdentity {
        id: block("PUB-0A1B2"),
        version,
    }
}

const CANDIDATE: &str = r#"{"id":"pub_00000000000000000000000001","state":"candidate","url":"https://hub.invalid/app/acme/designs/publications/pub_1"}"#;

fn key(kind: u8, n: u8) -> ObjectKey {
    ObjectKey { kind, hash: h(n) }
}

/// `keys` sorted by kind and hash, as the known step needs them.
fn sorted(mut keys: Vec<ObjectKey>) -> Vec<ObjectKey> {
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------------------------
// Pinned tests
// ---------------------------------------------------------------------------------------------

/// Discovery sends one GET to the configured remote; a remote or sync base outside the fence
/// reaches no request, a redirect is never followed, and every malformed sync base is refused.
#[test]
fn hpb7_discovery_follows_only_configured_origins() {
    let mut a = Server::start();
    let mut pinned = Server::start();
    let other = Server::start();
    let hub = Hub::new(&a, &[&pinned], &[&pinned, &other]);
    let cx = hub.cx_with(None, Some("r7"));
    let remote = format!("{}{REMOTE}", a.origin());

    // A remote on an unpinned origin: refused before any connection, with the pin-aware fix.
    let e = err(unsent(&[&a, &other], || {
        hub.wire
            .discovery(&cx, &format!("{}{REMOTE}", other.origin()))
    }));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    assert!(!e.message.contains(other.origin()), "{e:?}");

    // A path on the install: one GET with the sync media type and no body.
    let reply = format!(r#"{{"sync_base":"{BASE}","protocols":[1]}}"#);
    let (r, req) = served(&mut a, Reply::json(200, &reply), || {
        hub.wire.discovery(&cx, &remote)
    });
    let d = r.unwrap();
    assert_eq!(d.sync_base, format!("{}{BASE}", a.origin()));
    assert_eq!(d.protocols, vec![1]);
    assert_eq!((req.method.as_str(), req.target.as_str()), ("GET", REMOTE));
    assert_eq!(req.header("accept"), Some(SYNC_JSON));
    assert_eq!(req.header("content-type"), None);
    assert_eq!(
        req.header("authorization"),
        Some(&*format!("Bearer {ACCESS}"))
    );
    assert_eq!(req.header("rdy-actor"), Some("agent:claude"));
    assert_eq!(req.header("rdy-run"), Some("r7"));
    assert!(req.body.is_empty());

    // An absolute sync base on a pinned origin is admitted, and the next step goes there.
    let at_pin = format!("{}/sync/v2", pinned.origin());
    let (r, _) = served(
        &mut a,
        Reply::json(
            200,
            &format!(r#"{{"sync_base":"{at_pin}","protocols":[1,2]}}"#),
        ),
        || hub.wire.discovery(&cx, &remote),
    );
    let d = r.unwrap();
    assert_eq!(d.sync_base, at_pin);
    let state_cx = hub.cx_with(Some(d.sync_base), Some("r7"));
    let (r, req) = served(
        &mut pinned,
        Reply::json(200, &state_reply("[]", "null")),
        || hub.wire.state(&state_cx, &state_request()),
    );
    r.unwrap();
    assert_eq!(req.target, "/sync/v2/state");

    // A sync base on an unpinned origin, merely listed by the reply: refused, and that origin
    // never sees a request.
    let (r, _) = served(
        &mut a,
        Reply::json(
            200,
            &format!(
                r#"{{"sync_base":"{}/sync","protocols":[1]}}"#,
                other.origin()
            ),
        ),
        || hub.wire.discovery(&cx, &remote),
    );
    assert_eq!(err(r).code, E_UPSTREAM_ORIGIN);
    // The context's own sync base gets the same fence before any byte is sent.
    let wrong = hub.cx_with(Some(format!("{}/sync", other.origin())), Some("r7"));
    let e = err(unsent(&[&a, &other], || {
        hub.wire.state(&wrong, &state_request())
    }));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);

    // A redirect is never followed.
    let location = format!("location: {}/sync", other.origin());
    let (r, _) = served(
        &mut a,
        Reply::with_headers(302, &[location, "content-length: 0".into()], b""),
        || hub.wire.discovery(&cx, &remote),
    );
    protocol(r);

    // Malformed sync bases.
    let bad = [
        format!("//{}/sync", &a.origin()["https://".len()..]),
        "sync/v1".to_string(),
        "/sync/../x".to_string(),
        "/sync/./x".to_string(),
        "/sync/%2e%2e/x".to_string(),
        "/sync/%2E/x".to_string(),
        "/sync/.%2E/x".to_string(),
        "/sync?x=1".to_string(),
        "/sync#frag".to_string(),
        "/sync\\\\x".to_string(),
        format!("https://user@{}/sync", &a.origin()["https://".len()..]),
        format!("{}/sync?x=1", a.origin()),
        format!("{}/sync/%2e%2e", a.origin()),
        format!("http://{}/sync", &a.origin()["https://".len()..]),
        String::new(),
    ];
    for base in &bad {
        let reply = format!(r#"{{"sync_base":{},"protocols":[1]}}"#, json!(base));
        let (r, _) = served(&mut a, Reply::json(200, &reply), || {
            hub.wire.discovery(&cx, &remote)
        });
        assert!(r.is_err(), "{base}: {r:?}");
        let e = r.unwrap_err();
        assert!(
            e.code == E_UPSTREAM_PROTOCOL || e.code == E_UPSTREAM_ORIGIN,
            "{base}: {e:?}"
        );
    }
    // A reply that does not speak protocol 1, or is not the exact object.
    for reply in [
        format!(r#"{{"sync_base":"{BASE}","protocols":[2]}}"#),
        format!(r#"{{"sync_base":"{BASE}","protocols":[]}}"#),
        format!(r#"{{"sync_base":"{BASE}","protocols":[1,1]}}"#),
        format!(r#"{{"sync_base":"{BASE}"}}"#),
        format!(r#"{{"sync_base":"{BASE}","protocols":[1],"origins":["x"]}}"#),
        format!(r#"{{"sync_base":"{BASE}","protocols":[1],"protocols":[1]}}"#),
    ] {
        let (r, _) = served(&mut a, Reply::json(200, &reply), || {
            hub.wire.discovery(&cx, &remote)
        });
        protocol(r);
    }
    assert_eq!(other.finish(), 0);
    assert_eq!(pinned.finish(), 0);
    assert_eq!(a.finish(), 0);
}

/// Every endpoint's exact method, path, headers and JSON, with the nullable fields present.
#[test]
fn hpb7_wire_fixtures_match_every_endpoint() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let auth = format!("Bearer {ACCESS}");
    let rdy = rdy_engine::formats::formats().rdy;

    // state: the claim's actor and run, a nonempty compatible feature list, {id,version}.
    let features = r#"[{"name":"blob_chunks","class":"compatible"},{"name":"order_keys","class":"read_only"}]"#;
    let version = h(9);
    let prof = format!(r#"{{"id":"PUB-0A1B2","version":"{version}"}}"#);
    let (r, req) = served(
        &mut s,
        Reply::json(200, &state_reply(features, &prof)),
        || hub.wire.state(&cx, &state_request()),
    );
    let st = r.unwrap();
    assert_eq!(
        (req.method.as_str(), req.target.as_str()),
        ("POST", "/sync/v1/acme/designs/state")
    );
    assert_eq!(req.header("content-type"), Some("application/json"));
    assert_eq!(req.header("accept"), Some("application/json"));
    assert_eq!(req.header("authorization"), Some(auth.as_str()));
    assert_eq!(req.header("rdy-actor"), Some("agent:claude"));
    assert_eq!(req.header("rdy-run"), Some("r7"));
    assert_eq!(
        req.body_text(),
        format!(
            r#"{{"rdy":"{rdy}","stream_read":[1],"stream_write":[1],"actor":"agent:claude","run":"r7","site":"docs"}}"#
        )
    );
    assert_eq!(
        st,
        PublishState {
            canon: None,
            repo_id: None,
            rdy: "0.10.0".into(),
            stream_read: vec![1],
            stream_write: vec![1],
            features: vec![
                FeatureUse {
                    name: "blob_chunks".into(),
                    class: FeatureClass::Compatible
                },
                FeatureUse {
                    name: "order_keys".into(),
                    class: FeatureClass::ReadOnly
                },
            ],
            min_rdy: None,
            batch_bytes: 67_108_864,
            batch_records: 100_000,
            quarantine_seconds: 86_400,
            profile: Some(profile(version)),
            audience: None,
            artifact_bytes: 201_326_592,
            artifact_files: 32_000,
        }
    );

    // A session without a run: JSON null and no Rdy-Run header.
    let no_run = hub.cx_with(cx.sync_base.clone(), None);
    let request = StateRequest::new(&claim(None), SITE);
    let (r, req) = served(&mut s, Reply::json(200, &state_reply("[]", "null")), || {
        hub.wire.state(&no_run, &request)
    });
    r.unwrap();
    assert_eq!(req.header("rdy-run"), None);
    assert!(
        req.body_text().contains(r#""run":null"#),
        "{}",
        req.body_text()
    );
    // A request whose actor or run is not the context's claim is refused before sending.
    let mut other = state_request();
    other.run = Some("r8".into());
    assert_eq!(
        err(unsent(&[&s], || hub.wire.state(&cx, &other))).code,
        E_ACTOR_UNBOUND
    );
    let mut other = state_request();
    other.actor = "agent:someone".into();
    assert_eq!(
        err(unsent(&[&s], || hub.wire.state(&cx, &other))).code,
        E_ACTOR_UNBOUND
    );

    // Set fields, audience and lowercase repo id; larger caps are clamped, smaller kept.
    let full = format!(
        r#"{{"canon":"{}","repo_id":"{}","rdy":"0.11.0-rc.1","stream_read":[1,2],"stream_write":[1],"features":[],"min_rdy":"0.9.0","batch_bytes":9999999999,"batch_records":2000000,"quarantine_seconds":99999999,"profile":null,"audience":"public","artifact_bytes":9999999999,"artifact_files":64000}}"#,
        h(1),
        "ab".repeat(16)
    );
    let (r, _) = served(&mut s, Reply::json(200, &full), || {
        hub.wire.state(&cx, &state_request())
    });
    let st = r.unwrap();
    assert_eq!(st.canon, Some(h(1)));
    assert_eq!(st.repo_id, Some(RepoId([0xab; 16])));
    assert_eq!(st.min_rdy.as_deref(), Some("0.9.0"));
    assert_eq!(st.audience, Some(Audience::Public));
    assert_eq!(st.profile, None);
    assert_eq!(
        (
            st.batch_bytes,
            st.batch_records,
            st.quarantine_seconds,
            st.artifact_bytes,
            st.artifact_files
        ),
        (2_147_483_648, 1_000_000, 31_536_000, 2_147_483_648, 32_000)
    );

    // Each nullable member must be present; wrong types, zero caps, upper-case hex, unknown and
    // duplicate members are refused.
    let base = state_reply("[]", "null");
    let bad_states = [
        base.replace(r#""canon":null,"#, ""),
        base.replace(r#","min_rdy":null"#, ""),
        base.replace(r#","profile":null"#, ""),
        base.replace(r#","audience":null"#, ""),
        base.replace(r#""quarantine_seconds":86400"#, r#""quarantine_hours":24"#),
        base.replace(r#""batch_bytes":67108864"#, r#""batch_bytes":0"#),
        base.replace(r#""artifact_files":32000"#, r#""artifact_files":-1"#),
        base.replace(r#""batch_records":100000"#, r#""batch_records":"100000""#),
        base.replace(r#""audience":null"#, r#""audience":"everyone""#),
        base.replace(
            r#""canon":null"#,
            &format!(r#""canon":"{}""#, h(1).to_string().to_uppercase()),
        ),
        base.replace(
            r#""repo_id":null"#,
            &format!(r#""repo_id":"{}""#, "AB".repeat(16)),
        ),
        base.replace(r#""rdy":"0.10.0""#, r#""rdy":"ten""#),
        base.replace(r#""stream_read":[1]"#, r#""stream_read":[2]"#),
        base.replace(
            r#""profile":null"#,
            r#""profile":{"id":"PUB-0A1B2","version":"x"}"#,
        ),
        base.replace(
            r#""profile":null"#,
            &format!(
                r#""profile":{{"id":"PUB-0A1B2","version":"{}","audience":"org"}}"#,
                h(1)
            ),
        ),
        base.replace('}', r#","extra":1}"#),
        base.replace(r#""canon":null"#, r#""canon":null,"canon":null"#),
    ];
    for body in &bad_states {
        let (r, _) = served(&mut s, Reply::json(200, body), || {
            hub.wire.state(&cx, &state_request())
        });
        protocol(r);
    }

    // known: {push, keys:[{kind,hash}]} -> {missing:[...]}.
    let keys = sorted(vec![key(1, 1), key(1, 2), key(3, 3)]);
    let reply = format!(r#"{{"missing":[{{"kind":1,"hash":"{}"}}]}}"#, keys[0].hash);
    let (r, req) = served(&mut s, Reply::json(200, &reply), || {
        hub.wire.known(&cx, PUSH, &keys)
    });
    assert_eq!(r.unwrap(), vec![keys[0]]);
    assert_eq!(req.target, "/sync/v1/acme/designs/known");
    assert_eq!(req.header("content-type"), Some("application/json"));
    let keys_json: Vec<String> = keys
        .iter()
        .map(|k| format!(r#"{{"kind":{},"hash":"{}"}}"#, k.kind, k.hash))
        .collect();
    assert_eq!(
        req.body_text(),
        format!(r#"{{"push":"{PUSH}","keys":[{}]}}"#, keys_json.join(","))
    );

    // objects: raw stream bytes, the push in the query only.
    let mut batch = Cursor::new(b"batch-bytes".to_vec());
    let (r, req) = served(
        &mut s,
        Reply::json(201, r#"{"records":3,"bytes":11}"#),
        || hub.wire.put_objects(&cx, PUSH, &mut batch, 11),
    );
    assert_eq!(
        r.unwrap(),
        ObjectPut {
            records: 3,
            bytes: 11
        }
    );
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.target,
        format!("/sync/v1/acme/designs/objects/put?push={PUSH}")
    );
    assert_eq!(req.header("content-type"), Some("application/octet-stream"));
    assert_eq!(req.header("content-length"), Some("11"));
    assert_eq!(req.body, b"batch-bytes");
    // A 200 is a success too; anything but the exact object is not.
    let mut batch = Cursor::new(b"x".to_vec());
    let (r, _) = served(
        &mut s,
        Reply::json(200, r#"{"records":1,"bytes":1}"#),
        || hub.wire.put_objects(&cx, PUSH, &mut batch, 1),
    );
    r.unwrap();

    // files/known: {push, files:[{hash,size}]} -> {missing:[hash]}.
    let files = vec![
        FileIdentity {
            hash: h(2),
            size: 7,
        },
        FileIdentity {
            hash: h(1),
            size: 100,
        },
    ];
    let mut want = vec![h(1), h(2)];
    want.sort();
    let reply = format!(r#"{{"missing":["{}","{}"]}}"#, want[0], want[1]);
    let (r, req) = served(&mut s, Reply::json(200, &reply), || {
        hub.wire.known_files(&cx, PUSH, &files)
    });
    assert_eq!(r.unwrap(), want);
    assert_eq!(req.target, "/sync/v1/acme/designs/files/known");
    assert_eq!(
        req.body_text(),
        format!(
            r#"{{"push":"{PUSH}","files":[{{"hash":"{}","size":7}},{{"hash":"{}","size":100}}]}}"#,
            h(2),
            h(1)
        )
    );

    // files/put: the manifest media type and the raw bytes; the reply names the same file.
    let file = FileIdentity {
        hash: h(2),
        size: 4,
    };
    let mut bytes = Cursor::new(b"\x89PNG".to_vec());
    let reply = format!(r#"{{"hash":"{}","size":4}}"#, h(2));
    let (r, req) = served(&mut s, Reply::json(201, &reply), || {
        hub.wire.put_file(&cx, PUSH, &file, "image/png", &mut bytes)
    });
    assert_eq!(r.unwrap(), file);
    assert_eq!(
        req.target,
        format!("/sync/v1/acme/designs/files/put?push={PUSH}&hash={}", h(2))
    );
    assert_eq!(req.header("content-type"), Some("image/png"));
    assert_eq!(req.body, b"\x89PNG");

    // publish: keep_latest beside the record when given, absent when not; never inside it.
    let m = manifest();
    let rec = record(Some(profile(h(8))), &m);
    for keep in [Some(true), Some(false), None] {
        let (r, req) = served(&mut s, Reply::json(201, CANDIDATE), || {
            hub.wire.publish(&cx, PUSH, SITE, &rec, &m, keep)
        });
        assert_eq!(
            r.unwrap(),
            Candidate {
                id: "pub_00000000000000000000000001".into(),
                state: "candidate".into(),
                url: "https://hub.invalid/app/acme/designs/publications/pub_1".into(),
            }
        );
        assert_eq!(req.target, "/sync/v1/acme/designs/publish");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let mut expected = json!({
            "push": PUSH,
            "site": SITE,
            "record": {
                "repo_id": "ab".repeat(16),
                "tag": "v1.2.0",
                "landing": h(3).to_string(),
                "profile": {"id": "PUB-0A1B2", "version": h(8).to_string()},
                "selection": [
                    {"id": "D-0A000", "version": h(4).to_string(), "redaction": null},
                    {"id": "D-0B000", "version": h(5).to_string(), "redaction": h(6).to_string()},
                ],
                "selection_hash": h(7).to_string(),
                "manifest_recipe": 1,
                "manifest_hash": manifest_hash(&m, 1).unwrap().to_string(),
                "render_inputs": serde_json::to_value(inputs()).unwrap(),
            },
            "manifest": {"files": [
                {"path": "index.html", "hash": h(1).to_string(), "media_type": "text/html", "size": 100, "notice_offset": 10},
                {"path": "logo.png", "hash": h(2).to_string(), "media_type": "image/png", "size": 7, "notice_offset": null},
            ]},
        });
        if let Some(k) = keep {
            expected["keep_latest"] = json!(k);
        }
        assert_eq!(body, expected, "keep_latest {keep:?}");
        let text = req.body_text();
        match keep {
            Some(k) => assert!(
                text.ends_with(&format!(r#"}}]}},"keep_latest":{k}}}"#)),
                "{keep:?}"
            ),
            None => assert!(!text.contains("keep_latest"), "absent, never null"),
        }
        assert!(body["record"].get("keep_latest").is_none());
        assert!(body["record"].get("audience").is_none());
        assert!(body["record"].get("selection_identity").is_none());
    }
    // The implicit profile is null in the record.
    let rec = record(None, &m);
    let (r, req) = served(&mut s, Reply::json(200, CANDIDATE), || {
        hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None)
    });
    r.unwrap();
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["record"]["profile"], Value::Null);

    // The required issues member of an error object; a missing one is no hub refusal.
    let missing =
        r#"{"code":"E_HUB_BUSY","message":"m","fix":"f","request_id":"req_1","retry_after":1}"#;
    let (r, _) = served(&mut s, Reply::json(503, missing), || {
        hub.wire.state(&cx, &state_request())
    });
    protocol(r);
    let present = error_body("E_HUB_BUSY", "busy", "wait", "[]", r#","retry_after":5"#);
    let (r, _) = served(&mut s, Reply::json(503, &present), || {
        hub.wire.state(&cx, &state_request())
    });
    let e = err(r);
    assert_eq!(e.code, "E_HUB_BUSY");
    assert_eq!(e.issues.len(), 2, "request id and retry_after: {e:?}");
    assert_eq!(e.issues[1].message, "retry after 5 seconds");

    // Debug never prints the bearer.
    let text = format!("{cx:?}");
    assert!(!text.contains(ACCESS), "{text}");
    assert_eq!(s.finish(), 0);
}

/// The state reply's profile version is hub metadata: the record keeps the tag's revision.
#[test]
fn hpb7_profile_version_never_overrides_the_tag() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let hub_version = h(41);
    let tag_version = h(42);
    let prof = format!(r#"{{"id":"PUB-0A1B2","version":"{hub_version}"}}"#);
    let (r, _) = served(&mut s, Reply::json(200, &state_reply("[]", &prof)), || {
        hub.wire.state(&cx, &state_request())
    });
    let st = r.unwrap();
    assert_eq!(st.profile, Some(profile(hub_version)));
    let m = manifest();
    let rec = record(Some(profile(tag_version)), &m);
    let (r, req) = served(&mut s, Reply::json(201, CANDIDATE), || {
        hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None)
    });
    r.unwrap();
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(
        body["record"]["profile"],
        json!({"id": "PUB-0A1B2", "version": tag_version.to_string()})
    );
    assert!(!req.body_text().contains(&hub_version.to_string()));
    assert_eq!(s.finish(), 0);
}

/// 100,000 object keys and a 32,000-file manifest at the longest path fit every request and
/// reply bound; one more is refused before any connection.
#[test]
fn hpb7_large_key_and_manifest_shapes_fit_their_bounds() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();

    // 100,000 keys: the request fits 16 MiB, and a reply naming every key decodes.
    let keys: Vec<ObjectKey> = (0..MAX_KNOWN_KEYS as u32)
        .map(|i| ObjectKey {
            kind: 1 + (i % 3) as u8,
            hash: Hash::of(&i.to_le_bytes()),
        })
        .collect();
    let keys = sorted(keys);
    let all: Vec<String> = keys
        .iter()
        .map(|k| format!(r#"{{"kind":{},"hash":"{}"}}"#, k.kind, k.hash))
        .collect();
    let reply = format!(r#"{{"missing":[{}]}}"#, all.join(","));
    assert!(reply.len() <= KNOWN_REPLY_BYTES);
    let (r, req) = served(&mut s, Reply::json(200, &reply), || {
        hub.wire.known(&cx, PUSH, &keys)
    });
    assert_eq!(r.unwrap(), keys);
    assert!(req.body.len() <= KNOWN_REQUEST_BYTES);
    let mut more = keys.clone();
    more.push(ObjectKey {
        kind: 200,
        hash: h(0),
    });
    assert_eq!(
        err(unsent(&[&s], || hub.wire.known(&cx, PUSH, &more))).code,
        E_LIMIT
    );
    assert_eq!(
        err(unsent(&[&s], || hub.wire.known(&cx, PUSH, &[]))).code,
        E_BAD_INPUT
    );
    let mut unsorted = keys[..2].to_vec();
    unsorted.reverse();
    assert_eq!(
        err(unsent(&[&s], || hub.wire.known(&cx, PUSH, &unsorted))).code,
        E_BAD_INPUT
    );

    // 32,000 files: the files/known request fits, the reply naming each hash decodes.
    let files: Vec<FileIdentity> = (0..MAX_KNOWN_FILES as u32)
        .map(|i| FileIdentity {
            hash: Hash::of(&i.to_be_bytes()),
            size: u64::from(i) + 1,
        })
        .collect();
    let mut hashes: Vec<Hash> = files.iter().map(|f| f.hash).collect();
    hashes.sort();
    let all: Vec<String> = hashes.iter().map(|h| format!("\"{h}\"")).collect();
    let reply = format!(r#"{{"missing":[{}]}}"#, all.join(","));
    assert!(reply.len() <= FILES_KNOWN_REPLY_BYTES);
    let (r, req) = served(&mut s, Reply::json(200, &reply), || {
        hub.wire.known_files(&cx, PUSH, &files)
    });
    assert_eq!(r.unwrap(), hashes);
    assert!(req.body.len() <= FILES_KNOWN_REQUEST_BYTES);
    let mut more = files.clone();
    more.push(FileIdentity {
        hash: h(0),
        size: 1,
    });
    assert_eq!(
        err(unsent(&[&s], || hub.wire.known_files(&cx, PUSH, &more))).code,
        E_LIMIT
    );
    let twice = vec![files[0], files[0]];
    assert_eq!(
        err(unsent(&[&s], || hub.wire.known_files(&cx, PUSH, &twice))).code,
        E_BAD_INPUT
    );

    // The publish body with a full manifest at the longest path and 100,000 selection rows
    // fits its 268,435,456-byte cap.
    let filler = "f".repeat(255);
    let entries: Vec<ManifestEntry> = (0..32_000u32)
        .map(|i| {
            let mut path = format!("{i:05}");
            while path.len() + 256 <= 4096 {
                path.push('/');
                path.push_str(&filler);
            }
            let pad = 4096 - path.len() - 1;
            path.push('/');
            path.push_str(&"p".repeat(pad));
            assert_eq!(path.len(), 4096);
            ManifestEntry {
                path,
                hash: Hash::of(&i.to_le_bytes()),
                media_type: "text/html".into(),
                size: 64,
                notice_offset: Some(0),
            }
        })
        .collect();
    let big = Manifest { files: entries };
    const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let selection: Vec<SelectionEntry> = (0..MAX_SELECTION_ROWS as u32)
        .map(|i| {
            let body: String = (0..5)
                .rev()
                .map(|d| DIGITS[((i >> (5 * d)) & 31) as usize] as char)
                .collect();
            SelectionEntry {
                id: block(&format!("D-{body}")),
                version: Hash::of(&i.to_le_bytes()),
                redaction: Some(Hash::of(&i.to_be_bytes())),
            }
        })
        .collect();
    let mut rec = record(Some(profile(h(8))), &big);
    rec.selection = selection;
    let body = encode_publish(PUSH, SITE, &rec, &big, Some(true)).unwrap();
    assert!(body.len() <= PUBLISH_REQUEST_BYTES, "{}", body.len());
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["manifest"]["files"].as_array().unwrap().len(), 32_000);
    assert_eq!(v["record"]["selection"].as_array().unwrap().len(), 100_000);
    // One more selection row is refused.
    rec.selection.push(SelectionEntry {
        id: block("E-00000"),
        version: h(1),
        redaction: None,
    });
    assert_eq!(
        encode_publish(PUSH, SITE, &rec, &big, None)
            .unwrap_err()
            .code,
        E_LIMIT
    );
    assert_eq!(s.finish(), 0);
}

/// A missing list must be a sorted, unique subset of the request: an injected, repeated or
/// unsorted entry is a contract mismatch.
#[test]
fn hpb7_missing_replies_are_sorted_unique_subsets() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let keys = sorted(vec![key(1, 1), key(1, 2), key(2, 3)]);
    let k = |k: &ObjectKey| format!(r#"{{"kind":{},"hash":"{}"}}"#, k.kind, k.hash);
    let injected = key(1, 9);
    for missing in [
        vec![k(&keys[0]), k(&injected)],
        vec![k(&keys[0]), k(&keys[0])],
        vec![k(&keys[2]), k(&keys[0])],
        vec![format!(
            r#"{{"kind":1,"hash":"{}"}}"#,
            keys[0].hash.to_string().to_uppercase()
        )],
        vec![format!(
            r#"{{"kind":1,"hash":"{}","size":1}}"#,
            keys[0].hash
        )],
        vec![k(&keys[0]), k(&keys[1]), k(&keys[2]), k(&keys[2])],
    ] {
        let reply = format!(r#"{{"missing":[{}]}}"#, missing.join(","));
        let (r, _) = served(&mut s, Reply::json(200, &reply), || {
            hub.wire.known(&cx, PUSH, &keys)
        });
        protocol(r);
    }
    let (r, _) = served(&mut s, Reply::json(200, r#"{"missing":[]}"#), || {
        hub.wire.known(&cx, PUSH, &keys)
    });
    assert_eq!(r.unwrap(), vec![]);

    let files = vec![
        FileIdentity {
            hash: h(1),
            size: 1,
        },
        FileIdentity {
            hash: h(2),
            size: 2,
        },
    ];
    let mut held = [h(1), h(2)];
    held.sort();
    for missing in [
        vec![held[0].to_string(), h(9).to_string()],
        vec![held[0].to_string(), held[0].to_string()],
        vec![held[1].to_string(), held[0].to_string()],
    ] {
        let list: Vec<String> = missing.iter().map(|m| format!("\"{m}\"")).collect();
        let reply = format!(r#"{{"missing":[{}]}}"#, list.join(","));
        let (r, _) = served(&mut s, Reply::json(200, &reply), || {
            hub.wire.known_files(&cx, PUSH, &files)
        });
        protocol(r);
    }
    // A reply that names a different file than the one put is not the hub's answer to it; the
    // write may have completed.
    let file = FileIdentity {
        hash: h(1),
        size: 1,
    };
    let mut one = Cursor::new(vec![0u8]);
    let reply = format!(r#"{{"hash":"{}","size":1}}"#, h(2));
    let (r, _) = served(&mut s, Reply::json(201, &reply), || {
        hub.wire.put_file(&cx, PUSH, &file, "text/plain", &mut one)
    });
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);
    assert_eq!(s.finish(), 0);
}

/// Every recognized code at its status keeps its code, message and fix; a recognized code at
/// another status, an unknown code, a malformed object or a token echo shows no hub text.
#[test]
fn hpb7_hub_errors_keep_codes_fixes_and_hide_unknown_bodies() {
    /// Every API_ERRORS row: HA2a's 17 and the upload's 35.
    const ROWS: [(&str, u16); 52] = [
        ("E_HUB_ACTOR_CLAIM", 403),
        ("E_HUB_NO_CREDENTIAL", 401),
        ("E_HUB_CREDENTIAL_UNKNOWN", 401),
        ("E_HUB_CREDENTIAL_EXPIRED", 401),
        ("E_HUB_CREDENTIAL_REVOKED", 401),
        ("E_HUB_KEY_NOT_SIGNING", 422),
        ("E_HUB_KEY_CHALLENGE", 422),
        ("E_HUB_KEY_PROOF", 422),
        ("E_HUB_KEY_TYPE", 422),
        ("E_HUB_KEY_LIMIT", 422),
        ("E_HUB_VALIDATION", 422),
        ("E_HUB_KEY_TAKEN", 409),
        ("E_HUB_KEY_STATE", 409),
        ("E_HUB_NOT_FOUND", 404),
        ("E_HUB_KEY_ACTOR", 403),
        ("E_HUB_OUT_OF_REACH", 403),
        ("E_HUB_BAD_INPUT", 400),
        ("E_HUB_BAD_INPUT", 408),
        ("E_HUB_BAD_INPUT", 431),
        ("E_HUB_TWO_CREDENTIALS", 400),
        ("E_HUB_API_VERSION", 404),
        ("E_HUB_INTERNAL", 500),
        ("E_HUB_ACCESS_LEVEL", 403),
        ("E_HUB_CLIENT_UNVERIFIED", 403),
        ("E_HUB_SIGN_IN_SOURCE", 403),
        ("E_HUB_REPO_ARCHIVED", 403),
        ("E_HUB_EMAIL_UNVERIFIED", 403),
        ("E_HUB_STREAM_INVALID", 422),
        ("E_HUB_QUARANTINE_FULL", 422),
        ("E_HUB_STORE_LIMIT", 422),
        ("E_HUB_CORE_CHECK_FAILED", 422),
        ("E_HUB_OTHER_PROJECT", 409),
        ("E_HUB_STREAM_FORMAT", 422),
        ("E_HUB_RDY_TOO_OLD", 422),
        ("E_HUB_INSTALL_TOO_OLD", 422),
        ("E_HUB_OVER_BUDGET", 429),
        ("E_HUB_BUSY", 503),
        ("E_HUB_REPO_MOVING", 503),
        ("E_HUB_READ_ONLY", 503),
        ("E_HUB_MANIFEST_PATH", 422),
        ("E_HUB_ARTIFACT_LIMIT", 422),
        ("E_HUB_PROFILE_MISSING", 422),
        ("E_HUB_PROFILE_MISMATCH", 422),
        ("E_HUB_FILE_HASH", 422),
        ("E_HUB_MANIFEST_FILES", 422),
        ("E_HUB_RENDER_FAILED", 422),
        ("E_HUB_CANDIDATE_LIMIT", 422),
        ("E_HUB_TAG_PUBLISHED", 409),
        ("E_HUB_TAG_TAKEN", 409),
        ("E_HUB_REMOVAL_UNCOVERED", 409),
        ("E_HUB_SITE_AUDIENCE", 409),
        ("E_HUB_PUBLIC_SITES_OFF", 403),
    ];
    const RETRY: [&str; 4] = [
        "E_HUB_OVER_BUDGET",
        "E_HUB_BUSY",
        "E_HUB_REPO_MOVING",
        "E_HUB_READ_ONLY",
    ];
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let state = |s: &mut Server, status: u16, body: &str| {
        served(s, Reply::json(status, body), || {
            hub.wire.state(&cx, &state_request())
        })
        .0
    };
    let request_issue = |e: &RdyError| {
        let i = e
            .issues
            .iter()
            .find(|i| i.message == "hub request req_0001")
            .expect("request id issue");
        assert_eq!(i.code, E_UPSTREAM_REFUSED);
        assert_eq!(i.severity, rdy_schema::Severity::Error);
        assert_eq!(i.block, None);
        assert_eq!(i.fix, "quote this request id when you contact the operator");
    };

    for (code, status) in ROWS {
        let message = format!("{code} said this\nwith a second line");
        let fix = format!("do what {code} says");
        let extra = if RETRY.contains(&code) {
            r#","retry_after":30"#
        } else {
            ""
        };
        let body = error_body(code, &message, &fix, "[]", extra);
        let e = err(state(&mut s, status, &body));
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, &*message, &*fix)
        );
        request_issue(&e);
        let retry = e
            .issues
            .iter()
            .find(|i| i.message.starts_with("retry after"));
        assert_eq!(retry.is_some(), RETRY.contains(&code), "{code}");
        if let Some(i) = retry {
            assert_eq!(i.message, "retry after 30 seconds");
            assert_eq!(i.fix, "wait before trying again");
            assert_eq!(i.code, E_UPSTREAM_REFUSED);
        }
        // The same code at a status no row gives it: a mismatch.
        let wrong = if status == 418 { 419 } else { 418 };
        protocol(state(&mut s, wrong, &body));
    }
    // A retry code without retry_after; retry_after out of range.
    let no_retry = error_body("E_HUB_READ_ONLY", "read only", "wait", "[]", "");
    protocol(state(&mut s, 503, &no_retry));
    for n in ["0", "31536001", "-1", "1.5", "null", "\"5\""] {
        let body = error_body(
            "E_HUB_READ_ONLY",
            "m",
            "f",
            "[]",
            &format!(r#","retry_after":{n}"#),
        );
        protocol(state(&mut s, 503, &body));
    }
    let max = error_body(
        "E_HUB_READ_ONLY",
        "m",
        "f",
        "[]",
        r#","retry_after":31536000"#,
    );
    assert_eq!(err(state(&mut s, 503, &max)).code, "E_HUB_READ_ONLY");

    // An unknown code: the generic refusal, and none of the hub's text.
    let unknown = error_body(
        "E_HUB_SOMETHING_NEW",
        "SECRET-MESSAGE",
        "SECRET-FIX",
        "[]",
        "",
    );
    let e = err(state(&mut s, 422, &unknown));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_REFUSED,
            UPSTREAM_REFUSED_MESSAGE,
            UPSTREAM_REFUSED_FIX
        )
    );
    request_issue(&e);
    assert!(!format!("{e:?}").contains("SECRET"), "{e:?}");

    // Issues: recognized hub and core codes keep their text in reply order; an unknown one is
    // generic. A core code at the top passes unchanged.
    let issues = r#"[{"severity":"error","code":"E_HUB_CORE_CHECK_FAILED","block":"D-0A000","message":"core said","fix":"fix it"},{"severity":"warning","code":"E_UNRESOLVED_REF","block":null,"message":"ref","fix":"add it"},{"severity":"error","code":"E_NEW_THING","block":null,"message":"SECRET","fix":"SECRET"}]"#;
    let body = error_body("E_UNRESOLVED_REF", "core message", "core fix", issues, "");
    let e = err(state(&mut s, 422, &body));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_UNRESOLVED_REF, "core message", "core fix")
    );
    assert_eq!(e.issues.len(), 4, "{e:?}");
    assert_eq!(
        (
            e.issues[0].code,
            e.issues[0].message.as_str(),
            e.issues[0].fix.as_str()
        ),
        ("E_HUB_CORE_CHECK_FAILED", "core said", "fix it")
    );
    assert_eq!(e.issues[0].block, Some(block("D-0A000")));
    assert_eq!(e.issues[1].code, E_UNRESOLVED_REF);
    assert_eq!(e.issues[1].severity, rdy_schema::Severity::Warning);
    assert_eq!(
        (
            e.issues[2].code,
            e.issues[2].message.as_str(),
            e.issues[2].fix.as_str()
        ),
        (
            E_UPSTREAM_REFUSED,
            UPSTREAM_REFUSED_MESSAGE,
            UPSTREAM_REFUSED_FIX
        )
    );
    assert!(!format!("{e:?}").contains("SECRET"));
    request_issue(&e);
    // The named cases: a hub check of core's, sign-in source, a render job's code.
    for (code, status) in [
        ("E_HUB_CORE_CHECK_FAILED", 422),
        ("E_HUB_SIGN_IN_SOURCE", 403),
        ("E_HUB_RENDER_FAILED", 422),
    ] {
        let e = err(state(
            &mut s,
            status,
            &error_body(code, "hub text", "hub fix", "[]", ""),
        ));
        assert_eq!((e.code, e.fix.as_str()), (code, "hub fix"));
    }

    // Malformed objects: each is the protocol refusal.
    let issue = |code: &str, block: &str, message: &str| {
        format!(
            r#"{{"severity":"error","code":"{code}","block":{block},"message":{},"fix":"f"}}"#,
            json!(message)
        )
    };
    let many: Vec<String> = (0..201)
        .map(|_| issue("E_HUB_VALIDATION", "null", "m"))
        .collect();
    let bad = [
        error_body("e_hub_busy", "m", "f", "[]", ""),
        error_body("", "m", "f", "[]", ""),
        error_body(&format!("E_{}", "A".repeat(63)), "m", "f", "[]", ""),
        error_body("E_HUB-BUSY", "m", "f", "[]", ""),
        error_body("E_HUB_VALIDATION", &"m".repeat(4097), "f", "[]", ""),
        error_body("E_HUB_VALIDATION", "m", &"f".repeat(4097), "[]", ""),
        error_body("E_HUB_VALIDATION", "tab\there", "f", "[]", ""),
        error_body("E_HUB_VALIDATION", "m", "cr\rhere", "[]", ""),
        error_body("E_HUB_VALIDATION", "m", "f", "[]", "").replace("req_0001", ""),
        error_body("E_HUB_VALIDATION", "m", "f", "[]", "").replace("req_0001", "req\n1"),
        error_body("E_HUB_VALIDATION", "m", "f", "[]", "").replace("req_0001", &"r".repeat(4097)),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            &format!("[{}]", many.join(",")),
            "",
        ),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            &format!("[{}]", issue("E_HUB_VALIDATION", "\"not-a-block\"", "m")),
            "",
        ),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            &format!("[{}]", issue("e_lower", "null", "m")),
            "",
        ),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            &format!("[{}]", issue("E_HUB_VALIDATION", "null", &"m".repeat(4097))),
            "",
        ),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            r#"[{"severity":"fatal","code":"E_HUB_VALIDATION","block":null,"message":"m","fix":"f"}]"#,
            "",
        ),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            r#"[{"severity":"error","code":"E_HUB_VALIDATION","message":"m","fix":"f"}]"#,
            "",
        ),
        error_body("E_HUB_VALIDATION", "m", "f", "[]", r#","detail":"x""#),
        error_body(
            "E_HUB_VALIDATION",
            "m",
            "f",
            "[]",
            r#","code":"E_HUB_BUSY""#,
        ),
        error_body("E_HUB_VALIDATION", "m", "f", "null", ""),
        r#"{"code":"E_HUB_VALIDATION","message":"m","fix":"f","issues":[]}"#.to_string(),
        "not json".to_string(),
        String::new(),
    ];
    for body in &bad {
        protocol(state(&mut s, 422, body));
    }
    // LF is the one control character message and fix keep.
    let lf = error_body("E_HUB_VALIDATION", "one\ntwo", "three\nfour", "[]", "");
    assert_eq!(err(state(&mut s, 422, &lf)).message, "one\ntwo");

    // Token echoes, in every decoded string: the whole body is discarded.
    for token in [ACCESS, REFRESH, REFRESH_X, "rdyh_", "rdyx_"] {
        let with = |field: &str| format!("before {token} after {field}");
        let echoes = [
            error_body("E_HUB_VALIDATION", &with("message"), "f", "[]", ""),
            error_body("E_HUB_VALIDATION", "m", &with("fix"), "[]", ""),
            error_body("E_HUB_VALIDATION", "m", "f", "[]", "")
                .replace("req_0001", &with("request")),
            error_body(
                "E_HUB_VALIDATION",
                "m",
                "f",
                &format!("[{}]", issue("E_HUB_VALIDATION", "null", &with("issue"))),
                "",
            ),
            error_body(
                "E_HUB_VALIDATION",
                "m",
                "f",
                &format!(
                    r#"[{{"severity":"error","code":"E_X","block":null,"message":"m","fix":{}}}]"#,
                    json!(with("issue fix"))
                ),
                "",
            ),
            error_body("E_UNKNOWN_ELSEWHERE", &with("unknown"), "f", "[]", ""),
        ];
        for body in &echoes {
            let e = err(state(&mut s, 422, body));
            assert_eq!(e.code, E_UPSTREAM_PROTOCOL, "{token}");
            assert!(!format!("{e:?}").contains(token), "{e:?}");
        }
        // A sent write with an echoed body may have completed.
        let m = manifest();
        let rec = record(None, &m);
        let body = error_body("E_HUB_VALIDATION", &with("message"), "f", "[]", "");
        let (r, _) = served(&mut s, Reply::json(422, &body), || {
            hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None)
        });
        let e = err(r);
        assert_eq!(e.code, E_UPSTREAM_UNCERTAIN);
        assert!(!format!("{e:?}").contains(token));
    }
    // An escaped token is still a token.
    let escaped =
        error_body("E_HUB_VALIDATION", "x", "f", "[]", "").replace(r#""x""#, r#""\u0072dyh_a""#);
    protocol(state(&mut s, 422, &escaped));
    assert_eq!(s.finish(), 0);
}

/// Every `E_*` declared in error.rs and brief.rs maps to its own static spelling, and the hub's
/// copy of a core code reaches the caller unchanged.
#[test]
fn hpb7_every_core_error_keeps_its_static_code() {
    let mut declared = Vec::new();
    for source in [
        include_str!("../src/error.rs"),
        include_str!("../src/brief.rs"),
    ] {
        for line in source.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const E_") else {
                continue;
            };
            let (name, value) = rest
                .split_once(": &str = ")
                .expect("one-line &str constant");
            let value = value.trim_end_matches(';').trim_matches('"');
            assert_eq!(format!("E_{name}"), value, "a constant spells its own name");
            declared.push(value.to_string());
        }
    }
    assert_eq!(declared.len(), 96, "{declared:?}");
    for code in [
        "E_STREAM_TRUNCATED",
        "E_STREAM_PURPOSE",
        "E_REPO_MISMATCH",
        "E_BRIEF_REQUIRED",
        "E_BRIEF_STALE",
    ] {
        assert!(declared.iter().any(|d| d == code), "{code}");
    }
    for code in &declared {
        let found = core_error_code(code).unwrap_or_else(|| panic!("{code} is not looked up"));
        assert_eq!(found, code.as_str());
    }
    for not in [
        "E_HUB_BUSY",
        "E_UNKNOWN",
        "e_io",
        "E_IO ",
        "W_PUBLICATION_REFERENCE",
        "",
    ] {
        assert_eq!(core_error_code(not), None, "{not}");
    }

    // Through the wire: each core code, at the hub's refusal status, keeps its code and text.
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    for code in &declared {
        let body = error_body(code, "the hub's copy", "its fix", "[]", "");
        let (r, _) = served(&mut s, Reply::json(422, &body), || {
            hub.wire.state(&cx, &state_request())
        });
        let e = err(r);
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code.as_str(), "the hub's copy", "its fix")
        );
    }
    assert_eq!(s.finish(), 0);
}

/// Unknown compatible and read-only features do not block the upload; an unknown incompatible
/// one does. Malformed feature lists are refused.
#[test]
fn hpb7_feature_compatibility_is_not_an_empty_set_guard() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let ok = [
        "[]",
        r#"[{"name":"future_feature","class":"compatible"}]"#,
        r#"[{"name":"a-1","class":"read_only"},{"name":"b_2","class":"compatible"}]"#,
    ];
    for features in ok {
        let (r, _) = served(
            &mut s,
            Reply::json(200, &state_reply(features, "null")),
            || hub.wire.state(&cx, &state_request()),
        );
        r.unwrap_or_else(|e| panic!("{features}: {e:?}"));
    }
    let name_128 = "n".repeat(128);
    let (r, _) = served(
        &mut s,
        Reply::json(
            200,
            &state_reply(
                &format!(r#"[{{"name":"{name_128}","class":"compatible"}}]"#),
                "null",
            ),
        ),
        || hub.wire.state(&cx, &state_request()),
    );
    r.unwrap();
    let refused = [
        r#"[{"name":"future_feature","class":"incompatible"}]"#.to_string(),
        r#"[{"name":"a","class":"compatible"},{"name":"z","class":"incompatible"}]"#.to_string(),
        r#"[{"name":"b","class":"compatible"},{"name":"a","class":"compatible"}]"#.to_string(),
        r#"[{"name":"a","class":"compatible"},{"name":"a","class":"read_only"}]"#.to_string(),
        r#"[{"name":"","class":"compatible"}]"#.to_string(),
        r#"[{"name":"has space","class":"compatible"}]"#.to_string(),
        format!(r#"[{{"name":"{}","class":"compatible"}}]"#, "n".repeat(129)),
        r#"[{"name":"a","class":"readonly"}]"#.to_string(),
        r#"[{"name":"a"}]"#.to_string(),
        r#"[{"name":"a","class":"compatible","since":1}]"#.to_string(),
        r#"["a"]"#.to_string(),
    ];
    for features in &refused {
        let (r, _) = served(
            &mut s,
            Reply::json(200, &state_reply(features, "null")),
            || hub.wire.state(&cx, &state_request()),
        );
        protocol(r);
    }
    assert_eq!(s.finish(), 0);
}

/// A lost reply is never followed by a second request: reads report the transport, writes
/// report that they may have completed.
#[test]
fn hpb7_no_call_retries_after_a_lost_reply() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let remote = format!("{}{REMOTE}", s.origin());
    let keys = sorted(vec![key(1, 1)]);
    let files = vec![FileIdentity {
        hash: h(1),
        size: 1,
    }];
    let m = manifest();
    let rec = record(None, &m);
    type Call<'a> = Box<dyn Fn() -> Result<String, RdyError> + 'a>;
    let calls: Vec<(&str, &str, Call)> = vec![
        (
            "discovery",
            E_UPSTREAM_TRANSPORT,
            Box::new(|| hub.wire.discovery(&cx, &remote).map(|d| format!("{d:?}"))),
        ),
        (
            "state",
            E_UPSTREAM_TRANSPORT,
            Box::new(|| {
                hub.wire
                    .state(&cx, &state_request())
                    .map(|d| format!("{d:?}"))
            }),
        ),
        (
            "known",
            E_UPSTREAM_TRANSPORT,
            Box::new(|| hub.wire.known(&cx, PUSH, &keys).map(|d| format!("{d:?}"))),
        ),
        (
            "files known",
            E_UPSTREAM_TRANSPORT,
            Box::new(|| {
                hub.wire
                    .known_files(&cx, PUSH, &files)
                    .map(|d| format!("{d:?}"))
            }),
        ),
        (
            "objects",
            E_UPSTREAM_UNCERTAIN,
            Box::new(|| {
                let mut b = Cursor::new(b"abc".to_vec());
                hub.wire
                    .put_objects(&cx, PUSH, &mut b, 3)
                    .map(|d| format!("{d:?}"))
            }),
        ),
        (
            "file",
            E_UPSTREAM_UNCERTAIN,
            Box::new(|| {
                let mut b = Cursor::new(vec![7u8]);
                hub.wire
                    .put_file(&cx, PUSH, &files[0], "text/plain", &mut b)
                    .map(|d| format!("{d:?}"))
            }),
        ),
        (
            "publish",
            E_UPSTREAM_UNCERTAIN,
            Box::new(|| {
                hub.wire
                    .publish(&cx, PUSH, SITE, &rec, &m, None)
                    .map(|d| format!("{d:?}"))
            }),
        ),
    ];
    for (name, code, call) in &calls {
        let before = s.requests().len();
        let accepted = s.accepted();
        s.reply(Reply::Close);
        s.accept_n(1);
        let e = err(call());
        assert_eq!(e.code, *code, "{name}");
        assert_eq!(s.requests().len(), before + 1, "{name}: one request");
        assert_eq!(s.accepted(), accepted + 1, "{name}: one connection");
    }
    // An invalid reply to a write is uncertain too, still with one request; to a read, it is
    // the protocol refusal.
    let (r, _) = served(
        &mut s,
        Reply::json(201, r#"{"id":"pub_1","state":"live","url":"u"}"#),
        || hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None),
    );
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);
    let mut b = Cursor::new(b"abc".to_vec());
    let (r, _) = served(
        &mut s,
        Reply::json(202, r#"{"records":1,"bytes":3}"#),
        || hub.wire.put_objects(&cx, PUSH, &mut b, 3),
    );
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);
    let (r, _) = served(&mut s, Reply::json(201, &state_reply("[]", "null")), || {
        hub.wire.state(&cx, &state_request())
    });
    protocol(r);
    // A refusal before sending is a transport failure, never uncertain: a bad push id and a
    // missing sync base send nothing.
    let mut b = Cursor::new(b"abc".to_vec());
    assert_eq!(
        err(unsent(&[&s], || hub.wire.put_objects(
            &cx,
            "NOT-A-PUSH",
            &mut b,
            3
        )))
        .code,
        E_BAD_INPUT
    );
    let no_base = hub.cx_with(None, Some("r7"));
    assert_eq!(
        err(unsent(&[&s], || hub
            .wire
            .publish(&no_base, PUSH, SITE, &rec, &m, None)))
        .code,
        E_BAD_INPUT
    );
    assert_eq!(
        err(unsent(&[&s], || hub
            .wire
            .publish(&cx, PUSH, "-docs", &rec, &m, None)))
        .code,
        E_BAD_INPUT
    );
    assert_eq!(s.finish(), 0);
}

// ---------------------------------------------------------------------------------------------
// Fix round 1 regressions
// ---------------------------------------------------------------------------------------------

/// A global allocator that counts, on a thread that asks for it, the allocations of one size.
/// Other threads and other tests run unwatched, so the count is this call's alone.
mod sized_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Sized;

    thread_local! {
        /// The watched size and how many allocations of it this thread made.
        static WATCH: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    }

    fn note(size: usize) {
        let _ = WATCH.try_with(|w| {
            if let Some((watched, n)) = w.get()
                && watched == size
            {
                w.set(Some((watched, n + 1)));
            }
        });
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Sized {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note(new_size);
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// `f`'s result and how many allocations of exactly `size` bytes this thread made in it.
    pub fn count<T>(size: usize, f: impl FnOnce() -> T) -> (T, usize) {
        WATCH.with(|w| w.set(Some((size, 0))));
        let out = f();
        let n = WATCH.with(|w| w.replace(None)).map_or(0, |(_, n)| n);
        (out, n)
    }
}

#[global_allocator]
static ALLOC: sized_alloc::Sized = sized_alloc::Sized;

/// A stored tag reads at any length up to HA1's upload bound (1,048,576 bytes); the live site's
/// 256-byte limit on new names does not apply to a publication of an existing tag.
#[test]
fn hpb7_publication_takes_every_stored_tag_within_the_upload_bound() {
    let m = manifest();
    let mut rec = record(None, &m);
    // 257 bytes and more: a valid stored tag the site would not serve.
    rec.tag = format!("v1.0.0-{}", "a".repeat(250));
    let body = encode_publish(PUSH, SITE, &rec, &m, None).unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["record"]["tag"], json!(rec.tag));
    // Exactly the upload bound.
    let max = 1_048_576;
    rec.tag = format!("v1.0.0-{}", "a".repeat(max - "v1.0.0-".len()));
    assert_eq!(rec.tag.len(), max);
    encode_publish(PUSH, SITE, &rec, &m, None).unwrap();
    // One byte past it: HA1's limit refusal, checked before the version grammar.
    rec.tag.push('a');
    let e = err(encode_publish(PUSH, SITE, &rec, &m, None));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "upload tag exceeds 1048576 bytes",
            "reduce the tag name to the stated limit"
        )
    );
    rec.tag = format!("x{}", "a".repeat(max));
    assert_eq!(
        err(encode_publish(PUSH, SITE, &rec, &m, None)).code,
        E_LIMIT
    );
    // Not a version: the version grammar's refusal.
    for name in ["v1.2", "release", "v1.2.0+build", ""] {
        rec.tag = name.to_string();
        let e = err(encode_publish(PUSH, SITE, &rec, &m, None));
        assert_eq!(e.code, rdy_engine::E_BAD_TAG_NAME, "{name}");
    }
    // The long tag reaches the hub through the wire as well.
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    rec.tag = format!("v1.0.0-{}", "a".repeat(250));
    let (r, req) = served(&mut s, Reply::json(201, CANDIDATE), || {
        hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None)
    });
    r.unwrap();
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(sent["record"]["tag"], json!(rec.tag));
    assert_eq!(s.finish(), 0);
}

/// A refusal with more issues than its 200-issue bound is refused before the issues past the
/// bound are allocated: of 10,000 issue messages, at most 200 are ever built.
#[test]
fn hpb7_error_issues_stop_at_their_bound_before_allocation() {
    /// An odd message length no other allocation of the exchange shares.
    const LEN: usize = 1009;
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let keys = sorted(vec![key(1, 1)]);
    let message = "m".repeat(LEN);
    let issues = |n: usize| {
        let one = format!(
            r#"{{"severity":"error","code":"E_HUB_VALIDATION","block":null,"message":"{message}","fix":"f"}}"#
        );
        format!("[{}]", vec![one; n].join(","))
    };
    // 200 issues: kept, each message built once.
    let body = error_body("E_HUB_VALIDATION", "m", "f", &issues(200), "");
    let ((r, _), n) = sized_alloc::count(LEN, || {
        served(&mut s, Reply::json(422, &body), || {
            hub.wire.known(&cx, PUSH, &keys)
        })
    });
    let e = err(r);
    assert_eq!((e.code, e.issues.len()), ("E_HUB_VALIDATION", 201));
    assert_eq!(n, 200, "each kept message once");
    // 10,000 issues, within the known reply's byte and value caps: refused, and no message past
    // the 200th is built.
    let body = error_body("E_HUB_VALIDATION", "m", "f", &issues(10_000), "");
    assert!(body.len() <= KNOWN_REPLY_BYTES, "{}", body.len());
    let ((r, _), n) = sized_alloc::count(LEN, || {
        served(&mut s, Reply::json(422, &body), || {
            hub.wire.known(&cx, PUSH, &keys)
        })
    });
    protocol(r);
    assert!(n <= 200, "{n} issue messages built");
    assert_eq!(s.finish(), 0);
}

/// A token in `corrected`, however its JSON escapes spell it and at any depth, as a string or a
/// key, discards the whole body: a read is the protocol refusal and a sent write uncertain.
#[test]
fn hpb7_escaped_tokens_in_corrected_discard_the_body() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let m = manifest();
    let rec = record(None, &m);
    // Its first `r` as a JSON `\u` escape: the raw body never holds the token's text.
    let r_escaped = format!("\\u{:04x}", u32::from('r'));
    assert_eq!(r_escaped.len(), 6);
    let escape = |t: &str| t.replacen('r', &r_escaped, 1);
    // A harmless `corrected` is still bounded and discarded, and the refusal kept.
    let plain = error_body(
        "E_HUB_VALIDATION",
        "m",
        "f",
        "[]",
        r#","corrected":{"a":{"b":["fine",1,null]}}"#,
    );
    let (r, _) = served(&mut s, Reply::json(422, &plain), || {
        hub.wire.state(&cx, &state_request())
    });
    assert_eq!(err(r).code, "E_HUB_VALIDATION");
    for token in [ACCESS, REFRESH, REFRESH_X] {
        let t = escape(token);
        assert!(!t.contains(token));
        for corrected in [
            format!(r#","corrected":{{"a":{{"b":["x","{t}"]}}}}"#),
            format!(r#","corrected":{{"a":[{{"{t}":1}}]}}"#),
            format!(r#","corrected":{{"{t}":null}}"#),
            format!(r#","corrected":{{"a":"before {t} after"}}"#),
        ] {
            let body = error_body("E_HUB_VALIDATION", "m", "f", "[]", &corrected);
            let (r, _) = served(&mut s, Reply::json(422, &body), || {
                hub.wire.state(&cx, &state_request())
            });
            protocol(r);
            let (r, _) = served(&mut s, Reply::json(422, &body), || {
                hub.wire.publish(&cx, PUSH, SITE, &rec, &m, None)
            });
            let e = err(r);
            assert_eq!(e.code, E_UPSTREAM_UNCERTAIN, "{corrected}");
            assert!(!format!("{e:?}").contains(token));
        }
    }
    assert_eq!(s.finish(), 0);
}

/// A sync base the origin fence admits in another spelling of the same URL is taken in its
/// canonical form: an upper-case scheme and an empty path.
#[test]
fn hpb7_discovery_takes_harmless_spellings_of_an_admitted_base() {
    let mut a = Server::start();
    let hub = Hub::new(&a, &[], &[]);
    let cx = hub.cx_with(None, Some("r7"));
    let remote = format!("{}{REMOTE}", a.origin());
    let host = a.origin()["https://".len()..].to_string();
    for (given, canonical) in [
        (
            format!("HTTPS://{host}/sync"),
            format!("https://{host}/sync"),
        ),
        (
            format!("Https://{host}/sync/"),
            format!("https://{host}/sync/"),
        ),
        (format!("https://{host}"), format!("https://{host}/")),
    ] {
        let reply = format!(r#"{{"sync_base":{},"protocols":[1]}}"#, json!(given));
        let (r, _) = served(&mut a, Reply::json(200, &reply), || {
            hub.wire.discovery(&cx, &remote)
        });
        assert_eq!(r.unwrap().sync_base, canonical, "{given}");
    }
    // The empty path's routes sit at the root.
    let root = hub.cx_with(Some(format!("https://{host}/")), Some("r7"));
    let (r, req) = served(&mut a, Reply::json(200, &state_reply("[]", "null")), || {
        hub.wire.state(&root, &state_request())
    });
    r.unwrap();
    assert_eq!(req.target, "/state");
    // Dot segments are refused in the raw text, before the parser removes them.
    for bad in [
        format!("HTTPS://{host}/a/../sync"),
        format!("https://{host}/a/%2E%2e/sync"),
        format!("https://{host}/sync/."),
        format!("https://{host}//sync"),
    ] {
        let reply = format!(r#"{{"sync_base":{},"protocols":[1]}}"#, json!(bad));
        let (r, _) = served(&mut a, Reply::json(200, &reply), || {
            hub.wire.discovery(&cx, &remote)
        });
        protocol(r);
    }
    assert_eq!(a.finish(), 0);
}
