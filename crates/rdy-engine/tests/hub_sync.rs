//! scc8 (D-H0BZN, D-3RSCW, D-4XHPH, D-5Y8Y1, D-47HVF, D-0SC9C): the sync exchange through put.
//! The push picks the share remote, passes discovery and the state step, checks the hub's
//! minimum and the base rule, then sends the canon history's missing records under one push id,
//! recording the upload before its first put. Every test injects the clock, the credential
//! transport and the wire; a real project and a real sign-in file stand behind them. The hub
//! refusal mapping runs over the real HTTPS wire against a loopback TLS fixture.
mod common;
#[path = "support/hub_tls.rs"]
mod hub_tls;
#[path = "support/sync_env.rs"]
mod sync_env;

use hub_tls::{Reply, Server};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish_stream::HistorySent;
use rdy_engine::hub_publish_wire::{
    Discovery, ObjectPut, PublishState, SYNC_MEDIA_TYPE, UploadContext,
};
use rdy_engine::hub_sync::{PushOutcome, PushRequest, SyncService};
use rdy_engine::hub_sync_wire::{HttpSyncWire, SyncStateRequest, SyncWire};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::*;
use rdy_model::Hash;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{
    KIND_CANON, SYNC_LOCKED_FIX, SyncPending, SyncStep, SyncUpload, sync_locked_message,
};
use rdy_store::sync_stream::StreamPurpose;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use sync_env::*;

/// A loopback install origin on a port made at run time; nothing connects to it.
fn install() -> &'static str {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    ORIGIN.get_or_init(|| {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("https://127.0.0.1:{}", held.local_addr().unwrap().port())
    })
}

fn env(landings: usize) -> Env {
    env_with(install(), "# person settings\n", Some("shared"), landings)
}

// ---------------------------------------------------------------------------------------------
// The fake hub
// ---------------------------------------------------------------------------------------------

/// One wire request as the fake hub saw it.
#[derive(Clone, Debug)]
struct Call {
    step: &'static str,
    push: Option<String>,
    bearer: String,
    ca_file: Option<PathBuf>,
    sync_base: Option<String>,
    remote: Option<String>,
}

