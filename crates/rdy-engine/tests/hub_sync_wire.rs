//! scc8 (D-H0BZN, D-N8PR2, D-4XHPH): the sync wire's steps that rdyhub's frozen wire fixture v1
//! covers: discovery, state (the publish-upload form, no views), known (push form) and put.
//! Every test drives the real HTTPS wire against loopback TLS fixtures with generated CAs. The
//! fixture is a byte copy of the hub's file; its SHA-256 is pinned here.
#[path = "support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish_wire::{
    KNOWN_REPLY_BYTES, MAX_JSON_DEPTH, MAX_KNOWN_KEYS, ObjectPut, PublishState, REPLY_BYTES,
    SYNC_MEDIA_TYPE, UploadContext,
};
use rdy_engine::hub_sync::check_minimum;
use rdy_engine::hub_sync_wire::{HttpSyncWire, SyncStateRequest, SyncWire};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_STORE_FORMAT, E_UPSTREAM_ORIGIN, E_UPSTREAM_PROTOCOL,
    E_UPSTREAM_UNCERTAIN, RdyError, Session, SessionKind,
};
use rdy_model::{Hash, Role};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::HISTORY_FORMAT;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const FIXTURE: &[u8] = include_bytes!("../../rdy-qa/fixtures/sync/sync-client-wire-v1.json");
const FIXTURE_SHA256: &str = "18ade8e2c1a301d780270f20fe49fc116110a6aa512baf624d768ef5ba39711f";
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

/// The error the hub's fixture refusal `s` becomes: its code, message and fix, then the
/// request id notice.
#[track_caller]
fn matches_refusal(e: &RdyError, s: &Value) {
    let body = &s["reply"]["body"];
    assert_eq!(e.code, body["code"].as_str().unwrap());
    assert_eq!(e.message, body["message"].as_str().unwrap());
    assert_eq!(e.fix, body["fix"].as_str().unwrap());
    assert_eq!(
        e.issues.last().unwrap().message,
        format!("hub request {}", body["request_id"].as_str().unwrap())
    );
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// The four v1 steps against rdyhub's frozen fixture: discovery (both cases), state in the
/// publish-upload form (both cases), known in the push form (both cases) and put. The client's
/// requests carry the fixture's method, path, query, headers and member names; it decodes each
/// reply. Fixture v2 adds get, write, outcome, the pull form of known and dump (8a2).
#[test]
fn scc8_wire_fixture_matches_state_known_get_put_write() {
    let digest: String = Sha256::digest(FIXTURE)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, FIXTURE_SHA256);
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
    let st: PublishState = r.unwrap();
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
}

/// D-H0BZN, D-5Y8Y1: no request leaves before discovery passes the origin fence, and every
/// later request goes to the fenced sync base only.
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

    // a sync base outside the fence that skipped discovery: refused before connecting
    let stray = hub.cx_with(Some(format!("{}/sync", other.origin())));
    let e = err(unsent(&servers, || hub.wire.state(&stray, &request)));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    let e = err(unsent(&servers, || {
        hub.wire.put(&stray, PUSH, &mut &b"x"[..], 1)
    }));
    assert_eq!(e.code, E_UPSTREAM_ORIGIN);
    assert_eq!(a.finish(), 0);
}

/// D-N8PR2: discovery, state, known and put replies ignore unknown members, and a token in any
/// key or string, ignored member or not, discards the reply.
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
    assert_eq!(s.finish(), 0);
}

/// D-4XHPH: a state `min_rdy` above this rdy stops the exchange before any known or put
/// request; the feature check reads the real feature registry, not an empty list.
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
}

/// Bounds, redirects, type, size and status checks on the four v1 steps: known takes at most
/// 100,000 keys in at most 16,777,216 bytes; replies take 1,048,576 bytes (known's 16,777,216)
/// and depth 32. A redirect is never followed.
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
    assert_eq!(other.accepted(), 0);
    assert_eq!(s.finish(), 0);
}
