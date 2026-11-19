//! scc8 (D-H0BZN, D-N8PR2, D-4XHPH): the sync wire's steps against rdyhub's frozen wire
//! fixtures. Fixture v1 covers discovery, state (the publish-upload form, no views), known (push
//! form) and put; fixture v2 adds state with named views, known's pull form, get, write and the
//! outcome lookup; fixture v3a adds the held write reply and the held outcomes. Every test drives the real HTTPS wire against loopback TLS fixtures with
//! generated CAs; the exchange-level pins run the sync service over the fake hub of
//! `support/sync_env.rs`. Each fixture is a byte copy of the hub's file; its SHA-256 is pinned.
mod common;
#[path = "support/hub_tls.rs"]
mod hub_tls;
#[path = "support/sync_env.rs"]
mod sync_env;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::Engine;
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish_wire::{
    KNOWN_REPLY_BYTES, MAX_JSON_DEPTH, MAX_KNOWN_KEYS, ObjectPut, REPLY_BYTES, SYNC_MEDIA_TYPE,
    UploadContext,
};
use rdy_engine::hub_sync::{PullRequest, check_minimum};
use rdy_engine::hub_sync_wire::{
    GateSettings, GetRequest, HttpSyncWire, HubRequest, KnownPull, LookupOutcome, MAX_STATE_VIEWS,
    PackReason, ProposeGate, SETTINGS_MEMBERS, SyncStateRequest, SyncWire, WRITE_REQUEST_BYTES,
    WriteFailure, WriteKind, WriteOutcome, WriteRequest as SyncWrite, WriteTag, decode_settings,
};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::{
    E_BAD_INPUT, E_BAD_RECORD, E_LIMIT, E_STORE_FORMAT, E_UPSTREAM_ORIGIN, E_UPSTREAM_PROTOCOL,
    E_UPSTREAM_REFUSED, E_UPSTREAM_UNCERTAIN, RdyError, Session, SessionKind, UPSTREAM_REFUSED_FIX,
    UPSTREAM_REFUSED_MESSAGE,
};
use rdy_model::{Hash, Role};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{HISTORY_FORMAT, StreamPurpose};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::{Arc, OnceLock};

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const FIXTURE: &[u8] = include_bytes!("../../rdy-qa/fixtures/sync/sync-client-wire-v1.json");
const FIXTURE_SHA256: &str = "18ade8e2c1a301d780270f20fe49fc116110a6aa512baf624d768ef5ba39711f";
const FIXTURE_V2: &[u8] = include_bytes!("../../rdy-qa/fixtures/sync/sync-client-wire-v2.json");
const FIXTURE_V2_SHA256: &str = "ddc3e17a39425ba7af5a7a4be8531fa89e484b2ff82cc4f4b12d8d37418e33e7";
/// rdyhub's fixture v3a (972e7867): the held write reply and the held outcomes.
const FIXTURE_V3A: &[u8] = include_bytes!("../../rdy-qa/fixtures/sync/sync-client-wire-v3a.json");
const FIXTURE_V3A_SHA256: &str = "101179344b2a81b71878dbaf05864ceeaaed652b1978707fcf2bfd96f0b3b188";
const PUSH: &str = "0123456789abcdefghjkmnpqrs";
const REPO_PATH: &str = "/acme/docs";

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

/// The fixture's claim: actor `agent:ana/claude`, run `run-ana`.
fn claim() -> RequestClaim {
    RequestClaim::from_session(&Session {
        actor: "agent:ana/claude".into(),
        run: Some("run-ana".into()),
        role: Role::Writer,
        kind: SessionKind::Agent,
    })
    .unwrap()
}