type Hook = Box<dyn FnMut(&'static str) + Send>;

/// A fake hub. Its store holds `held`; each push id has its own quarantine of the keys its
/// puts carried. `fail` fails the n-th request (from 1) of a step; `hook` runs once, at the
/// start of the first request of its step.
struct Hub {
    state: Mutex<PublishState>,
    held: Mutex<BTreeSet<ObjectKey>>,
    quarantine: Mutex<BTreeMap<String, BTreeSet<ObjectKey>>>,
    /// Each push id's quarantine bytes so far.
    bytes: Mutex<BTreeMap<String, u64>>,
    fail: Mutex<BTreeMap<(&'static str, usize), RdyError>>,
    hook: Mutex<Option<(&'static str, Hook)>>,
    calls: Mutex<Vec<Call>>,
    /// Each put's push id and the keys its batch carried, in order.
    puts: Mutex<Vec<(String, Vec<ObjectKey>)>>,
}

fn base_state() -> PublishState {
    PublishState {
        canon: None,
        repo_id: None,
        rdy: "0.53.0".into(),
        stream_read: vec![2, 4],
        stream_write: vec![2, 4],
        features: vec![],
        min_rdy: None,
        batch_bytes: 67_108_864,
        batch_records: 100_000,
        quarantine_seconds: 3_600,
        profile: None,
        audience: None,
        artifact_bytes: 201_326_592,
        artifact_files: 32_000,
    }
}

impl Hub {
    fn new() -> Hub {
        Hub {
            state: Mutex::new(base_state()),
            held: Mutex::default(),
            quarantine: Mutex::default(),
            bytes: Mutex::default(),
            fail: Mutex::default(),
            hook: Mutex::default(),
            calls: Mutex::default(),
            puts: Mutex::default(),
        }
    }

    fn with_state(self, f: impl FnOnce(&mut PublishState)) -> Hub {
        f(&mut self.state.lock().unwrap());
        self
    }

    fn fail_at(&self, step: &'static str, n: usize, e: RdyError) {
        self.fail.lock().unwrap().insert((step, n), e);
    }

    fn steps(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().iter().map(|c| c.step).collect()
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn sent_keys(&self) -> Vec<ObjectKey> {
        self.puts
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, k)| k.clone())
            .collect()
    }

    /// Records the request, runs the hook of its step once, then fails it when asked.
    fn enter(
        &self,
        step: &'static str,
        cx: &UploadContext,
        push: Option<&str>,
        remote: Option<&str>,
    ) -> Result<(), RdyError> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call {
                step,
                push: push.map(str::to_string),
                bearer: cx.bearer.expose().to_string(),
                ca_file: cx.ca_file.clone(),
                sync_base: cx.sync_base.clone(),
                remote: remote.map(str::to_string),
            });
            calls.iter().filter(|c| c.step == step).count()
        };
        let hook = {
            let mut h = self.hook.lock().unwrap();
            match h.as_ref() {
                Some((s, _)) if *s == step => h.take(),
                _ => None,
            }
        };
        if let Some((_, mut f)) = hook {
            f(step);
        }
        match self.fail.lock().unwrap().remove(&(step, n)) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl SyncWire for Hub {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.enter("discovery", cx, None, Some(remote))?;
        Ok(Discovery {
            sync_base: format!("{}/sync{REPO_PATH}", cx.install),
            protocols: vec![1],
        })
    }

    fn state(
        &self,
        cx: &UploadContext,
        request: &SyncStateRequest,
    ) -> Result<PublishState, RdyError> {
        self.enter("state", cx, None, None)?;
        assert_eq!(request.site, None);
        assert_eq!(request.actor, cx.claim.actor);
        Ok(self.state.lock().unwrap().clone())
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.enter("known", cx, Some(push), None)?;
        let held = self.held.lock().unwrap();
        let q = self.quarantine.lock().unwrap();
        let mine = q.get(push).cloned().unwrap_or_default();
        Ok(keys
            .iter()
            .filter(|k| !held.contains(k) && !mine.contains(k))
            .copied()
            .collect())
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.enter("put", cx, Some(push), None)?;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len);
        let report = stream_check::check_batch(&bytes[..], None).unwrap();
        assert_eq!(report.header.purpose, StreamPurpose::CanonHistory);
        self.puts
            .lock()
            .unwrap()
            .push((push.to_string(), report.keys.clone()));
        let mut q = self.quarantine.lock().unwrap();
        let mine = q.entry(push.to_string()).or_default();
        mine.extend(report.keys.iter().copied());
        let mut bytes = self.bytes.lock().unwrap();
        let total = bytes.entry(push.to_string()).or_default();
        *total += len;
        Ok(ObjectPut {
            records: mine.len() as u64,
            bytes: *total,
        })
    }
}

fn service<'a>(
    e: &Env,
    api: &'a NoApi,
    clock: &'a Clock,
    hub: &'a dyn SyncWire,
) -> SyncService<'a> {
    SyncService::new(&e.project, &e.user_file, api, clock, hub)
}

fn push(e: &Env, hub: &Hub, remote: Option<&str>) -> Result<PushOutcome, RdyError> {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    service(e, &api, &clock, hub).push(
        &e.engine,
        &agent(),
        PushRequest {
            remote: remote.map(str::to_string),
        },
    )
}

#[track_caller]
fn err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Err(e) => e,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

fn upload_of(e: &Env, remote: &str) -> Option<SyncUpload> {
    e.engine.read().reader().sync_upload(remote).unwrap()
}

fn pending_of(e: &Env, remote: &str) -> Option<SyncPending> {
    e.engine.read().reader().sync_pending(remote).unwrap()
}