/// One sync wire to the install `install`, with `pinned` pinned and every server in `trusted`
/// trusted.
struct Hub {
    install: Origin,
    ca: std::path::PathBuf,
    wire: HttpSyncWire,
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
            wire: HttpSyncWire::new(HttpHubTransport::new(fence).unwrap()),
            _dir: dir,
        }
    }

    fn cx_with(&self, base: Option<String>) -> UploadContext {
        UploadContext {
            install: self.install.clone(),
            sync_base: base,
            ca_file: Some(self.ca.clone()),
            claim: claim(),
            bearer: Secret::new(ACCESS),
        }
    }

    fn cx(&self) -> UploadContext {
        self.cx_with(Some(format!("{}/sync{REPO_PATH}", self.install)))
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

fn key(kind: u8, n: u8) -> ObjectKey {
    ObjectKey {
        kind,
        hash: Hash::of(&[n]),
    }
}

/// A state reply: the fixture's empty repo, with `members` replacing or adding members.
fn state_reply(members: Value) -> String {
    let mut v = json!({
        "artifact_bytes": 201326592, "artifact_files": 32000, "audience": null,
        "batch_bytes": 1073741824, "batch_records": 1000000, "canon": null, "features": [],
        "min_rdy": null, "profile": null, "quarantine_seconds": 3600, "rdy": "0.53.0",
        "repo_id": null, "stream_read": [4], "stream_write": [4]
    });
    for (k, m) in members.as_object().unwrap() {
        v[k] = m.clone();
    }
    v.to_string()
}

fn object_keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn fixture() -> Value {
    serde_json::from_slice(FIXTURE).unwrap()
}

/// The fixture's steps named `step`, in order.
fn steps(f: &Value, step: &str) -> Vec<Value> {
    f["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["step"] == step)
        .cloned()
        .collect()
}

/// The fixture's reply of `s` as a reply of the TLS fixture.
fn reply_of(s: &Value) -> Reply {
    let r = &s["reply"];
    assert_eq!(r["content_type"], "application/json");
    Reply::json(r["status"].as_u64().unwrap() as u16, &r["body"].to_string())
}

/// The request matches the fixture's method and path with query, and carries each fixture
/// header with the token normalized as the fixture's `about` says.
#[track_caller]
fn matches_request(got: &Recorded, s: &Value) {
    let want = &s["request"];
    assert_eq!(got.method, want["method"].as_str().unwrap());
    assert_eq!(got.target, want["path"].as_str().unwrap());
    for (name, value) in want["headers"].as_object().unwrap() {
        let value = value.as_str().unwrap().replace("<token>", ACCESS);
        assert_eq!(got.header(name), Some(value.as_str()), "{name}");
    }
}

/// The error the hub's fixture refusal `s` becomes: its code, message and fix, with the request
/// id notice (and a busy hub's wait).
#[track_caller]
fn matches_refusal(e: &RdyError, s: &Value) {
    let body = &s["reply"]["body"];
    assert_eq!(e.code, body["code"].as_str().unwrap());
    assert_eq!(e.message, body["message"].as_str().unwrap());
    assert_eq!(e.fix, body["fix"].as_str().unwrap());
    let id = format!("hub request {}", body["request_id"].as_str().unwrap());
    assert!(e.issues.iter().any(|i| i.message == id), "{:?}", e.issues);
    // a busy hub's wait comes with it
    if let Some(n) = body["retry_after"].as_u64() {
        let wait = format!("retry after {n} seconds");
        assert!(e.issues.iter().any(|i| i.message == wait), "{:?}", e.issues);
    }
}

fn sha256(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fixture_v2() -> Value {
    serde_json::from_slice(FIXTURE_V2).unwrap()
}

/// An object key in the fixture's form.
fn key_of(v: &Value) -> ObjectKey {
    serde_json::from_value(v.clone()).unwrap()
}

/// A write of the fixture's write body `b`, with this client's gate values in place of the
/// fixture's nulls (open question Q2: the client sends its effective gate values), and the
/// body's tags.
fn write_of(b: &Value) -> SyncWrite {
    let render_inputs: RenderInputs = serde_json::from_value(b["render_inputs"].clone()).unwrap();
    let tags = b["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            assert_eq!(t["disagreements"], json!([]), "{t}");
            WriteTag {
                name: t["name"].as_str().unwrap().into(),
                disagreements: vec![],
            }
        })
        .collect();
    SyncWrite {
        expected: (!b["expected"].is_null()).then(|| key_of(&b["expected"])),
        gate: Some(GateSettings {
            coverage_gate: "off".into(),
            coverage_target: 0,
            decisions_home: None,
            pack_digest: Hash::of(b"the test's pack"),
            propose: ProposeGate {
                cadence: "break".into(),
                changes: 1,
            },
        }),
        kind: WriteKind::Landings,
        push: b["push"].as_str().unwrap().into(),
        render_inputs: Some(render_inputs),
        tags,
        view: None,
    }
}

/// The fixture's first write body: a valid write the tests reuse.
fn a_write() -> SyncWrite {
    let f = fixture_v2();
    write_of(&f["steps"][10]["request"]["body"])
}

#[track_caller]
fn refused<T: std::fmt::Debug>(r: Result<T, WriteFailure>) -> RdyError {
    match r {
        Err(WriteFailure::Refused(e)) => *e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A body that is `v` without its member `name`.
fn without(v: &Value, name: &str) -> Value {
    let mut v = v.clone();
    v.as_object_mut().unwrap().remove(name);
    v
}

/// A loopback install origin on a port made at run time; nothing connects to it.
fn install() -> &'static str {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    ORIGIN.get_or_init(|| {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("https://127.0.0.1:{}", held.local_addr().unwrap().port())
    })
}

fn fake_env(landings: usize) -> sync_env::Env {
    sync_env::env_with(install(), "# person settings\n", Some("shared"), landings)
}

/// Fixture v2 (8a2): every step the client sends, its request against the fixture's (headers
/// as a subset, the token normalized; the client sends stream format 4 alone), and every reply
/// the client decodes. The hub's refusals map as publish maps them, and `E_HUB_SHARE_OFF`, in
/// no table, is `E_UPSTREAM_REFUSED` with core's text (Q3). The settings steps are a URL clone's
/// settings read.
fn fixture_v2_steps() {
    assert_eq!(sha256(FIXTURE_V2), FIXTURE_V2_SHA256);
    let f = fixture_v2();
    assert_eq!(f["fixture"], "sync-client-wire");
    assert_eq!(f["version"], 2);
    assert_eq!(f["media_type"], SYNC_MEDIA_TYPE);
    let st: Vec<Value> = f["steps"].as_array().unwrap().clone();
    assert_eq!(st.len(), 35);
    let named: Vec<&str> = st.iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(
        named,
        [
            "discovery",
            "discovery",
            "state",
            "state",
            "state",
            "settings",
            "put",
            "write",
            "put",
            "known",
            "write",
            "outcome",
            "settings",
            "state",
            "known",
            "known",
            "get",
            "get",
            "write",
            "settings",
            "write",
            "outcome",
            "write",
            "write",
            "write",
            "outcome",
            "outcome",
            "outcome",
            "outcome",
            "write",
            "outcome",
            "write",
            "write",
            "write",
            "write"
        ]
    );
    // the cases the hub does not serve yet: six, each with its step, design and reason; the
    // client pins none of them here (rdyhub d84d2d96 serves the tag cases). Fixture v3a serves
    // the two held cases (scc8_wire_fixture_v3a_held_write_and_outcome)
    let not_served = f["not_served"].as_array().unwrap();
    let skipped: Vec<(&str, &str)> = not_served
        .iter()
        .map(|n| (n["step"].as_str().unwrap(), n["case"].as_str().unwrap()))
        .collect();
    assert_eq!(
        skipped.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        ["dump", "state", "write", "write", "write", "outcome"]
    );
    for n in not_served {
        for m in ["case", "design", "reason"] {
            assert!(!n[m].as_str().unwrap().is_empty(), "{n}");
        }
    }
    // the settings steps (5, 12, 19), a URL clone's settings read, are pinned below with every
    // other step
    let settings: Vec<usize> = (0..st.len()).filter(|i| named[*i] == "settings").collect();
    assert_eq!(settings, [5, 12, 19]);

    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let origin = s.origin().to_string();

    // discovery (0, 1)
    let cx = hub.cx_with(None);
    let path = |x: &Value| format!("{origin}{}", x["request"]["path"].as_str().unwrap());
    let (r, req) = served(&mut s, reply_of(&st[0]), || {
        hub.wire.discovery(&cx, &path(&st[0]))
    });
    matches_request(&req, &st[0]);
    let d = r.unwrap();
    assert_eq!(d.sync_base, format!("{origin}/sync{REPO_PATH}"));
    let (r, _) = served(&mut s, reply_of(&st[1]), || {
        hub.wire.discovery(&cx, &path(&st[1]))
    });
    matches_refusal(&err(r), &st[1]);

    // state (2, 3, 13; 4 decoded through the mock)
    let cx = hub.cx_with(Some(d.sync_base.clone()));
    let plain = SyncStateRequest::new(&cx.claim);
    let (r, req) = served(&mut s, reply_of(&st[2]), || hub.wire.state(&cx, &plain));
    matches_request(&req, &st[2]);
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    let want = &st[2]["request"]["body"];
    assert_eq!(object_keys(&sent), object_keys(want));
    for m in ["actor", "run", "site"] {
        assert_eq!(sent[m], want[m], "{m}");
    }
    // the client's formats are a subset of the fixture client's
    for m in ["stream_read", "stream_write"] {
        assert_eq!(sent[m], json!([HISTORY_FORMAT]));
        assert!(want[m].as_array().unwrap().contains(&json!(HISTORY_FORMAT)));
    }
    let state = r.unwrap();
    assert_eq!(
        (state.canon, state.min_rdy.as_deref()),
        (None, Some("0.52.0"))
    );
    assert!(state.repo_id.is_some());
    assert!(state.views.is_empty());
    let (r, _) = served(&mut s, reply_of(&st[3]), || hub.wire.state(&cx, &plain));
    matches_refusal(&err(r), &st[3]);
    let two = SyncStateRequest::new(&cx.claim).with_views(&["main".into(), "drafts".into()]);
    let (r, req) = served(&mut s, reply_of(&st[13]), || hub.wire.state(&cx, &two));
    matches_request(&req, &st[13]);
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(object_keys(&sent), object_keys(&st[13]["request"]["body"]));
    assert_eq!(sent["views"], st[13]["request"]["body"]["views"]);
    let state = r.unwrap();
    let seed = key_of(&st[13]["reply"]["body"]["canon"]);
    assert_eq!(state.canon, Some(seed.hash));
    assert_eq!(
        state
            .views
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["main", "drafts"]
    );
    assert!(
        state
            .views
            .iter()
            .all(|v| v.head.is_none() && v.root.is_none())
    );
    assert!(state.hub_views().is_empty());
    // 4: 257 names never leave; the hub's refusal of them maps as given
    let many: Vec<String> = (0..=MAX_STATE_VIEWS).map(|i| format!("v{i}")).collect();
    assert_eq!(
        many.len(),
        st[4]["request"]["body"]["views"].as_array().unwrap().len()
    );
    let over = SyncStateRequest::new(&cx.claim).with_views(&many);
    assert_eq!(
        err(unsent(&[&s], || hub.wire.state(&cx, &over))).code,
        E_LIMIT
    );
    let (r, _) = served(&mut s, reply_of(&st[4]), || hub.wire.state(&cx, &two));
    matches_refusal(&err(r), &st[4]);

    // put (6, 8): one binary batch under the push id in the query
    for i in [6, 8] {
        let put = &st[i];
        let p = put["request"]["path"].as_str().unwrap();
        let push = p.rsplit_once("push=").unwrap().1;
        let len = put["request"]["body"]["bytes"].as_u64().unwrap();
        let batch = vec![0u8; len as usize];
        let (r, req) = served(&mut s, reply_of(put), || {
            hub.wire.put(&cx, push, &mut &batch[..], len)
        });
        matches_request(&req, put);
        assert_eq!(req.body.len() as u64, len);
        let rep = &put["reply"]["body"];
        assert_eq!(
            r.unwrap(),
            ObjectPut {
                records: rep["records"].as_u64().unwrap(),
                bytes: rep["bytes"].as_u64().unwrap()
            }
        );
    }

    // known, push form (9)
    let k = &st[9];
    let keys: Vec<ObjectKey> = k["request"]["body"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_of)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let push = k["request"]["body"]["push"].as_str().unwrap();
    let (r, req) = served(&mut s, reply_of(k), || hub.wire.known(&cx, push, &keys));
    matches_request(&req, k);
    let missing: Vec<ObjectKey> = k["reply"]["body"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_of)
        .collect();
    assert_eq!(r.unwrap(), missing);

    // known, pull form (14, 15): the request is the fixture's; the page keeps the hub's order
    let pull_of = |b: &Value| KnownPull {
        after: (!b["after"].is_null()).then(|| key_of(&b["after"])),
        base: (!b["base"].is_null()).then(|| key_of(&b["base"])),
        purpose: StreamPurpose::CanonHistory,
        root: key_of(&b["root"]),
    };
    let k = &st[14];
    let pull = pull_of(&k["request"]["body"]);
    let (r, req) = served(&mut s, reply_of(k), || hub.wire.known_pull(&cx, &pull));
    matches_request(&req, k);
    assert_eq!(
        serde_json::from_slice::<Value>(&req.body).unwrap(),
        k["request"]["body"]
    );
    let page = r.unwrap();
    let order: Vec<ObjectKey> = k["reply"]["body"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_of)
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_ne!(order, sorted, "the fixture's page is in export order");
    assert_eq!((page.keys, page.next), (order.clone(), None));
    let (r, _) = served(&mut s, reply_of(&st[15]), || {
        hub.wire
            .known_pull(&cx, &pull_of(&st[15]["request"]["body"]))
    });
    matches_refusal(&err(r), &st[15]);

    // get (16, 17): the request is the fixture's; the reply streams to the sink
    let g = &st[16];
    let b = &g["request"]["body"];
    let get_of = |b: &Value| GetRequest {
        keys: b["keys"].as_array().unwrap().iter().map(key_of).collect(),
        purpose: StreamPurpose::CanonHistory,
        root: key_of(&b["root"]),
    };
    let len = g["reply"]["body"]["bytes"].as_u64().unwrap();
    let batch = vec![7u8; len as usize];
    let reply = Reply::with_headers(
        200,
        &[
            "content-type: application/octet-stream".into(),
            format!("content-length: {len}"),
            "connection: close".into(),
        ],
        &batch,
    );
    let mut sink = Vec::new();
    let (r, req) = served(&mut s, reply, || {
        hub.wire.get(&cx, &get_of(b), &mut sink, len)
    });
    matches_request(&req, g);
    assert_eq!(serde_json::from_slice::<Value>(&req.body).unwrap(), *b);
    assert_eq!(r.unwrap(), len);
    assert_eq!(sink, batch);
    let (r, _) = served(&mut s, reply_of(&st[17]), || {
        hub.wire.get(
            &cx,
            &get_of(&st[17]["request"]["body"]),
            &mut Vec::new(),
            len,
        )
    });
    matches_refusal(&err(r), &st[17]);

    // write (10, 18): the fixture's body but the gate's values; the gate's members as given
    for i in [10, 18] {
        let w = &st[i];
        let b = &w["request"]["body"];
        let (r, req) = served(&mut s, reply_of(w), || hub.wire.write(&cx, &write_of(b)));
        matches_request(&req, w);
        let sent: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(without(&sent, "gate"), without(b, "gate"), "step {i}");
        assert_eq!(object_keys(&sent["gate"]), object_keys(&b["gate"]));
        assert_eq!(
            object_keys(&sent["gate"]["propose"]),
            BTreeSet::from(["cadence".into(), "changes".into()])
        );
        let rep = &w["reply"]["body"];
        let got = r.unwrap();
        assert_eq!(
            (got.outcome, got.tip, got.request, got.min_rdy_raised),
            (
                WriteOutcome::Taken,
                Some(key_of(&rep["tip"])),
                None,
                rep["min_rdy_raised"].as_bool().unwrap()
            )
        );
    }
    // the hub's refusals of a write (7, 20, 23): its words and request id
    for i in [7, 20, 23] {
        let e = refused(served(&mut s, reply_of(&st[i]), || hub.wire.write(&cx, &a_write())).0);
        matches_refusal(&e, &st[i]);
    }
    // 22 and 24: the client never sends such a write (inputs 33 deep do not form its render
    // inputs; a landings write with no gate is refused before sending); their replies decode as
    // given
    assert!(
        serde_json::from_value::<RenderInputs>(st[22]["request"]["body"]["render_inputs"].clone())
            .is_err()
    );
    let mut no_gate = a_write();
    no_gate.gate = None;
    match unsent(&[&s], || hub.wire.write(&cx, &no_gate)) {
        Err(WriteFailure::NotSent(e)) => assert_eq!(e.code, E_BAD_INPUT),
        other => panic!("{other:?}"),
    }
    for i in [22, 24] {
        let e = refused(served(&mut s, reply_of(&st[i]), || hub.wire.write(&cx, &a_write())).0);
        matches_refusal(&e, &st[i]);
    }
    // the pushed-tag writes (29, 31, 32; rdyhub d84d2d96), each in its own hub repo: taken,
    // the body the fixture's but the gate's values, every carried tag named once with no
    // disagreements (32 names v1.0.0, which the hub holds at that landing, and is taken)
    for (i, repo) in [(29, "docs"), (31, "fresh"), (32, "kept")] {
        let w = &st[i];
        let b = &w["request"]["body"];
        let tags: Vec<&str> = b["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(!tags.is_empty(), "step {i}");
        assert!(
            tags.windows(2).all(|w| w[0] < w[1]),
            "step {i}: once each, by name"
        );
        let rcx = hub.cx_with(Some(format!("{origin}/sync/acme/{repo}")));
        let (r, req) = served(&mut s, reply_of(w), || hub.wire.write(&rcx, &write_of(b)));
        matches_request(&req, w);
        let sent: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(without(&sent, "gate"), without(b, "gate"), "step {i}");
        let rep = &w["reply"]["body"];
        let got = r.unwrap();
        assert_eq!(
            (got.outcome, got.tip, got.request, got.min_rdy_raised),
            (
                WriteOutcome::Taken,
                Some(key_of(&rep["tip"])),
                None,
                rep["min_rdy_raised"].as_bool().unwrap()
            ),
            "step {i}"
        );
    }
    // 33: E_HUB_TAG_TAKEN keeps the hub's words and request id (the exchange gives it core's
    // code E_TAG_EXISTS)
    let press = hub.cx_with(Some(format!("{origin}/sync/acme/press")));
    let w = &st[33];
    let (r, req) = served(&mut s, reply_of(w), || {
        hub.wire.write(&press, &write_of(&w["request"]["body"]))
    });
    matches_request(&req, w);
    let e = refused(r);
    matches_refusal(&e, w);
    assert_eq!(e.code, "E_HUB_TAG_TAKEN");

    // 34: E_HUB_SHARE_OFF is in no table: core's text
    let e = refused(
        served(&mut s, reply_of(&st[34]), || {
            hub.wire.write(&cx, &a_write())
        })
        .0,
    );
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_REFUSED,
            UPSTREAM_REFUSED_MESSAGE,
            UPSTREAM_REFUSED_FIX
        )
    );

    // outcome (11, 21, 25, 28, 30; refusals 26, 27)
    let tagged = key_of(&st[30]["reply"]["body"]["tip"]);
    for (i, want) in [
        (11, LookupOutcome::Taken { tip: seed }),
        (30, LookupOutcome::Taken { tip: tagged }),
        (
            21,
            LookupOutcome::Refused {
                code: "E_HUB_BEHIND".into(),
            },
        ),
        (25, LookupOutcome::Unknown),
        (
            28,
            LookupOutcome::Refused {
                code: "E_HUB_BUSY".into(),
            },
        ),
    ] {
        let o = &st[i];
        let push = o["request"]["body"]["push"].as_str().unwrap();
        let (r, req) = served(&mut s, reply_of(o), || hub.wire.outcome(&cx, push));
        matches_request(&req, o);
        assert_eq!(
            serde_json::from_slice::<Value>(&req.body).unwrap(),
            o["request"]["body"]
        );
        let got = r.unwrap();
        assert_eq!(got.outcome, want, "step {i}");
        assert_eq!(got.kind.as_deref(), o["reply"]["body"]["kind"].as_str());
        assert_eq!((got.view, got.request_state), (None, None));
    }
    for i in [26, 27] {
        let push = st[i]["request"]["body"]["push"].as_str().unwrap();
        let (r, _) = served(&mut s, reply_of(&st[i]), || hub.wire.outcome(&cx, push));
        matches_refusal(&err(r), &st[i]);
    }

    // settings (5, 12, 19): POST <sync base>/settings with {}; the hub's own members read, the
    // captured members kept whole as the hub gave them
    let cases: Vec<&str> = settings
        .iter()
        .map(|i| st[*i]["case"].as_str().unwrap().split(':').next().unwrap())
        .collect();
    assert_eq!(cases, ["none", "mismatch", "pack"]);
    for i in settings {
        let step = &st[i];
        let (r, req) = served(&mut s, reply_of(step), || hub.wire.settings(&cx));
        matches_request(&req, step);
        assert_eq!(
            serde_json::from_slice::<Value>(&req.body).unwrap(),
            json!({})
        );
        let rep = &step["reply"]["body"];
        let got = r.unwrap();
        let version = rep["rdy"]
            .as_str()
            .map(|v| semver::Version::parse(v).unwrap());
        assert_eq!(got.rdy, version, "step {i}");
        let reason = match rep["pack_reason"].as_str() {
            None => None,
            Some("none") => Some(PackReason::None),
            Some("mismatch") => Some(PackReason::Mismatch),
            Some(other) => panic!("step {i}: {other}"),
        };
        assert_eq!(got.pack_reason, reason, "step {i}");
        for (m, k) in [
            ("gate_source", got.gate_source),
            ("render_source", got.render_source),
        ] {
            assert_eq!(
                k,
                (!rep[m].is_null()).then(|| key_of(&rep[m])),
                "step {i} {m}"
            );
        }
        for m in SETTINGS_MEMBERS {
            assert_eq!(
                got.member(m),
                (!rep[m].is_null()).then_some(&rep[m]),
                "step {i} {m}"
            );
        }
        // every member the fixture names is read: the hub's own, rdy, and the captured ones
        let mut known: BTreeSet<String> = SETTINGS_MEMBERS.iter().map(|m| m.to_string()).collect();
        known.extend(["gate_source", "pack_reason", "rdy", "render_source"].map(String::from));
        assert_eq!(object_keys(rep), known, "step {i}");
    }
    // the cases: none holds no member; mismatch holds the gate and title but no pack; pack holds
    // the pack in full with no reason
    assert!(
        SETTINGS_MEMBERS
            .iter()
            .all(|m| st[5]["reply"]["body"][m].is_null())
    );
    assert!(st[12]["reply"]["body"]["pack"].is_null());
    assert!(st[12]["reply"]["body"]["coverage_gate"].is_string());
    assert!(st[19]["reply"]["body"]["pack"].is_object());
    assert!(st[19]["reply"]["body"]["pack_reason"].is_null());
    // a reply outside the form is a protocol failure
    let token = Secret::new(ACCESS);
    let base = st[5]["reply"]["body"].clone();
    let with = |m: &str, v: Value| {
        let mut b = base.clone();
        b[m] = v;
        b.to_string().into_bytes()
    };
    for bad in [
        with("pack_reason", json!("other")),
        with("pack", json!({})),
        with("rdy", json!("not a version")),
        with("rdy", json!(53)),
        with(
            "gate_source",
            json!({"hash": Hash::of(b"g").to_string(), "kind": 13}),
        ),
        b"[]".to_vec(),
    ] {
        assert_eq!(
            decode_settings(&bad, &token).unwrap_err().code,
            E_UPSTREAM_PROTOCOL
        );
    }
    let mut big = base.clone();
    big["description"] = json!("d".repeat(KNOWN_REPLY_BYTES));
    assert_eq!(
        decode_settings(big.to_string().as_bytes(), &token)
            .unwrap_err()
            .code,
        E_UPSTREAM_PROTOCOL
    );
    // a member this client does not know is left out
    let mut more = base.clone();
    more["added"] = json!({"x": 1});
    assert_eq!(
        decode_settings(more.to_string().as_bytes(), &token).unwrap(),
        decode_settings(base.to_string().as_bytes(), &token).unwrap()
    );
    assert_eq!(s.finish(), 0);
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// The four v1 steps against rdyhub's frozen fixture: discovery (both cases), state in the
/// publish-upload form (both cases), known in the push form (both cases) and put. The client's
/// requests carry the fixture's method, path, query, headers and member names; it decodes each
/// reply. Then fixture v2's steps (8a2): state with views, the pull form of known, get, write
/// and the outcome lookup, with its list of the cases the hub does not serve yet.
#[test]
fn scc8_wire_fixture_matches_state_known_get_put_write() {
    // v1 stays pinned beside v2
    assert_eq!(sha256(FIXTURE), FIXTURE_SHA256);
    let f = fixture();
    assert_eq!(f["fixture"], "sync-client-wire");
    assert_eq!(f["version"], 1);
    assert_eq!(f["media_type"], SYNC_MEDIA_TYPE);
    let served_steps: BTreeSet<&str> = f["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(
        served_steps,
        BTreeSet::from(["discovery", "known", "put", "state"])
    );

    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let origin = s.origin().to_string();

    // discovery: a readable repo, then a missing one
    let [found, missing] = <[Value; 2]>::try_from(steps(&f, "discovery")).unwrap();
    let cx = hub.cx_with(None);
    let path = |st: &Value| format!("{origin}{}", st["request"]["path"].as_str().unwrap());
    let (r, req) = served(&mut s, reply_of(&found), || {
        hub.wire.discovery(&cx, &path(&found))
    });
    matches_request(&req, &found);
    assert!(req.body.is_empty());
    let d = r.unwrap();
    assert_eq!(d.sync_base, format!("{origin}/sync{REPO_PATH}"));
    assert_eq!(d.protocols, [1]);
    let (r, req) = served(&mut s, reply_of(&missing), || {
        hub.wire.discovery(&cx, &path(&missing))
    });
    matches_request(&req, &missing);
    matches_refusal(&err(r), &missing);

    // state: the publish-upload form, no views
    let cx = hub.cx_with(Some(d.sync_base.clone()));
    let request = SyncStateRequest::new(&cx.claim);
    let [empty, too_old] = <[Value; 2]>::try_from(steps(&f, "state")).unwrap();
    let (r, req) = served(&mut s, reply_of(&empty), || hub.wire.state(&cx, &request));
    matches_request(&req, &empty);
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    let want = &empty["request"]["body"];
    assert_eq!(object_keys(&sent), object_keys(want));
    assert_eq!(sent["actor"], want["actor"]);
    assert_eq!(sent["run"], want["run"]);
    assert_eq!(sent["site"], Value::Null);
    // the client's own values: this rdy and Plan A's format 4 (fixture v2 settles them)
    assert_eq!(sent["rdy"], rdy_engine::formats::formats().rdy);
    assert_eq!(sent["stream_read"], json!([HISTORY_FORMAT]));
    assert_eq!(sent["stream_write"], json!([HISTORY_FORMAT]));
    let st = r.unwrap();
    assert_eq!(
        (st.canon, st.repo_id, st.min_rdy.clone()),
        (None, None, None)
    );
    assert_eq!(st.quarantine_seconds, 3600);
    assert_eq!(
        (st.batch_bytes, st.batch_records),
        (1_073_741_824, 1_000_000)
    );
    assert_eq!(st.stream_read, [2]);
    let (r, req) = served(&mut s, reply_of(&too_old), || hub.wire.state(&cx, &request));
    matches_request(&req, &too_old);
    matches_refusal(&err(r), &too_old);

    // known, push form: the client sends its keys sorted and unique
    let [ok, bad] = <[Value; 2]>::try_from(steps(&f, "known")).unwrap();
    let want = &ok["request"]["body"];
    let keys: Vec<ObjectKey> = want["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| ObjectKey {
            kind: k["kind"].as_u64().unwrap() as u8,
            hash: k["hash"].as_str().unwrap().parse().unwrap(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let push = want["push"].as_str().unwrap();
    let (r, req) = served(&mut s, reply_of(&ok), || hub.wire.known(&cx, push, &keys));
    matches_request(&req, &ok);
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(object_keys(&sent), object_keys(want));
    assert_eq!(sent["push"], want["push"]);
    for k in sent["keys"].as_array().unwrap() {
        assert_eq!(
            object_keys(k),
            BTreeSet::from(["hash".into(), "kind".into()])
        );
    }
    assert_eq!(r.unwrap(), keys);
    // a push id of the wrong form: the client refuses it before sending, in the hub's words
    let bad_push = bad["request"]["body"]["push"].as_str().unwrap();
    let e = err(unsent(&[&s], || hub.wire.known(&cx, bad_push, &[])));
    let body = &bad["reply"]["body"];
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            body["message"].as_str().unwrap(),
            body["fix"].as_str().unwrap()
        )
    );
    // the hub's refusal itself maps as the fixture gives it
    let (r, _) = served(&mut s, reply_of(&bad), || hub.wire.known(&cx, PUSH, &keys));
    matches_refusal(&err(r), &bad);

    // put: one binary batch under the push id
    let [put] = <[Value; 1]>::try_from(steps(&f, "put")).unwrap();
    let len = put["request"]["body"]["bytes"].as_u64().unwrap();
    let batch = vec![0u8; len as usize];
    let (r, req) = served(&mut s, reply_of(&put), || {
        hub.wire.put(&cx, PUSH, &mut &batch[..], len)
    });
    matches_request(&req, &put);
    assert_eq!(req.body.len() as u64, len);
    assert_eq!(
        r.unwrap(),
        ObjectPut {
            records: 10,
            bytes: 3485
        }
    );
    assert_eq!(s.finish(), 0);

    fixture_v2_steps();
}

/// D-H0BZN, D-5Y8Y1: no request leaves before discovery passes the origin fence, and every
/// later request (state, both known forms, put, get, write and outcome) goes to the fenced sync
/// base only.
#[test]
fn scc8_discovery_fence_precedes_every_request() {
    let mut a = Server::start();
    let pinned = Server::start();
    let other = Server::start();
    let hub = Hub::new(&a, &[&pinned], &[&pinned, &other]);
    let cx = hub.cx_with(None);

    // a remote on an unpinned origin: refused before any connection
    let e = err(unsent(&[&a, &other], || {
        hub.wire
            .discovery(&cx, &format!("{}{REPO_PATH}", other.origin()))
    }));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);

    // a sync base on an unpinned origin: refused, and that origin sees nothing
    let reply = format!(
        r#"{{"sync_base":"{}/sync","protocols":[1]}}"#,
        other.origin()
    );
    let at_a = format!("{}{REPO_PATH}", a.origin());
    let (r, _) = served(&mut a, Reply::json(200, &reply), || {
        hub.wire.discovery(&cx, &at_a)
    });
    assert_eq!(err(r).code, E_UPSTREAM_ORIGIN);
    assert_eq!(other.accepted(), 0);

    // no discovered sync base: state, known and put refuse before connecting
    let request = SyncStateRequest::new(&cx.claim);
    let servers = [&a, &pinned, &other];
    err(unsent(&servers, || hub.wire.state(&cx, &request)));
    err(unsent(&servers, || hub.wire.known(&cx, PUSH, &[key(1, 1)])));
    err(unsent(&servers, || {
        hub.wire.put(&cx, PUSH, &mut &b"x"[..], 1)
    }));
    // v2: the pull form, get, write and outcome too
    let root = key(kinds::LANDING, 1);
    let pull = KnownPull {
        after: None,
        base: None,
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let get = GetRequest {
        keys: vec![key(1, 1)],
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let write = a_write();
    err(unsent(&servers, || hub.wire.known_pull(&cx, &pull)));
    err(unsent(&servers, || {
        hub.wire.get(&cx, &get, &mut Vec::new(), 1 << 20)
    }));
    assert!(matches!(
        unsent(&servers, || hub.wire.write(&cx, &write)),
        Err(WriteFailure::NotSent(_))
    ));
    err(unsent(&servers, || hub.wire.outcome(&cx, PUSH)));

    // a sync base outside the fence that skipped discovery: refused before connecting
    let stray = hub.cx_with(Some(format!("{}/sync", other.origin())));
    let e = err(unsent(&servers, || hub.wire.state(&stray, &request)));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    let e = err(unsent(&servers, || {
        hub.wire.put(&stray, PUSH, &mut &b"x"[..], 1)
    }));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    for e in [
        err(unsent(&servers, || hub.wire.known_pull(&stray, &pull))),
        err(unsent(&servers, || {
            hub.wire.get(&stray, &get, &mut Vec::new(), 1 << 20)
        })),
        err(unsent(&servers, || hub.wire.outcome(&stray, PUSH))),
        match unsent(&servers, || hub.wire.write(&stray, &write)) {
            Err(WriteFailure::NotSent(e)) => *e,
            other => panic!("{other:?}"),
        },
    ] {
        assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    }
    assert_eq!(a.finish(), 0);
}

/// D-N8PR2: discovery, state, both known forms, put, write and outcome replies ignore unknown
/// members, and a token in any key or string, ignored member or not, discards the reply (a
/// write's reply so discarded is uncertain).
#[test]
fn scc8_additive_replies_scan_hidden_tokens() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let base_cx = hub.cx_with(None);
    let cx = hub.cx();
    let request = SyncStateRequest::new(&cx.claim);
    let remote = format!("{}{REPO_PATH}", s.origin());
    let keys = [key(1, 1), key(1, 2)];
    let missing = format!(r#"{{"hash":"{}","kind":1}}"#, key(1, 2).hash);
    let replies = |extra: &str| {
        [
            format!(r#"{{"sync_base":"/sync{REPO_PATH}","protocols":[1]{extra}}}"#),
            {
                let mut v: Value = serde_json::from_str(&state_reply(json!({}))).unwrap();
                let added: Value = serde_json::from_str(&format!("{{\"_\":0{extra}}}")).unwrap();
                for (k, m) in added.as_object().unwrap() {
                    v[k] = m.clone();
                }
                v.to_string()
            },
            format!(r#"{{"missing":[{missing}]{extra}}}"#),
            format!(r#"{{"records":1,"bytes":1{extra}}}"#),
        ]
    };
    let run = |s: &mut Server, bodies: [String; 4]| -> Vec<Result<(), RdyError>> {
        let [d, st, k, p] = bodies;
        vec![
            served(s, Reply::json(200, &d), || {
                hub.wire.discovery(&base_cx, &remote).map(|_| ())
            })
            .0,
            served(s, Reply::json(200, &st), || {
                hub.wire.state(&cx, &request).map(|_| ())
            })
            .0,
            served(s, Reply::json(200, &k), || {
                hub.wire.known(&cx, PUSH, &keys).map(|_| ())
            })
            .0,
            served(s, Reply::json(200, &p), || {
                hub.wire.put(&cx, PUSH, &mut &b"x"[..], 1).map(|_| ())
            })
            .0,
        ]
    };
    // unknown members, deep and wide: ignored
    for r in run(
        &mut s,
        replies(r#","later":{"nested":[1,"text",{"k":null}]},"flag":true"#),
    ) {
        r.unwrap();
    }
    // the bearer or a token prefix in an ignored member's string, or as a key
    for extra in [
        format!(r#","note":"{ACCESS}""#),
        r#","note":{"deep":["rdyh_r_x"]}"#.to_string(),
        r#","rdyh_a_key":1"#.to_string(),
        r#","note":"rdyh_a_escaped""#.to_string(),
    ] {
        let got = run(&mut s, replies(&extra));
        let codes: Vec<&str> = got.iter().map(|r| err(r.clone()).code).collect();
        assert_eq!(
            codes,
            [
                E_UPSTREAM_PROTOCOL,
                E_UPSTREAM_PROTOCOL,
                E_UPSTREAM_PROTOCOL,
                E_UPSTREAM_UNCERTAIN
            ],
            "{extra}"
        );
    }

    // v2: the pull form's page, the write reply and the outcome reply
    let root = key(kinds::LANDING, 1);
    let pull = KnownPull {
        after: None,
        base: None,
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let write = a_write();
    let v2 = |extra: &str| {
        [
            format!(r#"{{"keys":[],"next":null{extra}}}"#),
            format!(
                r#"{{"min_rdy_raised":false,"outcome":"taken","request":null,"tip":{{"hash":"{}","kind":4}}{extra}}}"#,
                root.hash
            ),
            format!(
                r#"{{"code":null,"kind":null,"outcome":"unknown","request":null,"request_state":null,"tip":null,"view":null{extra}}}"#
            ),
        ]
    };
    let run2 = |s: &mut Server, bodies: [String; 3]| -> Vec<Option<&'static str>> {
        let [k, w, o] = bodies;
        vec![
            served(s, Reply::json(200, &k), || hub.wire.known_pull(&cx, &pull))
                .0
                .err()
                .map(|e| e.code),
            match served(s, Reply::json(200, &w), || hub.wire.write(&cx, &write)).0 {
                Ok(_) => None,
                Err(WriteFailure::Uncertain) => Some(E_UPSTREAM_UNCERTAIN),
                Err(other) => panic!("{other:?}"),
            },
            served(s, Reply::json(200, &o), || hub.wire.outcome(&cx, PUSH))
                .0
                .err()
                .map(|e| e.code),
        ]
    };
    assert_eq!(
        run2(
            &mut s,
            v2(r#","later":{"nested":[1,"text",{"k":null}]},"flag":true"#)
        ),
        [None, None, None]
    );
    for extra in [
        format!(r#","note":"{ACCESS}""#),
        r#","note":{"deep":["rdyh_r_x"]}"#.to_string(),
        r#","rdyh_a_key":1"#.to_string(),
    ] {
        assert_eq!(
            run2(&mut s, v2(&extra)),
            [
                Some(E_UPSTREAM_PROTOCOL),
                Some(E_UPSTREAM_UNCERTAIN),
                Some(E_UPSTREAM_PROTOCOL)
            ],
            "{extra}"
        );
    }
    assert_eq!(s.finish(), 0);
}

/// D-4XHPH: a state `min_rdy` above this rdy stops the exchange, push or pull, before any known,
/// put, known pull or get request; the feature check reads the real feature registry, not an
/// empty list.
#[test]
fn scc8_minimum_stops_before_transfer() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let request = SyncStateRequest::new(&cx.claim);
    let (r, _) = served(
        &mut s,
        Reply::json(200, &state_reply(json!({"min_rdy": "999.0.0"}))),
        || hub.wire.state(&cx, &request),
    );
    let st = r.unwrap();
    let e = err(check_minimum(&st, "acme/docs"));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_STORE_FORMAT,
            "hub repo acme/docs needs rdy 999.0.0",
            "update rdy to 999.0.0 or later"
        )
    );
    // this rdy, and an older one, pass
    let mut st = st;
    st.min_rdy = Some(rdy_engine::formats::formats().rdy);
    check_minimum(&st, "acme/docs").unwrap();
    st.min_rdy = Some("0.1.0".into());
    check_minimum(&st, "acme/docs").unwrap();
    st.min_rdy = None;
    check_minimum(&st, "acme/docs").unwrap();

    // every registered feature is known, whatever its class; an unknown incompatible one is not
    let registered: Vec<String> = rdy_store::format::registered_features()
        .into_iter()
        .map(|f| f.name.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(!registered.is_empty());
    let features: Vec<Value> = registered
        .iter()
        .map(|n| json!({"name": n, "class": "incompatible"}))
        .collect();
    let (r, _) = served(
        &mut s,
        Reply::json(200, &state_reply(json!({"features": features}))),
        || hub.wire.state(&cx, &request),
    );
    assert_eq!(r.unwrap().features.len(), registered.len());
    let (r, _) = served(
        &mut s,
        Reply::json(
            200,
            &state_reply(
                json!({"features": [{"name": "zz-not-registered", "class": "incompatible"}]}),
            ),
        ),
        || hub.wire.state(&cx, &request),
    );
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(
        &mut s,
        Reply::json(
            200,
            &state_reply(
                json!({"features": [{"name": "zz-not-registered", "class": "compatible"}]}),
            ),
        ),
        || hub.wire.state(&cx, &request),
    );
    r.unwrap();
    // only the state requests reached the hub
    assert!(s.requests().iter().all(|r| r.target.ends_with("/state")));
    assert_eq!(s.finish(), 0);

    // the exchange: a pull stops after state too, before any known pull or get
    let e = fake_env(1);
    let fake = sync_env::Hub::new().with_state(|st| st.min_rdy = Some("999.0.0".into()));
    let x = err(sync_env::Sync::new(&e, &fake).pull(PullRequest::default()));
    assert_eq!(
        (x.code, x.message.as_str()),
        (E_STORE_FORMAT, "hub repo acme/docs needs rdy 999.0.0")
    );
    assert_eq!(fake.steps(), ["discovery", "state"]);
}

/// Bounds, redirects, type, size and status checks on every step: known takes at most 100,000
/// keys in at most 16,777,216 bytes; replies take 1,048,576 bytes (known's 16,777,216) and depth
/// 32; a pull page at most 100,000 distinct keys ending at `next`; a get reply at most its
/// limit (the hub's `batch_bytes`); a write body at most 16,777,216 bytes. A redirect is never
/// followed.
#[test]
fn scc8_wire_bounds_and_redirects_fail_closed() {
    let mut s = Server::start();
    let other = Server::start();
    let hub = Hub::new(&s, &[], &[&other]);
    let base_cx = hub.cx_with(None);
    let cx = hub.cx();
    let request = SyncStateRequest::new(&cx.claim);
    let remote = format!("{}{REPO_PATH}", s.origin());
    assert_eq!(MAX_KNOWN_KEYS, 100_000);
    assert_eq!(REPLY_BYTES, 1_048_576);
    assert_eq!(KNOWN_REPLY_BYTES, 16_777_216);
    assert_eq!(MAX_JSON_DEPTH, 32);

    // known: 100,001 keys never leave; unsorted keys never leave
    let many: Vec<ObjectKey> = (0..=MAX_KNOWN_KEYS as u32)
        .map(|i| ObjectKey {
            kind: 1,
            hash: Hash::of(&i.to_be_bytes()),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let e = err(unsent(&[&s], || hub.wire.known(&cx, PUSH, &many)));
    assert_eq!(e.code, E_LIMIT);
    let e = err(unsent(&[&s], || {
        hub.wire.known(&cx, PUSH, &[key(1, 2), key(1, 1)])
    }));
    assert_eq!(e.code, E_BAD_INPUT);
    // 100,000 keys go in one request within 16,777,216 bytes
    let (r, req) = served(&mut s, Reply::json(200, r#"{"missing":[]}"#), || {
        hub.wire.known(&cx, PUSH, &many[..MAX_KNOWN_KEYS])
    });
    assert_eq!(r.unwrap(), []);
    assert!(req.body.len() <= 16_777_216, "{}", req.body.len());

    // a reply one byte over its bound
    let pad = |n: usize| " ".repeat(n);
    let big = r#"{"sync_base":"/sync","protocols":[1]}"#;
    let over = format!("{big}{}", pad(REPLY_BYTES + 1 - big.len()));
    let (r, _) = served(&mut s, Reply::json(200, &over), || {
        hub.wire.discovery(&base_cx, &remote)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let st = state_reply(json!({}));
    let over = format!("{st}{}", pad(REPLY_BYTES + 1 - st.len()));
    let (r, _) = served(&mut s, Reply::json(200, &over), || {
        hub.wire.state(&cx, &request)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let k = r#"{"missing":[]}"#;
    let over = format!("{k}{}", pad(KNOWN_REPLY_BYTES + 1 - k.len()));
    let (r, _) = served(&mut s, Reply::json(200, &over), || {
        hub.wire.known(&cx, PUSH, &[key(1, 1)])
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let p = r#"{"records":1,"bytes":1}"#;
    let over = format!("{p}{}", pad(REPLY_BYTES + 1 - p.len()));
    let (r, _) = served(&mut s, Reply::json(200, &over), || {
        hub.wire.put(&cx, PUSH, &mut &b"x"[..], 1)
    });
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);

    // depth 33 in an ignored member
    let deep = format!(
        "{}1{}",
        "[".repeat(MAX_JSON_DEPTH),
        "]".repeat(MAX_JSON_DEPTH)
    );
    let st = state_reply(json!({})).replacen('{', &format!("{{\"deep\":{deep},"), 1);
    let (r, _) = served(&mut s, Reply::json(200, &st), || {
        hub.wire.state(&cx, &request)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);

    // a wrong type, a missing member, a stray success status
    let (r, _) = served(
        &mut s,
        Reply::json(200, &state_reply(json!({"batch_bytes": "big"}))),
        || hub.wire.state(&cx, &request),
    );
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, Reply::json(200, r#"{"records":1}"#), || {
        hub.wire.put(&cx, PUSH, &mut &b"x"[..], 1)
    });
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);
    let (r, _) = served(&mut s, Reply::json(204, r#"{"missing":[]}"#), || {
        hub.wire.known(&cx, PUSH, &[key(1, 1)])
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    // a missing key the request did not name
    let stray = format!(
        r#"{{"missing":[{{"hash":"{}","kind":1}}]}}"#,
        key(1, 9).hash
    );
    let (r, _) = served(&mut s, Reply::json(200, &stray), || {
        hub.wire.known(&cx, PUSH, &[key(1, 1)])
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);

    // redirects: never followed, on every step
    let location = format!("location: {}/sync", other.origin());
    let redirect =
        || Reply::with_headers(302, &[location.clone(), "content-length: 0".into()], b"");
    let (r, _) = served(&mut s, redirect(), || hub.wire.discovery(&base_cx, &remote));
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, redirect(), || hub.wire.state(&cx, &request));
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, redirect(), || {
        hub.wire.known(&cx, PUSH, &[key(1, 1)])
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, redirect(), || {
        hub.wire.put(&cx, PUSH, &mut &b"x"[..], 1)
    });
    assert_eq!(err(r).code, E_UPSTREAM_UNCERTAIN);
    // v2 bounds: a pull page over 100,000 keys; a get body over its limit; a write body over
    // 16,777,216 bytes never leaves; write and outcome replies over 1,048,576 bytes
    let root = key(kinds::LANDING, 1);
    let pull = KnownPull {
        after: None,
        base: None,
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let page: Vec<Value> = (0..=MAX_KNOWN_KEYS as u32)
        .map(|i| json!({"hash": Hash::of(&i.to_be_bytes()).to_string(), "kind": 1}))
        .collect();
    let body = json!({"keys": page, "next": null}).to_string();
    assert!(body.len() <= KNOWN_REPLY_BYTES);
    let (r, _) = served(&mut s, Reply::json(200, &body), || {
        hub.wire.known_pull(&cx, &pull)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    // a page whose next is not its last key, or that repeats a key
    let k1 = json!({"hash": key(1, 1).hash.to_string(), "kind": 1});
    let k2 = json!({"hash": key(1, 2).hash.to_string(), "kind": 1});
    for bad in [
        json!({"keys": [k1, k2], "next": k1}),
        json!({"keys": [k1, k1], "next": null}),
    ] {
        let (r, _) = served(&mut s, Reply::json(200, &bad.to_string()), || {
            hub.wire.known_pull(&cx, &pull)
        });
        assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    }
    let get = GetRequest {
        keys: vec![key(1, 1)],
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let octets = |n: usize| {
        Reply::with_headers(
            200,
            &[
                "content-type: application/octet-stream".into(),
                format!("content-length: {n}"),
                "connection: close".into(),
            ],
            &vec![1u8; n],
        )
    };
    let mut sink = Vec::new();
    let (r, _) = served(&mut s, octets(11), || {
        hub.wire.get(&cx, &get, &mut sink, 10)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    assert!(sink.len() <= 10);
    let (r, _) = served(&mut s, octets(10), || {
        hub.wire.get(&cx, &get, &mut Vec::new(), 10)
    });
    assert_eq!(r.unwrap(), 10);
    // the reply streams piece by piece, each piece written while the bytes so far are within
    // the limit: with one piece of room past a cap, a reply past the cap always shows its first
    // byte past the cap to the sink (the fetch splits a batch on it, final review F2)
    let piece = rdy_engine::hub_http::UPLOAD_BUFFER_BYTES;
    let cap = 100usize;
    let mut sink = Vec::new();
    let (r, _) = served(&mut s, octets(cap + 3 * piece), || {
        hub.wire.get(&cx, &get, &mut sink, (cap + piece) as u64)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    assert!(
        sink.len() > cap && sink.len() <= cap + piece,
        "{}",
        sink.len()
    );
    // a get of no key or of more than 100,000 keys never leaves
    let none = GetRequest {
        keys: vec![],
        ..get.clone()
    };
    assert_eq!(
        err(unsent(&[&s], || hub.wire.get(
            &cx,
            &none,
            &mut Vec::new(),
            10
        )))
        .code,
        E_LIMIT
    );
    // a write body past its bound: tags the hub lacks, each a long name
    assert_eq!(WRITE_REQUEST_BYTES, 16_777_216);
    let mut big = a_write();
    big.tags = (0..17)
        .map(|i| WriteTag {
            name: format!("{i}{}", "t".repeat(1 << 20)),
            disagreements: vec![],
        })
        .collect();
    match unsent(&[&s], || hub.wire.write(&cx, &big)) {
        Err(WriteFailure::NotSent(e)) => assert_eq!(e.code, E_LIMIT),
        other => panic!("{other:?}"),
    }
    let w = format!(
        r#"{{"min_rdy_raised":false,"outcome":"taken","request":null,"tip":{{"hash":"{}","kind":4}}}}"#,
        root.hash
    );
    let over = format!("{w}{}", pad(REPLY_BYTES + 1 - w.len()));
    match served(&mut s, Reply::json(200, &over), || {
        hub.wire.write(&cx, &a_write())
    })
    .0
    {
        Err(WriteFailure::Uncertain) => {}
        other => panic!("{other:?}"),
    }
    let o = r#"{"code":null,"kind":null,"outcome":"unknown","request":null,"request_state":null,"tip":null,"view":null}"#;
    let over = format!("{o}{}", pad(REPLY_BYTES + 1 - o.len()));
    let (r, _) = served(&mut s, Reply::json(200, &over), || {
        hub.wire.outcome(&cx, PUSH)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    // a held reply to a view write, a taken reply with no tip, a tip of another kind: uncertain
    let mut view = a_write();
    view.kind = WriteKind::View;
    view.gate = None;
    view.view = Some("drafts".into());
    for (req, body) in [
        (
            view.clone(),
            r#"{"min_rdy_raised":false,"outcome":"held","request":{"number":1,"version":1},"tip":null}"#.to_string(),
        ),
        (
            a_write(),
            r#"{"min_rdy_raised":false,"outcome":"taken","request":null,"tip":null}"#.to_string(),
        ),
        (
            a_write(),
            format!(
                r#"{{"min_rdy_raised":false,"outcome":"taken","request":null,"tip":{{"hash":"{}","kind":13}}}}"#,
                root.hash
            ),
        ),
    ] {
        match served(&mut s, Reply::json(200, &body), || hub.wire.write(&cx, &req)).0 {
            Err(WriteFailure::Uncertain) => {}
            other => panic!("{body}: {other:?}"),
        }
    }
    // an outcome with a request state outside open, landed and closed
    let o = r#"{"code":null,"kind":"landings","outcome":"held","request":{"number":1,"version":1},"request_state":"lost","tip":null,"view":null}"#;
    let (r, _) = served(&mut s, Reply::json(200, o), || hub.wire.outcome(&cx, PUSH));
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    // redirects: never followed on the v2 steps either
    let (r, _) = served(&mut s, redirect(), || hub.wire.known_pull(&cx, &pull));
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, redirect(), || {
        hub.wire.get(&cx, &get, &mut Vec::new(), 10)
    });
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    let (r, _) = served(&mut s, redirect(), || hub.wire.outcome(&cx, PUSH));
    assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL);
    match served(&mut s, redirect(), || hub.wire.write(&cx, &a_write())).0 {
        Err(WriteFailure::Uncertain) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(other.accepted(), 0);
    assert_eq!(s.finish(), 0);
}

/// D-H0BZN "State": the request names the views of this exchange, in order, and at most 256 of
/// them, each once; the reply gives each named view's head and root (both view ops, or both
/// null) in that order and nothing else, and a push names only its selected views.
#[test]
fn scc8_state_captures_only_named_heads_and_bounds_256() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let names: Vec<String> = vec!["main".into(), "drafts".into()];
    let two = SyncStateRequest::new(&cx.claim).with_views(&names);
    let op = |n: u8| json!({"hash": Hash::of(&[n]).to_string(), "kind": kinds::VIEW_OP});
    let with_views = |views: Value| state_reply(json!({ "views": views }));
    let (r, req) = served(
        &mut s,
        Reply::json(
            200,
            &with_views(json!([
                {"name": "main", "head": op(2), "root": op(1)},
                {"name": "drafts", "head": null, "root": null}
            ])),
        ),
        || hub.wire.state(&cx, &two),
    );
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(sent["views"], json!(["main", "drafts"]));
    let st = r.unwrap();
    assert_eq!(st.view_head("main"), Some(Hash::of(&[2])));
    assert_eq!(st.view_head("drafts"), None);
    assert_eq!(st.view_head("other"), None);
    let hub_views = st.hub_views();
    assert_eq!(hub_views.len(), 1);
    assert_eq!(hub_views["main"].root, Hash::of(&[1]));
    // no views named: no member sent
    let plain = SyncStateRequest::new(&cx.claim);
    let (r, req) = served(&mut s, Reply::json(200, &state_reply(json!({}))), || {
        hub.wire.state(&cx, &plain)
    });
    r.unwrap();
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert!(sent.get("views").is_none());
    // replies outside the form: another order, a missing or extra view, half a head, another kind
    for views in [
        json!([{"name": "drafts", "head": null, "root": null}, {"name": "main", "head": null, "root": null}]),
        json!([{"name": "main", "head": null, "root": null}]),
        json!([
            {"name": "main", "head": null, "root": null},
            {"name": "drafts", "head": null, "root": null},
            {"name": "extra", "head": null, "root": null}
        ]),
        json!([{"name": "main", "head": op(2), "root": null}, {"name": "drafts", "head": null, "root": null}]),
        json!([
            {"name": "main", "head": {"hash": Hash::of(&[2]).to_string(), "kind": kinds::LANDING}, "root": op(1)},
            {"name": "drafts", "head": null, "root": null}
        ]),
    ] {
        let (r, _) = served(&mut s, Reply::json(200, &with_views(views.clone())), || {
            hub.wire.state(&cx, &two)
        });
        assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL, "{views}");
    }
    // 256 names go; 257, or a name twice, never leave
    assert_eq!(MAX_STATE_VIEWS, 256);
    let most: Vec<String> = (0..MAX_STATE_VIEWS).map(|i| format!("v{i}")).collect();
    let views: Vec<Value> = most
        .iter()
        .map(|n| json!({"name": n, "head": null, "root": null}))
        .collect();
    let (r, _) = served(&mut s, Reply::json(200, &with_views(json!(views))), || {
        hub.wire
            .state(&cx, &SyncStateRequest::new(&cx.claim).with_views(&most))
    });
    assert_eq!(r.unwrap().views.len(), MAX_STATE_VIEWS);
    let mut over = most.clone();
    over.push("one-more".into());
    let e = err(unsent(&[&s], || {
        hub.wire
            .state(&cx, &SyncStateRequest::new(&cx.claim).with_views(&over))
    }));
    assert_eq!(e.code, E_LIMIT);
    let twice: Vec<String> = vec!["main".into(), "main".into()];
    let e = err(unsent(&[&s], || {
        hub.wire
            .state(&cx, &SyncStateRequest::new(&cx.claim).with_views(&twice))
    }));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(s.finish(), 0);

    // the exchange names only the push's selected views, and refuses 257 before any request
    let e = fake_env(1);
    sync_env::write_in(&e.engine, "alpha", "a1");
    sync_env::write_in(&e.engine, "beta", "b1");
    let fake = sync_env::Hub::new();
    let sync = sync_env::Sync::new(&e, &fake);
    sync.push(rdy_engine::hub_sync::PushRequest {
        views: vec!["beta".into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        fake.states.lock().unwrap()[0].views,
        Some(vec!["beta".to_string()])
    );
    fake.clear();
    let x = err(sync.push(rdy_engine::hub_sync::PushRequest {
        views: over,
        ..Default::default()
    }));
    assert_eq!(x.code, E_LIMIT);
    assert!(fake.steps().is_empty());
}

/// D-H0BZN "Pull known": pages follow `next` until null and keep the hub's order; the first
/// pull sends no base, and a later one sends the store's fetched tip once the store holds it.
#[test]
fn scc8_known_pages_in_export_order_from_fetched_base() {
    let e = fake_env(2);
    let (_dir, hub_store) = sync_env::copy_project(&e);
    let later = sync_env::land_in(&hub_store, "later");
    let fake = sync_env::Hub::new();
    fake.serve(hub_store.read());
    fake.state.lock().unwrap().canon = Some(later);
    *fake.page.lock().unwrap() = 2;
    let sync = sync_env::Sync::new(&e, &fake);
    sync.pull(PullRequest::default()).unwrap();
    let pulls = fake.pulls.lock().unwrap().clone();
    assert!(pulls.len() > 2, "{pulls:?}");
    let root = ObjectKey {
        kind: kinds::LANDING,
        hash: later,
    };
    assert!(pulls.iter().all(|p| p.root == root && p.base.is_none()));
    assert!(
        pulls
            .iter()
            .all(|p| p.purpose == StreamPurpose::CanonHistory)
    );
    // each page asks after the last key of the one before, in the hub's order
    let order = fake.history(StreamPurpose::CanonHistory, later, None);
    assert_eq!(pulls[0].after, None);
    for (i, p) in pulls.iter().enumerate().skip(1) {
        assert_eq!(p.after, Some(order[2 * i - 1]), "page {i}");
    }
    // the get asked for the keys this store lacked, in the hub's order
    let gets = fake.gets.lock().unwrap().clone();
    assert_eq!(gets.len(), 1);
    let read = e.engine.read();
    let held_before: Vec<ObjectKey> = order
        .iter()
        .filter(|k| !gets[0].keys.contains(k))
        .copied()
        .collect();
    let want: Vec<ObjectKey> = order
        .iter()
        .filter(|k| !held_before.contains(k))
        .copied()
        .collect();
    assert_eq!(gets[0].keys, want);
    assert!(read.reader().has_kind(kinds::LANDING, &later).unwrap());
    drop(read);
    assert_eq!(
        sync_env::canon_of(&e, "shared").unwrap().fetched,
        Some(later)
    );

    // the next pull sends the fetched tip as its base, and has nothing to get
    fake.clear();
    fake.pulls.lock().unwrap().clear();
    sync.pull(PullRequest::default()).unwrap();
    let pulls = fake.pulls.lock().unwrap().clone();
    assert_eq!(pulls[0].base, Some(root));
    assert!(!fake.steps().contains(&"get"));
}

/// D-H0BZN "Pull known": a key that reaches the hub before the cursor while a pull pages is not
/// in that pull; the next pull, from the fetched tip, fetches it.
#[test]
fn scc8_late_attachment_before_cursor_arrives_next_pull() {
    let e = fake_env(2);
    let (_dir, hub_store) = sync_env::copy_project(&e);
    let later = sync_env::land_in(&hub_store, "later");
    let hub_store = Arc::new(hub_store);
    let fake = Arc::new(sync_env::Hub::new());
    fake.serve(hub_store.read());
    fake.state.lock().unwrap().canon = Some(later);
    *fake.page.lock().unwrap() = 2;
    let tag_key: Arc<std::sync::Mutex<Option<ObjectKey>>> = Arc::default();
    {
        // at the first known pull, arm a second hook: before the second page, a tag on the hub
        // tip lands at the front of the hub's order, before the cursor
        let (fake2, store2, tag2) = (fake.clone(), hub_store.clone(), tag_key.clone());
        *fake.hook.lock().unwrap() = Some((
            "known_pull",
            Box::new(move |_| {
                let (fake3, store3, tag3) = (fake2.clone(), store2.clone(), tag2.clone());
                *fake2.hook.lock().unwrap() = Some((
                    "known_pull",
                    Box::new(move |_| {
                        let tag: Hash = rdy_engine::engine::with_writer_result(&*store3, |r| {
                            let v = r.tag(&sync_env::founder(), "v9.0.0", None, None)?;
                            Ok(v.tag.parse().unwrap())
                        })
                        .unwrap();
                        let k = ObjectKey {
                            kind: kinds::TAG,
                            hash: tag,
                        };
                        fake3.late.lock().unwrap().insert(k);
                        fake3.serve(store3.read());
                        *tag3.lock().unwrap() = Some(k);
                    }),
                ));
            }),
        ));
    }
    let sync = sync_env::Sync::new(&e, &fake);
    sync.pull(PullRequest::default()).unwrap();
    let tag = tag_key
        .lock()
        .unwrap()
        .expect("the tag landed during the pull");
    assert!(fake.pulls.lock().unwrap().len() > 2);
    assert!(
        !e.engine
            .read()
            .reader()
            .has_kind(kinds::TAG, &tag.hash)
            .unwrap()
    );
    assert!(!fake.gets.lock().unwrap()[0].keys.contains(&tag));

    // the next pull: from the fetched tip, the tag is all that is missing
    fake.clear();
    fake.gets.lock().unwrap().clear();
    sync.pull(PullRequest::default()).unwrap();
    let gets = fake.gets.lock().unwrap().clone();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].keys, [tag]);
    assert!(
        e.engine
            .read()
            .reader()
            .has_kind(kinds::TAG, &tag.hash)
            .unwrap()
    );
}

/// D-H0BZN "Get": each batch is checked before anything is taken. A batch of another purpose or
/// root, one past `batch_bytes`, or one that breaks the closure of a landing's snapshot delta
/// (SD1, kind 30) is refused, and the store takes nothing.
#[test]
fn scc8_get_checks_purpose_root_and_sd1_delta_closure() {
    let e = fake_env(2);
    let (_dir, hub_store) = sync_env::copy_project(&e);
    sync_env::write_in(&hub_store, "drafts", "draft-one");
    let later = sync_env::land_in(&hub_store, "later");
    let hub_read = hub_store.read();
    let batch = |purpose: StreamPurpose, root: Hash, known: &BTreeSet<ObjectKey>| {
        let mut out = Vec::new();
        rdy_engine::stream_export::write_history(&hub_read, purpose, root, None, known, &mut out)
            .unwrap();
        out
    };
    let nothing = BTreeSet::new();
    let view = hub_read.view_head("drafts").unwrap().head;
    // the hub's whole history above nothing, less only the tip: its landing needs the snapshot
    // its delta names, which the batch neither carries nor lists
    let all: BTreeSet<ObjectKey> = rdy_store::closure::history_closure_before(
        hub_read.reader(),
        StreamPurpose::CanonHistory,
        later,
        &BTreeSet::new(),
        None,
        hub_read.seq(),
    )
    .unwrap()
    .records
    .into_iter()
    .collect();
    let tip = ObjectKey {
        kind: kinds::LANDING,
        hash: later,
    };
    let less_tip: BTreeSet<ObjectKey> = all.iter().filter(|k| **k != tip).copied().collect();
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "another purpose",
            batch(StreamPurpose::ViewHistory, view, &nothing),
            E_UPSTREAM_PROTOCOL,
        ),
        (
            "another root",
            batch(StreamPurpose::CanonHistory, e.landings[1], &nothing),
            E_UPSTREAM_PROTOCOL,
        ),
        (
            "the snapshot delta's closure",
            batch(StreamPurpose::CanonHistory, later, &less_tip),
            E_BAD_RECORD,
        ),
    ];
    for (case, bytes, code) in cases {
        let fake = sync_env::Hub::new();
        fake.serve(hub_store.read());
        fake.state.lock().unwrap().canon = Some(later);
        *fake.get_bytes.lock().unwrap() = Some(bytes);
        let x = err(sync_env::Sync::new(&e, &fake).pull(PullRequest::default()));
        assert_eq!(x.code, code, "{case}: {x:?}");
        if code == E_BAD_RECORD {
            assert!(
                x.message.contains("unlisted dependency 5:"),
                "{}",
                x.message
            );
        }
        assert!(
            !e.engine
                .read()
                .reader()
                .has_kind(kinds::LANDING, &later)
                .unwrap()
        );
        assert_eq!(sync_env::canon_of(&e, "shared"), None, "{case}");
    }
    // a batch past the hub's batch_bytes: the get's limit
    let fake = sync_env::Hub::new();
    fake.serve(hub_store.read());
    fake.state.lock().unwrap().canon = Some(later);
    fake.state.lock().unwrap().batch_bytes = 64;
    let x = err(sync_env::Sync::new(&e, &fake).pull(PullRequest::default()));
    assert_eq!(x.code, E_UPSTREAM_PROTOCOL);
    assert!(
        !e.engine
            .read()
            .reader()
            .has_kind(kinds::LANDING, &later)
            .unwrap()
    );
    assert_eq!(sync_env::canon_of(&e, "shared"), None);
}

/// D-H0BZN "Get" (final review F2): a fetch batch keeps within both of the hub's limits. The
/// client cannot know a record's bytes before it fetches it, so a get whose reply passes
/// `batch_bytes` is split in two and each half fetched in turn, down to one key; a history that
/// needs several byte-limited batches is fetched whole, canon and view alike (a URL clone fetches
/// through the same step). Each half is checked as every batch is; one key past the limit is
/// still refused (`scc8_get_checks_purpose_root_and_sd1_delta_closure`).
#[test]
fn scc8_fetch_splits_batches_past_batch_bytes() {
    let e = fake_env(2);
    let (_dir, hub_store) = sync_env::copy_project(&e);
    let mut later = e.landings[1];
    for i in 0..20 {
        later = sync_env::land_in(&hub_store, &format!("later-{i}"));
    }
    for i in 0..5 {
        sync_env::write_in(&hub_store, "drafts", &format!("draft-{i}"));
    }
    let hub_read = hub_store.read();
    let drafts = hub_read.view_head("drafts").unwrap().head;
    let drafts_root = hub_read
        .reader()
        .view_chain_root("drafts", &drafts)
        .unwrap();
    let fake = sync_env::Hub::new();
    fake.serve(hub_store.read());
    {
        let mut st = fake.state.lock().unwrap();
        st.canon = Some(later);
        st.batch_bytes = 8_192;
        st.batch_records = 100_000;
    }
    fake.views
        .lock()
        .unwrap()
        .insert("drafts".into(), (drafts_root, drafts));
    let read = e.engine.read();
    let want = |purpose, root| -> Vec<ObjectKey> {
        fake.history(purpose, root, None)
            .into_iter()
            .filter(|k| !read.reader().has_kind(k.kind, &k.hash).unwrap())
            .collect()
    };
    let canon_want = want(StreamPurpose::CanonHistory, later);
    // the view fetch comes after canon's, which brought every record they share
    let view_want: Vec<ObjectKey> = want(StreamPurpose::ViewHistory, drafts)
        .into_iter()
        .filter(|k| !canon_want.contains(k))
        .collect();
    drop(read);
    // the whole history does not fit one batch of 8,192 bytes
    let mut whole = Vec::new();
    rdy_engine::stream_export::write_history(
        &hub_read,
        StreamPurpose::CanonHistory,
        later,
        None,
        &BTreeSet::new(),
        &mut whole,
    )
    .unwrap();
    assert!(whole.len() > 8_192, "{}", whole.len());

    sync_env::Sync::new(&e, &fake)
        .pull(PullRequest {
            views: vec!["drafts".into()],
            ..PullRequest::default()
        })
        .unwrap();
    let gets = fake.gets.lock().unwrap().clone();
    for (purpose, wanted) in [
        (StreamPurpose::CanonHistory, &canon_want),
        (StreamPurpose::ViewHistory, &view_want),
    ] {
        let mine: Vec<&GetRequest> = gets.iter().filter(|g| g.purpose == purpose).collect();
        // the first get asks for every key within batch_records; the halves follow
        assert_eq!(mine[0].keys, *wanted, "{purpose:?}");
        let asked: BTreeSet<ObjectKey> = mine.iter().flat_map(|g| g.keys.clone()).collect();
        let wanted_set: BTreeSet<ObjectKey> = wanted.iter().copied().collect();
        assert_eq!(asked, wanted_set, "{purpose:?}");
        // bounded: each split halves a group, so at most 2n - 1 gets
        assert!(mine.len() < 2 * wanted.len(), "{purpose:?}: {}", mine.len());
        assert!(mine.iter().all(|g| !g.keys.is_empty()));
    }
    assert!(
        gets.iter()
            .filter(|g| g.purpose == StreamPurpose::CanonHistory)
            .count()
            > 2,
        "{gets:?}"
    );
    let read = e.engine.read();
    assert!(read.reader().has_kind(kinds::LANDING, &later).unwrap());
    for k in canon_want.iter().chain(&view_want) {
        assert!(read.reader().has_kind(k.kind, &k.hash).unwrap(), "{k:?}");
    }
    drop(read);
    assert_eq!(
        sync_env::canon_of(&e, "shared").unwrap().fetched,
        Some(later)
    );
    let v = e
        .engine
        .read()
        .reader()
        .sync_view("shared", "drafts")
        .unwrap()
        .unwrap();
    assert_eq!(v.fetched, Some(drafts));
}

/// D-H0BZN "State": hub canon's tip reads from a landing's object key (Share on) and from a hex
/// hash (Share off) alike; a key of another kind, or a malformed one, is a protocol failure.
#[test]
fn scc8_state_canon_reads_object_key_and_hex_forms() {
    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();
    let request = SyncStateRequest::new(&cx.claim);
    let tip = Hash::of(b"hub tip");
    let repo = json!("397fd4b98bae3dfed53504ef0e4f9d91");
    for canon in [
        json!({"hash": tip.to_string(), "kind": kinds::LANDING}),
        json!(tip.to_string()),
    ] {
        let (r, _) = served(
            &mut s,
            Reply::json(200, &state_reply(json!({"canon": canon, "repo_id": repo}))),
            || hub.wire.state(&cx, &request),
        );
        assert_eq!(r.unwrap().canon, Some(tip), "{canon}");
    }
    for canon in [
        json!({"hash": tip.to_string(), "kind": kinds::VIEW_OP}),
        json!({"hash": tip.to_string()}),
        json!({"hash": "not hex", "kind": kinds::LANDING}),
        json!("not hex"),
        json!(4),
    ] {
        let (r, _) = served(
            &mut s,
            Reply::json(200, &state_reply(json!({"canon": canon, "repo_id": repo}))),
            || hub.wire.state(&cx, &request),
        );
        assert_eq!(err(r).code, E_UPSTREAM_PROTOCOL, "{canon}");
    }
    assert_eq!(s.finish(), 0);
}

/// Fixture v3a (rdyhub 972e7867; D-PGQZM, D-H67EN): the held forms. Step 0, the write held as a
/// new landing request (i-B40VF): the client sends the fixture's body (its gate values aside)
/// and reads `held` with the request and the unchanged hub tip. Steps 1 and 2, the outcome of a
/// held push (i-6YB7Y): `held` with its request, kind and tip, and `request_state` `open` or
/// `closed`. Each decoded outcome then drives a pull over the fake hub: `open` keeps `held`,
/// `closed` (the request closed with no landing) clears it. The dump step stays not served.
#[test]
fn scc8_wire_fixture_v3a_held_write_and_outcome() {
    assert_eq!(sha256(FIXTURE_V3A), FIXTURE_V3A_SHA256);
    let f: Value = serde_json::from_slice(FIXTURE_V3A).unwrap();
    assert_eq!(f["fixture"], "sync-client-wire");
    assert_eq!(f["version"], "3a");
    assert_eq!(f["media_type"], SYNC_MEDIA_TYPE);
    let st: Vec<Value> = f["steps"].as_array().unwrap().clone();
    let named: Vec<&str> = st.iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(named, ["write", "outcome", "outcome"]);
    let not_served: Vec<&str> = f["not_served"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["step"].as_str().unwrap())
        .collect();
    assert_eq!(not_served, ["dump"]);

    let mut s = Server::start();
    let hub = Hub::new(&s, &[], &[]);
    let cx = hub.cx();

    // 0: the held write reply
    let w = &st[0];
    let b = &w["request"]["body"];
    let (r, req) = served(&mut s, reply_of(w), || hub.wire.write(&cx, &write_of(b)));
    matches_request(&req, w);
    let sent: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(without(&sent, "gate"), without(b, "gate"));
    assert_eq!(object_keys(&sent["gate"]), object_keys(&b["gate"]));
    let rep = &w["reply"]["body"];
    let got = r.unwrap();
    assert_eq!(
        (got.outcome, got.tip, got.request, got.min_rdy_raised),
        (
            WriteOutcome::Held,
            Some(key_of(&rep["tip"])),
            Some(HubRequest {
                number: 1,
                version: 1
            }),
            false
        )
    );
    assert_eq!(got.tip, Some(key_of(&b["expected"])), "the hub tip stays");

    // 1, 2: the held outcomes, open and closed with no landing
    let mut replies = Vec::new();
    for (i, number, state) in [(1, 1, "open"), (2, 2, "closed")] {
        let o = &st[i];
        let push = o["request"]["body"]["push"].as_str().unwrap();
        let (r, req) = served(&mut s, reply_of(o), || hub.wire.outcome(&cx, push));
        matches_request(&req, o);
        assert_eq!(
            serde_json::from_slice::<Value>(&req.body).unwrap(),
            o["request"]["body"]
        );
        let got = r.unwrap();
        assert_eq!(
            got.outcome,
            LookupOutcome::Held {
                request: HubRequest { number, version: 1 },
                tip: Some(key_of(&o["reply"]["body"]["tip"])),
            },
            "step {i}"
        );
        assert_eq!(got.kind.as_deref(), Some("landings"));
        assert_eq!(got.view, None);
        assert_eq!(got.request_state.as_deref(), Some(state));
        replies.push((state, got));
    }
    assert_eq!(s.finish(), 0);

    // each decoded outcome, as the fake hub's answer for a held push: open keeps held, closed
    // clears it; applied stays at the hub tip
    for (state, reply) in replies {
        let e = fake_env(2);
        let tip = e.landings[0];
        let fake = sync_env::Hub::new().with_state(|x| x.canon = Some(tip));
        fake.plan_write(
            1,
            sync_env::WritePlan::Held {
                number: 9,
                version: 1,
            },
        );
        sync_env::push(&e, &fake, None).unwrap();
        let held = sync_env::canon_of(&e, "shared").unwrap().held.unwrap();
        fake.outcomes
            .lock()
            .unwrap()
            .insert(held.push.clone(), reply);
        let (_dir, hub_store) = sync_env::copy_project(&e);
        fake.serve(hub_store.read());
        let pulled = sync_env::Sync::new(&e, &fake)
            .pull(PullRequest::default())
            .unwrap();
        assert!(pulled.warnings.is_empty());
        let c = sync_env::canon_of(&e, "shared").unwrap();
        let want = (state == "open").then_some(held);
        assert_eq!((c.applied, c.held), (Some(tip), want), "{state}");
    }
}