/// The canon history's records from `head` above `base`, as the store's closure gives them.
fn closure(e: &Env, head: Hash, base: Option<Hash>) -> BTreeSet<ObjectKey> {
    let read = e.engine.read();
    rdy_store::closure::history_closure_before(
        read.reader(),
        StreamPurpose::CanonHistory,
        head,
        &BTreeSet::new(),
        base,
        read.seq(),
    )
    .unwrap()
    .records
    .into_iter()
    .collect()
}

fn repo_of(e: &Env) -> rdy_model::RepoId {
    e.engine.read().reader().repo_id().unwrap().unwrap()
}

/// Writes sync steps for `remote` straight to the store, as a test seed.
fn seed_sync(e: &Env, remote: &str, steps: Vec<SyncStep>) {
    let remote = remote.to_string();
    rdy_engine::engine::with_writer_result(&e.engine, move |r| {
        r.store_mut()
            .write_sync(&remote, steps)
            .map_err(RdyError::from)?;
        r.invalidate();
        Ok(())
    })
    .unwrap();
}

fn transport_failure() -> RdyError {
    RdyError::new(
        E_UPSTREAM_TRANSPORT,
        UPSTREAM_TRANSPORT_MESSAGE,
        UPSTREAM_TRANSPORT_FIX,
    )
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// D-H0BZN's base rule: a null hub tip seeds, a tip local canon holds is the base, and any
/// other tip is `E_DIVERGED` with the exact words before any known or put step. A tip that is
/// local canon's head has nothing to send.
#[test]
fn scc8_no_state_push_seeds_extends_or_diverges() {
    // seed: the whole canon history travels
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let hub = Hub::new();
    let out = push(&e, &hub, None).unwrap();
    let PushOutcome::Uploaded {
        push: id,
        base,
        head: pushed,
        resumed,
        sent,
    } = out
    else {
        panic!("{out:?}")
    };
    assert_eq!((base, pushed, resumed), (None, head, false));
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put"]);
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, None));
    assert_eq!(sent.batches, 1);
    assert_eq!(sent.records, sent_keys.len() as u64);
    // the upload stays for the write step: same push, canon kind, the remote's url
    let u = upload_of(&e, "shared").unwrap();
    assert_eq!(
        (
            u.push.as_str(),
            u.kind,
            u.view.as_deref(),
            u.url.as_str(),
            u.at_ms
        ),
        (id.as_str(), KIND_CANON, None, e.url.as_str(), T0)
    );
    assert_eq!(u.repo_id, repo_of(&e));

    // extend: a hub tip local canon holds is the base; its closure stays home
    let e = env(3);
    let tip = e.landings[0];
    let head = e.landings[2];
    let hub = Hub::new().with_state(|s| {
        s.canon = Some(tip);
    });
    let out = push(&e, &hub, None).unwrap();
    let PushOutcome::Uploaded { base, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(base, Some(tip));
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, Some(tip)));
    assert!(!sent_keys.contains(&ObjectKey {
        kind: rdy_store::kinds::LANDING,
        hash: tip
    }));

    // up to date: the hub tip is the head
    let hub = Hub::new().with_state(|s| s.canon = Some(head));
    assert_eq!(
        push(&e, &hub, None).unwrap(),
        PushOutcome::UpToDate { tip: Some(head) }
    );
    assert_eq!(hub.steps(), ["discovery", "state"]);

    // diverged: a tip local canon does not hold
    let e = env(1);
    let tip = Hash::of(b"a landing only the hub holds");
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    let x = err(push(&e, &hub, None));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_DIVERGED,
            &*format!("local canon does not hold hub tip {tip}"),
            "run rdy pull to replay local landings, then push again"
        )
    );
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(upload_of(&e, "shared"), None);
}

/// D-H0BZN "Resume a put": a put stopped mid-way leaves the upload, and the next push resumes
/// it under the same push id; known names only the keys the quarantine lacks, and only those
/// travel. After the quarantine expires, every key is missing and all travel again.
#[test]
fn scc8_resumed_put_uses_same_push_and_missing_keys_after_expiry() {
    let e = env(3);
    let head = e.landings[2];
    let all = closure(&e, head, None);
    assert!(all.len() > 6, "{}", all.len());
    let hub = Hub::new().with_state(|s| s.batch_records = 3);
    hub.fail_at("put", 2, transport_failure());
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, E_UPSTREAM_TRANSPORT);
    let first = upload_of(&e, "shared").expect("the upload stays to resume");
    let puts = hub.puts.lock().unwrap().clone();
    assert_eq!(puts.len(), 1);
    assert!(puts.iter().all(|(p, _)| *p == first.push));
    let landed: BTreeSet<ObjectKey> = puts[0].1.iter().copied().collect();

    // resume: the same push id, known again, only what the quarantine lacks
    hub.calls.lock().unwrap().clear();
    hub.puts.lock().unwrap().clear();
    let out = push(&e, &hub, None).unwrap();
    let PushOutcome::Uploaded {
        push: id, resumed, ..
    } = out
    else {
        panic!("{out:?}")
    };
    assert_eq!((id.as_str(), resumed), (first.push.as_str(), true));
    let calls = hub.calls();
    assert_eq!(calls[2].step, "known");
    assert!(
        calls
            .iter()
            .filter(|c| c.step == "known" || c.step == "put")
            .all(|c| c.push.as_deref() == Some(first.push.as_str()))
    );
    let resent: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert!(resent.is_disjoint(&landed), "a checked key travels again");
    let union: BTreeSet<ObjectKey> = resent.union(&landed).copied().collect();
    assert_eq!(union, all);
    assert_eq!(upload_of(&e, "shared"), Some(first.clone()));

    // the quarantine expired: every key is missing and all travel again, same push id
    hub.quarantine.lock().unwrap().clear();
    hub.bytes.lock().unwrap().clear();
    hub.calls.lock().unwrap().clear();
    hub.puts.lock().unwrap().clear();
    let out = push(&e, &hub, None).unwrap();
    let PushOutcome::Uploaded { push: id, sent, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(id, first.push);
    let again: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(again, all);
    assert_eq!(sent.records, all.len() as u64);
    assert!(sent.batches > 1, "{sent:?}");
}

/// D-H0BZN, D-3RSCW: a second push on a remote while one runs is `E_LOCKED` with the exact
/// words and sends nothing; two remotes do not block each other; a push never clears another
/// push's `upload` or `pending`; a found `pending` stops before known or put and keeps both refs.
#[test]
fn scc8_sync_serializes_each_remote_and_keeps_pending_ownership() {
    let e = Arc::new(env(1));
    let other = Arc::new(env_with(
        install(),
        "# person settings\n",
        Some("mirror"),
        1,
    ));
    let hub = Arc::new(Hub::new());
    let other_hub = Arc::new(Hub::new());
    let seen: Arc<Mutex<Vec<Result<PushOutcome, RdyError>>>> = Arc::default();
    {
        let (e, other, other_hub, hub2, seen) = (
            e.clone(),
            other.clone(),
            other_hub.clone(),
            hub.clone(),
            seen.clone(),
        );
        *hub.hook.lock().unwrap() = Some((
            "known",
            Box::new(move |_| {
                let before = hub2.calls().len();
                // the same remote: refused before any request
                let same = push(&e, &hub2, Some("shared"));
                assert_eq!(hub2.calls().len(), before, "the second push sent nothing");
                // another project's remote proceeds
                let elsewhere = push(&other, &other_hub, None);
                seen.lock().unwrap().extend([same, elsewhere]);
            }),
        ));
    }
    push(&e, &hub, None).unwrap();
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let [same, elsewhere] = <[_; 2]>::try_from(seen).unwrap();
    let x = err(same);
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (E_LOCKED, &*sync_locked_message("shared"), SYNC_LOCKED_FIX)
    );
    assert_eq!(x.message, "a push or pull with remote shared is running");
    assert!(matches!(elsewhere, Ok(PushOutcome::Uploaded { .. })));
    // once the first push ends, the remote is free again
    assert!(matches!(
        push(&e, &hub, None),
        Ok(PushOutcome::Uploaded { .. })
    ));

    // another push's upload: resumed, never cleared, even when the push fails before a put
    let e = env(1);
    let repo = repo_of(&e);
    let theirs = SyncUpload {
        repo_id: repo,
        url: e.url.clone(),
        push: "0123456789abcdefghjkmnpqrs".into(),
        kind: KIND_CANON,
        view: None,
        at_ms: T0 - 1,
    };
    seed_sync(&e, "shared", vec![SyncStep::Upload(theirs.clone())]);
    let hub = Hub::new();
    hub.fail_at("known", 1, transport_failure());
    err(push(&e, &hub, None));
    assert_eq!(upload_of(&e, "shared"), Some(theirs.clone()));
    hub.fail_at("put", 1, transport_failure());
    err(push(&e, &hub, None));
    assert_eq!(upload_of(&e, "shared"), Some(theirs.clone()));

    // this push's own upload, when the first put fails for certain: cleared, nothing to resume
    let e = env(1);
    let hub = Hub::new();
    hub.fail_at("put", 1, transport_failure());
    err(push(&e, &hub, None));
    assert_eq!(upload_of(&e, "shared"), None);
    // a lost reply to the first put may have reached the quarantine: the upload stays
    let hub = Hub::new();
    hub.fail_at(
        "put",
        1,
        RdyError::new(
            E_UPSTREAM_UNCERTAIN,
            UPSTREAM_UNCERTAIN_MESSAGE,
            UPSTREAM_UNCERTAIN_FIX,
        ),
    );
    err(push(&e, &hub, None));
    assert!(upload_of(&e, "shared").is_some());

    // a found pending write: stop after state, keep both refs, send nothing more
    let e = env(1);
    let head = e.landings[0];
    let mine = SyncUpload {
        repo_id: repo_of(&e),
        url: e.url.clone(),
        push: "7zzzzzzzzzzzzzzzzzzzzzzzzz".into(),
        kind: KIND_CANON,
        view: None,
        at_ms: T0,
    };
    let sent = SyncPending {
        repo_id: repo_of(&e),
        url: e.url.clone(),
        push: "0123456789abcdefghjkmnpqrs".into(),
        kind: KIND_CANON,
        view: None,
        expected: None,
        head,
        at_ms: T0,
    };
    seed_sync(
        &e,
        "shared",
        vec![
            SyncStep::Upload(SyncUpload {
                push: sent.push.clone(),
                ..mine.clone()
            }),
            SyncStep::Send(sent.clone()),
        ],
    );
    seed_sync(&e, "shared", vec![SyncStep::Upload(mine.clone())]);
    let hub = Hub::new();
    assert_eq!(
        push(&e, &hub, None).unwrap(),
        PushOutcome::PendingWrite {
            push: sent.push.clone()
        }
    );
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    assert_eq!(upload_of(&e, "shared"), Some(mine));
}

/// D-47HVF, D-0SC9C: hub refusals keep publish's mapping. The three move codes pass on the
/// hub's message and fix with its request id; one at another status is a protocol failure; an
/// unknown code is `E_UPSTREAM_REFUSED` with core's text.
#[test]
fn scc8_hub_refusals_use_publish_mapping() {
    let mut s = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, s.ca_pem()).unwrap();
    let origin = Origin::parse(s.origin()).unwrap();
    let fence = OriginFence::new(origin.clone(), vec![]).unwrap();
    let wire = HttpSyncWire::new(HttpHubTransport::new(fence).unwrap());
    let claim = RequestClaim::from_session(&agent()).unwrap();
    let cx = UploadContext {
        install: origin,
        sync_base: Some(format!("{}/sync{REPO_PATH}", s.origin())),
        ca_file: Some(ca),
        claim: claim.clone(),
        bearer: Secret::new(access()),
    };
    let request = SyncStateRequest::new(&claim);
    let body = |code: &str| {
        format!(
            r#"{{"code":"{code}","message":"the hub's words","fix":"the hub's fix","issues":[],"request_id":"req_0042"}}"#
        )
    };
    let keys = [ObjectKey {
        kind: 1,
        hash: Hash::of(b"k"),
    }];
    let push_id = "0123456789abcdefghjkmnpqrs";
    let mut served = |reply: Reply, step: &str| -> RdyError {
        s.reply(reply);
        s.accept_n(1);
        match step {
            "state" => err(wire.state(&cx, &request)),
            "known" => err(wire.known(&cx, push_id, &keys)),
            _ => err(wire.put(&cx, push_id, &mut &b"batch"[..], 5)),
        }
    };
    for (code, status, step) in [
        ("E_HUB_CHAIN_INVALID", 422, "put"),
        ("E_HUB_BEHIND", 409, "state"),
        ("E_HUB_VIEW_EXISTS", 409, "known"),
    ] {
        let x = served(Reply::json(status, &body(code)), step);
        assert_eq!(
            (x.code, x.message.as_str(), x.fix.as_str()),
            (code, "the hub's words", "the hub's fix"),
            "{code}"
        );
        let last = x.issues.last().unwrap();
        assert_eq!(last.message, "hub request req_0042");
    }
    // a move code at another status: a protocol failure (a put's is uncertain)
    let x = served(Reply::json(422, &body("E_HUB_BEHIND")), "state");
    assert_eq!(
        (x.code, x.message.as_str()),
        (E_UPSTREAM_PROTOCOL, UPSTREAM_PROTOCOL_MESSAGE)
    );
    let x = served(Reply::json(409, &body("E_HUB_CHAIN_INVALID")), "put");
    assert_eq!(x.code, E_UPSTREAM_UNCERTAIN);
    // a code in neither table: core's text
    let x = served(Reply::json(409, &body("E_HUB_SOMETHING_NEW")), "known");
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_REFUSED,
            UPSTREAM_REFUSED_MESSAGE,
            UPSTREAM_REFUSED_FIX
        )
    );
    // the requests carried the sync media type
    let log = s.requests();
    assert!(
        log.iter()
            .filter(|r| r.target.ends_with("/state") || r.target.ends_with("/known"))
            .all(|r| r.header("content-type") == Some(SYNC_MEDIA_TYPE))
    );
    assert_eq!(s.finish(), 0);
}

/// D-47HVF, D-4XHPH: the remote choice (share, else only, else `E_REMOTE_REQUIRED`), a remote
/// that is not a share remote, and the hub's minimum, each with its exact words and before any
/// known or put step. The retired codes stay absent from error.rs and the contract.
#[test]
fn scc8_share_mode_and_minimum_refusals_are_exact() {
    let e = env(1);
    let hub = Hub::new();
    // a publish remote by name: refused before any request
    let x = err(push(&e, &hub, Some("hub")));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_BAD_INPUT,
            "remote hub is not a share remote",
            "set its mode to share, or choose a share remote"
        )
    );
    assert!(hub.steps().is_empty());
    // a name no remote has
    assert_eq!(err(push(&e, &hub, Some("nowhere"))).code, E_NOT_FOUND);
    assert!(hub.steps().is_empty());
    // no name: the share remote
    push(&e, &hub, None).unwrap();
    assert_eq!(hub.calls()[0].remote.as_deref(), Some(e.url.as_str()));

    // no share remote and two others: no remote is selected
    let e = env_with(install(), "# person settings\n", None, 1);
    rdy_engine::hub_remotes::RemoteService::new(&e.project, &e.user_file)
        .add(
            "hub2",
            &e.url,
            rdy_engine::hub_config::RemoteMode::Publish,
            rdy_engine::hub_remotes::RemoteLevel::Project,
        )
        .unwrap();
    let hub = Hub::new();
    let x = err(push(&e, &hub, None));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_REMOTE_REQUIRED,
            REMOTE_REQUIRED_MESSAGE,
            REMOTE_REQUIRED_FIX
        )
    );
    // the only remote, a publish one: chosen, then refused as not a share remote
    let e = env_with(install(), "# person settings\n", None, 1);
    let x = err(push(&e, &hub, None));
    assert_eq!(x.message, "remote hub is not a share remote");
    assert!(hub.steps().is_empty());

    // the hub's minimum is above this rdy: stop after state
    let e = env(1);
    let hub = Hub::new().with_state(|s| s.min_rdy = Some("999.0.0".into()));
    let x = err(push(&e, &hub, None));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_STORE_FORMAT,
            "hub repo acme/docs needs rdy 999.0.0",
            "update rdy to 999.0.0 or later"
        )
    );
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(upload_of(&e, "shared"), None);
    // a minimum at or below this rdy passes
    let hub = Hub::new().with_state(|s| s.min_rdy = Some("0.1.0".into()));
    push(&e, &hub, None).unwrap();

    // the retired codes stay absent
    let sources = [
        include_str!("../src/error.rs"),
        include_str!("../../rdy-qa/fixtures/contracts/sync-client-b-v1.json"),
    ];
    for code in [
        "E_REMOTE_MODE",
        "E_NOT_FF",
        "E_UPSTREAM_REWRITTEN",
        "E_UPSTREAM_SIG_INVALID",
        "E_UPSTREAM_KEY_UNKNOWN",
    ] {
        for s in sources {
            let quoted = format!("\"{code}\"");
            assert!(!s.contains(&quoted), "{code}");
        }
        assert_eq!(core_error_code(code), None);
    }
}

/// D-5Y8Y1: every step uses the machine's sign-in credential and the install's `ca_file`; no
/// other credential call is made (no static token, root-key pin or key-document fetch), and no
/// credential reaches a sync state value.
#[test]
fn scc8_sign_in_credentials_and_tls_trust_stay_on_machine() {
    let home = private_dir();
    let ca = home.path().join("ca.pem");
    std::fs::write(&ca, "not read by the fake wire\n").unwrap();
    let config = format!(
        "hub:\n  installs:\n    \"{}\":\n      ca_file: {}\n",
        install(),
        ca.display()
    );
    let e = env_with(install(), &config, Some("shared"), 2);
    let hub = Hub::new().with_state(|s| s.batch_records = 2);
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let out = service(&e, &api, &clock, &hub)
        .push(&e.engine, &agent(), PushRequest { remote: None })
        .unwrap();
    assert!(matches!(out, PushOutcome::Uploaded { .. }));
    let calls = hub.calls();
    assert!(calls.len() >= 4);
    for c in &calls {
        assert_eq!(c.bearer, access(), "{}", c.step);
        assert_eq!(c.ca_file.as_deref(), Some(ca.as_path()), "{}", c.step);
    }
    // discovery has no sync base yet; every later step goes to the discovered one
    assert_eq!(calls[0].sync_base, None);
    for c in &calls[1..] {
        assert_eq!(
            c.sync_base.as_deref(),
            Some(&*format!("{}/sync{REPO_PATH}", install()))
        );
    }
    assert!(api.seen.lock().unwrap().is_empty());
    // the sync refs hold no token
    let read = e.engine.read();
    let u = read.reader().sync_upload("shared").unwrap().unwrap();
    let text = format!("{u:?}");
    assert!(!text.contains("rdyh_"), "{text}");
    for (name, value) in read.reader().refs_with_prefix("meta/sync/").unwrap() {
        let v = String::from_utf8_lossy(&value);
        assert!(!v.contains("rdyh_"), "{name}");
        assert!(!v.contains(&access()), "{name}");
    }
    let _ = HistorySent::default();
}
